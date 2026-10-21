import { ReactNode, useCallback, useEffect, useState } from "react";
import { api, ApiError } from "../api";
import { Toast } from "../plugins";
import { MiniMessageField } from "../MiniMessageField";
import {
  Bounds,
  ConfigState,
  GameRule,
  buildTree,
  TreeNode,
  invalid,
  isDefault,
  matches,
  Property,
  replaceEntry,
  savedMessage,
  Setting,
  textFormat,
} from "../config";

const BUTTON =
  "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";
const INPUT = "w-full rounded border border-zinc-300 bg-transparent px-2 py-1 font-mono text-sm dark:border-zinc-700";
const TABS = ["settings", "server.properties", "game rules"] as const;
type Tab = (typeof TABS)[number];

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

type Send = (path: string, body: Record<string, unknown>) => Promise<unknown | null>;

interface EditorProps {
  kind: string;
  value: string;
  bounds: Bounds;
  secret: boolean;
  set?: boolean;
  disabled: boolean;
  onSave: (text: string) => void;
}

function Editor({ kind, value, bounds, secret, set, disabled, onSave }: EditorProps) {
  const [draft, setDraft] = useState(secret ? "" : value);
  useEffect(() => setDraft(secret ? "" : value), [value, secret]);
  if (kind === "bool" && !secret) {
    return (
      <input
        type="checkbox"
        checked={value === "true"}
        disabled={disabled}
        onChange={(e) => onSave(e.target.checked ? "true" : "false")}
      />
    );
  }
  const problem = secret && draft === "" ? null : invalid(kind, draft, bounds);
  const changed = secret ? draft !== "" : draft !== value;
  return (
    <form
      className="flex items-center gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (changed && !problem) onSave(draft);
      }}
    >
      {kind === "choice" && bounds.options ? (
        <select className={INPUT} value={draft} disabled={disabled} onChange={(e) => setDraft(e.target.value)}>
          {bounds.options.map((o) => (
            <option key={o} value={o}>
              {o}
            </option>
          ))}
        </select>
      ) : (
        <input
          className={INPUT}
          type={secret ? "password" : "text"}
          autoComplete="off"
          value={draft}
          placeholder={secret ? (set ? "set (type to replace)" : "not set") : ""}
          disabled={disabled}
          onChange={(e) => setDraft(e.target.value)}
        />
      )}
      <button type="submit" className={BUTTON} disabled={disabled || !changed || problem !== null} title={problem ?? ""}>
        Save
      </button>
    </form>
  );
}

function ResetButton({ show, disabled, onReset, label = "Reset" }: { show: boolean; disabled: boolean; onReset: () => void; label?: string }) {
  if (!show) return null;
  return (
    <button type="button" className={BUTTON} disabled={disabled} onClick={onReset}>
      {label}
    </button>
  );
}

export function Config({ toast }: { toast: Toast }) {
  const [state, setState] = useState<ConfigState | null>(null);
  const [tab, setTab] = useState<Tab>("settings");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState<Set<string>>(() => {
    try {
      return new Set<string>(JSON.parse(localStorage.getItem("config.open") ?? "[]"));
    } catch {
      return new Set<string>();
    }
  });
  const toggle = (path: string) =>
    setOpen((old) => {
      const next = new Set(old);
      if (!next.delete(path)) next.add(path);
      try {
        localStorage.setItem("config.open", JSON.stringify([...next]));
      } catch {
        /* storage unavailable */
      }
      return next;
    });

  const refresh = useCallback(async () => {
    try {
      setState(await api<ConfigState>("/config"));
    } catch (e) {
      toast(message(e), "error");
    }
  }, [toast]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const send: Send = async (path, body) => {
    setBusy(true);
    try {
      const put = (b: Record<string, unknown>) => api<unknown>(`/config/${path}`, { method: "PUT", body: JSON.stringify(b) });
      try {
        return await put(body);
      } catch (e) {
        if (e instanceof ApiError && e.status === 409 && window.confirm(`${e.message}?`)) {
          return await put({ ...body, confirm: true });
        }
        throw e;
      }
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 409)) toast(message(e), "error");
      return null;
    } finally {
      setBusy(false);
    }
  };

  const saveSetting = async (s: Setting, body: Record<string, unknown>) => {
    const next = (await send(s.name, body)) as Setting | null;
    if (next) {
      setState((old) => (old ? { ...old, settings: replaceEntry(old.settings, next) } : old));
      toast(`Saved ${s.name}.`);
    }
  };
  const saveProperty = async (p: Property, body: Record<string, unknown>) => {
    const next = (await send(`properties/${p.name}`, body)) as Property | null;
    if (next) {
      setState((old) => (old ? { ...old, properties: replaceEntry(old.properties, next) } : old));
      toast(savedMessage(next));
    }
  };
  const saveRule = async (r: GameRule, body: Record<string, unknown>) => {
    const next = (await send(`gamerules/${r.name}`, body)) as GameRule | null;
    if (next) {
      setState((old) => (old ? { ...old, gameRules: replaceEntry(old.gameRules, next) } : old));
      toast(`Set ${r.name} to ${next.value}.`);
    }
  };

  if (!state) return <p className="p-4 text-sm text-zinc-500">Loading settings...</p>;
  const locked = busy || !state.writable;

  const settingRow = (s: Setting) => (
    <div key={s.name} className="grid gap-2 border-t border-zinc-200 py-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] dark:border-zinc-800">
      <div className="min-w-0">
        <div className="font-mono text-sm break-all">
          {s.name}
          {s.lockout && <span className="ml-2 text-xs text-amber-600">panel access</span>}
        </div>
        <div className="text-xs text-zinc-500">
          {s.description}
          {` (${s.kind}`}
          {s.secret ? ", secret" : `, default '${s.default}'`}
          {s.runtimeOnly ? ", this run only" : ""}
          {s.feature !== "core" ? `, ${s.feature}` : ""})
        </div>
      </div>
      {textFormat(s) ? (
        <MiniMessageField
          format={textFormat(s) ?? undefined}
          label={s.name}
          value={s.value ?? ""}
          onCommit={(value) => {
            if (!locked && value !== (s.value ?? "")) void saveSetting(s, { value });
          }}
        />
      ) : (
        <Editor
          kind={s.kind}
          value={s.value ?? ""}
          bounds={{}}
          secret={s.secret}
          set={s.set}
          disabled={locked}
          onSave={(value) => void saveSetting(s, { value })}
        />
      )}
      <ResetButton
        show={!isDefault(s)}
        disabled={locked}
        label={s.secret ? "Clear" : "Reset"}
        onReset={() => void saveSetting(s, { reset: true })}
      />
    </div>
  );

  const searching = query.trim() !== "";
  const branch = (node: TreeNode<Setting>): ReactNode => {
    const expanded = searching || open.has(node.path);
    return (
      <div key={node.path} className="flex flex-col">
        {node.children.length > 0 && (
          <button
            type="button"
            className="flex items-center gap-2 py-1 text-left font-mono text-sm font-semibold text-zinc-500"
            aria-expanded={expanded}
            onClick={() => toggle(node.path)}
          >
            <span>{expanded ? "v" : ">"}</span>
            <span>{node.segment}</span>
            <span className="text-xs font-normal">({node.count})</span>
          </button>
        )}
        {node.entry && settingRow(node.entry)}
        {node.children.length > 0 && expanded && <div className="ml-4 flex flex-col border-l border-zinc-200 pl-2 dark:border-zinc-800">{node.children.map(branch)}</div>}
      </div>
    );
  };
  const settings = () => buildTree(state.settings.filter((s) => matches(s, query))).map(branch);

  const properties = () =>
    state.properties
      .filter((p) => matches(p, query))
      .map((p) => (
        <div key={p.name} className="grid gap-2 border-t border-zinc-200 py-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] dark:border-zinc-800">
          <div className="min-w-0">
            <div className="font-mono text-sm break-all">{p.name}</div>
            <div className="text-xs text-zinc-500">
              {p.known ? `${p.kind}${p.min !== undefined ? ` ${p.min} to ${p.max}` : ""}` : "not defined by the game"}
              {p.default !== undefined && p.default !== null ? `, default '${p.default}'` : ""}, {p.effectNote}
            </div>
          </div>
          {textFormat(p) ? (
            <MiniMessageField
              format={textFormat(p) ?? undefined}
              label="motd"
              value={p.value ?? ""}
              onCommit={(value) => {
                if (!locked && value !== (p.value ?? "")) void saveProperty(p, { value });
              }}
            />
          ) : (
          <Editor
            kind={p.kind}
            value={p.value ?? ""}
            bounds={p}
            secret={p.secret}
            set={p.set}
            disabled={locked}
            onSave={(value) => void saveProperty(p, { value })}
          />
          )}
          <ResetButton
            show={p.known && !isDefault(p)}
            disabled={locked}
            label={p.secret ? "Clear" : "Reset"}
            onReset={() => void saveProperty(p, { reset: true })}
          />
        </div>
      ));

  const rules = () =>
    state.gameRules
      .filter((r) => matches(r, query))
      .map((r) => (
        <div key={r.name} className="grid gap-2 border-t border-zinc-200 py-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] dark:border-zinc-800">
          <div className="min-w-0">
            <div className="font-mono text-sm break-all">{r.name}</div>
            <div className="text-xs text-zinc-500">
              {r.kind}
              {r.min !== undefined ? ` ${r.min} to ${r.max}` : ""}, default {r.default}
            </div>
          </div>
          <Editor
            kind={r.kind}
            value={r.value ?? ""}
            bounds={r}
            secret={false}
            disabled={locked}
            onSave={(value) => void saveRule(r, { value })}
          />
          <ResetButton show={r.value !== r.default} disabled={locked} onReset={() => void saveRule(r, { reset: true })} />
        </div>
      ));

  return (
    <div className="flex flex-col gap-4 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <nav className="flex flex-wrap gap-1">
          {TABS.map((t) => (
            <button key={t} type="button" className={`${BUTTON} ${t === tab ? "bg-zinc-200 dark:bg-zinc-800" : ""}`} onClick={() => setTab(t)}>
              {t}
            </button>
          ))}
        </nav>
        <input className={`${INPUT} max-w-xs`} value={query} onChange={(e) => setQuery(e.target.value)} placeholder="search" />
        <button type="button" className={BUTTON} disabled={busy} onClick={() => void refresh()}>
          Refresh
        </button>
        {!state.writable && <span className="text-sm text-zinc-500">Read only: you may not change settings.</span>}
      </div>
      {tab === "settings" && settings()}
      {tab === "server.properties" && <div className="flex flex-col">{properties()}</div>}
      {tab === "game rules" && <div className="flex flex-col">{rules()}</div>}
    </div>
  );
}
