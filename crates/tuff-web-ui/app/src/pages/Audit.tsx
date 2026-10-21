import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api";
import { subscribe } from "../events";
import { AuditEntry, AuditFilter, auditQuery, prependLive } from "../access";

const PAGE = 100;
const INPUT = "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm dark:border-zinc-700";
const BUTTON = "rounded px-3 py-1 text-sm hover:bg-zinc-100 dark:hover:bg-zinc-900";

export function Audit() {
  const [entries, setEntries] = useState<AuditEntry[]>([]);
  const [error, setError] = useState("");
  const [more, setMore] = useState(true);
  const [draft, setDraft] = useState<AuditFilter>({ principal: "", action: "" });
  const [filter, setFilter] = useState<AuditFilter>({ principal: "", action: "" });
  const filterRef = useRef(filter);
  filterRef.current = filter;

  const load = useCallback(
    async (before?: number) => {
      try {
        const page = await api<{ entries: AuditEntry[] }>(auditQuery(filter, PAGE, before));
        setEntries((old) => (before === undefined ? page.entries : [...old, ...page.entries]));
        setMore(page.entries.length === PAGE);
        setError("");
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [filter],
  );

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(
    () =>
      subscribe(["audit"], ["audit"], (event, data) => {
        if (event === "audit") setEntries((old) => prependLive(old, data, filterRef.current));
      }),
    [],
  );

  return (
    <div className="overflow-auto p-4">
      <form
        className="mb-3 flex flex-wrap gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          setFilter({ ...draft });
        }}
      >
        <input className={INPUT} placeholder="who" value={draft.principal} onChange={(e) => setDraft({ ...draft, principal: e.target.value })} />
        <input className={INPUT} placeholder="action, or prefix like config." value={draft.action} onChange={(e) => setDraft({ ...draft, action: e.target.value })} />
        <button type="submit" className={BUTTON}>
          Filter
        </button>
      </form>
      {error && <p className="text-red-600">{error}</p>}
      <table className="w-full text-left text-sm">
        <thead className="text-zinc-500">
          <tr>
            <th className="py-1 pr-3">Time</th>
            <th className="py-1 pr-3">Who</th>
            <th className="py-1 pr-3">Action</th>
            <th className="py-1 pr-3">Target</th>
            <th className="py-1 pr-3">Detail</th>
            <th className="py-1 pr-3">Outcome</th>
          </tr>
        </thead>
        <tbody className="font-mono">
          {entries.map((e, i) => (
            <tr key={`${e.ts}-${i}`} className="border-t border-zinc-200 dark:border-zinc-800">
              <td className="py-1 pr-3 whitespace-nowrap">{new Date(e.ts).toLocaleString()}</td>
              <td className="py-1 pr-3">
                {e.principal} <span className="text-zinc-500">{e.remote}</span>
              </td>
              <td className="py-1 pr-3">{e.action}</td>
              <td className="py-1 pr-3 break-all">{e.target}</td>
              <td className="py-1 pr-3 break-all">{e.detail}</td>
              <td className="py-1 pr-3">{e.outcome}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {more && entries.length > 0 && (
        <button type="button" onClick={() => load(entries[entries.length - 1].ts)} className={`mt-3 ${BUTTON}`}>
          Older
        </button>
      )}
    </div>
  );
}
