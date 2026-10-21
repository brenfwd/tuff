// The message protocol between the shell and the sandboxed plugin page frame.
// The frame is an opaque origin with no network: everything a plugin page can
// do to the panel goes through this bridge, which only forwards calls scoped
// to the page's own plugin.
import type { PanelPageInfo } from "./sdk/context";

export const MAX_SUBSCRIPTIONS = 32;
export const MAX_BODY = 4 * 1024 * 1024;
const METHODS = new Set(["GET", "POST", "PUT", "PATCH", "DELETE"]);

export interface ApiRequest {
  type: "api";
  id: number;
  path: string;
  method?: string;
  body?: string;
}

export type FrameMessage =
  | { type: "ready" }
  | ApiRequest
  | { type: "subscribe"; sid: number; topics: string[]; names: string[] }
  | { type: "unsubscribe"; sid: number }
  | { type: "toast"; message: string; kind?: string }
  | { type: "height"; value: number };

export type HostMessage =
  | { type: "init"; page: PanelPageInfo; principal: { kind: string; name: string }; scopes: string[]; module: string; styles: string[]; dark: boolean }
  | { type: "api-result"; id: number; ok: true; value: unknown }
  | { type: "api-result"; id: number; ok: false; status: number; message: string }
  | { type: "event"; sid: number; name: string; data: string };

/** True when `path` addresses the page's own plugin API, with no way to
 * climb out of it. */
export function allowedApiPath(pluginId: string, path: unknown): path is string {
  if (typeof path !== "string" || path.length > 2048) return false;
  if (/[\\#\u0000-\u001f]/.test(path)) return false;
  const prefix = `/plugins/${pluginId}`;
  if (path !== prefix && !path.startsWith(`${prefix}/`) && !path.startsWith(`${prefix}?`)) return false;
  const pathname = path.split("?")[0];
  let decoded: string;
  try {
    decoded = decodeURIComponent(pathname);
  } catch {
    return false;
  }
  if (/[\\#]/.test(decoded)) return false;
  const segments = decoded.split("/").slice(1);
  if (segments[0] !== "plugins" || segments[1] !== pluginId) return false;
  return segments.every((s, i) => i < 2 || (s !== "." && s !== ".."));
}

/** True when every topic is the page's own `plugin.<id>` event topic. */
export function allowedTopics(pluginId: string, topics: unknown): topics is string[] {
  if (!Array.isArray(topics) || topics.length === 0 || topics.length > 16) return false;
  const own = `plugin.${pluginId}`;
  return topics.every(
    (t) => typeof t === "string" && t.length <= 128 && (t === own || (t.startsWith(`${own}.`) && !/[,\s]/.test(t))),
  );
}

function strings(value: unknown): value is string[] {
  return Array.isArray(value) && value.length <= 32 && value.every((v) => typeof v === "string" && v.length <= 128 && !/[,\s]/.test(v));
}

export interface BridgeDeps {
  page: PanelPageInfo;
  post(message: HostMessage): void;
  api(path: string, init: RequestInit): Promise<unknown>;
  subscribe(topics: string[], names: string[], handler: (name: string, data: string) => void): () => void;
  toast(message: string, kind?: "info" | "error"): void;
  setHeight(value: number): void;
}

/** The shell's end of the bridge for one mounted page frame. */
export class HostBridge {
  private subs = new Map<number, () => void>();

  constructor(private readonly deps: BridgeDeps) {}

  handle(raw: unknown) {
    if (!raw || typeof raw !== "object") return;
    const msg = raw as FrameMessage;
    switch (msg.type) {
      case "api":
        void this.api(msg);
        break;
      case "subscribe":
        this.subscribe(msg);
        break;
      case "unsubscribe":
        this.subs.get(msg.sid)?.();
        this.subs.delete(msg.sid);
        break;
      case "toast":
        if (typeof msg.message === "string") {
          this.deps.toast(msg.message.slice(0, 500), msg.kind === "error" ? "error" : "info");
        }
        break;
      case "height":
        if (typeof msg.value === "number" && Number.isFinite(msg.value)) {
          this.deps.setHeight(Math.min(Math.max(Math.ceil(msg.value), 0), 100000));
        }
        break;
    }
  }

  close() {
    for (const off of this.subs.values()) off();
    this.subs.clear();
  }

  private async api(msg: ApiRequest) {
    const { id } = msg;
    const fail = (status: number, message: string) => this.deps.post({ type: "api-result", id, ok: false, status, message });
    if (!allowedApiPath(this.deps.page.id, msg.path)) return fail(403, "the page may only call its own plugin API");
    const method = (msg.method ?? "GET").toUpperCase();
    if (!METHODS.has(method)) return fail(405, "method not allowed");
    if (msg.body !== undefined && (typeof msg.body !== "string" || msg.body.length > MAX_BODY)) {
      return fail(400, "bad request body");
    }
    const init: RequestInit = { method };
    if (msg.body !== undefined) init.body = msg.body;
    try {
      const value = await this.deps.api(msg.path, init);
      this.deps.post({ type: "api-result", id, ok: true, value });
    } catch (e) {
      const status = e && typeof e === "object" && "status" in e ? Number((e as { status: unknown }).status) : 0;
      fail(status, e instanceof Error ? e.message : String(e));
    }
  }

  private subscribe(msg: { sid: number; topics: unknown; names: unknown }) {
    const { sid } = msg;
    if (typeof sid !== "number" || this.subs.has(sid) || this.subs.size >= MAX_SUBSCRIPTIONS) return;
    if (!allowedTopics(this.deps.page.id, msg.topics) || !strings(msg.names)) return;
    const off = this.deps.subscribe(msg.topics, msg.names, (name, data) =>
      this.deps.post({ type: "event", sid, name, data }),
    );
    this.subs.set(sid, off);
  }
}
