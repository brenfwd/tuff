// Writes crawlable static snapshots, sitemap.xml and 404.html into dist/.
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const dist = join(root, "dist");
const SITE = "https://brenfwd.github.io/tuff/";
const template = readFileSync(join(dist, "index.html"), "utf8");
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

type Doc = { slug: string; title: string; group: string; description: string; html: string };
const docs: Doc[] = readdirSync(join(root, "src", "generated", "docs")).map((f) => JSON.parse(readFileSync(join(root, "src", "generated", "docs", f), "utf8")));
const nav: { slug: string; title: string }[] = JSON.parse(readFileSync(join(root, "src", "generated", "nav.json"), "utf8"));

function page({ title, description, url, body }: { title: string; description: string; url: string; body: string }) {
  let html = template
    .replace(/<title>.*?<\/title>/, `<title>${esc(title)}</title>`)
    .replace(/(<meta name="description" content=")[^"]*/, `$1${esc(description)}`)
    .replace(/(<link rel="canonical" href=")[^"]*/, `$1${url}`)
    .replace(/(<meta property="og:title" content=")[^"]*/, `$1${esc(title)}`)
    .replace(/(<meta property="og:description" content=")[^"]*/, `$1${esc(description)}`)
    .replace(/(<meta property="og:url" content=")[^"]*/, `$1${url}`)
    .replace(/(<meta name="twitter:title" content=")[^"]*/, `$1${esc(title)}`)
    .replace(/(<meta name="twitter:description" content=")[^"]*/, `$1${esc(description)}`);
  return html.replace("<!--prerender-->", body);
}

const docNav = `<nav aria-label="Documentation"><ul>${nav.map((p) => `<li><a href="${SITE}docs/${p.slug}/">${esc(p.title)}</a></li>`).join("")}</ul></nav>`;

const home = `<main style="max-width:72rem;margin:0 auto;padding:4rem 1rem;font-family:system-ui,sans-serif">
<h1>tuff: the Minecraft server, rebuilt in Rust</h1>
<p>tuff is a drop-in survival server for Minecraft Java Edition 26.3. The vanilla game runs as built-in features on an inert core, and external plugins are sandboxed WebAssembly modules.</p>
<p><a href="${SITE}docs/introduction/">Get started</a> | <a href="https://github.com/brenfwd/tuff">GitHub</a></p>
<h2>Performance</h2>
<table><thead><tr><th>Benchmark</th><th>Metric</th><th>tuff</th><th>Vanilla</th></tr></thead><tbody>
<tr><td>Village, 10-day sprint</td><td>ticks per second</td><td>1026</td><td>89.6</td></tr>
<tr><td>Village</td><td>p99 tick (ms)</td><td>2.13</td><td>12.9</td></tr>
<tr><td>Village</td><td>server memory (MiB)</td><td>1379</td><td>8966</td></tr>
<tr><td>Stress, 50 players mixed</td><td>TPS</td><td>20.35</td><td>2.15</td></tr>
<tr><td>Stress</td><td>mean tick (ms)</td><td>48.2</td><td>190</td></tr>
<tr><td>Stress</td><td>server memory (MiB)</td><td>2514</td><td>9005</td></tr>
</tbody></table>
<h2>Documentation</h2>${docNav}</main>`;

writeFileSync(join(dist, "index.html"), page({ title: "tuff - a fast Minecraft server in Rust with WebAssembly plugins", description: "tuff is a high-performance Minecraft Java Edition 26.3 server written in Rust. The vanilla game runs as built-in features on an inert core, and external plugins are sandboxed WebAssembly modules.", url: SITE, body: home }));
writeFileSync(join(dist, "404.html"), template.replace("<!--prerender-->", ""));

for (const d of docs) {
  const url = `${SITE}docs/${d.slug}/`;
  const body = `<main style="max-width:46rem;margin:0 auto;padding:3rem 1rem;font-family:system-ui,sans-serif"><article><p>${esc(d.group)}</p><h1>${esc(d.title)}</h1><p>${esc(d.description)}</p>${d.html}</article>${docNav}</main>`;
  const dir = join(dist, "docs", d.slug);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "index.html"), page({ title: `${d.title} - tuff docs`, description: d.description, url, body }));
}

const today = new Date().toISOString().slice(0, 10);
const urls = [SITE, ...nav.map((p) => `${SITE}docs/${p.slug}/`)];
writeFileSync(join(dist, "sitemap.xml"), `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${urls.map((u) => `  <url><loc>${u}</loc><lastmod>${today}</lastmod></url>`).join("\n")}\n</urlset>\n`);
writeFileSync(join(dist, ".nojekyll"), "");
console.log(`prerendered home and ${docs.length} docs pages`);
