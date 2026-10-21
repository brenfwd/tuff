import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, Session, setCsrf, storageGet, storageSet } from "./api";
import { Login } from "./pages/Login";
import { Console } from "./pages/Console";
import { Audit } from "./pages/Audit";
import { Dashboard } from "./pages/Dashboard";
import { Timings } from "./pages/Timings";
import { Permissions } from "./pages/Permissions";
import { Backups } from "./pages/Backups";
import { DataPacks } from "./pages/DataPacks";
import { ResourcePacks } from "./pages/ResourcePacks";
import { Players } from "./pages/Players";
import { Anticheat } from "./pages/Anticheat";
import { Config } from "./pages/Config";
import { World } from "./pages/World";
import { Sql } from "./pages/Sql";
import { Access } from "./pages/Access";
import { PluginPage } from "./pages/PluginPage";
import { pageHash, sortPages, Toast } from "./plugins";
import type { PanelPageInfo } from "./sdk/context";

interface Page {
  id: string;
  href: string;
  title: string;
  scope: string;
  render: (session: Session, toast: Toast) => React.ReactNode;
}

const PAGES: Page[] = [
  { id: "dashboard", href: "#/dashboard", title: "Dashboard", scope: "metrics.read", render: (s) => <Dashboard session={s} /> },
  { id: "timings", href: "#/timings", title: "Timings", scope: "timings", render: () => <Timings /> },
  { id: "players", href: "#/players", title: "Players", scope: "players.read", render: (s, t) => <Players session={s} toast={t} /> },
  { id: "anticheat", href: "#/anticheat", title: "Anticheat", scope: "players.moderate", render: () => <Anticheat /> },
  { id: "console", href: "#/console", title: "Console", scope: "console.run", render: (s) => <Console session={s} /> },
  { id: "permissions", href: "#/permissions", title: "Permissions", scope: "permissions", render: (_, t) => <Permissions toast={t} /> },
  { id: "backups", href: "#/backups", title: "Backups", scope: "backups.read", render: (s, t) => <Backups session={s} toast={t} /> },
  { id: "datapacks", href: "#/datapacks", title: "Data packs", scope: "datapacks.read", render: (s, t) => <DataPacks session={s} toast={t} /> },
  { id: "resourcepacks", href: "#/resourcepacks", title: "Resource packs", scope: "resourcepacks.read", render: (s, t) => <ResourcePacks session={s} toast={t} /> },
  { id: "config", href: "#/config", title: "Config", scope: "config.read", render: (_, t) => <Config toast={t} /> },
  { id: "world", href: "#/world", title: "World", scope: "players.read", render: (_, t) => <World toast={t} /> },
  { id: "sql", href: "#/sql", title: "SQL", scope: "sql.read", render: (s, t) => <Sql session={s} toast={t} /> },
  { id: "audit", href: "#/audit", title: "Audit log", scope: "panel.admin", render: () => <Audit /> },
  { id: "access", href: "#/access", title: "Access", scope: "panel.admin", render: (s, t) => <Access session={s} toast={t} /> },
];

function pluginPage(info: PanelPageInfo): Page {
  return {
    id: `p/${info.id}`,
    href: pageHash(info.id),
    title: info.title,
    scope: info.scope,
    render: (session, toast) => <PluginPage key={info.id} page={info} session={session} toast={toast} />,
  };
}

interface ToastMessage {
  id: number;
  text: string;
  kind: "info" | "error";
}

function useToasts(): [ToastMessage[], Toast] {
  const [toasts, setToasts] = useState<ToastMessage[]>([]);
  const next = useRef(0);
  const toast = useCallback<Toast>((text, kind = "info") => {
    const id = ++next.current;
    setToasts((old) => [...old.slice(-4), { id, text, kind }]);
    setTimeout(() => setToasts((old) => old.filter((t) => t.id !== id)), 4000);
  }, []);
  return [toasts, toast];
}

function useHash(): string {
  const [hash, setHash] = useState(window.location.hash);
  useEffect(() => {
    const on = () => setHash(window.location.hash);
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  return hash;
}

function useTheme(): [boolean, () => void] {
  const initial = () => {
    const saved = storageGet("tuff.theme");
    if (saved) return saved === "dark";
    return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
  };
  const [dark, setDark] = useState(initial);
  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
  }, [dark]);
  const toggle = () =>
    setDark((d) => {
      storageSet("tuff.theme", d ? "light" : "dark");
      return !d;
    });
  return [dark, toggle];
}

export function App() {
  const [session, setSession] = useState<Session | null>(null);
  const [checked, setChecked] = useState(false);
  const [dark, toggleTheme] = useTheme();
  const hash = useHash();
  const [plugins, setPlugins] = useState<PanelPageInfo[]>([]);
  const [toasts, toast] = useToasts();

  const adopt = useCallback((s: Session | null) => {
    setCsrf(s?.csrf ?? "");
    setSession(s);
  }, []);

  useEffect(() => {
    api<Session>("/session")
      .then(adopt)
      .catch((e) => {
        if (!(e instanceof ApiError) || e.status !== 401) console.warn(e);
        adopt(null);
      })
      .finally(() => setChecked(true));
  }, [adopt]);

  useEffect(() => {
    if (!session) {
      setPlugins([]);
      return;
    }
    api<{ pages: PanelPageInfo[] }>("/pages")
      .then((r) => setPlugins(sortPages(r.pages)))
      .catch((e) => toast(`Could not list plugin pages: ${e instanceof Error ? e.message : String(e)}`, "error"));
  }, [session, toast]);

  const pluginPages = useMemo(() => plugins.map(pluginPage), [plugins]);

  const logout = async () => {
    try {
      await api("/session", { method: "DELETE" });
    } finally {
      adopt(null);
    }
  };

  const shell = "bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100";
  if (!checked) return <div className={`${shell} min-h-full`} />;
  if (!session) {
    return (
      <div className={`${shell} min-h-full`}>
        <Login onLogin={adopt} />
      </div>
    );
  }
  const visible = [...PAGES.filter((p) => session.scopes.includes(p.scope)), ...pluginPages];
  const current = visible.find((p) => p.href === hash) ?? visible[0];
  return (
    <div className={`${shell} flex h-dvh flex-col overflow-hidden`}>
      <header className="flex flex-wrap items-center gap-3 border-b border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <span className="font-semibold">tuff</span>
        <nav className="flex flex-1 flex-wrap gap-1">
          {visible.map((p) => (
            <a
              key={p.id}
              href={p.href}
              className={`rounded px-2 py-1 text-sm ${
                p === current
                  ? "bg-zinc-200 dark:bg-zinc-800"
                  : "hover:bg-zinc-100 dark:hover:bg-zinc-900"
              }`}
            >
              {p.title}
            </a>
          ))}
        </nav>
        <span className="text-sm text-zinc-500">{session.principal.name}</span>
        <button
          type="button"
          onClick={toggleTheme}
          className="rounded px-2 py-1 text-sm hover:bg-zinc-100 dark:hover:bg-zinc-900"
        >
          {dark ? "Light" : "Dark"}
        </button>
        <button
          type="button"
          onClick={logout}
          className="rounded px-2 py-1 text-sm hover:bg-zinc-100 dark:hover:bg-zinc-900"
        >
          Log out
        </button>
      </header>
      <main className="flex min-h-0 flex-1 flex-col overflow-y-auto">
        {current ? current.render(session, toast) : <p className="p-4">Nothing to show for your role.</p>}
      </main>
      <div className="pointer-events-none fixed right-4 bottom-4 flex flex-col items-end gap-2" role="status">
        {toasts.map((t) => (
          <div
            key={t.id}
            className={`rounded px-3 py-2 text-sm shadow ${
              t.kind === "error"
                ? "bg-red-600 text-white"
                : "bg-zinc-800 text-zinc-100 dark:bg-zinc-200 dark:text-zinc-900"
            }`}
          >
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}
