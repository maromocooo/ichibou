import { useState } from "react";
import { api } from "./api";
import { useApp } from "./app-context";
import { useT } from "./i18n";

export function AgentPanel() {
  const t = useT();
  const { overview, events, act } = useApp();
  const [text, setText] = useState("");
  const live = overview.runs.filter((r) => overview.live_runs.includes(r.id));
  // The session a typed message goes to: the one building, if any.
  const main = live.find((r) => r.role === "writer") ?? live.find((r) => r.role === "scenario_author") ?? live[0];
  const claims = overview.status.requirements.filter((r) => r.state === "in_progress");

  return (
    <>
      <h2>{t("agent.title")}</h2>
      {live.length === 0 ? <p className="muted">{t("agent.none")}</p> : null}
      {live.map((run) => {
        const tool = [...events].reverse().find((e) => e.run === run.id && e.kind === "tool_started");
        // Claude reports how it is authenticated; anything but the
        // subscription login means usage is billed to an API key.
        const billedToKey = run.agent === "claude" && !!run.auth && run.auth !== "none";
        return (
          <div key={run.id} className="run">
            <div className="row">
              <span className={`dot ${run.activity}`} aria-hidden />
              <strong>{t(`role.${run.role}`)}</strong>
              <span className="muted clip">
                {run.agent}
                {run.model ? ` · ${run.model}` : ""}
              </span>
            </div>
            <div className={run.activity === "rate_limited" ? "warn" : "muted"}>{t(`activity.${run.activity}`)}</div>
            {billedToKey ? <div className="warn">{t("agent.billing")}</div> : null}
            {run.activity === "running_tool" && tool ? (
              <div className="mono clip">
                {String(tool.payload.name ?? "")} {String(tool.payload.summary ?? "")}
              </div>
            ) : null}
            {run.last_message ? <p className="last">{run.last_message}</p> : null}
            <div className="row">
              <button onClick={() => act(() => api.interrupt(run.id))}>{t("agent.interrupt")}</button>
              <button onClick={() => act(() => api.stop(run.id))}>{t("agent.stop")}</button>
            </div>
          </div>
        );
      })}

      {claims.length > 0 ? (
        <ul className="plain">
          {claims.map((c) => (
            <li key={c.requirement}>
              ◐ <code>{c.requirement}</code>
            </li>
          ))}
        </ul>
      ) : null}

      {overview.approvals.map((approval) => (
        <div key={`${approval.run}-${approval.id}`} className="approval">
          <strong>{t("agent.wants")}</strong>
          {/* Shown whole: the user is deciding on exactly this. */}
          <pre className="log">
            {approval.kind}: {approval.detail}
          </pre>
          <div className="row">
            <button className="primary" onClick={() => act(() => api.approval(approval.run, approval.id, true))}>
              {t("agent.approve")}
            </button>
            <button onClick={() => act(() => api.approval(approval.run, approval.id, false))}>{t("agent.deny")}</button>
          </div>
        </div>
      ))}

      {main ? (
        <form
          className="stack composer"
          onSubmit={(e) => {
            e.preventDefault();
            if (!text.trim()) return;
            act(() => api.message(main.id, text)).then(() => setText(""));
          }}
        >
          <textarea
            rows={3}
            value={text}
            aria-label={t("agent.message")}
            placeholder={t("agent.message")}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) e.currentTarget.form?.requestSubmit();
            }}
          />
          <button className="primary" disabled={!text.trim()}>
            {t("agent.send")}
          </button>
        </form>
      ) : null}
    </>
  );
}
