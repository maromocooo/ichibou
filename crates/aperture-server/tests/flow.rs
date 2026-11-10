//! The whole loop through the daemon's HTTP API, with the scripted agent:
//! build -> verify -> check mark -> annotate -> fix -> edit demotes ->
//! restart keeps state.

use std::fs;
use std::path::Path;
use std::time::Duration;

use aperture_core::manifest::Project;
use aperture_core::*;
use serde_json::{json, Value};

struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    fn new(server: &aperture_server::Server) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: format!("http://127.0.0.1:{}", server.addr.port()),
            token: server.engine.token.clone(),
        }
    }

    async fn get(&self, path: &str) -> Value {
        self.http
            .get(format!("{}{path}", self.base))
            .header("x-aperture-token", &self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .header("x-aperture-token", &self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{path}: {value}");
        value
    }

    /// Polls the overview until `done` holds.
    async fn until(&self, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let mut last = Value::Null;
        for _ in 0..200 {
            last = self.get("/api/overview").await;
            if done(&last) {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for {what}: {}", last["status"]["requirements"]);
    }
}

fn requirement_state(overview: &Value) -> &str {
    overview["status"]["requirements"][0]["state"].as_str().unwrap_or("")
}

fn setup(root: &Path) {
    let project = Project::new(root);
    project.init().unwrap();
    fs::write(root.join("answer.txt"), "0\n").unwrap();
    fs::write(
        root.join("check.sh"),
        r#"if [ "$(cat answer.txt)" = "42" ]; then
  echo '<testsuite><testcase name="answer"/></testsuite>' > "$1"
else
  echo '<testsuite><testcase name="answer"><failure>wrong</failure></testcase></testsuite>' > "$1"
  exit 1
fi
"#,
    )
    .unwrap();
    project
        .save_requirements(&RequirementsDoc {
            requirements: vec![Requirement {
                id: "REQ-1".into(),
                title: "Answer".into(),
                description: String::new(),
                depends_on: vec![],
                criteria: vec![Criterion { id: "c1".into(), text: "answer is 42".into() }],
                manual_checks: vec![],
            }],
        })
        .unwrap();
    project
        .save_architecture(&ArchitectureDoc {
            nodes: vec![ArchNode {
                id: "core".into(),
                kind: NodeKind::Service,
                title: "Core".into(),
                description: String::new(),
                paths: vec!["answer.txt".into()],
                requirements: vec!["REQ-1".into()],
            }],
            edges: vec![],
        })
        .unwrap();
    project
        .save_targets(&TargetsDoc {
            targets: vec![Target { name: "cli".into(), kind: TargetKind::Desktop, dev: None, verify: None, app_id: None }],
            ..Default::default()
        })
        .unwrap();
    project
        .save_scenarios(&ScenariosDoc {
            scenarios: vec![Scenario {
                id: "answer".into(),
                title: "Answer".into(),
                target: "cli".into(),
                runner: RunnerKind::Command,
                covers: vec![Coverage { requirement: "REQ-1".into(), criterion: "c1".into() }],
                files: vec!["check.sh".into()],
                command: Some("sh check.sh {report}".into()),
            }],
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn build_verify_annotate_demote_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(&root).unwrap();
    setup(&root);
    // The script lives outside the project so it is not part of the snapshot.
    let script = dir.path().join("script.json");
    fs::write(
        &script,
        json!({ "turns": [
            [
                { "mcp": { "tool": "activity_claim", "arguments": { "requirement": "REQ-1" } } },
                { "write": { "path": "answer.txt", "content": "42\n" } },
                { "mcp": { "tool": "activity_claim", "arguments": { "requirement": "REQ-1", "done": true } } },
                { "event": { "type": "message", "text": "Implemented." } }
            ],
            [
                { "handle_annotations": { "write": { "path": "notes.txt", "content": "renamed\n" }, "reply": "Renamed it." } }
            ]
        ]})
        .to_string(),
    )
    .unwrap();

    let server = aperture_server::serve(&root, 0).await.unwrap();
    let api = Client::new(&server);

    // Requests without the token are refused.
    let anonymous = reqwest::get(format!("{}/api/overview", api.base)).await.unwrap();
    assert_eq!(anonymous.status(), 403);

    api.post(
        "/api/config",
        json!({ "default_agent": "fake", "claude_permission_mode": "acceptEdits",
                "fake_script": script.to_string_lossy(), "auto_verify": true }),
    )
    .await;
    // Bypassing permission checks is never accepted as a setting.
    let refused = api
        .http
        .post(format!("{}/api/config", api.base))
        .header("x-aperture-token", &api.token)
        .json(&json!({ "default_agent": "claude", "claude_permission_mode": "bypassPermissions", "auto_verify": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 400);

    // Nothing runs until the user approved the commands and the scenario.
    let overview = api.get("/api/overview").await;
    assert_eq!(overview["targets_approved"], false);
    assert_eq!(overview["status"]["architecture"]["nodes"]["core"], "present");
    api.post("/api/targets/approve", json!({})).await;
    api.post("/api/scenarios/approve-all", json!({})).await;

    // The starting point does not satisfy the requirement.
    api.post("/api/verify", json!({})).await;
    api.until("initial failure", |o| requirement_state(o) == "failing").await;

    // Build: the agent claims, writes, reports done; Aperture verifies.
    api.post("/api/runs", json!({ "kind": "build" })).await;
    let overview = api.until("done after build", |o| requirement_state(o) == "done").await;
    assert_eq!(overview["status"]["complete"], true);
    assert_eq!(overview["status"]["architecture"]["nodes"]["core"], "verified");
    assert_eq!(overview["runs"][0]["last_message"], "Implemented.");

    // A fix request blocks completion, reaches the idle writer, and comes
    // back addressed with a reply. The user decides when it is resolved.
    let annotation = api
        .post(
            "/api/annotations",
            json!({ "intent": "fix", "anchor": { "kind": "arch_node", "node": "core" }, "body": "Rename this." }),
        )
        .await;
    let id = annotation["id"].as_str().unwrap().to_string();
    assert_eq!(annotation["requirements"][0], "REQ-1");
    let overview = api
        .until("annotation addressed", |o| o["annotations"][0]["state"] == "addressed" && o["verifying"] == false
            && requirement_state(o) == "blocked_by_annotations")
        .await;
    assert_eq!(overview["annotations"][0]["comments"][0]["body"], "Renamed it.");
    assert_eq!(overview["annotations"][0]["comments"][0]["author"], "agent");
    assert!(root.join("notes.txt").exists());
    api.post(&format!("/api/annotations/{id}/resolve"), json!({})).await;
    api.until("done after resolve", |o| requirement_state(o) == "done").await;

    // Saving the manifest through the UI moves the snapshot too, and is
    // re-verified without anyone asking.
    let mut targets = api.get("/api/overview").await["manifest"]["targets"].clone();
    targets["targets"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "name": "phone", "kind": "ios" }));
    api.post("/api/manifest/targets", targets).await;
    let overview = api
        .until("done after manifest edit", |o| {
            requirement_state(o) == "done" && o["results"][0]["snapshot"] == o["status"]["snapshot"]
        })
        .await;
    assert_eq!(overview["manifest"]["targets"]["targets"][1]["name"], "phone");

    // Editing the code by hand demotes the pass; re-verification then fails.
    fs::write(root.join("answer.txt"), "41\n").unwrap();
    api.until("failing after edit", |o| requirement_state(o) == "failing").await;
    let overview = api.get("/api/overview").await;
    assert_eq!(overview["status"]["complete"], false);
    assert!(overview["results"][0]["detail"].as_str().unwrap().contains("1 failed"));

    // A second daemon for the same project is refused.
    assert!(aperture_server::serve(&root, 0).await.is_err());

    // Restart: annotations, results and runs are still there.
    server.shutdown().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let server = aperture_server::serve(&root, 0).await.unwrap();
    let api = Client::new(&server);
    let overview = api.get("/api/overview").await;
    assert_eq!(overview["annotations"][0]["state"], "resolved");
    assert_eq!(requirement_state(&overview), "failing");
    assert!(overview["live_runs"].as_array().unwrap().is_empty());
    // A deliberate shutdown is not an interruption: nothing is resumed.
    assert!(overview["runs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["activity"] == "finished"));
    assert!(server.engine.resume_interrupted().await.unwrap().is_none());
    assert!(!overview["runs"].as_array().unwrap().is_empty());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn proposals_are_not_applied_until_the_user_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(&root).unwrap();
    let script = dir.path().join("script.json");
    fs::write(
        &script,
        json!({ "turns": [
            [
                { "mcp": { "tool": "requirements_propose_change", "arguments": { "requirements": [
                    { "id": "REQ-A", "title": "A", "criteria": [ { "id": "c1", "text": "works" } ] } ] } } },
                { "mcp": { "tool": "targets_register", "arguments": { "targets": [
                    { "name": "web", "kind": "web", "dev": { "command": "npm run dev", "url": "http://localhost:{port}" } } ] } } }
            ]
        ]})
        .to_string(),
    )
    .unwrap();
    let server = aperture_server::serve(&root, 0).await.unwrap();
    let api = Client::new(&server);
    api.post(
        "/api/config",
        json!({ "default_agent": "fake", "claude_permission_mode": "acceptEdits",
                "fake_script": script.to_string_lossy(), "auto_verify": true }),
    )
    .await;

    api.post("/api/runs", json!({ "kind": "plan", "text": "Build A" })).await;
    let overview = api
        .until("proposals", |o| !o["proposals"]["requirements"].is_null() && !o["proposals"]["targets"].is_null())
        .await;
    // Proposed, not applied.
    assert!(overview["manifest"]["requirements"]["requirements"].as_array().unwrap().is_empty());
    assert!(!root.join(".aperture/requirements.yaml").exists());

    api.post("/api/proposals/requirements", json!({ "accept": true })).await;
    api.post("/api/proposals/targets", json!({ "accept": false })).await;
    let overview = api.get("/api/overview").await;
    assert_eq!(overview["manifest"]["requirements"]["requirements"][0]["id"], "REQ-A");
    assert!(overview["manifest"]["targets"]["targets"].as_array().unwrap().is_empty());
    assert!(overview["proposals"]["requirements"].is_null());
    assert_eq!(overview["status"]["requirements"][0]["criteria"][0]["state"], "no_scenario");
    assert_eq!(overview["status"]["requirements"][0]["schedule"]["kind"], "next");
    server.shutdown().await;
}
