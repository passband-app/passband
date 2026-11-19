import { describe, expect, test } from "vitest";
import { checkHandoff, deviceName, serverAllowed, signInUrl } from "../src/session/handoff";

class MemStorage {
  private m = new Map<string, string>();
  get length() {
    return this.m.size;
  }
  clear() {
    this.m.clear();
  }
  getItem(k: string) {
    return this.m.get(k) ?? null;
  }
  key(i: number) {
    return [...this.m.keys()][i] ?? null;
  }
  removeItem(k: string) {
    this.m.delete(k);
  }
  setItem(k: string, v: string) {
    this.m.set(k, v);
  }
}

const stored = (state: string) => {
  const s = new MemStorage() as unknown as Storage;
  s.setItem("pb_pair_state", state);
  return s;
};
const ok = { url: "https://jamie.passband.email", code: "ABCD-EFGH", state: "s1" };

describe("serverAllowed", () => {
  test("hosted tenants only, in production", () => {
    expect(serverAllowed("https://jamie.passband.email", false)).toBe(true);
    expect(serverAllowed("https://a-b-1.passband.email", false)).toBe(true);
    for (const bad of [
      "http://jamie.passband.email",
      "https://jamie.passband.email/",
      "https://jamie.passband.email:8443",
      "https://jamie.passband.email.evil.example",
      "https://evil.example/?x=.passband.email",
      "https://a.b.passband.email",
      "https://-x.passband.email",
      "https://passband.email",
      "http://127.0.0.1:8849",
      null,
    ]) {
      expect(serverAllowed(bad, false), String(bad)).toBe(false);
    }
  });
  test("loopback only when the build allows it", () => {
    expect(serverAllowed("http://127.0.0.1:8849", true)).toBe(true);
    expect(serverAllowed("http://localhost:8849", true)).toBe(true);
    expect(serverAllowed("http://10.0.0.5:8849", true)).toBe(false);
  });
});

describe("checkHandoff", () => {
  test("a handoff this tab started, to a hosted tenant, is accepted", () => {
    expect(checkHandoff(ok, false, stored("s1"))).toBeNull();
  });
  test("a link this tab did not start is refused", () => {
    expect(checkHandoff({ ...ok, state: "attacker" }, false, stored("s1"))).toBe("state");
    expect(checkHandoff(ok, false, new MemStorage() as unknown as Storage)).toBe("state");
    expect(checkHandoff({ ...ok, state: null }, false, stored("s1"))).toBe("state");
  });
  test("the stored state is single use, matched or not", () => {
    const s = stored("s1");
    expect(checkHandoff({ ...ok, state: "wrong" }, false, s)).toBe("state");
    expect(checkHandoff(ok, false, s)).toBe("state");
  });
  test("a server outside the allowed set is refused", () => {
    expect(checkHandoff({ ...ok, url: "https://evil.example" }, false, stored("s1"))).toBe("server");
  });
  test("a malformed code is refused", () => {
    expect(checkHandoff({ ...ok, code: "<script>" }, false, stored("s1"))).toBe("code");
    expect(checkHandoff({ ...ok, code: null }, false, stored("s1"))).toBe("code");
  });
});

describe("signInUrl", () => {
  test("carries return=web and the state", () => {
    const u = new URL(signInUrl("https://signup.passband.example/app/auth", "abc"));
    expect(u.searchParams.get("return")).toBe("web");
    expect(u.searchParams.get("state")).toBe("abc");
    expect(u.searchParams.get("provider")).toBeNull();
  });
});

describe("deviceName", () => {
  test("names browser and OS", () => {
    expect(
      deviceName(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36",
      ),
    ).toBe("Chrome on macOS (web)");
    expect(
      deviceName(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/19.0 Safari/605.1.15",
      ),
    ).toBe("Safari on macOS (web)");
    expect(deviceName("Mozilla/5.0 (Windows NT 10.0; rv:140.0) Gecko/20100101 Firefox/140.0")).toBe(
      "Firefox on Windows (web)",
    );
  });
});
