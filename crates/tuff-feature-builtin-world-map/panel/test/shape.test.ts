import { describe, expect, test } from "bun:test";
import { chunkCount, containsChunk, dragTo, grab, handles } from "../src/shape";

describe("pre-generation shape", () => {
  test("chunk counts match the server's", () => {
    const cases: [number, number, number, number, number][] = [
      [0, 0, 100, 148, 196],
      [-1234, 567, 777, 7590, 9702],
      [5, -3, 1, 1, 1],
      [0, 0, 1000, 12522, 15876],
      [8, 8, 8, 3, 4],
      [8, 8, 7, 1, 1],
    ];
    for (const [x, z, radius, circle, square] of cases) {
      expect(chunkCount({ x, z, radius, shape: "circle" })).toBe(circle);
      expect(chunkCount({ x, z, radius, shape: "square" })).toBe(square);
    }
  });

  test("a circle leaves out the far corners", () => {
    const area = { x: 0, z: 0, radius: 100, shape: "circle" as const };
    expect(containsChunk(area, 6, 0)).toBe(true);
    expect(containsChunk(area, 7, 0)).toBe(false);
    expect(containsChunk(area, 4, 4)).toBe(true);
    expect(containsChunk(area, 5, 5)).toBe(false);
  });

  test("handles grab, the centre moves and the edges resize", () => {
    const view = { dim: 0, x: 0, z: 0, scale: 1 };
    const circle = { x: 0, z: 0, radius: 50, shape: "circle" as const };
    expect(handles(circle, view, 400, 400)).toHaveLength(4);
    expect(handles({ ...circle, shape: "square" }, view, 400, 400)).toHaveLength(8);
    expect(grab(circle, view, 400, 400, 200.5, 200.5)).toBe("centre");
    expect(grab(circle, view, 400, 400, 252, 200)).toBe("edge");
    expect(grab(circle, view, 400, 400, 230, 230)).toBeNull();
    expect(dragTo(circle, "centre", 30, -40)).toEqual({ ...circle, x: 30, z: -40 });
    expect(dragTo(circle, "edge", 30, 40).radius).toBe(50);
    expect(dragTo({ ...circle, shape: "square" }, "edge", 30, -70).radius).toBe(70);
    expect(dragTo(circle, "edge", 1e9, 0).radius).toBe(100_000);
  });
});
