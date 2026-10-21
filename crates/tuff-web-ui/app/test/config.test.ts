import { describe, expect, test } from "bun:test";
import { buildTree, groupSettings, invalid, isDefault, matches, Property, replaceEntry, savedMessage, Setting, textFormat } from "../src/config";

const setting = (name: string, extra: Partial<Setting> = {}): Setting => ({
  name,
  kind: "string",
  description: "",
  hints: [],
  runtimeOnly: false,
  plugin: "core",
  group: name.slice(0, name.lastIndexOf(".")),
  secret: false,
  lockout: false,
  value: "",
  default: "",
  ...extra,
});

describe("config", () => {
  test("text component entries get the MiniMessage field", () => {
    expect(textFormat(setting("tuff.anticheat.kickMessage", { textComponent: true }))).toBe("minimessage");
    expect(textFormat(setting("plugins.x.token", { textComponent: true, secret: true }))).toBeNull();
    expect(textFormat(setting("tuff.web.hostnames"))).toBeNull();
    const prop = (name: string) => ({ name, known: true, present: true, effect: "live", effectNote: "", secret: false, kind: "string" }) as unknown as Property;
    expect(textFormat(prop("motd"))).toBe("section");
    expect(textFormat(prop("level-name"))).toBeNull();
  });

  test("values are checked by kind", () => {
    expect(invalid("bool", "true")).toBeNull();
    expect(invalid("bool", "yes")).not.toBeNull();
    expect(invalid("int", "-3", { min: -5, max: 5 })).toBeNull();
    expect(invalid("int", "6", { min: -5, max: 5 })).toBe("At most 5.");
    expect(invalid("int", "1.5")).not.toBeNull();
    expect(invalid("port", "65535")).toBeNull();
    expect(invalid("port", "65536")).not.toBeNull();
    expect(invalid("ip", "::")).toBeNull();
    expect(invalid("ip", "host")).not.toBeNull();
    expect(invalid("url", "https://a.example/x")).toBeNull();
    expect(invalid("url", "a.example")).not.toBeNull();
    expect(invalid("choice", "HARD", { options: ["easy", "hard"] })).toBeNull();
    expect(invalid("choice", "brutal", { options: ["easy", "hard"] })).not.toBeNull();
    expect(invalid("string", "a\nwhite-list=false")).not.toBeNull();
    expect(invalid("string", "x".repeat(4097))).not.toBeNull();
  });

  test("search matches every word", () => {
    const s = setting("tuff.web.hostnames", { description: "Extra host names", plugin: "core" });
    expect(matches(s, "web host")).toBe(true);
    expect(matches(s, "WEB")).toBe(true);
    expect(matches(s, "web port")).toBe(false);
    expect(matches(s, "  ")).toBe(true);
  });

  test("settings group and sort", () => {
    const grouped = groupSettings([setting("tuff.web.z"), setting("a.b"), setting("tuff.web.a")]);
    expect(grouped.map(([g]) => g)).toEqual(["a", "tuff.web"]);
    expect(grouped[1][1].map((s) => s.name)).toEqual(["tuff.web.a", "tuff.web.z"]);
  });

  test("defaults and secrets", () => {
    expect(isDefault({ value: "1", default: "1" })).toBe(true);
    expect(isDefault({ value: "2", default: "1" })).toBe(false);
    expect(isDefault({ secret: true, set: true })).toBe(false);
    expect(isDefault({ secret: true, set: false })).toBe(true);
    expect(isDefault({ value: "x", default: null })).toBe(true);
    const list = [setting("a.b", { value: "1" }), setting("a.c")];
    expect(replaceEntry(list, setting("a.b", { value: "2" }))[0].value).toBe("2");
  });

  test("keys group into a dotted tree with every key once", () => {
    const names = ["tuff.a.b.c", "tuff.a.b", "tuff.a.d", "tuff.z", "other"];
    const tree = buildTree(names.map((n) => setting(n)));
    expect(tree.map((n) => n.segment)).toEqual(["other", "tuff"]);
    const tuff = tree[1];
    expect(tuff.count).toBe(4);
    expect(tuff.children.map((n) => n.segment)).toEqual(["a", "z"]);
    const b = tuff.children[0].children[0];
    expect(b.path).toBe("tuff.a.b");
    expect(b.entry?.name).toBe("tuff.a.b");
    expect(b.count).toBe(2);
    const leaves: string[] = [];
    const walk = (ns: typeof tree) => ns.forEach((n) => (n.entry && leaves.push(n.entry.name), walk(n.children)));
    walk(tree);
    expect(leaves.sort()).toEqual([...names].sort());
  });
});

describe("savedMessage", () => {
  const property = (extra: Partial<Property>): Property => ({
    name: "level-seed",
    known: true,
    present: true,
    effect: "live",
    effectNote: "applies at once",
    secret: false,
    kind: "string",
    ...extra,
  });

  test("a live key is just saved", () => {
    expect(savedMessage(property({ name: "motd" }))).toBe("Saved motd.");
  });

  test("other keys say what setting them does", () => {
    const p = property({ effect: "restart", effectNote: "needs a restart: the listener is bound at start" });
    expect(savedMessage(p)).toBe("Saved level-seed (needs a restart: the listener is bound at start).");
  });

  test("the server's note wins", () => {
    const p = property({ effect: "creation", note: "Saved. level-seed only applies when a new world is created." });
    expect(savedMessage(p)).toBe("level-seed: Saved. level-seed only applies when a new world is created.");
  });
});
