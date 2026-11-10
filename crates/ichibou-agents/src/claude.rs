//! Claude Code driver: `claude -p` with stream-json in both directions.

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::{Context, Result};
use ichibou_core::Activity;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as Process;
use tokio::sync::mpsc;

use crate::{truncate, AgentEvent, Command, Session, SessionSpec};

pub fn args(spec: &SessionSpec) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    if spec.read_only {
        // Read-only is enforced by what exists, not by plan mode: plan mode
        // also refuses MCP tools, and these sessions must be able to report
        // back through Ichibou's. Only tools that read are available.
        args.extend(["--permission-mode".into(), "default".into()]);
        args.extend(["--tools".into(), "Read,Glob,Grep".into()]);
    } else {
        args.extend([
            "--permission-mode".into(),
            spec.claude_permission_mode.clone(),
        ]);
    }

    for (flag, value) in [("--model", &spec.model), ("--effort", &spec.effort)] {
        if let Some(value) = value.as_deref().filter(|v| crate::valid_name(v)) {
            args.extend([flag.to_string(), value.to_string()]);
        }
    }
    if !spec.instructions.is_empty() {
        args.extend(["--append-system-prompt".into(), spec.instructions.clone()]);
    }
    if let Some(url) = &spec.mcp_url {
        let config = json!({ "mcpServers": { "ichibou": { "type": "http", "url": url } } });
        args.extend(["--mcp-config".into(), config.to_string()]);
        // Ichibou's own tools only report to Ichibou; without this every
        // call would be refused in a headless session.
        args.extend(["--allowedTools".into(), "mcp__ichibou".into()]);
        if spec.read_only {
            // Other MCP servers from the user's configuration may offer
            // tools that write or run commands; a read-only session gets
            // Ichibou's server alone.
            args.push("--strict-mcp-config".into());
        }
    }
    if let Some(session) = &spec.resume {
        args.extend(["--resume".into(), session.clone()]);
    }
    args
}

pub async fn start(spec: SessionSpec) -> Result<Session> {
    let program = spec
        .program
        .clone()
        .unwrap_or_else(|| "claude".into());
    // The agent itself is not wrapped in Ichibou's sandbox: both CLIs
    // sandbox the commands they run, and macOS refuses that inside another
    // sandbox. What keeps an agent out of Ichibou's state is its own
    // confinement to the workspace (the state lives outside it).
    let mut child = Process::new(&program)
        .args(args(&spec))
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot start {}", program.display()))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let stderr = child.stderr.take().context("no stderr")?;

    let (command_tx, mut command_rx) = mpsc::channel::<Command>(64);
    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(256);

    // stderr: keep the last line to explain an unexpected exit.
    let (err_tx, mut err_rx) = mpsc::channel::<String>(16);
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = err_tx.try_send(line);
        }
    });

    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut parser = Parser::default();
        let mut stdin_open = true;
        let mut request = 0u64;
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        for event in parser.line(&line) {
                            if event_tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                    _ => break,
                },
                command = command_rx.recv(), if stdin_open => {
                    let message = match command {
                        Some(Command::Send(text)) => Some(json!({
                            "type": "user",
                            "message": { "role": "user", "content": text }
                        })),
                        Some(Command::Interrupt) => {
                            request += 1;
                            Some(json!({
                                "type": "control_request",
                                "request_id": format!("ichibou-{request}"),
                                "request": { "subtype": "interrupt" }
                            }))
                        }
                        // Headless Claude resolves permissions from its
                        // permission mode; there is nothing to answer.
                        Some(Command::Approve { .. }) => None,
                        Some(Command::Stop) | None => {
                            stdin_open = false;
                            let _ = stdin.shutdown().await;
                            let _ = child.start_kill();
                            None
                        }
                    };
                    if let Some(message) = message {
                        let line = format!("{message}\n");
                        if stdin.write_all(line.as_bytes()).await.is_err() {
                            stdin_open = false;
                        } else {
                            let _ = stdin.flush().await;
                        }
                    }
                }
            }
        }
        let code = child.wait().await.ok().and_then(|s| s.code());
        if code != Some(0) {
            let mut last = String::new();
            while let Ok(line) = err_rx.try_recv() {
                last = line;
            }
            if !last.is_empty() {
                let _ = event_tx.send(AgentEvent::Error { message: last }).await;
            }
        }
        let _ = event_tx.send(AgentEvent::Exited { code }).await;
    });

    Ok(Session {
        commands: command_tx,
        events: event_rx,
        pid,
    })
}

/// Turns stream-json lines into events.
#[derive(Default)]
pub struct Parser {
    /// tool_use id -> file path, for tools that write files.
    writes: HashMap<String, String>,
}

impl Parser {
    pub fn line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![AgentEvent::Unknown {
                kind: "unparsable".into(),
            }];
        };
        let kind = value["type"].as_str().unwrap_or("");
        match kind {
            "system" => match value["subtype"].as_str() {
                Some("init") => vec![
                    AgentEvent::Session {
                        id: value["session_id"].as_str().unwrap_or("").to_string(),
                        model: value["model"].as_str().map(str::to_string),
                        auth: value["apiKeySource"].as_str().map(str::to_string),
                    },
                    AgentEvent::Activity {
                        activity: Activity::Working,
                    },
                ],
                other => vec![AgentEvent::Unknown {
                    kind: format!("system/{}", other.unwrap_or("")),
                }],
            },
            "assistant" => self.assistant(&value),
            "user" => self.tool_results(&value),
            "rate_limit_event" => {
                let info = &value["rate_limit_info"];
                let status = info["status"].as_str().unwrap_or("");
                vec![AgentEvent::RateLimit {
                    limited: !status.is_empty() && !status.starts_with("allowed"),
                    resets_at: info["resetsAt"].as_i64(),
                    detail: info["rateLimitType"].as_str().unwrap_or("").to_string(),
                }]
            }
            "result" => {
                let ok = !value["is_error"].as_bool().unwrap_or(false);
                let mut events = Vec::new();
                if let Some(denials) = value["permission_denials"].as_array() {
                    for denial in denials {
                        events.push(AgentEvent::Error {
                            message: format!(
                                "permission denied: {}",
                                truncate(&denial.to_string(), 200)
                            ),
                        });
                    }
                }
                events.push(AgentEvent::TurnCompleted {
                    ok,
                    summary: value["result"].as_str().unwrap_or("").to_string(),
                });
                events.push(AgentEvent::Activity {
                    activity: Activity::WaitingForInput,
                });
                events
            }
            other => vec![AgentEvent::Unknown {
                kind: other.to_string(),
            }],
        }
    }

    fn assistant(&mut self, value: &Value) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        let Some(blocks) = value["message"]["content"].as_array() else {
            return events;
        };
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or("");
                    if !text.trim().is_empty() {
                        events.push(AgentEvent::Message {
                            text: text.to_string(),
                        });
                    }
                }
                Some("tool_use") => {
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let name = block["name"].as_str().unwrap_or("").to_string();
                    let input = &block["input"];
                    let summary = ["command", "file_path", "path", "pattern", "url", "description"]
                        .iter()
                        .find_map(|key| input[*key].as_str())
                        .unwrap_or("");
                    if matches!(name.as_str(), "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                        if let Some(path) = input["file_path"]
                            .as_str()
                            .or_else(|| input["notebook_path"].as_str())
                        {
                            self.writes.insert(id.clone(), path.to_string());
                        }
                    }
                    events.push(AgentEvent::Activity {
                        activity: Activity::RunningTool,
                    });
                    events.push(AgentEvent::ToolStarted {
                        id,
                        name,
                        summary: truncate(summary, 200),
                    });
                }
                _ => {}
            }
        }
        events
    }

    fn tool_results(&mut self, value: &Value) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        let Some(blocks) = value["message"]["content"].as_array() else {
            return events;
        };
        for block in blocks {
            if block["type"].as_str() != Some("tool_result") {
                continue;
            }
            let id = block["tool_use_id"].as_str().unwrap_or("").to_string();
            let ok = !block["is_error"].as_bool().unwrap_or(false);
            if let Some(path) = self.writes.remove(&id) {
                if ok {
                    events.push(AgentEvent::FileChanged { paths: vec![path] });
                }
            }
            events.push(AgentEvent::ToolFinished { id, ok });
        }
        if !events.is_empty() {
            events.push(AgentEvent::Activity {
                activity: Activity::Working,
            });
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_session_parses() {
        let text = include_str!("../tests/fixtures/claude-simple.jsonl");
        let mut parser = Parser::default();
        let events: Vec<AgentEvent> = text.lines().flat_map(|l| parser.line(l)).collect();
        assert!(matches!(&events.iter().find(|e| matches!(e, AgentEvent::Session { .. })),
            Some(AgentEvent::Session { auth: Some(auth), .. }) if auth == "none"));
        assert!(events.contains(&AgentEvent::Message { text: "done".into() }));
        assert!(events.contains(&AgentEvent::TurnCompleted {
            ok: true,
            summary: "done".into()
        }));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::RateLimit { limited: false, .. })));
    }

    #[test]
    fn tool_use_and_write_are_tracked() {
        let mut parser = Parser::default();
        let started = parser.line(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/w/a.ts","content":"x"}}]}}"#,
        );
        assert!(started.contains(&AgentEvent::ToolStarted {
            id: "t1".into(),
            name: "Write".into(),
            summary: "/w/a.ts".into()
        }));
        let finished = parser.line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}"#,
        );
        assert!(finished.contains(&AgentEvent::FileChanged {
            paths: vec!["/w/a.ts".into()]
        }));
        assert!(finished.contains(&AgentEvent::ToolFinished {
            id: "t1".into(),
            ok: true
        }));
    }

    #[test]
    fn unknown_and_broken_lines_do_not_fail() {
        let mut parser = Parser::default();
        assert_eq!(
            parser.line(r#"{"type":"brand_new_event","x":1}"#),
            vec![AgentEvent::Unknown {
                kind: "brand_new_event".into()
            }]
        );
        assert_eq!(parser.line("not json").len(), 1);
        assert!(parser.line("").is_empty());
    }

    #[test]
    fn read_only_sessions_have_only_reading_tools() {
        let spec = SessionSpec {
            agent: ichibou_core::AgentKind::Claude,
            cwd: "/tmp".into(),
            resume: Some("abc".into()),
            read_only: true,
            instructions: "be brief".into(),
            mcp_url: Some("http://127.0.0.1:1/mcp/t".into()),
            model: Some("opus".into()),
            effort: Some("xhigh".into()),
            claude_permission_mode: "acceptEdits".into(),
            fake_script: None,
            program: None,
        };
        let args = args(&spec);
        let at = args.iter().position(|a| a == "--permission-mode").unwrap();
        assert_eq!(args[at + 1], "default");
        let at = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[at + 1], "Read,Glob,Grep");
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        let at = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[at + 1], "opus");
        let at = args.iter().position(|a| a == "--effort").unwrap();
        assert_eq!(args[at + 1], "xhigh");
        assert!(args.contains(&"--resume".to_string()));
        let at = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(args[at + 1], "mcp__ichibou");
        assert!(!args.iter().any(|a| a == "--bare"));
    }

    #[test]
    fn unsafe_model_names_are_not_passed_on() {
        assert!(crate::valid_name("claude-opus-5-5"));
        assert!(crate::valid_name("gpt-6.1-sol"));
        assert!(crate::valid_name("claude-opus-5-5[1m]"));
        for bad in ["", "--dangerously-skip-permissions", "a b", "x\"; rm -rf /", "a\nb"] {
            assert!(!crate::valid_name(bad), "{bad}");
        }
        let mut spec = SessionSpec {
            agent: ichibou_core::AgentKind::Claude,
            cwd: "/tmp".into(),
            resume: None,
            read_only: false,
            instructions: String::new(),
            mcp_url: None,
            model: Some("--bare".into()),
            effort: Some("max; echo".into()),
            claude_permission_mode: "acceptEdits".into(),
            fake_script: None,
            program: None,
        };
        let args = args(&spec);
        assert!(!args.iter().any(|a| a == "--model" || a == "--effort" || a == "--bare"));
        spec.model = Some("sonnet".into());
        assert!(super::args(&spec).contains(&"sonnet".to_string()));
    }
}
