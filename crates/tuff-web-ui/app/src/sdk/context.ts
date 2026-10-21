// What a plugin page module gets from the shell. Pure types, no imports, so
// plugin builds can import this file directly for type checking.

export interface PanelPrincipal {
  kind: "console" | "operator" | "token";
  name: string;
}

export interface PanelPageInfo {
  id: string;
  title: string;
  section: string;
  icon: string;
  scope: string;
  module: string;
  styles: string[];
}

export type PanelEventHandler = (event: string, data: string) => void;

export interface PanelContext {
  page: PanelPageInfo;
  principal: PanelPrincipal;
  scopes: string[];
  /** JSON request under /admin/api/v1 (plugin APIs are /plugins/<id>/...),
   * with the CSRF header on anything but GET. Rejects with an Error that
   * carries `status` on a non-2xx answer. */
  api<T = unknown>(path: string, init?: RequestInit): Promise<T>;
  /** Listens for events `names` on `topics` (plugin topics are
   * plugin.<page id>.<name>) over the page's one shared event stream.
   * A `resync` event means events were missed: refetch. Returns the
   * unsubscribe function. */
  events(topics: string[], names: string[], handler: PanelEventHandler): () => void;
  toast(message: string, kind?: "info" | "error"): void;
}

/** A plugin page module's export: render into `element`, optionally
 * returning a cleanup function called when the page is left. */
export type Mount = (element: HTMLElement, ctx: PanelContext) => void | (() => void);
