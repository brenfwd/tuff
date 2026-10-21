import { useCallback, useEffect, useState } from "react";
import { API, api, Session } from "../api";
import { formatBytes } from "../backups";
import { ResourcePack, ResourcePacksState, compatibilityLabel, formatRange, kindLabel, packUploadProblem, statusLabel, uploadFile } from "../packs";
import { Toast } from "../plugins";

const BUTTON =
  "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";
const INPUT = "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm dark:border-zinc-700";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function Settings({ pack, orders, busy, save }: { pack: ResourcePack; orders: [number, number]; busy: boolean; save: (body: object) => void }) {
  const [name, setName] = useState(pack.name);
  const [prompt, setPrompt] = useState(pack.prompt ?? "");
  const [order, setOrder] = useState(String(pack.order ?? 0));
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm">
      <input className={INPUT} value={name} onChange={(e) => setName(e.target.value)} placeholder="name" />
      <input className={INPUT} value={prompt} onChange={(e) => setPrompt(e.target.value)} placeholder="prompt shown to players" />
      <label className="flex items-center gap-1">
        order
        <input className={`${INPUT} w-20`} type="number" min={orders[0]} max={orders[1]} value={order} onChange={(e) => setOrder(e.target.value)} />
      </label>
      <button
        type="button"
        className={BUTTON}
        disabled={busy}
        onClick={() => save({ name, prompt: prompt.trim() ? prompt : null, order: Number.parseInt(order, 10) || 0 })}
      >
        Save
      </button>
    </div>
  );
}

export function ResourcePacks({ session, toast }: { session: Session; toast: Toast }) {
  const [state, setState] = useState<ResourcePacksState | null>(null);
  const [busy, setBusy] = useState(false);
  const [uploading, setUploading] = useState<number | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const manage = session.scopes.includes("resourcepacks.manage");

  const refresh = useCallback(async () => {
    try {
      setState(await api<ResourcePacksState>("/resourcepacks"));
    } catch (e) {
      toast(message(e), "error");
    }
  }, [toast]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 5000);
    return () => clearInterval(timer);
  }, [refresh]);

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
  const post = (body: object): RequestInit => ({ method: "POST", body: JSON.stringify(body) });

  const onUpload = async (file: File | undefined) => {
    if (!file || !state) return;
    const problem = packUploadProblem(file.name, file.size, state.maxUpload);
    if (problem) return toast(problem, "error");
    setUploading(0);
    try {
      await uploadFile(`/resourcepacks/upload?filename=${encodeURIComponent(file.name)}`, file, session.csrf, setUploading);
      toast(`Uploaded ${file.name}; it is offered to players as they join.`);
    } catch (e) {
      toast(`Upload refused: ${message(e)}`, "error");
    } finally {
      setUploading(null);
      void refresh();
    }
  };

  if (!state) return <p className="p-4 text-sm text-zinc-500">Loading resource packs...</p>;
  const names = new Map(state.packs.map((p) => [p.id, p.name]));
  return (
    <div className="flex flex-col gap-4 p-4">
      {!state.enabled && (
        <p className="rounded border border-amber-400 p-2 text-sm">
          Resource packs are switched off (tuff.resourcePacks.enabled); only essential packs are offered.
        </p>
      )}
      {manage && state.fileArea && (
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
          {uploading !== null && <span>{uploading < 100 ? `Uploading ${uploading}%` : "Checking..."}</span>}
          <span className="text-zinc-500">Packs are offered at join in ascending order; the custom content pack always comes last.</span>
        </div>
      )}
      <table className="w-full text-left text-sm">
        <thead className="text-zinc-500">
          <tr>
            <th className="py-1">Pack</th>
            <th>Kind</th>
            <th>Size</th>
            <th>Required</th>
            <th>On join</th>
            <th>Holders</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {state.packs.map((p) => (
            <tr key={p.id} className="border-t border-zinc-200 align-top dark:border-zinc-800">
              <td className="py-1">
                <div>{p.name}</div>
                <div className="font-mono text-xs text-zinc-500">{p.sha1 ?? p.state}</div>
                {p.description && <div className="text-xs">{p.description}</div>}
                {p.formats && (
                  <div className="text-xs text-zinc-500">
                    format {formatRange(p.formats)} {p.compatibility !== "compatible" && compatibilityLabel(p.compatibility)}
                  </div>
                )}
                {editing === p.id && (
                  <Settings
                    pack={p}
                    orders={state.orders}
                    busy={busy}
                    save={(body) => {
                      setEditing(null);
                      void act(`/resourcepacks/${p.id}/settings`, post(body), "Settings saved.");
                    }}
                  />
                )}
              </td>
              <td>{kindLabel(p.kind)}</td>
              <td>{p.size !== null ? formatBytes(p.size) : ""}</td>
              <td>
                {manage && p.editable ? (
                  <input type="checkbox" checked={p.required} disabled={busy} onChange={() => act(`/resourcepacks/${p.id}/settings`, post({ required: !p.required }), "Saved.")} />
                ) : p.required ? (
                  "yes"
                ) : (
                  "no"
                )}
              </td>
              <td>
                {manage && p.editable ? (
                  <input
                    type="checkbox"
                    checked={p.offerOnJoin}
                    disabled={busy}
                    onChange={() => act(`/resourcepacks/${p.id}/settings`, post({ offerOnJoin: !p.offerOnJoin }), "Saved.")}
                  />
                ) : p.offerOnJoin ? (
                  "yes"
                ) : (
                  "no"
                )}
              </td>
              <td>{p.holders}</td>
              <td className="flex flex-wrap justify-end gap-1 py-1">
                <a className={BUTTON} href={`${API}/resourcepacks/${p.id}`} download>
                  Download
                </a>
                {manage && (
                  <button type="button" className={BUTTON} disabled={busy || p.state !== "ready"} onClick={() => act(`/resourcepacks/${p.id}/push`, post({}), `Pushed ${p.name} to everyone online.`)}>
                    Push to all
                  </button>
                )}
                {manage && !p.essential && (
                  <button type="button" className={BUTTON} disabled={busy} onClick={() => act(`/resourcepacks/${p.id}/pop`, post({}), `Removed ${p.name} from everyone online.`)}>
                    Pop from all
                  </button>
                )}
                {manage && p.editable && (
                  <>
                    <button type="button" className={BUTTON} onClick={() => setEditing(editing === p.id ? null : p.id)}>
                      Settings
                    </button>
                    <button
                      type="button"
                      className={BUTTON}
                      disabled={busy}
                      onClick={() => window.confirm(`Delete ${p.name}?`) && act(`/resourcepacks/${p.id}`, { method: "DELETE" }, `${p.name} deleted.`)}
                    >
                      Delete
                    </button>
                  </>
                )}
              </td>
            </tr>
          ))}
          {state.packs.length === 0 && (
            <tr>
              <td colSpan={7} className="py-2 text-zinc-500">
                No resource packs are hosted.
              </td>
            </tr>
          )}
        </tbody>
      </table>
      <h2 className="font-semibold">Online players</h2>
      <table className="w-full text-left text-sm">
        <tbody>
          {state.players.map((pl) => (
            <tr key={pl.uuid} className="border-t border-zinc-200 align-top dark:border-zinc-800">
              <td className="py-1">{pl.name}</td>
              <td>
                {pl.packs.length === 0
                  ? "no packs"
                  : pl.packs.map((pk) => (
                      <div key={pk.id}>
                        {names.get(pk.id) ?? pk.id}: {statusLabel(pk)}
                        {manage && state.packs.some((p) => p.id === pk.id && !p.essential) && (
                          <button type="button" className={`${BUTTON} ml-2`} disabled={busy} onClick={() => act(`/resourcepacks/${pk.id}/pop`, post({ player: pl.uuid }), "Removed.")}>
                            Pop
                          </button>
                        )}
                      </div>
                    ))}
              </td>
              <td className="flex flex-wrap justify-end gap-1 py-1">
                {manage &&
                  state.packs
                    .filter((p) => p.state === "ready" && !pl.packs.some((pk) => pk.id === p.id))
                    .map((p) => (
                      <button key={p.id} type="button" className={BUTTON} disabled={busy} onClick={() => act(`/resourcepacks/${p.id}/push`, post({ player: pl.uuid }), `Pushed ${p.name}.`)}>
                        Push {p.name}
                      </button>
                    ))}
              </td>
            </tr>
          ))}
          {state.players.length === 0 && (
            <tr>
              <td className="py-2 text-zinc-500">Nobody is online.</td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}
