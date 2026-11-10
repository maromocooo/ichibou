//! HTTP API for the browser UI.

use std::sync::Arc;

use ichibou_core::*;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::engine::{Config, Engine, NewAnnotation, RunKind};

pub struct ApiError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("{:#}", self.0) })),
        )
            .into_response()
    }
}

type ApiResult<T = Json<Value>> = Result<T, ApiError>;

fn ok() -> ApiResult {
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: Option<String>,
}

/// The daemon can run commands on this machine, so every API call must come
/// from the UI it served: right token, loopback Host, and (when the browser
/// sends one) an Origin that is this server.
async fn guard(
    State(engine): State<Arc<Engine>>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let port = engine.port();
    let allowed_hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let host_ok = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| allowed_hosts.iter().any(|a| a == h));
    let origin_ok = match headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
        None => true,
        Some(origin) => allowed_hosts
            .iter()
            .any(|a| origin == format!("http://{a}")),
    };
    let presented = headers
        .get("x-ichibou-token")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .or(query.token);
    if !host_ok || !origin_ok || presented.as_deref() != Some(engine.token.as_str()) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    next.run(request).await
}

pub fn router(engine: Arc<Engine>) -> Router<Arc<Engine>> {
    Router::new()
        .route("/overview", get(overview))
        .route("/events", get(events))
        .route("/ws", get(ws))
        .route("/config", post(set_config))
        .route("/agents", get(agents))
        .route("/runs", post(start_run))
        .route("/runs/resume", post(resume))
        .route("/runs/{id}/message", post(message))
        .route("/runs/{id}/interrupt", post(interrupt))
        .route("/runs/{id}/stop", post(stop))
        .route("/runs/{id}/approvals/{approval}", post(approval))
        .route("/annotations", post(create_annotation))
        .route("/annotations/{id}/comment", post(comment))
        .route("/annotations/{id}/resolve", post(resolve))
        .route("/annotations/{id}/reopen", post(reopen))
        .route("/scenarios/{id}/approve", post(approve_scenario))
        .route("/scenarios/approve-all", post(approve_all))
        .route("/scenarios/{id}/files", get(scenario_files))
        .route("/verify", post(verify))
        .route("/manual", post(manual))
        .route("/proposals/{kind}", post(decide_proposal))
        .route("/targets/approve", post(approve_targets))
        .route("/requirements/approve", post(approve_requirements))
        .route("/manifest/{kind}", post(save_manifest))
        .route("/previews", get(previews))
        .route("/previews/{target}/start", post(preview_start))
        .route("/previews/{target}/stop", post(preview_stop))
        .route("/previews/{target}/frame", get(preview_frame))
        .route("/previews/{target}/stream", get(preview_stream))
        .route("/previews/{target}/video", get(preview_video))
        .route("/previews/{target}/tap", post(preview_tap))
        .route("/previews/{target}/text", post(preview_text))
        .route("/previews/{target}/hierarchy", get(preview_hierarchy))
        .route("/inspect/openapi/{node}", get(inspect_openapi))
        .route("/inspect/request/{node}", post(inspect_request))
        .route("/inspect/db/{node}", get(inspect_db))
        .route("/assets", get(assets))
        .route("/asset", get(asset))
        .route("/files/{*path}", get(file))
        .layer(axum::middleware::from_fn_with_state(engine, guard))
}

async fn overview(State(engine): State<Arc<Engine>>) -> ApiResult {
    let overview = tokio::task::spawn_blocking(move || engine.overview()).await??;
    Ok(Json(serde_json::to_value(overview)?))
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: Option<i64>,
}

async fn events(State(engine): State<Arc<Engine>>, Query(query): Query<EventsQuery>) -> ApiResult {
    let events = match query.after {
        Some(after) => engine.store.events_after(after, 500)?,
        None => engine.store.recent_events(200)?,
    };
    Ok(Json(serde_json::to_value(events)?))
}

async fn ws(State(engine): State<Arc<Engine>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| push(engine, socket))
}

async fn push(engine: Arc<Engine>, mut socket: WebSocket) {
    let mut events = engine.subscribe();
    loop {
        tokio::select! {
            event = events.recv() => {
                let text = match event {
                    Ok(event) => serde_json::to_string(&event).unwrap_or_default(),
                    // Fell behind: tell the UI to reload everything.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        r#"{"type":"resync"}"#.to_string()
                    }
                    Err(_) => break,
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                if !matches!(incoming, Some(Ok(_))) {
                    break;
                }
            }
        }
    }
}

async fn set_config(State(engine): State<Arc<Engine>>, Json(config): Json<Config>) -> ApiResult {
    engine.set_config(&config)?;
    ok()
}

#[derive(Deserialize)]
struct StartRun {
    kind: RunKind,
    #[serde(default)]
    agent: Option<AgentKind>,
    #[serde(default)]
    text: Option<String>,
}

async fn start_run(State(engine): State<Arc<Engine>>, Json(body): Json<StartRun>) -> ApiResult {
    let run = engine.start(body.kind, body.agent, body.text).await?;
    Ok(Json(serde_json::to_value(run)?))
}

async fn resume(State(engine): State<Arc<Engine>>) -> ApiResult {
    let run = engine.resume_interrupted().await?;
    Ok(Json(serde_json::to_value(run)?))
}

#[derive(Deserialize)]
struct Text {
    #[serde(default)]
    text: Option<String>,
}

async fn message(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(body): Json<Text>,
) -> ApiResult {
    engine.send_message(&id, body.text.unwrap_or_default()).await?;
    ok()
}

async fn interrupt(State(engine): State<Arc<Engine>>, Path(id): Path<String>) -> ApiResult {
    engine.interrupt(&id).await?;
    ok()
}

async fn stop(State(engine): State<Arc<Engine>>, Path(id): Path<String>) -> ApiResult {
    engine.stop_run(&id).await?;
    ok()
}

#[derive(Deserialize)]
struct Allow {
    allow: bool,
}

async fn approval(
    State(engine): State<Arc<Engine>>,
    Path((id, approval)): Path<(String, String)>,
    Json(body): Json<Allow>,
) -> ApiResult {
    engine.answer_approval(&id, &approval, body.allow).await?;
    ok()
}

async fn create_annotation(
    State(engine): State<Arc<Engine>>,
    Json(body): Json<NewAnnotation>,
) -> ApiResult {
    let annotation = engine.create_annotation(body).await?;
    Ok(Json(serde_json::to_value(annotation)?))
}

async fn comment(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(body): Json<Text>,
) -> ApiResult {
    let text = body.text.unwrap_or_default();
    if text.trim().is_empty() {
        return Err(anyhow::anyhow!("a comment needs text").into());
    }
    // A follow-up from the user reopens the thread for the agent.
    let annotation = engine.reopen(&id, Some(text)).await?;
    Ok(Json(serde_json::to_value(annotation)?))
}

async fn resolve(State(engine): State<Arc<Engine>>, Path(id): Path<String>) -> ApiResult {
    let annotation = engine.set_annotation_state(&id, AnnotationState::Resolved)?;
    Ok(Json(serde_json::to_value(annotation)?))
}

async fn reopen(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(body): Json<Text>,
) -> ApiResult {
    let annotation = engine.reopen(&id, body.text).await?;
    Ok(Json(serde_json::to_value(annotation)?))
}

async fn approve_scenario(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(seen): Json<Seen>,
) -> ApiResult {
    engine.approve_scenario(&id, &seen.hash)?;
    // A newly approved definition has no verdict yet.
    engine.verify_if_stale();
    ok()
}

#[derive(Deserialize)]
struct SeenScenarios {
    /// Scenario id -> definition hash the UI displayed.
    hashes: std::collections::HashMap<String, String>,
}

/// Approves exactly the scenarios the UI listed, each at the version shown.
async fn approve_all(
    State(engine): State<Arc<Engine>>,
    Json(seen): Json<SeenScenarios>,
) -> ApiResult {
    for (id, hash) in &seen.hashes {
        engine.approve_scenario(id, hash)?;
    }
    engine.verify_if_stale();
    ok()
}

async fn verify(State(engine): State<Arc<Engine>>) -> ApiResult {
    let _ = engine.refresh_snapshot();
    engine.request_verify();
    ok()
}

#[derive(Deserialize)]
struct Manual {
    requirement: String,
    check: String,
    confirmed: bool,
}

async fn manual(State(engine): State<Arc<Engine>>, Json(body): Json<Manual>) -> ApiResult {
    engine.confirm_manual(&body.requirement, &body.check, body.confirmed)?;
    ok()
}

#[derive(Deserialize)]
struct Decision {
    accept: bool,
    /// Hash of the proposal the UI displayed (from the overview).
    #[serde(default)]
    hash: String,
}

async fn decide_proposal(
    State(engine): State<Arc<Engine>>,
    Path(kind): Path<String>,
    Json(body): Json<Decision>,
) -> ApiResult {
    engine.decide_proposal(&kind, body.accept, &body.hash)?;
    ok()
}

#[derive(Deserialize)]
struct Seen {
    /// Hash of the content the UI displayed (from the overview).
    hash: String,
}

async fn approve_targets(State(engine): State<Arc<Engine>>, Json(seen): Json<Seen>) -> ApiResult {
    engine.approve_targets(&seen.hash)?;
    engine.verify_if_stale();
    ok()
}

/// The user editing the manifest directly in the UI.
async fn save_manifest(
    State(engine): State<Arc<Engine>>,
    Path(kind): Path<String>,
    Json(doc): Json<Value>,
) -> ApiResult {
    match kind.as_str() {
        "requirements" => {
            let written = engine.project.save_requirements(&serde_json::from_value(doc)?)?;
            engine.approve_written("requirements_approved", &written)?;
        }
        "architecture" => engine.project.save_architecture(&serde_json::from_value(doc)?)?,
        "targets" => {
            let written = engine.project.save_targets(&serde_json::from_value(doc)?)?;
            engine.approve_written("targets_approved", &written)?;
        }
        "scenarios" => engine.project.save_scenarios(&serde_json::from_value(doc)?)?,
        other => return Err(anyhow::anyhow!("unknown manifest part {other}").into()),
    }
    engine.workspace_changed();
    engine.notify(crate::engine::UiEvent::Status);
    ok()
}

/// Serves logs, reports and screenshots from the local state directory.
async fn file(State(engine): State<Arc<Engine>>, Path(path): Path<String>) -> Response {
    // Only what is under `artifacts/` or `annotations/` is served, judged
    // on the resolved path: `artifacts/../state.db` names neither.
    let base = engine.project.local_dir();
    let full = match manifest::resolve_inside(&base, &path) {
        Some(full) => full,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    // The two directories must be real directories of the state folder. If
    // one were a link (say `artifacts -> .`), "inside it" would mean inside
    // wherever it points.
    let Ok(base) = base.canonicalize() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let inside = ["artifacts", "annotations"].iter().any(|dir| {
        let allowed = base.join(dir);
        std::fs::symlink_metadata(&allowed).is_ok_and(|meta| meta.is_dir())
            && full.starts_with(&allowed)
    });
    if !inside {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            // Logs and reports contain whatever the app under test printed.
            // Only images keep their type; everything else is plain text, and
            // nothing may run script in the UI's origin.
            let guessed = mime_guess::from_path(&full).first_or_text_plain();
            let mime = if guessed.type_() == "image" && guessed.subtype() != "svg" {
                guessed.to_string()
            } else {
                "text/plain; charset=utf-8".to_string()
            };
            (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CONTENT_SECURITY_POLICY, "sandbox".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ],
                Body::from(bytes),
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn previews(State(engine): State<Arc<Engine>>) -> ApiResult {
    Ok(Json(serde_json::to_value(engine.previews.list(&engine).await)?))
}

async fn preview_start(State(engine): State<Arc<Engine>>, Path(target): Path<String>) -> ApiResult {
    engine.previews.start(&engine, &target).await?;
    ok()
}

async fn preview_stop(State(engine): State<Arc<Engine>>, Path(target): Path<String>) -> ApiResult {
    engine.previews.stop(&engine, &target).await;
    ok()
}

async fn preview_frame(
    State(engine): State<Arc<Engine>>,
    Path(target): Path<String>,
) -> Result<Response, ApiError> {
    let (mime, bytes) = engine.previews.frame(&engine, &target).await?;
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from(bytes),
    )
        .into_response())
}

#[derive(Deserialize)]
struct Point {
    x: f64,
    y: f64,
}

async fn preview_tap(
    State(engine): State<Arc<Engine>>,
    Path(target): Path<String>,
    Json(point): Json<Point>,
) -> ApiResult {
    if !(0.0..=1.0).contains(&point.x) || !(0.0..=1.0).contains(&point.y) {
        return Err(anyhow::anyhow!("tap position must be within the screen").into());
    }
    engine.previews.tap(&engine, &target, point.x, point.y).await?;
    ok()
}

async fn preview_text(
    State(engine): State<Arc<Engine>>,
    Path(target): Path<String>,
    Json(body): Json<Text>,
) -> ApiResult {
    engine
        .previews
        .text(&engine, &target, &body.text.unwrap_or_default())
        .await?;
    ok()
}

async fn preview_hierarchy(
    State(engine): State<Arc<Engine>>,
    Path(target): Path<String>,
) -> ApiResult {
    Ok(Json(engine.previews.hierarchy(&engine, &target).await?))
}

async fn inspect_openapi(State(engine): State<Arc<Engine>>, Path(node): Path<String>) -> ApiResult {
    Ok(Json(crate::inspect::openapi(&engine, &node).await?))
}

async fn inspect_request(
    State(engine): State<Arc<Engine>>,
    Path(node): Path<String>,
    Json(request): Json<crate::inspect::TryRequest>,
) -> ApiResult {
    Ok(Json(crate::inspect::try_request(&engine, &node, request).await?))
}

async fn inspect_db(State(engine): State<Arc<Engine>>, Path(node): Path<String>) -> ApiResult {
    Ok(Json(crate::inspect::database(&engine, &node).await?))
}

const ASSET_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "webp", "avif", "ico", "woff", "woff2", "ttf", "otf",
];

fn is_asset(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ASSET_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Image and font files in the project (ignored files excluded).
async fn assets(State(engine): State<Arc<Engine>>) -> ApiResult {
    let root = engine.project.root.clone();
    let list = tokio::task::spawn_blocking(move || {
        let mut list = Vec::new();
        for entry in ignore::WalkBuilder::new(&root).require_git(false).build().flatten() {
            let path = entry.path();
            if !path.is_file() || !is_asset(path) {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&root) else { continue };
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            list.push(json!({ "path": relative.to_string_lossy(), "size": size }));
            if list.len() >= 1000 {
                break;
            }
        }
        list
    })
    .await?;
    Ok(Json(Value::Array(list)))
}

#[derive(Deserialize)]
struct AssetQuery {
    path: String,
}

async fn asset(State(engine): State<Arc<Engine>>, Query(query): Query<AssetQuery>) -> Response {
    let root = engine.project.root.clone();
    let full = root.join(&query.path);
    let inside = match (full.canonicalize(), root.canonicalize()) {
        (Ok(full), Ok(root)) => full.starts_with(root),
        _ => false,
    };
    if !inside || !is_asset(&full) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&full).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    // An SVG is a document; never let one run script in the UI's origin.
                    (header::CONTENT_SECURITY_POLICY, "sandbox".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ],
                Body::from(bytes),
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Relays the live video stream of a native target (multipart MJPEG).
async fn preview_stream(State(engine): State<Arc<Engine>>, Path(target): Path<String>) -> Response {
    let Some(url) = engine.previews.stream_url(&target).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match reqwest::get(&url).await {
        Ok(upstream) => {
            let kind = upstream
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("multipart/x-mixed-replace; boundary=frame")
                .to_string();
            (
                [
                    (header::CONTENT_TYPE, kind),
                    (header::CACHE_CONTROL, "no-store".to_string()),
                ],
                Body::from_stream(upstream.bytes_stream()),
            )
                .into_response()
        }
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

async fn approve_requirements(
    State(engine): State<Arc<Engine>>,
    Json(seen): Json<Seen>,
) -> ApiResult {
    engine.approve_requirements(&seen.hash)?;
    ok()
}

async fn scenario_files(State(engine): State<Arc<Engine>>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(engine.scenario_files(&id)?))
}

/// Live H.264 video of an Android target, over a WebSocket.
async fn preview_video(
    State(engine): State<Arc<Engine>>,
    Path(target): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !engine.previews.is_android(&target).await {
        return StatusCode::NOT_FOUND.into_response();
    }
    upgrade.on_upgrade(crate::preview::android_video)
}

async fn agents(State(engine): State<Arc<Engine>>) -> ApiResult {
    Ok(Json(engine.agent_options().await))
}
