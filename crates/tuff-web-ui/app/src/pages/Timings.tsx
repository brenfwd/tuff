import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { API, api } from "../api";
import {
  ALL_THREADS,
  drawFlame,
  filterTree,
  formatNs,
  frameLabel,
  hit,
  layout,
  pathTo,
  regionsOf,
  rootOf,
  rootsFor,
  ROW,
  search,
  threadGroups,
  TimingsNode,
  TimingsReport,
} from "../flame";

interface Recording {
  started_unix_ms: number;
  elapsed_ms: number;
  threads: number;
  stops_after_s: number | null;
  by: string;
}

interface Summary {
  id: string;
  file: string | null;
  file_error: string | null;
  duration_ms: number;
  ticks: number;
  tps: number | null;
  mspt_mean: number | null;
  mspt_p99: number | null;
  mspt_max: number | null;
  top: { label: string; self_ns: number }[];
  dropped: number;
}

interface Status {
  recording: Recording | null;
  busy_elsewhere: boolean;
  last: Summary | null;
}

interface Info {
  id: string;
  file: string;
  started_unix_ms: number | null;
}

const DURATIONS = [
  { label: "Until stopped", seconds: null },
  { label: "30 s", seconds: 30 },
  { label: "1 min", seconds: 60 },
  { label: "5 min", seconds: 300 },
  { label: "10 min", seconds: 600 },
];

const BUTTON = "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";
const INPUT = "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm dark:border-zinc-700";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function seconds(ms: number): string {
  const s = Math.floor(ms / 1000);
  return s >= 60 ? `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s` : `${(ms / 1000).toFixed(1)} s`;
}

function useDark(): boolean {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  useEffect(() => {
    const observer = new MutationObserver(() => setDark(document.documentElement.classList.contains("dark")));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);
  return dark;
}

function downloadUrl(id: string, format: string): string {
  return `${API}/timings/reports/${encodeURIComponent(id)}?format=${format}&download=1`;
}

function Recorder({ status, onChange, setError }: { status: Status | null; onChange: () => void; setError: (e: string) => void }) {
  const [duration, setDuration] = useState(0);
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(Date.now());
  const [seen, setSeen] = useState(Date.now());
  const recording = status?.recording ?? null;

  useEffect(() => setSeen(Date.now()), [status]);
  useEffect(() => {
    if (!recording) return;
    const timer = setInterval(() => setNow(Date.now()), 200);
    return () => clearInterval(timer);
  }, [recording]);

  const act = async (path: string, body: unknown) => {
    setBusy(true);
    try {
      await api(`/timings/${path}`, { method: "POST", body: JSON.stringify(body) });
      setError("");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
      onChange();
    }
  };

  const elapsed = recording ? recording.elapsed_ms + Math.max(0, now - seen) : 0;
  return (
    <div className="flex flex-wrap items-center gap-2">
      <select className={INPUT} value={duration} disabled={!!recording} onChange={(e) => setDuration(Number(e.target.value))}>
        {DURATIONS.map((d, i) => (
          <option key={d.label} value={i}>
            {d.label}
          </option>
        ))}
      </select>
      <button
        type="button"
        className={`${BUTTON} font-semibold text-red-700 dark:text-red-400`}
        disabled={busy || !!recording || status?.busy_elsewhere}
        onClick={() => act("start", { seconds: DURATIONS[duration].seconds })}
      >
        Record
      </button>
      <button type="button" className={BUTTON} disabled={busy || !recording} onClick={() => act("stop", {})}>
        Stop
      </button>
      <span className="text-sm text-zinc-500">
        {recording ? (
          <span className="text-red-600 dark:text-red-400">
            Recording {seconds(elapsed)}
            {recording.stops_after_s ? ` of ${seconds(recording.stops_after_s * 1000)}` : ""} on {recording.threads} threads (started by{" "}
            {recording.by})
          </span>
        ) : status?.busy_elsewhere ? (
          "Another server in this process is recording."
        ) : (
          "Idle"
        )}
      </span>
    </div>
  );
}

function LastSummary({ summary }: { summary: Summary }) {
  return (
    <div className="text-sm">
      <div>
        Last: <span className="font-mono">{summary.id}</span>, {seconds(summary.duration_ms)}
        {summary.tps != null && (
          <>
            , {summary.ticks} ticks, {summary.tps.toFixed(2)} TPS, mspt mean {summary.mspt_mean?.toFixed(2)} p99 {summary.mspt_p99?.toFixed(2)} max{" "}
            {summary.mspt_max?.toFixed(2)}
          </>
        )}
      </div>
      {summary.file_error && <div className="text-amber-600">Not saved: {summary.file_error}</div>}
      {summary.dropped > 0 && <div className="text-amber-600">{summary.dropped} spans dropped</div>}
    </div>
  );
}

function Tooltip({ node, root, x, y }: { node: TimingsNode; root: TimingsNode; x: number; y: number }) {
  const pct = root.total_ns > 0 ? ((node.total_ns / root.total_ns) * 100).toFixed(1) : "0";
  const rows: [string, string][] = [
    ["total", `${formatNs(node.total_ns)} (${pct}%)`],
    ["self", formatNs(node.self_ns)],
  ];
  if (node.count > 0) {
    rows.push(["count", node.count.toLocaleString()]);
    rows.push(["mean", formatNs(node.total_ns / node.count)]);
    rows.push(["p50 / p90 / p99", `${formatNs(node.p50_ns)} / ${formatNs(node.p90_ns)} / ${formatNs(node.p99_ns)}`]);
    rows.push(["min / max", `${formatNs(node.min_ns)} / ${formatNs(node.max_ns)}`]);
  }
  return (
    <div
      className="pointer-events-none fixed z-10 max-w-md rounded border border-zinc-300 bg-white px-2 py-1 text-xs shadow dark:border-zinc-700 dark:bg-zinc-900"
      style={{ left: Math.min(x + 12, window.innerWidth - 320), top: y + 14 }}
    >
      <div className="mb-1 font-mono font-semibold break-all">{frameLabel(node)}</div>
      {node.async && <div className="text-blue-600">async: wall time across awaits</div>}
      <table className="font-mono">
        <tbody>
          {rows.map(([k, v]) => (
            <tr key={k}>
              <td className="pr-3 text-zinc-500">{k}</td>
              <td>{v}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function Flame({ report }: { report: TimingsReport }) {
  const [thread, setThread] = useState(ALL_THREADS);
  const [region, setRegion] = useState("");
  const [withAsync, setWithAsync] = useState(false);
  const [query, setQuery] = useState("");
  const [zoom, setZoom] = useState<TimingsNode | null>(null);
  const [hover, setHover] = useState<{ node: TimingsNode; x: number; y: number } | null>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const dark = useDark();

  const threads = useMemo(() => threadGroups(report.threads), [report]);
  const base = useMemo(() => rootsFor(report, thread), [report, thread]);
  const regions = useMemo(() => regionsOf(base), [base]);
  const root = useMemo(
    () => rootOf(filterTree(base, { async: withAsync, region: region || undefined }), thread === ALL_THREADS ? "all threads" : "thread"),
    [base, withAsync, region, thread],
  );
  useEffect(() => setZoom(null), [root]);
  const view = zoom ?? root;
  const crumbs = useMemo(() => (zoom ? pathTo(root, zoom) : [root]), [root, zoom]);
  const width = canvas.current?.clientWidth || 1000;
  const { rects, depth } = useMemo(() => layout(view, 0.5 / width), [view, width]);
  const found = useMemo(() => search(view, query), [view, query]);

  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const render = () => drawFlame(el, { rects, depth, matches: found.matches, hover: hover?.node, dark });
    render();
    const observer = new ResizeObserver(render);
    observer.observe(el);
    return () => observer.disconnect();
  }, [rects, depth, found, hover, dark]);

  const at = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    return hit(rects, Math.floor((e.clientY - box.top) / ROW), (e.clientX - box.left) / box.width);
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <label className="flex items-center gap-1">
          Thread
          <select className={INPUT} value={thread} onChange={(e) => setThread(e.target.value)}>
            <option value={ALL_THREADS}>All threads (merged)</option>
            {threads.map((group) => (
              <optgroup key={group.name} label={`${group.name} (${group.threads.length})`}>
                {group.threads.map((t) => (
                  <option key={t.value} value={t.value}>
                    {t.label}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1">
          Region
          <select className={INPUT} value={region} onChange={(e) => setRegion(e.target.value)}>
            <option value="">All</option>
            {regions.map((r) => (
              <option key={r} value={r}>
                {r.replace(/^minecraft:/, "")}
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1">
          <input type="checkbox" checked={withAsync} onChange={(e) => setWithAsync(e.target.checked)} />
          Async spans (wall time)
        </label>
        <input className={`${INPUT} min-w-48 flex-1`} placeholder="Search frames" value={query} onChange={(e) => setQuery(e.target.value)} />
        {query.trim() && (
          <span className="text-zinc-500">
            {found.matches.size} matches, {formatNs(found.total)} ({view.total_ns > 0 ? ((found.total / view.total_ns) * 100).toFixed(1) : 0}%)
          </span>
        )}
      </div>
      <nav className="flex flex-wrap items-center gap-1 text-sm">
        {crumbs.map((n, i) => (
          <span key={i} className="flex items-center gap-1">
            {i > 0 && <span className="text-zinc-400">/</span>}
            <button
              type="button"
              disabled={i === crumbs.length - 1}
              onClick={() => setZoom(i === 0 ? null : n)}
              className="rounded px-1 font-mono hover:bg-zinc-100 disabled:font-semibold disabled:hover:bg-transparent dark:hover:bg-zinc-900"
            >
              {frameLabel(n)}
            </button>
          </span>
        ))}
        <span className="text-zinc-500">{formatNs(view.total_ns)}</span>
      </nav>
      {root.total_ns === 0 ? (
        <p className="text-sm text-zinc-500">Nothing recorded for this selection.</p>
      ) : (
        <canvas
          ref={canvas}
          className="w-full cursor-pointer"
          onMouseMove={(e) => {
            const r = at(e);
            setHover(r ? { node: r.node, x: e.clientX, y: e.clientY } : null);
          }}
          onMouseLeave={() => setHover(null)}
          onClick={(e) => {
            const r = at(e);
            if (r) setZoom(r.node === root ? null : r.node);
          }}
        />
      )}
      {hover && <Tooltip node={hover.node} root={root} x={hover.x} y={hover.y} />}
    </div>
  );
}

function ReportView({ report }: { report: TimingsReport }) {
  const ticks = report.ticks;
  const features = report.features.filter((p) => p.calls > 0).slice(0, 8);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap gap-x-4 gap-y-1 text-sm text-zinc-500">
        <span>
          <span className="font-mono text-zinc-900 dark:text-zinc-100">{report.id}</span>, {new Date(report.started_unix_ms).toLocaleString()}
        </span>
        <span>{formatNs(report.duration_ns)}</span>
        {ticks && (
          <span>
            {ticks.count} ticks, {ticks.tps.toFixed(2)} TPS, mspt mean {ticks.mspt.mean.toFixed(2)} p50 {ticks.mspt.p50.toFixed(2)} p99{" "}
            {ticks.mspt.p99.toFixed(2)} max {ticks.mspt.max.toFixed(2)}
          </span>
        )}
        <span>{report.threads.length} threads</span>
        {report.dropped > 0 && <span className="text-amber-600">{report.dropped} spans dropped</span>}
      </div>
      {features.length > 0 && (
        <div className="text-sm text-zinc-500">
          Feature hooks:{" "}
          {features.map((p) => (
            <span key={p.id} className="mr-3 font-mono">
              {p.id} {formatNs(p.total_ns)} ({p.calls.toLocaleString()})
            </span>
          ))}
        </div>
      )}
      <Flame report={report} />
    </div>
  );
}

export function Timings() {
  const [status, setStatus] = useState<Status | null>(null);
  const [reports, setReports] = useState<Info[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [report, setReport] = useState<TimingsReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const lastId = useRef<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [s, list] = await Promise.all([api<Status>("/timings"), api<{ reports: Info[] }>("/timings/reports")]);
      setStatus(s);
      setReports(list.reports);
      const newest = s.last?.id ?? null;
      if (newest && newest !== lastId.current) {
        if (lastId.current !== null) setSelected(newest);
        lastId.current = newest;
      }
    } catch (e) {
      setError(message(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const recording = !!status?.recording;
  useEffect(() => {
    const timer = setInterval(() => void refresh(), recording ? 1000 : 5000);
    return () => clearInterval(timer);
  }, [recording, refresh]);

  useEffect(() => {
    if (!selected) {
      setReport(null);
      return;
    }
    let cancelled = false;
    setReport(null);
    setLoading(true);
    api<TimingsReport>(`/timings/reports/${encodeURIComponent(selected)}`)
      .then((r) => {
        if (!cancelled) {
          setReport(r);
          setError("");
        }
      })
      .catch((e) => !cancelled && setError(message(e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [selected]);

  return (
    <div className="flex flex-col gap-3 p-4">
      <section className="flex flex-col gap-2 rounded border border-zinc-200 p-3 dark:border-zinc-800">
        <h2 className="text-sm font-semibold">Recorder</h2>
        <Recorder status={status} onChange={() => void refresh()} setError={setError} />
        {status?.last && <LastSummary summary={status.last} />}
        {error && <p className="text-sm text-red-600">{error}</p>}
      </section>
      <section className="rounded border border-zinc-200 p-3 dark:border-zinc-800">
        <h2 className="mb-2 text-sm font-semibold">Recordings</h2>
        <ul className="max-h-56 overflow-y-auto text-sm">
          {reports.map((r) => (
            <li key={r.id} className="flex flex-wrap items-center gap-2 border-t border-zinc-200 py-1 first:border-t-0 dark:border-zinc-800">
              <button
                type="button"
                onClick={() => setSelected(r.id)}
                className={`rounded px-1 font-mono ${r.id === selected ? "bg-zinc-200 dark:bg-zinc-800" : "hover:bg-zinc-100 dark:hover:bg-zinc-900"}`}
              >
                {r.id}
              </button>
              {r.started_unix_ms != null && <span className="text-zinc-500">{new Date(r.started_unix_ms).toLocaleString()}</span>}
              <span className="flex-1" />
              <a className={BUTTON} href={downloadUrl(r.id, "json")} download>
                JSON
              </a>
              <a className={BUTTON} href={downloadUrl(r.id, "folded")} download>
                Folded
              </a>
              <a className={BUTTON} href={downloadUrl(r.id, "speedscope")} download>
                Speedscope
              </a>
            </li>
          ))}
          {reports.length === 0 && <li className="text-zinc-500">No recordings yet.</li>}
        </ul>
      </section>
      {selected && (
        <section className="rounded border border-zinc-200 p-3 dark:border-zinc-800">
          {loading && !report ? <p className="text-sm text-zinc-500">Loading {selected}...</p> : report && <ReportView report={report} />}
        </section>
      )}
    </div>
  );
}
