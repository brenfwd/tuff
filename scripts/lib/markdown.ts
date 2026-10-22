// Shared Markdown rendering for docs and blog posts: shiki code blocks, heading anchors and a ToC.
import MarkdownIt from "markdown-it";
import GithubSlugger from "github-slugger";
import { createHighlighter, type ShikiTransformer } from "shiki";

export type TocEntry = { level: number; id: string; text: string };

const langs = ["rust", "sh", "bash", "sql", "toml", "json", "c", "cpp", "yaml", "dockerfile", "ini", "diff", "typescript", "javascript", "html", "xml", "markdown"];
const aliases: Record<string, string> = { ts: "typescript", js: "javascript", md: "markdown", console: "sh", shell: "sh", patch: "diff" };
const highlighter = await createHighlighter({ themes: ["github-light", "github-dark-dimmed"], langs });

export const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

function guessLang(code: string, info: string) {
  if (info) return aliases[info] ?? info;
  if (/^\s*(SELECT|WITH|PRAGMA)\b/im.test(code)) return "sql";
  if (/^\s*(cargo|git|env|scripts\/|docker|\.\/|curl|bun|npm|for |cd |rustup|sudo|brew|apt|java|python3)/m.test(code)) return "sh";
  if (/^\s*\/[a-z]/m.test(code)) return "mc";
  return "";
}

function diffLines(source: string): ShikiTransformer {
  const lines = source.split("\n");
  return {
    name: "diff-lines",
    line(node, line) {
      const text = lines[line - 1] ?? "";
      if (/^(\+\+\+|---|@@|diff )/.test(text)) this.addClassToHast(node, "diff meta");
      else if (text.startsWith("+")) this.addClassToHast(node, "diff add");
      else if (text.startsWith("-")) this.addClassToHast(node, "diff remove");
    },
  };
}

export function renderCode(code: string, info: string) {
  const lang = guessLang(code, info.trim().split(/\s+/)[0] ?? "");
  const source = code.replace(/\n$/, "");
  const body = langs.includes(lang)
    ? highlighter.codeToHtml(source, {
        lang,
        themes: { light: "github-light", dark: "github-dark-dimmed" },
        defaultColor: false,
        transformers: lang === "diff" ? [diffLines(source)] : [],
      })
    : `<pre class="shiki"><code>${escapeHtml(source)}</code></pre>`;
  const label = lang === "mc" ? "command" : lang || "text";
  return `<div class="code-block" data-lang="${label}"><div class="code-head"><span>${label}</span><button type="button" class="copy-btn" aria-label="Copy code">Copy</button></div>${body}</div>`;
}

export function createRenderer(rewriteHref: (href: string) => string = (h) => h) {
  const md = new MarkdownIt({ html: false, linkify: true, typographer: false });
  const toc: TocEntry[] = [];
  let slugger = new GithubSlugger();

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

  return {
    md,
    render(raw: string) {
      toc.length = 0;
      slugger = new GithubSlugger();
      const html = md.render(raw);
      return { html, toc: [...toc] };
    },
  };
}

export type Section = { id: string; route: string; page: string; pageTitle: string; anchor: string; heading: string; text: string; group: string };

// Splits plain Markdown text by heading for the search index.
export function searchSections(raw: string, base: { route: string; page: string; pageTitle: string; group: string }): Section[] {
  const out: Section[] = [];
  let heading = base.pageTitle;
  let anchor = "";
  let buf: string[] = [];
  let inFence = false;
  const slugger = new GithubSlugger();
  const flush = () => {
    const text = plainText(buf.join("\n"));
    out.push({ id: `${base.page}#${anchor}`, ...base, anchor, heading, text });
    buf = [];
  };
  for (const line of raw.split("\n")) {
    if (/^(```|~~~)/.test(line)) inFence = !inFence;
    const m = !inFence && /^(#{1,6}) (.+)$/.exec(line);
    if (m) {
      flush();
      heading = m[2].replace(/`/g, "");
      anchor = slugger.slug(heading);
    } else buf.push(line);
  }
  flush();
  return out;
}

export function plainText(md: string) {
  return md
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[`*_|>#\[\]]/g, " ")
    .replace(/\(https?:[^)]*\)/g, "")
    .replace(/-{3,}|:?-{2,}:?/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}
