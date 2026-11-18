// Three routes, so no routing library: `/`, `/connect`, `/t/<thread id>`.
import { useSyncExternalStore } from "react";

export type Route =
  | { name: "home" }
  | { name: "connect" }
  | { name: "thread"; threadId: string };

export function parseRoute(pathname: string): Route {
  if (pathname === "/connect") return { name: "connect" };
  const m = /^\/t\/([^/]+)$/.exec(pathname);
  if (m?.[1]) {
    try {
      return { name: "thread", threadId: decodeURIComponent(m[1]) };
    } catch {
      return { name: "home" };
    }
  }
  return { name: "home" };
}

export function pathFor(r: Route): string {
  switch (r.name) {
    case "home":
      return "/";
    case "connect":
      return "/connect";
    case "thread":
      return `/t/${encodeURIComponent(r.threadId)}`;
  }
}

const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());
if (typeof window !== "undefined") window.addEventListener("popstate", notify);

export function navigate(r: Route, replace = false): void {
  const path = pathFor(r);
  if (replace) history.replaceState(null, "", path);
  else history.pushState(null, "", path);
  notify();
}

export function useRoute(): Route {
  const path = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => location.pathname,
  );
  return parseRoute(path);
}
