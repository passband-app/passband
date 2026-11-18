import { describe, expect, test, vi } from "vitest";
import {
  ApiError,
  InPageTransport,
  MemoryCredentialStore,
  assertPath,
  buildQuery,
} from "../src/api/transport";

const SERVER = "https://tenant.passband.example";
const TOKEN = "sqd_secret_token_value";

function harness(respond: (url: string, init: RequestInit) => Response | Promise<Response>) {
  const calls: { url: string; init: RequestInit }[] = [];
  const fetchImpl = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(url), init: init ?? {} });
    return respond(String(url), init ?? {});
  });
  const store = new MemoryCredentialStore({ server: SERVER, token: TOKEN });
  const t = new InPageTransport(store, fetchImpl as unknown as typeof fetch);
  return { t, store, calls };
}

describe("request discipline", () => {
  test("token only in Authorization; no cookies, no referrer, no redirects", async () => {
    const { t, calls } = harness(() => new Response("{}", { status: 200 }));
    await t.request("/client/stats", { query: { limit: 5 } });
    const c = calls[0]!;
    expect(c.url).toBe(`${SERVER}/client/stats?limit=5`);
    expect(c.url).not.toContain(TOKEN);
    const h = c.init.headers as Record<string, string>;
    expect(h.Authorization).toBe(`Bearer ${TOKEN}`);
    expect(h["X-Passband-Client"]).toMatch(/^web\//);
    expect(c.init.credentials).toBe("omit");
    expect(c.init.redirect).toBe("error");
    expect(c.init.referrerPolicy).toBe("no-referrer");
  });

  test("a JSON body sets the content type", async () => {
    const { t, calls } = harness(() => new Response("{}"));
    await t.request("/client/actions/archive", { method: "POST", body: { message_id: 1 } });
    const h = calls[0]!.init.headers as Record<string, string>;
    expect(h["Content-Type"]).toBe("application/json");
    expect(calls[0]!.init.body).toBe('{"message_id":1}');
  });

  test("paths that could leave the /client tree are refused before any request", async () => {
    const { t, calls } = harness(() => new Response("{}"));
    for (const p of ["/console", "//evil.example/x", "/client/../x?y", "https://evil.example/client/x", "/client/a#b"]) {
      await expect(t.request(p)).rejects.toThrow();
    }
    expect(calls).toHaveLength(0);
  });
});

describe("failures", () => {
  test("401 forgets the credential and reports unauthorized", async () => {
    const { t, store } = harness(() => new Response("", { status: 401 }));
    await expect(t.request("/client/stats")).rejects.toMatchObject({ kind: "unauthorized" });
    expect(await store.load()).toBeNull();
    expect(t.server()).toBeNull();
    await expect(t.request("/client/stats")).rejects.toMatchObject({ kind: "signed_out" });
  });

  test("other statuses keep the credential", async () => {
    const { t, store } = harness(() => new Response("", { status: 503 }));
    await expect(t.request("/client/stats")).rejects.toMatchObject({ kind: "http", status: 503 });
    expect(await store.load()).not.toBeNull();
  });

  test("a network error never carries the URL or the token", async () => {
    const { t } = harness(() => {
      throw new TypeError(`Failed to fetch ${SERVER}/client/stats Bearer ${TOKEN}`);
    });
    const err = await t.request("/client/stats").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).kind).toBe("network");
    expect(String((err as Error).message)).not.toContain(TOKEN);
    expect(String((err as Error).message)).not.toContain(SERVER);
  });

  test("a request past its deadline is a timeout", async () => {
    vi.useFakeTimers();
    try {
      const { t } = harness(
        (_u, init) =>
          new Promise((_r, reject) =>
            init.signal?.addEventListener("abort", () => reject(new Error("aborted"))),
          ),
      );
      const p = t.request("/client/stats", { timeoutMs: 1000 }).catch((e: unknown) => e);
      await vi.advanceTimersByTimeAsync(1001);
      expect(await p).toMatchObject({ kind: "timeout" });
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("pairing", () => {
  test("a minted token is kept, and only in the store", async () => {
    const store = new MemoryCredentialStore();
    const calls: RequestInit[] = [];
    const t = new InPageTransport(store, (async (_u: unknown, init?: RequestInit) => {
      calls.push(init!);
      return new Response(JSON.stringify({ token: "sqd_new", device_id: 9, name: "Web browser" }));
    }) as unknown as typeof fetch);
    const r = await t.pair(SERVER, "ABCD-EFGH", "Web browser");
    expect(r).toEqual({ deviceName: "Web browser" });
    expect(await store.load()).toEqual({ server: SERVER, token: "sqd_new" });
    expect(calls[0]!.credentials).toBe("omit");
    expect((calls[0]!.headers as Record<string, string>).Authorization).toBeUndefined();
  });

  test("a refused claim stores nothing", async () => {
    const store = new MemoryCredentialStore();
    const t = new InPageTransport(store, (async () => new Response("", { status: 401 })) as unknown as typeof fetch);
    await expect(t.pair(SERVER, "nope", "Web")).rejects.toMatchObject({ kind: "unauthorized" });
    expect(await store.load()).toBeNull();
  });
});

describe("helpers", () => {
  test("buildQuery drops absent values", () => {
    expect(buildQuery({ a: 1, b: undefined, c: null, d: "x y" })).toBe("?a=1&d=x+y");
    expect(buildQuery(undefined)).toBe("");
  });
  test("assertPath", () => {
    expect(() => assertPath("/client/thread/abc")).not.toThrow();
    expect(() => assertPath("/client/thread/a b")).toThrow();
  });
});
