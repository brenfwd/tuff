import { useEffect } from "react";
import { DESCRIPTION, SITE_URL } from "./site";

const DEFAULT_IMAGE = { url: `${SITE_URL}og.png`, alt: "tuff: a Minecraft Java Edition server written in Rust", width: 1200, height: 630 };

function setMeta(attr: "name" | "property", key: string, value: string | null) {
  let el = document.head.querySelector<HTMLMetaElement>(`meta[${attr}="${key}"]`);
  if (value === null) return el?.remove();
  if (!el) {
    el = document.createElement("meta");
    el.setAttribute(attr, key);
    document.head.appendChild(el);
  }
  el.content = value;
}

type Seo = {
  title: string;
  description?: string;
  path?: string;
  type?: "website" | "article";
  image?: { url: string; alt: string; width: number | null; height: number | null };
};

export function useSeo({ title, description = DESCRIPTION, path = "", type = "website", image = DEFAULT_IMAGE }: Seo) {
  useEffect(() => {
    const url = SITE_URL + path;
    document.title = title;
    setMeta("name", "description", description);
    setMeta("property", "og:type", type);
    setMeta("property", "og:title", title);
    setMeta("property", "og:description", description);
    setMeta("property", "og:url", url);
    setMeta("property", "og:image", image.url);
    setMeta("property", "og:image:width", image.width ? String(image.width) : null);
    setMeta("property", "og:image:height", image.height ? String(image.height) : null);
    setMeta("property", "og:image:alt", image.alt);
    setMeta("name", "twitter:title", title);
    setMeta("name", "twitter:description", description);
    setMeta("name", "twitter:image", image.url);
    const canonical = document.head.querySelector<HTMLLinkElement>('link[rel="canonical"]');
    if (canonical) canonical.href = url;
  }, [title, description, path, type, image.url, image.alt, image.width, image.height]);
}
