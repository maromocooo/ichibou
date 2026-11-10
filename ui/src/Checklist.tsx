import { useState } from "react";
import { type CriterionState, type RequirementState, type RequirementStatus, api } from "./api";
import { useApp } from "./app-context";
import { type T, useT } from "./i18n";

const MARK: Record<RequirementState, string> = {
  not_started: "○",
  in_progress: "◐",
  needs_verification: "◌",
  failing: "✕",
  awaiting_manual: "◑",
  blocked_by_annotations: "◑",
  done: "✓",
};

/** States in which the requirement is waiting for the agent to get to it. */
const WAITING: RequirementState[] = ["not_started", "needs_verification", "failing"];

export const CRITERION_MARK: Record<CriterionState, string> = {
  no_scenario: "–",
  awaiting_approval: "?",
  not_run: "○",
  stale: "◌",
  passed: "✓",
  failed: "✕",
  error: "!",
  aborted: "!",
  skipped: "–",
};

/** "When will this be worked on": order and dependencies, not clock time. */
export function when(status: RequirementStatus, t: T): string | null {
  const { kind, position, waiting_on } = status.schedule;
  switch (kind) {
    case "done":
      return null;
    case "now":
      return t("when.now");
    case "next":
      return t("when.next");
    case "queued":
      return t("when.queued", { n: position - 1 });
    case "blocked":
      return t("when.blocked", { ids: waiting_on.join(", ") });
  }
}

export function Checklist() {
  const t = useT();
  const { overview, annotate, act } = useApp();
  const [open, setOpen] = useState<string | null>(null);
  const requirements = overview.manifest.requirements.requirements;

  if (requirements.length === 0) {
    return (
      <>
        <h2>{t("req.title")}</h2>
        <p className="muted">{t("req.empty")}</p>
      </>
    );
  }

  return (
    <>
      <h2>{t("req.title")}</h2>
      <ul className="checklist">
        {requirements.map((req) => {
          const status = overview.status.requirements.find((s) => s.requirement === req.id);
          if (!status) return null;
          const expanded = open === req.id;
          const hint = when(status, t);
          return (
            <li key={req.id} className={`req ${status.state}`}>
              <button className="req-head" aria-expanded={expanded} onClick={() => setOpen(expanded ? null : req.id)}>
                <span className={`mark ${status.state}`} aria-hidden>
                  {MARK[status.state]}
                </span>
                <span className="req-text">
                  <span className="req-title">{req.title}</span>
                  <span className="req-meta">
                    <code>{req.id}</code> · {t(`state.${status.state}`)}
                    {hint && WAITING.includes(status.state) ? ` · ${hint}` : ""}
                  </span>
                </span>
              </button>
              {expanded ? (
                <div className="req-body">
                  {req.description ? <p>{req.description}</p> : null}
                  {req.depends_on.length > 0 ? (
                    <p className="muted">{t("req.depends", { ids: req.depends_on.join(", ") })}</p>
                  ) : null}
                  <h4>{t("req.criteria")}</h4>
                  <ul className="plain">
                    {req.criteria.map((criterion) => {
                      const state = status.criteria.find((c) => c.criterion === criterion.id)?.state ?? "no_scenario";
                      return (
                        <li key={criterion.id} className="criterion">
                          <span className={`mark small ${state}`} title={t(`crit.${state}`)}>
                            {CRITERION_MARK[state]}
                          </span>
                          <span>
                            {criterion.text} <span className="muted">— {t(`crit.${state}`)}</span>
                          </span>
                        </li>
                      );
                    })}
                  </ul>
                  {req.manual_checks.length > 0 ? (
                    <>
                      <h4>{t("req.manual")}</h4>
                      <ul className="plain">
                        {req.manual_checks.map((check) => {
                          const manual = status.manual.find((m) => m.check === check.id);
                          return (
                            <li key={check.id}>
                              <label className="check">
                                <input
                                  type="checkbox"
                                  checked={manual?.confirmed ?? false}
                                  onChange={(e) => act(() => api.manual(req.id, check.id, e.target.checked))}
                                />
                                <span>
                                  {check.text}
                                  {manual?.stale ? <span className="muted"> — {t("req.manual.stale")}</span> : null}
                                </span>
                              </label>
                            </li>
                          );
                        })}
                      </ul>
                    </>
                  ) : null}
                  {status.open_fixes.length > 0 ? (
                    <p className="warn">{t("req.fixes", { n: status.open_fixes.length })}</p>
                  ) : null}
                  <button className="link" onClick={() => annotate({ kind: "requirement", requirement: req.id })}>
                    {t("req.annotate")}
                  </button>
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
    </>
  );
}
