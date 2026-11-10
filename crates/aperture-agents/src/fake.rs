//! Scripted driver for tests and demos.
//!
//! It behaves like a real agent from the daemon's point of view: it edits
//! files in the workspace and calls the daemon's MCP tools over HTTP. Each
//! message it receives runs the next turn of its script.
//!
//! Script file:
//!
//! ```json
//! { "turns": [ [ { "mcp": { "tool": "activity_claim", "arguments": { "requirement": "REQ-1" } } },
//!                { "write": { "path": "src/app.js", "content": "..." } } ],
//!              [ { "handle_annotations": { "reply": "Fixed." } } ] ],
//!   "read_only_turns": [ [ { "handle_annotations": { "reply": "Because ..." } } ] ] }
//! ```
//!
//! Read-only sessions (answering a question) run `read_only_turns`.
//!
//! The session id is `fake:<turns completed>`, so resuming continues the
//! script where the previous process stopped.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use aperture_core::Activity;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::{AgentEvent, Command, Session, SessionSpec};

#[derive(Debug, Deserialize)]
struct Script {
    turns: Vec<Vec<Step>>,
    /// Turns used by read-only sessions (answering a question) instead of
    /// `turns`, so a question does not replay the build.
    #[serde(default)]
    read_only_turns: Vec<Vec<Step>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    SleepMs(u64),
    Event(AgentEvent),
    Write { path: String, content: String },
    Mcp { tool: String, #[serde(default)] arguments: Value },
    /// Acknowledges every pending annotation, optionally edits a file, and
    /// reports each one as addressed with `reply`.
    HandleAnnotations {
        #[serde(default)]
        write: Option<FileWrite>,
        reply: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct FileWrite {
    path: String,
    content: String,
}

pub async fn start(spec: SessionSpec) -> Result<Session> {
    let path = spec
        .fake_script
        .clone()
        .context("the fake agent needs a script")?;
    let script: Script = serde_json::from_str(
        &tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("cannot read {}", path.display()))?,
    )
    .with_context(|| format!("invalid script {}", path.display()))?;

    let mut turn = spec
        .resume
        .as_deref()
        .and_then(|id| id.strip_prefix("fake:"))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);

    let (command_tx, mut command_rx) = mpsc::channel::<Command>(64);
    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(256);

    tokio::spawn(async move {
        let emit = |event: AgentEvent| {
            let tx = event_tx.clone();
            async move { tx.send(event).await.is_ok() }
        };
        let session = |turn: usize| AgentEvent::Session {
            id: format!("fake:{turn}"),
            model: Some("fake".into()),
            auth: None,
        };
        if !emit(session(turn)).await {
            return;
        }
        let _ = emit(AgentEvent::Activity {
            activity: Activity::WaitingForInput,
        })
        .await;

        'session: while let Some(command) = command_rx.recv().await {
            match command {
                Command::Stop => break,
                Command::Interrupt | Command::Approve { .. } => continue,
                Command::Send(_) => {}
            }
            let _ = emit(AgentEvent::Activity {
                activity: Activity::Working,
            })
            .await;
            let turns = if spec.read_only {
                &script.read_only_turns
            } else {
                &script.turns
            };
            let steps = turns.get(turn).cloned().unwrap_or_default();
            let mut ok = true;
            let mut summary = String::new();
            for step in steps {
                // A stop or interrupt between steps ends the turn early.
                match command_rx.try_recv() {
                    Ok(Command::Stop) => break 'session,
                    Ok(Command::Interrupt) => {
                        let _ = emit(AgentEvent::Activity {
                            activity: Activity::Interrupted,
                        })
                        .await;
                        continue 'session;
                    }
                    _ => {}
                }
                if let Err(err) = run_step(&spec, step, &event_tx).await {
                    ok = false;
                    summary = format!("{err:#}");
                    break;
                }
            }
            turn += 1;
            let _ = emit(session(turn)).await;
            let _ = emit(AgentEvent::TurnCompleted { ok, summary }).await;
            let _ = emit(AgentEvent::Activity {
                activity: Activity::WaitingForInput,
            })
            .await;
        }
        let _ = emit(AgentEvent::Exited { code: Some(0) }).await;
    });

    Ok(Session {
        commands: command_tx,
        events: event_rx,
        pid: None,
    })
}

async fn write_file(cwd: &Path, path: &str, content: &str) -> Result<PathBuf> {
    let full = cwd.join(path);
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&full, content).await?;
    Ok(full)
}

async fn run_step(spec: &SessionSpec, step: Step, events: &mpsc::Sender<AgentEvent>) -> Result<()> {
    match step {
        Step::SleepMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
        Step::Event(event) => {
            let _ = events.send(event).await;
        }
        Step::Write { path, content } => {
            write_file(&spec.cwd, &path, &content).await?;
            let _ = events.send(AgentEvent::FileChanged { paths: vec![path] }).await;
        }
        Step::Mcp { tool, arguments } => {
            call_tool(spec, &tool, arguments).await?;
        }
        Step::HandleAnnotations { write, reply } => {
            let pending = call_tool(spec, "annotations_list_pending", json!({})).await?;
            let annotations = pending["annotations"].as_array().cloned().unwrap_or_default();
            for annotation in &annotations {
                let id = annotation["id"].as_str().unwrap_or("");
                call_tool(spec, "annotations_ack", json!({ "id": id })).await?;
            }
            if !annotations.is_empty() {
                if let Some(write) = &write {
                    write_file(&spec.cwd, &write.path, &write.content).await?;
                    let _ = events
                        .send(AgentEvent::FileChanged {
                            paths: vec![write.path.clone()],
                        })
                        .await;
                }
            }
            for annotation in &annotations {
                let id = annotation["id"].as_str().unwrap_or("");
                call_tool(
                    spec,
                    "annotations_reply",
                    json!({ "id": id, "body": reply, "addressed": true }),
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Calls one of the daemon's MCP tools and returns its JSON result.
async fn call_tool(spec: &SessionSpec, tool: &str, arguments: Value) -> Result<Value> {
    let url = spec.mcp_url.as_deref().context("no MCP endpoint")?;
    let response: Value = reqwest::Client::new()
        .post(url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(error) = response.get("error") {
        bail!("{tool}: {}", error["message"].as_str().unwrap_or("error"));
    }
    let result = &response["result"];
    let text = result["content"][0]["text"].as_str().unwrap_or("null");
    if result["isError"].as_bool().unwrap_or(false) {
        bail!("{tool}: {text}");
    }
    Ok(serde_json::from_str(text).unwrap_or(Value::Null))
}
