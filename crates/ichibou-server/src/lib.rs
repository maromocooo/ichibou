//! The Ichibou daemon: one process per project, bound to loopback.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use notify::{RecursiveMode, Watcher};
use serde_json::Value;

pub mod api;
pub mod engine;
pub mod inspect;
pub mod mcp;
pub mod preview;
pub mod prompts;

pub use engine::{Engine, UiEvent};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../ui/dist"]
struct Ui;

pub(crate) fn base64_decode(text: &str) -> Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::STANDARD.decode(text.trim())?)
}

/// Holds an exclusive lock so two daemons never manage the same project.
pub struct InstanceLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl InstanceLock {
    pub fn acquire(local_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(local_dir)?;
        let path = local_dir.join("daemon.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        if file.try_lock().is_err() {
            bail!(
                "another Ichibou daemon is already running for this project ({})",
                path.display()
            );
        }
        Ok(Self { _file: file, path })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub struct Server {
    pub engine: Arc<Engine>,
    pub addr: SocketAddr,
    _lock: InstanceLock,
    _watcher: notify::RecommendedWatcher,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    /// URL to open in a browser, carrying the access token.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/?token={}", self.addr.port(), self.engine.token)
    }

    pub async fn shutdown(self) {
        self.engine.shutdown().await;
        self.task.abort();
    }
}

/// Starts the daemon for the project at `root`. `port` 0 picks a free port.
pub async fn serve(root: impl Into<PathBuf>, port: u16) -> Result<Server> {
    let root: PathBuf = root.into();
    let root = root
        .canonicalize()
        .with_context(|| format!("{} does not exist", root.display()))?;
    let lock = InstanceLock::acquire(&ichibou_core::manifest::Project::new(&root).local_dir())?;
    let engine = Engine::open(&root)?;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    engine.set_port(addr.port());

    let app = Router::new()
        .nest("/api", api::router(engine.clone()))
        .route("/mcp/{token}", post(mcp_endpoint).get(mcp_get))
        .fallback(get(ui))
        .with_state(engine.clone());
    let task = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!("server stopped: {err}");
        }
    });

    let watcher = watch(engine.clone())?;
    engine.verify_if_stale();
    Ok(Server {
        engine,
        addr,
        _lock: lock,
        _watcher: watcher,
        task,
    })
}

/// Watches the workspace. A change moves the snapshot, which demotes every
/// earlier pass; when no agent is mid-turn, verification runs again.
fn watch(engine: Arc<Engine>) -> Result<notify::RecommendedWatcher> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let root = engine.project.root.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        let relevant = event.paths.iter().any(|path| {
            let relative = path.strip_prefix(&root).unwrap_or(path);
            let mut parts = relative.components().map(|c| c.as_os_str().to_string_lossy());
            match parts.next().as_deref() {
                Some(".git") => false,
                Some(".ichibou") => parts.next().as_deref() != Some("local"),
                _ => true,
            }
        });
        if relevant {
            let _ = tx.send(());
        }
    })?;
    watcher.watch(&engine.project.root, RecursiveMode::Recursive)?;

    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Let a burst of writes settle before hashing the tree.
            tokio::time::sleep(Duration::from_millis(400)).await;
            while rx.try_recv().is_ok() {}
            let engine = engine.clone();
            let _ = tokio::task::spawn_blocking(move || engine.workspace_changed()).await;
        }
    });
    Ok(watcher)
}

async fn mcp_endpoint(
    State(engine): State<Arc<Engine>>,
    UrlPath(token): UrlPath<String>,
    Json(message): Json<Value>,
) -> Response {
    let Some(run) = engine.run_for_mcp_token(&token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match mcp::handle(&engine, &run, message).await {
        Some(reply) => Json(reply).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Ichibou's MCP server never pushes messages, so it offers no SSE stream.
async fn mcp_get() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

async fn ui(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (file, name) = match Ui::get(path) {
        Some(file) if !path.is_empty() => (Some(file), path),
        // Client-side routes all load the app shell.
        _ => (Ui::get("index.html"), "index.html"),
    };
    match file {
        Some(file) => {
            let mime = mime_guess::from_path(name).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                ],
                Body::from(file.data.into_owned()),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
