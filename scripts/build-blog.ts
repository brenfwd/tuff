// Renders content/blog/<slug>.md into src/generated/blog/. Drafts are skipped unless BLOG_DRAFTS=1.
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createRenderer, plainText, searchSections, type Section } from "./lib/markdown";
import { SITE_URL } from "../src/lib/site";
import type { BlogIndex, Post, PostMeta } from "../src/lib/blog";

const root = join(import.meta.dir, "..");
const blogDir = join(root, "content", "blog");
export const includeDrafts = process.env.BLOG_DRAFTS === "1";
const WORDS_PER_MINUTE = 230;

function fail(file: string, msg: string): never {
  throw new Error(`content/blog/${file}: ${msg}`);
}

function parseFrontmatter(file: string, src: string) {
  const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(src);
  if (!m) fail(file, "missing YAML frontmatter");
  const data = Bun.YAML.parse(m[1]) as Record<string, unknown>;
  if (!data || typeof data !== "object") fail(file, "frontmatter is not a mapping");
  return { data, body: src.slice(m[0].length) };
}

function toIso(file: string, v: unknown) {
  const d = v instanceof Date ? v : new Date(/^\d{4}-\d{2}-\d{2}$/.test(String(v)) ? `${v}T00:00:00Z` : String(v));
  if (Number.isNaN(d.getTime())) fail(file, `invalid date ${JSON.stringify(v)}`);
  return d.toISOString();
}

function gitUpdated(path: string) {
  const r = Bun.spawnSync(["git", "log", "-1", "--format=%cI", "--", path], { cwd: root });
  const out = r.success ? r.stdout.toString().trim() : "";
  return out ? new Date(out).toISOString() : null;
}

function imageSize(path: string): [number, number] | null {
  if (!existsSync(path)) return null;
  const b = readFileSync(path);
  if (b.length > 24 && b.readUInt32BE(0) === 0x89504e47) return [b.readUInt32BE(16), b.readUInt32BE(20)];
  if (b[0] === 0xff && b[1] === 0xd8) {
    let i = 2;
    while (i + 9 < b.length) {
      const marker = b[i + 1];
      const len = b.readUInt16BE(i + 2);
      if (marker >= 0xc0 && marker <= 0xcf && ![0xc4, 0xc8, 0xcc].includes(marker)) return [b.readUInt16BE(i + 7), b.readUInt16BE(i + 5)];
      i += 2 + len;
    }
  }
  return null;
}

function excerptOf(body: string) {
  const para = body
    .replace(/^(```|~~~)[\s\S]*?^\1/gm, "")
    .split(/\n\s*\n/)
    .map((p) => p.trim())
    .find((p) => p && !/^(#|>|[-*+] |\d+\. |\||!\[)/.test(p));
  const text = plainText(para ?? "");
  if (text.length <= 220) return text;
  return `${text.slice(0, text.lastIndexOf(" ", 220))}...`;
}

const str = (file: string, data: Record<string, unknown>, key: string) => {
  const v = data[key];
  if (typeof v !== "string" || !v.trim()) fail(file, `frontmatter field "${key}" must be a non-empty string`);
  return v.trim();
};

export function buildBlog(genDir: string): { posts: number; sections: Section[] } {
  const out = join(genDir, "blog");
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  const files = existsSync(blogDir) ? readdirSync(blogDir).filter((f) => f.endsWith(".md")) : [];
  const renderer = createRenderer();
  const posts: (PostMeta & { html: string; toc: Post["toc"]; body: string })[] = [];

  for (const file of files) {
    const slug = file.replace(/\.md$/, "");
    if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(slug)) fail(file, "file name must be a lowercase kebab-case slug");
    const { data, body } = parseFrontmatter(file, readFileSync(join(blogDir, file), "utf8"));
    const draft = data.draft === true;
    if (draft && !includeDrafts) continue;
    const tags = data.tags ?? [];
    if (!Array.isArray(tags) || tags.some((t) => typeof t !== "string")) fail(file, 'frontmatter field "tags" must be a list of strings');
    const title = str(file, data, "title");
    const date = toIso(file, data.date ?? fail(file, 'frontmatter field "date" is required'));
    const git = gitUpdated(join("content", "blog", file));
    const updated = git && git > date ? git : date;
    const { html, toc } = renderer.render(body);
    const prose = plainText(body.replace(/^(```|~~~)[\s\S]*?^\1/gm, ""));
    const wordCount = prose ? prose.split(" ").length : 0;

    let image = `${SITE_URL}blog/og/${slug}.png`;
    let size: [number, number] | null = [1200, 630];
    const custom = typeof data.image === "string" && data.image.trim() ? data.image.trim() : null;
    if (custom) {
      image = /^https?:/.test(custom) ? custom : SITE_URL + custom.replace(/^\//, "");
      size = /^https?:/.test(custom) ? null : imageSize(join(root, "public", custom.replace(/^\//, "")));
    }

    posts.push({
      slug,
      title,
      description: str(file, data, "description"),
      date,
      updated,
      author: str(file, data, "author"),
      tags: (tags as string[]).map((t) => t.trim()).filter(Boolean),
      image,
      imageAlt: title,
      imageWidth: size?.[0] ?? null,
      imageHeight: size?.[1] ?? null,
      generatedImage: !custom,
      wordCount,
      readingMinutes: Math.max(1, Math.round(wordCount / WORDS_PER_MINUTE)),
      excerpt: excerptOf(body),
      draft,
      html,
      toc,
      body,
    });
  }

  posts.sort((a, b) => b.date.localeCompare(a.date) || a.slug.localeCompare(b.slug));
  const sections: Section[] = [];
  const metas: PostMeta[] = [];
  posts.forEach((p, i) => {
    const { html, toc, body, ...meta } = p;
    const newer = posts[i - 1];
    const older = posts[i + 1];
    const post: Post = { ...meta, html, toc, prev: older ? { slug: older.slug, title: older.title } : null, next: newer ? { slug: newer.slug, title: newer.title } : null };
    writeFileSync(join(out, `${p.slug}.json`), JSON.stringify(post));
    metas.push(meta);
    sections.push(...searchSections(body, { route: `/blog/${p.slug}`, page: `blog/${p.slug}`, pageTitle: p.title, group: "Blog" }));
  });

  const counts = new Map<string, number>();
  for (const p of metas) for (const t of p.tags) counts.set(t, (counts.get(t) ?? 0) + 1);
  const tags = [...counts].map(([name, count]) => ({ name, count })).sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
  const index: BlogIndex = { posts: metas, tags };
  writeFileSync(join(out, "index.json"), JSON.stringify(index));
  return { posts: metas.length, sections };
}
