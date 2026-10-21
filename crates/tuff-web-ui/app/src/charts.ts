import type { Point } from "./metrics";

export interface Extent {
  min: number;
  max: number;
}

/** The value range to plot: covers every point and `include` (e.g. a
 * 50 ms threshold), starts at 0 for non-negative data, never empty. */
export function extent(series: Point[][], include: number[] = [], useMax = false): Extent {
  let min = Infinity;
  let max = -Infinity;
  for (const points of series) {
    for (const p of points) {
      const v = useMax ? Math.max(p[1], p[2]) : p[1];
      if (!Number.isFinite(v)) continue;
      min = Math.min(min, p[1]);
      max = Math.max(max, v);
    }
  }
  for (const v of include) {
    min = Math.min(min, v);
    max = Math.max(max, v);
  }
  if (!Number.isFinite(min)) return { min: 0, max: 1 };
  if (min >= 0) min = 0;
  if (max <= min) max = min + 1;
  return { min, max };
}

/** Round tick values from `e.min` to at least `e.max`, about `count`. */
export function niceTicks(e: Extent, count = 4): number[] {
  const span = e.max - e.min;
  const raw = span / Math.max(1, count);
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? 10 * mag;
  const ticks: number[] = [];
  const start = Math.ceil(e.min / step) * step;
  for (let v = start; v <= e.max + step * 1e-9; v += step) ticks.push(Number(v.toFixed(10)));
  return ticks;
}

/** Maps points into a `width` x `height` box, x by time over [t0, t1]. */
export function project(points: Point[], t0: number, t1: number, e: Extent, width: number, height: number): [number, number][] {
  const dt = Math.max(1, t1 - t0);
  const dv = e.max - e.min || 1;
  return points.map((p) => [((p[0] - t0) / dt) * width, height - ((p[1] - e.min) / dv) * height]);
}

export interface Line {
  points: Point[];
  color: string;
  fill?: boolean;
}

export interface ChartOptions {
  lines: Line[];
  t0: number;
  t1: number;
  /** Horizontal reference lines, e.g. the 50 ms tick budget. */
  thresholds?: { value: number; color: string }[];
  axis?: boolean;
  format?: (v: number) => string;
  grid?: string;
  text?: string;
}

/** Draws a line chart onto `canvas`, sized to its CSS box at device
 * pixel ratio. */
export function draw(canvas: HTMLCanvasElement, o: ChartOptions) {
  const ratio = window.devicePixelRatio || 1;
  const cssW = canvas.clientWidth;
  const cssH = canvas.clientHeight;
  if (!cssW || !cssH) return;
  if (canvas.width !== Math.round(cssW * ratio) || canvas.height !== Math.round(cssH * ratio)) {
    canvas.width = Math.round(cssW * ratio);
    canvas.height = Math.round(cssH * ratio);
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  ctx.clearRect(0, 0, cssW, cssH);
  const left = o.axis ? 44 : 1;
  const top = o.axis ? 6 : 2;
  const bottom = o.axis ? 4 : 2;
  const w = cssW - left - 1;
  const h = cssH - top - bottom;
  const e = extent(
    o.lines.map((l) => l.points),
    (o.thresholds ?? []).map((t) => t.value),
  );
  ctx.save();
  ctx.translate(left, top);
  if (o.axis) {
    ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
    ctx.textAlign = "right";
    ctx.textBaseline = "middle";
    for (const v of niceTicks(e)) {
      const y = h - ((v - e.min) / (e.max - e.min)) * h;
      ctx.strokeStyle = o.grid ?? "rgba(128,128,128,0.2)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(0, Math.round(y) + 0.5);
      ctx.lineTo(w, Math.round(y) + 0.5);
      ctx.stroke();
      ctx.fillStyle = o.text ?? "rgb(113,113,122)";
      ctx.fillText(o.format ? o.format(v) : String(v), -6, y);
    }
  }
  for (const t of o.thresholds ?? []) {
    const y = h - ((t.value - e.min) / (e.max - e.min)) * h;
    ctx.strokeStyle = t.color;
    ctx.setLineDash([4, 3]);
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
    ctx.setLineDash([]);
  }
  for (const line of o.lines) {
    const xy = project(line.points, o.t0, o.t1, e, w, h);
    if (!xy.length) continue;
    ctx.beginPath();
    xy.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
    ctx.strokeStyle = line.color;
    ctx.lineWidth = 1.5;
    ctx.lineJoin = "round";
    ctx.stroke();
    if (line.fill) {
      ctx.lineTo(xy[xy.length - 1][0], h);
      ctx.lineTo(xy[0][0], h);
      ctx.closePath();
      ctx.globalAlpha = 0.15;
      ctx.fillStyle = line.color;
      ctx.fill();
      ctx.globalAlpha = 1;
    }
  }
  ctx.restore();
}
