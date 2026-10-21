import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../api";
import type { Toast } from "../plugins";
import { MiniMessageField, TextPreview, usePreview } from "../MiniMessageField";
import { cssColor } from "../minimessage";
import {
  ATTRIBUTES,
  effective,
  fallbackLabel,
  nodeTree,
  type Held,
  type NodeBranch,
  type PermChange,
  type PermNode,
  type PermRole,
  type PermState,
  validRole,
  validPattern,
} from "../permissions";

const input =
  "rounded border border-zinc-300 bg-white px-2 py-1 text-sm dark:border-zinc-700 dark:bg-zinc-900";
const button = "rounded px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-50 dark:hover:bg-zinc-800";
const section = "rounded border border-zinc-200 p-3 dark:border-zinc-800";

type Send = (change: PermChange, done: string) => Promise<boolean>;

function TriState({
  value,
  inherited,
  onSet,
  label,
}: {
  value: Held | undefined;
  inherited: string;
  onSet: (held: Held) => void;
  label: string;
}) {
  const opt = (held: Held, text: string, on: string) => (
    <button
      type="button"
      aria-pressed={value === held || (held === null && value === undefined)}
      aria-label={`${label} ${text}`}
      className={`px-2 py-0.5 text-xs ${
        value === held || (held === null && value === undefined) ? on : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
      }`}
      onClick={() => onSet(held)}
    >
      {text}
    </button>
  );
  return (
    <span className="flex items-center gap-2">
      <span className="inline-flex overflow-hidden rounded border border-zinc-300 dark:border-zinc-700">
        {opt(true, "Allow", "bg-green-600 text-white")}
        {opt(null, "Default", "bg-zinc-500 text-white")}
        {opt(false, "Deny", "bg-red-600 text-white")}
      </span>
      {value === undefined && <span className="text-xs text-zinc-500">{inherited}</span>}
    </span>
  );
}

function inheritedLabel(settings: Record<string, boolean>, pattern: string, node?: PermNode): string {
  const from = effective(settings, pattern);
  if (from) return `${from.held ? "allowed" : "denied"} by ${from.from}`;
  return node ? `default: ${fallbackLabel(node)}` : "";
}

function Branch({
  branch,
  settings,
  onSet,
  open,
  toggle,
  forceOpen,
}: {
  branch: NodeBranch;
  settings: Record<string, boolean>;
  onSet: (pattern: string, held: Held) => void;
  open: Set<string>;
  toggle: (path: string) => void;
  forceOpen: boolean;
}) {
  const expanded = forceOpen || open.has(branch.path);
  const wildcard = `${branch.path}.*`;
  return (
    <li>
      {branch.node && (
        <div className="flex flex-wrap items-center gap-2 py-0.5">
          <span className="w-4" />
          <span className="font-mono text-sm break-all" title={branch.node.description}>
            {branch.segment}
          </span>
          <span className="hidden flex-1 truncate text-xs text-zinc-500 md:inline">{branch.node.description}</span>
          <span className="flex-1 md:hidden" />
          <TriState
            label={branch.path}
            value={settings[branch.path]}
            inherited={inheritedLabel(settings, branch.path, branch.node)}
            onSet={(held) => onSet(branch.path, held)}
          />
        </div>
      )}
      {branch.children.length > 0 && (
        <>
          <div className="flex flex-wrap items-center gap-2 py-0.5">
            <button
              type="button"
              className="w-4 text-zinc-500"
              aria-expanded={expanded}
              aria-label={`${expanded ? "collapse" : "expand"} ${branch.path}`}
              onClick={() => toggle(branch.path)}
            >
              {expanded ? "v" : ">"}
            </button>
            <span className="font-mono text-sm font-semibold">{branch.segment}.*</span>
            <span className="text-xs text-zinc-500">{branch.count}</span>
            <span className="flex-1" />
            <TriState
              label={wildcard}
              value={settings[wildcard]}
              inherited={inheritedLabel(settings, wildcard)}
              onSet={(held) => onSet(wildcard, held)}
            />
          </div>
          {expanded && (
            <ul className="ml-4 border-l border-zinc-200 pl-2 dark:border-zinc-800">
              {branch.children.map((c) => (
                <Branch
                  key={c.path}
                  branch={c}
                  settings={settings}
                  onSet={onSet}
                  open={open}
                  toggle={toggle}
                  forceOpen={forceOpen}
                />
              ))}
            </ul>
          )}
        </>
      )}
    </li>
  );
}

/** Registered nodes as a collapsible tree with a three-state control per
 * node and per branch wildcard. */
function NodeTree({
  nodes,
  settings,
  onSet,
}: {
  nodes: PermNode[];
  settings: Record<string, boolean>;
  onSet: (pattern: string, held: Held) => void;
}) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [custom, setCustom] = useState("");
  const tree = useMemo(() => nodeTree(nodes, query), [nodes, query]);
  const registered = useMemo(() => new Set(nodes.map((n) => n.id)), [nodes]);
  const unregistered = Object.keys(settings)
    .filter((p) => !p.endsWith("*") && !registered.has(p))
    .sort();
  const toggle = (path: string) =>
    setOpen((old) => {
      const next = new Set(old);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  return (
    <div>
      <div className="mb-2 flex flex-wrap items-center gap-2">
        <input
          className={`${input} min-w-48 flex-1`}
          placeholder="search nodes"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <button type="button" className={button} onClick={() => setOpen(new Set())}>
          Collapse all
        </button>
      </div>
      <div className="flex flex-wrap items-center gap-2 py-0.5">
        <span className="w-4" />
        <span className="font-mono text-sm font-semibold">*</span>
        <span className="text-xs text-zinc-500">every node</span>
        <span className="flex-1" />
        <TriState
          label="*"
          value={settings["*"]}
          inherited="default per node"
          onSet={(held) => onSet("*", held)}
        />
      </div>
      <ul>
        {tree.map((b) => (
          <Branch
            key={b.path}
            branch={b}
            settings={settings}
            onSet={onSet}
            open={open}
            toggle={toggle}
            forceOpen={query.trim() !== ""}
          />
        ))}
      </ul>
      {unregistered.length > 0 && (
        <div className="mt-2 text-sm">
          <p className="text-zinc-500">Settings for nodes no plugin registers:</p>
          {unregistered.map((p) => (
            <div key={p} className="flex items-center gap-2">
              <span className="font-mono">{p}</span>
              <TriState label={p} value={settings[p]} inherited="" onSet={(held) => onSet(p, held)} />
            </div>
          ))}
        </div>
      )}
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <input
          className={`${input} min-w-48 flex-1 font-mono`}
          placeholder="other node or pattern.*"
          value={custom}
          onChange={(e) => setCustom(e.target.value)}
        />
        {[true, false].map((held) => (
          <button
            key={String(held)}
            type="button"
            className={button}
            disabled={!validPattern(custom.trim())}
            onClick={() => {
              onSet(custom.trim(), held);
              setCustom("");
            }}
          >
            {held ? "Allow" : "Deny"}
          </button>
        ))}
      </div>
    </div>
  );
}

function RoleName({ role }: { role: PermRole }) {
  const runs = usePreview(role.attributes.display_name ?? "");
  const color = role.attributes.name_color ? cssColor(role.attributes.name_color) : undefined;
  return (
    <span className="flex items-center gap-2">
      <span className="inline-block size-3 rounded-full border border-zinc-400" style={{ background: color ?? "transparent" }} />
      {role.attributes.display_name && runs ? <TextPreview runs={runs} /> : <span className="font-mono">{role.name}</span>}
    </span>
  );
}

function NameSample({ role }: { role: PermRole }) {
  const prefix = usePreview(role.attributes.prefix ?? "");
  const suffix = usePreview(role.attributes.suffix ?? "");
  const color = (role.attributes.name_color && cssColor(role.attributes.name_color)) || undefined;
  return (
    <div className="rounded bg-zinc-800/90 px-2 py-1 font-mono text-sm text-white">
      {prefix && <TextPreview runs={prefix} />}
      <span style={color ? { color } : undefined}>Steve</span>
      {suffix && <TextPreview runs={suffix} />}
      <span>: hello</span>
    </div>
  );
}

function RoleEditor({
  role,
  state,
  send,
}: {
  role: PermRole;
  state: PermState;
  send: Send;
}) {
  const [tab, setTab] = useState<"display" | "permissions" | "members">("display");
  const [member, setMember] = useState("");
  const isDefault = role.name === state.default_role;
  const setAttr = (key: (typeof ATTRIBUTES)[number]["key"], value: string) => {
    const next = value.trim() === "" ? null : value;
    if ((role.attributes[key] ?? null) === next) return;
    void send(
      { kind: "attribute", role: role.name, attribute: key, value: next },
      next === null ? `Cleared ${role.name}'s ${key}` : `Set ${role.name}'s ${key}`,
    );
  };
  const tabs = [
    ["display", "Display"],
    ["permissions", "Permissions"],
    ["members", `Members${isDefault ? "" : ` (${role.members.length})`}`],
  ] as const;
  return (
    <div className="flex-1 space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-lg font-semibold">
          <RoleName role={role} />
        </h3>
        <span className="font-mono text-xs text-zinc-500">{role.name}</span>
        <label className="text-sm text-zinc-500">
          priority{" "}
          <input
            key={`${role.name}-${role.priority}`}
            className={`${input} w-20`}
            type="number"
            defaultValue={role.priority}
            onBlur={(e) => {
              const priority = Number.parseInt(e.target.value, 10);
              if (!Number.isNaN(priority) && priority !== role.priority)
                void send({ kind: "priority", role: role.name, priority }, `Set ${role.name}'s priority`);
            }}
          />
        </label>
        <span className="flex-1" />
        <button
          type="button"
          className={`${button} text-red-700 dark:text-red-400`}
          onClick={() => {
            if (window.confirm(`Delete role ${role.name}?`))
              void send({ kind: "delete_role", role: role.name }, `Deleted role ${role.name}`);
          }}
        >
          Delete role
        </button>
      </div>
      <div className="flex gap-1 border-b border-zinc-200 dark:border-zinc-800" role="tablist">
        {tabs.map(([key, label]) => (
          <button
            key={key}
            type="button"
            role="tab"
            aria-selected={tab === key}
            className={`px-3 py-1 text-sm ${tab === key ? "border-b-2 border-violet-600 font-semibold" : "text-zinc-500"}`}
            onClick={() => setTab(key)}
          >
            {label}
          </button>
        ))}
      </div>
      {tab === "display" && (
        <div className="space-y-3">
          {ATTRIBUTES.filter((a) => a.key !== "name_color").map((a) => (
            <label key={`${role.name}-${a.key}`} className="block text-sm">
              <span className="mb-1 block text-zinc-500">{a.label} (MiniMessage)</span>
              <MiniMessageField
                value={role.attributes[a.key] ?? ""}
                placeholder={a.key === "display_name" ? role.name : "none"}
                label={a.label}
                onCommit={(v) => setAttr(a.key, v)}
              />
            </label>
          ))}
          <label className="flex flex-wrap items-center gap-2 text-sm">
            <span className="text-zinc-500">Name colour</span>
            <input
              key={`${role.name}-picker-${role.attributes.name_color ?? ""}`}
              type="color"
              aria-label="name colour picker"
              defaultValue={cssColor(role.attributes.name_color ?? "") ?? "#ffffff"}
              onChange={(e) => setAttr("name_color", e.target.value.toUpperCase())}
            />
            <input
              key={`${role.name}-color-${role.attributes.name_color ?? ""}`}
              className={`${input} w-36 font-mono`}
              placeholder="gold or #RRGGBB"
              defaultValue={role.attributes.name_color ?? ""}
              onBlur={(e) => setAttr("name_color", e.target.value.trim())}
            />
            {role.attributes.name_color && (
              <button type="button" className={button} onClick={() => setAttr("name_color", "")}>
                Clear
              </button>
            )}
          </label>
          <div className="text-sm">
            <span className="mb-1 block text-zinc-500">In chat and the player list</span>
            <NameSample role={role} />
          </div>
        </div>
      )}
      {tab === "permissions" && (
        <NodeTree
          nodes={state.nodes}
          settings={role.nodes}
          onSet={(node, held) =>
            void send(
              { kind: "role", role: role.name, node, held },
              held === null ? `Cleared ${node} on ${role.name}` : `${held ? "Allowed" : "Denied"} ${node} for ${role.name}`,
            )
          }
        />
      )}
      {tab === "members" &&
        (isDefault ? (
          <p className="text-sm text-zinc-500">Every player is in this role.</p>
        ) : (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {role.members.length === 0 && <span className="text-zinc-500">No members</span>}
            {role.members.map((m) => (
              <span key={m} className="rounded bg-zinc-100 px-2 dark:bg-zinc-800">
                {m}
                <button
                  type="button"
                  className="ml-1 text-zinc-500"
                  aria-label={`remove ${m}`}
                  onClick={() =>
                    void send({ kind: "member", player: m, role: role.name, member: false }, `Removed ${m} from ${role.name}`)
                  }
                >
                  x
                </button>
              </span>
            ))}
            <input
              className={input}
              placeholder="player name or uuid"
              value={member}
              onChange={(e) => setMember(e.target.value)}
            />
            <button
              type="button"
              className={button}
              disabled={!member.trim()}
              onClick={async () => {
                const name = member.trim();
                if (await send({ kind: "member", player: name, role: role.name, member: true }, `Added ${name} to ${role.name}`))
                  setMember("");
              }}
            >
              Add member
            </button>
          </div>
        ))}
    </div>
  );
}

export function Permissions({ toast }: { toast: Toast }) {
  const [state, setState] = useState<PermState | null>(null);
  const [error, setError] = useState("");
  const [newRole, setNewRole] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [player, setPlayer] = useState<string | null>(null);
  const [newPlayer, setNewPlayer] = useState("");

  const load = useCallback(async () => {
    try {
      setState(await api<PermState>("/permissions"));
      setError("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const send: Send = useCallback(
    async (change, done) => {
      try {
        const r = await api<{ changed: boolean; state: PermState }>("/permissions/changes", {
          method: "POST",
          body: JSON.stringify(change),
        });
        setState(r.state);
        toast(r.changed ? done : "Nothing changed");
        return true;
      } catch (e) {
        toast(e instanceof Error ? e.message : String(e), "error");
        return false;
      }
    },
    [toast],
  );

  if (!state) return <div className="p-4">{error ? <p className="text-red-600">{error}</p> : "Loading..."}</div>;

  const role = state.roles.find((r) => r.name === selected) ?? state.roles[0];
  const shownPlayer = state.players.find((p) => p.uuid === player);

  return (
    <div className="space-y-4 p-4">
      <p className="text-sm text-zinc-500">
        A player's own setting wins, then their roles by priority (everyone is in the "{state.default_role}" role
        when it exists), then operator bypass, then the node's default. Role prefixes, suffixes and name colours
        decorate members' names in chat and the player list, taken from their highest priority role that sets each.
      </p>

      <section className={section}>
        <h2 className="mb-2 font-semibold">Roles</h2>
        <div className="flex flex-col gap-4 md:flex-row">
          <div className="w-full space-y-1 md:w-56">
            {state.roles.map((r) => (
              <button
                key={r.name}
                type="button"
                className={`flex w-full items-center justify-between rounded px-2 py-1 text-left text-sm ${
                  role?.name === r.name ? "bg-zinc-200 dark:bg-zinc-800" : "hover:bg-zinc-100 dark:hover:bg-zinc-900"
                }`}
                onClick={() => setSelected(r.name)}
              >
                <RoleName role={r} />
                <span className="text-xs text-zinc-500">{r.priority}</span>
              </button>
            ))}
            {state.roles.length === 0 && <p className="text-sm text-zinc-500">No roles.</p>}
            <div className="flex gap-1 pt-2">
              <input
                className={`${input} w-full font-mono`}
                placeholder="new role"
                value={newRole}
                onChange={(e) => setNewRole(e.target.value)}
              />
              <button
                type="button"
                className={button}
                disabled={!validRole(newRole.trim())}
                onClick={async () => {
                  const name = newRole.trim();
                  const top = Math.max(0, ...state.roles.map((r) => r.priority + 1));
                  if (await send({ kind: "create_role", role: name, priority: top }, `Created role ${name}`)) {
                    setNewRole("");
                    setSelected(name);
                  }
                }}
              >
                Create
              </button>
            </div>
          </div>
          {role && <RoleEditor key={role.name} role={role} state={state} send={send} />}
        </div>
      </section>

      <section className={section}>
        <h2 className="mb-2 font-semibold">Players</h2>
        <div className="flex flex-col gap-4 md:flex-row">
          <div className="w-full space-y-1 md:w-56">
            {state.players.map((p) => (
              <button
                key={p.uuid}
                type="button"
                className={`block w-full rounded px-2 py-1 text-left text-sm ${
                  shownPlayer?.uuid === p.uuid ? "bg-zinc-200 dark:bg-zinc-800" : "hover:bg-zinc-100 dark:hover:bg-zinc-900"
                }`}
                onClick={() => setPlayer(p.uuid)}
              >
                {p.name || p.uuid}
                {p.roles.length > 0 && <span className="ml-1 text-xs text-zinc-500">{p.roles.join(", ")}</span>}
              </button>
            ))}
            {state.players.length === 0 && <p className="text-sm text-zinc-500">No player has settings.</p>}
            <div className="flex gap-1 pt-2">
              <input
                className={`${input} w-full`}
                placeholder="online player or uuid"
                value={newPlayer}
                onChange={(e) => setNewPlayer(e.target.value)}
              />
            </div>
          </div>
          <div className="flex-1">
            {shownPlayer ? (
              <>
                <h3 className="mb-2 font-semibold">
                  {shownPlayer.name} <span className="font-mono text-xs text-zinc-500">{shownPlayer.uuid}</span>
                </h3>
                <NodeTree
                  nodes={state.nodes}
                  settings={shownPlayer.nodes}
                  onSet={(node, held) =>
                    void send(
                      { kind: "player", player: shownPlayer.uuid, node, held },
                      held === null ? `Cleared ${node} for ${shownPlayer.name}` : `Set ${node} for ${shownPlayer.name}`,
                    )
                  }
                />
              </>
            ) : newPlayer.trim() ? (
              <NodeTree
                nodes={state.nodes}
                settings={{}}
                onSet={(node, held) =>
                  void send(
                    { kind: "player", player: newPlayer.trim(), node, held },
                    `Set ${node} for ${newPlayer.trim()}`,
                  ).then((ok) => ok && setNewPlayer(""))
                }
              />
            ) : (
              <p className="text-sm text-zinc-500">Pick a player, or type an online player's name to give them a setting.</p>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}
