//! Drivers for the coding agents Ichibou supervises.
//!
//! Each driver spawns the agent's official CLI as a child process, using
//! whatever login the user already has. Ichibou never reads, stores or
//! forwards credentials.
//!
//! All drivers speak one small protocol to the rest of the daemon: commands
//! in ([`Command`]), normalized events out ([`AgentEvent`]). Anything a driver
//! does not recognize is passed through as [`AgentEvent::Unknown`] rather than
//! treated as an error, because the CLIs add event types between releases.

use std::path::PathBuf;

use anyhow::Result;
use ichibou_core::{Activity, AgentKind};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub mod claude;
pub mod codex;
pub mod fake;

#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub agent: AgentKind,
    pub cwd: PathBuf,
    /// Agent session or thread id to continue.
    pub resume: Option<String>,
    /// The session must not modify the workspace.
    pub read_only: bool,
    /// Instructions appended to the agent's own system prompt.
    pub instructions: String,
    /// Streamable-HTTP MCP endpoint exposed by the daemon for this run.
    pub mcp_url: Option<String>,
    /// Model to run, as the agent's CLI names it. `None` uses the CLI's own
    /// default.
    pub model: Option<String>,
    /// Reasoning effort, as the agent's CLI names it. `None` uses its default.
    pub effort: Option<String>,
    /// Claude permission mode for sessions that may write. Chosen by the user.
    pub claude_permission_mode: String,
    /// Script for the fake driver.
    pub fake_script: Option<PathBuf>,
    /// Overrides the binary to launch.
    pub program: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Deliver a user message. Starts a turn, or steers the running one.
    Send(String),
    Interrupt,
    Approve { id: String, allow: bool },
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Session {
        id: String,
        #[serde(default)]
        model: Option<String>,
        /// How the CLI is authenticated, when it says. Used to warn when an
        /// API key would bill usage instead of the subscription.
        #[serde(default)]
        auth: Option<String>,
    },
    Activity {
        activity: Activity,
    },
    Message {
        text: String,
    },
    ToolStarted {
        id: String,
        name: String,
        #[serde(default)]
        summary: String,
    },
    ToolFinished {
        id: String,
        ok: bool,
    },
    FileChanged {
        paths: Vec<String>,
    },
    /// The agent wants permission for something; answer with
    /// [`Command::Approve`].
    ApprovalRequest {
        id: String,
        kind: String,
        #[serde(default)]
        detail: String,
    },
    RateLimit {
        limited: bool,
        #[serde(default)]
        resets_at: Option<i64>,
        #[serde(default)]
        detail: String,
    },
    TurnCompleted {
        ok: bool,
        #[serde(default)]
        summary: String,
    },
    Error {
        message: String,
    },
    Unknown {
        kind: String,
    },
    Exited {
        #[serde(default)]
        code: Option<i32>,
    },
}

pub struct Session {
    pub commands: mpsc::Sender<Command>,
    pub events: mpsc::Receiver<AgentEvent>,
    pub pid: Option<u32>,
}

/// Starts an agent session. The first user message is sent with
/// [`Command::Send`].
pub async fn start(spec: SessionSpec) -> Result<Session> {
    match spec.agent {
        AgentKind::Claude => claude::start(spec).await,
        AgentKind::Codex => codex::start(spec).await,
        AgentKind::Fake => fake::start(spec).await,
    }
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Whether a model or effort name is safe to pass on a command line and
/// inside a quoted configuration value: letters, digits and a few
/// punctuation marks, nothing that could end a string or start an option.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '[' | ']'))
}

/// A model an agent offers, with the reasoning efforts it accepts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

/// What the UI offers for one agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentOptions {
    pub models: Vec<ModelInfo>,
    /// The CLI accepts model names beyond the listed ones.
    pub free_text: bool,
    /// Why the list could not be read, when it could not.
    #[serde(default)]
    pub error: Option<String>,
}

/// Claude Code has no command that lists models. It accepts aliases for the
/// latest model of each family, or a model's full name, and one fixed set of
/// effort levels (`claude --help`).
pub fn claude_options() -> AgentOptions {
    let efforts: Vec<String> = ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .map(|e| e.to_string())
        .collect();
    AgentOptions {
        models: ["fable", "opus", "sonnet", "haiku"]
            .iter()
            .map(|id| ModelInfo {
                id: id.to_string(),
                name: id.to_string(),
                efforts: efforts.clone(),
                default_effort: None,
                is_default: false,
            })
            .collect(),
        free_text: true,
        error: None,
    }
}
