import { useEffect, useRef, useState } from "react";
import { type Anchor, api } from "./api";
import { anchorLabel, useApp } from "./app-context";
import { useT } from "./i18n";

/** Guesses the intent from the text so the common case needs no click. */
function looksLikeQuestion(text: string): boolean {
  return /[?？]\s*$/.test(text.trim()) || /^(why|what|how|is|are|does|do|can|なぜ|どう|何|为什么|怎么|是否)/i.test(text.trim());
}

export function AnnotationDialog({ anchor, onClose }: { anchor: Anchor; onClose: () => void }) {
  const t = useT();
  const { act, go } = useApp();
  const [body, setBody] = useState("");
  const [intent, setIntent] = useState<"fix" | "question" | null>(null);
  const ref = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const chosen = intent ?? (looksLikeQuestion(body) ? "question" : "fix");

  useEffect(() => {
    ref.current?.showModal();
    // showModal focuses the first control (a radio); typing should go to the text.
    input.current?.focus();
  }, []);

  return (
    <dialog ref={ref} className="dialog" onClose={onClose}>
      <form
        className="stack"
        onSubmit={(e) => {
          e.preventDefault();
          if (!body.trim()) return;
          act(() => api.annotate({ intent: chosen, anchor, body })).then((saved) => {
            if (saved) {
              onClose();
              go("feedback");
            }
          });
        }}
      >
        <div className="muted">
          {t("fb.on")}: <span className="mono">{anchorLabel(anchor)}</span>
        </div>
        <div className="row" role="radiogroup">
          {(["fix", "question"] as const).map((kind) => (
            <label key={kind} className={chosen === kind ? "choice active" : "choice"}>
              <input type="radio" name="intent" checked={chosen === kind} onChange={() => setIntent(kind)} />
              {t(`fb.${kind}`)}
            </label>
          ))}
        </div>
        <textarea
          ref={input}
          rows={4}
          value={body}
          placeholder={t(`fb.placeholder.${chosen}`)}
          onChange={(e) => setBody(e.target.value)}
        />
        <div className="row end">
          <button type="button" onClick={onClose}>
            {t("fb.cancel")}
          </button>
          <button className="primary" disabled={!body.trim()}>
            {t("fb.send")}
          </button>
        </div>
      </form>
    </dialog>
  );
}
