import { useEffect, useState } from "react";
import { type ScenarioFiles as Files, type ShownFile, api } from "../api";
import { useApp } from "../app-context";
import { useT } from "../i18n";

type Line = { kind: "same" | "add" | "del"; text: string };

/** Line diff by longest common subsequence. Fine for test-file sizes. */
/** Largest line-count product the diff table is built for (about 32 MB). */
const MAX_DIFF_CELLS = 4_000_000;

/**
 * Line diff by longest common subsequence. Returns null when the inputs are
 * too large to diff this way; the size is checked before anything is
 * allocated.
 */
export function diffLines(before: string, after: string): Line[] | null {
  const a = before.split("\n");
  const b = after.split("\n");
  if ((a.length + 1) * (b.length + 1) > MAX_DIFF_CELLS) return null;
  // lcs[i][j] = length of the LCS of a[i..] and b[j..]
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i]![j] = a[i] === b[j] ? lcs[i + 1]![j + 1]! + 1 : Math.max(lcs[i + 1]![j]!, lcs[i]![j + 1]!);
    }
  }
  const out: Line[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ kind: "same", text: a[i]! });
      i++;
      j++;
    } else if (lcs[i + 1]![j]! >= lcs[i]![j + 1]!) {
      out.push({ kind: "del", text: a[i++]! });
    } else {
      out.push({ kind: "add", text: b[j++]! });
    }
  }
  while (i < a.length) out.push({ kind: "del", text: a[i++]! });
  while (j < b.length) out.push({ kind: "add", text: b[j++]! });
  return out;
}


function Content({ file }: { file: ShownFile }) {
  const t = useT();
  const describe = (side: ShownFile["current"]) => {
    if (side === null) return <p className="muted">{t("sc.file.missing")}</p>;
    if ("too_large" in side) {
      return <p className="muted">{t("sc.file.large", { n: Math.round(side.too_large / 1024) })}</p>;
    }
    return <pre className="log code">{side.text}</pre>;
  };

  const { current, approved } = file;
  const bothText = current !== null && approved !== null && "text" in current && "text" in approved;
  if (file.changed && bothText) {
    const lines = diffLines(approved.text, current.text);
    if (lines) {
      return (
        <pre className="log code diff">
          {lines.map((line, index) => (
            <span key={index} className={line.kind}>
              {line.kind === "add" ? "+ " : line.kind === "del" ? "- " : "  "}
              {/* Each line is a block; an empty one still needs height. */}
              {line.text || " "}
            </span>
          ))}
        </pre>
      );
    }
  }
  if (file.changed) {
    return (
      <div className="stack">
        <span className="muted">{t("sc.diff.approved")}</span>
        {describe(approved)}
        <span className="muted">{t("sc.diff.current")}</span>
        {describe(current)}
      </div>
    );
  }
  return describe(current);
}

/** A scenario's test files, with what changed since the user approved them. */
export function ScenarioFiles({ id }: { id: string }) {
  const t = useT();
  const { overview, act } = useApp();
  const [open, setOpen] = useState(false);
  const [data, setData] = useState<Files | null>(null);
  const snapshot = overview.status.snapshot;
  const approvedAt = overview.status.scenarios.find((s) => s.scenario === id)?.state;

  useEffect(() => {
    if (!open) return;
    let alive = true;
    api
      .scenarioFiles(id)
      .then((files) => alive && setData(files))
      .catch(() => alive && setData(null));
    return () => {
      alive = false;
    };
    // Reload when files change or the approval state moves.
  }, [open, id, snapshot, approvedAt]);

  const changed = data ? data.entry_changed || data.files.some((f) => f.changed) : false;
  return (
    <details onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary>{t("sc.view")}</summary>
      {data ? (
        <div className="stack">
          {data.entry_changed ? <p className="warn">{t("sc.entryChanged")}</p> : null}
          {data.files.map((file) => (
            <div key={file.path} className="stack">
              <div className="row">
                <span className="mono">{file.path}</span>
                {file.changed ? <span className="tag fix">{t("sc.changed")}</span> : null}
              </div>
              <Content file={file} />
            </div>
          ))}
          {!data.approved || changed ? (
            <div>
              {/* `definition` is the hash of exactly the content shown above. */}
              <button className="primary" onClick={() => act(() => api.approveScenario(id, data.definition))}>
                {t("sc.approveShown")}
              </button>
            </div>
          ) : null}
        </div>
      ) : null}
    </details>
  );
}
