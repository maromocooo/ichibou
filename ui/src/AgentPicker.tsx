import { useEffect, useState } from "react";
import { type AgentKind, type AgentOptions, type Config, api } from "./api";
import { useApp } from "./app-context";
import { useT } from "./i18n";

/** Which agent runs new sessions, with which model and reasoning effort. */
export function AgentPicker({ onChanged }: { onChanged: () => void }) {
  const t = useT();
  const { overview, act } = useApp();
  const config = overview.config;
  const agent = config.default_agent;
  const [options, setOptions] = useState<Record<string, AgentOptions> | null>(null);

  useEffect(() => {
    let alive = true;
    api
      .agents()
      .then((result) => alive && setOptions(result))
      .catch(() => alive && setOptions(null));
    return () => {
      alive = false;
    };
  }, []);

  const save = (change: Partial<Config>) => act(() => api.setConfig({ ...config, ...change })).then(onChanged);

  if (agent === "fake") {
    return <AgentSelect agent={agent} config={config} save={save} />;
  }
  const modelKey = agent === "claude" ? "claude_model" : "codex_model";
  const effortKey = agent === "claude" ? "claude_effort" : "codex_effort";
  const model = config[modelKey] ?? "";
  const effort = config[effortKey] ?? "";
  const mine = options?.[agent];
  const models = mine?.models ?? [];
  // Efforts of the chosen model; with no model chosen, of the agent's default.
  const current = models.find((m) => m.id === model) ?? models.find((m) => m.is_default) ?? models[0];
  const efforts = current?.efforts ?? [];
  const listed = models.some((m) => m.id === model);

  return (
    <>
      <AgentSelect agent={agent} config={config} save={save} />
      <label className="inline">
        {t("app.model")}
        {mine && !mine.free_text ? (
          <select
            value={model}
            onChange={(e) => {
              const next = models.find((m) => m.id === e.target.value);
              // An effort the new model does not offer is dropped.
              const keep = !effort || !next || next.efforts.includes(effort);
              save({ [modelKey]: e.target.value || null, ...(keep ? {} : { [effortKey]: null }) });
            }}
          >
            <option value="">{t("app.default")}</option>
            {!listed && model ? <option value={model}>{model}</option> : null}
            {models.map((m) => (
              <option key={m.id} value={m.id}>
                {m.name}
              </option>
            ))}
          </select>
        ) : (
          <ModelInput
            key={`${agent}:${model}`}
            initial={model}
            suggestions={models.map((m) => m.id)}
            placeholder={t("app.default")}
            title={mine?.error ?? undefined}
            onCommit={(value) => save({ [modelKey]: value || null })}
          />
        )}
      </label>
      <label className="inline">
        {t("app.effort")}
        <select value={effort} onChange={(e) => save({ [effortKey]: e.target.value || null })}>
          <option value="">
            {current?.default_effort ? `${t("app.default")} (${current.default_effort})` : t("app.default")}
          </option>
          {effort && !efforts.includes(effort) ? <option value={effort}>{effort}</option> : null}
          {efforts.map((level) => (
            <option key={level} value={level}>
              {level}
            </option>
          ))}
        </select>
      </label>
    </>
  );
}

function AgentSelect({
  agent,
  config,
  save,
}: {
  agent: AgentKind;
  config: Config;
  save: (change: Partial<Config>) => void;
}) {
  const t = useT();
  return (
    <label className="inline">
      {t("app.agent")}
      <select value={agent} onChange={(e) => save({ default_agent: e.target.value as AgentKind })}>
        <option value="claude">Claude Code</option>
        <option value="codex">Codex</option>
        {config.fake_script ? <option value="fake">Fake (script)</option> : null}
      </select>
    </label>
  );
}

/** A model name typed by hand, saved when the field is left or Enter is pressed. */
function ModelInput({
  initial,
  suggestions,
  placeholder,
  title,
  onCommit,
}: {
  initial: string;
  suggestions: string[];
  placeholder: string;
  title?: string;
  onCommit: (value: string) => void;
}) {
  const [value, setValue] = useState(initial);
  const commit = () => {
    if (value.trim() !== initial) onCommit(value.trim());
  };
  return (
    <>
      <input
        className="model"
        list="ichibou-models"
        value={value}
        placeholder={placeholder}
        title={title}
        onChange={(e) => setValue(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
        }}
      />
      <datalist id="ichibou-models">
        {suggestions.map((id) => (
          <option key={id} value={id} />
        ))}
      </datalist>
    </>
  );
}
