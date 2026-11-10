import { createContext, useContext } from "react";
import type { Anchor, Overview, PreviewInfo, StoredEvent } from "./api";

export interface AppContext {
  overview: Overview;
  previews: PreviewInfo[];
  events: StoredEvent[];
  /** Opens the dialog to attach a fix request or question to something. */
  annotate: (anchor: Anchor) => void;
  /** Runs an API call and shows its error, if any, instead of throwing. */
  act: <R>(work: () => Promise<R>) => Promise<R | undefined>;
  /** Switches the main tab (used by "next step" shortcuts). */
  go: (tab: TabId) => void;
}

export type TabId = "frontend" | "backend" | "scenarios" | "feedback" | "activity";

export const App = createContext<AppContext | null>(null);

export function useApp(): AppContext {
  const value = useContext(App);
  if (!value) throw new Error("useApp outside the app");
  return value;
}

/** A short human-readable name for what an annotation is attached to. */
export function anchorLabel(anchor: Anchor): string {
  switch (anchor.kind) {
    case "ui_element":
      return `${anchor.target} ${anchor.url} — ${anchor.text || anchor.selector}`;
    case "ui_region":
      return `${anchor.target}${anchor.url ? ` ${anchor.url}` : ""} — region`;
    case "arch_node":
      return anchor.node;
    case "arch_edge":
      return `${anchor.from} → ${anchor.to}`;
    case "api_operation":
      return `${anchor.node}: ${anchor.operation}`;
    case "db_table":
      return `${anchor.node}: ${anchor.table}${anchor.column ? `.${anchor.column}` : ""}`;
    case "requirement":
      return anchor.requirement;
    case "scenario":
      return anchor.scenario;
  }
}
