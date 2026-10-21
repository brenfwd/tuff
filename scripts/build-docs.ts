// Renders content/docs/*.md into JSON modules for the app and a search index.
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import MarkdownIt from "markdown-it";
import GithubSlugger from "github-slugger";
import MiniSearch from "minisearch";
import { createHighlighter } from "shiki";

const root = join(import.meta.dir, "..");
const contentDir = join(root, "content", "docs");
const genDir = join(root, "src", "generated");
const repoBlob = "https://github.com/brenfwd/tuff/blob/main/";

type Meta = { slug: string; title: string; group: string; description: string };
const manifest: { pages: Meta[]; commit: string } = JSON.parse(readFileSync(join(contentDir, "manifest.json"), "utf8"));

const langs = ["rust", "sh", "bash", "sql", "toml", "json", "c", "cpp", "yaml", "dockerfile", "ini"];
const highlighter = await createHighlighter({ themes: ["github-light", "github-dark-dimmed"], langs });

const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

function guessLang(code: string, info: string) {
  if (info) return info === "console" ? "sh" : info;
  if (/^\s*(SELECT|WITH|PRAGMA)\b/im.test(code)) return "sql";
  if (/^\s*(cargo|git|env|scripts\/|docker|\.\/|curl|bun|npm|for |cd |rustup|sudo|brew|apt|java|python3)/m.test(code)) return "sh";
  if (/^\s*\/[a-z]/m.test(code)) return "mc";
  return "";
}

function renderCode(code: string, info: string) {
  const lang = guessLang(code, info.trim().split(/\s+/)[0] ?? "");
  const body = langs.includes(lang)
    ? highlighter.codeToHtml(code.replace(/\n$/, ""), { lang, themes: { light: "github-light", dark: "github-dark-dimmed" }, defaultColor: false })
    : `<pre class="shiki"><code>${escapeHtml(code.replace(/\n$/, ""))}</code></pre>`;
  const label = lang === "mc" ? "command" : lang || "text";
  return `<div class="code-block" data-lang="${label}"><div class="code-head"><span>${label}</span><button type="button" class="copy-btn" aria-label="Copy code">Copy</button></div>${body}</div>`;
}

function rewriteHref(href: string) {
  if (/^(https?:|mailto:|#)/.test(href)) return href;
  const [path, hash] = href.split("#");
  const name = path.split("/").pop()!.replace(/\.md$/, "");
  if (manifest.pages.some((p) => p.slug === `guides/${name}`) && path.endsWith(".md")) return `#/docs/guides/${name}${hash ? `?h=${hash}` : ""}`;
  return repoBlob + path.replace(/^\.\//, "");
}

type Section = { id: string; page: string; pageTitle: string; anchor: string; heading: string; text: string; group: string };
const sections: Section[] = [];

rmSync(join(genDir, "docs"), { recursive: true, force: true });
mkdirSync(join(genDir, "docs"), { recursive: true });

for (const meta of manifest.pages) {
  const raw = readFileSync(join(contentDir, `${meta.slug}.md`), "utf8").replace(/^---[\s\S]*?---\n/, "");
  const md = new MarkdownIt({ html: false, linkify: true, typographer: false });
  const slugger = new GithubSlugger();
  const toc: { level: number; id: string; text: string }[] = [];

  md.renderer.rules.fence = (tokens, i) => renderCode(tokens[i].content, tokens[i].info);
  md.renderer.rules.code_block = (tokens, i) => renderCode(tokens[i].content, "");
  md.renderer.rules.table_open = () => '<div class="table-wrap"><table>';
  md.renderer.rules.table_close = () => "</table></div>";
  md.renderer.rules.heading_open = (tokens, i, opts, _env, self) => {
    const inline = tokens[i + 1];
    const text = inline.children?.map((c) => c.content).join("") ?? inline.content;
    const id = slugger.slug(text);
    tokens[i].attrSet("id", id);
    const level = Number(tokens[i].tag.slice(1));
    if (level <= 3) toc.push({ level, id, text });
    return self.renderToken(tokens, i, opts);
  };
  md.renderer.rules.heading_close = (tokens, i, opts, _env, self) => {
    const id = tokens[i - 2]?.attrGet("id");
    return `${id ? `<a class="anchor" href="#${id}" aria-label="Link to this section">#</a>` : ""}${self.renderToken(tokens, i, opts)}`;
  };
  const defaultLink = md.renderer.rules.link_open ?? ((t, i, o, _e, s) => s.renderToken(t, i, o));
  md.renderer.rules.link_open = (tokens, i, opts, env, self) => {
    const href = rewriteHref(tokens[i].attrGet("href") ?? "");
    tokens[i].attrSet("href", href);
    if (/^https?:/.test(href)) {
      tokens[i].attrSet("target", "_blank");
      tokens[i].attrSet("rel", "noopener noreferrer");
    }
    return defaultLink(tokens, i, opts, env, self);
  };

  const html = md.render(raw);

  // Split plain text by heading for the search index.
  let heading = meta.title;
  let anchor = "";
  let buf: string[] = [];
  let inFence = false;
  const s2 = new GithubSlugger();
  const flush = () => {
    const text = buf.join(" ").replace(/[`*_|>#\[\]]/g, " ").replace(/\(https?:[^)]*\)/g, "").replace(/-{3,}|:?-{2,}:?/g, " ").replace(/\s+/g, " ").trim();
    sections.push({ id: `${meta.slug}#${anchor}`, page: meta.slug, pageTitle: meta.title, anchor, heading, text, group: meta.group });
    buf = [];
  };
  for (const line of raw.split("\n")) {
    if (/^(```|~~~)/.test(line)) inFence = !inFence;
    const m = !inFence && /^(#{1,6}) (.+)$/.exec(line);
    if (m) {
      flush();
      heading = m[2].replace(/`/g, "");
      anchor = s2.slug(heading);
    } else buf.push(line);
  }
  flush();

  const idx = manifest.pages.indexOf(meta);
  const file = meta.slug.replace(/\//g, "__");
  writeFileSync(join(genDir, "docs", `${file}.json`), JSON.stringify({ ...meta, html, toc, prev: manifest.pages[idx - 1] ?? null, next: manifest.pages[idx + 1] ?? null }));
}

writeFileSync(join(genDir, "nav.json"), JSON.stringify(manifest.pages));

import { searchOptions } from "../src/lib/searchOptions";
const ms = new MiniSearch(searchOptions);
ms.addAll(sections.map((s) => ({ ...s, snippet: s.text.slice(0, 400) })));
mkdirSync(join(root, "public"), { recursive: true });
writeFileSync(join(root, "public", "search-index.json"), JSON.stringify(ms));
console.log(`rendered ${manifest.pages.length} pages, ${sections.length} search sections`);
