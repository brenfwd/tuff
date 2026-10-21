// Entry of the sandboxed plugin page frame (an opaque-origin iframe with no
// network access). It exposes the shared React runtime to the plugin module,
// loads the module from the source text the shell fetched for it, and relays
// the page context's calls to the shell over postMessage.
import "./index.css";
import { exposeShared } from "./sdk/shared";
import type { Mount, PanelContext, PanelEventHandler } from "./sdk/context";
import type { HostMessage } from "./frameBridge";

exposeShared();

const parentWindow = window.parent;
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
const handlers = new Map<number, { names: string[]; handler: PanelEventHandler }>();
let nextId = 1;
let started = false;

const send = (message: unknown) => parentWindow.postMessage(message, "*");

function context(init: Extract<HostMessage, { type: "init" }>): PanelContext {
  return Object.freeze({
    page: init.page,
    principal: init.principal as PanelContext["principal"],
    scopes: init.scopes,
    api: <T,>(path: string, request?: RequestInit) =>
      new Promise<T>((resolve, reject) => {
        const id = nextId++;
        pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
        const body = typeof request?.body === "string" ? request.body : undefined;
        send({ type: "api", id, path, method: request?.method, body });
      }),
    events: (topics: string[], names: string[], handler: PanelEventHandler) => {
      const sid = nextId++;
      handlers.set(sid, { names, handler });
      send({ type: "subscribe", sid, topics, names });
      return () => {
        if (handlers.delete(sid)) send({ type: "unsubscribe", sid });
      };
    },
    toast: (message: string, kind?: "info" | "error") => send({ type: "toast", message, kind }),
  });
}

function fail(message: string) {
  const p = document.createElement("p");
  p.textContent = message;
  p.className = "p-4 text-red-600";
  document.getElementById("root")!.replaceChildren(p);
}

async function start(init: Extract<HostMessage, { type: "init" }>) {
  document.documentElement.classList.toggle("dark", init.dark);
  const sheets: CSSStyleSheet[] = [];
  for (const text of init.styles) {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(text);
    sheets.push(sheet);
  }
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, ...sheets];
  const url = URL.createObjectURL(new Blob([init.module], { type: "text/javascript" }));
  try {
    const mod: { mount?: Mount } = await import(/* @vite-ignore */ url);
    if (typeof mod.mount !== "function") throw new Error("the page module exports no mount function");
    const host = document.createElement("div");
    document.getElementById("root")!.replaceChildren(host);
    mod.mount(host, context(init));
    new ResizeObserver(() => send({ type: "height", value: document.documentElement.scrollHeight })).observe(host);
  } catch (e) {
    fail(`${init.page.title} failed to load: ${e instanceof Error ? e.message : String(e)}`);
  } finally {
    URL.revokeObjectURL(url);
  }
}

window.addEventListener("message", (e: MessageEvent) => {
  if (e.source !== parentWindow) return;
  const msg = e.data as HostMessage;
  if (!msg || typeof msg !== "object") return;
  if (msg.type === "init") {
    if (started) return;
    started = true;
    void start(msg);
  } else if (msg.type === "api-result") {
    const call = pending.get(msg.id);
    if (!call) return;
    pending.delete(msg.id);
    if (msg.ok) call.resolve(msg.value);
    else call.reject(Object.assign(new Error(msg.message), { status: msg.status }));
  } else if (msg.type === "event") {
    const sub = handlers.get(msg.sid);
    if (sub && (msg.name === "resync" || sub.names.includes(msg.name))) sub.handler(msg.name, msg.data);
  }
});

send({ type: "ready" });
