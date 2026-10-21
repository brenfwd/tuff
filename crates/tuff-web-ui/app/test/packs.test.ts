import { describe, expect, test } from "bun:test";
import { compatibilityLabel, DataPack, formatRange, kindLabel, packUploadProblem, sortDataPacks, statusLabel } from "../src/packs";

function dp(name: string, position: number | null): DataPack {
  return { id: name, name, title: name, source: "world", kind: "zip", required: false, selected: position !== null, position, meta: null, error: null };
}

describe("packs", () => {
  test("enabled packs list highest priority first", () => {
    const sorted = sortDataPacks([dp("b", null), dp("vanilla", 0), dp("a", null), dp("top", 1)]);
    expect(sorted.map((p) => p.name)).toEqual(["top", "vanilla", "a", "b"]);
  });

  test("format ranges read compactly", () => {
    expect(formatRange([[88, 0], [88, 0]])).toBe("88");
    expect(formatRange([[80, 0], [88, 2147483647]])).toBe("80-88");
    expect(formatRange([[69, 1], [69, 1]])).toBe("69.1");
    expect(formatRange([null, null])).toBe("unknown");
    expect(compatibilityLabel("too_new")).toContain("newer");
  });

  test("player statuses flag older versions", () => {
    expect(statusLabel({ id: "x", status: "loaded", loaded: true, current: true })).toBe("Loaded");
    expect(statusLabel({ id: "x", status: null, loaded: true, current: false })).toContain("older");
    expect(statusLabel({ id: "x", status: "declined", loaded: false, current: true })).toBe("Declined");
    expect(kindLabel("custom")).toBe("Custom content");
  });

  test("uploads are checked before they start", () => {
    expect(packUploadProblem("a.txt", 5, 10)).toContain(".zip");
    expect(packUploadProblem("a.zip", 0, 10)).toContain("empty");
    expect(packUploadProblem("a.ZIP", 11, 10)).toContain("limit");
    expect(packUploadProblem("a.zip", 10, 10)).toBeNull();
  });
});
