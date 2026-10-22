export type TocEntry = { level: number; id: string; text: string };

export type PostMeta = {
  slug: string;
  title: string;
  description: string;
  date: string;
  updated: string;
  author: string;
  tags: string[];
  image: string;
  imageAlt: string;
  imageWidth: number | null;
  imageHeight: number | null;
  generatedImage: boolean;
  wordCount: number;
  readingMinutes: number;
  excerpt: string;
  draft: boolean;
};

export type PostLink = { slug: string; title: string };
export type Post = PostMeta & { html: string; toc: TocEntry[]; prev: PostLink | null; next: PostLink | null };
export type BlogIndex = { posts: PostMeta[]; tags: { name: string; count: number }[] };

export const tagKey = (tag: string) => tag.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");

export const formatDate = (iso: string) => new Date(iso).toLocaleDateString("en-US", { year: "numeric", month: "long", day: "numeric", timeZone: "UTC" });

export const sameDay = (a: string, b: string) => a.slice(0, 10) === b.slice(0, 10);
