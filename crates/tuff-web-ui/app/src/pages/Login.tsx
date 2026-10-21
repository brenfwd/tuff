import { FormEvent, useState } from "react";
import { api, Session } from "../api";

function format(raw: string): string {
  const digits = raw.replace(/\D/g, "").slice(0, 16);
  return digits.replace(/(\d{4})(?=\d)/g, "$1-");
}

export function Login({ onLogin }: { onLogin: (s: Session) => void }) {
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const complete = code.replace(/\D/g, "").length === 16;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!complete || busy) return;
    setBusy(true);
    setError("");
    try {
      onLogin(await api<Session>("/session", { method: "POST", body: JSON.stringify({ code }) }));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className="mx-auto flex max-w-sm flex-col gap-4 px-4 pt-24">
      <h1 className="text-xl font-semibold">Server admin</h1>
      <p className="text-sm text-zinc-600 dark:text-zinc-400">
        Run <code className="font-mono">/tuff web login</code> on the server console or in game,
        then enter the code it shows. Codes expire after two minutes.
      </p>
      <input
        value={code}
        onChange={(e) => setCode(format(e.target.value))}
        inputMode="numeric"
        autoComplete="one-time-code"
        autoFocus
        placeholder="0000-0000-0000-0000"
        aria-label="Login code"
        className="rounded border border-zinc-300 bg-white px-3 py-2 font-mono text-lg tracking-wider dark:border-zinc-700 dark:bg-zinc-900"
      />
      {error && <p className="text-sm text-red-600 dark:text-red-400">{error}</p>}
      <button
        type="submit"
        disabled={!complete || busy}
        className="rounded bg-emerald-600 px-3 py-2 font-medium text-white disabled:opacity-50"
      >
        Log in
      </button>
    </form>
  );
}
