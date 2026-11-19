// Spike 4: zero-typing pairing handoff, app side.
//
// Flow: the app starts sign-in (mints a one-shot `state`, keeps it in this
// tab's sessionStorage), control does the identity step and redirects back to
// /connect with `url`, `code` and `state` in the FRAGMENT (never sent to any
// server, never in a Referer). The app strips the fragment before doing
// anything else, then refuses the handoff unless it started it and the server
// is one it is allowed to talk to. Only then does it claim the code.

/** Servers the app may pair with. Production: hosted tenants only. */
export const SERVER_RULES = [
  /^https:\/\/[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.passband\.email$/,
];

const STATE_KEY = "pb_pair_state";

export function startSignIn(controlUrl, win = window) {
  const bytes = new Uint8Array(32);
  win.crypto.getRandomValues(bytes);
  const state = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  win.sessionStorage.setItem(STATE_KEY, state);
  const u = new URL(controlUrl);
  u.searchParams.set("return", "web");
  u.searchParams.set("state", state);
  win.location.assign(u.toString());
}

/** Parse and IMMEDIATELY erase the fragment, whatever happens next. */
export function takeHandoff(win = window) {
  const params = new URLSearchParams(win.location.hash.slice(1));
  win.history.replaceState(null, "", win.location.pathname);
  return {
    url: params.get("url"),
    code: params.get("code"),
    state: params.get("state"),
  };
}

export function serverAllowed(url, rules = SERVER_RULES) {
  return typeof url === "string" && rules.some((r) => r.test(url));
}

/**
 * Validate a handoff. The stored state is consumed on every call, matched or
 * not, so a second link in the same tab cannot reuse a pending sign-in.
 */
export function checkHandoff(h, rules, storage = sessionStorage) {
  const expected = storage.getItem(STATE_KEY);
  storage.removeItem(STATE_KEY);
  if (!expected || !h.state || h.state !== expected) return "state";
  if (!serverAllowed(h.url, rules)) return "server";
  if (!h.code || !/^[A-Za-z0-9-]{4,32}$/.test(h.code)) return "code";
  return null;
}

export async function claim(h, deviceName, fetchImpl = fetch) {
  const resp = await fetchImpl(`${h.url}/client/pair`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code: h.code, device_name: deviceName }),
    credentials: "omit",
    redirect: "error",
    referrerPolicy: "no-referrer",
    cache: "no-store",
  });
  if (!resp.ok) throw new Error(`pair refused (${resp.status})`);
  const body = await resp.json();
  if (typeof body.token !== "string") throw new Error("pair answer had no token");
  return body;
}
