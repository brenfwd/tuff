import { expect, test } from "bun:test";
import { allowedApiPath, allowedTopics, HostBridge, type HostMessage } from "../src/frameBridge";
import type { PanelPageInfo } from "../src/sdk/context";

const page: PanelPageInfo = { id: "gold", title: "Gold", section: "x", icon: "", scope: "", module: "", styles: [] };

test("only the page's own plugin API paths pass", () => {
  for (const ok of ["/plugins/gold", "/plugins/gold/count", "/plugins/gold/a/b?x=1", "/plugins/gold?x=1"]) {
    expect(allowedApiPath("gold", ok)).toBe(true);
  }
  for (const bad of [
    "/session",
    "/tokens",
    "/plugins/gold2/x",
    "/plugins/other/x",
    "/plugins/gold/../../tokens",
    "/plugins/gold/%2e%2e/%2e%2e/tokens",
    "/plugins/gold/%2E%2E/tokens",
    "/plugins/gold/..%2ftokens",
    "/plugins/gold/x\\y",
    "/plugins/gold/x#y",
    "//evil.example/plugins/gold",
    "/plugins/%67old/x/../../../sql",
    42,
    undefined,
  ]) {
    expect(allowedApiPath("gold", bad)).toBe(false);
  }
});

test("only the page's own event topics pass", () => {
  expect(allowedTopics("gold", ["plugin.gold.count", "plugin.gold"])).toBe(true);
  for (const bad of [[], ["logs"], ["plugin.gold2.x"], ["plugin.gold.x,logs"], ["plugin.other.x"], "plugin.gold.x", [1]]) {
    expect(allowedTopics("gold", bad)).toBe(false);
  }
});

function bridge() {
  const posted: HostMessage[] = [];
  const calls: [string, RequestInit][] = [];
  const toasts: string[] = [];
  const subs: { topics: string[]; off: boolean; fire: (n: string, d: string) => void }[] = [];
  const b = new HostBridge({
    page,
    post: (m) => posted.push(m),
    api: async (path, init) => {
      calls.push([path, init]);
      if (path.endsWith("/boom")) throw Object.assign(new Error("nope"), { status: 409 });
      return { ok: true };
    },
    subscribe: (topics, _names, handler) => {
      const s = { topics, off: false, fire: handler };
      subs.push(s);
      return () => (s.off = true);
    },
    toast: (m) => toasts.push(m),
    setHeight: () => {},
  });
  return { b, posted, calls, toasts, subs };
}

test("the bridge forwards own API calls and refuses the rest", async () => {
  const { b, posted, calls } = bridge();
  b.handle({ type: "api", id: 1, path: "/plugins/gold/count", method: "post", body: "{}" });
  b.handle({ type: "api", id: 2, path: "/tokens" });
  b.handle({ type: "api", id: 3, path: "/plugins/gold/x", method: "TRACE" });
  b.handle({ type: "api", id: 4, path: "/plugins/gold/boom" });
  await new Promise((r) => setTimeout(r, 0));
  expect(calls.map((c) => c[0])).toEqual(["/plugins/gold/count", "/plugins/gold/boom"]);
  expect(calls[0][1]).toEqual({ method: "POST", body: "{}" });
  const byId = new Map(posted.map((m) => [(m as { id: number }).id, m]));
  expect(byId.get(1)).toMatchObject({ ok: true });
  expect(byId.get(2)).toMatchObject({ ok: false, status: 403 });
  expect(byId.get(3)).toMatchObject({ ok: false, status: 405 });
  expect(byId.get(4)).toMatchObject({ ok: false, status: 409, message: "nope" });
});

test("the bridge scopes subscriptions and cleans them up", () => {
  const { b, posted, subs } = bridge();
  b.handle({ type: "subscribe", sid: 1, topics: ["logs"], names: ["line"] });
  expect(subs.length).toBe(0);
  b.handle({ type: "subscribe", sid: 2, topics: ["plugin.gold.count"], names: ["n"] });
  expect(subs.length).toBe(1);
  subs[0].fire("n", "5");
  expect(posted).toEqual([{ type: "event", sid: 2, name: "n", data: "5" }]);
  b.close();
  expect(subs[0].off).toBe(true);
});
