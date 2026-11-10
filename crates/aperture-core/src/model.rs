use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Manifest documents (live in the target project's `.aperture/`, git-tracked)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RequirementsDoc {
    #[serde(default)]
    pub requirements: Vec<Requirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Requirement {
    /// Stable id such as `REQ-LOGIN-01`.
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    /// Things only a human can judge (visual quality, feel).
    #[serde(default)]
    pub manual_checks: Vec<ManualCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Criterion {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManualCheck {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ArchitectureDoc {
    #[serde(default)]
    pub nodes: Vec<ArchNode>,
    #[serde(default)]
    pub edges: Vec<ArchEdge>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Client,
    Api,
    Service,
    Db,
    Queue,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArchNode {
    pub id: String,
    pub kind: NodeKind,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Source paths (files or directories) that implement this node.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArchEdge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub requirements: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TargetsDoc {
    #[serde(default)]
    pub targets: Vec<Target>,
    #[serde(default)]
    pub apis: Vec<ApiSource>,
    #[serde(default)]
    pub databases: Vec<DatabaseSource>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Web,
    Ios,
    Android,
    Desktop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Target {
    pub name: String,
    pub kind: TargetKind,
    /// How the live preview is started in the working tree.
    #[serde(default)]
    pub dev: Option<Launch>,
    /// How the app is built and started inside a materialized snapshot for
    /// verification. Falls back to `dev` when absent.
    #[serde(default)]
    pub verify: Option<VerifyEnv>,
    /// iOS bundle id / Android package / macOS app name, depending on kind.
    #[serde(default)]
    pub app_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Launch {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    /// URL the app answers on once ready. `{port}` is substituted.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VerifyEnv {
    /// Commands run once in the snapshot copy before start (install, build,
    /// database reset).
    #[serde(default)]
    pub setup: Vec<String>,
    #[serde(default)]
    pub start: Option<Launch>,
    /// Ignored files (for example `.env.test`) copied into the snapshot copy.
    #[serde(default)]
    pub copy: Vec<String>,
    /// Ignored directories (dependency caches such as `node_modules`)
    /// symlinked into the snapshot copy instead of being rebuilt every run.
    #[serde(default)]
    pub link: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiSource {
    /// Architecture node this spec belongs to.
    pub node: String,
    #[serde(default)]
    pub openapi_file: Option<String>,
    #[serde(default)]
    pub openapi_url: Option<String>,
    #[serde(default)]
    pub openapi_command: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseKind {
    Postgres,
    Sqlite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatabaseSource {
    pub node: String,
    pub kind: DatabaseKind,
    /// Connection URL, or a file path for SQLite.
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScenariosDoc {
    #[serde(default)]
    pub scenarios: Vec<Scenario>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    Playwright,
    Maestro,
    /// Any command that writes a JUnit XML report.
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    pub target: String,
    pub runner: RunnerKind,
    pub covers: Vec<Coverage>,
    /// Test files, relative to the project root. Their content is part of the
    /// approved definition.
    #[serde(default)]
    pub files: Vec<String>,
    /// For `command` runners: the command line. `{report}` is replaced with
    /// the JUnit XML path the runner must write.
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Coverage {
    pub requirement: String,
    pub criterion: String,
}

// ---------------------------------------------------------------------------
// Local state (SQLite, never committed)
// ---------------------------------------------------------------------------

/// Content hash of the working tree. Evidence is only valid for the snapshot
/// it was produced from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SnapshotId(pub String);

/// Result of one verification attempt. Only `Passed` ever counts as success.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    /// The runner itself broke (build failed, app did not start).
    Error,
    Aborted,
    Skipped,
    NotApplicable,
}

/// A verdict produced by the daemon's verification runner. Agents cannot
/// create these.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationResult {
    pub id: String,
    pub scenario: String,
    pub outcome: Outcome,
    pub snapshot: SnapshotId,
    /// Hash of the scenario definition that was executed.
    pub definition: String,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub detail: String,
    /// Directory holding logs, screenshots and traces for this run.
    #[serde(default)]
    pub artifacts: Option<String>,
}

/// The scenario definition hash the user approved. Runs use this definition;
/// edits by an agent stay pending until the user approves them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScenarioApproval {
    pub scenario: String,
    pub definition: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManualConfirmation {
    pub requirement: String,
    pub check: String,
    pub snapshot: SnapshotId,
    pub at: DateTime<Utc>,
}

/// What an agent says it is working on. Drives the "working on" display only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Claim {
    pub run: String,
    pub requirement: String,
    #[serde(default)]
    pub note: String,
    /// Set when the agent reports the implementation as finished.
    pub reported_done: bool,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationIntent {
    Fix,
    Question,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationState {
    /// Stored, not yet delivered to an agent.
    Queued,
    /// Delivered; waiting for the agent to acknowledge.
    Sent,
    /// The agent acknowledged it.
    Acknowledged,
    /// The agent reports the fix or answer as done; a fix still needs
    /// verification.
    Addressed,
    Resolved,
    /// The annotated thing no longer exists; the user must re-point it.
    AnchorLost,
}

/// What an annotation is attached to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    UiElement {
        target: String,
        url: String,
        selector: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        source: Option<String>,
        rect: Rect,
    },
    UiRegion {
        target: String,
        #[serde(default)]
        url: Option<String>,
        rect: Rect,
    },
    ArchNode {
        node: String,
    },
    ArchEdge {
        from: String,
        to: String,
    },
    ApiOperation {
        node: String,
        operation: String,
    },
    DbTable {
        node: String,
        table: String,
        #[serde(default)]
        column: Option<String>,
    },
    Requirement {
        requirement: String,
    },
    Scenario {
        scenario: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Annotation {
    pub id: String,
    pub intent: AnnotationIntent,
    pub state: AnnotationState,
    pub anchor: Anchor,
    pub body: String,
    /// Snapshot the user was looking at.
    pub snapshot: SnapshotId,
    /// Requirements this annotation blocks, when known.
    #[serde(default)]
    pub requirements: Vec<String>,
    /// Screenshot file, relative to the local state directory.
    #[serde(default)]
    pub image: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    pub id: String,
    pub author: Author,
    pub body: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    /// Replays a recorded event script. Used in tests and demos.
    Fake,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Idle,
    Working,
    RunningTool,
    WaitingForInput,
    RateLimited,
    Interrupted,
    Finished,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunRole {
    /// The single session allowed to write to the workspace.
    Writer,
    /// Read-only session answering question annotations.
    Responder,
    /// Session drafting scenarios from requirements.
    ScenarioAuthor,
    /// Read-only session drafting manifests for an existing repository.
    Onboarding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Run {
    pub id: String,
    pub agent: AgentKind,
    pub role: RunRole,
    pub activity: Activity,
    /// The agent's own session or thread id, used to resume.
    #[serde(default)]
    pub session: Option<String>,
    /// Ownership generation of the workspace when this run started. Results
    /// from a run with an older generation are discarded.
    pub generation: u64,
    #[serde(default)]
    pub pid: Option<u32>,
    /// How the agent CLI says it is authenticated (Claude: `none` means the
    /// subscription login; anything else is an API key).
    #[serde(default)]
    pub auth: Option<String>,
    #[serde(default)]
    pub last_message: String,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
