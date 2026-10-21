import { describe, expect, test } from "bun:test";
import { blockAt, levelFor, panBy, screenOf, tileKey, visibleTiles, zoomAt } from "../src/view";

describe("view math", () => {
  test("levels follow the scale and stop at the pyramid's ends", () => {
    expect(levelFor(8)).toBe(0);
    expect(levelFor(1)).toBe(0);
    expect(levelFor(0.75)).toBe(0);
    expect(levelFor(0.5)).toBe(1);
    expect(levelFor(0.25)).toBe(2);
    expect(levelFor(1 / 1024)).toBe(6);
  });

  test("visible tiles cover the canvas and sit where their blocks are", () => {
    const view = { dim: 0, x: 0, z: 0, scale: 1 };
    const tiles = visibleTiles(view, 800, 600);
    expect(tiles.map(tileKey).sort()).toEqual(["0/0/-1/-1", "0/0/-1/0", "0/0/0/-1", "0/0/0/0"]);
    const origin = tiles.find((t) => t.x === 0 && t.z === 0)!;
    expect([origin.left, origin.top, origin.size]).toEqual([400, 300, 512]);
    const far = visibleTiles({ ...view, scale: 0.25 }, 800, 600);
    expect(far.every((t) => t.level === 2 && t.size === 512)).toBe(true);
  });

  test("zooming keeps the block under the cursor still", () => {
    const view = { dim: 0, x: 100, z: -50, scale: 1 };
    const before = blockAt(view, 800, 600, 600, 100);
    const zoomed = zoomAt(view, 800, 600, 600, 100, 2);
    expect(zoomed.scale).toBe(2);
    expect(blockAt(zoomed, 800, 600, 600, 100)).toEqual(before);
    expect(zoomAt(view, 800, 600, 0, 0, 1e9).scale).toBe(8);
  });

  test("panning moves the world with the pointer", () => {
    const view = panBy({ dim: 0, x: 0, z: 0, scale: 2 }, 20, -10);
    expect([view.x, view.z]).toEqual([-10, 5]);
    expect(screenOf(view, 800, 600, -10, 5)).toEqual({ left: 400, top: 300 });
  });
});
