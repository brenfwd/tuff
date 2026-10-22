// Renders content/docs/*.md and content/blog/*.md into JSON modules for the app and a search index.
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import MiniSearch from "minisearch";
import { createRenderer, searchSections, type Section } from "./lib/markdown";
import { buildBlog } from "./build-blog";
import { searchOptions } from "../src/lib/searchOptions";

const root = join(import.meta.dir, "..");
const contentDir = join(root, "content", "docs");
const genDir = join(root, "src", "generated");
const repoBlob = "https://github.com/brenfwd/tuff/blob/main/";

type Meta = { slug: string; title: string; group: string; description: string };
const manifest: { pages: Meta[]; commit: string } = JSON.parse(readFileSync(join(contentDir, "manifest.json"), "utf8"));

function rewriteHref(href: string) {
  if (/^(https?:|mailto:|#)/.test(href)) return href;
  const [path, hash] = href.split("#");
  const name = path.split("/").pop()!.replace(/\.md$/, "");
  if (manifest.pages.some((p) => p.slug === `guides/${name}`) && path.endsWith(".md")) return `#/docs/guides/${name}${hash ? `?h=${hash}` : ""}`;
  return repoBlob + path.replace(/^\.\//, "");
}

const sections: Section[] = [];
const renderer = createRenderer(rewriteHref);

rmSync(join(genDir, "docs"), { recursive: true, force: true });
mkdirSync(join(genDir, "docs"), { recursive: true });

for (const meta of manifest.pages) {
  const raw = readFileSync(join(contentDir, `${meta.slug}.md`), "utf8").replace(/^---[\s\S]*?---\n/, "");
  const { html, toc } = renderer.render(raw);
  sections.push(...searchSections(raw, { route: `/docs/${meta.slug}`, page: meta.slug, pageTitle: meta.title, group: meta.group }));
  const idx = manifest.pages.indexOf(meta);
  const file = meta.slug.replace(/\//g, "__");
  writeFileSync(join(genDir, "docs", `${file}.json`), JSON.stringify({ ...meta, html, toc, prev: manifest.pages[idx - 1] ?? null, next: manifest.pages[idx + 1] ?? null }));
}

writeFileSync(join(genDir, "nav.json"), JSON.stringify(manifest.pages));

const blog = buildBlog(genDir);
sections.push(...blog.sections);

const ms = new MiniSearch(searchOptions);
ms.addAll(sections.map((s) => ({ ...s, snippet: s.text.slice(0, 400) })));
mkdirSync(join(root, "public"), { recursive: true });
writeFileSync(join(root, "public", "search-index.json"), JSON.stringify(ms));
console.log(`rendered ${manifest.pages.length} docs pages, ${blog.posts} blog posts, ${sections.length} search sections`);
