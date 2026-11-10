import { useState } from "react";
import { type Annotation, api } from "../api";
import { anchorLabel, useApp } from "../app-context";
import { useT } from "../i18n";

// What needs the user first, then what is in flight, then what is finished.
const ORDER: Annotation["state"][] = ["addressed", "anchor_lost", "acknowledged", "sent", "queued", "resolved"];

export function Feedback() {
  const t = useT();
  const { overview } = useApp();
  const annotations = [...overview.annotations].sort(
    (a, b) => ORDER.indexOf(a.state) - ORDER.indexOf(b.state) || b.created_at.localeCompare(a.created_at),
  );
  if (annotations.length === 0) return <p className="muted">{t("fb.empty")}</p>;
  return (
    <div className="stack">
      {annotations.map((annotation) => (
        <Thread key={annotation.id} annotation={annotation} />
      ))}
    </div>
  );
}

function Thread({ annotation }: { annotation: Annotation }) {
  const t = useT();
  const { act } = useApp();
  const [reply, setReply] = useState("");
  const { id, state } = annotation;
  return (
    <article className={`card thread ${state}`}>
      <div className="row">
        <span className={`tag ${annotation.intent}`}>{t(`fb.${annotation.intent}`)}</span>
        <span className="mono clip">{anchorLabel(annotation.anchor)}</span>
        <span className="spacer" />
        <span className={state === "addressed" ? "warn" : "muted"}>{t(`fb.state.${state}`)}</span>
      </div>
      {annotation.anchor.kind === "ui_element" && annotation.anchor.source ? (
        <div className="mono muted">{annotation.anchor.source}</div>
      ) : null}
      <p className="bubble user">{annotation.body}</p>
      {annotation.comments.map((comment) => (
        <p key={comment.id} className={`bubble ${comment.author}`}>
          <span className="muted">{comment.author === "agent" ? t("fb.agent") : t("fb.you")}: </span>
          {comment.body}
        </p>
      ))}
      {state !== "resolved" ? (
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            if (reply.trim()) act(() => api.comment(id, reply)).then(() => setReply(""));
          }}
        >
          <input
            value={reply}
            placeholder={t("fb.reply")}
            aria-label={t("fb.reply")}
            onChange={(e) => setReply(e.target.value)}
          />
          <button disabled={!reply.trim()}>{t("agent.send")}</button>
          <button type="button" className="primary" onClick={() => act(() => api.resolve(id))}>
            {t("fb.resolve")}
          </button>
          {state === "addressed" && annotation.intent === "fix" ? (
            <button type="button" onClick={() => act(() => api.reopen(id, reply || undefined)).then(() => setReply(""))}>
              {t("fb.reopen")}
            </button>
          ) : null}
        </form>
      ) : null}
    </article>
  );
}
