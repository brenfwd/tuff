import { describe, expect, test } from "bun:test";
import {
  cellText,
  compareCells,
  completionData,
  EXAMPLE_QUERIES,
  filenameFrom,
  flattenPlan,
  parseHistory,
  parseJsonCell,
  pushHistory,
  sortedIndices,
  toCsv,
  toTsv,
  upsertSaved,
} from "../src/sql";

describe("sql", () => {
  test("tsv flattens tabs and newlines and blanks nulls", () => {
    expect(toTsv(["a", "b"], [[1, "x\ty"], [null, "l1\nl2"]])).toBe("a\tb\n1\tx y\n\tl1 l2");
    expect(toTsv(null, [[1, 2]])).toBe("1\t2");
  });

  test("csv quotes when needed", () => {
    expect(toCsv(["n", "s"], [[1, 'say "hi"'], [null, "a,b"]])).toBe('n,s\r\n1,"say ""hi"""\r\n,"a,b"');
  });

  test("blob cells show hex and size", () => {
    expect(cellText({ blob: "00ff", size: 2 })).toBe("x'00ff' (2 bytes)");
    expect(cellText({ blob: "00ff", size: 100 })).toBe("x'00ff'... (100 bytes)");
    expect(cellText(null)).toBe("NULL");
  });

  test("sort orders nulls, numbers, text, blobs and is stable", () => {
    expect(compareCells(null, 1)).toBeLessThan(0);
    expect(compareCells(2, 10)).toBeLessThan(0);
    expect(compareCells(10, "a")).toBeLessThan(0);
    expect(compareCells("b", "a")).toBeGreaterThan(0);
    expect(compareCells({ blob: "", size: 1 }, "z")).toBeGreaterThan(0);
    const rows = [[3], [1], [null], [1]];
    expect(sortedIndices(rows, 0, false)).toEqual([2, 1, 3, 0]);
    expect(sortedIndices(rows, 0, true)).toEqual([0, 1, 3, 2]);
  });

  test("json detection only accepts objects and arrays", () => {
    expect(parseJsonCell('{"a":1}')).toEqual({ a: 1 });
    expect(parseJsonCell(" [1,2] ")).toEqual([1, 2]);
    expect(parseJsonCell("{nope")).toBeUndefined();
    expect(parseJsonCell("12")).toBeUndefined();
    expect(parseJsonCell(12)).toBeUndefined();
  });

  test("history dedupes, caps and ignores blanks", () => {
    expect(pushHistory(["a", "b"], " b ")).toEqual(["b", "a"]);
    expect(pushHistory(["a"], "  ")).toEqual(["a"]);
    expect(pushHistory(["a", "b", "c"], "d", 3)).toEqual(["d", "a", "b"]);
    expect(parseHistory('["x", 1]')).toEqual(["x"]);
    expect(parseHistory("garbage")).toEqual([]);
    expect(parseHistory(null)).toEqual([]);
  });

  test("completion data comes from the schema", () => {
    const d = completionData({
      objects: [{ name: "players", type: "table", columns: [{ name: "name", type: "TEXT", pk: true, notNull: true }] }],
      functions: [{ name: "nbt_json", signature: "nbt_json(blob)", description: "NBT as JSON" }],
    });
    expect(d.schema).toEqual({ players: ["name"] });
    expect(d.functions).toEqual([{ label: "nbt_json", detail: "nbt_json(blob)", info: "NBT as JSON" }]);
  });

  test("saved queries replace by name and sort", () => {
    expect(upsertSaved([{ name: "b", sql: "1" }, { name: "a", sql: "2" }], { name: "b", sql: "3" })).toEqual([
      { name: "a", sql: "2" },
      { name: "b", sql: "3" },
    ]);
  });

  test("plans flatten into an indented tree", () => {
    const flat = flattenPlan([
      { id: 2, parent: 0, detail: "SCAN a" },
      { id: 5, parent: 2, detail: "USE INDEX" },
      { id: 9, parent: 0, detail: "SCAN b" },
    ]);
    expect(flat.map((n) => [n.row.detail, n.depth])).toEqual([
      ["SCAN a", 0],
      ["USE INDEX", 1],
      ["SCAN b", 0],
    ]);
  });

  test("export filename and examples", () => {
    expect(filenameFrom('attachment; filename="q.csv"', "x")).toBe("q.csv");
    expect(filenameFrom(null, "x")).toBe("x");
    expect(EXAMPLE_QUERIES.length).toBe(5);
  });
});
