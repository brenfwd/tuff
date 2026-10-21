import { describe, expect, test } from "bun:test";
import { cssColor, flatten, plain, runCss } from "../src/minimessage";

describe("minimessage preview", () => {
  test("children inherit style and override it", () => {
    const runs = flatten({
      text: "",
      color: "red",
      extra: ["a", { text: "b", bold: true, color: "#00ff00" }, { text: "c", italic: true }],
    });
    expect(runs.map((r) => r.text)).toEqual(["a", "b", "c"]);
    expect(runs[0].style.color).toBe("#FF5555");
    expect(runs[1].style).toMatchObject({ color: "#00FF00", bold: true });
    expect(runs[2].style).toMatchObject({ color: "#FF5555", italic: true });
  });

  test("non-text contents show what they stand for", () => {
    expect(plain({ translate: "a.b", fallback: "F" })).toBe("F");
    expect(plain({ keybind: "key.jump" })).toBe("[key.jump]");
    expect(plain({ score: { name: "@s", objective: "kills" } })).toBe("[score @s kills]");
    expect(plain({ selector: "@a" })).toBe("[@a]");
  });

  test("hover and click are summarised", () => {
    const [run] = flatten({
      text: "x",
      hover_event: { action: "show_text", value: { text: "tip" } },
      click_event: { action: "open_url", url: "https://example.com" },
    });
    expect(run.style.hover).toBe("tip");
    expect(run.style.click).toBe("open_url: https://example.com");
  });

  test("css for runs", () => {
    expect(cssColor("dark_gray")).toBe("#555555");
    expect(cssColor("nope")).toBeUndefined();
    expect(runCss({ bold: true, underlined: true, strikethrough: true })).toEqual({
      fontWeight: "bold",
      textDecoration: "underline line-through",
    });
  });

  test("deep nesting stops", () => {
    let c: unknown = "x";
    for (let i = 0; i < 200; i++) c = { text: "", extra: [c] };
    expect(flatten(c as never)).toEqual([]);
  });
});
