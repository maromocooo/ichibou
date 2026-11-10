// Types mirror the daemon's JSON (crates/ichibou-core/src/model.rs and
// status.rs). Keep them in step when the Rust side changes.

export interface Criterion {
  id: string;
  text: string;
}

export interface Requirement {
  id: string;
  title: string;
  description: string;
  depends_on: string[];
  criteria: Criterion[];
  manual_checks: Criterion[];
}

export type NodeKind = "client" | "api" | "service" | "db" | "queue" | "external";

export interface ArchNode {
  id: string;
  kind: NodeKind;
  title: string;
  description: string;
  paths: string[];
  requirements: string[];
}

export interface ArchEdge {
  from: string;
  to: string;
  label: string;
  requirements: string[];
}

export type TargetKind = "web" | "ios" | "android" | "desktop";

export interface Launch {
  command: string;
  cwd?: string | null;
  url?: string | null;
  port?: number | null;
}

export interface Target {
  name: string;
  kind: TargetKind;
  dev?: Launch | null;
  verify?: { setup: string[]; start?: Launch | null; copy: string[]; link: string[] } | null;
  app_id?: string | null;
  /** False when the target's commands run without the write guard. */
  guard?: boolean;
}

export interface ApiSource {
  node: string;
  openapi_file?: string | null;
  openapi_url?: string | null;
  openapi_command?: string | null;
  base_url?: string | null;
}

export interface DatabaseSource {
  node: string;
  kind: "postgres" | "sqlite";
  url: string;
}

export interface TargetsDoc {
  targets: Target[];
  apis: ApiSource[];
  databases: DatabaseSource[];
}

export interface Scenario {
  id: string;
  title: string;
  target: string;
  runner: "playwright" | "maestro" | "command";
  covers: { requirement: string; criterion: string }[];
  files: string[];
  command?: string | null;
}

export interface Manifest {
  requirements: { requirements: Requirement[] };
  architecture: { nodes: ArchNode[]; edges: ArchEdge[] };
  targets: TargetsDoc;
  scenarios: { scenarios: Scenario[] };
}

export type CriterionState =
  | "no_scenario"
  | "awaiting_approval"
  | "not_run"
  | "stale"
  | "passed"
  | "failed"
  | "error"
  | "aborted"
  | "skipped";

export type RequirementState =
  | "not_started"
  | "in_progress"
  | "needs_verification"
  | "failing"
  | "awaiting_manual"
  | "blocked_by_annotations"
  | "done";

export type ScheduleKind = "now" | "next" | "queued" | "blocked" | "done";

export interface RequirementStatus {
  requirement: string;
  state: RequirementState;
  criteria: { criterion: string; state: CriterionState; scenarios: string[] }[];
  manual: { check: string; confirmed: boolean; stale: boolean }[];
  open_fixes: string[];
  claimed_by: string | null;
  schedule: { kind: ScheduleKind; position: number; waiting_on: string[] };
}

export interface ScenarioStatus {
  scenario: string;
  state: CriterionState;
  pending_change: boolean;
  last_result: string | null;
  definition: string | null;
}

export type Provenance = "declared" | "present" | "verified";

export interface ProjectStatus {
  snapshot: string;
  requirements: RequirementStatus[];
  scenarios: ScenarioStatus[];
  architecture: { nodes: Record<string, Provenance>; edges: Record<string, Provenance> };
  complete: boolean;
}

export interface VerificationResult {
  id: string;
  scenario: string;
  outcome: "passed" | "failed" | "error" | "aborted" | "skipped" | "not_applicable";
  snapshot: string;
  definition: string;
  at: string;
  detail: string;
  artifacts: string | null;
}

export type AgentKind = "claude" | "codex" | "fake";
export type Activity =
  | "idle"
  | "working"
  | "running_tool"
  | "waiting_for_input"
  | "rate_limited"
  | "interrupted"
  | "finished"
  | "failed";
export type RunRole = "writer" | "responder" | "scenario_author" | "onboarding";

export interface Run {
  id: string;
  agent: AgentKind;
  role: RunRole;
  activity: Activity;
  session: string | null;
  auth?: string | null;
  model?: string | null;
  last_message: string;
  started_at: string;
  updated_at: string;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type Anchor =
  | { kind: "ui_element"; target: string; url: string; selector: string; text: string; source: string | null; rect: Rect }
  | { kind: "ui_region"; target: string; url: string | null; rect: Rect }
  | { kind: "arch_node"; node: string }
  | { kind: "arch_edge"; from: string; to: string }
  | { kind: "api_operation"; node: string; operation: string }
  | { kind: "db_table"; node: string; table: string; column: string | null }
  | { kind: "requirement"; requirement: string }
  | { kind: "scenario"; scenario: string };

export type AnnotationState = "queued" | "sent" | "acknowledged" | "addressed" | "resolved" | "anchor_lost";

export interface Annotation {
  id: string;
  intent: "fix" | "question";
  state: AnnotationState;
  anchor: Anchor;
  body: string;
  snapshot: string;
  requirements: string[];
  image: string | null;
  created_at: string;
  comments: { id: string; author: "user" | "agent"; body: string; at: string }[];
}

export interface Config {
  default_agent: AgentKind;
  claude_permission_mode: string;
  fake_script?: string | null;
  auto_verify: boolean;
  claude_model?: string | null;
  claude_effort?: string | null;
  codex_model?: string | null;
  codex_effort?: string | null;
}

export interface ModelInfo {
  id: string;
  name: string;
  efforts: string[];
  default_effort?: string | null;
  is_default: boolean;
}

export interface AgentOptions {
  models: ModelInfo[];
  /** The agent accepts model names beyond the listed ones. */
  free_text: boolean;
  error?: string | null;
}

export interface Overview {
  project_root: string;
  manifest: Manifest;
  status: ProjectStatus;
  results: VerificationResult[];
  runs: Run[];
  live_runs: string[];
  annotations: Annotation[];
  proposals: {
    requirements: { requirements: Requirement[] } | null;
    architecture: { nodes: ArchNode[]; edges: ArchEdge[] } | null;
    targets: TargetsDoc | null;
  };
  proposal_hashes: { requirements: string | null; architecture: string | null; targets: string | null };
  approvals: { run: string; id: string; kind: string; detail: string }[];
  verifying: boolean;
  config: Config;
  targets_approved: boolean;
  requirements_approved: boolean;
  targets_hash: string;
  requirements_hash: string;
}

export interface StoredEvent {
  seq: number;
  at: string;
  run: string | null;
  kind: string;
  payload: Record<string, unknown>;
}

export interface PreviewInfo {
  target: string;
  kind: TargetKind;
  running: boolean;
  url: string | null;
  upstream: string | null;
  stream: boolean;
  video: boolean;
  log: string;
}

export type ShownText = null | { text: string } | { too_large: number };

export interface ShownFile {
  path: string;
  current: ShownText;
  approved: ShownText;
  executable: boolean;
  changed: boolean;
}

export interface ScenarioFiles {
  /** Hash of exactly the current content in `files`; send it to approve. */
  definition: string;
  approved: boolean;
  entry_changed: boolean;
  files: ShownFile[];
}

export interface DbTable {
  name: string;
  columns: { name: string; type: string; nullable: boolean; primary_key: boolean }[];
  foreign_keys: { column: string; references_table: string; references_column: string }[];
}

// The daemon puts the access token in the URL it opens. Keep it for the
// session and take it out of the address bar.
function readToken(): string {
  const params = new URLSearchParams(location.search);
  const fromUrl = params.get("token");
  if (fromUrl) {
    try {
      sessionStorage.setItem("ichibou-token", fromUrl);
    } catch {
      // Storage can be unavailable; the token still works for this page load.
    }
    params.delete("token");
    const rest = params.toString();
    history.replaceState(null, "", location.pathname + (rest ? `?${rest}` : ""));
    return fromUrl;
  }
  try {
    return sessionStorage.getItem("ichibou-token") ?? "";
  } catch {
    return "";
  }
}

export const token = readToken();

export class ApiError extends Error {}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`/api${path}`, {
    method,
    headers: { "x-ichibou-token": token, ...(body === undefined ? {} : { "content-type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (response.status === 403) {
    throw new ApiError("forbidden");
  }
  const data = (await response.json().catch(() => ({}))) as { error?: string };
  if (!response.ok) {
    throw new ApiError(data.error ?? `HTTP ${response.status}`);
  }
  return data as T;
}

export const api = {
  overview: () => call<Overview>("GET", "/overview"),
  events: () => call<StoredEvent[]>("GET", "/events"),
  previews: () => call<PreviewInfo[]>("GET", "/previews"),
  setConfig: (config: Config) => call("POST", "/config", config),
  agents: () => call<Record<string, AgentOptions>>("GET", "/agents"),
  startRun: (kind: "plan" | "scenarios" | "build" | "onboard", text?: string, agent?: AgentKind) =>
    call<Run>("POST", "/runs", { kind, text, agent }),
  resumeRun: () => call("POST", "/runs/resume", {}),
  message: (run: string, text: string) => call("POST", `/runs/${run}/message`, { text }),
  interrupt: (run: string) => call("POST", `/runs/${run}/interrupt`, {}),
  stop: (run: string) => call("POST", `/runs/${run}/stop`, {}),
  approval: (run: string, id: string, allow: boolean) =>
    call("POST", `/runs/${run}/approvals/${encodeURIComponent(id)}`, { allow }),
  annotate: (input: { intent: "fix" | "question"; anchor: Anchor; body: string; image_base64?: string }) =>
    call<Annotation>("POST", "/annotations", input),
  comment: (id: string, text: string) => call("POST", `/annotations/${id}/comment`, { text }),
  resolve: (id: string) => call("POST", `/annotations/${id}/resolve`, {}),
  reopen: (id: string, text?: string) => call("POST", `/annotations/${id}/reopen`, { text }),
  // Hashes are the scenario definitions that were on screen (from the overview).
  approveScenario: (id: string, hash: string) =>
    call("POST", `/scenarios/${encodeURIComponent(id)}/approve`, { hash }),
  scenarioFiles: (id: string) => call<ScenarioFiles>("GET", `/scenarios/${encodeURIComponent(id)}/files`),
  approveScenarios: (hashes: Record<string, string>) => call("POST", "/scenarios/approve-all", { hashes }),
  verify: () => call("POST", "/verify", {}),
  manual: (requirement: string, check: string, confirmed: boolean) =>
    call("POST", "/manual", { requirement, check, confirmed }),
  // `hash` is the hash of the proposal that was on screen (from the overview).
  decideProposal: (kind: "requirements" | "architecture" | "targets", accept: boolean, hash: string) =>
    call("POST", `/proposals/${kind}`, { accept, hash }),
  // `hash` is the hash of the content that was on screen (from the overview).
  approveTargets: (hash: string) => call("POST", "/targets/approve", { hash }),
  approveRequirements: (hash: string) => call("POST", "/requirements/approve", { hash }),
  previewStart: (target: string) => call("POST", `/previews/${encodeURIComponent(target)}/start`, {}),
  previewStop: (target: string) => call("POST", `/previews/${encodeURIComponent(target)}/stop`, {}),
  previewTap: (target: string, x: number, y: number) =>
    call("POST", `/previews/${encodeURIComponent(target)}/tap`, { x, y }),
  previewText: (target: string, text: string) =>
    call("POST", `/previews/${encodeURIComponent(target)}/text`, { text }),
  frameUrl: (target: string, tick: number) =>
    `/api/previews/${encodeURIComponent(target)}/frame?token=${token}&t=${tick}`,
  videoUrl: (target: string) =>
    `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/previews/${encodeURIComponent(target)}/video?token=${token}`,
  streamUrl: (target: string) => `/api/previews/${encodeURIComponent(target)}/stream?token=${token}`,
  fileUrl: (path: string) => `/api/files/${path}?token=${token}`,
  assets: () => call<{ path: string; size: number }[]>("GET", "/assets"),
  assetUrl: (path: string) => `/api/asset?path=${encodeURIComponent(path)}&token=${token}`,
  openapi: (node: string) =>
    call<{ spec: Record<string, unknown>; base_url: string | null }>("GET", `/inspect/openapi/${encodeURIComponent(node)}`),
  tryRequest: (node: string, request: { method: string; path: string; headers: [string, string][]; body?: string }) =>
    call<{ status: number; headers: [string, string][]; body: string; truncated: boolean; duration_ms: number }>(
      "POST",
      `/inspect/request/${encodeURIComponent(node)}`,
      request,
    ),
  database: (node: string) =>
    call<{ kind: string; tables: DbTable[] }>("GET", `/inspect/db/${encodeURIComponent(node)}`),
};

/** Subscribes to daemon change notifications. Reconnects on its own. */
export function subscribe(onChange: (type: string) => void): () => void {
  let socket: WebSocket | null = null;
  let closed = false;
  let retry: ReturnType<typeof setTimeout> | undefined;
  const connect = () => {
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    socket = new WebSocket(`${scheme}://${location.host}/api/ws?token=${token}`);
    socket.onmessage = (message) => {
      try {
        onChange((JSON.parse(String(message.data)) as { type: string }).type);
      } catch {
        onChange("resync");
      }
    };
    socket.onopen = () => onChange("resync");
    socket.onclose = () => {
      if (!closed) retry = setTimeout(connect, 1000);
    };
  };
  connect();
  return () => {
    closed = true;
    clearTimeout(retry);
    socket?.close();
  };
}
