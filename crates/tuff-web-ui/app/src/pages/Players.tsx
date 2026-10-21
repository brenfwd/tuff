import { useCallback, useEffect, useState } from "react";
import { api, Session } from "../api";
import { subscribe } from "../events";
import { Toast } from "../plugins";
import { applyEvent, formatPosition, GAME_MODES, OnlinePlayer, packLabel, PlayersState, validIp, validName, validText, Violations } from "../players";

const BUTTON =
  "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";
const INPUT = "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm dark:border-zinc-700";
const TABS = ["online", "offline", "ops", "whitelist", "bans", "ip bans"] as const;
type Tab = (typeof TABS)[number];

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

interface ActionReply {
  ok: boolean;
  feedback: string[];
  state: PlayersState | null;
}

export function Players({ toast }: { session: Session; toast: Toast }) {
  const [state, setState] = useState<PlayersState | null>(null);
  const [tab, setTab] = useState<Tab>("online");
  const [busy, setBusy] = useState(false);
  const [violations, setViolations] = useState<Map<string, Violations> | null>(null);
  const [name, setName] = useState("");
  const [ip, setIp] = useState("");

  const refresh = useCallback(async () => {
    try {
      setState(await api<PlayersState>("/players"));
    } catch (e) {
      toast(message(e), "error");
    }
    try {
      const r = await api<{ players: Violations[] }>("/plugins/anticheat");
      setViolations(new Map(r.players.map((p) => [p.uuid, p])));
    } catch {
      setViolations(null);
    }
  }, [toast]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 15000);
    const close = subscribe(["players"], ["join", "update", "leave"], (event, data) => {
      if (event === "resync") return void refresh();
      setState((s) => (s ? { ...s, online: applyEvent(s.online, event, data) } : s));
    });
    return () => {
      clearInterval(timer);
      close();
    };
  }, [refresh]);

  const act = async (body: Record<string, unknown>, confirmText?: string) => {
    if (confirmText && !window.confirm(confirmText)) return;
    setBusy(true);
    try {
      const reply = await api<ActionReply>("/players/actions", { method: "POST", body: JSON.stringify(body) });
      if (reply.state) setState(reply.state);
      toast(reply.feedback.join(" ") || (reply.ok ? "Done." : "Nothing changed."), reply.ok ? "info" : "error");
    } catch (e) {
      toast(message(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const ask = (prompt: string): string | null | undefined => {
    const text = window.prompt(prompt, "");
    if (text === null) return undefined;
    if (text.trim() === "") return null;
    if (!validText(text)) {
      toast("Use one line of at most 256 characters.", "error");
      return undefined;
    }
    return text;
  };

  const withReason = (action: string, player: string, verb: string) => {
    const reason = ask(`Reason to ${verb} ${player} (optional)`);
    if (reason !== undefined) void act({ action, player, reason });
  };

  if (!state) return <p className="p-4 text-sm text-zinc-500">Loading players...</p>;
  const mod = state.moderate;
  const nameOk = validName(name);

  const onlineRow = (p: OnlinePlayer) => {
    const v = violations?.get(p.uuid);
    return (
      <tr key={p.uuid} className="border-t border-zinc-200 align-top dark:border-zinc-800">
        <td className="py-1">
          <div>{p.name}</div>
          <div className="font-mono text-xs text-zinc-500">{p.uuid}</div>
        </td>
        <td>{p.ping} ms</td>
        <td>{p.gameMode}</td>
        <td>
          <div>{p.dimension ?? ""}</div>
          <div className="text-xs text-zinc-500">{formatPosition(p.position)}</div>
        </td>
        <td>{p.opLevel > 0 ? `op ${p.opLevel}` : ""}</td>
        <td>{p.brand ?? ""}</td>
        <td>{packLabel(p.pack)}</td>
        {mod && <td className="font-mono text-xs">{p.address ?? ""}</td>}
        {violations && (
          <td title={v?.checks.map((c) => `${c.check}: ${c.flags}`).join(", ")}>{v ? `${v.flags} flags, ${v.kicks} kicks` : "none"}</td>
        )}
        {mod && (
          <td className="flex flex-wrap justify-end gap-1 py-1">
            <select
              className={INPUT}
              value=""
              disabled={busy}
              onChange={(e) => e.target.value && void act({ action: "gamemode", player: p.name, mode: e.target.value })}
            >
              <option value="">mode...</option>
              {GAME_MODES.filter((m) => m !== p.gameMode).map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
            <button type="button" className={BUTTON} disabled={busy} onClick={() => {
              const text = ask(`Message to ${p.name}`);
              if (text) void act({ action: "message", player: p.name, text });
            }}>
              Message
            </button>
            <button type="button" className={BUTTON} disabled={busy} onClick={() => act({ action: "spawn", player: p.name }, `Teleport ${p.name} to the world spawn?`)}>
              To spawn
            </button>
            <button type="button" className={BUTTON} disabled={busy} onClick={() => withReason("kick", p.name, "kick")}>
              Kick
            </button>
            <button type="button" className={BUTTON} disabled={busy} onClick={() => withReason("ban", p.name, "ban")}>
              Ban
            </button>
            {p.opLevel > 0 ? (
              <button type="button" className={BUTTON} disabled={busy} onClick={() => act({ action: "deop", player: p.name }, `Deop ${p.name}?`)}>
                Deop
              </button>
            ) : (
              <button type="button" className={BUTTON} disabled={busy} onClick={() => act({ action: "op", player: p.name }, `Make ${p.name} an operator?`)}>
                Op
              </button>
            )}
          </td>
        )}
      </tr>
    );
  };

  const empty = (cols: number, text: string) => (
    <tr>
      <td colSpan={cols} className="py-2 text-zinc-500">
        {text}
      </td>
    </tr>
  );

  return (
    <div className="flex flex-col gap-4 p-4">
      <nav className="flex flex-wrap gap-1">
        {TABS.filter((t) => t !== "ip bans" || state.ipBans).map((t) => (
          <button key={t} type="button" className={`${BUTTON} ${t === tab ? "bg-zinc-200 dark:bg-zinc-800" : ""}`} onClick={() => setTab(t)}>
            {t}
            {t === "online" && ` (${state.online.length})`}
          </button>
        ))}
      </nav>
      {mod && (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <input className={INPUT} value={name} onChange={(e) => setName(e.target.value)} placeholder="player name" />
          <button type="button" className={BUTTON} disabled={busy || !nameOk} onClick={() => act({ action: "whitelist-add", player: name })}>
            Whitelist
          </button>
          <button type="button" className={BUTTON} disabled={busy || !nameOk} onClick={() => withReason("ban", name, "ban")}>
            Ban
          </button>
          <button type="button" className={BUTTON} disabled={busy || !nameOk} onClick={() => act({ action: "op", player: name }, `Make ${name} an operator?`)}>
            Op
          </button>
          <input className={INPUT} value={ip} onChange={(e) => setIp(e.target.value)} placeholder="IP address" />
          <button type="button" className={BUTTON} disabled={busy || !validIp(ip)} onClick={() => {
            const reason = ask(`Reason to ban ${ip} (optional)`);
            if (reason !== undefined) void act({ action: "ban-ip", target: ip, reason }, `Ban ${ip}?`);
          }}>
            Ban IP
          </button>
        </div>
      )}
      <table className="w-full text-left text-sm">
        <tbody>
          {tab === "online" && (
            <>
              <tr className="text-zinc-500">
                <th className="py-1">Player</th>
                <th>Ping</th>
                <th>Mode</th>
                <th>Where</th>
                <th>Op</th>
                <th>Client</th>
                <th>Packs</th>
                {mod && <th>Address</th>}
                {violations && <th>Anti-cheat</th>}
                {mod && <th />}
              </tr>
              {state.online.map(onlineRow)}
              {state.online.length === 0 && empty(10, "Nobody is online.")}
            </>
          )}
          {tab === "offline" &&
            (state.offline.length === 0
              ? empty(3, "No offline players are known.")
              : state.offline.map((p) => (
                  <tr key={p.uuid} className="border-t border-zinc-200 dark:border-zinc-800">
                    <td className="py-1">{p.name}</td>
                    <td className="font-mono text-xs text-zinc-500">{p.uuid}</td>
                    <td>{[p.opLevel > 0 && `op ${p.opLevel}`, p.whitelisted && "whitelisted", p.banned && "banned"].filter(Boolean).join(", ")}</td>
                  </tr>
                )))}
          {tab === "ops" &&
            (state.ops.length === 0
              ? empty(3, "There are no operators.")
              : state.ops.map((o) => (
                  <tr key={o.uuid} className="border-t border-zinc-200 dark:border-zinc-800">
                    <td className="py-1">{o.name}</td>
                    <td>level {o.level}</td>
                    <td className="text-right">
                      {mod && (
                        <button type="button" className={BUTTON} disabled={busy} onClick={() => act({ action: "deop", player: o.uuid }, `Deop ${o.name}?`)}>
                          Deop
                        </button>
                      )}
                    </td>
                  </tr>
                )))}
          {tab === "whitelist" && (
            <>
              <tr>
                <td colSpan={3} className="py-1">
                  Whitelist is {state.whitelist.enabled ? "on" : "off"}
                  {state.whitelist.enforced ? " (enforced)" : ""}
                  {mod && (
                    <button
                      type="button"
                      className={`${BUTTON} ml-2`}
                      disabled={busy}
                      onClick={() => act({ action: state.whitelist.enabled ? "whitelist-off" : "whitelist-on" }, `Turn the whitelist ${state.whitelist.enabled ? "off" : "on"}?`)}
                    >
                      Turn {state.whitelist.enabled ? "off" : "on"}
                    </button>
                  )}
                </td>
              </tr>
              {state.whitelist.entries.map((w) => (
                <tr key={w.uuid} className="border-t border-zinc-200 dark:border-zinc-800">
                  <td className="py-1">{w.name}</td>
                  <td className="font-mono text-xs text-zinc-500">{w.uuid}</td>
                  <td className="text-right">
                    {mod && (
                      <button type="button" className={BUTTON} disabled={busy} onClick={() => act({ action: "whitelist-remove", player: w.uuid }, `Remove ${w.name} from the whitelist?`)}>
                        Remove
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </>
          )}
          {(tab === "bans" || tab === "ip bans") &&
            (() => {
              const list = tab === "bans" ? state.bans : (state.ipBans ?? []);
              if (list.length === 0) return empty(4, "Nothing is banned.");
              return list.map((b) => {
                const who = b.name ?? b.ip ?? "";
                const body = b.ip ? { action: "pardon-ip", ip: b.ip } : { action: "pardon", player: b.uuid };
                return (
                  <tr key={b.uuid ?? b.ip} className="border-t border-zinc-200 dark:border-zinc-800">
                    <td className="py-1">{who}</td>
                    <td>{b.reason ?? ""}</td>
                    <td className="text-zinc-500">
                      by {b.source}, {new Date(b.created * 1000).toLocaleString()}
                      {b.expires ? `, until ${new Date(b.expires * 1000).toLocaleString()}` : ""}
                    </td>
                    <td className="text-right">
                      {mod && (
                        <button type="button" className={BUTTON} disabled={busy} onClick={() => act(body, `Pardon ${who}?`)}>
                          Pardon
                        </button>
                      )}
                    </td>
                  </tr>
                );
              });
            })()}
        </tbody>
      </table>
    </div>
  );
}
