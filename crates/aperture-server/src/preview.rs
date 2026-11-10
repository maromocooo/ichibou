//! Live previews of what is being built.
//!
//! Web targets are shown through a reverse proxy on its own port (its own
//! origin, so the previewed app cannot call the daemon's API). The proxy adds
//! one script to HTML pages: the annotation overlay.
//!
//! iOS Simulator, Android Emulator and desktop windows are shown as a stream
//! of screenshots taken with the platform's own tools, with taps and text
//! forwarded where the platform allows it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use aperture_core::*;
use aperture_verify::process::{self, Background};
use axum::body::Body;
use axum::extract::ws::{Message as ClientMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::process::Command;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;

use crate::engine::{Engine, UiEvent};

const OVERLAY: &str = include_str!("../../../overlay/overlay.js");
const OVERLAY_PATH: &str = "/__aperture/overlay.js";

pub struct Preview {
    kind: TargetKind,
    app: Option<Background>,
    /// Where the app itself listens (web only).
    upstream: Option<String>,
    /// The proxied URL the UI embeds (web only).
    url: Option<String>,
    proxy: Option<tokio::task::JoinHandle<()>>,
    /// serve-sim helper streaming an iOS simulator, and its MJPEG URL.
    helper: Option<Background>,
    stream: Option<String>,
    log: String,
}

#[derive(Debug, Serialize)]
pub struct PreviewInfo {
    pub target: String,
    pub kind: TargetKind,
    pub running: bool,
    pub url: Option<String>,
    pub upstream: Option<String>,
    /// A live video stream is available (otherwise the UI polls screenshots).
    pub stream: bool,
    /// Last lines of the app's output, to explain a preview that is down.
    pub log: String,
}

#[derive(Default)]
pub struct Previews {
    running: tokio::sync::Mutex<HashMap<String, Preview>>,
}

fn find_target(engine: &Engine, name: &str) -> Result<Target> {
    engine
        .project
        .load()?
        .targets
        .targets
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| anyhow!("unknown target {name}"))
}

fn tail(path: &Path, max: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(max)..].iter().collect()
}

impl Previews {
    pub async fn list(&self, engine: &Engine) -> Vec<PreviewInfo> {
        let targets = engine
            .project
            .load()
            .map(|m| m.targets.targets)
            .unwrap_or_default();
        let mut running = self.running.lock().await;
        targets
            .into_iter()
            .map(|target| {
                let preview = running.get_mut(&target.name);
                let log_path = engine
                    .project
                    .local_dir()
                    .join(format!("preview-{}.log", target.name));
                match preview {
                    Some(p) => PreviewInfo {
                        target: target.name,
                        kind: target.kind,
                        // A native preview has no process of its own to watch.
                        running: p.app.as_mut().map(|a| !a.exited()).unwrap_or(true),
                        url: p.url.clone(),
                        upstream: p.upstream.clone(),
                        stream: p.stream.is_some(),
                        log: tail(Path::new(&p.log), 2000),
                    },
                    None => PreviewInfo {
                        target: target.name,
                        kind: target.kind,
                        running: false,
                        url: None,
                        upstream: None,
                        stream: false,
                        log: tail(&log_path, 2000),
                    },
                }
            })
            .collect()
    }

    pub async fn start(&self, engine: &Arc<Engine>, name: &str) -> Result<()> {
        if !engine.targets_approved() {
            bail!("approve the commands in targets.yaml before starting a preview");
        }
        let target = find_target(engine, name)?;
        self.stop(engine, name).await;

        let log = engine
            .project
            .local_dir()
            .join(format!("preview-{name}.log"));
        let _ = std::fs::remove_file(&log);
        let mut preview = Preview {
            kind: target.kind,
            app: None,
            upstream: None,
            url: None,
            proxy: None,
            helper: None,
            stream: None,
            log: log.to_string_lossy().into_owned(),
        };

        if let Some(launch) = &target.dev {
            let port = match launch.port {
                Some(port) => port,
                None => process::free_port()?,
            };
            let fill = |text: &str| text.replace("{port}", &port.to_string());
            let cwd = match &launch.cwd {
                Some(cwd) => engine.project.root.join(cwd),
                None => engine.project.root.clone(),
            };
            let env = vec![("PORT".to_string(), port.to_string())];
            preview.app = Some(Background::spawn(&fill(&launch.command), &cwd, &env, &log)?);
            preview.upstream = launch.url.as_deref().map(fill);
        }

        if target.kind == TargetKind::Web {
            let upstream = preview
                .upstream
                .clone()
                .context("a web target needs `dev.url` so Aperture knows where the app listens")?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
            let addr = listener.local_addr()?;
            let state = Arc::new(Proxy {
                upstream: upstream.trim_end_matches('/').to_string(),
                ui_origin: format!("http://127.0.0.1:{}", engine.port()),
                target: name.to_string(),
                client: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?,
            });
            let app = Router::new().fallback(proxy).with_state(state);
            preview.proxy = Some(tokio::spawn(async move {
                let _ = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await;
            }));
            preview.url = Some(format!("http://127.0.0.1:{}", addr.port()));
        }

        if target.kind == TargetKind::Ios {
            // With serve-sim installed the simulator is shown as live video;
            // without it the UI falls back to polling screenshots.
            match start_ios_stream(engine, name).await {
                Ok((helper, url)) => {
                    preview.helper = Some(helper);
                    preview.stream = Some(url);
                }
                Err(err) => tracing::info!("no live iOS stream, using screenshots: {err:#}"),
            }
        }

        self.running.lock().await.insert(name.to_string(), preview);
        engine.notify(UiEvent::Preview);
        Ok(())
    }

    /// URL of the live video stream for a target, when there is one.
    pub async fn stream_url(&self, name: &str) -> Option<String> {
        self.running.lock().await.get(name)?.stream.clone()
    }

    pub async fn stop(&self, engine: &Engine, name: &str) {
        let preview = self.running.lock().await.remove(name);
        if let Some(preview) = preview {
            if let Some(proxy) = preview.proxy {
                proxy.abort();
            }
            if let Some(app) = preview.app {
                app.stop().await;
            }
            if let Some(helper) = preview.helper {
                helper.stop().await;
            }
            engine.notify(UiEvent::Preview);
        }
    }

    pub async fn stop_all(&self, engine: &Engine) {
        let names: Vec<String> = self.running.lock().await.keys().cloned().collect();
        for name in names {
            self.stop(engine, &name).await;
        }
    }

    async fn kind(&self, engine: &Engine, name: &str) -> Result<(TargetKind, Option<String>)> {
        if let Some(preview) = self.running.lock().await.get(name) {
            let target = find_target(engine, name)?;
            return Ok((preview.kind, target.app_id));
        }
        let target = find_target(engine, name)?;
        Ok((target.kind, target.app_id))
    }

    /// One screenshot of a native target, as (content type, bytes).
    pub async fn frame(&self, engine: &Engine, name: &str) -> Result<(&'static str, Vec<u8>)> {
        let (kind, app_id) = self.kind(engine, name).await?;
        match kind {
            TargetKind::Ios => {
                let udid = ios_device().await?;
                // simctl only writes screenshots to a file ("-" is taken as a
                // file name, not stdout).
                let file = engine
                    .project
                    .local_dir()
                    .join(format!("frame-{name}.jpg"));
                run(
                    "xcrun",
                    &[
                        "simctl",
                        "io",
                        &udid,
                        "screenshot",
                        "--type=jpeg",
                        &file.to_string_lossy(),
                    ],
                )
                .await?;
                Ok(("image/jpeg", tokio::fs::read(&file).await?))
            }
            TargetKind::Android => {
                let bytes = output(&adb(), &["exec-out", "screencap", "-p"]).await?;
                Ok(("image/png", bytes))
            }
            TargetKind::Desktop => {
                let app = app_id.context("a desktop target needs `app_id` (the app's name)")?;
                let window = desktop_window(&app).await?;
                let file = engine
                    .project
                    .local_dir()
                    .join(format!("frame-{name}.jpg"));
                let status = Command::new("screencapture")
                    .args(["-x", "-o", "-t", "jpg", "-l", &window.to_string()])
                    .arg(&file)
                    .status()
                    .await?;
                if !status.success() {
                    bail!("screencapture failed; grant Screen Recording permission to the terminal running Aperture");
                }
                Ok(("image/jpeg", tokio::fs::read(&file).await?))
            }
            TargetKind::Web => bail!("web targets are previewed through the proxy"),
        }
    }

    /// Taps at a position given as fractions of the screen (0..1).
    pub async fn tap(&self, engine: &Engine, name: &str, x: f64, y: f64) -> Result<()> {
        let (kind, _) = self.kind(engine, name).await?;
        match kind {
            TargetKind::Ios => {
                let udid = ios_device().await?;
                let (width, height) = ios_points(&udid).await?;
                run(
                    "idb",
                    &[
                        "ui",
                        "tap",
                        "--udid",
                        &udid,
                        &((x * width).round() as i64).to_string(),
                        &((y * height).round() as i64).to_string(),
                    ],
                )
                .await
            }
            TargetKind::Android => {
                let (width, height) = android_pixels().await?;
                run(
                    &adb(),
                    &[
                        "shell",
                        "input",
                        "tap",
                        &((x * width).round() as i64).to_string(),
                        &((y * height).round() as i64).to_string(),
                    ],
                )
                .await
            }
            TargetKind::Desktop => {
                bail!("input is not forwarded to desktop windows yet; interact with the app window directly")
            }
            TargetKind::Web => bail!("web targets are interactive in the preview itself"),
        }
    }

    pub async fn text(&self, engine: &Engine, name: &str, text: &str) -> Result<()> {
        let (kind, _) = self.kind(engine, name).await?;
        match kind {
            TargetKind::Ios => {
                let udid = ios_device().await?;
                run("idb", &["ui", "text", "--udid", &udid, text]).await
            }
            TargetKind::Android => {
                // `input text` takes %s for a space and treats the rest literally.
                let escaped = text.replace(' ', "%s");
                run(&adb(), &["shell", "input", "text", &escaped]).await
            }
            _ => bail!("text input is not forwarded to this kind of target"),
        }
    }

    /// Accessibility elements with their frames as fractions of the screen,
    /// so an annotation can name the element instead of just a region.
    pub async fn hierarchy(&self, engine: &Engine, name: &str) -> Result<Value> {
        let (kind, _) = self.kind(engine, name).await?;
        match kind {
            TargetKind::Ios => {
                let udid = ios_device().await?;
                let (width, height) = ios_points(&udid).await?;
                let raw = output("idb", &["ui", "describe-all", "--udid", &udid, "--json"]).await?;
                let nodes: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                let elements: Vec<Value> = nodes
                    .as_array()
                    .map(|nodes| {
                        nodes
                            .iter()
                            .filter_map(|n| {
                                let f = &n["frame"];
                                Some(json!({
                                    "label": n["AXLabel"].as_str().or(n["title"].as_str()).unwrap_or(""),
                                    "role": n["type"].as_str().or(n["role"].as_str()).unwrap_or(""),
                                    "rect": {
                                        "x": f["x"].as_f64()? / width,
                                        "y": f["y"].as_f64()? / height,
                                        "width": f["width"].as_f64()? / width,
                                        "height": f["height"].as_f64()? / height,
                                    }
                                }))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(json!({ "elements": elements }))
            }
            TargetKind::Android => {
                let raw = output(&adb(), &["exec-out", "uiautomator", "dump", "/dev/tty"]).await?;
                let (width, height) = android_pixels().await?;
                Ok(json!({ "elements": android_elements(&String::from_utf8_lossy(&raw), width, height) }))
            }
            _ => Ok(json!({ "elements": [] })),
        }
    }
}

/// Starts serve-sim's stream helper for the booted simulator and returns it
/// with the MJPEG URL it reports.
async fn start_ios_stream(engine: &Engine, name: &str) -> Result<(Background, String)> {
    run("serve-sim", &["--version"])
        .await
        .context("serve-sim is not installed (npm install -g serve-sim)")?;
    let udid = ios_device().await?;
    let port = process::free_port()?;
    let log = engine
        .project
        .local_dir()
        .join(format!("preview-{name}-stream.log"));
    let _ = std::fs::remove_file(&log);
    let mut helper = Background::spawn(
        &format!("serve-sim --no-preview -q -p {port} {udid}"),
        &engine.project.root,
        &[],
        &log,
    )?;
    // The helper prints one JSON line with its URLs once it is ready.
    for _ in 0..80 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let url = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|v| v["streamUrl"].as_str().map(str::to_string));
        if let Some(url) = url {
            return Ok((helper, url));
        }
        if helper.exited() {
            break;
        }
    }
    helper.stop().await;
    bail!("serve-sim did not start: {}", tail(&log, 400))
}

/// `adb`, from PATH or from the usual Android SDK locations (the SDK is
/// often installed without adding its tools to PATH).
fn adb() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let roots = [
        std::env::var("ANDROID_HOME").unwrap_or_default(),
        std::env::var("ANDROID_SDK_ROOT").unwrap_or_default(),
        format!("{home}/Library/Android/sdk"),
        "/opt/homebrew/share/android-commandlinetools".to_string(),
    ];
    roots
        .iter()
        .filter(|root| !root.is_empty())
        .map(|root| format!("{root}/platform-tools/adb"))
        .find(|path| Path::new(path).is_file())
        .unwrap_or_else(|| "adb".to_string())
}

async fn output(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        Command::new(program).args(args).output(),
    )
    .await
    .with_context(|| format!("{program} timed out"))?
    .with_context(|| format!("{program} is not installed"))?;
    if !out.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

async fn run(program: &str, args: &[&str]) -> Result<()> {
    output(program, args).await.map(|_| ())
}

/// The booted iOS simulator (not watchOS, tvOS or visionOS).
async fn ios_device() -> Result<String> {
    let raw = output("xcrun", &["simctl", "list", "devices", "booted", "-j"]).await?;
    let value: Value = serde_json::from_slice(&raw)?;
    value["devices"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(runtime, _)| runtime.contains("iOS"))
        .flat_map(|(_, devices)| devices.as_array().into_iter().flatten())
        .find_map(|d| d["udid"].as_str().map(str::to_string))
        .context("no iOS simulator is booted; start one from Xcode or `xcrun simctl boot <device>`")
}

async fn ios_points(udid: &str) -> Result<(f64, f64)> {
    let raw = output("idb", &["describe", "--udid", udid, "--json"]).await?;
    let value: Value = serde_json::from_slice(&raw)?;
    let screen = &value["screen_dimensions"];
    Ok((
        screen["width_points"].as_f64().context("unknown screen size")?,
        screen["height_points"].as_f64().context("unknown screen size")?,
    ))
}

async fn android_pixels() -> Result<(f64, f64)> {
    let raw = output(&adb(), &["shell", "wm", "size"]).await?;
    let text = String::from_utf8_lossy(&raw);
    let size = text
        .lines()
        .filter_map(|l| l.rsplit(':').next())
        .next_back()
        .unwrap_or("")
        .trim();
    let (w, h) = size.split_once('x').context("unknown screen size")?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}

/// Pulls `bounds="[x1,y1][x2,y2]"` nodes out of a uiautomator dump.
pub fn android_elements(xml: &str, width: f64, height: f64) -> Vec<Value> {
    let attr = |node: &str, name: &str| -> Option<String> {
        let start = node.find(&format!("{name}=\""))? + name.len() + 2;
        let end = node[start..].find('"')? + start;
        Some(node[start..end].to_string())
    };
    xml.split("<node ")
        .skip(1)
        .filter_map(|node| {
            let bounds = attr(node, "bounds")?;
            let numbers: Vec<f64> = bounds
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse().ok())
                .collect();
            let [x1, y1, x2, y2] = numbers[..] else {
                return None;
            };
            let text = attr(node, "text").unwrap_or_default();
            let label = if text.is_empty() {
                attr(node, "content-desc").unwrap_or_default()
            } else {
                text
            };
            Some(json!({
                "label": label,
                "role": attr(node, "class").unwrap_or_default(),
                "rect": { "x": x1 / width, "y": y1 / height,
                          "width": (x2 - x1) / width, "height": (y2 - y1) / height }
            }))
        })
        .collect()
}

/// Finds the id of the frontmost window owned by `app`.
async fn desktop_window(app: &str) -> Result<u32> {
    // CoreGraphics is only reachable from native code; a short Swift script
    // avoids shipping a helper binary for one lookup.
    let script = r#"
import CoreGraphics
import Foundation
let wanted = CommandLine.arguments[1].lowercased()
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
for window in list {
    let owner = (window[kCGWindowOwnerName as String] as? String ?? "").lowercased()
    let layer = window[kCGWindowLayer as String] as? Int ?? 1
    if owner == wanted && layer == 0, let id = window[kCGWindowNumber as String] as? Int {
        print(id)
        exit(0)
    }
}
exit(1)
"#;
    let dir = std::env::temp_dir().join("aperture-window.swift");
    tokio::fs::write(&dir, script).await?;
    let raw = output("swift", &[&dir.to_string_lossy(), app])
        .await
        .with_context(|| format!("no window found for `{app}`; is the app running?"))?;
    Ok(String::from_utf8_lossy(&raw).trim().parse()?)
}

// ---------------------------------------------------------------------------
// Web reverse proxy
// ---------------------------------------------------------------------------

struct Proxy {
    upstream: String,
    ui_origin: String,
    target: String,
    client: reqwest::Client,
}

/// Headers that describe one hop and must not be forwarded.
fn hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

fn inject_overlay(html: &str) -> String {
    let tag = format!("<script src=\"{OVERLAY_PATH}\"></script>");
    match html.to_ascii_lowercase().rfind("</body>") {
        Some(at) => format!("{}{tag}{}", &html[..at], &html[at..]),
        None => format!("{html}{tag}"),
    }
}

async fn proxy(State(proxy): State<Arc<Proxy>>, request: Request) -> Response {
    let path_and_query = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());

    if request.uri().path() == OVERLAY_PATH {
        let script = OVERLAY
            .replace("__APERTURE_ORIGIN__", &proxy.ui_origin)
            .replace("__APERTURE_TARGET__", &proxy.target);
        return (
            [
                (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            script,
        )
            .into_response();
    }

    let wants_upgrade = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if wants_upgrade {
        return tunnel(proxy, request, path_and_query).await;
    }

    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, 64 * 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let mut headers = HeaderMap::new();
    for (name, value) in &parts.headers {
        if !hop_by_hop(name) && name != header::ACCEPT_ENCODING {
            headers.append(name.clone(), value.clone());
        }
    }
    // Ask for an uncompressed body so HTML can be edited.
    headers.insert(header::ACCEPT_ENCODING, "identity".parse().unwrap());

    let upstream = match proxy
        .client
        .request(parts.method, format!("{}{path_and_query}", proxy.upstream))
        .headers(headers)
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => {
            // The app is restarting or broken: say so instead of a blank frame.
            let page = format!(
                "<!doctype html><meta charset=utf-8><body style=\"font:14px system-ui;padding:24px;color:#666\">\
                 <p><strong>The app is not answering.</strong></p><p>{}</p>\
                 <script>setTimeout(()=>location.reload(),1500)</script></body>",
                err.to_string().replace('<', "&lt;")
            );
            return (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                page,
            )
                .into_response();
        }
    };

    let status = upstream.status();
    let is_html = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    let mut response_headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        // These would stop the page from being framed by the Aperture UI or
        // from loading the overlay script.
        let blocks_embedding = matches!(
            name.as_str(),
            "content-security-policy" | "content-security-policy-report-only" | "x-frame-options"
        );
        if !hop_by_hop(name) && !blocks_embedding {
            response_headers.append(name.clone(), value.clone());
        }
    }

    let mut response = if is_html {
        let text = upstream.text().await.unwrap_or_default();
        Response::new(Body::from(inject_overlay(&text)))
    } else {
        Response::new(Body::from_stream(upstream.bytes_stream()))
    };
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

/// Relays a WebSocket (dev-server hot reload) to the app.
async fn tunnel(proxy: Arc<Proxy>, request: Request, path_and_query: String) -> Response {
    let protocols: Vec<String> = request
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').map(|p| p.trim().to_string()).collect())
        .unwrap_or_default();
    let (mut parts, _) = request.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(err) => return err.into_response(),
    };

    let url = format!(
        "{}{path_and_query}",
        proxy.upstream.replacen("http", "ws", 1)
    );
    let mut upstream_request = match url.into_client_request() {
        Ok(request) => request,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    if !protocols.is_empty() {
        if let Ok(value) = protocols.join(", ").parse() {
            upstream_request
                .headers_mut()
                .insert(header::SEC_WEBSOCKET_PROTOCOL, value);
        }
    }
    let upstream = match tokio_tungstenite::connect_async(upstream_request).await {
        Ok((socket, _)) => socket,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    upgrade
        .protocols(protocols)
        .on_upgrade(move |client| relay(client, upstream))
}

async fn relay(
    client: WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let to_upstream = async {
        while let Some(Ok(message)) = client_rx.next().await {
            let forward = match message {
                ClientMessage::Text(text) => UpstreamMessage::Text(text.as_str().into()),
                ClientMessage::Binary(bytes) => UpstreamMessage::Binary(bytes),
                ClientMessage::Ping(bytes) => UpstreamMessage::Ping(bytes),
                ClientMessage::Pong(bytes) => UpstreamMessage::Pong(bytes),
                ClientMessage::Close(_) => break,
            };
            if upstream_tx.send(forward).await.is_err() {
                break;
            }
        }
    };
    let to_client = async {
        while let Some(Ok(message)) = upstream_rx.next().await {
            let forward = match message {
                UpstreamMessage::Text(text) => ClientMessage::Text(text.as_str().into()),
                UpstreamMessage::Binary(bytes) => ClientMessage::Binary(bytes),
                UpstreamMessage::Ping(bytes) => ClientMessage::Ping(bytes),
                UpstreamMessage::Pong(bytes) => ClientMessage::Pong(bytes),
                UpstreamMessage::Close(_) | UpstreamMessage::Frame(_) => break,
            };
            if client_tx.send(forward).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = to_upstream => {}
        _ = to_client => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_goes_before_the_closing_body() {
        assert_eq!(
            inject_overlay("<html><body><p>x</p></BODY></html>"),
            format!("<html><body><p>x</p><script src=\"{OVERLAY_PATH}\"></script></BODY></html>")
        );
        assert!(inject_overlay("<p>fragment</p>").ends_with("</script>"));
    }

    #[test]
    fn uiautomator_dump_is_parsed() {
        let xml = r#"<hierarchy><node text="" class="android.widget.FrameLayout" bounds="[0,0][1080,2400]"><node text="Sign in" class="android.widget.Button" content-desc="" bounds="[100,200][500,300]" /></node></hierarchy>"#;
        let elements = android_elements(xml, 1080.0, 2400.0);
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[1]["label"], "Sign in");
        assert_eq!(elements[1]["rect"]["height"], 100.0 / 2400.0);
    }
}
