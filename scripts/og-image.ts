import { SHOW_PERFORMANCE } from "../src/lib/site";
// Renders the Open Graph image and PNG icons into public/ with headless Chromium.
import { chromium } from "playwright-core";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const pub = join(root, "public");
const font = (pkg: string, file: string) => readFileSync(join(root, "node_modules", "@fontsource-variable", pkg, "files", file)).toString("base64");
const display = font("space-grotesk", "space-grotesk-latin-wght-normal.woff2");
const sans = font("inter", "inter-latin-wght-normal.woff2");
const mono = font("jetbrains-mono", "jetbrains-mono-latin-wght-normal.woff2");
const favicon = readFileSync(join(pub, "app-icon.svg"), "utf8");

const cube = (x: number, y: number, s: number, o: number) =>
  `<g transform="translate(${x} ${y}) scale(${s})" opacity="${o}"><path d="M16 2 29 9.5 16 17 3 9.5Z" fill="#3a3530"/><path d="M3 9.5 16 17v13L3 22.5Z" fill="#1d1a18"/><path d="M29 9.5 16 17v13l13-7.5Z" fill="#282421"/></g>`;

const og = `<!doctype html><html><head><style>
@font-face{font-family:D;src:url(data:font/woff2;base64,${display})}
@font-face{font-family:S;src:url(data:font/woff2;base64,${sans})}
@font-face{font-family:M;src:url(data:font/woff2;base64,${mono})}
*{margin:0;box-sizing:border-box}
body{width:1200px;height:630px;background:#0c0a09;color:#f5f4f2;font-family:S;position:relative;overflow:hidden}
.glow{position:absolute;width:900px;height:900px;right:-260px;top:-320px;background:radial-gradient(circle,rgba(224,96,26,.45),transparent 62%)}
.grid{position:absolute;inset:0;background-image:linear-gradient(rgba(255,255,255,.045) 1px,transparent 1px),linear-gradient(90deg,rgba(255,255,255,.045) 1px,transparent 1px);background-size:48px 48px}
.wrap{position:absolute;inset:0;padding:72px 80px;display:flex;flex-direction:column}
.brand{display:flex;align-items:center;gap:18px;font-family:D;font-weight:700;font-size:44px;letter-spacing:-1px}
h1{font-family:D;font-weight:700;font-size:78px;line-height:1.02;letter-spacing:-2.6px;margin-top:56px;max-width:900px}
h1 span{color:#fb8a4c}
p{font-size:28px;color:#b5b0aa;margin-top:26px;max-width:820px;line-height:1.4}
.stats{margin-top:auto;display:flex;gap:16px;font-family:M;font-size:22px}
.stat{border:1px solid #3a3530;background:rgba(22,20,18,.8);border-radius:14px;padding:12px 20px}
.stat b{color:#fb8a4c}
svg.cubes{position:absolute;right:40px;bottom:30px}
</style></head><body><div class="grid"></div><div class="glow"></div>
<svg class="cubes" width="420" height="420" viewBox="0 0 140 140">${cube(70, 10, 2, 0.9)}${cube(10, 60, 1.4, 0.7)}${cube(90, 80, 1.1, 0.5)}</svg>
<div class="wrap"><div class="brand">${favicon.replace("<svg ", '<svg width="64" height="64" ')}tuff</div>
<h1>The Minecraft server,<br><span>rebuilt in Rust.</span></h1>
<p>A drop-in survival server for Java Edition 26.3 with sandboxed WebAssembly plugins.</p>
${SHOW_PERFORMANCE ? `<div class="stats"><div class="stat"><b>4.2x</b> vanilla tick rate (sim 32)</div><div class="stat"><b>20</b> TPS at 50 players</div></div>` : ""}</div>
</body></html>`;

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1200, height: 630 } });
await page.setContent(og);
await page.evaluate(() => document.fonts.ready);
await page.screenshot({ path: join(pub, "og.png") });
for (const [name, size] of [["favicon-32.png", 32], ["apple-touch-icon.png", 180], ["icon-192.png", 192], ["icon-512.png", 512]] as const) {
  await page.setViewportSize({ width: size, height: size });
  await page.setContent(`<style>*{margin:0}body{background:transparent}</style>${favicon.replace("<svg ", `<svg width="${size}" height="${size}" `)}`);
  await page.screenshot({ path: join(pub, name), omitBackground: true });
}
await browser.close();
console.log("wrote og.png and icons");
