import { useState } from "react";
import { useReveal } from "../lib/motion";

type Metric = { bench: "Village" | "Stress"; label: string; unit: string; tuff: number; vanilla: number; better: "higher" | "lower"; ratio: string };

export const metrics: Metric[] = [
  { bench: "Village", label: "Ticks per second", unit: "ticks/s", tuff: 1026, vanilla: 89.6, better: "higher", ratio: "11.5x" },
  { bench: "Village", label: "p99 tick", unit: "ms", tuff: 2.13, vanilla: 12.9, better: "lower", ratio: "6.1x lower" },
  { bench: "Village", label: "Server memory", unit: "MiB", tuff: 1379, vanilla: 8966, better: "lower", ratio: "6.5x lower" },
  { bench: "Stress", label: "TPS", unit: "TPS", tuff: 20.35, vanilla: 2.15, better: "higher", ratio: "9.5x" },
  { bench: "Stress", label: "Mean tick", unit: "ms", tuff: 48.2, vanilla: 190, better: "lower", ratio: "3.9x lower" },
  { bench: "Stress", label: "Server memory", unit: "MiB", tuff: 2514, vanilla: 9005, better: "lower", ratio: "3.6x lower" },
];

const table = [
  ["Village, 10-day sprint (1 player, 32-chunk view)", "ticks per second", "1026", "89.6", "11.5x"],
  ["Village", "mean tick (ms)", "0.97", "11.15", "11.5x lower"],
  ["Village", "p99 tick (ms)", "2.13", "12.9", "6.1x lower"],
  ["Village", "server memory (MiB)", "1379", "8966", "6.5x lower"],
  ["Village", "CPU cores used", "2.70", "1.02", ""],
  ["Stress, 50 players mixed (12 flying, 12 mining, 26 walking; ~1250 entities; view 10)", "TPS", "20.35", "2.15", "9.5x (tuff at the 20 TPS cap)"],
  ["Stress", "mean tick (ms)", "48.2", "190", "3.9x lower"],
  ["Stress", "p99 tick (ms)", "102", "253", "2.5x lower"],
  ["Stress", "server memory (MiB)", "2514", "9005", "3.6x lower"],
  ["Stress", "CPU cores used", "15.35", "3.90", ""],
];

const fmt = (n: number) => n.toLocaleString("en-US");

function Bar({ name, value, max, unit, color, delay }: { name: string; value: number; max: number; unit: string; color: string; delay: number }) {
  const pct = Math.max(1.2, (value / max) * 100);
  return (
    <div title={`${name}: ${fmt(value)} ${unit}`}>
      <div className="flex items-baseline justify-between text-sm">
        <span className="text-fg-muted">{name}</span>
        <span className="font-mono tabular-nums text-fg">{fmt(value)}</span>
      </div>
      <div className="mt-1.5 h-2.5">
        <div className="bar-fill h-2.5 rounded-r-[4px]" style={{ width: `${pct}%`, background: color, ["--delay" as string]: `${delay}ms` }} />
      </div>
    </div>
  );
}

export function PerfChart() {
  const [view, setView] = useState<"chart" | "table">("chart");
  const ref = useReveal<HTMLDivElement>();
  return (
    <div ref={ref} className="reveal">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div className="flex items-center gap-5 text-sm" aria-label="Legend">
          <span className="flex items-center gap-2"><span className="h-3 w-3 rounded-sm bg-ember" /> tuff</span>
          <span className="flex items-center gap-2"><span className="h-3 w-3 rounded-sm bg-steel" /> Vanilla 26.3</span>
        </div>
        <div role="tablist" aria-label="Benchmark view" className="flex rounded-xl border border-line bg-bg-sunken p-1 text-sm">
          {(["chart", "table"] as const).map((v) => (
            <button key={v} role="tab" aria-selected={view === v} onClick={() => setView(v)} className={`rounded-lg px-3.5 py-1.5 font-medium capitalize transition ${view === v ? "bg-bg-raised text-fg shadow-sm" : "text-fg-muted hover:text-fg"}`}>
              {v}
            </button>
          ))}
        </div>
      </div>

      {view === "chart" ? (
        <div className="mt-6 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {metrics.map((m, i) => {
            const max = Math.max(m.tuff, m.vanilla);
            return (
              <figure key={m.bench + m.label} className="card p-5">
                <figcaption className="flex items-baseline justify-between gap-3">
                  <span>
                    <span className="block text-xs font-semibold uppercase tracking-[0.1em] text-fg-subtle">{m.bench}</span>
                    <span className="mt-1 block font-medium">{m.label} <span className="text-fg-subtle">({m.unit})</span></span>
                  </span>
                  <span className="shrink-0 rounded-full bg-ember-soft px-2.5 py-1 font-mono text-xs font-semibold text-ember-text">{m.ratio}</span>
                </figcaption>
                <div className="mt-5 space-y-3">
                  <Bar name="tuff" value={m.tuff} max={max} unit={m.unit} color="var(--ember)" delay={i * 90} />
                  <Bar name="Vanilla" value={m.vanilla} max={max} unit={m.unit} color="var(--steel)" delay={i * 90 + 60} />
                </div>
                <p className="mt-4 text-xs text-fg-subtle">{m.better === "higher" ? "Higher is better" : "Lower is better"}</p>
              </figure>
            );
          })}
        </div>
      ) : (
        <div className="card mt-6 overflow-x-auto">
          <table className="w-full min-w-[640px] text-left text-sm">
            <caption className="sr-only">tuff and vanilla benchmark medians</caption>
            <thead className="bg-bg-sunken text-fg-muted">
              <tr>{["Benchmark", "Metric", "tuff", "Vanilla", "Ratio"].map((h) => <th key={h} scope="col" className="px-4 py-3 font-semibold">{h}</th>)}</tr>
            </thead>
            <tbody>
              {table.map((r, i) => (
                <tr key={i} className="border-t border-line">
                  <td className="px-4 py-3 text-fg-muted">{r[0]}</td>
                  <td className="px-4 py-3">{r[1]}</td>
                  <td className="px-4 py-3 font-mono tabular-nums font-semibold text-ember-text">{r[2]}</td>
                  <td className="px-4 py-3 font-mono tabular-nums">{r[3]}</td>
                  <td className="px-4 py-3 text-fg-muted">{r[4]}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
