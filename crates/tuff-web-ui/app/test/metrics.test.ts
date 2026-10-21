import { expect, test } from "bun:test";
import { append, formatBytes, formatUnit, formatUptime, seriesOf, Snapshot } from "../src/metrics";
import { extent, niceTicks, project } from "../src/charts";

const snapshot = (over: Partial<Snapshot> = {}): Snapshot => ({
  unix_ms: 1000,
  uptime_s: 5,
  tick: null,
  tick_stale: false,
  world: { regions: 2, chunks: 9, loading: 0, generating: 1, entities: 4, block_entities: 0, scheduled_ticks: 0, players: 1 },
  slowest_regions: [],
  manager: null,
  chunk_io: null,
  runtime: null,
  memory: null,
  cpu: null,
  series: { "net.bytes_in": { kind: "counter", value: 500, rate: 25 }, "net.connections.play": { kind: "gauge", value: 3, rate: 0 } },
  dropped_reports: 0,
  ...over,
});

test("series values mirror the server's history", () => {
  const out = seriesOf(snapshot());
  expect(out["net.bytes_in"]).toBe(25);
  expect(out["net.connections.play"]).toBe(3);
  expect(out["world.chunks"]).toBe(9);
  expect(out["players"]).toBe(1);
  expect(out["tick.mspt"]).toBeUndefined();
  const tick = { tick: 20, tps: 20, mspt_mean: 3, mspt_max: 9, mspt_p95: 7, target_rate: 20, frozen: false, sprinting: false, behind: 0, players: [] };
  const ticked = seriesOf(snapshot({ tick, memory: { rss_bytes: 10, peak_rss_bytes: null } }));
  expect(ticked["tick.mspt.p95"]).toBe(7);
  expect(ticked["players"]).toBe(0);
  expect(ticked["memory.rss"]).toBe(10);
  expect(ticked["process.cpu"]).toBeUndefined();
  expect(seriesOf(snapshot({ cpu: { cores: 2.3, host_cores: 8 } }))["process.cpu"]).toBe(2.3);
  expect(seriesOf(snapshot({ tick, tick_stale: true }))["tick.tps"]).toBeUndefined();
});

test("append keeps time order and the window", () => {
  let points = append([], [1000, 1, 1], 5000);
  points = append(points, [1000, 2, 2], 5000);
  expect(points).toEqual([[1000, 1, 1]]);
  points = append(points, [3000, 2, 2], 5000);
  points = append(points, [7000, 3, 3], 5000);
  expect(points.map((p) => p[0])).toEqual([3000, 7000]);
});

test("formatting", () => {
  expect(formatBytes(512)).toBe("512 B");
  expect(formatBytes(1536)).toBe("1.5 KiB");
  expect(formatBytes(300 * 1024 * 1024)).toBe("300 MiB");
  expect(formatUnit(2048, "bytes", true)).toBe("2.0 KiB/s");
  expect(formatUnit(12.345, "millis")).toBe("12.3 ms");
  expect(formatUnit(12345, "count")).toBe("12.3k");
  expect(formatUnit(2.3, "cores")).toBe("2.30 cores");
  expect(formatUnit(12.34, "cores")).toBe("12.3 cores");
  expect(formatUptime(3725)).toBe("1h 2m");
  expect(formatUptime(90061)).toBe("1d 1h");
});

test("extent covers data and thresholds", () => {
  expect(extent([])).toEqual({ min: 0, max: 1 });
  expect(extent([[[0, 3, 3], [1, 8, 8]]], [50])).toEqual({ min: 0, max: 50 });
  expect(extent([[[0, -2, -2], [1, 4, 4]]])).toEqual({ min: -2, max: 4 });
  expect(extent([[[0, 0, 0]]])).toEqual({ min: 0, max: 1 });
});

test("nice ticks are round", () => {
  expect(niceTicks({ min: 0, max: 50 })).toEqual([0, 20, 40]);
  expect(niceTicks({ min: 0, max: 1 }, 5)).toEqual([0, 0.2, 0.4, 0.6, 0.8, 1]);
});

test("projection maps time and value into the box", () => {
  const xy = project([[0, 0, 0], [10, 5, 5]], 0, 10, { min: 0, max: 10 }, 100, 50);
  expect(xy).toEqual([[0, 50], [100, 25]]);
});
