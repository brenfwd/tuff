import { expect, test } from "bun:test";
import {
  filterTree,
  formatNs,
  hit,
  isRegionTag,
  layout,
  pathTo,
  regionsOf,
  rootOf,
  search,
  threadGroups,
  TimingsNode,
} from "../src/flame";

const node = (name: string, total: number, self: number, children: TimingsNode[] = [], extra: Partial<TimingsNode> = {}): TimingsNode => ({
  name,
  count: 1,
  total_ns: total,
  self_ns: self,
  min_ns: 0,
  max_ns: 0,
  p50_ns: 0,
  p90_ns: 0,
  p99_ns: 0,
  children,
  ...extra,
});

const tree = (): TimingsNode[] => [
  node("server.tick", 100, 20, [
    node("region.tick", 50, 10, [node("entity", 40, 40, [], { tag: "pig" })], { tag: "minecraft:overworld 0,0" }),
    node("region.tick", 30, 30, [], { tag: "minecraft:overworld -1,2" }),
  ]),
  node("session.packet", 500, 500, [], { async: true }),
];

test("region tags are found and recognized", () => {
  expect(isRegionTag("minecraft:overworld -1,2")).toBe(true);
  expect(isRegionTag("pig")).toBe(false);
  expect(isRegionTag(undefined)).toBe(false);
  expect(regionsOf(tree())).toEqual(["minecraft:overworld -1,2", "minecraft:overworld 0,0"]);
});

test("filters drop async nodes and keep one region with recomputed ancestors", () => {
  const roots = tree();
  const sync = filterTree(roots, { async: false });
  expect(sync.map((n) => n.name)).toEqual(["server.tick"]);
  expect(sync[0]).toBe(roots[0]);
  const one = filterTree(roots, { async: false, region: "minecraft:overworld 0,0" });
  expect(one).toHaveLength(1);
  expect(one[0].total_ns).toBe(50);
  expect(one[0].self_ns).toBe(0);
  expect(one[0].children!.map((c) => c.tag)).toEqual(["minecraft:overworld 0,0"]);
  expect(one[0].children![0]).toBe(roots[0].children![0]);
  expect(filterTree(roots, { async: true, region: "nowhere 9,9" })).toEqual([]);
});

test("layout splits each parent by child totals and scales oversized children", () => {
  const root = rootOf(tree());
  expect(root.total_ns).toBe(600);
  const { rects, depth } = layout(root);
  expect(depth).toBe(3);
  const tick = rects.find((r) => r.node.name === "server.tick")!;
  expect(tick.x).toBe(0);
  expect(tick.w).toBeCloseTo(100 / 600);
  const second = rects.find((r) => r.node.tag === "minecraft:overworld -1,2")!;
  expect(second.x).toBeCloseTo(50 / 600);
  expect(second.w).toBeCloseTo(30 / 600);
  const parent = node("p", 10, 0, [node("a", 30, 30, [], { async: true }), node("b", 10, 10)]);
  const scaled = layout(parent).rects;
  expect(scaled[1].w).toBeCloseTo(0.75);
  expect(scaled[2].x).toBeCloseTo(0.75);
  expect(scaled[2].w).toBeCloseTo(0.25);
  expect(layout(root, 0.1).rects.map((r) => r.node.name)).toEqual(["all", "server.tick", "session.packet"]);
});

test("hits find the rect under a point", () => {
  const { rects } = layout(rootOf(tree()));
  expect(hit(rects, 0, 0.5)?.node.name).toBe("all");
  expect(hit(rects, 1, 0.01)?.node.name).toBe("server.tick");
  expect(hit(rects, 1, 0.5)?.node.name).toBe("session.packet");
  expect(hit(rects, 3, 0.9)).toBeUndefined();
});

test("paths lead from the root to a node for breadcrumbs", () => {
  const root = rootOf(tree());
  const pig = root.children![0].children![0].children![0];
  expect(pathTo(root, pig).map((n) => n.name)).toEqual(["all", "server.tick", "region.tick", "entity"]);
  expect(pathTo(root, node("x", 1, 1))).toEqual([]);
});

test("search matches labels and counts nested time once", () => {
  const root = rootOf(tree());
  const found = search(root, "TICK");
  expect(found.matches.size).toBe(3);
  expect(found.total).toBe(100);
  expect(search(root, "pig").total).toBe(40);
  expect(search(root, "  ").matches.size).toBe(0);
});

test("durations format by magnitude", () => {
  expect(formatNs(950)).toBe("950 ns");
  expect(formatNs(1_500)).toBe("1.50 us");
  expect(formatNs(25_000_000)).toBe("25.0 ms");
  expect(formatNs(3_000_000_000)).toBe("3.00 s");
});

test("threads are grouped by pool and sorted by name", () => {
  const thread = (index: number, name: string) => ({ index, name, dropped: 0, total_ns: 1_000, roots: [] });
  const groups = threadGroups([
    thread(0, "tokio-worker-10"),
    thread(1, "rayon-2"),
    thread(2, "tokio-worker-2"),
    thread(3, "main"),
    thread(4, "tokio-blocking-0"),
    thread(5, "rayon-map-0"),
    thread(6, "tokio-blocking-0"),
  ]);
  expect(groups.map((g) => g.name)).toEqual(["main", "rayon", "rayon-map", "tokio-blocking", "tokio-worker"]);
  expect(groups[4].threads.map((t) => t.value)).toEqual(["2", "0"]);
  expect(groups[4].threads[0].label).toBe("tokio-worker-2 (1.00 us)");
  expect(groups[3].threads.map((t) => t.label)).toEqual(["tokio-blocking-0 #4 (1.00 us)", "tokio-blocking-0 #6 (1.00 us)"]);
});
