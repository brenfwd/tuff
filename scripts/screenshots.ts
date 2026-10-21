// Screenshots the built site. Usage: bun scripts/screenshots.ts <out-dir>
import { chromium, type Page } from "playwright-core";
import { spawn } from "node:child_process";
import { mkdirSync } from "node:fs";

const out = process.argv[2] ?? "shots";
mkdirSync(out, { recursive: true });
const port = 4789;
const server = spawn("bunx", ["vite", "preview", "--port", String(port), "--strictPort"], { stdio: "ignore" });
const base = `http://localhost:${port}/`;
for (let i = 0; i < 50; i++) {
  try {
    await fetch(base);
    break;
  } catch {
    await new Promise((r) => setTimeout(r, 200));
  }
}

async function scrollThrough(page: Page) {
  const h = await page.evaluate(() => document.body.scrollHeight);
  for (let y = 0; y < h; y += 400) {
    await page.evaluate((y) => scrollTo(0, y), y);
    await page.waitForTimeout(60);
  }
  await page.evaluate(() => scrollTo(0, 0));
  await page.waitForTimeout(1200);
}

const browser = await chromium.launch();
const sizes = [
  ["desktop", 1440, 900],
  ["390", 390, 844],
  ["375", 375, 740],
] as const;
const overflow: string[] = [];
for (const theme of ["dark", "light"] as const) {
  for (const [name, width, height] of sizes) {
    if (theme === "light" && name === "375") continue;
    const ctx = await browser.newContext({ viewport: { width, height }, deviceScaleFactor: 1, colorScheme: theme, hasTouch: name !== "desktop", isMobile: name !== "desktop" });
    const page = await ctx.newPage();
    const shot = async (label: string, full = false) => {
      const sw = await page.evaluate(() => document.documentElement.scrollWidth);
      if (sw > width) overflow.push(`${label}-${name}-${theme}: scrollWidth ${sw} > ${width}`);
      await page.screenshot({ path: `${out}/${label}-${name}-${theme}.png`, fullPage: full });
    };
    await page.goto(base);
    await page.waitForTimeout(1500);
    await shot("home-top");
    await scrollThrough(page);
    await shot("home-full", true);
    for (const id of ["architecture", "features", "performance", "plugins", "get-started"]) {
      await page.evaluate((id) => document.getElementById(id)?.scrollIntoView(), id);
      await page.waitForTimeout(900);
      await shot(`home-${id}`);
    }
    await page.goto(`${base}#/docs/plugins`);
    await page.waitForTimeout(800);
    await shot("docs");
    await page.goto(`${base}#/docs/performance?h=benchmarks`);
    await page.waitForTimeout(800);
    await shot("docs-anchor");
    if (name !== "desktop") {
      await page.getByRole("button", { name: "Menu", exact: true }).click();
      await page.waitForTimeout(300);
      await shot("docs-drawer");
      await page.keyboard.press("Escape");
      await page.getByRole("button", { name: "Open menu" }).click();
      await page.waitForTimeout(300);
      await shot("nav-menu");
      await page.getByRole("button", { name: "Close menu" }).first().click();
    }
    await page.getByRole("button", { name: "Search documentation" }).click();
    await page.waitForTimeout(500);
    await page.keyboard.type("plugin grants");
    await page.waitForTimeout(500);
    await shot("search");
    await ctx.close();
  }
}
await browser.close();
server.kill();
console.log(overflow.length ? `OVERFLOW:\n${overflow.join("\n")}` : "no horizontal overflow");
