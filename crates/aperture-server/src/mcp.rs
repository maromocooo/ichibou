//! The MCP server agents talk to (streamable HTTP, JSON responses).
//!
//! Each run gets its own URL with a random token in the path; the token maps
//! to the run, so every call is attributed without trusting what the agent
//! says about itself. There is deliberately no tool that marks a requirement
//! as complete.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use aperture_core::*;
use chrono::Utc;
use serde_json::{json, Value};

use crate::engine::{Engine, UiEvent};

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required }
    })
}

pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "requirements_get",
            "Read the requirements, their acceptance criteria, and the current verified status of each.",
            json!({}),
            &[],
        ),
        tool(
            "requirements_propose_change",
            "Propose the full requirements list. The user reviews it before it takes effect.",
            json!({ "requirements": { "type": "array", "items": { "type": "object", "properties": {
                "id": { "type": "string", "description": "Stable id, e.g. REQ-LOGIN-01" },
                "title": { "type": "string" },
                "description": { "type": "string" },
                "depends_on": { "type": "array", "items": { "type": "string" } },
                "criteria": { "type": "array", "items": { "type": "object", "properties": {
                    "id": { "type": "string" }, "text": { "type": "string" } }, "required": ["id", "text"] } },
                "manual_checks": { "type": "array", "items": { "type": "object", "properties": {
                    "id": { "type": "string" }, "text": { "type": "string" } }, "required": ["id", "text"] } }
            }, "required": ["id", "title", "criteria"] } } }),
            &["requirements"],
        ),
        tool("architecture_get", "Read the target architecture.", json!({}), &[]),
        tool(
            "architecture_propose_change",
            "Propose the full target architecture. The user reviews it before it takes effect.",
            json!({
                "nodes": { "type": "array", "items": { "type": "object", "properties": {
                    "id": { "type": "string" },
                    "kind": { "type": "string", "enum": ["client", "api", "service", "db", "queue", "external"] },
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Source paths that implement this node" },
                    "requirements": { "type": "array", "items": { "type": "string" } }
                }, "required": ["id", "kind", "title"] } },
                "edges": { "type": "array", "items": { "type": "object", "properties": {
                    "from": { "type": "string" }, "to": { "type": "string" }, "label": { "type": "string" },
                    "requirements": { "type": "array", "items": { "type": "string" } }
                }, "required": ["from", "to"] } }
            }),
            &["nodes"],
        ),
        tool("targets_get", "Read how the app is started, previewed and verified.", json!({}), &[]),
        tool(
            "targets_register",
            "Propose how the app is started for preview and verification, where its OpenAPI document is, and its databases. The user approves the commands before any of them runs.",
            json!({
                "targets": { "type": "array", "items": { "type": "object", "properties": {
                    "name": { "type": "string" },
                    "kind": { "type": "string", "enum": ["web", "ios", "android", "desktop"] },
                    "dev": { "type": "object", "description": "Live preview: { command, cwd?, url? with {port}, port? }" },
                    "verify": { "type": "object", "description": "Verification in a clean copy: { setup: [commands], start: { command, url }, copy: [ignored files], link: [ignored dirs such as node_modules] }" },
                    "app_id": { "type": "string" }
                }, "required": ["name", "kind"] } },
                "apis": { "type": "array", "items": { "type": "object", "properties": {
                    "node": { "type": "string" }, "openapi_file": { "type": "string" },
                    "openapi_url": { "type": "string" }, "openapi_command": { "type": "string" }, "base_url": { "type": "string" }
                }, "required": ["node"] } },
                "databases": { "type": "array", "items": { "type": "object", "properties": {
                    "node": { "type": "string" }, "kind": { "type": "string", "enum": ["postgres", "sqlite"] }, "url": { "type": "string" }
                }, "required": ["node", "kind", "url"] } }
            }),
            &["targets"],
        ),
        tool(
            "scenarios_propose",
            "Register the full list of scenario tests. Each needs the user's approval before its results count.",
            json!({ "scenarios": { "type": "array", "items": { "type": "object", "properties": {
                "id": { "type": "string" }, "title": { "type": "string" }, "target": { "type": "string" },
                "runner": { "type": "string", "enum": ["playwright", "maestro", "command"] },
                "covers": { "type": "array", "items": { "type": "object", "properties": {
                    "requirement": { "type": "string" }, "criterion": { "type": "string" } }, "required": ["requirement", "criterion"] } },
                "files": { "type": "array", "items": { "type": "string" } },
                "command": { "type": "string", "description": "For runner=command. {report} is the JUnit XML path to write, {url} the app URL." }
            }, "required": ["id", "title", "target", "runner", "covers"] } } }),
            &["scenarios"],
        ),
        tool(
            "activity_claim",
            "Say which requirement you are working on. Call again with done=true when its implementation is finished. This does not mark it complete; verification does.",
            json!({ "requirement": { "type": "string" }, "note": { "type": "string" }, "done": { "type": "boolean" } }),
            &["requirement"],
        ),
        tool(
            "annotations_list_pending",
            "List feedback from the user that is waiting for you.",
            json!({}),
            &[],
        ),
        tool(
            "annotations_ack",
            "Acknowledge that you received an annotation and will handle it.",
            json!({ "id": { "type": "string" } }),
            &["id"],
        ),
        tool(
            "annotations_reply",
            "Reply on an annotation. Set addressed=true when the fix is made or the question is answered.",
            json!({ "id": { "type": "string" }, "body": { "type": "string" }, "addressed": { "type": "boolean" } }),
            &["id", "body"],
        ),
        tool(
            "evidence_submit",
            "Attach a note for the user about how a requirement was implemented or checked. Informational only.",
            json!({ "requirement": { "type": "string" }, "note": { "type": "string" } }),
            &["requirement", "note"],
        ),
        tool(
            "verification_request",
            "Ask Aperture to run the approved scenarios against the current workspace. Returns immediately; read results with verification_status.",
            json!({}),
            &[],
        ),
        tool(
            "verification_status",
            "Read whether verification is running and which scenarios fail, with their output.",
            json!({}),
            &[],
        ),
    ]
}

/// Handles one JSON-RPC message. Returns `None` for notifications.
pub async fn handle(engine: &Arc<Engine>, run: &str, message: Value) -> Option<Value> {
    let id = message.get("id").cloned().filter(|v| !v.is_null())?;
    let method = message["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "aperture", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap_or("").to_string();
            let arguments = message["params"]["arguments"].clone();
            let outcome = call(engine, run, &name, arguments).await;
            let (text, is_error) = match outcome {
                Ok(value) => (value.to_string(), false),
                Err(err) => (format!("{err:#}"), true),
            };
            Ok(json!({ "content": [ { "type": "text", "text": text } ], "isError": is_error }))
        }
        other => Err(format!("method not found: {other}")),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(message) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } }),
    })
}

fn text<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    arguments[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("`{key}` is required"))
}

async fn call(engine: &Arc<Engine>, run: &str, name: &str, arguments: Value) -> Result<Value> {
    let role = engine
        .store
        .run(run)?
        .map(|r| r.role)
        .context("unknown run")?;
    let read_only = matches!(role, RunRole::Responder);
    match name {
        "requirements_get" => {
            let (manifest, status) = engine.status()?;
            let requirements: Vec<Value> = manifest
                .requirements
                .requirements
                .iter()
                .map(|req| {
                    let state = status.requirements.iter().find(|s| s.requirement == req.id);
                    json!({ "requirement": req, "status": state })
                })
                .collect();
            Ok(json!({ "requirements": requirements, "complete": status.complete }))
        }
        "architecture_get" => Ok(serde_json::to_value(engine.project.load()?.architecture)?),
        "targets_get" => Ok(serde_json::to_value(engine.project.load()?.targets)?),
        "requirements_propose_change" | "architecture_propose_change" | "targets_register" => {
            if read_only {
                bail!("this session cannot propose changes");
            }
            let kind = match name {
                "requirements_propose_change" => "requirements",
                "architecture_propose_change" => "architecture",
                _ => "targets",
            };
            engine.propose(kind, arguments)?;
            Ok(json!({ "status": "proposed", "note": "The user will review this before it takes effect." }))
        }
        "scenarios_propose" => {
            if read_only {
                bail!("this session cannot propose scenarios");
            }
            let doc: ScenariosDoc = serde_json::from_value(arguments)?;
            let manifest = engine.project.load()?;
            for scenario in &doc.scenarios {
                if !manifest.targets.targets.iter().any(|t| t.name == scenario.target) {
                    bail!("scenario {} uses unknown target {}", scenario.id, scenario.target);
                }
                for cover in &scenario.covers {
                    let known = manifest.requirements.requirements.iter().any(|r| {
                        r.id == cover.requirement && r.criteria.iter().any(|c| c.id == cover.criterion)
                    });
                    if !known {
                        bail!(
                            "scenario {} covers unknown criterion {}/{}",
                            scenario.id,
                            cover.requirement,
                            cover.criterion
                        );
                    }
                }
                for file in &scenario.files {
                    if !engine.project.root.join(file).is_file() {
                        bail!("scenario {} lists a file that does not exist: {file}", scenario.id);
                    }
                }
            }
            let uncovered: Vec<String> = manifest
                .requirements
                .requirements
                .iter()
                .flat_map(|r| r.criteria.iter().map(move |c| (r, c)))
                .filter(|(r, c)| {
                    !doc.scenarios.iter().any(|s| {
                        s.covers.iter().any(|x| x.requirement == r.id && x.criterion == c.id)
                    })
                })
                .map(|(r, c)| format!("{}/{}", r.id, c.id))
                .collect();
            engine.project.save_scenarios(&doc)?;
            engine.workspace_changed();
            engine.notify(UiEvent::Status);
            Ok(json!({ "status": "saved", "awaiting_user_approval": true, "uncovered_criteria": uncovered }))
        }
        "activity_claim" => {
            let requirement = text(&arguments, "requirement")?;
            let manifest = engine.project.load()?;
            if !manifest.requirements.requirements.iter().any(|r| r.id == requirement) {
                bail!("unknown requirement {requirement}");
            }
            engine.store.put_claim(&Claim {
                run: run.to_string(),
                requirement: requirement.to_string(),
                note: arguments["note"].as_str().unwrap_or("").to_string(),
                reported_done: arguments["done"].as_bool().unwrap_or(false),
                at: Utc::now(),
            })?;
            engine.notify(UiEvent::Status);
            Ok(json!({ "status": "recorded", "note": "Completion is decided by verification, not by this call." }))
        }
        "annotations_list_pending" => {
            let wanted = if read_only {
                AnnotationIntent::Question
            } else {
                AnnotationIntent::Fix
            };
            let local = engine.project.local_dir();
            let annotations: Vec<Value> = engine
                .store
                .annotations()?
                .into_iter()
                .filter(|a| {
                    a.intent == wanted
                        && matches!(
                            a.state,
                            AnnotationState::Queued | AnnotationState::Sent | AnnotationState::Acknowledged
                        )
                })
                .map(|a| {
                    json!({
                        "id": a.id,
                        "state": a.state,
                        "attached_to": a.anchor,
                        "message": a.body,
                        "requirements": a.requirements,
                        "screenshot": a.image.as_ref().map(|i| local.join(i)),
                        "thread": a.comments,
                    })
                })
                .collect();
            Ok(json!({ "annotations": annotations }))
        }
        "annotations_ack" => {
            let id = text(&arguments, "id")?;
            let updated = engine
                .store
                .update_annotation(id, |a| {
                    // Acknowledging twice, or after the work is done, changes nothing.
                    if matches!(a.state, AnnotationState::Queued | AnnotationState::Sent) {
                        a.state = AnnotationState::Acknowledged;
                    }
                })?
                .ok_or_else(|| anyhow!("unknown annotation {id}"))?;
            engine.notify(UiEvent::Annotations);
            Ok(json!({ "id": updated.id, "state": updated.state }))
        }
        "annotations_reply" => {
            let id = text(&arguments, "id")?;
            let body = text(&arguments, "body")?.to_string();
            engine.comment(id, Author::Agent, body)?;
            let mut state = None;
            if arguments["addressed"].as_bool().unwrap_or(false) {
                let updated = engine
                    .store
                    .update_annotation(id, |a| {
                        if a.state != AnnotationState::Resolved {
                            a.state = AnnotationState::Addressed;
                        }
                    })?
                    .ok_or_else(|| anyhow!("unknown annotation {id}"))?;
                state = Some(updated.state);
                engine.notify(UiEvent::Annotations);
                engine.notify(UiEvent::Status);
            }
            Ok(json!({ "id": id, "state": state, "note": "The user confirms whether this is resolved." }))
        }
        "evidence_submit" => {
            let requirement = text(&arguments, "requirement")?;
            let note = text(&arguments, "note")?;
            let seq = engine.store.append_event(
                Some(run),
                "agent_note",
                &json!({ "type": "agent_note", "requirement": requirement, "note": note }),
            )?;
            engine.notify(UiEvent::Event { seq });
            Ok(json!({ "status": "recorded", "note": "Notes are shown to the user but do not count as verification." }))
        }
        "verification_request" => {
            let _ = engine.refresh_snapshot();
            engine.request_verify();
            Ok(json!({ "status": "started" }))
        }
        "verification_status" => {
            let overview = engine.overview()?;
            let states: Vec<Value> = overview
                .status
                .requirements
                .iter()
                .map(|r| json!({ "requirement": r.requirement, "state": r.state }))
                .collect();
            Ok(json!({
                "verifying": overview.verifying,
                "targets_approved": overview.targets_approved,
                "requirements": states,
                "failing": engine.failure_report(None)?,
            }))
        }
        other => bail!("unknown tool {other}"),
    }
}
