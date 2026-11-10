import { useState } from "react";
import { type Overview, type TargetsDoc, api } from "./api";
import { useApp } from "./app-context";
import { useT } from "./i18n";

/** Every command Ichibou would run for these targets, for the user to read. */
function commands(targets: TargetsDoc): string[] {
  const list: string[] = [];
  for (const target of targets.targets) {
    if (target.dev?.command) list.push(`${target.name} · preview: ${target.dev.command}`);
    for (const step of target.verify?.setup ?? []) list.push(`${target.name} · verify setup: ${step}`);
    if (target.verify?.start?.command) list.push(`${target.name} · verify start: ${target.verify.start.command}`);
  }
  for (const source of targets.apis) {
    if (source.openapi_command) list.push(`${source.node} · openapi: ${source.openapi_command}`);
  }
  return list;
}

function busy(overview: Overview): boolean {
  return overview.runs.some(
    (r) => overview.live_runs.includes(r.id) && (r.activity === "working" || r.activity === "running_tool"),
  );
}

export function NextStep() {
  const t = useT();
  const { overview, act, go } = useApp();
  const [request, setRequest] = useState("");
  const { proposals, manifest, status } = overview;
  const hasRequirements = manifest.requirements.requirements.length > 0;
  const hasProposal = proposals.requirements || proposals.architecture || proposals.targets;
  const scenarios = manifest.scenarios.scenarios;
  const unapproved = status.scenarios.filter((s) => s.state === "awaiting_approval" || s.pending_change);
  const interrupted = overview.runs.filter((r) => r.role === "writer").slice(-1)[0]?.activity === "interrupted";

  let body: JSX.Element;
  if (hasProposal) {
    body = (
      <div className="stack">
        <strong>{t("next.review")}</strong>
        {proposals.requirements ? (
          <div className="proposal">
            <h4>{t("proposal.requirements")}</h4>
            <ol>
              {proposals.requirements.requirements.map((req) => (
                <li key={req.id}>
                  <code>{req.id}</code> {req.title}
                  <ul>
                    {req.criteria.map((c) => (
                      <li key={c.id} className="muted">
                        {c.text}
                      </li>
                    ))}
                  </ul>
                </li>
              ))}
            </ol>
            <Decide kind="requirements" />
          </div>
        ) : null}
        {proposals.architecture ? (
          <div className="proposal">
            <h4>{t("proposal.architecture")}</h4>
            <ul>
              {proposals.architecture.nodes.map((node) => (
                <li key={node.id}>
                  <code>{node.kind}</code> {node.title} <span className="muted">{node.paths.join(", ")}</span>
                </li>
              ))}
            </ul>
            <Decide kind="architecture" />
          </div>
        ) : null}
        {proposals.targets ? (
          <div className="proposal">
            <h4>{t("proposal.targets")}</h4>
            <ul className="mono">
              {commands(proposals.targets).map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
            <p className="muted">{t("proposal.targets.note")}</p>
            <Decide kind="targets" />
          </div>
        ) : null}
      </div>
    );
  } else if (busy(overview)) {
    body = <span className="muted">{t("next.working")}</span>;
  } else if (!hasRequirements) {
    body = (
      <div className="stack">
        <label htmlFor="request">
          <strong>{t("next.describe")}</strong>
        </label>
        <textarea
          id="request"
          rows={4}
          value={request}
          placeholder={t("next.describe.placeholder")}
          onChange={(e) => setRequest(e.target.value)}
        />
        <div className="row">
          <button
            className="primary"
            disabled={!request.trim()}
            onClick={() => act(() => api.startRun("plan", request)).then((run) => run && setRequest(""))}
          >
            {t("next.plan")}
          </button>
          <button className="link" onClick={() => act(() => api.startRun("onboard"))}>
            {t("next.onboard")}
          </button>
        </div>
      </div>
    );
  } else if (!overview.requirements_approved) {
    // The file changed outside Ichibou (or was never reviewed here).
    body = (
      <div className="stack">
        <strong>{t("next.approveRequirements")}</strong>
        <ol>
          {manifest.requirements.requirements.map((req) => (
            <li key={req.id}>
              <code>{req.id}</code> {req.title}
              <ul>
                {req.criteria.map((c) => (
                  <li key={c.id} className="muted">
                    {c.text}
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ol>
        <div>
          <button className="primary" onClick={() => act(() => api.approveRequirements(overview.requirements_hash))}>
            {t("proposal.accept")}
          </button>
        </div>
      </div>
    );
  } else if (interrupted) {
    body = (
      <button className="primary" onClick={() => act(() => api.resumeRun())}>
        {t("next.resume")}
      </button>
    );
  } else if (scenarios.length === 0) {
    body = (
      <button className="primary" onClick={() => act(() => api.startRun("scenarios"))}>
        {t("next.scenarios")}
      </button>
    );
  } else if (unapproved.length > 0) {
    body = (
      <button className="primary" onClick={() => go("scenarios")}>
        {t("next.approveScenarios")} ({unapproved.length})
      </button>
    );
  } else if (!overview.targets_approved) {
    body = (
      <div className="stack">
        <strong>{t("next.approveTargets")}</strong>
        <ul className="mono">
          {commands(manifest.targets).map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
        <div>
          <button className="primary" onClick={() => act(() => api.approveTargets(overview.targets_hash))}>
            {t("proposal.accept")}
          </button>
        </div>
      </div>
    );
  } else if (status.complete) {
    body = <span className="ok-text">✓ {t("next.done")}</span>;
  } else if (
    status.requirements.every((r) => ["done", "awaiting_manual", "blocked_by_annotations"].includes(r.state))
  ) {
    // Everything automated has passed; what is left only the user can do.
    body = <span>{t("next.yours")}</span>;
  } else {
    body = (
      <div className="row">
        <button className="primary" onClick={() => act(() => api.startRun("build"))}>
          {t("next.build")}
        </button>
        <button onClick={() => act(() => api.verify())} disabled={overview.verifying}>
          {overview.verifying ? t("app.verifying") : t("next.verify")}
        </button>
      </div>
    );
  }

  return (
    <section className="next" aria-label={t("next.title")}>
      {body}
    </section>
  );
}

function Decide({ kind }: { kind: "requirements" | "architecture" | "targets" }) {
  const t = useT();
  const { act, overview } = useApp();
  // The hash of the proposal this render is showing.
  const seen = overview.proposal_hashes[kind] ?? "";
  return (
    <div className="row">
      <button className="primary" onClick={() => act(() => api.decideProposal(kind, true, seen))}>
        {t("proposal.accept")}
      </button>
      <button onClick={() => act(() => api.decideProposal(kind, false, seen))}>{t("proposal.reject")}</button>
    </div>
  );
}
