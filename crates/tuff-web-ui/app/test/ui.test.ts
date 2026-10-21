import { expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import * as Ui from "../src/sdk/ui";
import { exposeShared, SHARED_KEY } from "../src/sdk/shared";

test("pages clamp and split rows", () => {
  const rows = Array.from({ length: 7 }, (_, i) => i);
  expect(Ui.pageOf(rows, 0, 3)).toEqual({ rows: [0, 1, 2], page: 0, pages: 3 });
  expect(Ui.pageOf(rows, 2, 3)).toEqual({ rows: [6], page: 2, pages: 3 });
  expect(Ui.pageOf(rows, 9, 3).page).toBe(2);
  expect(Ui.pageOf([], 0, 3)).toEqual({ rows: [], page: 0, pages: 1 });
});

test("line paths scale into the box", () => {
  expect(Ui.linePath([], 100, 10)).toBe("");
  expect(Ui.linePath([[0, 0], [10, 5]], 100, 10)).toBe("M0.0,10.0 L100.0,0.0");
  expect(Ui.linePath([[1, 2]], 100, 10)).toBe("M50.0,0.0");
  expect(Ui.linePath([[0, NaN], [1, 1], [2, 1]], 100, 10)).toBe("M0.0,0.0 L100.0,0.0");
});

test("components render", () => {
  const table = renderToStaticMarkup(
    createElement(Ui.Table<{ id: string }>, {
      columns: [{ key: "id", title: "Id", render: (r: { id: string }) => r.id }],
      rows: [{ id: "a" }, { id: "b" }, { id: "c" }],
      rowKey: (r: { id: string }) => r.id,
      pageSize: 2,
    }),
  );
  expect(table).toContain(">a<");
  expect(table).not.toContain(">c<");
  expect(table).toContain("1 / 2");
  expect(renderToStaticMarkup(createElement(Ui.Stat, { label: "Gold", value: 3 }))).toContain(">3<");
  const field = renderToStaticMarkup(createElement(Ui.Field, { label: "Name", value: "x", onChange: () => {}, error: "bad" }));
  expect(field).toContain("bad");
  expect(field).toContain('aria-invalid="true"');
  expect(renderToStaticMarkup(createElement(Ui.LineChart, { points: [[0, 1], [1, 2]], label: "TPS" }))).toContain("<path");
});

test("the kit is shared for plugin builds", async () => {
  const target: Record<string, unknown> = {};
  exposeShared(target);
  expect((target[SHARED_KEY] as { Ui: typeof Ui }).Ui.Table).toBe(Ui.Table);
  if (!(SHARED_KEY in globalThis)) exposeShared();
  const shim = await import("../src/sdk/shims/ui");
  expect(shim.Table).toBe(Ui.Table);
  expect(shim.pageOf).toBe(Ui.pageOf);
});
