// The shell's component kit, shared with plugin pages through
// `globalThis.__tuffPanel.Ui` (plugin builds import it as `@tuff/panel-ui`).
import { useEffect, useId, useState, type ReactNode } from "react";

export interface Column<T> {
  key: string;
  title: string;
  render: (row: T) => ReactNode;
  className?: string;
}

/** The rows of page `page` (0-based, clamped) and the page count. */
export function pageOf<T>(rows: T[], page: number, size: number): { rows: T[]; page: number; pages: number } {
  const pages = Math.max(1, Math.ceil(rows.length / Math.max(1, size)));
  const at = Math.min(Math.max(0, Math.floor(page)), pages - 1);
  return { rows: rows.slice(at * size, at * size + size), page: at, pages };
}

export interface TableProps<T> {
  columns: Column<T>[];
  rows: T[];
  rowKey: (row: T) => string;
  pageSize?: number;
  empty?: ReactNode;
}

const BUTTON =
  "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";

export function Table<T>({ columns, rows, rowKey, pageSize = 25, empty = "Nothing to show." }: TableProps<T>) {
  const [page, setPage] = useState(0);
  const shown = pageOf(rows, page, pageSize);
  useEffect(() => {
    if (shown.page !== page) setPage(shown.page);
  }, [shown.page, page]);
  return (
    <div className="flex flex-col gap-2">
      <table className="w-full text-left text-sm">
        <thead className="text-zinc-500">
          <tr>
            {columns.map((c) => (
              <th key={c.key} className={`py-1 pr-3 ${c.className ?? ""}`}>
                {c.title}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {shown.rows.map((row) => (
            <tr key={rowKey(row)} className="border-t border-zinc-200 dark:border-zinc-800">
              {columns.map((c) => (
                <td key={c.key} className={`py-1 pr-3 ${c.className ?? ""}`}>
                  {c.render(row)}
                </td>
              ))}
            </tr>
          ))}
          {rows.length === 0 && (
            <tr>
              <td colSpan={columns.length} className="py-2 text-zinc-500">
                {empty}
              </td>
            </tr>
          )}
        </tbody>
      </table>
      {shown.pages > 1 && (
        <div className="flex items-center gap-2 text-sm">
          <button type="button" aria-label="Previous page" className={BUTTON} disabled={shown.page === 0} onClick={() => setPage(shown.page - 1)}>
            Previous
          </button>
          <span className="text-zinc-500">
            {shown.page + 1} / {shown.pages}
          </span>
          <button type="button" aria-label="Next page" className={BUTTON} disabled={shown.page + 1 >= shown.pages} onClick={() => setPage(shown.page + 1)}>
            Next
          </button>
        </div>
      )}
    </div>
  );
}

/** A point of a series: [x, y]. */
export type XY = [number, number];

/** SVG path data for `points` scaled into a `width` by `height` box (y up);
 * empty for no points. */
export function linePath(points: XY[], width: number, height: number): string {
  const finite = points.filter(([x, y]) => Number.isFinite(x) && Number.isFinite(y));
  if (finite.length === 0) return "";
  const xs = finite.map((p) => p[0]);
  const ys = finite.map((p) => p[1]);
  const [x0, x1] = [Math.min(...xs), Math.max(...xs)];
  const y0 = Math.min(0, ...ys);
  const y1 = Math.max(...ys);
  const sx = (x: number) => (x1 === x0 ? width / 2 : ((x - x0) / (x1 - x0)) * width);
  const sy = (y: number) => (y1 === y0 ? height / 2 : height - ((y - y0) / (y1 - y0)) * height);
  return finite.map(([x, y], i) => `${i === 0 ? "M" : "L"}${sx(x).toFixed(1)},${sy(y).toFixed(1)}`).join(" ");
}

export interface LineChartProps {
  points: XY[];
  label: string;
  height?: number;
  unit?: string;
}

export function LineChart({ points, label, height = 120, unit = "" }: LineChartProps) {
  const width = 400;
  const last = points.length > 0 ? points[points.length - 1][1] : null;
  return (
    <figure className="flex flex-col gap-1">
      <figcaption className="flex justify-between text-sm text-zinc-500">
        <span>{label}</span>
        <span className="font-mono">{last === null ? "-" : `${last}${unit}`}</span>
      </figcaption>
      <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" className="w-full text-sky-600" style={{ height }} role="img" aria-label={label}>
        <path d={linePath(points, width, height)} fill="none" stroke="currentColor" strokeWidth={2} vectorEffect="non-scaling-stroke" />
      </svg>
    </figure>
  );
}

export interface FieldProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  type?: "text" | "number" | "password";
  /** Why the value is not accepted, shown under the input. */
  error?: string | null;
  hint?: string;
}

export function Field({ label, value, onChange, type = "text", error, hint }: FieldProps) {
  const id = useId();
  return (
    <label htmlFor={id} className="flex flex-col gap-1 text-sm">
      <span>{label}</span>
      <input
        id={id}
        type={type}
        value={value}
        aria-invalid={error ? true : undefined}
        onChange={(e) => onChange(e.target.value)}
        className="rounded border border-zinc-300 bg-transparent px-2 py-1 dark:border-zinc-700"
      />
      {error ? <span className="text-xs text-red-600">{error}</span> : hint && <span className="text-xs text-zinc-500">{hint}</span>}
    </label>
  );
}

export function Stat({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="max-w-sm rounded border border-zinc-200 p-4 dark:border-zinc-800">
      <p className="text-sm text-zinc-500">{label}</p>
      <p className="font-mono text-4xl tabular-nums">{value}</p>
    </div>
  );
}

export function Button({ children, onClick, disabled }: { children: ReactNode; onClick: () => void; disabled?: boolean }) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="rounded bg-zinc-200 px-3 py-1 text-sm hover:bg-zinc-300 disabled:opacity-50 dark:bg-zinc-800 dark:hover:bg-zinc-700"
    >
      {children}
    </button>
  );
}
