// Sign-in handoff: from "Continue with Google" to a paired browser with nothing
// typed. Ported from passband-web/spikes/pairing (see its FINDINGS.md).
//
// 1. `startSignIn` mints a one-shot `state`, keeps it in this tab's
//    sessionStorage, and sends the browser to control.
// 2. Control redirects back to /connect with `url`, `code` and `state` in the
//    FRAGMENT, which is never sent to a server or put in a Referer.
// 3. `takeHandoff` erases the fragment before anything else runs.
// 4. `checkHandoff` refuses anything this tab did not start, and any server
//    that is not a hosted tenant.

const STATE_KEY = "pb_pair_state";

/** Hosted tenants only. */
const HOSTED = /^https:\/\/[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.passband\.email$/;
/**
 * Dev, test and e2e builds only: a local daemon. `__ALLOW_LOOPBACK__` is a
 * build-time constant, so a production bundle does not contain this pattern.
 */
const LOOPBACK = __ALLOW_LOOPBACK__ ? /^http:\/\/(?:127\.0\.0\.1|localhost):\d{2,5}$/ : null;

export function serverAllowed(url: string | null | undefined, allowLoopback: boolean): boolean {
  if (typeof url !== "string") return false;
  return HOSTED.test(url) || (allowLoopback && LOOPBACK !== null && LOOPBACK.test(url));
}

export interface Handoff {
  url: string | null;
  code: string | null;
  state: string | null;
}

export type HandoffRefusal = "state" | "server" | "code";

function randomState(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function signInUrl(controlUrl: string, state: string, provider?: "microsoft"): string {
  const u = new URL(controlUrl);
  u.searchParams.set("return", "web");
  u.searchParams.set("state", state);
  if (provider) u.searchParams.set("provider", provider);
  return u.toString();
}

export function startSignIn(
  controlUrl: string,
  provider?: "microsoft",
  storage: Storage = sessionStorage,
  go: (url: string) => void = (u) => location.assign(u),
): void {
  const state = randomState();
  storage.setItem(STATE_KEY, state);
  go(signInUrl(controlUrl, state, provider));
}

/** Read the fragment and erase it from the address bar and history. */
export function takeHandoff(loc: Location = location, hist: History = history): Handoff {
  const params = new URLSearchParams(loc.hash.slice(1));
  hist.replaceState(null, "", loc.pathname);
  return { url: params.get("url"), code: params.get("code"), state: params.get("state") };
}

/**
 * Null when the handoff may be claimed. The stored state is consumed on every
 * call, matched or not, so no second link can complete a pending sign-in.
 */
export function checkHandoff(
  h: Handoff,
  allowLoopback: boolean,
  storage: Storage = sessionStorage,
): HandoffRefusal | null {
  const expected = storage.getItem(STATE_KEY);
  storage.removeItem(STATE_KEY);
  if (!expected || !h.state || h.state !== expected) return "state";
  if (!serverAllowed(h.url, allowLoopback)) return "server";
  if (!h.code || !/^[A-Za-z0-9-]{4,32}$/.test(h.code)) return "code";
  return null;
}

/** "Chrome on macOS": what the token is labelled with, so it can be revoked later. */
export function deviceName(ua: string = navigator.userAgent): string {
  const browser = /Edg\//.test(ua)
    ? "Edge"
    : /Firefox\//.test(ua)
      ? "Firefox"
      : /Chrome\//.test(ua)
        ? "Chrome"
        : /Safari\//.test(ua)
          ? "Safari"
          : "Browser";
  const os = /Mac OS X/.test(ua) && !/iPhone|iPad/.test(ua)
    ? "macOS"
    : /iPhone|iPad/.test(ua)
      ? "iOS"
      : /Windows/.test(ua)
        ? "Windows"
        : /Android/.test(ua)
          ? "Android"
          : /Linux/.test(ua)
            ? "Linux"
            : "web";
  return `${browser} on ${os} (web)`;
}
