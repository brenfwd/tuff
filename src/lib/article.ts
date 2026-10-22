import { useEffect, useState, type MouseEvent } from "react";
import { navigate } from "./hash";

export function useActiveHeading(ids: string[]) {
  const [active, setActive] = useState<string | null>(null);
  useEffect(() => {
    if (!ids.length) return;
    const els = ids.map((id) => document.getElementById(id)).filter(Boolean) as HTMLElement[];
    const onScroll = () => {
      let current: string | null = null;
      for (const el of els) if (el.getBoundingClientRect().top < 120) current = el.id;
      setActive(current ?? ids[0]);
    };
    onScroll();
    addEventListener("scroll", onScroll, { passive: true });
    return () => removeEventListener("scroll", onScroll);
  }, [ids]);
  return active;
}

// Handles copy buttons and in-page heading links inside rendered Markdown.
export const onArticleClick = (route: string) => (e: MouseEvent) => {
  const target = e.target as HTMLElement;
  const copy = target.closest<HTMLButtonElement>(".copy-btn");
  if (copy) {
    const code = copy.closest(".code-block")?.querySelector("pre")?.innerText ?? "";
    navigator.clipboard?.writeText(code).then(() => {
      copy.textContent = "Copied";
      setTimeout(() => (copy.textContent = "Copy"), 1500);
    });
    return;
  }
  const a = target.closest<HTMLAnchorElement>("a[href^='#']");
  const href = a?.getAttribute("href");
  if (a && href && !href.startsWith("#/")) {
    e.preventDefault();
    navigate(`${route}?h=${href.slice(1)}`, { replace: true });
    document.getElementById(href.slice(1))?.scrollIntoView();
  }
};
