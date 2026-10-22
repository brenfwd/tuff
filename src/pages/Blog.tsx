import { useEffect, useState } from "react";
import { Link, useLocation } from "wouter";
import blogIndex from "../generated/blog/index.json";
import { useHashParam, useHashSection } from "../lib/hash";
import { useSeo } from "../lib/useSeo";
import { onArticleClick, useActiveHeading } from "../lib/article";
import { formatDate, sameDay, tagKey, type BlogIndex, type Post, type PostMeta } from "../lib/blog";
import { ArrowLeft, ArrowRight } from "../components/Icons";
import { SITE_NAME } from "../lib/site";

const index = blogIndex as BlogIndex;
const modules = import.meta.glob<{ default: Post }>(["../generated/blog/*.json", "!../generated/blog/index.json"]);
const load = (slug: string) => modules[`../generated/blog/${slug}.json`]?.().then((m) => m.default);

const BLOG_DESCRIPTION = "News, release notes and engineering write-ups from the tuff Minecraft server project.";

function Time({ iso, className }: { iso: string; className?: string }) {
  return (
    <time dateTime={iso} className={className}>
      {formatDate(iso)}
    </time>
  );
}

function Tag({ name, active }: { name: string; active?: boolean }) {
  return (
    <Link
      href={`/blog?tag=${tagKey(name)}`}
      className={`inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium transition-colors ${active ? "border-ember/40 bg-ember-soft text-ember-text" : "border-line text-fg-muted hover:border-line-strong hover:text-fg"}`}
    >
      {name}
    </Link>
  );
}

function PostMetaLine({ post, showUpdated }: { post: PostMeta; showUpdated?: boolean }) {
  return (
    <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-fg-subtle">
      <Time iso={post.date} />
      {showUpdated && !sameDay(post.date, post.updated) && (
        <>
          <span aria-hidden="true">/</span>
          <span>
            Updated <Time iso={post.updated} />
          </span>
        </>
      )}
      <span aria-hidden="true">/</span>
      <span>{post.readingMinutes} min read</span>
      {post.draft && <span className="rounded-md bg-ember-soft px-1.5 py-0.5 text-xs font-semibold text-ember-text">Draft</span>}
    </p>
  );
}

function Feeds() {
  return (
    <p className="text-sm text-fg-subtle">
      Subscribe via{" "}
      {[
        ["rss.xml", "RSS"],
        ["atom.xml", "Atom"],
        ["feed.json", "JSON Feed"],
      ].map(([file, label], i) => (
        <span key={file}>
          {i > 0 && ", "}
          <a href={`${import.meta.env.BASE_URL}blog/${file}`} className="text-fg-muted underline decoration-line-strong underline-offset-4 hover:text-fg">
            {label}
          </a>
        </span>
      ))}
    </p>
  );
}

function BlogIndexPage() {
  const tag = useHashParam("tag");
  const activeTag = index.tags.find((t) => tagKey(t.name) === tag)?.name ?? null;
  const posts = activeTag ? index.posts.filter((p) => p.tags.includes(activeTag)) : index.posts;
  useSeo({ title: `Blog - ${SITE_NAME}`, description: BLOG_DESCRIPTION, path: "blog/" });

  return (
    <main id="main" tabIndex={-1} className="mx-auto max-w-3xl px-4 py-12 outline-none sm:px-6 lg:py-16">
      <header>
        <h1 className="font-display text-[2.1rem] font-bold leading-tight tracking-tight sm:text-5xl">Blog</h1>
        <p className="mt-4 text-lg leading-8 text-fg-muted">{BLOG_DESCRIPTION}</p>
        <div className="mt-3">
          <Feeds />
        </div>
      </header>

      {index.tags.length > 0 && (
        <nav aria-label="Filter by tag" className="mt-8 flex flex-wrap gap-2">
          <Link
            href="/blog"
            aria-current={activeTag ? undefined : "page"}
            className={`inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium transition-colors ${activeTag ? "border-line text-fg-muted hover:border-line-strong hover:text-fg" : "border-ember/40 bg-ember-soft text-ember-text"}`}
          >
            All posts
          </Link>
          {index.tags.map((t) => (
            <Tag key={t.name} name={t.name} active={t.name === activeTag} />
          ))}
        </nav>
      )}

      {posts.length === 0 ? (
        <p className="mt-12 rounded-xl border border-line bg-bg-raised px-5 py-8 text-center text-fg-muted">{activeTag ? `No posts tagged "${activeTag}".` : "No posts yet. Subscribe to a feed to hear about the first one."}</p>
      ) : (
        <ol className="mt-10 divide-y divide-line border-t border-line">
          {posts.map((p) => (
            <li key={p.slug}>
              <article className="py-8">
                <PostMetaLine post={p} />
                <h2 className="mt-2 font-display text-xl font-semibold tracking-tight sm:text-2xl">
                  <Link href={`/blog/${p.slug}`} className="hover:text-ember-text">
                    {p.title}
                  </Link>
                </h2>
                <p className="mt-2 leading-7 text-fg-muted">{p.description}</p>
                <div className="mt-4 flex flex-wrap items-center gap-2">
                  {p.tags.map((t) => (
                    <Tag key={t} name={t} active={t === activeTag} />
                  ))}
                  <Link href={`/blog/${p.slug}`} className="ml-auto inline-flex items-center gap-1 text-sm font-medium text-ember-text" aria-label={`Read ${p.title}`}>
                    Read <ArrowRight className="h-3.5 w-3.5" />
                  </Link>
                </div>
              </article>
            </li>
          ))}
        </ol>
      )}
    </main>
  );
}

function BlogPostPage({ slug }: { slug: string }) {
  const section = useHashSection();
  const [post, setPost] = useState<Post | null | undefined>(undefined);

  useEffect(() => {
    let live = true;
    const p = load(slug);
    if (!p) setPost(null);
    else p.then((d) => live && setPost(d));
    return () => {
      live = false;
    };
  }, [slug]);

  useSeo({
    title: post ? `${post.title} - ${SITE_NAME}` : `Blog - ${SITE_NAME}`,
    description: post?.description ?? BLOG_DESCRIPTION,
    path: post ? `blog/${post.slug}/` : "blog/",
    type: post ? "article" : "website",
    image: post ? { url: post.image, alt: post.imageAlt, width: post.imageWidth, height: post.imageHeight } : undefined,
  });

  useEffect(() => {
    if (!post || post.slug !== slug) return;
    if (section) document.getElementById(section)?.scrollIntoView();
    else scrollTo({ top: 0 });
  }, [post, slug, section]);

  const active = useActiveHeading(post?.toc.map((t) => t.id) ?? []);
  const tocLink = (id: string) => `#/blog/${slug}?h=${id}`;

  if (post === null)
    return (
      <main id="main" tabIndex={-1} className="mx-auto max-w-3xl px-4 py-24 text-center">
        <h1 className="font-display text-3xl font-bold">Post not found</h1>
        <p className="mt-3 text-fg-muted">There is no blog post at this address.</p>
        <Link href="/blog" className="mt-6 inline-block font-medium text-ember-text underline underline-offset-4">
          See all posts
        </Link>
      </main>
    );

  return (
    <div className="mx-auto max-w-6xl px-4 sm:px-6">
      <div className="xl:grid xl:grid-cols-[minmax(0,1fr)_13rem] xl:gap-12">
        <main id="main" tabIndex={-1} className="min-w-0 py-8 outline-none lg:py-12">
          {post && (
            <article className="mx-auto max-w-[46rem]">
              <Link href="/blog" className="inline-flex items-center gap-1.5 text-sm font-medium text-fg-muted hover:text-fg">
                <ArrowLeft className="h-3.5 w-3.5" /> Blog
              </Link>
              <header className="mt-6">
                <PostMetaLine post={post} showUpdated />
                <h1 className="mt-3 font-display text-[2.1rem] font-bold leading-tight tracking-tight sm:text-5xl">{post.title}</h1>
                <p className="mt-4 text-lg leading-8 text-fg-muted">{post.description}</p>
                <div className="mt-5 flex flex-wrap items-center gap-x-4 gap-y-3 border-b border-line pb-6">
                  <span className="text-sm font-medium text-fg">{post.author}</span>
                  {post.tags.length > 0 && (
                    <ul className="flex flex-wrap gap-2" aria-label="Tags">
                      {post.tags.map((t) => (
                        <li key={t}>
                          <Tag name={t} />
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              </header>
              {post.toc.length > 2 && (
                <details className="mt-6 rounded-xl border border-line bg-bg-raised px-4 py-3 xl:hidden">
                  <summary className="cursor-pointer text-sm font-medium">On this page</summary>
                  <ul className="mt-3 space-y-2 text-sm">
                    {post.toc.filter((t) => t.level === 2).map((t) => (
                      <li key={t.id}>
                        <a href={tocLink(t.id)} className="text-fg-muted hover:text-fg">
                          {t.text}
                        </a>
                      </li>
                    ))}
                  </ul>
                </details>
              )}
              <div className="doc-prose blog-prose mt-8" onClick={onArticleClick(`/blog/${slug}`)} dangerouslySetInnerHTML={{ __html: post.html }} />

              {(post.prev || post.next) && (
                <nav aria-label="Previous and next posts" className="mt-16 grid gap-4 border-t border-line pt-8 sm:grid-cols-2">
                  {post.prev ? (
                    <Link href={`/blog/${post.prev.slug}`} className="group flex flex-col rounded-xl border border-line p-5 transition-colors hover:border-line-strong">
                      <span className="flex items-center gap-1.5 text-xs text-fg-subtle">
                        <ArrowLeft className="h-3.5 w-3.5" /> Older post
                      </span>
                      <span className="mt-1 font-medium text-fg group-hover:text-ember-text">{post.prev.title}</span>
                    </Link>
                  ) : (
                    <span />
                  )}
                  {post.next && (
                    <Link href={`/blog/${post.next.slug}`} className="group flex flex-col rounded-xl border border-line p-5 text-right transition-colors hover:border-line-strong">
                      <span className="flex items-center justify-end gap-1.5 text-xs text-fg-subtle">
                        Newer post <ArrowRight className="h-3.5 w-3.5" />
                      </span>
                      <span className="mt-1 font-medium text-fg group-hover:text-ember-text">{post.next.title}</span>
                    </Link>
                  )}
                </nav>
              )}
              <div className="mt-10 border-t border-line pt-6">
                <Feeds />
              </div>
            </article>
          )}
        </main>

        <aside className="hidden xl:block" aria-label="On this page">
          {post && post.toc.length > 1 && (
            <div className="sticky top-16 max-h-[calc(100vh-4rem)] overflow-y-auto py-12">
              <h2 className="text-xs font-semibold uppercase tracking-[0.12em] text-fg-subtle">On this page</h2>
              <ul className="mt-3 space-y-1 border-l border-line text-[0.84rem]">
                {post.toc.map((t) => (
                  <li key={t.id}>
                    <a
                      href={tocLink(t.id)}
                      className={`-ml-px block border-l py-1 transition-colors ${t.level === 3 ? "pl-6" : "pl-3"} ${active === t.id ? "border-ember font-medium text-fg" : "border-transparent text-fg-muted hover:text-fg"}`}
                    >
                      {t.text}
                    </a>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </aside>
      </div>
    </div>
  );
}

export default function Blog() {
  const [path] = useLocation();
  const slug = path.replace(/^\/blog\/?/, "").replace(/\/$/, "");
  return slug ? <BlogPostPage key={slug} slug={slug} /> : <BlogIndexPage />;
}
