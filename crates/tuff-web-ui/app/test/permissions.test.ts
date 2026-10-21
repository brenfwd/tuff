import { describe, expect, test } from "bun:test";
import { entries, fallbackLabel, filterNodes, validRole, validPattern } from "../src/permissions";

const node = (id: string, description = "", extra = {}) => ({
  id,
  description,
  default: false,
  op_bypass: true,
  op_level: 4,
  ...extra,
});

describe("permissions", () => {
  test("patterns follow the server rules", () => {
    expect(validPattern("tuff.web.access")).toBe(true);
    expect(validPattern("minecraft.command.*")).toBe(true);
    expect(validPattern("*")).toBe(true);
    expect(validPattern("single")).toBe(false);
    expect(validPattern("a.*.b")).toBe(false);
    expect(validPattern("Upper.case")).toBe(false);
    expect(validRole("mods")).toBe(true);
    expect(validRole("a.b")).toBe(false);
    expect(validRole("")).toBe(false);
  });

  test("nodes filter by every word", () => {
    const nodes = [node("minecraft.command.gamemode", "Use /gamemode"), node("tuff.web.access", "Use the panel")];
    expect(filterNodes(nodes, "").length).toBe(2);
    expect(filterNodes(nodes, "use panel").map((n) => n.id)).toEqual(["tuff.web.access"]);
    expect(filterNodes(nodes, "GAMEMODE").map((n) => n.id)).toEqual(["minecraft.command.gamemode"]);
  });

  test("labels and entries", () => {
    expect(fallbackLabel(node("a.b", "", { default: true }))).toBe("everyone");
    expect(fallbackLabel(node("a.b", "", { op_bypass: false }))).toBe("nobody (no operator bypass)");
    expect(fallbackLabel(node("a.b", "", { op_level: 2 }))).toBe("operators level 2+");
    expect(entries({ "b.c": true, "a.b": false })).toEqual([
      ["a.b", false],
      ["b.c", true],
    ]);
  });
});

import { candidates, effective, nodeTree } from "../src/permissions";

describe("permission tree", () => {
  test("candidates run from most to least specific", () => {
    expect(candidates("a.b.c")).toEqual(["a.b.c", "a.b.*", "a.*", "*"]);
    expect(candidates("a.b.*")).toEqual(["a.b.*", "a.*", "*"]);
    expect(candidates("*")).toEqual(["*"]);
  });

  test("effective settings come from the nearest pattern", () => {
    const nodes = { "a.*": true, "a.b.c": false };
    expect(effective(nodes, "a.b.c")).toEqual({ held: false, from: "a.b.c" });
    expect(effective(nodes, "a.b.d")).toEqual({ held: true, from: "a.*" });
    expect(effective(nodes, "x.y")).toBeNull();
  });

  test("nodes form a filtered tree", () => {
    const n = (id: string, description = "") => ({ id, description, default: false, op_bypass: true, op_level: 4 });
    const tree = nodeTree([n("tuff.web.access"), n("tuff.command.perm", "perms"), n("minecraft.command.give")]);
    expect(tree.map((b) => b.segment)).toEqual(["minecraft", "tuff"]);
    expect(tree[1].count).toBe(2);
    expect(tree[1].children.map((b) => b.path)).toEqual(["tuff.command", "tuff.web"]);
    expect(tree[1].children[0].children[0].node?.id).toBe("tuff.command.perm");
    expect(nodeTree([n("tuff.web.access"), n("tuff.command.perm", "perms")], "perms")[0].count).toBe(1);
  });
});
