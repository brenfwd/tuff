import { api, Session } from "./api";
import type { PanelContext, PanelEventHandler, PanelPageInfo } from "./sdk/context";

export interface EventSourceLike {
  addEventListener(name: string, listener: (e: MessageEvent) => void): void;
  close(): void;
}

export type OpenSource = (url: string) => EventSourceLike;

export const openEventSource: OpenSource = (url) => new EventSource(url, { withCredentials: true });

export function eventsUrl(topics: string[]): string {
  return `/admin/events?topics=${encodeURIComponent(topics.join(","))}`;
}

interface Subscription {
  topics: string[];
  names: string[];
  handler: PanelEventHandler;
}

/** One event stream per mounted page, reopened over the union of its
 * subscriptions' topics whenever that set changes. */
export class PageEvents {
  private subs = new Set<Subscription>();
  private source: EventSourceLike | null = null;
  private key = "";

  constructor(private readonly open: OpenSource) {}

  subscribe(topics: string[], names: string[], handler: PanelEventHandler): () => void {
    const sub = { topics: [...topics], names: [...names], handler };
    this.subs.add(sub);
    this.refresh();
    return () => {
      if (this.subs.delete(sub)) this.refresh();
    };
  }

  close() {
    this.subs.clear();
    this.refresh();
  }

  private refresh() {
    const all = [...this.subs];
    const topics = [...new Set(all.flatMap((s) => s.topics))].sort();
    const names = [...new Set([...all.flatMap((s) => s.names), "resync"])].sort();
    const key = topics.length ? `${topics.join(",")}|${names.join(",")}` : "";
    if (key === this.key) return;
    this.source?.close();
    this.source = null;
    this.key = key;
    if (!key) return;
    const source = this.open(eventsUrl(topics));
    for (const name of names) {
      source.addEventListener(name, (e) => this.dispatch(name, String(e.data)));
    }
    this.source = source;
  }

  private dispatch(name: string, data: string) {
    for (const sub of [...this.subs]) {
      if (name === "resync" || sub.names.includes(name)) sub.handler(name, data);
    }
  }
}

export type Toast = (message: string, kind?: "info" | "error") => void;

export function makeContext(
  page: PanelPageInfo,
  session: Session,
  toast: Toast,
  events: PageEvents,
): PanelContext {
  return Object.freeze({
    page,
    principal: session.principal,
    scopes: [...session.scopes],
    api: <T,>(path: string, init?: RequestInit) => api<T>(path, init),
    events: (topics: string[], names: string[], handler: PanelEventHandler) =>
      events.subscribe(topics, names, handler),
    toast,
  });
}

/** Navigation order: by section, then title. */
export function sortPages(pages: PanelPageInfo[]): PanelPageInfo[] {
  return [...pages].sort(
    (a, b) => a.section.localeCompare(b.section) || a.title.localeCompare(b.title) || a.id.localeCompare(b.id),
  );
}

export const pageHash = (id: string) => `#/p/${id}`;
