import { api } from "../api";
import { useApp } from "../app-context";
import { CRITERION_MARK } from "../Checklist";
import { useT } from "../i18n";
import { ScenarioFiles } from "./ScenarioFiles";

export function Scenarios() {
  const t = useT();
  const { overview, act, annotate } = useApp();
  const scenarios = overview.manifest.scenarios.scenarios;
  const requirements = overview.manifest.requirements.requirements;
  const authorBusy = overview.runs.some((r) => overview.live_runs.includes(r.id) && r.role === "scenario_author");

  const uncovered = requirements.flatMap((req) =>
    req.criteria
      .filter((c) => !scenarios.some((s) => s.covers.some((x) => x.requirement === req.id && x.criterion === c.id)))
      .map((c) => ({ req, criterion: c })),
  );
  const needApproval = overview.status.scenarios.filter((s) => s.state === "awaiting_approval" || s.pending_change);

  return (
    <div className="stack">
      <div className="row wrap">
        <button onClick={() => act(() => api.verify())} disabled={overview.verifying || scenarios.length === 0}>
          {overview.verifying ? t("app.verifying") : t("sc.run")}
        </button>
        <button onClick={() => act(() => api.startRun("scenarios"))} disabled={authorBusy || requirements.length === 0}>
          {t("sc.write")}
        </button>
        {needApproval.length > 0 ? (
          <button
            className="primary"
            onClick={() =>
              act(() =>
                api.approveScenarios(Object.fromEntries(needApproval.map((s) => [s.scenario, s.definition ?? ""]))),
              )
            }
          >
            {t("sc.approveAll")} ({needApproval.length})
          </button>
        ) : null}
      </div>

      {scenarios.length === 0 ? <p className="muted">{t("sc.empty")}</p> : null}

      {scenarios.map((scenario) => {
        const status = overview.status.scenarios.find((s) => s.scenario === scenario.id);
        const result = overview.results.find((r) => r.scenario === scenario.id);
        const state = status?.state ?? "awaiting_approval";
        return (
          <article key={scenario.id} className={`card scenario ${state}`}>
            <div className="row">
              <span className={`mark ${state}`} title={t(`crit.${state}`)}>
                {CRITERION_MARK[state]}
              </span>
              <strong>{scenario.title}</strong>
              <span className="muted">
                {scenario.target} · {scenario.runner} · {t(`crit.${state}`)}
              </span>
              <span className="spacer" />
              {state === "awaiting_approval" ? (
                <button className="primary" onClick={() => act(() => api.approveScenario(scenario.id, status?.definition ?? ""))}>
                  {t("sc.approve")}
                </button>
              ) : null}
              <button onClick={() => annotate({ kind: "scenario", scenario: scenario.id })}>{t("be.annotate")}</button>
            </div>
            {status?.pending_change ? (
              <div className="warn row">
                <span>{t("sc.pending")}</span>
                <button onClick={() => act(() => api.approveScenario(scenario.id, status?.definition ?? ""))}>{t("sc.approveChange")}</button>
              </div>
            ) : null}
            <dl className="facts">
              <dt>{t("sc.covers")}</dt>
              <dd>
                {scenario.covers.map((cover) => {
                  const req = requirements.find((r) => r.id === cover.requirement);
                  const criterion = req?.criteria.find((c) => c.id === cover.criterion);
                  return (
                    <div key={`${cover.requirement}/${cover.criterion}`}>
                      <code>{cover.requirement}</code> {criterion?.text ?? cover.criterion}
                    </div>
                  );
                })}
              </dd>
              {scenario.files.length > 0 ? (
                <>
                  <dt>{t("sc.files")}</dt>
                  <dd className="mono">{scenario.files.join(", ")}</dd>
                </>
              ) : null}
              {scenario.command ? (
                <>
                  <dt>$</dt>
                  <dd className="mono">{scenario.command}</dd>
                </>
              ) : null}
            </dl>
            {scenario.files.length > 0 ? <ScenarioFiles id={scenario.id} /> : null}
            {result ? (
              <details open={result.outcome !== "passed"}>
                <summary>
                  {t("sc.result")}: {result.outcome} · {new Date(result.at).toLocaleString()} ·{" "}
                  <span className="mono">{result.snapshot.slice(0, 8)}</span>
                  {result.snapshot !== overview.status.snapshot ? ` (${t("crit.stale")})` : ""}
                </summary>
                <pre className="log">{result.detail}</pre>
                {result.artifacts ? (
                  <a href={api.fileUrl(`${result.artifacts}/log.txt`)} target="_blank" rel="noreferrer">
                    {t("sc.log")}
                  </a>
                ) : null}
              </details>
            ) : null}
          </article>
        );
      })}

      {uncovered.length > 0 ? (
        <div className="card">
          <strong>{t("sc.uncovered")}</strong>
          <ul>
            {uncovered.map(({ req, criterion }) => (
              <li key={`${req.id}/${criterion.id}`}>
                <code>{req.id}</code> {criterion.text}
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  );
}
