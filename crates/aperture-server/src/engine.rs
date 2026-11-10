//! The daemon's state and orchestration: runs, annotations, verification.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use aperture_agents::{AgentEvent, Command, SessionSpec};
use aperture_core::manifest::{Manifest, Project};
use aperture_core::status::{project_status, Evidence, ProjectStatus};
use aperture_core::store::Store;
use aperture_core::*;
use aperture_verify::{Unstable, Verifier};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc};

use crate::prompts;

/// How many times a writer is told about failing scenarios before Aperture
/// stops repeating itself and leaves it to the user.
const MAX_NUDGES: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub default_agent: AgentKind,
    /// Permission mode for Claude sessions that write. Aperture never picks a
    /// mode that bypasses permission checks on its own.
    pub claude_permission_mode: String,
    #[serde(default)]
    pub fake_script: Option<String>,
    pub auto_verify: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_agent: AgentKind::Claude,
            claude_permission_mode: "acceptEdits".into(),
            fake_script: None,
            auto_verify: true,
        }
    }
}

/// What changed, so the UI knows what to fetch again.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Status,
    Runs,
    Annotations,
    Proposals,
    Preview,
    Event { seq: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingApproval {
    pub run: String,
    pub id: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Proposals {
    pub requirements: Option<RequirementsDoc>,
    pub architecture: Option<ArchitectureDoc>,
    pub targets: Option<TargetsDoc>,
}

#[derive(Debug, Serialize)]
pub struct Overview {
    pub project_root: String,
    pub manifest: Manifest,
    pub status: ProjectStatus,
    /// Latest result per scenario.
    pub results: Vec<VerificationResult>,
    pub runs: Vec<Run>,
    pub live_runs: Vec<String>,
    pub annotations: Vec<Annotation>,
    pub proposals: Proposals,
    pub approvals: Vec<PendingApproval>,
    pub verifying: bool,
    pub config: Config,
    /// Whether the commands in targets.yaml were approved by the user.
    pub targets_approved: bool,
    /// Whether requirements.yaml is the version the user approved.
    pub requirements_approved: bool,
    /// Hashes of the manifest files this overview was built from. The UI
    /// sends them back when the user approves what it displayed.
    pub targets_hash: String,
    pub requirements_hash: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Plan,
    Scenarios,
    Build,
    Onboard,
}

#[derive(Debug, Deserialize)]
pub struct NewAnnotation {
    pub intent: AnnotationIntent,
    pub anchor: Anchor,
    pub body: String,
    #[serde(default)]
    pub requirements: Vec<String>,
    /// PNG bytes, base64-encoded (no data-URL prefix).
    #[serde(default)]
    pub image_base64: Option<String>,
}

struct Live {
    commands: mpsc::Sender<Command>,
    role: RunRole,
    generation: u64,
    /// Aperture asked this session to stop, so its exit is not a failure.
    stopping: bool,
}

pub struct Engine {
    pub project: Project,
    pub store: Arc<Store>,
    /// Secret the UI must present on every request.
    pub token: String,
    port: AtomicU16,
    bus: broadcast::Sender<UiEvent>,
    live: Mutex<HashMap<String, Live>>,
    /// MCP token -> run id.
    mcp_tokens: Mutex<HashMap<String, String>>,
    snapshot: Mutex<Option<SnapshotId>>,
    verify_lock: tokio::sync::Mutex<()>,
    verifying: AtomicBool,
    verify_again: AtomicBool,
    approvals: Mutex<Vec<PendingApproval>>,
    nudges: Mutex<HashMap<String, u32>>,
    /// Snapshot of the last completed verification run.
    last_verified: Mutex<Option<SnapshotId>>,
    pub previews: crate::preview::Previews,
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Roles that may modify the workspace. Only one such session runs at a time.
fn takes_writer_slot(role: RunRole) -> bool {
    matches!(role, RunRole::Writer | RunRole::ScenarioAuthor)
}

fn is_over(activity: Activity) -> bool {
    matches!(activity, Activity::Finished | Activity::Failed)
}

impl Engine {
    pub fn open(root: impl Into<std::path::PathBuf>) -> Result<Arc<Self>> {
        let project = Project::new(root);
        project.init()?;
        let store = Arc::new(Store::open(&project.local_dir().join("state.db"))?);
        let (bus, _) = broadcast::channel(256);
        let engine = Arc::new(Self {
            project,
            store,
            token: uuid::Uuid::new_v4().simple().to_string(),
            port: AtomicU16::new(0),
            bus,
            live: Mutex::new(HashMap::new()),
            mcp_tokens: Mutex::new(HashMap::new()),
            snapshot: Mutex::new(None),
            verify_lock: tokio::sync::Mutex::new(()),
            verifying: AtomicBool::new(false),
            verify_again: AtomicBool::new(false),
            approvals: Mutex::new(Vec::new()),
            nudges: Mutex::new(HashMap::new()),
            last_verified: Mutex::new(None),
            previews: Default::default(),
        });
        engine.recover()?;
        Ok(engine)
    }

    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<UiEvent> {
        self.bus.subscribe()
    }

    pub fn notify(&self, event: UiEvent) {
        let _ = self.bus.send(event);
    }

    // -- configuration -----------------------------------------------------

    pub fn config(&self) -> Config {
        self.store
            .kv_get("config")
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn set_config(&self, config: &Config) -> Result<()> {
        if config.claude_permission_mode == "bypassPermissions" {
            bail!("Aperture does not run agents with permission checks bypassed");
        }
        self.store.kv_set("config", &serde_json::to_string(config)?)
    }

    // -- status ------------------------------------------------------------

    /// Current snapshot of the workspace, cached until a file changes.
    pub fn current_snapshot(&self) -> Result<SnapshotId> {
        if let Some(id) = lock(&self.snapshot).clone() {
            return Ok(id);
        }
        let id = snapshot::take(&self.project.root)?;
        *lock(&self.snapshot) = Some(id.clone());
        Ok(id)
    }

    /// Called when files changed. Returns true when the snapshot moved.
    pub fn refresh_snapshot(&self) -> Result<bool> {
        let id = snapshot::take(&self.project.root)?;
        let mut cached = lock(&self.snapshot);
        let changed = cached.as_ref() != Some(&id);
        *cached = Some(id);
        Ok(changed)
    }

    /// Call after anything may have changed files in the workspace. A moved
    /// snapshot demotes earlier passes; when no agent is mid-turn the
    /// scenarios are run again. Returns true when the snapshot moved.
    pub fn workspace_changed(self: &Arc<Self>) -> bool {
        let changed = self.refresh_snapshot().unwrap_or(false);
        if changed {
            self.notify(UiEvent::Status);
            if self.config().auto_verify && !self.writer_busy() {
                self.request_verify();
            }
        }
        changed
    }

    fn live_ids(&self) -> Vec<String> {
        lock(&self.live).keys().cloned().collect()
    }

    pub fn status(&self) -> Result<(Manifest, ProjectStatus)> {
        let manifest = self.project.load()?;
        let current = self.current_snapshot()?;
        let mut definitions = HashMap::new();
        for scenario in &manifest.scenarios.scenarios {
            definitions.insert(
                scenario.id.clone(),
                self.project.scenario_definition(scenario)?,
            );
        }
        let live = self.live_ids();
        // A claim from a session that is gone only matters if it reported the
        // work as finished; otherwise nobody is working on it any more.
        let claims: Vec<Claim> = self
            .store
            .claims()?
            .into_iter()
            .filter(|c| c.reported_done || live.contains(&c.run))
            .collect();
        let annotations = self.store.annotations()?;
        let results = self.store.results()?;
        let approvals = self.store.approvals()?;
        let confirmations = self.store.confirmations()?;
        let evidence = Evidence {
            current: &current,
            results: &results,
            approvals: &approvals,
            definitions: &definitions,
            confirmations: &confirmations,
            claims: &claims,
            annotations: &annotations,
        };
        let root = self.project.root.clone();
        let mut status = project_status(&manifest, &evidence, &|path| root.join(path).exists());
        // A fix request that is not tied to a requirement still blocks the
        // overall "request complete" state.
        let unlinked_fix = annotations.iter().any(|a| {
            a.intent == AnnotationIntent::Fix
                && a.state != AnnotationState::Resolved
                && a.requirements.is_empty()
        });
        status.complete = status.complete && !unlinked_fix && self.requirements_approved();
        Ok((manifest, status))
    }

    pub fn proposals(&self) -> Proposals {
        let get = |key: &str| self.store.kv_get(key).ok().flatten();
        Proposals {
            requirements: get("proposal:requirements").and_then(|t| serde_json::from_str(&t).ok()),
            architecture: get("proposal:architecture").and_then(|t| serde_json::from_str(&t).ok()),
            targets: get("proposal:targets").and_then(|t| serde_json::from_str(&t).ok()),
        }
    }

    /// Hash of a manifest file as it is on disk now.
    pub fn manifest_hash(&self, file: &str) -> String {
        let bytes = std::fs::read(self.project.manifest_dir().join(file)).unwrap_or_default();
        hash(&bytes)
    }

    /// Commands and URLs in targets.yaml are only used while the file is
    /// exactly the version the user approved.
    pub fn targets_approved(&self) -> bool {
        self.store.kv_get("targets_approved").ok().flatten()
            == Some(self.manifest_hash("targets.yaml"))
    }

    /// The requirements file is in the workspace, where an agent could edit
    /// it (dropping a requirement would make the rest look complete). The
    /// request only counts as complete for the version the user approved.
    pub fn requirements_approved(&self) -> bool {
        self.store.kv_get("requirements_approved").ok().flatten()
            == Some(self.manifest_hash("requirements.yaml"))
    }

    /// Approves a manifest file. `seen` is the hash of the content the user
    /// was shown; if the file has changed since, the approval is refused
    /// rather than silently applied to something they never read.
    fn approve(&self, file: &str, key: &str, seen: &str) -> Result<()> {
        if self.manifest_hash(file) != seen {
            bail!("{file} changed after it was shown to you; review it again");
        }
        self.store.kv_set(key, seen)?;
        self.notify(UiEvent::Status);
        Ok(())
    }

    pub fn approve_targets(&self, seen: &str) -> Result<()> {
        self.approve("targets.yaml", "targets_approved", seen)
    }

    pub fn approve_requirements(&self, seen: &str) -> Result<()> {
        self.approve("requirements.yaml", "requirements_approved", seen)
    }

    /// Records approval of content Aperture itself just wrote on the user's
    /// behalf, hashed from the written bytes rather than re-read from disk.
    pub fn approve_written(&self, key: &str, written: &str) -> Result<()> {
        self.store.kv_set(key, &hash(written.as_bytes()))?;
        self.notify(UiEvent::Status);
        Ok(())
    }

    pub fn overview(&self) -> Result<Overview> {
        // Hash before loading: if a file changes in between, the hash is of
        // the older content and the approval will be refused, never the
        // other way round.
        let targets_hash = self.manifest_hash("targets.yaml");
        let requirements_hash = self.manifest_hash("requirements.yaml");
        let (manifest, status) = self.status()?;
        let mut latest: HashMap<String, VerificationResult> = HashMap::new();
        for result in self.store.results()? {
            latest.insert(result.scenario.clone(), result);
        }
        let mut results: Vec<VerificationResult> = latest.into_values().collect();
        results.sort_by(|a, b| a.scenario.cmp(&b.scenario));
        Ok(Overview {
            project_root: self.project.root.to_string_lossy().into_owned(),
            manifest,
            status,
            results,
            runs: self.store.runs()?,
            live_runs: self.live_ids(),
            annotations: self.store.annotations()?,
            proposals: self.proposals(),
            approvals: lock(&self.approvals).clone(),
            verifying: self.verifying.load(Ordering::SeqCst),
            config: self.config(),
            targets_approved: self.targets_approved(),
            requirements_approved: self.requirements_approved(),
            targets_hash,
            requirements_hash,
        })
    }

    // -- proposals ---------------------------------------------------------

    pub fn propose(&self, kind: &str, doc: serde_json::Value) -> Result<()> {
        match kind {
            "requirements" => {
                let doc: RequirementsDoc = serde_json::from_value(doc)?;
                manifest::validate_requirements(&doc)?;
                self.store
                    .kv_set("proposal:requirements", &serde_json::to_string(&doc)?)?;
            }
            "architecture" => {
                let doc: ArchitectureDoc = serde_json::from_value(doc)?;
                manifest::validate_architecture(&doc)?;
                self.store
                    .kv_set("proposal:architecture", &serde_json::to_string(&doc)?)?;
            }
            "targets" => {
                let doc: TargetsDoc = serde_json::from_value(doc)?;
                self.store
                    .kv_set("proposal:targets", &serde_json::to_string(&doc)?)?;
            }
            other => bail!("unknown proposal kind {other}"),
        }
        self.notify(UiEvent::Proposals);
        Ok(())
    }

    /// Accepts or discards a proposal. Accepting targets also approves their
    /// commands, because the user just reviewed them.
    pub fn decide_proposal(self: &Arc<Self>, kind: &str, accept: bool) -> Result<()> {
        let key = format!("proposal:{kind}");
        let Some(text) = self.store.kv_get(&key)? else {
            bail!("there is no pending {kind} proposal");
        };
        if accept {
            match kind {
                "requirements" => {
                    let written = self.project.save_requirements(&serde_json::from_str(&text)?)?;
                    self.approve_written("requirements_approved", &written)?;
                }
                "architecture" => self.project.save_architecture(&serde_json::from_str(&text)?)?,
                "targets" => {
                    let written = self.project.save_targets(&serde_json::from_str(&text)?)?;
                    self.approve_written("targets_approved", &written)?;
                }
                other => bail!("unknown proposal kind {other}"),
            }
        }
        self.store.kv_set(&key, "null")?;
        self.workspace_changed();
        self.notify(UiEvent::Proposals);
        self.notify(UiEvent::Status);
        Ok(())
    }

    // -- scenarios and verification ----------------------------------------

    pub fn approve_scenario(self: &Arc<Self>, id: &str) -> Result<()> {
        let manifest = self.project.load()?;
        let scenario = manifest
            .scenarios
            .scenarios
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow!("unknown scenario {id}"))?;
        let (approval, frozen) = aperture_verify::freeze(&self.project, scenario)?;
        self.store.approve_scenario(&approval, &frozen)?;
        self.notify(UiEvent::Status);
        Ok(())
    }

    pub fn confirm_manual(&self, requirement: &str, check: &str, confirmed: bool) -> Result<()> {
        if confirmed {
            self.store.confirm_manual(&ManualConfirmation {
                requirement: requirement.into(),
                check: check.into(),
                snapshot: self.current_snapshot()?,
                at: Utc::now(),
            })?;
        } else {
            self.store.unconfirm_manual(requirement, check)?;
        }
        self.notify(UiEvent::Status);
        Ok(())
    }

    /// Runs the scenarios when the newest verdicts are for an older snapshot
    /// (for example after files changed while the daemon was not running).
    pub fn verify_if_stale(self: &Arc<Self>) {
        if !self.config().auto_verify || !self.targets_approved() {
            return;
        }
        let Ok(current) = self.current_snapshot() else {
            return;
        };
        let results = self.store.results().unwrap_or_default();
        // Stale when any approved scenario lacks a verdict for the current
        // snapshot and its approved definition.
        let stale = self.store.approvals().unwrap_or_default().iter().any(|a| {
            !results.iter().any(|r| {
                r.scenario == a.scenario && r.definition == a.definition && r.snapshot == current
            })
        });
        if stale {
            self.request_verify();
        }
    }

    /// Starts a verification run in the background. If one is already
    /// running, another is queued to start when it finishes.
    pub fn request_verify(self: &Arc<Self>) {
        if self.verifying.swap(true, Ordering::SeqCst) {
            self.verify_again.store(true, Ordering::SeqCst);
            return;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            let _guard = engine.verify_lock.lock().await;
            loop {
                engine.verify_again.store(false, Ordering::SeqCst);
                engine.notify(UiEvent::Status);
                engine.verify_once().await;
                if !engine.verify_again.load(Ordering::SeqCst) {
                    break;
                }
            }
            engine.verifying.store(false, Ordering::SeqCst);
            engine.notify(UiEvent::Status);
            engine.after_verify().await;
        });
    }

    async fn verify_once(self: &Arc<Self>) {
        if !self.targets_approved() {
            let _ = self.store.append_event(
                None,
                "verify_blocked",
                &serde_json::json!({ "reason": "targets.yaml commands are not approved" }),
            );
            return;
        }
        let verifier = Verifier {
            project: self.project.clone(),
            store: self.store.clone(),
        };
        for attempt in 0..5 {
            match verifier.run(None).await {
                Ok(report) => {
                    *lock(&self.last_verified) = Some(report.snapshot.clone());
                    let summary: Vec<_> = report
                        .results
                        .iter()
                        .map(|r| serde_json::json!({ "scenario": r.scenario, "outcome": r.outcome }))
                        .collect();
                    if let Ok(seq) = self.store.append_event(
                        None,
                        "verified",
                        &serde_json::json!({ "snapshot": report.snapshot, "results": summary }),
                    ) {
                        self.notify(UiEvent::Event { seq });
                    }
                    return;
                }
                Err(err) if err.is::<Unstable>() => {
                    // An agent is mid-edit. Wait for it to settle.
                    tokio::time::sleep(std::time::Duration::from_secs(2 + attempt)).await;
                }
                Err(err) => {
                    let _ = self.store.append_event(
                        None,
                        "verify_error",
                        &serde_json::json!({ "message": format!("{err:#}") }),
                    );
                    return;
                }
            }
        }
    }

    /// Text describing scenarios that currently fail, for an agent to read.
    pub fn failure_report(&self, requirements: Option<&[String]>) -> Result<String> {
        let (manifest, status) = self.status()?;
        let results = self.store.results()?;
        let mut out = String::new();
        for scenario in &status.scenarios {
            let failing = matches!(
                scenario.state,
                status::CriterionState::Failed
                    | status::CriterionState::Error
                    | status::CriterionState::Aborted
                    | status::CriterionState::Skipped
            );
            if !failing {
                continue;
            }
            let Some(definition) = manifest.scenarios.scenarios.iter().find(|s| s.id == scenario.scenario)
            else {
                continue;
            };
            if let Some(wanted) = requirements {
                if !definition.covers.iter().any(|c| wanted.contains(&c.requirement)) {
                    continue;
                }
            }
            let detail = scenario
                .last_result
                .as_ref()
                .and_then(|id| results.iter().find(|r| &r.id == id))
                .map(|r| r.detail.clone())
                .unwrap_or_default();
            let covers: Vec<String> = definition
                .covers
                .iter()
                .map(|c| format!("{}/{}", c.requirement, c.criterion))
                .collect();
            out.push_str(&format!(
                "## {} ({:?}) — covers {}\nfiles: {}\n{}\n\n",
                definition.id,
                scenario.state,
                covers.join(", "),
                definition.files.join(", "),
                detail
            ));
        }
        Ok(out)
    }

    /// After a verification run: tell an idle writer about failures on the
    /// requirements it claimed. Bounded, so a writer that cannot fix
    /// something is not prompted forever.
    async fn after_verify(self: &Arc<Self>) {
        let Some((run, commands)) = self.idle_writer() else {
            return;
        };
        let claimed: Vec<String> = self
            .store
            .claims()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.run == run)
            .map(|c| c.requirement)
            .collect();
        if claimed.is_empty() {
            return;
        }
        let report = self.failure_report(Some(&claimed)).unwrap_or_default();
        if report.is_empty() {
            lock(&self.nudges).remove(&run);
            return;
        }
        {
            let mut nudges = lock(&self.nudges);
            let count = nudges.entry(run.clone()).or_insert(0);
            if *count >= MAX_NUDGES {
                return;
            }
            *count += 1;
        }
        let _ = commands
            .send(Command::Send(prompts::verification_failed(&report)))
            .await;
    }

    // -- runs --------------------------------------------------------------

    fn idle_writer(&self) -> Option<(String, mpsc::Sender<Command>)> {
        let live = lock(&self.live);
        live.iter()
            .filter(|(_, l)| l.role == RunRole::Writer && !l.stopping)
            .find(|(id, _)| {
                self.store
                    .run(id)
                    .ok()
                    .flatten()
                    .is_some_and(|r| r.activity == Activity::WaitingForInput)
            })
            .map(|(id, l)| (id.clone(), l.commands.clone()))
    }

    fn live_writer(&self) -> Option<(String, mpsc::Sender<Command>)> {
        lock(&self.live)
            .iter()
            .find(|(_, l)| l.role == RunRole::Writer && !l.stopping)
            .map(|(id, l)| (id.clone(), l.commands.clone()))
    }

    /// Whether a session holding the writer slot is in the middle of a turn.
    pub fn writer_busy(&self) -> bool {
        let ids: Vec<String> = lock(&self.live)
            .iter()
            .filter(|(_, l)| takes_writer_slot(l.role) && !l.stopping)
            .map(|(id, _)| id.clone())
            .collect();
        ids.iter().any(|id| {
            self.store.run(id).ok().flatten().is_some_and(|r| {
                matches!(r.activity, Activity::Working | Activity::RunningTool)
            })
        })
    }

    fn last_writer_run(&self) -> Option<Run> {
        self.store
            .runs()
            .ok()?
            .into_iter()
            .filter(|r| r.role == RunRole::Writer && r.session.is_some())
            .next_back()
    }

    pub async fn start(
        self: &Arc<Self>,
        kind: RunKind,
        agent: Option<AgentKind>,
        text: Option<String>,
    ) -> Result<Run> {
        let agent = agent.unwrap_or(self.config().default_agent);
        match kind {
            RunKind::Plan => {
                let request = text.filter(|t| !t.trim().is_empty()).context("describe what to build")?;
                self.spawn(agent, RunRole::Writer, None, prompts::plan(&request)).await
            }
            RunKind::Scenarios => {
                self.spawn(agent, RunRole::ScenarioAuthor, None, prompts::author_scenarios())
                    .await
            }
            RunKind::Onboard => {
                self.spawn(agent, RunRole::Onboarding, None, prompts::onboard()).await
            }
            RunKind::Build => {
                let message = text.unwrap_or_else(prompts::build);
                // Keep the planning session's context when there is one.
                if let Some((id, commands)) = self.live_writer() {
                    commands.send(Command::Send(message)).await?;
                    return self.store.run(&id)?.context("run disappeared");
                }
                match self.last_writer_run() {
                    Some(previous) => {
                        self.spawn(previous.agent, RunRole::Writer, previous.session, message)
                            .await
                    }
                    None => self.spawn(agent, RunRole::Writer, None, message).await,
                }
            }
        }
    }

    /// Boxed because starting a run can, through event handling, lead to
    /// starting another one; the recursion needs a nameable future type.
    fn spawn<'a>(
        self: &'a Arc<Self>,
        agent: AgentKind,
        role: RunRole,
        resume: Option<String>,
        first_message: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Run>> + Send + 'a>> {
        Box::pin(self.spawn_inner(agent, role, resume, first_message))
    }

    async fn spawn_inner(
        self: &Arc<Self>,
        agent: AgentKind,
        role: RunRole,
        resume: Option<String>,
        first_message: String,
    ) -> Result<Run> {
        let config = self.config();
        let generation = if takes_writer_slot(role) {
            // One writer at a time: whoever held the slot is stopped, and
            // anything it still sends is ignored because its generation is old.
            let holders: Vec<(String, mpsc::Sender<Command>)> = {
                let mut live = lock(&self.live);
                live.iter_mut()
                    .filter(|(_, l)| takes_writer_slot(l.role))
                    .map(|(id, l)| {
                        l.stopping = true;
                        (id.clone(), l.commands.clone())
                    })
                    .collect()
            };
            for (_, commands) in holders {
                let _ = commands.send(Command::Stop).await;
            }
            self.store.next_generation()?
        } else {
            self.store.generation()?
        };

        let id = uuid::Uuid::new_v4().to_string();
        let mcp_token = uuid::Uuid::new_v4().simple().to_string();
        let read_only = matches!(role, RunRole::Responder | RunRole::Onboarding);
        let spec = SessionSpec {
            agent,
            cwd: self.project.root.clone(),
            resume,
            read_only,
            instructions: prompts::instructions(role),
            mcp_url: Some(format!("http://127.0.0.1:{}/mcp/{mcp_token}", self.port())),
            claude_permission_mode: config.claude_permission_mode.clone(),
            fake_script: config.fake_script.clone().map(Into::into),
            program: None,
        };
        let session = aperture_agents::start(spec).await?;
        let now = Utc::now();
        let run = Run {
            id: id.clone(),
            agent,
            role,
            activity: Activity::Working,
            session: None,
            generation,
            pid: session.pid,
            auth: None,
            last_message: String::new(),
            started_at: now,
            updated_at: now,
        };
        self.store.put_run(&run)?;
        lock(&self.mcp_tokens).insert(mcp_token.clone(), id.clone());
        lock(&self.live).insert(
            id.clone(),
            Live {
                commands: session.commands.clone(),
                role,
                generation,
                stopping: false,
            },
        );
        session.commands.send(Command::Send(first_message)).await?;
        self.notify(UiEvent::Runs);

        let engine = self.clone();
        let commands = session.commands.clone();
        let mut events = session.events;
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                engine.on_event(&id, &commands, event).await;
            }
            engine.on_gone(&id, &mcp_token);
        });
        Ok(run)
    }

    fn update_run(&self, id: &str, change: impl FnOnce(&mut Run)) {
        if let Ok(Some(mut run)) = self.store.run(id) {
            change(&mut run);
            run.updated_at = Utc::now();
            let _ = self.store.put_run(&run);
        }
    }

    async fn on_event(self: &Arc<Self>, run: &str, commands: &mpsc::Sender<Command>, event: AgentEvent) {
        let (role, generation, stopping) = match lock(&self.live).get(run) {
            Some(l) => (l.role, l.generation, l.stopping),
            None => return,
        };
        // A session that lost the writer slot may still be emitting events;
        // they no longer describe the workspace.
        if takes_writer_slot(role) && self.store.generation().unwrap_or(0) != generation {
            return;
        }

        let record = !matches!(
            event,
            AgentEvent::Unknown { .. }
                | AgentEvent::Activity { .. }
                | AgentEvent::Session { .. }
                | AgentEvent::ToolFinished { .. }
                | AgentEvent::RateLimit { limited: false, .. }
        );
        if record {
            if let Ok(payload) = serde_json::to_value(&event) {
                let kind = payload["type"].as_str().unwrap_or("event").to_string();
                if let Ok(seq) = self.store.append_event(Some(run), &kind, &payload) {
                    self.notify(UiEvent::Event { seq });
                }
            }
        }

        match event {
            AgentEvent::Session { id, auth, .. } => self.update_run(run, |r| {
                r.session = Some(id);
                r.auth = auth;
            }),
            AgentEvent::Activity { activity } => self.update_run(run, |r| {
                // Keep showing a rate limit until the agent works again.
                if r.activity != Activity::RateLimited || activity != Activity::WaitingForInput {
                    r.activity = activity;
                }
            }),
            AgentEvent::Message { text } => self.update_run(run, |r| r.last_message = text),
            AgentEvent::RateLimit { limited: true, .. } => {
                self.update_run(run, |r| r.activity = Activity::RateLimited)
            }
            AgentEvent::ApprovalRequest { id, kind, detail } => {
                lock(&self.approvals).push(PendingApproval {
                    run: run.to_string(),
                    id,
                    kind,
                    detail,
                });
            }
            AgentEvent::TurnCompleted { .. } => {
                self.update_run(run, |r| {
                    if r.activity != Activity::RateLimited {
                        r.activity = Activity::WaitingForInput;
                    }
                });
                match role {
                    RunRole::Writer if !stopping => {
                        // A turn boundary is the safe point to hand over
                        // feedback; otherwise check what was built.
                        if !self.deliver_fixes().await && self.config().auto_verify {
                            let _ = self.refresh_snapshot();
                            // Nothing changed since the last run: its
                            // verdicts still stand.
                            let verified = lock(&self.last_verified).clone();
                            if verified != self.current_snapshot().ok() {
                                self.request_verify();
                            }
                        }
                    }
                    RunRole::Writer => {}
                    // Single-purpose sessions end after their one turn.
                    _ => {
                        if let Some(l) = lock(&self.live).get_mut(run) {
                            l.stopping = true;
                        }
                        let _ = commands.send(Command::Stop).await;
                    }
                }
            }
            AgentEvent::Exited { code } => {
                let clean = stopping || code == Some(0);
                self.update_run(run, |r| {
                    r.activity = if clean {
                        Activity::Finished
                    } else {
                        Activity::Failed
                    }
                });
            }
            _ => {}
        }
        self.notify(UiEvent::Runs);
        self.notify(UiEvent::Status);
    }

    fn on_gone(&self, run: &str, mcp_token: &str) {
        lock(&self.live).remove(run);
        lock(&self.mcp_tokens).remove(mcp_token);
        lock(&self.approvals).retain(|a| a.run != run);
        self.update_run(run, |r| {
            if !is_over(r.activity) {
                r.activity = Activity::Finished;
            }
        });
        self.notify(UiEvent::Runs);
        self.notify(UiEvent::Status);
    }

    pub fn run_for_mcp_token(&self, token: &str) -> Option<String> {
        lock(&self.mcp_tokens).get(token).cloned()
    }

    pub async fn send_message(self: &Arc<Self>, run: &str, text: String) -> Result<()> {
        let commands = lock(&self.live)
            .get(run)
            .map(|l| l.commands.clone())
            .context("this run is not active")?;
        // The user spoke, so the failure reminders may start over.
        lock(&self.nudges).remove(run);
        if let Ok(seq) = self.store.append_event(
            Some(run),
            "user_message",
            &serde_json::json!({ "type": "user_message", "text": text }),
        ) {
            self.notify(UiEvent::Event { seq });
        }
        commands.send(Command::Send(text)).await?;
        Ok(())
    }

    pub async fn interrupt(&self, run: &str) -> Result<()> {
        let commands = lock(&self.live)
            .get(run)
            .map(|l| l.commands.clone())
            .context("this run is not active")?;
        commands.send(Command::Interrupt).await?;
        Ok(())
    }

    pub async fn stop_run(&self, run: &str) -> Result<()> {
        let commands = {
            let mut live = lock(&self.live);
            let l = live.get_mut(run).context("this run is not active")?;
            l.stopping = true;
            l.commands.clone()
        };
        commands.send(Command::Stop).await?;
        Ok(())
    }

    pub async fn answer_approval(&self, run: &str, id: &str, allow: bool) -> Result<()> {
        let commands = lock(&self.live)
            .get(run)
            .map(|l| l.commands.clone())
            .context("this run is not active")?;
        lock(&self.approvals).retain(|a| !(a.run == run && a.id == id));
        commands
            .send(Command::Approve {
                id: id.to_string(),
                allow,
            })
            .await?;
        self.notify(UiEvent::Runs);
        Ok(())
    }

    pub async fn shutdown(&self) {
        let all: Vec<mpsc::Sender<Command>> = {
            let mut live = lock(&self.live);
            live.values_mut()
                .map(|l| {
                    l.stopping = true;
                    l.commands.clone()
                })
                .collect()
        };
        // Record the stop now: the process may exit before the sessions
        // report back, and a run left looking active would be resumed on the
        // next start even though the user stopped it on purpose.
        for id in self.live_ids() {
            self.update_run(&id, |r| r.activity = Activity::Finished);
        }
        for commands in all {
            let _ = commands.send(Command::Stop).await;
        }
        self.previews.stop_all(self).await;
    }

    /// Repairs state left by a daemon that stopped without cleaning up.
    fn recover(&self) -> Result<()> {
        for mut run in self.store.runs()? {
            if !is_over(run.activity) {
                // Its process died with the old daemon (the CLIs exit when
                // their stdin closes).
                run.activity = Activity::Interrupted;
                run.updated_at = Utc::now();
                self.store.put_run(&run)?;
            }
        }
        // A fix that was handed over but never acknowledged goes back in the
        // queue. Agents fetch fixes by id, so a repeat is harmless.
        for annotation in self.store.annotations()? {
            if annotation.state == AnnotationState::Sent {
                self.store
                    .update_annotation(&annotation.id, |a| a.state = AnnotationState::Queued)?;
            }
        }
        Ok(())
    }

    /// Resumes the writer that was working when the daemon last stopped.
    pub async fn resume_interrupted(self: &Arc<Self>) -> Result<Option<Run>> {
        let Some(previous) = self
            .store
            .runs()?
            .into_iter()
            .filter(|r| r.role == RunRole::Writer)
            .next_back()
        else {
            return Ok(None);
        };
        if previous.activity != Activity::Interrupted || previous.session.is_none() {
            return Ok(None);
        }
        self.update_run(&previous.id, |r| r.activity = Activity::Finished);
        let run = self
            .spawn(previous.agent, RunRole::Writer, previous.session, prompts::resumed())
            .await?;
        Ok(Some(run))
    }

    // -- annotations -------------------------------------------------------

    fn infer_requirements(&self, anchor: &Anchor) -> Vec<String> {
        let Ok(manifest) = self.project.load() else {
            return vec![];
        };
        let node_requirements = |id: &str| {
            manifest
                .architecture
                .nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.requirements.clone())
                .unwrap_or_default()
        };
        match anchor {
            Anchor::Requirement { requirement } => vec![requirement.clone()],
            Anchor::ArchNode { node }
            | Anchor::ApiOperation { node, .. }
            | Anchor::DbTable { node, .. } => node_requirements(node),
            Anchor::ArchEdge { from, to } => manifest
                .architecture
                .edges
                .iter()
                .find(|e| &e.from == from && &e.to == to)
                .map(|e| e.requirements.clone())
                .unwrap_or_default(),
            Anchor::Scenario { scenario } => {
                let mut ids: Vec<String> = manifest
                    .scenarios
                    .scenarios
                    .iter()
                    .filter(|s| &s.id == scenario)
                    .flat_map(|s| s.covers.iter().map(|c| c.requirement.clone()))
                    .collect();
                ids.dedup();
                ids
            }
            Anchor::UiElement { .. } | Anchor::UiRegion { .. } => vec![],
        }
    }

    pub async fn create_annotation(self: &Arc<Self>, input: NewAnnotation) -> Result<Annotation> {
        if input.body.trim().is_empty() {
            bail!("an annotation needs a message");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let image = match input.image_base64 {
            Some(encoded) => {
                let bytes = crate::base64_decode(&encoded).context("invalid image")?;
                let relative = format!("annotations/{id}.png");
                let path = self.project.local_dir().join(&relative);
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, bytes)?;
                Some(relative)
            }
            None => None,
        };
        let requirements = if input.requirements.is_empty() {
            self.infer_requirements(&input.anchor)
        } else {
            input.requirements
        };
        let annotation = Annotation {
            id,
            intent: input.intent,
            state: AnnotationState::Queued,
            anchor: input.anchor,
            body: input.body,
            snapshot: self.current_snapshot()?,
            requirements,
            image,
            created_at: Utc::now(),
            comments: vec![],
        };
        self.store.put_annotation(&annotation)?;
        self.notify(UiEvent::Annotations);
        self.notify(UiEvent::Status);
        match annotation.intent {
            AnnotationIntent::Fix => {
                // Only interrupt nothing: hand it over now if the writer is
                // idle, otherwise at its next turn boundary.
                if self.idle_writer().is_some() || self.live_writer().is_none() {
                    self.deliver_fixes().await;
                }
            }
            AnnotationIntent::Question => self.ask(&annotation).await?,
        }
        Ok(annotation)
    }

    async fn ask(self: &Arc<Self>, annotation: &Annotation) -> Result<()> {
        let agent = self
            .last_writer_run()
            .map(|r| r.agent)
            .unwrap_or(self.config().default_agent);
        self.spawn(agent, RunRole::Responder, None, prompts::question(annotation))
            .await?;
        self.store
            .update_annotation(&annotation.id, |a| a.state = AnnotationState::Sent)?;
        self.notify(UiEvent::Annotations);
        Ok(())
    }

    /// Tells the writer about queued fix requests. Returns true when a
    /// message was sent. The agent pulls the annotations by id, so telling
    /// it twice does not duplicate work.
    pub async fn deliver_fixes(self: &Arc<Self>) -> bool {
        let queued: Vec<Annotation> = self
            .store
            .annotations()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.intent == AnnotationIntent::Fix && a.state == AnnotationState::Queued)
            .collect();
        if queued.is_empty() {
            return false;
        }
        let message = prompts::annotations_waiting(queued.len());
        let sent = match self.live_writer() {
            Some((_, commands)) => commands.send(Command::Send(message)).await.is_ok(),
            // No session is running: wake the last writer so the queue is
            // not stuck until the user notices.
            None => match self.last_writer_run() {
                Some(previous) => self
                    .spawn(previous.agent, RunRole::Writer, previous.session, message)
                    .await
                    .is_ok(),
                None => false,
            },
        };
        if sent {
            for annotation in queued {
                let _ = self
                    .store
                    .update_annotation(&annotation.id, |a| a.state = AnnotationState::Sent);
            }
            self.notify(UiEvent::Annotations);
        }
        sent
    }

    pub fn comment(&self, id: &str, author: Author, body: String) -> Result<Annotation> {
        let comment = Comment {
            id: uuid::Uuid::new_v4().to_string(),
            author,
            body,
            at: Utc::now(),
        };
        let updated = self
            .store
            .update_annotation(id, |a| a.comments.push(comment))?
            .ok_or_else(|| anyhow!("unknown annotation {id}"))?;
        self.notify(UiEvent::Annotations);
        Ok(updated)
    }

    pub fn set_annotation_state(&self, id: &str, state: AnnotationState) -> Result<Annotation> {
        let updated = self
            .store
            .update_annotation(id, |a| a.state = state)?
            .ok_or_else(|| anyhow!("unknown annotation {id}"))?;
        self.notify(UiEvent::Annotations);
        self.notify(UiEvent::Status);
        Ok(updated)
    }

    /// The user is not satisfied: the fix goes back in the queue.
    pub async fn reopen(self: &Arc<Self>, id: &str, body: Option<String>) -> Result<Annotation> {
        if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
            self.comment(id, Author::User, body)?;
        }
        let annotation = self.set_annotation_state(id, AnnotationState::Queued)?;
        match annotation.intent {
            AnnotationIntent::Fix => {
                if self.idle_writer().is_some() || self.live_writer().is_none() {
                    self.deliver_fixes().await;
                }
            }
            AnnotationIntent::Question => self.ask(&annotation).await?,
        }
        Ok(annotation)
    }
}
