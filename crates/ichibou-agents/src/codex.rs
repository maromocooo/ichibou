//! Codex driver: `codex app-server` over stdio (line-delimited JSON-RPC).

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::{Context, Result};
use ichibou_core::Activity;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as Process;
use tokio::sync::mpsc;

use crate::{truncate, AgentEvent, Command, Session, SessionSpec};

/// Protocol methods this driver depends on. The compatibility test checks
/// them against the schema of the installed CLI.
pub const CLIENT_METHODS: &[&str] = &[
    "initialize",
    "thread/start",
    "thread/resume",
    "turn/start",
    "turn/steer",
    "turn/interrupt",
];
pub const SERVER_METHODS: &[&str] = &[
    "turn/started",
    "turn/completed",
    "item/started",
    "item/completed",
    "error",
];

pub async fn start(spec: SessionSpec) -> Result<Session> {
    let program = spec.program.clone().unwrap_or_else(|| "codex".into());
    let mut process = Process::new(&program);
    process.arg("app-server");
    if let Some(url) = &spec.mcp_url {
        process.args(["-c", &format!("mcp_servers.ichibou.url=\"{url}\"")]);
    }
    let mut child = process
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot start {}", program.display()))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;

    let (command_tx, mut command_rx) = mpsc::channel::<Command>(64);
    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(256);

    tokio::spawn(async move {
        let mut protocol = Protocol::new(&spec);
        let mut lines = BufReader::new(stdout).lines();
        let mut outgoing = protocol.begin();
        let mut open = true;
        loop {
            for message in outgoing.drain(..) {
                let line = format!("{message}\n");
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    open = false;
                }
            }
            let _ = stdin.flush().await;
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
                        let (events, replies) = protocol.incoming(message);
                        outgoing = replies;
                        for event in events {
                            if event_tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                    _ => break,
                },
                command = command_rx.recv(), if open => match command {
                    Some(Command::Stop) | None => {
                        open = false;
                        let _ = stdin.shutdown().await;
                        let _ = child.start_kill();
                    }
                    Some(command) => outgoing = protocol.command(command),
                },
            }
        }
        let code = child.wait().await.ok().and_then(|s| s.code());
        let _ = event_tx.send(AgentEvent::Exited { code }).await;
    });

    Ok(Session {
        commands: command_tx,
        events: event_rx,
        pid,
    })
}

enum Pending {
    Initialize,
    Thread,
    TurnStart,
    /// A steer that may have raced the end of the turn; the text is kept so
    /// it can be re-sent as a new turn.
    Steer(String),
    Other,
}

/// The JSON-RPC state machine, separated from process I/O so it can be
/// tested with recorded messages.
pub struct Protocol {
    cwd: String,
    resume: Option<String>,
    read_only: bool,
    instructions: String,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    thread: Option<String>,
    turn: Option<String>,
    /// A turn/start was sent and turn/started has not arrived yet.
    starting: bool,
    queued: Vec<String>,
    /// Approval id -> JSON-RPC request id to answer, and whether the answer
    /// is an MCP elicitation (`action`) or an approval (`decision`).
    approvals: HashMap<String, (Value, bool)>,
}

impl Protocol {
    pub fn new(spec: &SessionSpec) -> Self {
        Self {
            cwd: spec.cwd.to_string_lossy().into_owned(),
            resume: spec.resume.clone(),
            read_only: spec.read_only,
            instructions: spec.instructions.clone(),
            next_id: 0,
            pending: HashMap::new(),
            thread: None,
            turn: None,
            starting: false,
            queued: Vec::new(),
            approvals: HashMap::new(),
        }
    }

    fn request(&mut self, method: &str, params: Value, pending: Pending) -> Value {
        self.next_id += 1;
        self.pending.insert(self.next_id, pending);
        json!({ "id": self.next_id, "method": method, "params": params })
    }

    pub fn begin(&mut self) -> Vec<Value> {
        vec![self.request(
            "initialize",
            json!({ "clientInfo": {
                "name": "ichibou",
                "title": "Ichibou",
                "version": env!("CARGO_PKG_VERSION")
            }}),
            Pending::Initialize,
        )]
    }

    fn open_thread(&mut self) -> Value {
        let mut params = json!({
            "cwd": self.cwd,
            "sandbox": if self.read_only { "read-only" } else { "workspace-write" },
            // "never" would also refuse MCP tool calls, and read-only
            // sessions must reach Ichibou's tools. Approvals are asked for,
            // and this driver answers them itself for read-only sessions.
            "approvalPolicy": "on-request",
        });
        if !self.instructions.is_empty() {
            params["developerInstructions"] = json!(self.instructions);
        }
        match self.resume.clone() {
            Some(thread) => {
                params["threadId"] = json!(thread);
                self.request("thread/resume", params, Pending::Thread)
            }
            None => self.request("thread/start", params, Pending::Thread),
        }
    }

    fn start_turn(&mut self, text: String) -> Value {
        self.starting = true;
        let thread = self.thread.clone().unwrap_or_default();
        self.request(
            "turn/start",
            json!({ "threadId": thread, "input": [ { "type": "text", "text": text } ] }),
            Pending::TurnStart,
        )
    }

    fn deliver(&mut self, text: String) -> Vec<Value> {
        if self.thread.is_none() || self.starting {
            self.queued.push(text);
            return vec![];
        }
        match self.turn.clone() {
            // `turn/steer` only reaches a turn that is still running.
            Some(turn) => {
                let thread = self.thread.clone().unwrap_or_default();
                vec![self.request(
                    "turn/steer",
                    json!({
                        "threadId": thread,
                        "expectedTurnId": turn,
                        "input": [ { "type": "text", "text": text } ]
                    }),
                    Pending::Steer(text),
                )]
            }
            None => vec![self.start_turn(text)],
        }
    }

    fn flush_queue(&mut self) -> Vec<Value> {
        if self.queued.is_empty() {
            return vec![];
        }
        let text = std::mem::take(&mut self.queued).join("\n\n");
        self.deliver(text)
    }

    pub fn command(&mut self, command: Command) -> Vec<Value> {
        match command {
            Command::Send(text) => self.deliver(text),
            Command::Interrupt => match (self.thread.clone(), self.turn.clone()) {
                (Some(thread), Some(turn)) => vec![self.request(
                    "turn/interrupt",
                    json!({ "threadId": thread, "turnId": turn }),
                    Pending::Other,
                )],
                _ => vec![],
            },
            Command::Approve { id, allow } => match self.approvals.remove(&id) {
                Some((request, elicitation)) => {
                    let answer = if allow { "accept" } else { "decline" };
                    let result = if elicitation {
                        json!({ "action": answer, "content": {} })
                    } else {
                        json!({ "decision": answer })
                    };
                    vec![json!({ "id": request, "result": result })]
                }
                None => vec![],
            },
            Command::Stop => vec![],
        }
    }

    pub fn incoming(&mut self, message: Value) -> (Vec<AgentEvent>, Vec<Value>) {
        let method = message["method"].as_str().map(str::to_string);
        let id = message.get("id").cloned().filter(|v| !v.is_null());
        match (method, id) {
            (Some(method), Some(id)) => self.server_request(&method, id, &message["params"]),
            (Some(method), None) => self.notification(&method, &message["params"]),
            (None, Some(id)) => self.response(id, &message),
            (None, None) => (vec![], vec![]),
        }
    }

    fn response(&mut self, id: Value, message: &Value) -> (Vec<AgentEvent>, Vec<Value>) {
        let Some(pending) = id.as_u64().and_then(|id| self.pending.remove(&id)) else {
            return (vec![], vec![]);
        };
        let error = message.get("error").filter(|e| !e.is_null());
        let error_text = || {
            error
                .and_then(|e| e["message"].as_str())
                .unwrap_or("request failed")
                .to_string()
        };
        match pending {
            Pending::Initialize => {
                if error.is_some() {
                    return (vec![AgentEvent::Error { message: error_text() }], vec![]);
                }
                let open = self.open_thread();
                (vec![], vec![json!({ "method": "initialized" }), open])
            }
            Pending::Thread => {
                if error.is_some() {
                    return (vec![AgentEvent::Error { message: error_text() }], vec![]);
                }
                let result = &message["result"];
                let thread = result["thread"]["id"].as_str().unwrap_or("").to_string();
                self.thread = Some(thread.clone());
                let events = vec![
                    AgentEvent::Session {
                        id: thread,
                        model: result["model"]
                            .as_str()
                            .or_else(|| result["thread"]["model"].as_str())
                            .map(str::to_string),
                        auth: None,
                    },
                    AgentEvent::Activity {
                        activity: Activity::WaitingForInput,
                    },
                ];
                (events, self.flush_queue())
            }
            Pending::TurnStart => {
                if error.is_some() {
                    self.starting = false;
                    return (vec![AgentEvent::Error { message: error_text() }], vec![]);
                }
                // Some versions return the turn here before `turn/started`.
                if let Some(turn) = message["result"]["turn"]["id"].as_str() {
                    self.turn = Some(turn.to_string());
                }
                (vec![], vec![])
            }
            Pending::Steer(text) => {
                if error.is_some() {
                    // The turn ended before the steer landed: start a new one.
                    self.turn = None;
                    return (vec![], self.deliver(text));
                }
                (vec![], vec![])
            }
            Pending::Other => (vec![], vec![]),
        }
    }

    fn server_request(
        &mut self,
        method: &str,
        id: Value,
        params: &Value,
    ) -> (Vec<AgentEvent>, Vec<Value>) {
        // A read-only session never gets more access: anything it asks for
        // beyond Ichibou's own tools is declined without involving the user.
        if self.read_only {
            let ichibou = method == "mcpServer/elicitation/request"
                && params["serverName"].as_str() == Some("ichibou");
            let result = match method {
                "mcpServer/elicitation/request" => {
                    json!({ "action": if ichibou { "accept" } else { "decline" }, "content": {} })
                }
                "item/commandExecution/requestApproval"
                | "execCommandApproval"
                | "item/fileChange/requestApproval"
                | "applyPatchApproval"
                | "item/permissions/requestApproval" => json!({ "decision": "decline" }),
                _ => Value::Null,
            };
            if !result.is_null() {
                return (vec![], vec![json!({ "id": id, "result": result })]);
            }
        }
        let mut elicitation = false;
        // The user decides on what is shown, so show the request itself:
        // long enough for a whole command line or change list.
        let approval = |kind: &str, detail: String| AgentEvent::ApprovalRequest {
            id: id.to_string(),
            kind: kind.to_string(),
            detail: truncate(&detail, 8000),
        };
        // Everything Codex sent about a request, for kinds without one
        // obvious field to show.
        let everything = || serde_json::to_string_pretty(params).unwrap_or_default();
        let event = match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                let command = match &params["command"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => everything(),
                };
                let cwd = params["cwd"].as_str().unwrap_or("");
                let reason = params["reason"].as_str().unwrap_or("");
                let mut detail = command;
                if !cwd.is_empty() {
                    detail.push_str(&format!("\nin {cwd}"));
                }
                if !reason.is_empty() {
                    detail.push_str(&format!("\nreason: {reason}"));
                }
                approval("command", detail)
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                approval("file_change", everything())
            }
            "item/permissions/requestApproval" => approval("permissions", everything()),
            "mcpServer/elicitation/request" => {
                // Codex asks before calling an MCP tool. Ichibou's own tools
                // only report to Ichibou, so they are always allowed; any
                // other server's request goes to the user.
                if params["serverName"].as_str() == Some("ichibou") {
                    let reply = json!({ "id": id, "result": { "action": "accept", "content": {} } });
                    return (vec![], vec![reply]);
                }
                elicitation = true;
                approval(
                    "mcp",
                    format!(
                        "{}: {}",
                        params["serverName"].as_str().unwrap_or(""),
                        params["message"].as_str().unwrap_or("")
                    ),
                )
            }
            _ => {
                let reply = json!({
                    "id": id,
                    "error": { "code": -32601, "message": "not supported by Ichibou" }
                });
                return (
                    vec![AgentEvent::Unknown {
                        kind: format!("request/{method}"),
                    }],
                    vec![reply],
                );
            }
        };
        self.approvals.insert(id.to_string(), (id, elicitation));
        (
            vec![
                event,
                AgentEvent::Activity {
                    activity: Activity::WaitingForInput,
                },
            ],
            vec![],
        )
    }

    fn notification(&mut self, method: &str, params: &Value) -> (Vec<AgentEvent>, Vec<Value>) {
        let item = &params["item"];
        let item_id = || item["id"].as_str().unwrap_or("").to_string();
        match method {
            "turn/started" => {
                self.starting = false;
                self.turn = params["turn"]["id"].as_str().map(str::to_string);
                let events = vec![AgentEvent::Activity {
                    activity: Activity::Working,
                }];
                (events, self.flush_queue())
            }
            "turn/completed" => {
                self.starting = false;
                self.turn = None;
                let turn = &params["turn"];
                let status = turn["status"].as_str().unwrap_or("");
                let summary = turn["error"]["message"].as_str().unwrap_or("").to_string();
                let events = vec![
                    AgentEvent::TurnCompleted {
                        ok: status == "completed",
                        summary,
                    },
                    AgentEvent::Activity {
                        activity: if status == "interrupted" {
                            Activity::Interrupted
                        } else {
                            Activity::WaitingForInput
                        },
                    },
                ];
                (events, self.flush_queue())
            }
            "item/started" => {
                let started = |name: String, summary: &str| {
                    vec![
                        AgentEvent::Activity {
                            activity: Activity::RunningTool,
                        },
                        AgentEvent::ToolStarted {
                            id: item_id(),
                            name,
                            summary: truncate(summary, 200),
                        },
                    ]
                };
                let events = match item["type"].as_str() {
                    Some("commandExecution") => {
                        started("shell".into(), item["command"].as_str().unwrap_or(""))
                    }
                    Some("fileChange") => started("edit".into(), ""),
                    Some("mcpToolCall") => started(
                        format!(
                            "{}.{}",
                            item["server"].as_str().unwrap_or(""),
                            item["tool"].as_str().unwrap_or("")
                        ),
                        "",
                    ),
                    Some("webSearch") => {
                        started("web_search".into(), item["query"].as_str().unwrap_or(""))
                    }
                    _ => vec![],
                };
                (events, vec![])
            }
            "item/completed" => {
                let finished = |ok: bool| AgentEvent::ToolFinished { id: item_id(), ok };
                let working = AgentEvent::Activity {
                    activity: Activity::Working,
                };
                let events = match item["type"].as_str() {
                    Some("agentMessage") => {
                        let text = item["text"].as_str().unwrap_or("");
                        if text.trim().is_empty() {
                            vec![]
                        } else {
                            vec![AgentEvent::Message {
                                text: text.to_string(),
                            }]
                        }
                    }
                    Some("commandExecution") => {
                        vec![finished(item["exitCode"].as_i64() == Some(0)), working]
                    }
                    Some("fileChange") => {
                        let ok = item["status"].as_str() == Some("completed");
                        let paths: Vec<String> = item["changes"]
                            .as_array()
                            .map(|changes| {
                                changes
                                    .iter()
                                    .filter_map(|c| c["path"].as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut events = Vec::new();
                        if ok && !paths.is_empty() {
                            events.push(AgentEvent::FileChanged { paths });
                        }
                        events.push(finished(ok));
                        events.push(working);
                        events
                    }
                    Some("mcpToolCall") => vec![finished(item["error"].is_null()), working],
                    Some("webSearch") => vec![finished(true), working],
                    _ => vec![],
                };
                (events, vec![])
            }
            "error" => {
                let error = &params["error"];
                let message = error["message"].as_str().unwrap_or("error").to_string();
                let info = error["codexErrorInfo"].to_string().to_lowercase();
                let mut events = Vec::new();
                if info.contains("usagelimit") || info.contains("ratelimit") {
                    events.push(AgentEvent::RateLimit {
                        limited: true,
                        resets_at: None,
                        detail: message.clone(),
                    });
                }
                if !params["willRetry"].as_bool().unwrap_or(false) {
                    events.push(AgentEvent::Error { message });
                }
                (events, vec![])
            }
            // Streaming deltas would flood the event log; the completed item
            // carries the full text.
            m if m.ends_with("/delta") || m.ends_with("Delta") => (vec![], vec![]),
            other => (
                vec![AgentEvent::Unknown {
                    kind: other.to_string(),
                }],
                vec![],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ichibou_core::AgentKind;

    fn spec() -> SessionSpec {
        SessionSpec {
            agent: AgentKind::Codex,
            cwd: "/work".into(),
            resume: None,
            read_only: false,
            instructions: "use the ichibou tools".into(),
            mcp_url: None,
            claude_permission_mode: "acceptEdits".into(),
            fake_script: None,
            program: None,
        }
    }

    /// Runs the handshake and returns a protocol with an open thread.
    fn ready() -> Protocol {
        let mut p = Protocol::new(&spec());
        let init = p.begin();
        assert_eq!(init[0]["method"], "initialize");
        let (_, out) = p.incoming(json!({ "id": 1, "result": {} }));
        assert_eq!(out[0]["method"], "initialized");
        assert_eq!(out[1]["method"], "thread/start");
        assert_eq!(out[1]["params"]["sandbox"], "workspace-write");
        let (events, _) = p.incoming(json!({ "id": 2, "result": { "thread": { "id": "th1" } } }));
        assert!(matches!(&events[0], AgentEvent::Session { id, .. } if id == "th1"));
        p
    }

    #[test]
    fn message_before_thread_is_queued_then_started() {
        let mut p = Protocol::new(&spec());
        p.begin();
        assert!(p.command(Command::Send("build it".into())).is_empty());
        p.incoming(json!({ "id": 1, "result": {} }));
        let (_, out) = p.incoming(json!({ "id": 2, "result": { "thread": { "id": "th1" } } }));
        assert_eq!(out[0]["method"], "turn/start");
        assert_eq!(out[0]["params"]["input"][0]["text"], "build it");
    }

    #[test]
    fn steers_a_running_turn_and_starts_a_new_one_after_it_ends() {
        let mut p = ready();
        let out = p.command(Command::Send("first".into()));
        assert_eq!(out[0]["method"], "turn/start");
        // Sent while the turn is still starting: held, then steered in.
        assert!(p.command(Command::Send("second".into())).is_empty());
        let (_, out) = p.incoming(json!({ "method": "turn/started", "params": { "turn": { "id": "t1" } } }));
        assert_eq!(out[0]["method"], "turn/steer");
        assert_eq!(out[0]["params"]["expectedTurnId"], "t1");

        p.incoming(json!({ "method": "turn/completed", "params": { "turn": { "id": "t1", "status": "completed" } } }));
        let out = p.command(Command::Send("third".into()));
        assert_eq!(out[0]["method"], "turn/start");
    }

    #[test]
    fn steer_that_loses_the_race_becomes_a_new_turn() {
        let mut p = ready();
        p.command(Command::Send("first".into()));
        p.incoming(json!({ "method": "turn/started", "params": { "turn": { "id": "t1" } } }));
        let steer = p.command(Command::Send("fix the header".into()));
        let id = steer[0]["id"].clone();
        let (_, out) = p.incoming(json!({ "id": id, "error": { "code": -1, "message": "no active turn" } }));
        assert_eq!(out[0]["method"], "turn/start");
        assert_eq!(out[0]["params"]["input"][0]["text"], "fix the header");
    }

    #[test]
    fn items_map_to_events_and_approvals_round_trip() {
        let mut p = ready();
        let (events, _) = p.incoming(json!({ "method": "item/completed", "params": { "item": {
            "type": "fileChange", "id": "i1", "status": "completed",
            "changes": [ { "path": "src/a.ts" } ] } } }));
        assert!(events.contains(&AgentEvent::FileChanged { paths: vec!["src/a.ts".into()] }));

        let (events, replies) = p.incoming(json!({ "id": 7, "method": "item/commandExecution/requestApproval",
            "params": { "command": "npm install" } }));
        assert!(replies.is_empty());
        let AgentEvent::ApprovalRequest { id, detail, .. } = &events[0] else { panic!() };
        assert_eq!(detail, "npm install");
        // A long command is shown whole, with where it would run.
        let long = format!("echo {} && rm -rf build", "x".repeat(900));
        let (events, _) = p.incoming(json!({ "id": 70, "method": "item/commandExecution/requestApproval",
            "params": { "command": long, "cwd": "/work/app" } }));
        let AgentEvent::ApprovalRequest { detail, .. } = &events[0] else { panic!() };
        assert!(detail.ends_with("&& rm -rf build\nin /work/app"), "{detail}");
        p.command(Command::Approve { id: "70".into(), allow: false });
        let out = p.command(Command::Approve { id: id.clone(), allow: true });
        assert_eq!(out[0], json!({ "id": 7, "result": { "decision": "accept" } }));

        // Ichibou's own MCP tools are allowed without asking; another
        // server's request is put to the user and answered as an elicitation.
        let (events, replies) = p.incoming(json!({ "id": 9, "method": "mcpServer/elicitation/request",
            "params": { "serverName": "ichibou", "threadId": "th1", "message": "Allow tool?", "mode": "form", "requestedSchema": {} } }));
        assert!(events.is_empty());
        assert_eq!(replies[0], json!({ "id": 9, "result": { "action": "accept", "content": {} } }));
        let (events, replies) = p.incoming(json!({ "id": 10, "method": "mcpServer/elicitation/request",
            "params": { "serverName": "other", "threadId": "th1", "message": "Allow tool?", "mode": "form", "requestedSchema": {} } }));
        assert!(replies.is_empty());
        let AgentEvent::ApprovalRequest { id, kind, .. } = &events[0] else { panic!() };
        assert_eq!(kind, "mcp");
        let out = p.command(Command::Approve { id: id.clone(), allow: false });
        assert_eq!(out[0], json!({ "id": 10, "result": { "action": "decline", "content": {} } }));

        // Requests Ichibou does not implement get an error, not silence.
        let (_, replies) = p.incoming(json!({ "id": 8, "method": "attestation/generate", "params": {} }));
        assert_eq!(replies[0]["error"]["code"], -32601);

        let (events, _) = p.incoming(json!({ "method": "some/new/notification", "params": {} }));
        assert_eq!(events, vec![AgentEvent::Unknown { kind: "some/new/notification".into() }]);
    }

    #[test]
    fn read_only_thread_cannot_write() {
        let mut s = spec();
        s.read_only = true;
        s.resume = Some("th9".into());
        let mut p = Protocol::new(&s);
        p.begin();
        let (_, out) = p.incoming(json!({ "id": 1, "result": {} }));
        assert_eq!(out[1]["method"], "thread/resume");
        assert_eq!(out[1]["params"]["threadId"], "th9");
        assert_eq!(out[1]["params"]["sandbox"], "read-only");
        assert_eq!(out[1]["params"]["approvalPolicy"], "on-request");
        p.incoming(json!({ "id": 2, "result": { "thread": { "id": "th9" } } }));

        // Ichibou's tools are allowed; everything else is declined without
        // asking the user.
        let (events, replies) = p.incoming(json!({ "id": 5, "method": "mcpServer/elicitation/request",
            "params": { "serverName": "ichibou", "threadId": "th9", "message": "", "mode": "form", "requestedSchema": {} } }));
        assert!(events.is_empty());
        assert_eq!(replies[0]["result"]["action"], "accept");
        let (events, replies) = p.incoming(json!({ "id": 6, "method": "mcpServer/elicitation/request",
            "params": { "serverName": "other", "threadId": "th9", "message": "", "mode": "form", "requestedSchema": {} } }));
        assert!(events.is_empty());
        assert_eq!(replies[0]["result"]["action"], "decline");
        for (id, method) in [(7, "item/commandExecution/requestApproval"), (8, "item/fileChange/requestApproval"), (9, "item/permissions/requestApproval")] {
            let (events, replies) = p.incoming(json!({ "id": id, "method": method, "params": { "command": "rm -rf x" } }));
            assert!(events.is_empty(), "{method}");
            assert_eq!(replies[0], json!({ "id": id, "result": { "decision": "decline" } }));
        }
    }
}
