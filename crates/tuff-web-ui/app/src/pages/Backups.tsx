import { useCallback, useEffect, useState } from "react";
import { API, api, Session } from "../api";
import {
  BackupsState,
  confirmed,
  errorText,
  formatBytes,
  percent,
  phaseLabel,
  restorable,
  Running,
  uploadProblem,
} from "../backups";
import { subscribe } from "../events";
import { Toast } from "../plugins";

const BUTTON =
  "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";
const INPUT = "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm dark:border-zinc-700";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function Bar({ value }: { value: number }) {
  return (
    <div className="h-2 w-48 overflow-hidden rounded bg-zinc-200 dark:bg-zinc-800">
      <div className="h-full bg-emerald-600" style={{ width: `${value}%` }} />
    </div>
  );
}

function upload(file: File, csrf: string, onProgress: (p: number) => void): Promise<string> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("PUT", `${API}/backups/upload?filename=${encodeURIComponent(file.name)}`);
    xhr.setRequestHeader("x-tuff-csrf", csrf);
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(percent(e.loaded, e.total));
    xhr.onload = () => {
      if (xhr.status === 200) resolve(JSON.parse(xhr.responseText).backup.name);
      else reject(new Error(errorText(xhr.status, xhr.responseText)));
    };
    xhr.onerror = () => reject(new Error(errorText(0, "")));
    xhr.send(file);
  });
}

export function Backups({ session, toast }: { session: Session; toast: Toast }) {
  const [state, setState] = useState<BackupsState | null>(null);
  const [running, setRunning] = useState<Running | null>(null);
  const [restoreProgress, setRestoreProgress] = useState<number | null>(null);
  const [uploading, setUploading] = useState<number | null>(null);
  const [restoring, setRestoring] = useState<string | null>(null);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [withCaches, setWithCaches] = useState(false);
  const manage = session.scopes.includes("backups.manage");
  const mayRestore = session.scopes.includes("backups.restore");

  const refresh = useCallback(async () => {
    try {
      const s = await api<BackupsState>("/backups");
      setState(s);
      setRunning(s.status.running);
    } catch (e) {
      toast(message(e), "error");
    }
  }, [toast]);

  useEffect(() => {
    void refresh();
    return subscribe(["jobs"], ["progress", "finished", "staged"], (event, data) => {
      if (event === "resync") return void refresh();
      const parsed = JSON.parse(data);
      if (parsed.kind === "backup" && event === "progress") setRunning(parsed.running);
      else if (parsed.kind === "restore" && event === "progress") {
        setRestoreProgress(percent(parsed.progress.bytes_done, parsed.progress.bytes_total));
      } else {
        if (event === "finished" && parsed.finished?.error) toast(`Backup failed: ${parsed.finished.error}`, "error");
        void refresh();
      }
    });
  }, [refresh, toast]);

  const act = async (path: string, init: RequestInit, done?: string) => {
    setBusy(true);
    try {
      await api(path, init);
      if (done) toast(done);
    } catch (e) {
      toast(message(e), "error");
    } finally {
      setBusy(false);
      void refresh();
    }
  };

  const onUpload = async (file: File | undefined) => {
    if (!file || !state) return;
    const problem = uploadProblem(file.size, state.maxUpload);
    if (problem) return toast(problem, "error");
    setUploading(0);
    try {
      const name = await upload(file, session.csrf, setUploading);
      toast(`Uploaded and checked as ${name}`);
    } catch (e) {
      toast(`Upload refused: ${message(e)}`, "error");
    } finally {
      setUploading(null);
      void refresh();
    }
  };

  const stage = async () => {
    if (!restoring || !state) return;
    setRestoreProgress(0);
    await act(
      "/backups/restore",
      { method: "POST", body: JSON.stringify({ name: restoring, confirm: typed.trim() }) },
      "Restore staged; it is applied when the server next starts.",
    );
    setRestoreProgress(null);
    setRestoring(null);
    setTyped("");
  };

  if (!state) return <p className="p-4 text-sm text-zinc-500">Loading backups...</p>;
  const p = running?.progress;
  return (
    <div className="flex flex-col gap-4 p-4">
      {state.staged && (
        <div className="flex flex-wrap items-center gap-2 rounded border border-amber-400 bg-amber-50 p-3 text-sm dark:bg-amber-950">
          <span className="flex-1">
            Restore of <span className="font-mono">{state.staged.backup}</span> staged by {state.staged.by}. It replaces the world the next time the
            server starts; the current world is kept as <span className="font-mono">{state.level}-before-restore-...</span>.
          </span>
          {mayRestore && (
            <>
              <button
                type="button"
                className={`${BUTTON} font-semibold text-red-700 dark:text-red-400`}
                disabled={busy}
                onClick={() => act("/backups/restore/shutdown", { method: "POST", body: "{}" }, "The server is shutting down.")}
              >
                Shut down now
              </button>
              <button type="button" className={BUTTON} disabled={busy} onClick={() => act("/backups/restore", { method: "DELETE" }, "Restore cancelled.")}>
                Cancel restore
              </button>
            </>
          )}
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {manage && (
          <>
            <button type="button" className={BUTTON} disabled={busy || !!running || !state.world} onClick={() => act("/backups/create", { method: "POST", body: JSON.stringify({ includeCaches: withCaches }) })}>
              Back up now
            </button>
            <label className="flex items-center gap-1">
              <input type="checkbox" checked={withCaches} onChange={(e) => setWithCaches(e.target.checked)} /> include map cache
            </label>
            <button type="button" className={BUTTON} disabled={busy || !running} onClick={() => act("/backups/cancel", { method: "POST", body: "{}" })}>
              Cancel
            </button>
            <label className={`${BUTTON} cursor-pointer ${uploading !== null ? "pointer-events-none opacity-40" : ""}`}>
              Upload...
              <input
                type="file"
                accept=".zip,.mcserver"
                className="hidden"
                onChange={(e) => {
                  void onUpload(e.target.files?.[0]);
                  e.target.value = "";
                }}
              />
            </label>
          </>
        )}
        {running && p && (
          <span className="flex items-center gap-2 text-sm">
            {running.name}: {phaseLabel(p.phase)} <Bar value={percent(p.bytes_done, p.bytes_total)} /> {formatBytes(p.bytes_done)} / {formatBytes(p.bytes_total)}
          </span>
        )}
        {uploading !== null && (
          <span className="flex items-center gap-2 text-sm">
            {uploading < 100 ? "Uploading" : "Checking"} <Bar value={uploading} />
          </span>
        )}
        {!running && state.status.last && (
          <span className="text-sm text-zinc-500">
            Last: {state.status.last.name}{" "}
            {state.status.last.error ? <span className="text-red-600">failed: {state.status.last.error}</span> : `(${formatBytes(state.status.last.size)})`}
          </span>
        )}
      </div>
      <table className="w-full text-left text-sm">
        <thead className="text-zinc-500">
          <tr>
            <th className="py-1">Backup</th>
            <th>Size</th>
            <th>Created</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {state.backups.map((b) => (
            <tr key={b.name} className="border-t border-zinc-200 dark:border-zinc-800">
              <td className="py-1 font-mono">{b.name}</td>
              <td>{formatBytes(b.size)}</td>
              <td>{new Date(b.created_unix * 1000).toLocaleString()}</td>
              <td className="flex flex-wrap justify-end gap-1 py-1">
                <a className={BUTTON} href={`${API}/backups/${encodeURIComponent(b.name)}`} download>
                  Download
                </a>
                {mayRestore && restorable(state.world, b.name) && (
                  <button type="button" className={BUTTON} disabled={busy} onClick={() => setRestoring(b.name)}>
                    Restore...
                  </button>
                )}
                {manage && (
                  <button
                    type="button"
                    className={BUTTON}
                    disabled={busy}
                    onClick={() => window.confirm(`Delete ${b.name}?`) && act(`/backups/${encodeURIComponent(b.name)}`, { method: "DELETE" })}
                  >
                    Delete
                  </button>
                )}
              </td>
            </tr>
          ))}
          {state.backups.length === 0 && (
            <tr>
              <td colSpan={4} className="py-2 text-zinc-500">
                No backups yet.
              </td>
            </tr>
          )}
        </tbody>
      </table>
      {restoring && (
        <div className="flex flex-col gap-2 rounded border border-red-400 p-3 text-sm">
          <p>
            Restoring <span className="font-mono">{restoring}</span> replaces the whole world at the next start. Type the level name{" "}
            <span className="font-mono font-semibold">{state.level}</span> to confirm.
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <input className={INPUT} value={typed} onChange={(e) => setTyped(e.target.value)} placeholder={state.level} />
            <button type="button" className={`${BUTTON} text-red-700 dark:text-red-400`} disabled={busy || !confirmed(typed, state.level)} onClick={() => void stage()}>
              Stage restore
            </button>
            <button type="button" className={BUTTON} disabled={busy} onClick={() => setRestoring(null)}>
              Close
            </button>
            {restoreProgress !== null && <Bar value={restoreProgress} />}
          </div>
        </div>
      )}
    </div>
  );
}
