import { expect, test } from "bun:test";
import { isAtBottom, STICK_THRESHOLD_PX } from "../src/scroll";

test("exactly at bottom", () => {
  expect(isAtBottom({ scrollHeight: 1000, scrollTop: 800, clientHeight: 200 })).toBe(true);
});

test("scrolled up", () => {
  expect(isAtBottom({ scrollHeight: 1000, scrollTop: 500, clientHeight: 200 })).toBe(false);
});

test("no overflow", () => {
  expect(isAtBottom({ scrollHeight: 200, scrollTop: 0, clientHeight: 200 })).toBe(true);
  expect(isAtBottom({ scrollHeight: 50, scrollTop: 0, clientHeight: 200 })).toBe(true);
});

test("within threshold", () => {
  const bottom = 800;
  expect(isAtBottom({ scrollHeight: 1000, scrollTop: bottom - STICK_THRESHOLD_PX, clientHeight: 200 })).toBe(true);
  expect(isAtBottom({ scrollHeight: 1000, scrollTop: bottom - 0.5, clientHeight: 200 })).toBe(true);
  expect(isAtBottom({ scrollHeight: 1000, scrollTop: bottom - STICK_THRESHOLD_PX - 1, clientHeight: 200 })).toBe(false);
});

test("custom threshold", () => {
  expect(isAtBottom({ scrollHeight: 1000, scrollTop: 780, clientHeight: 200 }, 20)).toBe(true);
  expect(isAtBottom({ scrollHeight: 1000, scrollTop: 779, clientHeight: 200 }, 20)).toBe(false);
});
