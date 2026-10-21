/** A call-tree node as the timings report (schema version 1) stores it. */
export interface TimingsNode {
  name: string;
  tag?: string;
  async?: boolean;
  count: number;
  total_ns: number;
  self_ns: number;
  min_ns: number;
  max_ns: number;
  p50_ns: number;
  p90_ns: number;
  p99_ns: number;
  children?: TimingsNode[];
}

export interface TimingsThread {
  index: number;
  name: string;
  dropped: number;
  total_ns: number;
  roots: TimingsNode[];
}

export interface TimingsReport {
  format: string;
  version: number;
  id: string;
  started_unix_ms: number;
  duration_ns: number;
  server: { name: string; version: string; game_version: string };
  ticks: { count: number; tps: number; mspt: { mean: number; min: number; max: number; p50: number; p90: number; p99: number } } | null;
  features: { id: string; total_ns: number; calls: number }[];
  dropped: number;
  threads: TimingsThread[];
  merged: TimingsNode[];
}

export const ALL_THREADS = "all";

export function frameLabel(node: TimingsNode): string {
  return node.tag ? `${node.name} [${node.tag}]` : node.name;
}

const REGION_TAG = /^\S+ -?\d+,-?\d+$/;

/** Whether a tag names a region (`<dimension> <x>,<z>`). */
export function isRegionTag(tag: string | undefined): tag is string {
  return tag !== undefined && REGION_TAG.test(tag);
}

/** Every region tag in the trees, sorted. */
export function regionsOf(roots: TimingsNode[]): string[] {
  const out = new Set<string>();
  const walk = (n: TimingsNode) => {
    if (isRegionTag(n.tag)) out.add(n.tag);
    n.children?.forEach(walk);
  };
  roots.forEach(walk);
  return [...out].sort();
}

export function rootsFor(report: TimingsReport, thread: string): TimingsNode[] {
  if (thread === ALL_THREADS) return report.merged;
  return report.threads.find((t) => String(t.index) === thread)?.roots ?? [];
}

export interface ThreadGroup {
  name: string;
  threads: { value: string; label: string }[];
}

const byName = (a: string, b: string) => a.localeCompare(b, "en", { numeric: true });

/** Threads grouped by pool (the name without its `-<index>`), sorted by
 * name; a name seen twice keeps its recorder index to stay distinct. */
export function threadGroups(threads: TimingsThread[]): ThreadGroup[] {
  const seen = new Map<string, number>();
  threads.forEach((t) => seen.set(t.name, (seen.get(t.name) ?? 0) + 1));
  const groups = new Map<string, TimingsThread[]>();
  for (const t of threads) {
    const pool = t.name.replace(/-\d+$/, "");
    groups.set(pool, [...(groups.get(pool) ?? []), t]);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => byName(a, b))
    .map(([name, members]) => ({
      name,
      threads: members
        .sort((a, b) => byName(a.name, b.name) || a.index - b.index)
        .map((t) => ({
          value: String(t.index),
          label: `${(seen.get(t.name) ?? 0) > 1 ? `${t.name} #${t.index}` : t.name} (${formatNs(t.total_ns)})`,
        })),
    }));
}

export interface Filter {
  /** Only time under this region's nodes; ancestors keep only that time. */
  region?: string;
  /** Keep asynchronous (wall time) nodes. */
  async: boolean;
}

function sum(nodes: TimingsNode[]): number {
  return nodes.reduce((a, n) => a + n.total_ns, 0);
}

/** The trees with `filter` applied; nodes are copied only when changed. */
export function filterTree(roots: TimingsNode[], filter: Filter): TimingsNode[] {
  const visit = (n: TimingsNode, inRegion: boolean): TimingsNode | null => {
    if (!filter.async && n.async) return null;
    let matched = inRegion;
    if (filter.region !== undefined && isRegionTag(n.tag)) {
      if (n.tag !== filter.region) return null;
      matched = true;
    }
    const children: TimingsNode[] = [];
    let changed = false;
    for (const c of n.children ?? []) {
      const kept = visit(c, matched);
      if (kept !== c) changed = true;
      if (kept) children.push(kept);
    }
    if (filter.region !== undefined && !matched) {
      if (children.length === 0) return null;
      return { ...n, children, self_ns: 0, total_ns: sum(children) };
    }
    return changed ? { ...n, children } : n;
  };
  return roots.map((r) => visit(r, false)).filter((n): n is TimingsNode => n !== null);
}

/** One synthetic root over several trees, so they share one graph. */
export function rootOf(roots: TimingsNode[], name = "all"): TimingsNode {
  const total = sum(roots);
  return {
    name,
    count: 0,
    total_ns: total,
    self_ns: 0,
    min_ns: 0,
    max_ns: 0,
    p50_ns: 0,
    p90_ns: 0,
    p99_ns: 0,
    children: roots,
  };
}

export interface Rect {
  node: TimingsNode;
  depth: number;
  /** Left edge and width as fractions of the graph's width. */
  x: number;
  w: number;
}

/** Icicle layout of `root` across [0, 1]: children sit under their parent
 * in order, as wide as their share of the parent. Asynchronous children
 * can add up to more than their parent, so they are scaled to fit.
 * Rects narrower than `minWidth` are dropped with their subtrees. */
export function layout(root: TimingsNode, minWidth = 0): { rects: Rect[]; depth: number } {
  const rects: Rect[] = [];
  let depth = 0;
  const place = (n: TimingsNode, d: number, x: number, w: number) => {
    rects.push({ node: n, depth: d, x, w });
    depth = Math.max(depth, d);
    const kids = n.children ?? [];
    const scale = Math.max(n.total_ns, sum(kids));
    if (scale <= 0) return;
    let cx = x;
    for (const c of kids) {
      const cw = (c.total_ns / scale) * w;
      if (cw >= minWidth && cw > 0) place(c, d + 1, cx, cw);
      cx += cw;
    }
  };
  place(root, 0, 0, 1);
  return { rects, depth };
}

/** The rect at depth `depth` covering fraction `x`, if any. */
export function hit(rects: Rect[], depth: number, x: number): Rect | undefined {
  return rects.find((r) => r.depth === depth && x >= r.x && x < r.x + r.w);
}

/** Path from `root` down to `target` (both included), or empty. */
export function pathTo(root: TimingsNode, target: TimingsNode): TimingsNode[] {
  if (root === target) return [root];
  for (const c of root.children ?? []) {
    const p = pathTo(c, target);
    if (p.length) return [root, ...p];
  }
  return [];
}

/** Nodes whose label contains `query` (case-insensitive) and the time they
 * cover, not counting a match nested inside another. */
export function search(root: TimingsNode, query: string): { matches: Set<TimingsNode>; total: number } {
  const matches = new Set<TimingsNode>();
  let total = 0;
  const q = query.trim().toLowerCase();
  if (!q) return { matches, total };
  const walk = (n: TimingsNode, covered: boolean) => {
    const hitHere = frameLabel(n).toLowerCase().includes(q);
    if (hitHere) {
      matches.add(n);
      if (!covered) total += n.total_ns;
    }
    n.children?.forEach((c) => walk(c, covered || hitHere));
  };
  walk(root, false);
  return { matches, total };
}

export function formatNs(ns: number): string {
  if (ns < 1_000) return `${Math.round(ns)} ns`;
  if (ns < 1_000_000) return `${(ns / 1_000).toFixed(ns < 10_000 ? 2 : 1)} us`;
  if (ns < 1_000_000_000) return `${(ns / 1_000_000).toFixed(ns < 10_000_000 ? 2 : 1)} ms`;
  return `${(ns / 1_000_000_000).toFixed(2)} s`;
}

/** A stable warm color per frame name; async frames are blue, feature
 * frames purple. */
export function colorOf(node: TimingsNode): string {
  let h = 0;
  for (let i = 0; i < node.name.length; i++) h = (h * 31 + node.name.charCodeAt(i)) >>> 0;
  const jitter = h % 1000 / 1000;
  if (node.async) return `hsl(${205 + jitter * 20}, 65%, ${58 + jitter * 10}%)`;
  if (node.name === "feature") return `hsl(${270 + jitter * 20}, 55%, ${62 + jitter * 8}%)`;
  return `hsl(${jitter * 50}, 80%, ${55 + (h % 7) * 2}%)`;
}

export const ROW = 18;

export interface DrawOptions {
  rects: Rect[];
  depth: number;
  matches: Set<TimingsNode>;
  hover?: TimingsNode;
  dark: boolean;
}

/** Draws the graph at the canvas's CSS width; sets its height to fit. */
export function drawFlame(canvas: HTMLCanvasElement, o: DrawOptions) {
  const ratio = window.devicePixelRatio || 1;
  const cssW = canvas.clientWidth;
  const cssH = (o.depth + 1) * ROW;
  if (!cssW) return;
  canvas.style.height = `${cssH}px`;
  if (canvas.width !== Math.round(cssW * ratio) || canvas.height !== Math.round(cssH * ratio)) {
    canvas.width = Math.round(cssW * ratio);
    canvas.height = Math.round(cssH * ratio);
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  ctx.clearRect(0, 0, cssW, cssH);
  ctx.font = "11px ui-monospace, SFMono-Regular, Menlo, monospace";
  ctx.textBaseline = "middle";
  const searching = o.matches.size > 0;
  for (const r of o.rects) {
    const x = r.x * cssW;
    const w = r.w * cssW;
    if (w < 0.5) continue;
    const y = r.depth * ROW;
    const dim = searching && !o.matches.has(r.node);
    ctx.fillStyle = dim ? (o.dark ? "rgb(63,63,70)" : "rgb(212,212,216)") : colorOf(r.node);
    ctx.fillRect(x, y, Math.max(w - 1, 0.5), ROW - 1);
    if (r.node === o.hover) {
      ctx.strokeStyle = o.dark ? "#fff" : "#000";
      ctx.lineWidth = 1;
      ctx.strokeRect(x + 0.5, y + 0.5, Math.max(w - 2, 0.5), ROW - 2);
    }
    if (w > 28) {
      const label = fit(ctx, frameLabel(r.node), w - 6);
      if (label) {
        ctx.fillStyle = dim ? (o.dark ? "rgb(161,161,170)" : "rgb(113,113,122)") : "rgb(24,24,27)";
        ctx.fillText(label, x + 3, y + ROW / 2);
      }
    }
  }
}

function fit(ctx: CanvasRenderingContext2D, text: string, width: number): string {
  if (ctx.measureText(text).width <= width) return text;
  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (ctx.measureText(text.slice(0, mid) + "..").width <= width) lo = mid;
    else hi = mid - 1;
  }
  return lo > 0 ? text.slice(0, lo) + ".." : "";
}
