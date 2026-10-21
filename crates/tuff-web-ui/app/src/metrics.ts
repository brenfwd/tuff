export type Point = [number, number, number];

export interface TickReport {
  tick: number;
  tps: number;
  mspt_mean: number;
  mspt_max: number;
  mspt_p95: number;
  target_rate: number;
  frozen: boolean;
  sprinting: boolean;
  behind: number;
  players: { name: string; uuid: string; ping_ms: number }[];
}

export interface RegionReport {
  dimension: string;
  x: number;
  z: number;
  mspt_mean: number;
  mspt_max: number;
  chunks: number;
  entities: number;
  players: number;
}

export interface SeriesValue {
  kind: "counter" | "gauge";
  value: number;
  rate: number;
}

export interface Snapshot {
  unix_ms: number;
  uptime_s: number;
  tick: TickReport | null;
  tick_stale: boolean;
  world: {
    regions: number;
    chunks: number;
    loading: number;
    generating: number;
    entities: number;
    block_entities: number;
    scheduled_ticks: number;
    players: number;
  };
  slowest_regions: RegionReport[];
  manager: { regions: number; closing: number; region_tickets: number; held_chunks: number; forced_chunks: number } | null;
  chunk_io: {
    queued: number;
    running: number;
    open_files: number;
    reads: number;
    writes: number;
    bytes_read: number;
    bytes_written: number;
    save_latency_max_ms: number;
  } | null;
  runtime: { workers: number; alive_tasks: number; global_queue_depth: number; rayon_threads: number } | null;
  memory: { rss_bytes: number; peak_rss_bytes: number | null } | null;
  cpu: { cores: number; host_cores: number | null } | null;
  series: Record<string, SeriesValue>;
  dropped_reports: number;
}

export interface Live {
  snapshot: Snapshot;
  panel: { auditFailures: number };
}

export interface Card {
  series: string;
  title: string;
  unit: Unit;
}

export interface Alert {
  ts: number;
  text: string;
}

export type Unit = "count" | "bytes" | "millis" | "cores";

export const LIVE_WINDOW_MS = 10 * 60 * 1000;

/** The values the server records into history for one snapshot, by
 * series name, so live points line up with fetched history. */
export function seriesOf(s: Snapshot): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [name, v] of Object.entries(s.series)) out[name] = v.kind === "counter" ? v.rate : v.value;
  if (s.tick && !s.tick_stale) {
    out["tick.tps"] = s.tick.tps;
    out["tick.mspt"] = s.tick.mspt_mean;
    out["tick.mspt.max"] = s.tick.mspt_max;
    out["tick.mspt.p95"] = s.tick.mspt_p95;
  }
  out["players"] = s.tick ? s.tick.players.length : s.world.players;
  out["world.regions"] = s.world.regions;
  out["world.chunks"] = s.world.chunks;
  out["world.entities"] = s.world.entities;
  if (s.chunk_io) out["chunk_io.queued"] = s.chunk_io.queued;
  if (s.memory) out["memory.rss"] = s.memory.rss_bytes;
  if (s.cpu) out["process.cpu"] = s.cpu.cores;
  return out;
}

/** Appends `point` unless it is not newer than the last one, dropping
 * points older than `windowMs` before it. */
export function append(points: Point[], point: Point, windowMs = LIVE_WINDOW_MS): Point[] {
  const last = points[points.length - 1];
  if (last && point[0] <= last[0]) return points;
  const since = point[0] - windowMs;
  let start = 0;
  while (start < points.length && points[start][0] < since) start++;
  return [...points.slice(start), point];
}

export function formatBytes(n: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = Math.abs(n);
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const digits = i === 0 || v >= 100 ? 0 : 1;
  return `${n < 0 ? "-" : ""}${v.toFixed(digits)} ${units[i]}`;
}

export function formatNumber(n: number): string {
  if (!Number.isFinite(n)) return "-";
  if (Math.abs(n) >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (Math.abs(n) >= 1e4) return `${(n / 1e3).toFixed(1)}k`;
  if (Number.isInteger(n)) return String(n);
  return n.toFixed(Math.abs(n) >= 100 ? 0 : Math.abs(n) >= 10 ? 1 : 2);
}

export function formatUnit(n: number, unit: Unit, rate = false): string {
  const suffix = rate ? "/s" : "";
  switch (unit) {
    case "bytes":
      return formatBytes(n) + suffix;
    case "millis":
      return `${n.toFixed(n >= 10 ? 1 : 2)} ms${suffix}`;
    case "cores":
      return `${n.toFixed(n >= 10 ? 1 : 2)} cores${suffix}`;
    default:
      return formatNumber(n) + suffix;
  }
}

export function formatUptime(seconds: number): string {
  const s = Math.floor(seconds);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m ${s % 60}s`;
}
