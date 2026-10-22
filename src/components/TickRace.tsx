import { useEffect, useRef, useState } from "react";
import { prefersReducedMotion } from "../lib/motion";

const RATES = { tuff: 386, vanilla: 91.6 };
const DAY = 24000;

/** Counts ticks at the measured village-benchmark rates while the panel is on screen. */
export function TickRace() {
  const ref = useRef<HTMLDivElement>(null);
  const [elapsed, setElapsed] = useState(0);
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (prefersReducedMotion()) {
      setReduced(true);
      return;
    }
    let frame = 0;
    let last = 0;
    let total = 0;
    const loop = (t: number) => {
      if (last) total += Math.min(0.1, (t - last) / 1000);
      last = t;
      setElapsed(total);
      frame = requestAnimationFrame(loop);
    };
    const io = new IntersectionObserver(([e]) => {
      cancelAnimationFrame(frame);
      last = 0;
      if (e.isIntersecting) frame = requestAnimationFrame(loop);
    });
    io.observe(el);
    return () => {
      io.disconnect();
      cancelAnimationFrame(frame);
    };
  }, []);

  const lanes = [
    { name: "tuff", rate: RATES.tuff, color: "var(--ember)" },
    { name: "Vanilla 26.3", rate: RATES.vanilla, color: "var(--steel)" },
  ];

  return (
    <div ref={ref} className="card relative overflow-hidden p-5 sm:p-7">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 className="font-display text-lg font-semibold">Tick race</h3>
        <p className="text-xs text-fg-subtle">{reduced ? "Measured median rates" : "Live, at the measured median rates"}</p>
      </div>
      <div className="mt-6 space-y-6">
        {lanes.map((l) => {
          const ticks = Math.floor(elapsed * l.rate);
          const dayFrac = reduced ? 0 : (elapsed * l.rate) / DAY;
          return (
            <div key={l.name}>
              <div className="flex items-baseline justify-between gap-3">
                <span className="flex items-center gap-2 text-sm font-medium"><span className="h-2.5 w-2.5 rounded-sm" style={{ background: l.color }} />{l.name}</span>
                <span className="font-mono text-sm tabular-nums text-fg-muted">{l.rate.toLocaleString("en-US")} ticks/s</span>
              </div>
              <div className="mt-2 font-mono text-3xl font-semibold tabular-nums tracking-tight sm:text-4xl" aria-hidden={!reduced}>
                {reduced ? `${((l.rate * 60) / DAY).toFixed(2)} days/min` : ticks.toLocaleString("en-US")}
              </div>
              <div className="mt-3 grid grid-cols-[repeat(24,minmax(0,1fr))] gap-[3px]" aria-hidden="true">
                {Array.from({ length: 24 }, (_, i) => {
                  const filled = reduced ? i < Math.round(((l.rate * 60) / DAY) * 9) : (dayFrac % 1) * 24 > i;
                  return <span key={i} className="h-2.5 rounded-[3px] transition-colors duration-300" style={{ background: filled ? l.color : "var(--line)" }} />;
                })}
              </div>
              {!reduced && <p className="mt-2 text-xs text-fg-subtle">In-game days simulated: {dayFrac.toFixed(3)}</p>}
            </div>
          );
        })}
      </div>
      <p className="mt-6 border-t border-line pt-4 text-xs leading-5 text-fg-subtle">
        Village benchmark, 2-day sprint, 1 player, view and simulation distance 32 for both servers; tuff on about 4.8 cores, vanilla on 1. A Minecraft day is 24,000 ticks, so tuff simulates about 0.97 days a minute where vanilla manages about 0.23.
      </p>
    </div>
  );
}
