// Copies the docs source from the main branch into content/docs/.
// Usage: bun scripts/sync-docs.ts [git-ref] (default: main)
import { execFileSync } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const ref = process.argv[2] ?? "main";
const out = join(import.meta.dir, "..", "content", "docs");
const git = (...args: string[]) => execFileSync("git", args, { encoding: "utf8" });
const show = (path: string) => git("show", `${ref}:${path}`);

type Page = { slug: string; title: string; group: string; description: string; sections: string[] };

const readmePages: Page[] = [
  { slug: "introduction", title: "Introduction", group: "Getting started", description: "What tuff is and what the first public alpha supports.", sections: ["#", "Release notes: v0.1.0-alpha.1"] },
  { slug: "installing", title: "Installing", group: "Getting started", description: "Requirements, build cost, cross-building, Docker and updating.", sections: ["Installing", "Building"] },
  { slug: "running", title: "Running", group: "Getting started", description: "Starting the server, offline mode, proxies and restarting.", sections: ["Running"] },
  { slug: "settings", title: "Settings", group: "Operating", description: "Every server setting, dimensions, mob caps and crowd mode.", sections: ["Settings"] },
  { slug: "features", title: "Built-in features", group: "Operating", description: "The vanilla game as built-in features, optional features and the custom content pack.", sections: ["Built-in features", "Custom content pack"] },
  { slug: "pre-generating", title: "Pre-generating chunks", group: "Operating", description: "Generating terrain ahead of players.", sections: ["Pre-generating chunks"] },
  { slug: "admin-panel", title: "Web admin panel", group: "Operating", description: "The browser-based admin panel and how to log in.", sections: ["Web admin panel"] },
  { slug: "anti-cheat", title: "Anti-cheat and anti-xray", group: "Operating", description: "Packet checks, enforcement settings and anti-xray.", sections: ["Anti-cheat", "Anti-xray"] },
  { slug: "data-packs", title: "Data packs", group: "Operating", description: "Loading data packs.", sections: ["Data packs"] },
  { slug: "plugins", title: "WebAssembly plugins", group: "Plugins", description: "Sandboxed runtime plugins: managing, grants, writing one in Rust and the ABI.", sections: ["Runtime plugins (WebAssembly)"] },
  { slug: "performance", title: "Performance", group: "Reference", description: "Side-by-side benchmarks against vanilla and how to reproduce them.", sections: ["Performance", "Benchmarks"] },
  { slug: "tests", title: "Tests", group: "Reference", description: "Running the test suite and the opt-in real-client tests.", sections: ["Tests"] },
  { slug: "license", title: "License", group: "Reference", description: "AGPL server, MIT plugin SDK, and what that means in practice.", sections: ["License"] },
];

const guides = git("ls-tree", "--name-only", ref, "docs/guides/").trim().split("\n").filter((p) => p.endsWith(".md"));

const readme = show("README.md");
const blocks = new Map<string, string>();
let current = "#";
let buf: string[] = [];
let inFence = false;
for (const line of readme.split("\n")) {
  if (/^(```|~~~)/.test(line)) inFence = !inFence;
  const m = !inFence && /^## (.+)$/.exec(line);
  if (m) {
    blocks.set(current, buf.join("\n"));
    current = m[1].trim();
    buf = [];
  }
  buf.push(line);
}
blocks.set(current, buf.join("\n"));

rmSync(out, { recursive: true, force: true });
mkdirSync(join(out, "guides"), { recursive: true });

const used = new Set<string>();
const manifest: Omit<Page, "sections">[] = [];
for (const p of readmePages) {
  const parts = p.sections.map((s, i) => {
    const b = blocks.get(s);
    if (b === undefined) throw new Error(`README section not found: ${s}`);
    used.add(s);
    // The page title replaces the first section's own heading.
    if (i > 0) return b;
    return b.replace(/^#{1,2} .*\n/, "").replace(/^#(#{2,5}) /gm, "$1 ");
  });
  const body = parts.join("\n\n").trim();
  writeFileSync(join(out, `${p.slug}.md`), `---\ntitle: ${p.title}\ndescription: ${p.description}\ngroup: ${p.group}\nsource: README.md\n---\n\n${body}\n`);
  manifest.push({ slug: p.slug, title: p.title, group: p.group, description: p.description });
}
const unused = [...blocks.keys()].filter((k) => !used.has(k));
if (unused.length) console.warn(`README sections not placed on any page: ${unused.join(", ")}`);

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
