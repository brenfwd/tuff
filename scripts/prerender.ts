// Writes crawlable static snapshots, blog feeds, sitemap.xml and 404.html into dist/.
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { DESCRIPTION, SITE_NAME, SITE_URL as SITE } from "../src/lib/site";
import { formatDate, sameDay, type BlogIndex, type Post, type PostMeta } from "../src/lib/blog";

const root = join(import.meta.dir, "..");
const dist = join(root, "dist");
const template = readFileSync(join(dist, "index.html"), "utf8");
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const jsonLd = (data: unknown) => `<script type="application/ld+json">${JSON.stringify(data).replace(/</g, "\\u003c")}</script>`;

type Doc = { slug: string; title: string; group: string; description: string; html: string };
const docs: Doc[] = readdirSync(join(root, "src", "generated", "docs")).map((f) => JSON.parse(readFileSync(join(root, "src", "generated", "docs", f), "utf8")));
const nav: { slug: string; title: string }[] = JSON.parse(readFileSync(join(root, "src", "generated", "nav.json"), "utf8"));
const blogGen = join(root, "src", "generated", "blog");
const blog: BlogIndex = JSON.parse(readFileSync(join(blogGen, "index.json"), "utf8"));
const posts: Post[] = blog.posts.map((p) => JSON.parse(readFileSync(join(blogGen, `${p.slug}.json`), "utf8")));

type Image = { url: string; alt: string; width: number | null; height: number | null };
type PageOpts = { title: string; description: string; url: string; body: string; type?: string; image?: Image; head?: string[]; ld?: unknown };

function setMeta(html: string, attr: "name" | "property", key: string, value: string | null) {
  const re = new RegExp(`\\s*<meta ${attr}="${key.replace(/:/g, "\\:")}" content="[^"]*" />`);
  if (value === null) return html.replace(re, "");
  const tag = `<meta ${attr}="${key}" content="${esc(value)}" />`;
  return re.test(html) ? html.replace(re, (m) => m.replace(/content="[^"]*"/, `content="${esc(value)}"`)) : html.replace("</head>", `    ${tag}\n  </head>`);
}

function page({ title, description, url, body, type, image, head = [], ld }: PageOpts) {
  let html = template
    .replace(/<title>.*?<\/title>/, `<title>${esc(title)}</title>`)
    .replace(/(<link rel="canonical" href=")[^"]*/, `$1${url}`);
  for (const [attr, key, value] of [
    ["name", "description", description],
    ["property", "og:title", title],
    ["property", "og:description", description],
    ["property", "og:url", url],
    ["name", "twitter:title", title],
    ["name", "twitter:description", description],
  ] as const)
    html = setMeta(html, attr, key, value);
  if (type) html = setMeta(html, "property", "og:type", type);
  if (image) {
    html = setMeta(html, "property", "og:image", image.url);
    html = setMeta(html, "property", "og:image:width", image.width ? String(image.width) : null);
    html = setMeta(html, "property", "og:image:height", image.height ? String(image.height) : null);
    html = setMeta(html, "property", "og:image:alt", image.alt);
    html = setMeta(html, "name", "twitter:image", image.url);
    html = setMeta(html, "name", "twitter:image:alt", image.alt);
  }
  if (ld) html = html.replace(/<script type="application\/ld\+json">[\s\S]*?<\/script>/, jsonLd(ld));
  if (head.length) html = html.replace("</head>", `    ${head.join("\n    ")}\n  </head>`);
  return html.replace("<!--prerender-->", body);
}

function write(path: string, content: string) {
  const file = join(dist, path);
  mkdirSync(join(file, ".."), { recursive: true });
  writeFileSync(file, content);
}

const docNav = `<nav aria-label="Documentation"><ul>${nav.map((p) => `<li><a href="${SITE}docs/${p.slug}/">${esc(p.title)}</a></li>`).join("")}</ul></nav>`;

const home = `<main data-snapshot style="max-width:72rem;margin:0 auto;padding:4rem 1rem;font-family:system-ui,sans-serif">
<h1>tuff: the Minecraft server, rebuilt in Rust</h1>
<p>tuff is a drop-in survival server for Minecraft Java Edition 26.3. The vanilla game runs as built-in features on an inert core, and external plugins are sandboxed WebAssembly modules.</p>
<p><a href="${SITE}docs/introduction/">Get started</a> | <a href="${SITE}blog/">Blog</a> | <a href="https://github.com/brenfwd/tuff">GitHub</a></p>
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

writeFileSync(join(dist, "index.html"), page({ title: "tuff - a fast Minecraft server in Rust with WebAssembly plugins", description: DESCRIPTION, url: SITE, body: home }));
writeFileSync(join(dist, "404.html"), template.replace("<!--prerender-->", ""));

for (const d of docs) {
  const url = `${SITE}docs/${d.slug}/`;
  const body = `<main data-snapshot style="max-width:46rem;margin:0 auto;padding:3rem 1rem;font-family:system-ui,sans-serif"><article><p>${esc(d.group)}</p><h1>${esc(d.title)}</h1><p>${esc(d.description)}</p>${d.html}</article>${docNav}</main>`;
  write(`docs/${d.slug}/index.html`, page({ title: `${d.title} - tuff docs`, description: d.description, url, body }));
}

// Blog
const BLOG_URL = `${SITE}blog/`;
const BLOG_TITLE = `${SITE_NAME} blog`;
const BLOG_DESCRIPTION = "News, release notes and engineering write-ups from the tuff Minecraft server project.";
const postUrl = (slug: string) => `${BLOG_URL}${slug}/`;
const publisher = { "@type": "Organization", name: SITE_NAME, url: SITE, logo: { "@type": "ImageObject", url: `${SITE}icon-512.png`, width: 512, height: 512 } };
const person = (name: string) => ({ "@type": "Person", name });
const crumbs = (items: [string, string][]) => ({
  "@type": "BreadcrumbList",
  itemListElement: items.map(([name, item], i) => ({ "@type": "ListItem", position: i + 1, name, item })),
});
const lastUpdated = posts.reduce((a, p) => (p.updated > a ? p.updated : a), posts[0]?.updated ?? new Date().toISOString());
const time = (iso: string) => `<time datetime="${iso}">${formatDate(iso)}</time>`;
const blogStyle = "max-width:46rem;margin:0 auto;padding:3rem 1rem;font-family:system-ui,sans-serif";
const blogNav = `<nav aria-label="Breadcrumb"><a href="${SITE}">Home</a> / <a href="${BLOG_URL}">Blog</a></nav>`;

const aiNote = (p: Post) => (p.ai ? `<hr /><aside id="ai-note"><p><small>AI assistance: ${esc(p.ai)}</small></p></aside>` : "");

const postLd = (p: Post) => ({
  "@type": "BlogPosting",
  "@id": `${postUrl(p.slug)}#article`,
  headline: p.title,
  description: p.description,
  datePublished: p.date,
  dateModified: p.updated,
  author: person(p.author),
  ...(p.ai ? { contributor: { "@type": "Organization", name: "AI assistance", description: p.ai } } : {}),
  publisher,
  image: { "@type": "ImageObject", url: p.image, ...(p.imageWidth ? { width: p.imageWidth, height: p.imageHeight } : {}) },
  mainEntityOfPage: { "@type": "WebPage", "@id": postUrl(p.slug) },
  url: postUrl(p.slug),
  wordCount: p.wordCount,
  keywords: p.tags.join(", "),
  inLanguage: "en",
  isPartOf: { "@type": "Blog", "@id": `${BLOG_URL}#blog` },
});

const indexBody = `<main data-snapshot style="${blogStyle}"><nav aria-label="Breadcrumb"><a href="${SITE}">Home</a></nav><h1>Blog</h1><p>${esc(BLOG_DESCRIPTION)}</p>
<p>Subscribe: <a href="${BLOG_URL}rss.xml">RSS</a> | <a href="${BLOG_URL}atom.xml">Atom</a> | <a href="${BLOG_URL}feed.json">JSON Feed</a></p>
${posts.length ? posts.map((p) => `<article><h2><a href="${postUrl(p.slug)}">${esc(p.title)}</a></h2><p>${time(p.date)} | ${esc(p.author)} | ${p.readingMinutes} min read</p><p>${esc(p.description)}</p>${p.tags.length ? `<ul>${p.tags.map((t) => `<li>${esc(t)}</li>`).join("")}</ul>` : ""}</article>`).join("\n") : "<p>No posts yet.</p>"}</main>`;

write(
  "blog/index.html",
  page({
    title: `Blog - ${SITE_NAME}`,
    description: BLOG_DESCRIPTION,
    url: BLOG_URL,
    body: indexBody,
    ld: {
      "@context": "https://schema.org",
      "@graph": [
        {
          "@type": "Blog",
          "@id": `${BLOG_URL}#blog`,
          name: BLOG_TITLE,
          description: BLOG_DESCRIPTION,
          url: BLOG_URL,
          inLanguage: "en",
          publisher,
          blogPost: posts.map((p) => ({ "@type": "BlogPosting", headline: p.title, url: postUrl(p.slug), datePublished: p.date, dateModified: p.updated, author: person(p.author) })),
        },
        crumbs([["Home", SITE], ["Blog", BLOG_URL]]),
      ],
    },
  }),
);

for (const p of posts) {
  const url = postUrl(p.slug);
  const neighbours = [p.prev && `<a rel="prev" href="${postUrl(p.prev.slug)}">Previous: ${esc(p.prev.title)}</a>`, p.next && `<a rel="next" href="${postUrl(p.next.slug)}">Next: ${esc(p.next.title)}</a>`].filter(Boolean);
  const body = `<main data-snapshot style="${blogStyle}">${blogNav}<article itemscope itemtype="https://schema.org/BlogPosting">
<header><h1 itemprop="headline">${esc(p.title)}</h1><p itemprop="description">${esc(p.description)}</p>
<p>By <span itemprop="author">${esc(p.author)}</span>${p.ai ? '<sup><a href="#ai-note" aria-label="AI assistance note">*</a></sup>' : ""} | Published ${time(p.date).replace("<time", '<time itemprop="datePublished"')}${sameDay(p.date, p.updated) ? "" : ` | Updated ${time(p.updated).replace("<time", '<time itemprop="dateModified"')}`} | ${p.readingMinutes} min read</p>
${p.tags.length ? `<ul aria-label="Tags">${p.tags.map((t) => `<li>${esc(t)}</li>`).join("")}</ul>` : ""}</header>
<div itemprop="articleBody">${p.html}</div>
${aiNote(p)}
${neighbours.length ? `<footer><nav aria-label="Previous and next posts">${neighbours.join(" | ")}</nav></footer>` : ""}</article></main>`;
  const head = [
    `<meta property="article:published_time" content="${p.date}" />`,
    `<meta property="article:modified_time" content="${p.updated}" />`,
    `<meta property="article:author" content="${esc(p.author)}" />`,
    `<meta property="article:section" content="Blog" />`,
    ...p.tags.map((t) => `<meta property="article:tag" content="${esc(t)}" />`),
    ...(p.prev ? [`<link rel="prev" href="${postUrl(p.prev.slug)}" />`] : []),
    ...(p.next ? [`<link rel="next" href="${postUrl(p.next.slug)}" />`] : []),
  ];
  write(
    `blog/${p.slug}/index.html`,
    page({
      title: `${p.title} - ${SITE_NAME}`,
      description: p.description,
      url,
      body,
      type: "article",
      image: { url: p.image, alt: p.imageAlt, width: p.imageWidth, height: p.imageHeight },
      head,
      ld: { "@context": "https://schema.org", "@graph": [postLd(p), crumbs([["Home", SITE], ["Blog", BLOG_URL], [p.title, url]])] },
    }),
  );
}

// Feeds
const feedHtml = (p: Post) =>
  p.html
    .replace(/<button type="button" class="copy-btn"[^>]*>Copy<\/button>/g, "")
    .replace(/<a class="anchor"[^>]*>#<\/a>/g, "")
    .replace(/(href|src)="(#[^"]*)"/g, (_m, a, h) => `${a}="${postUrl(p.slug)}${h}"`)
    .replace(/(href|src)="\/(?!\/)([^"]*)"/g, (_m, a, path) => `${a}="${SITE}${path}"`) + (p.ai ? `<hr /><p><small>AI assistance: ${esc(p.ai)}</small></p>` : "");
const xmlDecl = '<?xml version="1.0" encoding="UTF-8"?>\n';

write(
  "blog/rss.xml",
  `${xmlDecl}<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:content="http://purl.org/rss/1.0/modules/content/" xmlns:dc="http://purl.org/dc/elements/1.1/">
<channel>
<title>${esc(BLOG_TITLE)}</title>
<link>${BLOG_URL}</link>
<description>${esc(BLOG_DESCRIPTION)}</description>
<language>en</language>
<lastBuildDate>${new Date(lastUpdated).toUTCString()}</lastBuildDate>
<atom:link href="${BLOG_URL}rss.xml" rel="self" type="application/rss+xml" />
<image><url>${SITE}icon-512.png</url><title>${esc(BLOG_TITLE)}</title><link>${BLOG_URL}</link></image>
${posts
  .map(
    (p) => `<item>
<title>${esc(p.title)}</title>
<link>${postUrl(p.slug)}</link>
<guid isPermaLink="true">${postUrl(p.slug)}</guid>
<pubDate>${new Date(p.date).toUTCString()}</pubDate>
<dc:creator>${esc(p.author)}</dc:creator>
<description>${esc(p.description)}</description>
${p.tags.map((t) => `<category>${esc(t)}</category>`).join("\n")}
<content:encoded>${esc(feedHtml(p))}</content:encoded>
</item>`,
  )
  .join("\n")}
</channel>
</rss>
`,
);

write(
  "blog/atom.xml",
  `${xmlDecl}<feed xmlns="http://www.w3.org/2005/Atom" xml:lang="en">
<title>${esc(BLOG_TITLE)}</title>
<subtitle>${esc(BLOG_DESCRIPTION)}</subtitle>
<id>${BLOG_URL}</id>
<link rel="alternate" type="text/html" href="${BLOG_URL}" />
<link rel="self" type="application/atom+xml" href="${BLOG_URL}atom.xml" />
<updated>${lastUpdated}</updated>
<icon>${SITE}favicon-32.png</icon>
<logo>${SITE}icon-512.png</logo>
${posts
  .map(
    (p) => `<entry>
<title>${esc(p.title)}</title>
<id>${postUrl(p.slug)}</id>
<link rel="alternate" type="text/html" href="${postUrl(p.slug)}" />
<published>${p.date}</published>
<updated>${p.updated}</updated>
<author><name>${esc(p.author)}</name></author>
<summary>${esc(p.description)}</summary>
${p.tags.map((t) => `<category term="${esc(t)}" />`).join("\n")}
<content type="html">${esc(feedHtml(p))}</content>
</entry>`,
  )
  .join("\n")}
</feed>
`,
);

write(
  "blog/feed.json",
  JSON.stringify(
    {
      version: "https://jsonfeed.org/version/1.1",
      title: BLOG_TITLE,
      home_page_url: BLOG_URL,
      feed_url: `${BLOG_URL}feed.json`,
      description: BLOG_DESCRIPTION,
      icon: `${SITE}icon-512.png`,
      favicon: `${SITE}favicon-32.png`,
      language: "en",
      items: posts.map((p) => ({
        id: postUrl(p.slug),
        url: postUrl(p.slug),
        title: p.title,
        summary: p.description,
        content_html: feedHtml(p),
        image: p.image,
        date_published: p.date,
        date_modified: p.updated,
        authors: [{ name: p.author }],
        tags: p.tags,
      })),
    },
    null,
    2,
  ),
);

// Sitemap
const today = new Date().toISOString().slice(0, 10);
const entries: [string, string][] = [
  [SITE, today],
  ...nav.map((p): [string, string] => [`${SITE}docs/${p.slug}/`, today]),
  [BLOG_URL, lastUpdated],
  ...posts.map((p): [string, string] => [postUrl(p.slug), p.updated]),
];
writeFileSync(join(dist, "sitemap.xml"), `${xmlDecl}<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${entries.map(([u, d]) => `  <url><loc>${u}</loc><lastmod>${d}</lastmod></url>`).join("\n")}\n</urlset>\n`);
writeFileSync(join(dist, ".nojekyll"), "");
if (!existsSync(join(dist, "robots.txt"))) throw new Error("robots.txt missing from dist");
console.log(`prerendered home, ${docs.length} docs pages, the blog index, ${posts.length} posts and 3 feeds`);
