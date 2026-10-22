import { useState } from "react";
import { useReveal } from "../lib/motion";

type Metric = { bench: "Village" | "Stress"; label: string; unit: string; tuff: number; vanilla: number; better: "higher" | "lower"; ratio: string };

export const metrics: Metric[] = [
  { bench: "Village", label: "Ticks per second, sim 32", unit: "ticks/s", tuff: 386, vanilla: 91.6, better: "higher", ratio: "4.2x" },
  { bench: "Village", label: "Ticks per second, sim 10", unit: "ticks/s", tuff: 1019, vanilla: 529, better: "higher", ratio: "1.9x" },
  { bench: "Village", label: "p99 tick, sim 32", unit: "ms", tuff: 4.52, vanilla: 12.75, better: "lower", ratio: "2.8x lower" },
  { bench: "Stress", label: "TPS, sim 10 (tuff at the 20 TPS cap)", unit: "TPS", tuff: 20.0, vanilla: 2.12, better: "higher", ratio: "at cap" },
  { bench: "Stress", label: "TPS, sim 32", unit: "TPS", tuff: 14.2, vanilla: 1.92, better: "higher", ratio: "7.4x" },
  { bench: "Stress", label: "Mean tick, sim 10", unit: "ms", tuff: 47.7, vanilla: 191.9, better: "lower", ratio: "4.0x lower" },
];

const table = [
  ["Village, sim 32 (1 player, 32-chunk view, 2-day sprint)", "ticks per second", "386", "91.6", "4.2x"],
  ["Village, sim 32", "mean / p99 tick (ms)", "2.58 / 4.52", "10.9 / 12.8", ""],
  ["Village, sim 32", "CPU cores used", "4.8", "1.0", ""],
  ["Village, sim 10", "ticks per second", "1019", "529", "1.9x"],
  ["Village, sim 10", "mean / p99 tick (ms)", "0.97 / 2.23", "1.88 / 3.17", ""],
  ["Village, sim 10", "CPU cores used", "2.8", "1.1", ""],
  ["Stress, sim 10 (50 players: 12 flying, 12 mining, 26 walking; 1250 mobs)", "TPS", "20.0", "2.1", "tuff at the 20 TPS cap"],
  ["Stress, sim 10", "mean / p99 tick (ms)", "48 / 106", "192 / 254", ""],
  ["Stress, sim 10", "CPU cores used", "15.4", "4.0", ""],
  ["Stress, sim 32", "TPS", "14.2", "1.9", "7.4x"],
  ["Stress, sim 32", "mean / p99 tick (ms)", "70 / 180", "196 / 241", ""],
  ["Stress, sim 32", "CPU cores used", "14.8", "3.5", ""],
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
