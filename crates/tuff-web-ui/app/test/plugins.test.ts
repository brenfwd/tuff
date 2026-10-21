import { expect, test } from "bun:test";
import * as React from "react";
import * as JsxRuntime from "react/jsx-runtime";
import { eventsUrl, makeContext, PageEvents, pageHash, sortPages } from "../src/plugins";
import type { PanelPageInfo } from "../src/sdk/context";
import { exposeShared, SHARED_KEY } from "../src/sdk/shared";

class FakeSource {
  listeners = new Map<string, (e: MessageEvent) => void>();
  closed = false;
  constructor(public url: string) {}
  addEventListener(name: string, fn: (e: MessageEvent) => void) {
    this.listeners.set(name, fn);
  }
  close() {
    this.closed = true;
  }
  emit(name: string, data: string) {
    this.listeners.get(name)?.({ data } as MessageEvent);
  }
}

function fakes() {
  const opened: FakeSource[] = [];
  const events = new PageEvents((url) => {
    const s = new FakeSource(url);
    opened.push(s);
    return s;
  });
  return { opened, events };
}

test("one stream per page over the union of topics", () => {
  const { opened, events } = fakes();
  const a: string[] = [];
  const b: string[] = [];
  const stopA = events.subscribe(["plugin.x.count"], ["count"], (n, d) => a.push(`${n}:${d}`));
  expect(opened.length).toBe(1);
  expect(opened[0].url).toBe(eventsUrl(["plugin.x.count"]));
  events.subscribe(["plugin.x.count"], ["count"], (n, d) => b.push(`${n}:${d}`));
  expect(opened.length).toBe(1);
  events.subscribe(["logs"], ["log"], () => {});
  expect(opened.length).toBe(2);
  expect(opened[0].closed).toBe(true);
  expect(opened[1].url).toBe(`/admin/events?topics=${encodeURIComponent("logs,plugin.x.count")}`);
  opened[1].emit("count", "1");
  opened[1].emit("resync", "{}");
  expect(a).toEqual(["count:1", "resync:{}"]);
  expect(b).toEqual(["count:1", "resync:{}"]);
  stopA();
  expect(opened.length).toBe(2);
  opened[1].emit("count", "2");
  expect(a.length).toBe(2);
  events.close();
  expect(opened[1].closed).toBe(true);
  expect(opened.length).toBe(2);
});

test("no stream without topics", () => {
  const { opened, events } = fakes();
  const stop = events.subscribe([], ["x"], () => {});
  expect(opened.length).toBe(0);
  stop();
  stop();
  expect(opened.length).toBe(0);
});

const page = (id: string, section: string, title: string): PanelPageInfo => ({
  id,
  title,
  section,
  icon: "",
  scope: "x.read",
  module: `/admin/pages/${id}/main.js`,
  styles: [],
});

test("pages sort by section then title", () => {
  const sorted = sortPages([page("c", "B", "A"), page("b", "A", "Z"), page("a", "A", "Y")]);
  expect(sorted.map((p) => p.id)).toEqual(["a", "b", "c"]);
  expect(pageHash("gold")).toBe("#/p/gold");
});

test("the context is frozen and carries the session", () => {
  const { events } = fakes();
  const toasts: string[] = [];
  const ctx = makeContext(
    page("gold", "Plugins", "Gold"),
    { principal: { kind: "console", name: "console" }, level: 4, csrf: "t", scopes: ["x.read"] },
    (m) => toasts.push(m),
    events,
  );
  expect(Object.isFrozen(ctx)).toBe(true);
  expect(ctx.principal.name).toBe("console");
  expect(ctx.page.id).toBe("gold");
  ctx.toast("hi");
  expect(toasts).toEqual(["hi"]);
});

test("shims hand plugins the shell's React", async () => {
  exposeShared();
  expect(Object.isFrozen((globalThis as Record<string, unknown>)[SHARED_KEY])).toBe(true);
  const shim = await import("../src/sdk/shims/react");
  const runtime = await import("../src/sdk/shims/jsx-runtime");
  expect(shim.useState).toBe(React.useState);
  expect(shim.default).toBe((globalThis as unknown as { __tuffPanel: { React: unknown } }).__tuffPanel.React);
  expect(runtime.jsx).toBe(JsxRuntime.jsx);
});
