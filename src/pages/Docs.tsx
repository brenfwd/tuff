import { useEffect, useRef, useState, type MouseEvent } from "react";
import { Link, useLocation } from "wouter";
import nav from "../generated/nav.json";
import { navigate, useHashSection } from "../lib/hash";
import { useSeo } from "../lib/useSeo";
import { ArrowLeft, ArrowRight, CloseIcon, MenuIcon } from "../components/Icons";
import { REPO_URL } from "../lib/site";

type PageMeta = { slug: string; title: string; group: string; description: string };
type Doc = PageMeta & { html: string; toc: { level: number; id: string; text: string }[]; prev: PageMeta | null; next: PageMeta | null };

const modules = import.meta.glob<{ default: Doc }>("../generated/docs/*.json");
const load = (slug: string) => modules[`../generated/docs/${slug.replace(/\//g, "__")}.json`]?.().then((m) => m.default);
const groups = [...new Set(nav.map((p) => p.group))];

function Sidebar({ slug, onNavigate }: { slug: string; onNavigate?: () => void }) {
  return (
    <nav aria-label="Documentation" className="space-y-7">
      {groups.map((g) => (
        <div key={g}>
          <h2 className="px-3 text-xs font-semibold uppercase tracking-[0.12em] text-fg-subtle">{g}</h2>
          <ul className="mt-2 space-y-0.5">
            {nav.filter((p) => p.group === g).map((p) => {
              const active = p.slug === slug;
              return (
                <li key={p.slug}>
                  <Link
                    href={`/docs/${p.slug}`}
                    onClick={onNavigate}
                    aria-current={active ? "page" : undefined}
                    className={`relative block rounded-lg px-3 py-1.5 text-[0.92rem] transition-colors ${active ? "bg-ember-soft font-medium text-ember-text" : "text-fg-muted hover:bg-bg-sunken hover:text-fg"}`}
                  >
                    {p.title}
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}

function useActiveHeading(ids: string[]) {
  const [active, setActive] = useState<string | null>(null);
  useEffect(() => {
    if (!ids.length) return;
    const els = ids.map((id) => document.getElementById(id)).filter(Boolean) as HTMLElement[];
    const onScroll = () => {
      let current: string | null = null;
      for (const el of els) if (el.getBoundingClientRect().top < 120) current = el.id;
      setActive(current ?? ids[0]);
    };
    onScroll();
    addEventListener("scroll", onScroll, { passive: true });
    return () => removeEventListener("scroll", onScroll);
  }, [ids]);
  return active;
}

export default function Docs() {
  const [path] = useLocation();
  const slug = path.replace(/^\/docs\/?/, "").replace(/\/$/, "") || "introduction";
  const section = useHashSection();
  const [doc, setDoc] = useState<Doc | null | undefined>(undefined);
  const [drawer, setDrawer] = useState(false);
  const article = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    const p = load(slug);
    if (!p) setDoc(null);
    else p.then((d) => live && setDoc(d));
    return () => {
      live = false;
    };
  }, [slug]);

  useSeo({
    title: doc ? `${doc.title} - tuff docs` : "tuff docs",
    description: doc?.description,
    path: doc ? `docs/${doc.slug}/` : "",
  });

  useEffect(() => {
    if (!doc || doc.slug !== slug) return;
    if (section) document.getElementById(section)?.scrollIntoView();
    else scrollTo({ top: 0 });
  }, [doc, slug, section]);

  useEffect(() => {
    if (!drawer) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setDrawer(false);
    addEventListener("keydown", onKey);
    document.body.style.overflow = "hidden";
    return () => {
      removeEventListener("keydown", onKey);
      document.body.style.overflow = "";
    };
  }, [drawer]);

  const active = useActiveHeading(doc?.toc.map((t) => t.id) ?? []);

  const onArticleClick = (e: MouseEvent) => {
    const target = e.target as HTMLElement;
    const copy = target.closest<HTMLButtonElement>(".copy-btn");
    if (copy) {
      const code = copy.closest(".code-block")?.querySelector("pre")?.innerText ?? "";
      navigator.clipboard?.writeText(code).then(() => {
        copy.textContent = "Copied";
        setTimeout(() => (copy.textContent = "Copy"), 1500);
      });
      return;
    }
    const a = target.closest<HTMLAnchorElement>("a[href^='#']");
    const href = a?.getAttribute("href");
    if (a && href && !href.startsWith("#/")) {
      e.preventDefault();
      navigate(`/docs/${slug}?h=${href.slice(1)}`, { replace: true });
      document.getElementById(href.slice(1))?.scrollIntoView();
    }
  };

  const tocLink = (id: string) => `#/docs/${slug}?h=${id}`;

  if (doc === null)
    return (
      <main id="main" tabIndex={-1} className="mx-auto max-w-3xl px-4 py-24 text-center">
        <h1 className="font-display text-3xl font-bold">Page not found</h1>
        <p className="mt-3 text-fg-muted">There is no docs page at this address.</p>
        <Link href="/docs/introduction" className="mt-6 inline-block font-medium text-ember-text underline underline-offset-4">Go to the introduction</Link>
      </main>
    );

  return (
    <div className="mx-auto max-w-7xl px-4 sm:px-6">
      <div className="sticky top-16 z-30 -mx-4 flex items-center gap-2 border-b border-line bg-bg/90 px-4 py-2 backdrop-blur lg:hidden">
        <button type="button" onClick={() => setDrawer(true)} className="flex h-9 items-center gap-2 rounded-lg px-2 text-sm font-medium text-fg-muted hover:bg-bg-sunken" aria-expanded={drawer} aria-controls="docs-drawer">
          <MenuIcon className="h-4 w-4" /> Menu
        </button>
        <span className="truncate text-sm text-fg-subtle">{doc?.group} <span aria-hidden="true">/</span> <span className="text-fg">{doc?.title}</span></span>
      </div>

      {drawer && (
        <div className="fixed inset-0 z-50 lg:hidden" role="dialog" aria-modal="true" aria-label="Documentation menu" id="docs-drawer">
          <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={() => setDrawer(false)} />
          <div className="absolute inset-y-0 left-0 flex w-[min(20rem,85vw)] flex-col border-r border-line bg-bg shadow-2xl">
            <div className="flex h-14 items-center justify-between border-b border-line px-4">
              <span className="font-display font-semibold">Documentation</span>
              <button type="button" onClick={() => setDrawer(false)} className="grid h-9 w-9 place-items-center rounded-lg hover:bg-bg-sunken" aria-label="Close menu" autoFocus>
                <CloseIcon className="h-5 w-5" />
              </button>
            </div>
            <div className="flex-1 overflow-y-auto px-2 py-5">
              <Sidebar slug={slug} onNavigate={() => setDrawer(false)} />
            </div>
          </div>
        </div>
      )}

      <div className="lg:grid lg:grid-cols-[15rem_minmax(0,1fr)] lg:gap-10 xl:grid-cols-[15rem_minmax(0,1fr)_13rem]">
        <aside className="hidden lg:block">
          <div className="sticky top-16 max-h-[calc(100vh-4rem)] overflow-y-auto py-10 pr-2">
            <Sidebar slug={slug} />
          </div>
        </aside>

        <main id="main" tabIndex={-1} className="min-w-0 py-8 outline-none lg:py-12">
          {doc && (
            <article className="mx-auto max-w-[46rem]">
              <p className="text-sm font-semibold text-ember-text">{doc.group}</p>
              <h1 className="mt-2 font-display text-[2.1rem] font-bold leading-tight tracking-tight sm:text-5xl">{doc.title}</h1>
              <p className="mt-4 text-lg leading-8 text-fg-muted">{doc.description}</p>
              {doc.toc.length > 2 && (
                <details className="mt-6 rounded-xl border border-line bg-bg-raised px-4 py-3 xl:hidden">
                  <summary className="cursor-pointer text-sm font-medium">On this page</summary>
                  <ul className="mt-3 space-y-2 text-sm">
                    {doc.toc.filter((t) => t.level === 2).map((t) => (
                      <li key={t.id}><a href={tocLink(t.id)} className="text-fg-muted hover:text-fg">{t.text}</a></li>
                    ))}
                  </ul>
                </details>
              )}
              <div ref={article} className="doc-prose mt-8" onClick={onArticleClick} dangerouslySetInnerHTML={{ __html: doc.html }} />

              <nav aria-label="Previous and next pages" className="mt-16 grid gap-4 border-t border-line pt-8 sm:grid-cols-2">
                {doc.prev ? (
                  <Link href={`/docs/${doc.prev.slug}`} className="card card-hover group flex flex-col p-5">
                    <span className="flex items-center gap-1.5 text-xs text-fg-subtle"><ArrowLeft className="h-3.5 w-3.5" /> Previous</span>
                    <span className="mt-1 font-medium text-fg group-hover:text-ember-text">{doc.prev.title}</span>
                  </Link>
                ) : <span />}
                {doc.next && (
                  <Link href={`/docs/${doc.next.slug}`} className="card card-hover group flex flex-col p-5 text-right">
                    <span className="flex items-center justify-end gap-1.5 text-xs text-fg-subtle">Next <ArrowRight className="h-3.5 w-3.5" /></span>
                    <span className="mt-1 font-medium text-fg group-hover:text-ember-text">{doc.next.title}</span>
                  </Link>
                )}
              </nav>
              <p className="mt-10 text-sm text-fg-subtle">
                Found a problem on this page? <a className="underline underline-offset-4 hover:text-fg" href={`${REPO_URL}/issues`} target="_blank" rel="noopener noreferrer">Open an issue</a>.
              </p>
            </article>
          )}
        </main>

        <aside className="hidden xl:block" aria-label="On this page">
          {doc && doc.toc.length > 1 && (
            <div className="sticky top-16 max-h-[calc(100vh-4rem)] overflow-y-auto py-12">
              <h2 className="text-xs font-semibold uppercase tracking-[0.12em] text-fg-subtle">On this page</h2>
              <ul className="mt-3 space-y-1 border-l border-line text-[0.84rem]">
                {doc.toc.map((t) => (
                  <li key={t.id}>
                    <a
                      href={tocLink(t.id)}
                      className={`-ml-px block border-l py-1 transition-colors ${t.level === 3 ? "pl-6" : "pl-3"} ${active === t.id ? "border-ember font-medium text-fg" : "border-transparent text-fg-muted hover:text-fg"}`}
                    >
                      {t.text}
                    </a>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </aside>
      </div>
    </div>
  );
}
