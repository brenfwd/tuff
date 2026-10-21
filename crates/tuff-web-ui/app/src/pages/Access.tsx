import { useCallback, useEffect, useState } from "react";
import { api, Session } from "../api";
import { Toast } from "../plugins";
import { age, expiryLabel, TOKEN_EXPIRIES, validTokenName } from "../access";

const BUTTON =
  "rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900";
const INPUT = "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm dark:border-zinc-700";

interface TokenRow {
  id: string;
  name: string;
  level: number;
  created: number;
  lastUsed: number | null;
  expires: number | null;
  expired: boolean;
  by: string;
}

interface SessionRow {
  session: string;
  principal: { kind: string; name: string };
  created: number;
  lastSeen: number;
  remote: string | null;
  current: boolean;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function Access({ session, toast }: { session: Session; toast: Toast }) {
  const [tokens, setTokens] = useState<TokenRow[]>([]);
  const [sessions, setSessions] = useState<SessionRow[]>([]);
  const [name, setName] = useState("");
  const [level, setLevel] = useState(Math.min(2, session.level));
  const [expiresIn, setExpiresIn] = useState<number | null>(null);
  const [fresh, setFresh] = useState("");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setTokens((await api<{ tokens: TokenRow[] }>("/tokens")).tokens);
      setSessions((await api<{ sessions: SessionRow[] }>("/sessions")).sessions);
    } catch (e) {
      toast(message(e), "error");
    }
  }, [toast]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = async (call: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await call();
    } catch (e) {
      toast(message(e), "error");
    } finally {
      setBusy(false);
      void refresh();
    }
  };

  const create = () =>
    run(async () => {
      const made = await api<{ token: string }>("/tokens", { method: "POST", body: JSON.stringify({ name, level, expiresIn }) });
      setFresh(made.token);
      setName("");
    });

  return (
    <div className="flex flex-col gap-6 p-4">
      <section className="flex flex-col gap-2">
        <h2 className="font-semibold">API tokens</h2>
        <div className="flex flex-wrap items-center gap-2">
          <input className={INPUT} placeholder="token name" value={name} onChange={(e) => setName(e.target.value)} />
          <select className={INPUT} value={level} onChange={(e) => setLevel(Number(e.target.value))}>
            {[0, 1, 2, 3, 4]
              .filter((l) => l <= session.level)
              .map((l) => (
                <option key={l} value={l}>
                  level {l}
                </option>
              ))}
          </select>
          <select
            className={INPUT}
            value={expiresIn ?? ""}
            onChange={(e) => setExpiresIn(e.target.value === "" ? null : Number(e.target.value))}
          >
            {TOKEN_EXPIRIES.map((x) => (
              <option key={x.label} value={x.seconds ?? ""}>
                {x.label}
              </option>
            ))}
          </select>
          <button type="button" className={BUTTON} disabled={busy || !validTokenName(name)} onClick={() => void create()}>
            Create
          </button>
        </div>
        {fresh && (
          <div className="rounded border border-amber-500 p-2 text-sm">
            <p>Copy this token now; it is not shown again. Send it as an Authorization: Bearer header.</p>
            <code className="font-mono break-all select-all">{fresh}</code>
            <button type="button" className={`${BUTTON} ml-2`} onClick={() => setFresh("")}>
              Done
            </button>
          </div>
        )}
        <table className="w-full text-left text-sm">
          <tbody>
            <tr className="text-zinc-500">
              <th className="py-1">Name</th>
              <th>Level</th>
              <th>Created</th>
              <th>Last used</th>
              <th>Expires</th>
              <th>By</th>
              <th />
            </tr>
            {tokens.map((t) => (
              <tr key={t.id} className="border-t border-zinc-200 dark:border-zinc-800">
                <td className="py-1">{t.name}</td>
                <td>{t.level}</td>
                <td>{new Date(t.created).toLocaleString()}</td>
                <td>{t.lastUsed ? `${age(t.lastUsed)} ago` : "never"}</td>
                <td className={t.expired ? "text-red-600" : undefined}>{expiryLabel(t.expires)}</td>
                <td>{t.by}</td>
                <td className="text-right">
                  <button
                    type="button"
                    className={BUTTON}
                    disabled={busy}
                    onClick={() => window.confirm(`Revoke token ${t.name}?`) && void run(() => api(`/tokens/${t.id}`, { method: "DELETE" }))}
                  >
                    Revoke
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
      <section className="flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <h2 className="font-semibold">Sessions</h2>
          <button
            type="button"
            className={BUTTON}
            disabled={busy}
            onClick={() => window.confirm("End every session, yours included?") && void run(() => api("/sessions", { method: "DELETE" }))}
          >
            End all
          </button>
        </div>
        <table className="w-full text-left text-sm">
          <tbody>
            <tr className="text-zinc-500">
              <th className="py-1">Who</th>
              <th>From</th>
              <th>Age</th>
              <th>Last seen</th>
              <th />
            </tr>
            {sessions.map((s) => (
              <tr key={s.session} className="border-t border-zinc-200 dark:border-zinc-800">
                <td className="py-1">
                  {s.principal.name}
                  {s.current && <span className="ml-2 text-xs text-zinc-500">(this one)</span>}
                </td>
                <td className="font-mono text-xs">{s.remote ?? ""}</td>
                <td>{age(s.created)}</td>
                <td>{age(s.lastSeen)} ago</td>
                <td className="text-right">
                  <button type="button" className={BUTTON} disabled={busy} onClick={() => void run(() => api(`/sessions/${s.session}`, { method: "DELETE" }))}>
                    End
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </div>
  );
}
