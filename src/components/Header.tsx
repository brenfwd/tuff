import { useEffect, useState } from "react";
import { Link, useLocation } from "wouter";
import { Logo } from "./Logo";
import { useSearch } from "./SearchContext";
import { CloseIcon, GitHubIcon, MenuIcon, MoonIcon, SearchIcon, SunIcon } from "./Icons";
import { REPO_URL } from "../lib/site";

function useTheme() {
  const [theme, setTheme] = useState(() => document.documentElement.dataset.theme ?? "dark");
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem("theme", theme);
    } catch {}
  }, [theme]);
  return [theme, () => setTheme((t) => (t === "dark" ? "light" : "dark"))] as const;
}

const nav = [
  { href: "/docs/introduction", label: "Docs", match: "/docs" },
  { href: "/docs/plugins", label: "Plugins", match: "/docs/plugins" },
  { href: "/docs/performance", label: "Performance", match: "/docs/performance" },
  { href: "/blog", label: "Blog", match: "/blog" },
];

export function Header() {
  const [theme, toggle] = useTheme();
  const { open } = useSearch();
  const [path] = useLocation();
  const [menu, setMenu] = useState(false);
  const [scrolled, setScrolled] = useState(false);
  const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

  useEffect(() => setMenu(false), [path]);
  useEffect(() => {
    const onScroll = () => setScrolled(scrollY > 8);
    onScroll();
    addEventListener("scroll", onScroll, { passive: true });
    return () => removeEventListener("scroll", onScroll);
  }, []);
  useEffect(() => {
    if (!menu) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setMenu(false);
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [menu]);

  const active = (m: string) => (m === "/docs" ? path.startsWith("/docs") && !nav.slice(1).some((n) => path === n.match) : m === "/blog" ? path.startsWith("/blog") : path === m);

  return (
    <header className={`glass sticky top-0 z-40 border-b transition-colors ${scrolled || menu ? "border-line" : "border-transparent"}`}>
      <div className="mx-auto flex h-16 max-w-7xl items-center gap-3 px-4 sm:px-6">
        <Link href="/" className="mr-2 rounded-lg" aria-label="tuff home">
          <Logo />
        </Link>
        <nav aria-label="Main" className="hidden items-center gap-1 md:flex">
          {nav.map((n) => (
            <Link
              key={n.href}
              href={n.href}
              aria-current={active(n.match) ? "page" : undefined}
              className={`rounded-lg px-3 py-2 text-sm font-medium transition-colors hover:text-fg ${active(n.match) ? "text-fg" : "text-fg-muted"}`}
            >
              {n.label}
            </Link>
          ))}
        </nav>
        <div className="ml-auto flex items-center gap-1.5">
          <button
            type="button"
            onClick={open}
            className="flex h-10 items-center gap-2 rounded-xl border border-line bg-bg-raised/70 px-3 text-sm text-fg-muted shadow-sm transition hover:border-line-strong hover:text-fg sm:w-56 lg:w-64"
            aria-label="Search documentation"
          >
            <SearchIcon className="h-4 w-4 shrink-0" />
            <span className="hidden sm:inline">Search...</span>
            <kbd className="ml-auto hidden rounded-md border border-line bg-bg-sunken px-1.5 py-0.5 font-mono text-[11px] text-fg-subtle sm:inline">
              {isMac ? "⌘" : "Ctrl"} K
            </kbd>
          </button>
          <button type="button" onClick={toggle} className="grid h-10 w-10 place-items-center rounded-xl text-fg-muted transition hover:bg-bg-sunken hover:text-fg" aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} theme`}>
            {theme === "dark" ? <SunIcon className="h-5 w-5" /> : <MoonIcon className="h-5 w-5" />}
          </button>
          <a href={REPO_URL} target="_blank" rel="noopener noreferrer" className="hidden h-10 w-10 place-items-center rounded-xl text-fg-muted transition hover:bg-bg-sunken hover:text-fg sm:grid" aria-label="tuff on GitHub">
            <GitHubIcon className="h-5 w-5" />
          </a>
          <button
            type="button"
            className="grid h-10 w-10 place-items-center rounded-xl text-fg-muted transition hover:bg-bg-sunken hover:text-fg md:hidden"
            aria-label={menu ? "Close menu" : "Open menu"}
            aria-expanded={menu}
            aria-controls="mobile-menu"
            onClick={() => setMenu((m) => !m)}
          >
            {menu ? <CloseIcon className="h-5 w-5" /> : <MenuIcon className="h-5 w-5" />}
          </button>
        </div>
      </div>
      {menu && (
        <nav id="mobile-menu" aria-label="Main" className="border-t border-line px-4 pb-5 pt-2 md:hidden">
          {[{ href: "/", label: "Home", match: "/" }, ...nav].map((n) => (
            <Link key={n.href} href={n.href} className="flex items-center justify-between border-b border-line py-3.5 text-base font-medium text-fg last:border-0">
              {n.label}
            </Link>
          ))}
          <a href={REPO_URL} target="_blank" rel="noopener noreferrer" className="mt-3 flex items-center gap-2 py-2 text-base font-medium text-fg-muted">
            <GitHubIcon className="h-5 w-5" /> GitHub
          </a>
        </nav>
      )}
    </header>
  );
}
