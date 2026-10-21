import { useCallback, useEffect, useRef, useState } from "react";
import { api, Session } from "../api";
import { subscribe } from "../events";
import { draw, Line } from "../charts";
import {
  Alert,
  append,
  Card,
  formatBytes,
  formatNumber,
  formatUnit,
  formatUptime,
  Live,
  Point,
  seriesOf,
  Snapshot,
  Unit,
} from "../metrics";

interface Overview extends Live {
  cards: Card[];
  alerts: Alert[];
}

interface History {
  series: Record<string, Point[] | null>;
}

interface Tile {
  title: string;
  series: string;
  unit: Unit;
  rate?: boolean;
  limit?: number;
}

const TILES: Tile[] = [
  { title: "TPS", series: "tick.tps", unit: "count" },
  { title: "MSPT mean", series: "tick.mspt", unit: "millis", limit: 50 },
  { title: "MSPT p95", series: "tick.mspt.p95", unit: "millis", limit: 50 },
  { title: "Players", series: "players", unit: "count" },
  { title: "Loaded chunks", series: "world.chunks", unit: "count" },
  { title: "Entities", series: "world.entities", unit: "count" },
  { title: "CPU", series: "process.cpu", unit: "cores" },
  { title: "Memory", series: "memory.rss", unit: "bytes" },
  { title: "Net in", series: "net.bytes_in", unit: "bytes", rate: true },
  { title: "Net out", series: "net.bytes_out", unit: "bytes", rate: true },
];

const RANGES = [
  { label: "10 min", range: "10m", ms: 10 * 60 * 1000 },
  { label: "1 h", range: "1h", ms: 60 * 60 * 1000 },
  { label: "24 h", range: "24h", ms: 24 * 60 * 60 * 1000 },
];

const MSPT_LINES = [
  { series: "tick.mspt", color: "#2563eb", label: "mean", fill: true },
  { series: "tick.mspt.p95", color: "#d97706", label: "p95" },
  { series: "tick.mspt.max", color: "#dc2626", label: "max" },
];
const BUDGET_MS = 50;
const MAX_HISTORY = 8;
const MAX_ALERTS = 100;

type Buffers = Record<string, Point[]>;

function useDark(): boolean {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  useEffect(() => {
    const observer = new MutationObserver(() => setDark(document.documentElement.classList.contains("dark")));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);
  return dark;
}

function Chart({ lines, span, thresholds, axis, format, className }: {
  lines: Line[];
  span: number;
  thresholds?: { value: number; color: string }[];
  axis?: boolean;
  format?: (v: number) => string;
  className: string;
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  const dark = useDark();
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const render = () => {
      const t1 = Math.max(Date.now(), ...lines.map((l) => l.points[l.points.length - 1]?.[0] ?? 0));
      draw(canvas, {
        lines,
        t0: t1 - span,
        t1,
        thresholds,
        axis,
        format,
        grid: dark ? "rgba(255,255,255,0.08)" : "rgba(0,0,0,0.08)",
        text: dark ? "rgb(161,161,170)" : "rgb(113,113,122)",
      });
    };
    render();
    const observer = new ResizeObserver(render);
    observer.observe(canvas);
    return () => observer.disconnect();
  }, [lines, span, thresholds, axis, format, dark]);
  return <canvas ref={ref} className={className} />;
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-3 text-sm">
      <span className="text-zinc-500">{label}</span>
      <span className="font-mono">{value}</span>
    </div>
  );
}

function Panel({ title, children, className = "" }: { title: string; children: React.ReactNode; className?: string }) {
  return (
    <section className={`rounded border border-zinc-200 p-3 dark:border-zinc-800 ${className}`}>
      <h2 className="mb-2 text-sm font-semibold">{title}</h2>
      {children}
    </section>
  );
}

function TileView({ tile, value, points }: { tile: Tile; value: number | undefined; points: Point[] }) {
  const over = tile.limit !== undefined && value !== undefined && value > tile.limit;
  const thresholds = tile.limit !== undefined ? [{ value: tile.limit, color: "rgba(220,38,38,0.5)" }] : undefined;
  return (
    <div className="rounded border border-zinc-200 p-3 dark:border-zinc-800">
      <div className="text-xs text-zinc-500">{tile.title}</div>
      <div className={`font-mono text-xl ${over ? "text-red-600 dark:text-red-400" : ""}`}>
        {value === undefined ? "-" : formatUnit(value, tile.unit, tile.rate)}
      </div>
      <Chart
        lines={[{ points, color: over ? "#dc2626" : "#2563eb", fill: true }]}
        span={RANGES[0].ms}
        thresholds={thresholds}
        className="mt-1 h-10 w-full"
      />
    </div>
  );
}

export function Dashboard({ session }: { session: Session }) {
  const [overview, setOverview] = useState<Overview | null>(null);
  const [live, setLive] = useState<Live | null>(null);
  const [buffers, setBuffers] = useState<Buffers>({});
  const [alerts, setAlerts] = useState<Alert[]>([]);
  const [range, setRange] = useState(RANGES[0]);
  const [longHistory, setLongHistory] = useState<Buffers>({});
  const [error, setError] = useState("");
  const canAlerts = session.scopes.includes("logs.read");

  const load = useCallback(async () => {
    try {
      const first = await api<Overview>("/metrics");
      setOverview(first);
      setLive(first);
      setAlerts(first.alerts.slice(-MAX_ALERTS).reverse());
      const names = [
        ...new Set([...TILES.map((t) => t.series), ...MSPT_LINES.map((l) => l.series), ...first.cards.map((c) => c.series)]),
      ];
      const fetched: Buffers = {};
      for (let i = 0; i < names.length; i += MAX_HISTORY) {
        const batch = names.slice(i, i + MAX_HISTORY);
        const h = await api<History>(`/metrics/history?series=${encodeURIComponent(batch.join(","))}&range=10m`);
        for (const name of batch) fetched[name] = h.series[name] ?? [];
      }
      setBuffers(fetched);
      setError("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void load();
    const topics = canAlerts ? ["metrics", "alerts"] : ["metrics"];
    return subscribe(topics, ["snapshot", "alert"], (event, data) => {
      if (event === "resync") {
        void load();
        return;
      }
      try {
        if (event === "snapshot") {
          const next = JSON.parse(data) as Live;
          setLive(next);
          const t = next.snapshot.unix_ms;
          const values = seriesOf(next.snapshot);
          setBuffers((old) => {
            const out: Buffers = { ...old };
            for (const [name, v] of Object.entries(values)) out[name] = append(old[name] ?? [], [t, v, v]);
            return out;
          });
        } else if (event === "alert") {
          const alert = JSON.parse(data) as Alert;
          setAlerts((old) => [alert, ...old].slice(0, MAX_ALERTS));
        }
      } catch {
        /* malformed event */
      }
    });
  }, [load, canAlerts]);

  useEffect(() => {
    if (range === RANGES[0]) return;
    let cancelled = false;
    const fetchLong = async () => {
      try {
        const names = MSPT_LINES.map((l) => l.series).join(",");
        const h = await api<History>(`/metrics/history?series=${encodeURIComponent(names)}&range=${range.range}`);
        if (cancelled) return;
        const out: Buffers = {};
        for (const l of MSPT_LINES) out[l.series] = h.series[l.series] ?? [];
        setLongHistory(out);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    };
    void fetchLong();
    const timer = setInterval(fetchLong, 60_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [range]);

  if (!live) {
    return <div className="p-4">{error ? <p className="text-red-600">{error}</p> : <p className="text-zinc-500">Loading metrics...</p>}</div>;
  }
  const s: Snapshot = live.snapshot;
  const values = seriesOf(s);
  const mspt = range === RANGES[0] ? buffers : longHistory;
  const msptLines: Line[] = MSPT_LINES.map((l) => ({ points: mspt[l.series] ?? [], color: l.color, fill: l.fill }));
  const status = !s.tick ? "no tick report yet" : s.tick_stale ? "stalled" : s.tick.frozen ? "frozen" : s.tick.sprinting ? "sprinting" : "running";

  return (
    <div className="flex flex-col gap-3 p-4">
      {error && <p className="text-sm text-red-600">{error}</p>}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-zinc-500">
        <span>
          Status: <span className={s.tick_stale ? "text-red-600" : "text-zinc-900 dark:text-zinc-100"}>{status}</span>
        </span>
        <span>Uptime {formatUptime(s.uptime_s)}</span>
        {s.tick && <span>Tick {s.tick.tick} at target {s.tick.target_rate}/s, {formatNumber(s.tick.behind)} behind</span>}
        {s.dropped_reports > 0 && <span className="text-amber-600">{s.dropped_reports} metric reports dropped</span>}
        {live.panel.auditFailures > 0 && <span className="text-red-600">{live.panel.auditFailures} audit write failures</span>}
      </div>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
        {TILES.map((t) => (
          <TileView key={t.series} tile={t} value={values[t.series]} points={buffers[t.series] ?? []} />
        ))}
        {(overview?.cards ?? []).map((c) => (
          <TileView
            key={c.series}
            tile={{ title: c.title, series: c.series, unit: c.unit, rate: s.series[c.series]?.kind === "counter" }}
            value={values[c.series]}
            points={buffers[c.series] ?? []}
          />
        ))}
      </div>
      <Panel title="Milliseconds per tick">
        <div className="mb-2 flex flex-wrap items-center gap-3 text-xs">
          {MSPT_LINES.map((l) => (
            <span key={l.series} className="flex items-center gap-1">
              <span className="inline-block h-2 w-3 rounded-sm" style={{ background: l.color }} />
              {l.label}
            </span>
          ))}
          <span className="text-zinc-500">dashed: {BUDGET_MS} ms budget</span>
          <span className="flex-1" />
          {RANGES.map((r) => (
            <button
              key={r.range}
              type="button"
              onClick={() => setRange(r)}
              className={`rounded px-2 py-0.5 ${r === range ? "bg-zinc-200 dark:bg-zinc-800" : "hover:bg-zinc-100 dark:hover:bg-zinc-900"}`}
            >
              {r.label}
            </button>
          ))}
        </div>
        <Chart
          lines={msptLines}
          span={range.ms}
          thresholds={[{ value: BUDGET_MS, color: "rgba(220,38,38,0.6)" }]}
          axis
          format={(v) => `${v} ms`}
          className="h-48 w-full"
        />
      </Panel>
      <div className="grid gap-3 lg:grid-cols-3">
        <Panel title="Slowest regions" className="overflow-x-auto lg:col-span-2">
          <table className="w-full text-left text-sm">
            <thead className="text-zinc-500">
              <tr>
                <th className="py-1 pr-3">Dimension</th>
                <th className="py-1 pr-3">Region</th>
                <th className="py-1 pr-3 text-right">Tick ms</th>
                <th className="py-1 pr-3 text-right">Max ms</th>
                <th className="py-1 pr-3 text-right">Chunks</th>
                <th className="py-1 pr-3 text-right">Entities</th>
                <th className="py-1 text-right">Players</th>
              </tr>
            </thead>
            <tbody className="font-mono">
              {s.slowest_regions.map((r) => (
                <tr key={`${r.dimension}/${r.x}/${r.z}`} className="border-t border-zinc-200 dark:border-zinc-800">
                  <td className="py-1 pr-3">{r.dimension.replace(/^minecraft:/, "")}</td>
                  <td className="py-1 pr-3">
                    {r.x}, {r.z}
                  </td>
                  <td className={`py-1 pr-3 text-right ${r.mspt_mean > BUDGET_MS ? "text-red-600" : ""}`}>{r.mspt_mean.toFixed(2)}</td>
                  <td className="py-1 pr-3 text-right">{r.mspt_max.toFixed(2)}</td>
                  <td className="py-1 pr-3 text-right">{r.chunks}</td>
                  <td className="py-1 pr-3 text-right">{r.entities}</td>
                  <td className="py-1 text-right">{r.players}</td>
                </tr>
              ))}
              {s.slowest_regions.length === 0 && (
                <tr>
                  <td colSpan={7} className="py-1 text-zinc-500">
                    No regions loaded.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </Panel>
        <div className="flex flex-col gap-3">
          <Panel title="World">
            <Stat label="Regions" value={formatNumber(s.world.regions)} />
            <Stat label="Chunks loading" value={formatNumber(s.world.loading)} />
            <Stat label="Chunks generating" value={formatNumber(s.world.generating)} />
            <Stat label="Block entities" value={formatNumber(s.world.block_entities)} />
            <Stat label="Scheduled ticks" value={formatNumber(s.world.scheduled_ticks)} />
            {s.manager && <Stat label="Region tickets" value={formatNumber(s.manager.region_tickets)} />}
            {s.manager && <Stat label="Held / forced chunks" value={`${s.manager.held_chunks} / ${s.manager.forced_chunks}`} />}
          </Panel>
          <Panel title="Chunk storage">
            {s.chunk_io ? (
              <>
                <Stat label="Queued / running" value={`${s.chunk_io.queued} / ${s.chunk_io.running}`} />
                <Stat label="Open files" value={formatNumber(s.chunk_io.open_files)} />
                <Stat label="Reads / writes" value={`${s.chunk_io.reads} / ${s.chunk_io.writes}`} />
                <Stat label="Read / written" value={`${formatBytes(s.chunk_io.bytes_read)} / ${formatBytes(s.chunk_io.bytes_written)}`} />
                <Stat label="Save latency max" value={formatUnit(s.chunk_io.save_latency_max_ms, "millis")} />
              </>
            ) : (
              <p className="text-sm text-zinc-500">No report yet.</p>
            )}
          </Panel>
          <Panel title="Runtime">
            {s.runtime && (
              <>
                <Stat label="Tokio workers" value={formatNumber(s.runtime.workers)} />
                <Stat label="Alive tasks" value={formatNumber(s.runtime.alive_tasks)} />
                <Stat label="Global queue" value={formatNumber(s.runtime.global_queue_depth)} />
                <Stat label="Rayon threads" value={formatNumber(s.runtime.rayon_threads)} />
              </>
            )}
            {s.cpu?.host_cores != null && <Stat label="CPU cores available" value={formatNumber(s.cpu.host_cores)} />}
            {s.memory?.peak_rss_bytes != null && <Stat label="Peak memory" value={formatBytes(s.memory.peak_rss_bytes)} />}
            <Stat label="Metric reports dropped" value={formatNumber(s.dropped_reports)} />
            <Stat label="Audit write failures" value={formatNumber(live.panel.auditFailures)} />
          </Panel>
        </div>
      </div>
      {canAlerts && (
        <Panel title="Anti-cheat alerts">
          <ul className="max-h-64 overflow-y-auto font-mono text-sm">
            {alerts.map((a, i) => (
              <li key={`${a.ts}-${i}`} className="border-t border-zinc-200 py-1 first:border-t-0 dark:border-zinc-800">
                <span className="text-zinc-500">{new Date(a.ts).toLocaleTimeString([], { hour12: false })}</span> {a.text}
              </li>
            ))}
            {alerts.length === 0 && <li className="text-zinc-500">No alerts.</li>}
          </ul>
        </Panel>
      )}
    </div>
  );
}
