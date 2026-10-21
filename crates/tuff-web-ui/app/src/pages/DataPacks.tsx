import { useCallback, useEffect, useState } from "react";
import { API, api, Session } from "../api";
import { DataPacksState, compatibilityLabel, formatRange, packUploadProblem, sortDataPacks, uploadFile } from "../packs";
import { Toast } from "../plugins";

const BUTTON =
  "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function DataPacks({ session, toast }: { session: Session; toast: Toast }) {
  const [state, setState] = useState<DataPacksState | null>(null);
  const [busy, setBusy] = useState(false);
  const [uploading, setUploading] = useState<number | null>(null);
  const [enableNew, setEnableNew] = useState(true);
  const [replace, setReplace] = useState(false);
  const manage = session.scopes.includes("datapacks.manage");

  const refresh = useCallback(async () => {
    try {
      setState(await api<DataPacksState>("/datapacks"));
    } catch (e) {
      toast(message(e), "error");
    }
  }, [toast]);

  useEffect(() => void refresh(), [refresh]);

  const act = async (path: string, init: RequestInit, done: string) => {
    setBusy(true);
    try {
      await api(path, init);
      toast(done);
    } catch (e) {
      toast(message(e), "error");
    } finally {
      setBusy(false);
      void refresh();
    }
  };

  const onUpload = async (file: File | undefined) => {
    if (!file || !state) return;
    const problem = packUploadProblem(file.name, file.size, state.maxUpload);
    if (problem) return toast(problem, "error");
    setUploading(0);
    const query = `filename=${encodeURIComponent(file.name)}${enableNew ? "&enable=1" : ""}${replace ? "&replace=1" : ""}`;
    try {
      const r = (await uploadFile(`/datapacks/upload?${query}`, file, session.csrf, setUploading)) as { id: string; enabled: boolean };
      toast(`Stored ${r.id}${r.enabled ? " and enabled it" : ""}`);
    } catch (e) {
      toast(`Upload refused: ${message(e)}`, "error");
    } finally {
      setUploading(null);
      void refresh();
    }
  };

  if (!state) return <p className="p-4 text-sm text-zinc-500">Loading data packs...</p>;
  const post = (body: object): RequestInit => ({ method: "POST", body: JSON.stringify(body) });
  return (
    <div className="flex flex-col gap-4 p-4">
      {manage && (
        <div className="flex flex-wrap items-center gap-3 text-sm">
          <label className={`${BUTTON} cursor-pointer ${uploading !== null ? "pointer-events-none opacity-40" : ""}`}>
            Upload pack...
            <input
              type="file"
              accept=".zip"
              className="hidden"
              onChange={(e) => {
                void onUpload(e.target.files?.[0]);
                e.target.value = "";
              }}
            />
          </label>
          <label className="flex items-center gap-1">
            <input type="checkbox" checked={enableNew} onChange={(e) => setEnableNew(e.target.checked)} /> enable after upload
          </label>
          <label className="flex items-center gap-1">
            <input type="checkbox" checked={replace} onChange={(e) => setReplace(e.target.checked)} /> replace a pack of the same name
          </label>
          <button type="button" className={BUTTON} disabled={busy} onClick={() => act("/datapacks/reload", post({}), "Data packs reloaded.")}>
            Reload
          </button>
          {uploading !== null && <span>{uploading < 100 ? `Uploading ${uploading}%` : "Checking..."}</span>}
        </div>
      )}
      <table className="w-full text-left text-sm">
        <thead className="text-zinc-500">
          <tr>
            <th className="py-1">Pack</th>
            <th>Description</th>
            <th>Format</th>
            <th>Enabled</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {sortDataPacks(state.packs).map((p) => (
            <tr key={p.id} className="border-t border-zinc-200 align-top dark:border-zinc-800">
              <td className="py-1 font-mono">
                {p.id}
                {p.kind && <span className="ml-1 text-xs text-zinc-500">({p.kind})</span>}
              </td>
              <td>
                {p.meta?.description}
                {p.error && <div className="text-red-600">{p.error}</div>}
                {p.meta && p.meta.missingFeatures.length > 0 && (
                  <div className="text-amber-600">needs features: {p.meta.missingFeatures.join(", ")}</div>
                )}
              </td>
              <td>
                {formatRange(p.meta?.formats)}{" "}
                {p.meta && p.meta.compatibility !== "compatible" && <span className="text-amber-600">{compatibilityLabel(p.meta.compatibility)}</span>}
              </td>
              <td>
                {manage && !p.required && !p.error ? (
                  <input
                    type="checkbox"
                    checked={p.selected}
                    disabled={busy}
                    onChange={() =>
                      act(
                        p.selected ? "/datapacks/disable" : "/datapacks/enable",
                        post({ id: p.id }),
                        `${p.id} ${p.selected ? "disabled" : "enabled"}.`,
                      )
                    }
                  />
                ) : p.selected ? (
                  "yes"
                ) : (
                  "no"
                )}
              </td>
              <td className="flex flex-wrap justify-end gap-1 py-1">
                {p.kind && (
                  <a className={BUTTON} href={`${API}/datapacks/${encodeURIComponent(p.name)}`} download>
                    Download
                  </a>
                )}
                {manage && p.source === "world" && (
                  <button
                    type="button"
                    className={BUTTON}
                    disabled={busy}
                    onClick={() =>
                      window.confirm(`Delete ${p.id} from the world?`) &&
                      act(`/datapacks/${encodeURIComponent(p.name)}`, { method: "DELETE" }, `${p.id} removed.`)
                    }
                  >
                    Delete
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
