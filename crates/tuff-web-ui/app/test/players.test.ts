import { describe, expect, test } from "bun:test";
import { applyEvent, formatPosition, packLabel, validIp, validName, validText } from "../src/players";

const row = (name: string, extra = {}) => ({
  uuid: `u-${name}`,
  name,
  ping: 1,
  gameMode: "survival",
  dimension: "minecraft:overworld",
  position: [0, 64, 0] as [number, number, number],
  opLevel: 0,
  brand: "vanilla",
  pack: null,
  ...extra,
});

describe("players", () => {
  test("inputs follow the server rules", () => {
    expect(validName("Alex_01")).toBe(true);
    expect(validName("Alex op")).toBe(false);
    expect(validName("@a")).toBe(false);
    expect(validName("seventeen_letters")).toBe(false);
    expect(validText("griefing")).toBe(true);
    expect(validText("a\nop Steve")).toBe(false);
    expect(validText("  ")).toBe(false);
    expect(validText("x".repeat(257))).toBe(false);
    expect(validIp("10.0.0.1")).toBe(true);
    expect(validIp("::1")).toBe(true);
    expect(validIp("300.0.0.1")).toBe(false);
    expect(validIp("Alex")).toBe(false);
  });

  test("events join, update and leave", () => {
    let online = applyEvent([], "join", JSON.stringify(row("Bob")));
    online = applyEvent(online, "join", JSON.stringify(row("Alex")));
    expect(online.map((p) => p.name)).toEqual(["Alex", "Bob"]);
    online = [{ ...online[0], address: "127.0.0.1" }, online[1]];
    online = applyEvent(online, "update", JSON.stringify(row("Alex", { gameMode: "creative" })));
    expect(online[0].gameMode).toBe("creative");
    expect(online[0].address).toBe("127.0.0.1");
    online = applyEvent(online, "leave", JSON.stringify({ uuid: "u-Bob", name: "Bob" }));
    expect(online.map((p) => p.name)).toEqual(["Alex"]);
    expect(applyEvent(online, "join", "not json")).toBe(online);
  });

  test("labels", () => {
    expect(formatPosition([1, 2.25, -3])).toBe("1.0, 2.3, -3.0");
    expect(formatPosition(null)).toBe("");
    expect(packLabel("failed_download")).toBe("failed download");
    expect(packLabel(null)).toBe("none");
  });
});
