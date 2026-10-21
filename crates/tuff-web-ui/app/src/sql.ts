export const EXAMPLE_QUERIES: { label: string; sql: string }[] = [
  { label: "Players", sql: "SELECT name, dimension, x, y, z, health, xp_level FROM players ORDER BY name" },
  { label: "Entity counts", sql: "SELECT type, count(*) AS n FROM entity_list GROUP BY type ORDER BY n DESC LIMIT 20" },
  { label: "Largest chunks", sql: "SELECT dimension, x, z, length(data) AS bytes FROM chunks ORDER BY bytes DESC LIMIT 20" },
  { label: "Chests", sql: "SELECT type, x, y, z FROM block_entities WHERE type = 'minecraft:chest' LIMIT 50" },
  { label: "Tables and views", sql: "SELECT name FROM sqlite_schema WHERE type IN ('table','view') ORDER BY name" },
];

export const PAGE_SIZE = 100;
export const HISTORY_KEY = "tuff.sql.history";
export const HISTORY_CAP = 50;

export interface BlobCell {
  blob: string;
  size: number;
}
export type Cell = null | number | string | BlobCell;

export interface SqlInfo {
  available: boolean;
  reason?: string;
  backend: string;
  canWrite: boolean;
  limits: { rows: number; bytes: number; timeoutMs: number };
}

export interface SchemaColumn {
  name: string;
  type: string;
  pk: boolean;
  notNull: boolean;
}
export interface SchemaObject {
  name: string;
  type: "table" | "view";
  columns: SchemaColumn[];
}
export interface SchemaFunction {
  name: string;
  signature: string;
  description: string;
}
export interface Schema {
  objects: SchemaObject[];
  functions: SchemaFunction[];
}

export interface QueryResult {
  columns: { name: string; declType: string | null }[];
  rows: Cell[][];
  types: string[][];
  hasMore: boolean;
  truncated: boolean;
  elapsedMs: number;
  cursor?: number;
}

export interface PlanRow {
  id: number;
  parent: number;
  detail: string;
}

export interface SavedQuery {
  name: string;
  sql: string;
}

export function isBlob(c: Cell): c is BlobCell {
  return typeof c === "object" && c !== null && "blob" in c;
}

export function cellText(c: Cell): string {
  if (c === null) return "NULL";
  if (isBlob(c)) return `x'${c.blob}'${c.size * 2 > c.blob.length ? "..." : ""} (${c.size} bytes)`;
  return String(c);
}

function tsvField(c: Cell): string {
  if (c === null) return "";
  return cellText(c).replace(/[\t\r\n]/g, " ");
}

export function toTsv(columns: string[] | null, rows: Cell[][]): string {
  const lines = rows.map((r) => r.map(tsvField).join("\t"));
  if (columns) lines.unshift(columns.map((c) => c.replace(/[\t\r\n]/g, " ")).join("\t"));
  return lines.join("\n");
}

export function csvField(c: Cell): string {
  if (c === null) return "";
  const s = cellText(c);
  return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function toCsv(columns: string[], rows: Cell[][]): string {
  return [columns.map(csvField), ...rows.map((r) => r.map(csvField))].map((l) => l.join(",")).join("\r\n");
}

function rank(c: Cell): number {
  if (c === null) return 0;
  if (typeof c === "number") return 1;
  if (typeof c === "string") return 2;
  return 3;
}

export function compareCells(a: Cell, b: Cell): number {
  const ra = rank(a);
  const rb = rank(b);
  if (ra !== rb) return ra - rb;
  if (typeof a === "number" && typeof b === "number") return a - b;
  if (typeof a === "string" && typeof b === "string") return a < b ? -1 : a > b ? 1 : 0;
  if (isBlob(a) && isBlob(b)) return a.size - b.size || (a.blob < b.blob ? -1 : a.blob > b.blob ? 1 : 0);
  return 0;
}

export function sortedIndices(rows: Cell[][], column: number, desc: boolean): number[] {
  const idx = rows.map((_, i) => i);
  idx.sort((x, y) => {
    const c = compareCells(rows[x][column] ?? null, rows[y][column] ?? null);
    return (desc ? -c : c) || x - y;
  });
  return idx;
}

export function parseJsonCell(c: Cell): unknown | undefined {
  if (typeof c !== "string") return undefined;
  const t = c.trim();
  if (!(t.startsWith("{") && t.endsWith("}")) && !(t.startsWith("[") && t.endsWith("]"))) return undefined;
  try {
    return JSON.parse(t);
  } catch {
    return undefined;
  }
}

export function pushHistory(history: string[], sql: string, cap = HISTORY_CAP): string[] {
  const q = sql.trim();
  if (!q) return history;
  return [q, ...history.filter((h) => h !== q)].slice(0, cap);
}

export function parseHistory(raw: string | null): string[] {
  try {
    const v = JSON.parse(raw ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string").slice(0, HISTORY_CAP) : [];
  } catch {
    return [];
  }
}

export interface CompletionData {
  schema: Record<string, string[]>;
  functions: { label: string; detail: string; info: string }[];
}

export function completionData(s: Schema): CompletionData {
  const schema: Record<string, string[]> = {};
  for (const o of s.objects) schema[o.name] = o.columns.map((c) => c.name);
  const functions = s.functions.map((f) => ({ label: f.name, detail: f.signature, info: f.description }));
  return { schema, functions };
}

export function upsertSaved(list: SavedQuery[], q: SavedQuery): SavedQuery[] {
  const rest = list.filter((x) => x.name !== q.name);
  return [...rest, q].sort((a, b) => a.name.localeCompare(b.name));
}

export interface PlanNode {
  row: PlanRow;
  depth: number;
}

export function flattenPlan(plan: PlanRow[]): PlanNode[] {
  const ids = new Set(plan.map((p) => p.id));
  const kids = new Map<number, PlanRow[]>();
  for (const p of plan) {
    const parent = ids.has(p.parent) && p.parent !== p.id ? p.parent : 0;
    const list = kids.get(parent) ?? [];
    list.push(p);
    kids.set(parent, list);
  }
  const out: PlanNode[] = [];
  const seen = new Set<number>();
  const walk = (parent: number, depth: number) => {
    for (const p of kids.get(parent) ?? []) {
      if (seen.has(p.id)) continue;
      seen.add(p.id);
      out.push({ row: p, depth });
      walk(p.id, depth + 1);
    }
  };
  walk(0, 0);
  return out;
}

export function filenameFrom(disposition: string | null, fallback: string): string {
  const m = disposition && /filename="?([^";]+)"?/i.exec(disposition);
  return m ? m[1] : fallback;
}
