import { Background, type Edge, Handle, type Node, type NodeProps, Position, ReactFlow } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useEffect, useMemo, useState } from "react";
import { type ArchNode, type DbTable, type NodeKind, type Provenance, api } from "../api";
import { useApp } from "../app-context";
import { useT } from "../i18n";

// Left to right: who calls whom.
const COLUMN: Record<NodeKind, number> = { client: 0, api: 1, service: 2, queue: 2, db: 3, external: 3 };

type Data = { node: ArchNode; provenance: Provenance; selected: boolean; feedback: number };

function ArchBox({ data }: NodeProps<Node<Data>>) {
  const t = useT();
  return (
    <div className={`arch ${data.provenance} ${data.selected ? "selected" : ""}`}>
      <Handle type="target" position={Position.Left} />
      <div className="arch-kind">
        {data.node.kind}
        <span className="arch-state">{t(`be.legend.${data.provenance}`)}</span>
      </div>
      <div className="arch-title">
        {data.provenance === "verified" ? "✓ " : ""}
        {data.node.title}
        {data.feedback > 0 ? <span className="count">{data.feedback}</span> : null}
      </div>
      <Handle type="source" position={Position.Right} />
    </div>
  );
}

const nodeTypes = { arch: ArchBox };

export function Backend() {
  const t = useT();
  const { overview } = useApp();
  const [selected, setSelected] = useState<string | null>(null);
  const { nodes: archNodes, edges: archEdges } = overview.manifest.architecture;
  const provenance = overview.status.architecture;

  const nodes = useMemo<Node<Data>[]>(() => {
    const rows: Record<number, number> = {};
    // Only columns that hold a node take up space.
    const used = [...new Set(archNodes.map((n) => COLUMN[n.kind]))].sort((a, b) => a - b);
    return archNodes.map((node) => {
      const column = used.indexOf(COLUMN[node.kind]);
      const row = rows[column] ?? 0;
      rows[column] = row + 1;
      const feedback = overview.annotations.filter(
        (a) => a.state !== "resolved" && "node" in a.anchor && a.anchor.node === node.id,
      ).length;
      return {
        id: node.id,
        type: "arch",
        position: { x: column * 230, y: row * 100 },
        data: {
          node,
          provenance: provenance.nodes[node.id] ?? "declared",
          selected: selected === node.id,
          feedback,
        },
      };
    });
  }, [archNodes, overview.annotations, provenance.nodes, selected]);

  const edges = useMemo<Edge[]>(
    () =>
      archEdges.map((edge) => {
        const state = provenance.edges[`${edge.from}->${edge.to}`] ?? "declared";
        return {
          id: `${edge.from}->${edge.to}`,
          source: edge.from,
          target: edge.to,
          label: edge.label || undefined,
          animated: false,
          className: `edge ${state}`,
        };
      }),
    [archEdges, provenance.edges],
  );

  if (archNodes.length === 0) return <p className="muted">{t("be.empty")}</p>;
  const current = archNodes.find((n) => n.id === selected);

  return (
    <div className="backend">
      <div className="diagram">
        <div className="legend">
          {(["declared", "present", "verified"] as const).map((state) => (
            <span key={state} className={`legend-item ${state}`}>
              {t(`be.legend.${state}`)}
            </span>
          ))}
        </div>
        <ReactFlow
          nodes={nodes}
          edges={edges}
          nodeTypes={nodeTypes}
          fitView
          fitViewOptions={{ padding: 0.12, maxZoom: 1.25 }}
          nodesConnectable={false}
          nodesDraggable={false}
          proOptions={{ hideAttribution: true }}
          onNodeClick={(_, node) => setSelected(node.id)}
          onPaneClick={() => setSelected(null)}
        >
          <Background />
        </ReactFlow>
      </div>
      <div className="inspector">
        {current ? <Inspector node={current} /> : <p className="muted">{t("be.select")}</p>}
      </div>
    </div>
  );
}

function Inspector({ node }: { node: ArchNode }) {
  const t = useT();
  const { overview, annotate } = useApp();
  const hasApi = overview.manifest.targets.apis.some((a) => a.node === node.id);
  const hasDb = overview.manifest.targets.databases.some((d) => d.node === node.id);
  return (
    <div className="stack">
      <div className="row">
        <h3>{node.title}</h3>
        <span className="spacer" />
        <button onClick={() => annotate({ kind: "arch_node", node: node.id })}>{t("be.annotate")}</button>
      </div>
      {node.description ? <p>{node.description}</p> : null}
      {node.paths.length > 0 ? (
        <p>
          <span className="muted">{t("be.paths")}: </span>
          <span className="mono">{node.paths.join(", ")}</span>
        </p>
      ) : null}
      {node.requirements.length > 0 ? (
        <div>
          <span className="muted">{t("be.requirements")}</span>
          <ul className="plain">
            {node.requirements.map((id) => {
              const status = overview.status.requirements.find((r) => r.requirement === id);
              return (
                <li key={id}>
                  <code>{id}</code> {status ? t(`state.${status.state}`) : ""}
                </li>
              );
            })}
          </ul>
        </div>
      ) : null}
      {hasApi ? <ApiView node={node.id} /> : null}
      {hasDb ? <DbView node={node.id} /> : null}
    </div>
  );
}

interface Operation {
  id: string;
  method: string;
  path: string;
  summary: string;
  hasBody: boolean;
}

const METHODS = ["get", "post", "put", "patch", "delete", "head", "options"];

function operations(spec: Record<string, unknown>): Operation[] {
  const paths = (spec.paths ?? {}) as Record<string, Record<string, Record<string, unknown>>>;
  const list: Operation[] = [];
  for (const [path, item] of Object.entries(paths)) {
    for (const method of METHODS) {
      const operation = item?.[method];
      if (!operation) continue;
      list.push({
        id: String(operation.operationId ?? `${method.toUpperCase()} ${path}`),
        method: method.toUpperCase(),
        path,
        summary: String(operation.summary ?? operation.description ?? ""),
        hasBody: operation.requestBody !== undefined,
      });
    }
  }
  return list;
}

function ApiView({ node }: { node: string }) {
  const t = useT();
  const { overview, annotate } = useApp();
  const [spec, setSpec] = useState<Record<string, unknown> | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const snapshot = overview.status.snapshot;

  useEffect(() => {
    let alive = true;
    api
      .openapi(node)
      .then((result) => {
        if (!alive) return;
        setSpec(result.spec);
        setProblem(null);
      })
      .catch((err: Error) => alive && setProblem(err.message));
    return () => {
      alive = false;
    };
    // The document changes as the agent writes the API.
  }, [node, snapshot]);

  const list = spec ? operations(spec) : [];
  return (
    <div className="stack">
      <h4>{t("be.api")}</h4>
      {problem ? (
        <p className="muted">
          {t("be.unavailable")}: {problem}
        </p>
      ) : null}
      {spec && list.length === 0 ? <p className="muted">{t("be.api.none")}</p> : null}
      <ul className="plain ops">
        {list.map((op) => (
          <li key={`${op.method} ${op.path}`}>
            <button className="op" aria-expanded={open === op.id} onClick={() => setOpen(open === op.id ? null : op.id)}>
              <span className={`method ${op.method.toLowerCase()}`}>{op.method}</span>
              <span className="mono">{op.path}</span>
              <span className="muted clip">{op.summary}</span>
            </button>
            {open === op.id ? (
              <div className="op-body">
                <TryIt node={node} operation={op} />
                <button className="link" onClick={() => annotate({ kind: "api_operation", node, operation: op.id })}>
                  {t("be.annotate")}
                </button>
              </div>
            ) : null}
          </li>
        ))}
      </ul>
    </div>
  );
}

function TryIt({ node, operation }: { node: string; operation: Operation }) {
  const t = useT();
  const { act } = useApp();
  const [path, setPath] = useState(operation.path);
  const [body, setBody] = useState(operation.hasBody ? "{\n  \n}" : "");
  const [response, setResponse] = useState<{ status: number; body: string; duration_ms: number } | null>(null);

  return (
    <form
      className="stack"
      onSubmit={(e) => {
        e.preventDefault();
        act(() =>
          api.tryRequest(node, {
            method: operation.method,
            path,
            headers: body ? [["content-type", "application/json"]] : [],
            body: body || undefined,
          }),
        ).then((result) => result && setResponse(result));
      }}
    >
      <label className="stack">
        <span className="muted">{t("be.api.path")}</span>
        <input className="mono" value={path} onChange={(e) => setPath(e.target.value)} />
      </label>
      {operation.hasBody ? (
        <label className="stack">
          <span className="muted">{t("be.api.body")}</span>
          <textarea className="mono" rows={5} value={body} onChange={(e) => setBody(e.target.value)} />
        </label>
      ) : null}
      <div>
        <button className="primary">{t("be.api.try")}</button>
      </div>
      {response ? (
        <div>
          <span className="muted">{t("be.api.response")}: </span>
          <strong className={response.status < 400 ? "ok-text" : "warn"}>{response.status}</strong>{" "}
          <span className="muted">{response.duration_ms} ms</span>
          <pre className="log">{pretty(response.body)}</pre>
        </div>
      ) : null}
    </form>
  );
}

function pretty(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

function DbView({ node }: { node: string }) {
  const t = useT();
  const { overview, annotate } = useApp();
  const [tables, setTables] = useState<DbTable[] | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const snapshot = overview.status.snapshot;

  useEffect(() => {
    let alive = true;
    api
      .database(node)
      .then((result) => {
        if (!alive) return;
        setTables(result.tables);
        setProblem(null);
      })
      .catch((err: Error) => alive && setProblem(err.message));
    return () => {
      alive = false;
    };
  }, [node, snapshot]);

  return (
    <div className="stack">
      <h4>{t("be.db")}</h4>
      {problem ? (
        <p className="muted">
          {t("be.unavailable")}: {problem}
        </p>
      ) : null}
      {tables && tables.length === 0 ? <p className="muted">{t("be.db.none")}</p> : null}
      {tables?.map((table) => (
        <div key={table.name} className="table-card">
          <div className="row">
            <strong className="mono">{table.name}</strong>
            <span className="spacer" />
            <button className="link" onClick={() => annotate({ kind: "db_table", node, table: table.name, column: null })}>
              {t("be.annotate")}
            </button>
          </div>
          <table>
            <tbody>
              {table.columns.map((column) => {
                const fk = table.foreign_keys.find((k) => k.column === column.name);
                return (
                  <tr key={column.name}>
                    <td className="mono">
                      {column.primary_key ? "🔑 " : ""}
                      {column.name}
                    </td>
                    <td className="mono muted">
                      {column.type}
                      {column.nullable ? "?" : ""}
                    </td>
                    <td className="mono muted">{fk ? `→ ${fk.references_table}.${fk.references_column}` : ""}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      ))}
    </div>
  );
}
