import { useCallback, useLayoutEffect, useRef } from "react";
import { isAtBottom } from "./scroll";

export function useStickToBottom<T extends HTMLElement>(content: unknown) {
  const ref = useRef<T>(null);
  const atBottom = useRef(true);

  const scrollToBottom = useCallback(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onScroll = () => {
      atBottom.current = isAtBottom(el);
    };
    const resize = new ResizeObserver(() => {
      if (atBottom.current) scrollToBottom();
    });
    el.addEventListener("scroll", onScroll, { passive: true });
    resize.observe(el);
    return () => {
      el.removeEventListener("scroll", onScroll);
      resize.disconnect();
    };
  }, [scrollToBottom]);

  useLayoutEffect(() => {
    if (atBottom.current) scrollToBottom();
  }, [content, scrollToBottom]);

  const stick = useCallback(() => {
    atBottom.current = true;
    scrollToBottom();
  }, [scrollToBottom]);

  return { ref, stick };
}
