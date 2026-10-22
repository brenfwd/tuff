import { useSyncExternalStore } from "react";

const subscribe = (cb: () => void) => {
  addEventListener("hashchange", cb);
  return () => removeEventListener("hashchange", cb);
};

const raw = () => location.hash.replace(/^#/, "") || "/";
export const currentPath = () => raw().split("?")[0] || "/";
export const currentParam = (name: string) => new URLSearchParams(raw().split("?")[1] ?? "").get(name);
export const currentSection = () => currentParam("h");

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

export function useHashParam(name: string) {
  return useSyncExternalStore(subscribe, () => currentParam(name), () => null);
}
