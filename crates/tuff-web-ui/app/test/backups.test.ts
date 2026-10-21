import { describe, expect, test } from "bun:test";
import { confirmed, errorText, formatBytes, percent, phaseLabel, restorable, uploadProblem } from "../src/backups";

describe("backups", () => {
  test("sizes read in binary units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(16 * 1024 ** 3)).toBe("16.0 GiB");
    expect(formatBytes(200 * 1024 ** 2)).toBe("200 MiB");
  });

  test("progress is clamped to whole percents", () => {
    expect(percent(0, 0)).toBe(0);
    expect(percent(1, 3)).toBe(33);
    expect(percent(5, 3)).toBe(100);
    expect(phaseLabel("compressing")).toBe("Compressing");
    expect(phaseLabel("other")).toBe("other");
  });

  test("restores match the world's storage", () => {
    expect(restorable("folder", "world-20261003-120000.zip")).toBe(true);
    expect(restorable("folder", "world-20261003-120000.mcserver")).toBe(false);
    expect(restorable("server_file", "world-20261003-120000.mcserver")).toBe(true);
    expect(restorable(null, "world-20261003-120000.zip")).toBe(false);
  });

  test("uploads are checked before they start", () => {
    expect(uploadProblem(0, 10)).toContain("empty");
    expect(uploadProblem(11, 10)).toContain("limit");
    expect(uploadProblem(10, 10)).toBeNull();
  });

  test("confirmation needs the exact level name", () => {
    expect(confirmed(" world ", "world")).toBe(true);
    expect(confirmed("World", "world")).toBe(false);
    expect(errorText(422, '{"error":"bad zip"}')).toBe("bad zip");
    expect(errorText(500, "oops")).toBe("HTTP 500");
    expect(errorText(0, "")).toBe("the connection failed");
  });
});
