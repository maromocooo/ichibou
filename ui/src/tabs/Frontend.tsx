import { useEffect, useRef, useState } from "react";
import { type PreviewInfo, type Rect, api } from "../api";
import { useApp } from "../app-context";
import { type Frame, H264Parser } from "../h264";
import { useT } from "../i18n";

export function Frontend() {
  const t = useT();
  const { overview, previews, act } = useApp();
  const targets = overview.manifest.targets.targets;
  const [selected, setSelected] = useState<string | null>(null);
  const [view, setView] = useState<"preview" | "assets">("preview");
  const current = previews.find((p) => p.target === selected) ?? previews[0];

  if (targets.length === 0) {
    return (
      <div className="stack">
        <p className="muted">{t("fe.noTargets")}</p>
        <Assets />
      </div>
    );
  }

  return (
    <div className="stack fill">
      <div className="row wrap">
        {previews.map((p) => (
          <button
            key={p.target}
            className={p.target === current?.target && view === "preview" ? "chip active" : "chip"}
            onClick={() => {
              setSelected(p.target);
              setView("preview");
            }}
          >
            <span className={`dot ${p.running ? "working" : "idle"}`} aria-hidden /> {p.target}{" "}
            <span className="muted">{p.kind}</span>
          </button>
        ))}
        <button className={view === "assets" ? "chip active" : "chip"} onClick={() => setView("assets")}>
          {t("fe.assets")}
        </button>
      </div>

      {view === "assets" ? <Assets /> : null}
      {view === "preview" && current ? (
        <>
          <div className="row">
            {current.running ? (
              <button onClick={() => act(() => api.previewStop(current.target))}>{t("fe.stop")}</button>
            ) : (
              <button
                className="primary"
                disabled={!overview.targets_approved}
                onClick={() => act(() => api.previewStart(current.target))}
              >
                {t("fe.start")}
              </button>
            )}
            {!overview.targets_approved ? <span className="muted">{t("fe.needsApproval")}</span> : null}
          </div>
          {current.running ? (
            current.kind === "web" ? (
              <WebPreview key={current.target} preview={current} />
            ) : (
              <DevicePreview key={current.target} preview={current} />
            )
          ) : (
            <p className="muted">{t("fe.notRunning")}</p>
          )}
          {current.log ? (
            <details>
              <summary>{t("fe.log")}</summary>
              <pre className="log">{current.log}</pre>
            </details>
          ) : null}
        </>
      ) : null}
    </div>
  );
}

interface Pick {
  type: string;
  target: string;
  kind?: "element" | "region";
  url?: string;
  selector?: string;
  text?: string;
  source?: string | null;
  rect?: Rect;
}

function WebPreview({ preview }: { preview: PreviewInfo }) {
  const t = useT();
  const { annotate } = useApp();
  const frame = useRef<HTMLIFrameElement>(null);
  const [picking, setPicking] = useState(false);
  const [page, setPage] = useState("/");
  const origin = preview.url ?? "";

  useEffect(() => {
    // Only messages from this preview's own origin are trusted.
    const onMessage = (event: MessageEvent<Pick>) => {
      if (event.origin !== origin || !event.data || event.data.target !== preview.target) return;
      const data = event.data;
      if (data.type === "aperture:location" && data.url) setPage(data.url);
      if (data.type === "aperture:cancel") setPicking(false);
      if (data.type === "aperture:pick" && data.rect) {
        setPicking(false);
        if (data.kind === "element" && data.selector) {
          annotate({
            kind: "ui_element",
            target: preview.target,
            url: data.url ?? page,
            selector: data.selector,
            text: data.text ?? "",
            source: data.source ?? null,
            rect: data.rect,
          });
        } else {
          annotate({ kind: "ui_region", target: preview.target, url: data.url ?? page, rect: data.rect });
        }
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [annotate, origin, page, preview.target]);

  const setMode = (on: boolean) => {
    setPicking(on);
    frame.current?.contentWindow?.postMessage({ type: "aperture:pick-mode", on }, origin);
  };

  return (
    <div className="stack fill">
      <div className="row">
        <button className={picking ? "primary" : ""} aria-pressed={picking} onClick={() => setMode(!picking)}>
          {t("fe.annotate")}
        </button>
        <button
          onClick={() => {
            if (frame.current) frame.current.src = origin + page;
          }}
        >
          {t("fe.reload")}
        </button>
        <code className="url clip">{page}</code>
        {picking ? <span className="muted">{t("fe.annotate.hint")}</span> : null}
      </div>
      {/* The preview is a different origin (its own port), so the app inside
          cannot reach the daemon's API or this page. */}
      <iframe ref={frame} className="preview" title={preview.target} src={origin} />
    </div>
  );
}

function DevicePreview({ preview }: { preview: PreviewInfo }) {
  const t = useT();
  const { annotate, act } = useApp();
  const [tick, setTick] = useState(0);
  const [failed, setFailed] = useState(false);
  const [picking, setPicking] = useState(false);
  const [drag, setDrag] = useState<{ x: number; y: number; x2: number; y2: number } | null>(null);
  const [text, setText] = useState("");
  const box = useRef<HTMLDivElement>(null);
  const interactive = preview.kind !== "desktop";
  // Android can be shown as decoded video when the browser has WebCodecs.
  const [videoFailed, setVideoFailed] = useState(false);
  const video = preview.video && !videoFailed && "VideoDecoder" in window;

  // Ask for the next screenshot only after the previous one arrived, so a
  // slow device is never flooded.
  const next = () => setTimeout(() => setTick((n) => n + 1), 450);

  const position = (event: React.MouseEvent) => {
    // The frame wraps the picture exactly, whichever element draws it.
    const rect = box.current?.getBoundingClientRect();
    if (!rect) return { x: 0, y: 0 };
    return {
      x: Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width)),
      y: Math.min(1, Math.max(0, (event.clientY - rect.top) / rect.height)),
    };
  };

  return (
    <div className="stack">
      <div className="row">
        <button className={picking ? "primary" : ""} aria-pressed={picking} onClick={() => setPicking(!picking)}>
          {t("fe.annotate")}
        </button>
        <span className="muted">
          {picking
            ? t("fe.annotate.hint")
            : !interactive
              ? t("fe.desktop.hint")
              : preview.stream || video
                ? t("fe.native.live")
                : t("fe.native.hint")}
        </span>
      </div>
      <div
        ref={box}
        className={picking ? "device picking" : "device"}
        onMouseDown={(e) => {
          if (!picking) return;
          const p = position(e);
          setDrag({ x: p.x, y: p.y, x2: p.x, y2: p.y });
        }}
        onMouseMove={(e) => {
          if (!drag) return;
          const p = position(e);
          setDrag({ ...drag, x2: p.x, y2: p.y });
        }}
        onMouseUp={(e) => {
          const p = position(e);
          if (picking && drag) {
            // Fractions of the screen, so the region means the same thing at
            // any zoom level.
            const rect = {
              x: Math.min(drag.x, p.x),
              y: Math.min(drag.y, p.y),
              width: Math.max(0.02, Math.abs(p.x - drag.x)),
              height: Math.max(0.02, Math.abs(p.y - drag.y)),
            };
            setDrag(null);
            setPicking(false);
            annotate({ kind: "ui_region", target: preview.target, url: null, rect });
          } else if (interactive) {
            act(() => api.previewTap(preview.target, p.x, p.y));
          }
        }}
      >
        {video ? (
          <LiveVideo target={preview.target} onFail={() => setVideoFailed(true)} />
        ) : (
          <img
            alt={preview.target}
            draggable={false}
            // A live stream is one long response; screenshots are polled.
            src={preview.stream ? api.streamUrl(preview.target) : api.frameUrl(preview.target, tick)}
            onLoad={() => {
              setFailed(false);
              if (!preview.stream) next();
            }}
            onError={() => {
              setFailed(true);
              setTimeout(() => setTick((n) => n + 1), 2000);
            }}
          />
        )}
        {drag ? (
          <span
            className="region"
            style={{
              left: `${Math.min(drag.x, drag.x2) * 100}%`,
              top: `${Math.min(drag.y, drag.y2) * 100}%`,
              width: `${Math.abs(drag.x2 - drag.x) * 100}%`,
              height: `${Math.abs(drag.y2 - drag.y) * 100}%`,
            }}
          />
        ) : null}
      </div>
      {failed ? <p className="warn">{t("fe.frameError")}</p> : null}
      {interactive ? (
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            if (text) act(() => api.previewText(preview.target, text)).then(() => setText(""));
          }}
        >
          <input value={text} placeholder={t("fe.type")} aria-label={t("fe.type")} onChange={(e) => setText(e.target.value)} />
          <button disabled={!text}>{t("agent.send")}</button>
        </form>
      ) : null}
    </div>
  );
}

/**
 * Draws an Android target's H.264 stream on a canvas. Calls `onFail` when
 * decoding cannot work here, so the caller falls back to screenshots.
 */
function LiveVideo({ target, onFail }: { target: string; onFail: () => void }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const fail = useRef(onFail);
  fail.current = onFail;

  useEffect(() => {
    let closed = false;
    let socket: WebSocket | null = null;
    let decoder: VideoDecoder | null = null;
    let quiet: ReturnType<typeof setTimeout> | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let errors = 0;

    const connect = () => {
      const parser = new H264Parser();
      let configured = "";
      let timestamp = 0;
      let waitingForKey = true;
      decoder = new VideoDecoder({
        output: (frame) => {
          const el = canvas.current;
          if (el) {
            if (el.width !== frame.displayWidth || el.height !== frame.displayHeight) {
              el.width = frame.displayWidth;
              el.height = frame.displayHeight;
            }
            el.getContext("2d")?.drawImage(frame, 0, 0);
          }
          frame.close();
          errors = 0;
        },
        // A broken picture cannot be repaired mid-stream; a new connection
        // starts a new recording, which begins with a key frame.
        error: () => restart(),
      });

      const decode = (frames: Frame[]) => {
        for (const frame of frames) {
          if (!decoder || decoder.state === "closed") return;
          if (frame.key && frame.codec && frame.codec !== configured) {
            decoder.configure({ codec: frame.codec, optimizeForLatency: true });
            configured = frame.codec;
          }
          if (waitingForKey && !frame.key) continue;
          if (!configured) continue;
          waitingForKey = false;
          decoder.decode(
            new EncodedVideoChunk({ type: frame.key ? "key" : "delta", timestamp: timestamp++, data: frame.data }),
          );
        }
      };

      socket = new WebSocket(api.videoUrl(target));
      socket.binaryType = "arraybuffer";
      socket.onmessage = (message) => {
        decode(parser.push(new Uint8Array(message.data as ArrayBuffer)));
        // The recorder writes only when the screen changes. Once it goes
        // quiet, what is buffered is a complete picture: show it now.
        clearTimeout(quiet);
        quiet = setTimeout(() => decode(parser.flush()), 80);
      };
      socket.onclose = () => {
        if (!closed) restart();
      };
    };

    const stop = () => {
      clearTimeout(quiet);
      if (socket) {
        socket.onclose = null;
        socket.close();
      }
      if (decoder && decoder.state !== "closed") decoder.close();
      socket = null;
      decoder = null;
    };

    const restart = () => {
      stop();
      if (closed) return;
      errors += 1;
      if (errors > 3) {
        fail.current();
        return;
      }
      retry = setTimeout(connect, 500);
    };

    connect();
    return () => {
      closed = true;
      clearTimeout(retry);
      stop();
    };
  }, [target]);

  return <canvas ref={canvas} aria-label={target} />;
}

function Assets() {
  const t = useT();
  const { overview } = useApp();
  const [assets, setAssets] = useState<{ path: string; size: number }[] | null>(null);
  const snapshot = overview.status.snapshot;

  useEffect(() => {
    let alive = true;
    api
      .assets()
      .then((list) => alive && setAssets(list))
      .catch(() => alive && setAssets([]));
    return () => {
      alive = false;
    };
    // New files change the snapshot; that is the signal to list again.
  }, [snapshot]);

  if (!assets) return null;
  if (assets.length === 0) return <p className="muted">{t("fe.assets.empty")}</p>;
  const isImage = (path: string) => /\.(png|jpe?g|gif|svg|webp|avif|ico)$/i.test(path);
  return (
    <ul className="assets">
      {assets.map((asset) => (
        <li key={asset.path}>
          <a href={api.assetUrl(asset.path)} target="_blank" rel="noreferrer">
            <span className="thumb">
              {isImage(asset.path) ? <img loading="lazy" alt="" src={api.assetUrl(asset.path)} /> : <span>Aa</span>}
            </span>
            <span className="clip mono">{asset.path}</span>
            <span className="muted">{(asset.size / 1024).toFixed(1)} KB</span>
          </a>
        </li>
      ))}
    </ul>
  );
}
