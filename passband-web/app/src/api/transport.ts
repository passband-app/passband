// THE ONLY MODULE THAT HOLDS THE DEVICE TOKEN.
//
// Everything that talks to the daemon goes through a `Transport`: callers name
// a path and get a `Response` back, and never see the token or build a URL
// with the server in it. Keeping the token behind this one seam is what lets it
// move later (to a separate vault origin) without touching a single caller.
//
// Request discipline, matching the Mac client's APIClient:
//   - the token travels only in `Authorization`, never in a URL;
//   - no cookies (`credentials: "omit"`), no Referer, no redirects followed;
//   - every request has a deadline;
//   - errors never carry the URL or the token.

import { pairResult } from "./wire";

export const CLIENT_VERSION = "0.1.0";

export type ApiErrorKind =
  | "unauthorized"
  | "forbidden"
  | "not_found"
  | "rate_limited"
  | "http"
  | "timeout"
  | "network"
  | "decode"
  | "signed_out";

export class ApiError extends Error {
  constructor(
    readonly kind: ApiErrorKind,
    readonly status?: number,
  ) {
    super(status ? `${kind} (${status})` : kind);
    this.name = "ApiError";
  }
}

export interface Credential {
  server: string;
  token: string;
}

/** Where the credential lives between visits. */
export interface CredentialStore {
  load(): Promise<Credential | null>;
  save(c: Credential): Promise<void>;
  clear(): Promise<void>;
}

export interface RequestOptions {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  /** Serialized as JSON. */
  body?: unknown;
  query?: Record<string, string | number | boolean | undefined | null>;
  timeoutMs?: number;
  signal?: AbortSignal;
}

export interface Transport {
  /** The server this browser is paired with, or null. Never the token. */
  server(): string | null;
  request(path: string, opts?: RequestOptions): Promise<Response>;
  /** Claim a pairing code against `server` and keep the token it mints. */
  pair(server: string, code: string, deviceName: string): Promise<{ deviceName: string }>;
  /** Forget the credential locally. Server-side revocation is the caller's call first. */
  forget(): Promise<void>;
}

const DEFAULT_TIMEOUT_MS = 15_000;

function timeZone(): string | undefined {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return undefined;
  }
}

export function kindForStatus(status: number): ApiErrorKind {
  if (status === 401) return "unauthorized";
  if (status === 403) return "forbidden";
  if (status === 404) return "not_found";
  if (status === 429) return "rate_limited";
  return "http";
}

/** Paths are fixed strings from this codebase; refuse anything that could change the origin. */
export function assertPath(path: string): void {
  if (!path.startsWith("/client/") || path.includes("//") || /[\s#?\\]/.test(path)) {
    throw new Error("transport: refused path");
  }
}

export function buildQuery(query: RequestOptions["query"]): string {
  if (!query) return "";
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(query)) {
    if (v === undefined || v === null) continue;
    q.set(k, String(v));
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}

export class InPageTransport implements Transport {
  private cred: Credential | null = null;
  private loaded: Promise<void>;

  constructor(
    private readonly store: CredentialStore,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
  ) {
    this.loaded = store.load().then((c) => {
      this.cred = c;
    });
  }

  ready(): Promise<void> {
    return this.loaded;
  }

  server(): string | null {
    return this.cred?.server ?? null;
  }

  async request(path: string, opts: RequestOptions = {}): Promise<Response> {
    await this.loaded;
    assertPath(path);
    const cred = this.cred;
    if (!cred) throw new ApiError("signed_out");

    const headers: Record<string, string> = {
      Authorization: `Bearer ${cred.token}`,
      "X-Passband-Client": `web/${CLIENT_VERSION}`,
    };
    const zone = timeZone();
    if (zone) headers["X-Passband-Timezone"] = zone;
    let body: string | undefined;
    if (opts.body !== undefined) {
      headers["Content-Type"] = "application/json";
      body = JSON.stringify(opts.body);
    }

    const resp = await this.send(
      `${cred.server}${path}${buildQuery(opts.query)}`,
      { method: opts.method ?? "GET", headers, body },
      opts,
    );
    if (resp.status === 401) {
      // The token is dead (revoked, expired, or never valid). Forget it so the
      // app falls back to sign-in instead of retrying a credential that will
      // never work again.
      await this.forget();
    }
    if (!resp.ok) throw new ApiError(kindForStatus(resp.status), resp.status);
    return resp;
  }

  async pair(server: string, code: string, deviceName: string) {
    const resp = await this.send(
      `${server}/client/pair`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        // `platform` puts the token under the daemon's web idle rule from
        // the moment it is minted.
        body: JSON.stringify({ code, device_name: deviceName, platform: "web" }),
      },
      {},
    );
    // Every refusal on this route is a bare 401, by design. Never retried: a
    // miss is charged against the live code.
    if (!resp.ok) throw new ApiError(kindForStatus(resp.status), resp.status);
    let parsed: unknown;
    try {
      parsed = await resp.json();
    } catch {
      throw new ApiError("decode");
    }
    const minted = pairResult(parsed);
    if (!minted) throw new ApiError("decode");
    const cred = { server, token: minted.token };
    await this.store.save(cred);
    this.cred = cred;
    return { deviceName: minted.name };
  }

  async forget(): Promise<void> {
    this.cred = null;
    await this.store.clear();
  }

  private async send(url: string, init: RequestInit, opts: RequestOptions): Promise<Response> {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), opts.timeoutMs ?? DEFAULT_TIMEOUT_MS);
    const onAbort = () => ctl.abort();
    opts.signal?.addEventListener("abort", onAbort, { once: true });
    try {
      return await this.fetchImpl(url, {
        ...init,
        credentials: "omit",
        redirect: "error",
        referrerPolicy: "no-referrer",
        cache: "no-store",
        signal: ctl.signal,
      });
    } catch {
      // Deliberately not the underlying error: it can carry the URL.
      if (ctl.signal.aborted && !opts.signal?.aborted) throw new ApiError("timeout");
      throw new ApiError("network");
    } finally {
      clearTimeout(timer);
      opts.signal?.removeEventListener("abort", onAbort);
    }
  }
}

/** For tests and for a browser with no IndexedDB (private modes): forgotten on reload. */
export class MemoryCredentialStore implements CredentialStore {
  private c: Credential | null = null;
  constructor(initial: Credential | null = null) {
    this.c = initial;
  }
  async load() {
    return this.c;
  }
  async save(c: Credential) {
    this.c = c;
  }
  async clear() {
    this.c = null;
  }
}
