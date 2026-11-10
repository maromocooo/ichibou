import type { StoredEvent } from "../api";
import { useApp } from "../app-context";
import { type T, useT } from "../i18n";

function describe(event: StoredEvent, t: T): { text: string; tone?: string } {
  const p = event.payload;
  const text = (key: string) => String(p[key] ?? "");
  switch (event.kind) {
    case "message":
      return { text: text("text") };
    case "user_message":
      return { text: `${t("fb.you")}: ${text("text")}`, tone: "user" };
    case "tool_started":
      return { text: `${text("name")} ${text("summary")}`, tone: "mono" };
    case "file_changed":
      return { text: `✎ ${(p.paths as string[] | undefined)?.join(", ") ?? ""}`, tone: "mono" };
    case "approval_request":
      return { text: `${t("agent.wants")}: ${text("detail")}`, tone: "warn" };
    case "rate_limit":
      return { text: t("activity.rate_limited"), tone: "warn" };
    case "turn_completed":
      return { text: p.ok ? "—" : text("summary"), tone: p.ok ? "muted" : "warn" };
    case "error":
      return { text: text("message"), tone: "warn" };
    case "exited":
      return { text: t("activity.finished"), tone: "muted" };
    case "agent_note":
      return { text: `${text("requirement")}: ${text("note")}` };
    case "verified": {
      const results = (p.results as { scenario: string; outcome: string }[] | undefined) ?? [];
      return {
        text: `${t("ev.verified")} — ${results.map((r) => `${r.scenario}: ${r.outcome}`).join(", ") || "0"}`,
        tone: results.every((r) => r.outcome === "passed") ? "ok" : "warn",
      };
    }
    case "verify_blocked":
      return { text: t("ev.verify_blocked"), tone: "warn" };
    case "verify_error":
      return { text: `${t("ev.verify_error")}: ${text("message")}`, tone: "warn" };
    default:
      return { text: event.kind, tone: "muted" };
  }
}

export function ActivityLog() {
  const t = useT();
  const { events } = useApp();
  if (events.length === 0) return <p className="muted">{t("ev.empty")}</p>;
  return (
    <ol className="events">
      {[...events].reverse().map((event) => {
        const { text, tone } = describe(event, t);
        return (
          <li key={event.seq} className={tone ?? ""}>
            <time className="muted">{new Date(event.at).toLocaleTimeString()}</time>
            <span>{text}</span>
          </li>
        );
      })}
    </ol>
  );
}
