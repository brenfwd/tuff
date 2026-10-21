import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { bracketMatching, defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { autocompletion, completionKeymap, type CompletionContext } from "@codemirror/autocomplete";
import { sql as sqlLang, SQLite } from "@codemirror/lang-sql";
import { oneDark } from "@codemirror/theme-one-dark";
import { API, api, Session, storageGet, storageSet } from "../api";
import { Toast } from "../plugins";
import {
  Cell,
  cellText,
  completionData,
  EXAMPLE_QUERIES,
  filenameFrom,
  flattenPlan,
  HISTORY_KEY,
  isBlob,
  PAGE_SIZE,
  parseHistory,
  parseJsonCell,
  PlanRow,
  pushHistory,
  QueryResult,
  SavedQuery,
  Schema,
  sortedIndices,
  SqlInfo,
  toTsv,
  upsertSaved,
} from "../sql";

const BUTTON =
  "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";
const INPUT = "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm dark:border-zinc-700 dark:bg-zinc-950";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function useDarkClass(): boolean {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  useEffect(() => {
    const el = document.documentElement;
    const obs = new MutationObserver(() => setDark(el.classList.contains("dark")));
    obs.observe(el, { attributes: true, attributeFilter: ["class"] });
    return () => obs.disconnect();
  }, []);
  return dark;
}

const baseTheme = EditorView.theme({
  "&": { minHeight: "8rem", fontSize: "13px" },
  ".cm-scroller": { fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace" },
  "&.cm-focused": { outline: "none" },
});
const lightTheme = EditorView.theme({ "&": { backgroundColor: "#ffffff", color: "#18181b" } }, { dark: false });

interface EditorHandle {
  get: () => string;
  set: (text: string) => void;
  insert: (text: string) => void;
}

function SqlEditor({
  schema,
  dark,
  onRun,
  handle,
}: {
  schema: Schema | null;
  dark: boolean;
  onRun: () => void;
  handle: React.RefObject<EditorHandle | null>;
}) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const themeC = useRef(new Compartment());
  const langC = useRef(new Compartment());
  const run = useRef(onRun);
  run.current = onRun;

  useEffect(() => {
    if (!host.current) return;
    const shadow = host.current.shadowRoot ?? host.current.attachShadow({ mode: "open" });
    const v = new EditorView({
      root: shadow,
      parent: shadow,
      state: EditorState.create({
        doc: "",
        extensions: [
          lineNumbers(),
          highlightActiveLine(),
          history(),
          bracketMatching(),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          keymap.of([
            { key: "Mod-Enter", run: () => (run.current(), true) },
            ...completionKeymap,
            ...defaultKeymap,
            ...historyKeymap,
          ]),
          langC.current.of(sqlLang({ dialect: SQLite })),
          baseTheme,
          themeC.current.of(lightTheme),
          EditorView.lineWrapping,
        ],
      }),
    });
    view.current = v;
    handle.current = {
      get: () => v.state.doc.toString(),
      set: (text) => v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: text } }),
      insert: (text) => {
        v.dispatch(v.state.replaceSelection(text));
        v.focus();
      },
    };
    return () => {
      v.destroy();
      view.current = null;
      handle.current = null;
    };
  }, [handle]);

  useEffect(() => {
    view.current?.dispatch({ effects: themeC.current.reconfigure(dark ? oneDark : lightTheme) });
  }, [dark]);

  useEffect(() => {
    if (!schema) return;
    const data = completionData(schema);
    const lang = sqlLang({ dialect: SQLite, schema: data.schema, upperCaseKeywords: true });
    const fnSource = (ctx: CompletionContext) => {
      const word = ctx.matchBefore(/\w+/);
      if (!word || (word.from === word.to && !ctx.explicit)) return null;
      return { from: word.from, options: data.functions.map((f) => ({ ...f, type: "function", apply: `${f.label}(` })) };
    };
    view.current?.dispatch({
      effects: langC.current.reconfigure([lang, lang.language.data.of({ autocomplete: fnSource }), autocompletion()]),
    });
  }, [schema]);

  return <div ref={host} className="overflow-hidden rounded border border-zinc-300 font-mono text-sm dark:border-zinc-700" />;
}

function JsonTree({ value, name, depth = 0 }: { value: unknown; name?: string; depth?: number }) {
  const [open, setOpen] = useState(depth < 2);
  const label = name !== undefined ? <span className="text-sky-700 dark:text-sky-400">{name}: </span> : null;
  if (value !== null && typeof value === "object") {
    const entries = Array.isArray(value) ? value.map((v, i) => [String(i), v] as const) : Object.entries(value);
    const brackets = Array.isArray(value) ? ["[", "]"] : ["{", "}"];
    return (
      <div className="pl-4">
        <button type="button" className="-ml-4 w-4 text-zinc-500" onClick={() => setOpen(!open)}>
          {open ? "-" : "+"}
        </button>
        {label}
        {brackets[0]}
        {!open && <span className="text-zinc-500"> {entries.length} items </span>}
        {open && entries.map(([k, v]) => <JsonTree key={k} name={k} value={v} depth={depth + 1} />)}
        {brackets[1]}
      </div>
    );
  }
  const cls = typeof value === "string" ? "text-emerald-700 dark:text-emerald-400" : "text-amber-700 dark:text-amber-400";
  return (
    <div className="pl-4">
      {label}
      <span className={cls}>{typeof value === "string" ? JSON.stringify(value) : String(value)}</span>
    </div>
  );
}

function CellViewer({ cell, onClose }: { cell: Cell; onClose: () => void }) {
  const json = parseJsonCell(cell);
  return (
    <div className="flex max-h-96 flex-col gap-2 overflow-auto rounded border border-zinc-300 p-2 text-sm dark:border-zinc-700">
      <div className="flex gap-2">
        <span className="flex-1 text-zinc-500">{isBlob(cell) ? `Blob, ${cell.size} bytes` : json !== undefined ? "JSON" : "Value"}</span>
        <button type="button" className={BUTTON} onClick={() => void navigator.clipboard?.writeText(typeof cell === "string" ? cell : cellText(cell))}>
          Copy
        </button>
        <button type="button" className={BUTTON} onClick={onClose}>
          Close
        </button>
      </div>
      {isBlob(cell) ? (
        <pre className="font-mono break-all whitespace-pre-wrap">
          {(cell.blob.match(/.{1,32}/g) ?? []).map((l) => l.replace(/(..)/g, "$1 ")).join("\n")}
          {cell.size * 2 > cell.blob.length ? `\n... (${cell.size - cell.blob.length / 2} more bytes)` : ""}
        </pre>
      ) : json !== undefined ? (
        <div className="-ml-4 font-mono">
          <JsonTree value={json} />
        </div>
      ) : (
        <pre className="font-mono whitespace-pre-wrap">{cellText(cell)}</pre>
      )}
    </div>
  );
}

function ResultGrid({
  result,
  offset,
  onPage,
  busy,
}: {
  result: QueryResult;
  offset: number;
  onPage: (offset: number) => void;
  busy: boolean;
}) {
  const [sort, setSort] = useState<{ col: number; desc: boolean } | null>(null);
  const [widths, setWidths] = useState<Record<number, number>>({});
  const [selected, setSelected] = useState<{ row: number; col: number } | null>(null);
  useEffect(() => {
    setSort(null);
    setSelected(null);
  }, [result]);
  const order = useMemo(
    () => (sort ? sortedIndices(result.rows, sort.col, sort.desc) : result.rows.map((_, i) => i)),
    [result, sort],
  );
  const names = result.columns.map((c) => c.name);
  const copy = (text: string) => void navigator.clipboard?.writeText(text);

  const startResize = (col: number, e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const th = (e.target as HTMLElement).parentElement;
    const start = e.clientX;
    const base = th?.getBoundingClientRect().width ?? 120;
    const move = (ev: MouseEvent) => setWidths((w) => ({ ...w, [col]: Math.max(40, base + ev.clientX - start) }));
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const sel = selected ? result.rows[selected.row]?.[selected.col] : undefined;
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span className="text-zinc-500">
          rows {result.rows.length === 0 ? 0 : offset + 1}-{offset + result.rows.length}
          {result.hasMore ? "+" : ""} in {result.elapsedMs} ms
        </span>
        {result.truncated && (
          <span className="rounded bg-amber-100 px-2 py-0.5 text-amber-900 dark:bg-amber-900 dark:text-amber-100">
            Truncated: the byte limit was hit
          </span>
        )}
        <span className="flex-1" />
        <button type="button" className={BUTTON} disabled={!selected} onClick={() => sel !== undefined && copy(toTsv(null, [[sel]]))}>
          Copy cell
        </button>
        <button type="button" className={BUTTON} disabled={!selected} onClick={() => selected && copy(toTsv(null, [result.rows[selected.row]]))}>
          Copy row
        </button>
        <button type="button" className={BUTTON} onClick={() => copy(toTsv(names, order.map((i) => result.rows[i])))}>
          Copy all
        </button>
        <button type="button" className={BUTTON} disabled={busy || offset === 0} onClick={() => onPage(Math.max(0, offset - PAGE_SIZE))}>
          Previous
        </button>
        <button type="button" className={BUTTON} disabled={busy || !result.hasMore} onClick={() => onPage(offset + PAGE_SIZE)}>
          Next
        </button>
      </div>
      <div className="max-h-[32rem] overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
        <table className="table-fixed border-collapse text-left font-mono text-xs">
          <thead className="sticky top-0 bg-zinc-100 dark:bg-zinc-900">
            <tr>
              {result.columns.map((c, i) => (
                <th
                  key={i}
                  className="relative cursor-pointer border-r border-zinc-200 px-2 py-1 select-none dark:border-zinc-800"
                  style={{ width: widths[i] ?? 160 }}
                  onClick={() => setSort((s) => (s && s.col === i ? (s.desc ? null : { col: i, desc: true }) : { col: i, desc: false }))}
                >
                  <div className="truncate">
                    {c.name}
                    {sort?.col === i ? (sort.desc ? " v" : " ^") : ""}
                  </div>
                  <div className="truncate font-normal text-zinc-500">{c.declType ?? result.types[0]?.[i] ?? ""}</div>
                  <span className="absolute top-0 right-0 h-full w-1.5 cursor-col-resize hover:bg-sky-500" onMouseDown={(e) => startResize(i, e)} />
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {order.map((r) => (
              <tr key={r} className="border-t border-zinc-200 dark:border-zinc-800">
                {result.rows[r].map((cell, c) => (
                  <td
                    key={c}
                    title={result.types[r]?.[c]}
                    onClick={() => setSelected({ row: r, col: c })}
                    className={`max-w-0 cursor-pointer truncate border-r border-zinc-200 px-2 py-0.5 dark:border-zinc-800 ${
                      selected?.row === r && selected.col === c ? "bg-sky-100 dark:bg-sky-900" : ""
                    } ${cell === null ? "text-zinc-400 italic" : ""}`}
                  >
                    {cellText(cell)}
                  </td>
                ))}
              </tr>
            ))}
            {result.rows.length === 0 && (
              <tr>
                <td colSpan={Math.max(1, result.columns.length)} className="px-2 py-2 text-zinc-500">
                  No rows.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      {sel !== undefined && sel !== null && (isBlob(sel) || parseJsonCell(sel) !== undefined || cellText(sel).length > 40) && (
        <CellViewer cell={sel} onClose={() => setSelected(null)} />
      )}
    </div>
  );
}

function Sidebar({ schema, onInsert }: { schema: Schema | null; onInsert: (s: string) => void }) {
  const [open, setOpen] = useState<Record<string, boolean>>({});
  if (!schema) return <p className="text-sm text-zinc-500">Loading schema...</p>;
  return (
    <div className="flex flex-col gap-0.5 text-sm">
      {schema.objects.map((o) => (
        <div key={o.name}>
          <div className="flex items-center gap-1">
            <button type="button" className="w-4 text-zinc-500" onClick={() => setOpen((s) => ({ ...s, [o.name]: !s[o.name] }))}>
              {open[o.name] ? "-" : "+"}
            </button>
            <button type="button" className="truncate font-mono hover:underline" onClick={() => onInsert(o.name)}>
              {o.name}
            </button>
            <span className="text-xs text-zinc-500">{o.type}</span>
          </div>
          {open[o.name] &&
            o.columns.map((c) => (
              <button
                key={c.name}
                type="button"
                className="flex w-full gap-1 pl-6 text-left font-mono text-xs hover:underline"
                onClick={() => onInsert(c.name)}
              >
                <span className="truncate">{c.name}</span>
                <span className="text-zinc-500">
                  {c.type}
                  {c.pk ? " pk" : ""}
                  {c.notNull ? " not null" : ""}
                </span>
              </button>
            ))}
        </div>
      ))}
      {schema.functions.length > 0 && <p className="mt-2 text-xs font-semibold text-zinc-500">Functions</p>}
      {schema.functions.map((f) => (
        <button key={f.name} type="button" title={f.description} className="truncate text-left font-mono text-xs hover:underline" onClick={() => onInsert(`${f.name}(`)}>
          {f.signature}
        </button>
      ))}
    </div>
  );
}

type Pending = { sql: string; affected: number; elapsedMs: number };

export function Sql({ session, toast }: { session: Session; toast: Toast }) {
  const dark = useDarkClass();
  const [info, setInfo] = useState<SqlInfo | null>(null);
  const [schema, setSchema] = useState<Schema | null>(null);
  const [pages, setPages] = useState<QueryResult[]>([]);
  const [pageIndex, setPageIndex] = useState(0);
  const result = pages[pageIndex] ?? null;
  const offset = pageIndex * PAGE_SIZE;
  const [error, setError] = useState<string | null>(null);
  const [plan, setPlan] = useState<PlanRow[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [hist, setHist] = useState<string[]>(() => parseHistory(storageGet(HISTORY_KEY)));
  const [saved, setSaved] = useState<SavedQuery[]>([]);
  const [saveName, setSaveName] = useState("");
  const [writeMode, setWriteMode] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [pending, setPending] = useState<Pending | null>(null);
  const editor = useRef<EditorHandle | null>(null);

  useEffect(() => {
    api<SqlInfo>("/sql")
      .then((i) => {
        setInfo(i);
        if (!i.available) return;
        api<Schema>("/sql/schema").then(setSchema, (e) => toast(`Could not load schema: ${message(e)}`, "error"));
        api<{ queries: SavedQuery[] }>("/sql/saved").then((r) => setSaved(r.queries), (e) => toast(message(e), "error"));
      })
      .catch((e) => setError(message(e)));
  }, [toast]);

  const remember = (sql: string) =>
    setHist((h) => {
      const next = pushHistory(h, sql);
      storageSet(HISTORY_KEY, JSON.stringify(next));
      return next;
    });

  const query = useCallback(async (sql: string) => {
    setBusy(true);
    setError(null);
    setPlan(null);
    try {
      const r = await api<QueryResult>("/sql/query", { method: "POST", body: JSON.stringify({ sql, limit: PAGE_SIZE }) });
      setPages([r]);
      setPageIndex(0);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const goToPage = useCallback(
    async (index: number) => {
      if (index < pages.length) {
        setPageIndex(index);
        return;
      }
      const cursor = pages[pages.length - 1]?.cursor;
      if (cursor === undefined) return;
      setBusy(true);
      setError(null);
      try {
        const r = await api<QueryResult>("/sql/query", { method: "POST", body: JSON.stringify({ cursor, limit: PAGE_SIZE }) });
        setPages((p) => [...p, r]);
        setPageIndex(index);
      } catch (e) {
        setError(message(e));
      } finally {
        setBusy(false);
      }
    },
    [pages],
  );

  const cancel = async () => {
    try {
      await api("/sql/cancel", { method: "POST", body: "{}" });
    } catch (e) {
      setError(message(e));
    }
  };

  const preview = async (sql: string) => {
    setBusy(true);
    setError(null);
    setPlan(null);
    try {
      const r = await api<{ affected: number; elapsedMs: number }>("/sql/write", { method: "POST", body: JSON.stringify({ sql, action: "preview" }) });
      setPending({ sql, affected: r.affected, elapsedMs: r.elapsedMs });
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  const commit = async () => {
    if (!pending) return;
    setBusy(true);
    try {
      const r = await api<{ affected: number; warning?: string }>("/sql/write", {
        method: "POST",
        body: JSON.stringify({ sql: pending.sql, action: "commit", expect: pending.affected }),
      });
      toast(`Committed: ${r.affected} rows changed`);
      if (r.warning) toast(r.warning, "error");
      setPending(null);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  const rollback = async () => {
    if (!pending) return;
    setBusy(true);
    try {
      await api("/sql/write", { method: "POST", body: JSON.stringify({ sql: pending.sql, action: "rollback" }) });
      toast("Discarded");
      setPending(null);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  const run = () => {
    const sql = editor.current?.get().trim() ?? "";
    if (!sql || busy) return;
    remember(sql);
    if (writeMode) void preview(sql);
    else void query(sql);
  };

  const explain = async () => {
    const sql = editor.current?.get().trim() ?? "";
    if (!sql) return;
    setError(null);
    try {
      const r = await api<{ plan: PlanRow[] }>("/sql/explain", { method: "POST", body: JSON.stringify({ sql }) });
      setPlan(r.plan);
    } catch (e) {
      setError(message(e));
    }
  };

  const exportAs = async (format: "csv" | "json") => {
    const sql = editor.current?.get().trim() ?? "";
    if (!sql) return;
    try {
      const resp = await fetch(`${API}/sql/export`, {
        method: "POST",
        credentials: "same-origin",
        headers: { "content-type": "application/json", "x-tuff-csrf": session.csrf },
        body: JSON.stringify({ sql, format }),
      });
      if (!resp.ok) {
        const text = await resp.text();
        let msg = `HTTP ${resp.status}`;
        try {
          msg = JSON.parse(text).error ?? msg;
        } catch {
          /* not json */
        }
        throw new Error(msg);
      }
      const url = URL.createObjectURL(await resp.blob());
      const a = document.createElement("a");
      a.href = url;
      a.download = filenameFrom(resp.headers.get("content-disposition"), `query.${format}`);
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (e) {
      setError(message(e));
    }
  };

  const putSaved = async (queries: SavedQuery[]) => {
    try {
      const r = await api<{ queries: SavedQuery[] }>("/sql/saved", { method: "PUT", body: JSON.stringify({ queries }) });
      setSaved(r.queries);
    } catch (e) {
      toast(message(e), "error");
    }
  };

  if (!info) return <p className="p-4 text-sm text-zinc-500">{error ?? "Loading..."}</p>;
  if (!info.available) return <p className="p-4 text-sm">{info.reason ?? "SQL queries need the SQLite backend"}</p>;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {writeMode && (
        <div className="bg-red-600 px-4 py-1 text-center text-sm font-bold tracking-wide text-white" role="alert">
          READ-WRITE: statements change the live world file
        </div>
      )}
      <div className="flex min-h-0 flex-1 gap-4 p-4">
        <aside className="hidden w-56 shrink-0 overflow-y-auto md:block">
          <Sidebar schema={schema} onInsert={(s) => editor.current?.insert(s)} />
        </aside>
        <div className="flex min-w-0 flex-1 flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" className={`${BUTTON} font-semibold ${writeMode ? "text-red-700 dark:text-red-400" : ""}`} disabled={busy || !!pending} onClick={run}>
              {writeMode ? "Preview write" : "Run"}
            </button>
            {busy && (
              <button type="button" className={`${BUTTON} text-red-700 dark:text-red-400`} onClick={() => void cancel()}>
                Cancel
              </button>
            )}
            <button type="button" className={BUTTON} onClick={() => void explain()}>
              Explain
            </button>
            <button type="button" className={BUTTON} onClick={() => void exportAs("csv")}>
              Export CSV
            </button>
            <button type="button" className={BUTTON} onClick={() => void exportAs("json")}>
              Export JSON
            </button>
            <select className={INPUT} value="" onChange={(e) => e.target.value && editor.current?.set(e.target.value)}>
              <option value="">Examples...</option>
              {EXAMPLE_QUERIES.map((q) => (
                <option key={q.label} value={q.sql}>
                  {q.label}
                </option>
              ))}
            </select>
            <select className={`${INPUT} max-w-48`} value="" onChange={(e) => e.target.value && editor.current?.set(e.target.value)}>
              <option value="">History...</option>
              {hist.map((h) => (
                <option key={h} value={h}>
                  {h.slice(0, 80)}
                </option>
              ))}
            </select>
            <span className="flex-1" />
            {info.canWrite && (
              <label className="flex items-center gap-1 text-sm">
                <input
                  type="checkbox"
                  checked={writeMode}
                  disabled={!!pending}
                  onChange={(e) => (e.target.checked ? setConfirming(true) : setWriteMode(false))}
                />
                Read-write
              </label>
            )}
          </div>
          {confirming && (
            <div className="flex flex-col gap-2 rounded border border-red-400 p-3 text-sm" role="dialog" aria-modal="false">
              <p>
                Read-write mode runs statements against the live world file. Each statement is previewed in a transaction first; nothing changes until you
                press Commit. Changes cannot be undone except by restoring a backup.
              </p>
              <div className="flex gap-2">
                <button
                  type="button"
                  className={`${BUTTON} text-red-700 dark:text-red-400`}
                  onClick={() => {
                    setWriteMode(true);
                    setConfirming(false);
                  }}
                >
                  Enable read-write
                </button>
                <button type="button" className={BUTTON} onClick={() => setConfirming(false)}>
                  Cancel
                </button>
              </div>
            </div>
          )}
          <SqlEditor schema={schema} dark={dark} onRun={run} handle={editor} />
          <p className="text-xs text-zinc-500">
            Ctrl/Cmd+Enter runs. Limits: {info.limits.rows} rows, {info.limits.bytes} bytes, {info.limits.timeoutMs} ms. Backend: {info.backend}.
          </p>
          {error && (
            <pre className="rounded border border-red-400 bg-red-50 p-2 text-sm whitespace-pre-wrap text-red-800 dark:bg-red-950 dark:text-red-200">
              {error}
            </pre>
          )}
          {pending && (
            <div className="flex flex-wrap items-center gap-2 rounded border border-red-400 p-3 text-sm">
              <span className="flex-1">
                {pending.affected} {pending.affected === 1 ? "row" : "rows"} would change ({pending.elapsedMs} ms preview, rolled back).
              </span>
              <button type="button" className={`${BUTTON} font-semibold text-red-700 dark:text-red-400`} disabled={busy} onClick={() => void commit()}>
                Commit
              </button>
              <button type="button" className={BUTTON} disabled={busy} onClick={() => void rollback()}>
                Rollback
              </button>
            </div>
          )}
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <input className={INPUT} placeholder="Save as..." value={saveName} onChange={(e) => setSaveName(e.target.value)} />
            <button
              type="button"
              className={BUTTON}
              disabled={!saveName.trim()}
              onClick={() => {
                const sql = editor.current?.get().trim() ?? "";
                if (sql) void putSaved(upsertSaved(saved, { name: saveName.trim(), sql }));
                setSaveName("");
              }}
            >
              Save
            </button>
            {saved.map((q) => (
              <span key={q.name} className="flex items-center rounded border border-zinc-300 dark:border-zinc-700">
                <button type="button" className="px-2 py-0.5 hover:underline" title={q.sql} onClick={() => editor.current?.set(q.sql)}>
                  {q.name}
                </button>
                <button
                  type="button"
                  aria-label={`Delete ${q.name}`}
                  className="px-1 text-zinc-500 hover:text-red-600"
                  onClick={() => void putSaved(saved.filter((x) => x.name !== q.name))}
                >
                  x
                </button>
              </span>
            ))}
          </div>
          {plan && (
            <div className="rounded border border-zinc-200 p-2 font-mono text-xs dark:border-zinc-800">
              <p className="mb-1 font-sans text-sm text-zinc-500">Query plan</p>
              {flattenPlan(plan).map((n) => (
                <div key={n.row.id} style={{ paddingLeft: n.depth * 16 }}>
                  {n.depth > 0 ? "`-- " : ""}
                  {n.row.detail}
                </div>
              ))}
            </div>
          )}
          {result && !writeMode && <ResultGrid result={result} offset={offset} busy={busy} onPage={(at) => void goToPage(at / PAGE_SIZE)} />}
        </div>
      </div>
    </div>
  );
}
