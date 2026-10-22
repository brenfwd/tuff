import { useEffect, useMemo, useRef, useState } from "react";
import { Command } from "cmdk";
import MiniSearch from "minisearch";
import { navigate } from "../lib/hash";
import { searchOptions } from "../lib/searchOptions";
import nav from "../generated/nav.json";
import blogIndex from "../generated/blog/index.json";
import type { BlogIndex } from "../lib/blog";

const blog = blogIndex as BlogIndex;
import { ArrowRight, CloseIcon, DocIcon, HashIcon, SearchIcon } from "./Icons";

type Hit = { id: string; route: string; page: string; pageTitle: string; anchor: string; heading: string; group: string; snippet: string; terms: string[] };

let indexPromise: Promise<MiniSearch> | null = null;
const loadIndex = () =>
  (indexPromise ??= fetch(`${import.meta.env.BASE_URL}search-index.json`)
    .then((r) => r.text())
    .then((json) => MiniSearch.loadJSON(json, searchOptions)));

function around(text: string, terms: string[]) {
  const lower = text.toLowerCase();
  const at = Math.min(...terms.map((t) => lower.indexOf(t.toLowerCase())).filter((i) => i >= 0), Infinity);
  if (!Number.isFinite(at) || at < 40) return text;
  return `...${text.slice(text.lastIndexOf(" ", at - 30) + 1)}`;
}

function highlight(text: string, terms: string[]) {
  if (!terms.length) return text;
  const re = new RegExp(`(${terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`, "gi");
  return text.split(re).map((part, i) => (i % 2 ? <mark key={i} className="rounded-sm bg-ember-soft text-ember-text">{part}</mark> : part));
}

export default function SearchPalette({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState<MiniSearch | null>(null);
  useEffect(() => {
    loadIndex().then(setIndex);
  }, []);
  useEffect(() => {
    if (!open) setQuery("");
  }, [open]);

  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const id = requestAnimationFrame(() => {
      if (listRef.current) listRef.current.scrollTop = 0;
      const root = listRef.current?.closest("[cmdk-root]");
      if (root) root.scrollTop = 0;
      if (root?.parentElement) root.parentElement.scrollTop = 0;
    });
    return () => cancelAnimationFrame(id);
  }, [query]);

  const hits = useMemo(() => {
    if (!index || !query.trim()) return [];
    return (index.search(query) as unknown as Hit[]).slice(0, 30);
  }, [index, query]);

  const grouped = useMemo(() => {
    const g = new Map<string, Hit[]>();
    for (const h of hits) g.set(h.pageTitle, [...(g.get(h.pageTitle) ?? []), h]);
    return [...g];
  }, [hits]);

  const go = (to: string) => {
    onOpenChange(false);
    navigate(to);
  };

  return (
    <Command.Dialog
      open={open}
      onOpenChange={onOpenChange}
      label="Search documentation"
      shouldFilter={false}
      loop
      overlayClassName="fixed inset-0 z-50 bg-black/50 backdrop-blur-sm"
      contentClassName="fixed z-50 inset-0 sm:inset-auto sm:left-1/2 sm:top-[12vh] sm:w-[640px] sm:max-w-[calc(100vw-2rem)] sm:-translate-x-1/2 flex flex-col overflow-hidden bg-bg-raised sm:rounded-2xl sm:border sm:border-line-strong shadow-2xl"
    >
      <div className="flex items-center gap-3 border-b border-line px-4">
        <SearchIcon className="h-5 w-5 shrink-0 text-fg-subtle" />
        <Command.Input value={query} onValueChange={setQuery} placeholder="Search the docs and blog..." className="h-14 w-full bg-transparent text-base text-fg outline-none placeholder:text-fg-subtle" />
        <button type="button" onClick={() => onOpenChange(false)} className="grid h-9 shrink-0 place-items-center rounded-lg px-2 text-xs text-fg-muted hover:bg-bg-sunken" aria-label="Close search">
          <kbd className="hidden font-mono sm:inline">Esc</kbd>
          <CloseIcon className="h-5 w-5 sm:hidden" />
        </button>
      </div>
      <Command.List ref={listRef} className="max-h-none flex-1 overflow-y-auto overscroll-contain p-2 sm:max-h-[min(60vh,520px)]">
        {query.trim() && index && <Command.Empty className="px-4 py-12 text-center text-sm text-fg-muted">No results for "{query}"</Command.Empty>}
        {!query.trim() &&
          [...new Set(nav.map((p) => p.group))].map((group) => (
            <Command.Group key={group} heading={group}>
              {nav.filter((p) => p.group === group).map((p) => (
                <Command.Item key={p.slug} value={p.slug} onSelect={() => go(`/docs/${p.slug}`)} className="group flex cursor-pointer items-center gap-3 rounded-xl px-3 py-2.5">
                  <DocIcon className="h-4 w-4 shrink-0 text-fg-subtle" />
                  <span className="flex-1 truncate text-sm font-medium">{p.title}</span>
                  <ArrowRight className="cmdk-arrow h-4 w-4 text-fg-subtle opacity-0" />
                </Command.Item>
              ))}
            </Command.Group>
          ))}
        {!query.trim() && blog.posts.length > 0 && (
          <Command.Group heading="Blog">
            {blog.posts.slice(0, 5).map((p) => (
              <Command.Item key={p.slug} value={`blog/${p.slug}`} onSelect={() => go(`/blog/${p.slug}`)} className="group flex cursor-pointer items-center gap-3 rounded-xl px-3 py-2.5">
                <DocIcon className="h-4 w-4 shrink-0 text-fg-subtle" />
                <span className="flex-1 truncate text-sm font-medium">{p.title}</span>
                <ArrowRight className="cmdk-arrow h-4 w-4 text-fg-subtle opacity-0" />
              </Command.Item>
            ))}
          </Command.Group>
        )}
        {grouped.map(([page, items]) => (
          <Command.Group key={page} heading={page}>
            {items.map((h) => (
              <Command.Item key={h.id} value={h.id} onSelect={() => go(`${h.route}${h.anchor ? `?h=${h.anchor}` : ""}`)} className="group flex cursor-pointer items-start gap-3 rounded-xl px-3 py-2.5">
                {h.anchor ? <HashIcon className="mt-0.5 h-4 w-4 shrink-0 text-fg-subtle" /> : <DocIcon className="mt-0.5 h-4 w-4 shrink-0 text-fg-subtle" />}
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-medium text-fg">{highlight(h.heading, h.terms)}</span>
                  {h.snippet && <span className="mt-0.5 line-clamp-1 block text-xs text-fg-muted">{highlight(around(h.snippet, h.terms), h.terms)}</span>}
                </span>
                <ArrowRight className="cmdk-arrow mt-0.5 h-4 w-4 shrink-0 text-fg-subtle opacity-0" />
              </Command.Item>
            ))}
          </Command.Group>
        ))}
      </Command.List>
      <div className="hidden items-center gap-4 border-t border-line px-4 py-2.5 text-xs text-fg-subtle sm:flex">
        <span><kbd className="font-mono">Up/Down</kbd> to move</span>
        <span><kbd className="font-mono">Enter</kbd> to open</span>
        <span><kbd className="font-mono">Esc</kbd> to close</span>
      </div>
    </Command.Dialog>
  );
}
