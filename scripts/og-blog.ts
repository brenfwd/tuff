// Renders a 1200x630 Open Graph image per blog post into public/blog/og/ with headless Chromium.
// Posts with a frontmatter `image` are skipped; unchanged posts reuse the cached PNG.
import { chromium } from "playwright-core";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { BlogIndex, PostMeta } from "../src/lib/blog";
import { formatDate } from "../src/lib/blog";

const root = join(import.meta.dir, "..");
const outDir = join(root, "public", "blog", "og");
const cacheDir = join(root, "node_modules", ".cache");
const cacheFile = join(cacheDir, "og-blog.json");
const index: BlogIndex = JSON.parse(readFileSync(join(root, "src", "generated", "blog", "index.json"), "utf8"));
const TEMPLATE_VERSION = "2";

const font = (pkg: string, file: string) => readFileSync(join(root, "node_modules", "@fontsource-variable", pkg, "files", file)).toString("base64");
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

const wanted = index.posts.filter((p) => p.generatedImage);
const key = (p: PostMeta) => createHash("sha256").update(JSON.stringify([TEMPLATE_VERSION, p.title, p.author, p.date, p.tags])).digest("hex").slice(0, 16);

mkdirSync(outDir, { recursive: true });
mkdirSync(cacheDir, { recursive: true });
let cache: Record<string, string> = {};
try {
  cache = JSON.parse(readFileSync(cacheFile, "utf8"));
} catch {}
const keep = new Set(wanted.map((p) => `${p.slug}.png`));
for (const f of readdirSync(outDir)) if (f.endsWith(".png") && !keep.has(f)) rmSync(join(outDir, f));

const todo = wanted.filter((p) => cache[p.slug] !== key(p) || !existsSync(join(outDir, `${p.slug}.png`)));
if (todo.length) {
  const display = font("space-grotesk", "space-grotesk-latin-wght-normal.woff2");
  const sans = font("inter", "inter-latin-wght-normal.woff2");
  const mono = font("jetbrains-mono", "jetbrains-mono-latin-wght-normal.woff2");
  const favicon = readFileSync(join(root, "public", "favicon.svg"), "utf8");
  const html = (p: PostMeta) => `<!doctype html><html><head><style>
@font-face{font-family:D;src:url(data:font/woff2;base64,${display})}
@font-face{font-family:S;src:url(data:font/woff2;base64,${sans})}
@font-face{font-family:M;src:url(data:font/woff2;base64,${mono})}
*{margin:0;box-sizing:border-box}
body{width:1200px;height:630px;background:#0c0a09;color:#f5f4f2;font-family:S;position:relative;overflow:hidden}
.glow{position:absolute;width:900px;height:900px;right:-300px;top:-380px;background:radial-gradient(circle,rgba(224,96,26,.38),transparent 62%)}
.grid{position:absolute;inset:0;background-image:linear-gradient(rgba(255,255,255,.04) 1px,transparent 1px),linear-gradient(90deg,rgba(255,255,255,.04) 1px,transparent 1px);background-size:48px 48px}
.wrap{position:absolute;inset:0;padding:68px 80px 64px;display:flex;flex-direction:column}
.top{display:flex;align-items:center;justify-content:space-between}
.brand{display:flex;align-items:center;gap:16px;font-family:D;font-weight:700;font-size:40px;letter-spacing:-1px}
.label{font-family:M;font-size:22px;color:#fb8a4c;border:1px solid #3a3530;background:rgba(22,20,18,.8);border-radius:12px;padding:8px 18px;letter-spacing:.06em;text-transform:uppercase}
.title{flex:1;display:flex;align-items:center}
h1{font-family:D;font-weight:700;font-size:72px;line-height:1.06;letter-spacing:-2px;max-width:1040px}
.meta{display:flex;gap:14px;align-items:center;font-size:26px;color:#b5b0aa;border-top:1px solid #27231f;padding-top:28px}
.meta .dot{color:#57524d}
.meta .url{margin-left:auto;font-family:M;font-size:22px;color:#918b84}
</style></head><body><div class="grid"></div><div class="glow"></div>
<div class="wrap"><div class="top"><div class="brand">${favicon.replace("<svg ", '<svg width="56" height="56" ')}tuff</div><div class="label">Blog</div></div>
<div class="title"><h1 id="t">${esc(p.title)}</h1></div>
<div class="meta"><span>${esc(p.author)}</span><span class="dot">/</span><span>${esc(formatDate(p.date))}</span><span class="url">tuff.bforward.me/blog</span></div></div>
</body></html>`;

  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1200, height: 630 } });
  for (const p of todo) {
    await page.setContent(html(p));
    await page.evaluate(() => document.fonts.ready);
    await page.evaluate(() => {
      const h = document.getElementById("t")!;
      const box = h.parentElement!;
      let size = 72;
      while (size > 36 && (h.scrollHeight > box.clientHeight || h.scrollWidth > box.clientWidth)) h.style.fontSize = `${(size -= 2)}px`;
    });
    await page.screenshot({ path: join(outDir, `${p.slug}.png`) });
    cache[p.slug] = key(p);
  }
  await browser.close();
}
for (const slug of Object.keys(cache)) if (!keep.has(`${slug}.png`)) delete cache[slug];
writeFileSync(cacheFile, JSON.stringify(cache));
console.log(`blog og images: ${todo.length} rendered, ${wanted.length - todo.length} cached`);
