import { useSyncExternalStore } from "react";

const subscribe = (cb: () => void) => {
  addEventListener("hashchange", cb);
  return () => removeEventListener("hashchange", cb);
};

const raw = () => location.hash.replace(/^#/, "") || "/";
export const currentPath = () => raw().split("?")[0] || "/";
export const currentSection = () => new URLSearchParams(raw().split("?")[1] ?? "").get("h");

export function navigate(to: string, opts?: { replace?: boolean }) {
  const url = `${location.pathname}${location.search}#${to}`;
  if (opts?.replace) {
    history.replaceState(history.state, "", url);
    dispatchEvent(new HashChangeEvent("hashchange"));
  } else location.hash = to;
}

export function useHashLocation(): [string, (to: string, opts?: { replace?: boolean }) => void] {
  const path = useSyncExternalStore(subscribe, currentPath, () => "/");
  return [path, navigate];
}

export function useHashSection() {
  return useSyncExternalStore(subscribe, currentSection, () => null);
}
