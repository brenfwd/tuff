import { describe, expect, test } from "bun:test";
import { age, auditMatches, auditQuery, borderLabel, borderRules, dayTime, expiryLabel, prependLive, validTokenName } from "../src/access";

const entry = (principal: string, action: string) => ({ ts: 1, principal, remote: "", action, target: "", detail: "", outcome: "ok" });

describe("access", () => {
  test("audit queries carry filters", () => {
    expect(auditQuery({ principal: "", action: "" }, 100)).toBe("/audit?limit=100");
    expect(auditQuery({ principal: " Alex ", action: "config." }, 50, 9)).toBe("/audit?limit=50&before=9&principal=Alex&action=config.");
  });

  test("live entries follow the filter", () => {
    const f = { principal: "alex", action: "config." };
    expect(auditMatches(entry("Alex", "config.set"), f)).toBe(true);
    expect(auditMatches(entry("Alex", "configx"), f)).toBe(false);
    expect(auditMatches(entry("Steve", "config.set"), f)).toBe(false);
    expect(auditMatches(entry("Steve", "login"), { principal: "", action: "login" })).toBe(true);
    const list = prependLive([], JSON.stringify(entry("Alex", "config.set")), f);
    expect(list.length).toBe(1);
    expect(prependLive(list, "nope", f)).toBe(list);
    expect(prependLive(list, JSON.stringify(entry("Alex", "login")), f)).toBe(list);
  });

  test("ages, names and day time", () => {
    expect(age(0, 59_000)).toBe("59s");
    expect(age(0, 3_600_000)).toBe("1h");
    expect(age(0, 90_000_000)).toBe("1d");
    expect(expiryLabel(null, 5)).toBe("never");
    expect(expiryLabel(5, 5)).toBe("expired");
    expect(expiryLabel(7_200_000, 0)).toBe("in 2h");
    expect(validTokenName("ci-bot.1")).toBe(true);
    expect(validTokenName("has space")).toBe(false);
    expect(dayTime(48_100)).toBe(100);
    expect(dayTime(null)).toBeNull();
  });
  test("borders show a running size change", () => {
    const border = { center: [0.5, -3] as [number, number], size: 59999968, target: 59999968, lerpTicks: 0, damagePerBlock: 0.2, safeZone: 5, warningBlocks: 5, warningTicks: 300 };
    expect(borderLabel(border)).toBe("59999968 wide at 0.5, -3");
    expect(borderLabel({ ...border, size: 120.25, target: 20, lerpTicks: 600 })).toBe("120.3 wide at 0.5, -3, shrinking to 20 over 30s");
    expect(borderLabel({ ...border, size: 20, target: 100, lerpTicks: 30 })).toBe("20 wide at 0.5, -3, growing to 100 over 1.5s");
    expect(borderRules(border)).toBe("0.2 damage per block past 5; warns at 5 blocks or 15s");
  });
});
