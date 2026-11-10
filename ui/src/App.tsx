import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { type Anchor, ApiError, type Overview, type PreviewInfo, type StoredEvent, api, subscribe } from "./api";
import { AgentPanel } from "./AgentPanel";
import { AnnotationDialog } from "./AnnotationDialog";
import { App as AppCtx, type TabId } from "./app-context";
import { Checklist } from "./Checklist";
import { I18n, LANGS, type Lang, initialLang, translator } from "./i18n";
import { NextStep } from "./NextStep";
import { ActivityLog } from "./tabs/ActivityLog";
import { Backend } from "./tabs/Backend";
import { Feedback } from "./tabs/Feedback";
import { Frontend } from "./tabs/Frontend";
import { Scenarios } from "./tabs/Scenarios";

const TABS: TabId[] = ["frontend", "backend", "scenarios", "feedback", "activity"];

export function App() {
  const [lang, setLang] = useState<Lang>(initialLang);
  const t = useMemo(() => translator(lang), [lang]);
  const [overview, setOverview] = useState<Overview | null>(null);
  const [previews, setPreviews] = useState<PreviewInfo[]>([]);
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const [tab, setTab] = useState<TabId>("frontend");
  const [error, setError] = useState<string | null>(null);
  const [forbidden, setForbidden] = useState(false);
  const [draft, setDraft] = useState<Anchor | null>(null);

  useEffect(() => {
    document.documentElement.lang = lang === "zh" ? "zh-CN" : lang;
    try {
      localStorage.setItem("ichibou-lang", lang);
    } catch {
      // The choice just will not be remembered.
    }
  }, [lang]);

  // Change notifications arrive in bursts; one reload per burst is enough.
  const pending = useRef<Set<string>>(new Set());
  const timer = useRef<ReturnType<typeof setTimeout>>();
  const reload = useCallback((what: string) => {
    pending.current.add(what);
    clearTimeout(timer.current);
    timer.current = setTimeout(async () => {
      const wanted = pending.current;
      pending.current = new Set();
      const all = wanted.has("resync");
      try {
        if (all || [...wanted].some((w) => w !== "event" && w !== "preview")) {
          setOverview(await api.overview());
        }
        if (all || wanted.has("preview") || wanted.has("status")) setPreviews(await api.previews());
        if (all || wanted.has("event")) setEvents(await api.events());
      } catch (err) {
        if (err instanceof ApiError && err.message === "forbidden") setForbidden(true);
      }
    }, 80);
  }, []);

  useEffect(() => subscribe(reload), [reload]);

  const act = useCallback(async <R,>(work: () => Promise<R>): Promise<R | undefined> => {
    try {
      const result = await work();
      setError(null);
      return result;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return undefined;
    }
  }, []);

  if (forbidden) {
    return <div className="center muted">{t("app.forbidden")}</div>;
  }
  if (!overview) {
    return <div className="center muted">{t("app.loading")}</div>;
  }

  const total = overview.status.requirements.length;
  const done = overview.status.requirements.filter((r) => r.state === "done").length;
  const openFeedback = overview.annotations.filter((a) => a.state !== "resolved").length;
  const projectName = overview.project_root.split("/").filter(Boolean).pop() ?? "";

  return (
    <I18n.Provider value={t}>
      <AppCtx.Provider value={{ overview, previews, events, annotate: setDraft, act, go: setTab }}>
        <div className="shell">
          <header className="topbar">
            <strong className="brand">Ichibou</strong>
            <span className="project" title={overview.project_root}>
              {projectName}
            </span>
            <span className={`pill ${overview.status.complete ? "ok" : ""}`}>
              {overview.verifying
                ? t("app.verifying")
                : overview.status.complete
                  ? t("app.complete")
                  : t("app.incomplete", { done, total })}
            </span>
            <span className="spacer" />
            <label className="inline">
              {t("app.agent")}
              <select
                value={overview.config.default_agent}
                onChange={(e) =>
                  act(() =>
                    api.setConfig({ ...overview.config, default_agent: e.target.value as Overview["config"]["default_agent"] }),
                  ).then(() => reload("status"))
                }
              >
                <option value="claude">Claude Code</option>
                <option value="codex">Codex</option>
                {overview.config.fake_script ? <option value="fake">Fake (script)</option> : null}
              </select>
            </label>
            <label className="inline">
              {t("app.language")}
              <select value={lang} onChange={(e) => setLang(e.target.value as Lang)}>
                {LANGS.map((l) => (
                  <option key={l.id} value={l.id}>
                    {l.label}
                  </option>
                ))}
              </select>
            </label>
          </header>

          {error ? (
            <div className="error" role="alert">
              <span>{error}</span>
              <button className="link" onClick={() => setError(null)}>
                ×
              </button>
            </div>
          ) : null}

          <aside className="sidebar">
            <Checklist />
          </aside>

          <main className="main">
            <NextStep />
            <nav className="tabs" role="tablist">
              {TABS.map((id) => (
                <button
                  key={id}
                  role="tab"
                  aria-selected={tab === id}
                  className={tab === id ? "tab active" : "tab"}
                  onClick={() => setTab(id)}
                >
                  {t(`tab.${id}`)}
                  {id === "feedback" && openFeedback > 0 ? <span className="count">{openFeedback}</span> : null}
                </button>
              ))}
            </nav>
            <section className="tabpanel">
              {tab === "frontend" ? <Frontend /> : null}
              {tab === "backend" ? <Backend /> : null}
              {tab === "scenarios" ? <Scenarios /> : null}
              {tab === "feedback" ? <Feedback /> : null}
              {tab === "activity" ? <ActivityLog /> : null}
            </section>
          </main>

          <aside className="agentbar">
            <AgentPanel />
          </aside>
        </div>
        {draft ? <AnnotationDialog anchor={draft} onClose={() => setDraft(null)} /> : null}
      </AppCtx.Provider>
    </I18n.Provider>
  );
}
