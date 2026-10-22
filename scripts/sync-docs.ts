// Copies the docs source (docs/*.md) from the main branch into content/docs/.
// Usage: bun scripts/sync-docs.ts [git-ref] (default: main)
import { execFileSync } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const ref = process.argv[2] ?? "main";
const out = join(import.meta.dir, "..", "content", "docs");
const git = (...args: string[]) => execFileSync("git", args, { encoding: "utf8" });
const show = (path: string) => git("show", `${ref}:${path}`);

type Page = { slug: string; title: string; group: string; description: string };

const pages: Page[] = [
  { slug: "introduction", title: "Introduction", group: "Getting started", description: "What tuff is and what the first public alpha supports." },
  { slug: "installing", title: "Installing", group: "Getting started", description: "Requirements, build cost, cross-building, Docker and updating." },
  { slug: "running", title: "Running", group: "Getting started", description: "Starting the server, offline mode, proxies and restarting." },
  { slug: "settings", title: "Settings", group: "Operating", description: "Every server setting, dimensions, mob caps and crowd mode." },
  { slug: "features", title: "Built-in features", group: "Operating", description: "The vanilla game as built-in features, optional features and the custom content pack." },
  { slug: "pre-generating", title: "Pre-generating chunks", group: "Operating", description: "Generating terrain ahead of players." },
  { slug: "admin-panel", title: "Web admin panel", group: "Operating", description: "The browser-based admin panel and how to log in." },
  { slug: "anti-cheat", title: "Anti-cheat and anti-xray", group: "Operating", description: "Packet checks, enforcement settings and anti-xray." },
  { slug: "data-packs", title: "Data packs", group: "Operating", description: "Loading data packs." },
  { slug: "plugins", title: "WebAssembly plugins", group: "Plugins", description: "Sandboxed runtime plugins: managing, grants, writing one in Rust and the ABI." },
  { slug: "performance", title: "Performance", group: "Reference", description: "Side-by-side benchmarks against vanilla and how to reproduce them." },
  { slug: "tests", title: "Tests", group: "Reference", description: "Running the test suite and the opt-in real-client tests." },
  { slug: "license", title: "License", group: "Reference", description: "AGPL server, MIT plugin SDK, and what that means in practice." },
];

const guides = git("ls-tree", "--name-only", ref, "docs/guides/").trim().split("\n").filter((p) => p.endsWith(".md"));

rmSync(out, { recursive: true, force: true });
mkdirSync(join(out, "guides"), { recursive: true });

const manifest: Page[] = [];
for (const p of pages) {
  const body = show(`docs/${p.slug}.md`).replace(/^# .*\n+/, "").trim();
  writeFileSync(join(out, `${p.slug}.md`), `---\ntitle: ${p.title}\ndescription: ${p.description}\ngroup: ${p.group}\nsource: docs/${p.slug}.md\n---\n\n${body}\n`);
  manifest.push(p);
}

for (const path of guides) {
  const src = show(path);
  const name = path.split("/").pop()!.replace(/\.md$/, "");
  const title = /^# (.+)$/m.exec(src)?.[1].trim() ?? name;
  const firstPara = src.replace(/^# .*\n+/, "").split(/\n\s*\n/)[0].replace(/\s+/g, " ").replace(/[`*]/g, "").trim();
  const description = firstPara.length > 155 ? `${firstPara.slice(0, 152).replace(/\s\S*$/, "")}...` : firstPara;
  const body = src.replace(/^# .*\n/, "").trim();
  const slug = `guides/${name}`;
  writeFileSync(join(out, `${slug}.md`), `---\ntitle: ${title}\ndescription: ${JSON.stringify(description)}\ngroup: Guides\nsource: ${path}\n---\n\n${body}\n`);
  manifest.push({ slug, title, group: "Guides", description });
}

writeFileSync(join(out, "manifest.json"), `${JSON.stringify({ ref, commit: git("rev-parse", ref).trim(), pages: manifest }, null, 2)}\n`);
console.log(`synced ${manifest.length} pages from ${ref}`);
