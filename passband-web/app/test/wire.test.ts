// Decoding against real daemon answers (synthetic mail, captured from the
// spike daemon), plus the leniency rules from Model/WireTypes.swift.
import { describe, expect, test } from "vitest";
import statsFixture from "./fixtures/stats.json";
import threadFixture from "./fixtures/thread_t-reply.json";
import updatesFixture from "./fixtures/updates_limit_3.json";
import {
  attentionUpdate,
  page,
  pairResult,
  stats,
  status,
  threadView,
  tier,
} from "../src/api/wire";

describe("real daemon answers decode", () => {
  test("/client/updates", () => {
    const p = page(updatesFixture, attentionUpdate);
    expect(p?.items).toHaveLength(3);
    expect(p?.next_cursor).toBe("b2ZmOjM");
    expect(p?.items[0]).toMatchObject({ id: 26, tier: "noise", status: "new", sender: "alice@example.com" });
  });
  test("/client/thread", () => {
    const t = threadView(threadFixture);
    expect(t?.subject).toBe("Re: Plans");
    expect(t?.messages[0]?.html).toContain("gmail_quote");
    expect(t?.messages[0]?.attachments).toEqual([]);
  });
  test("/client/stats", () => {
    expect(stats(statsFixture)).toMatchObject({
      account_email: "me@example.com",
      bands: { standing: 0, new: 0, open: 3 },
    });
  });
});

describe("leniency", () => {
  const row = {
    id: 1,
    thread_id: "t",
    tier: "deadline",
    importance: 3,
    sender: "a@example.com",
    one_line: "x",
    reason: "y",
    status: "open",
  };
  test("an unknown enum value falls back instead of failing the row", () => {
    expect(tier("brand_new_tier")).toBe("signal");
    expect(status("snoozed")).toBe("new");
    expect(attentionUpdate({ ...row, tier: "brand_new_tier" })?.tier).toBe("signal");
  });
  test("fields a newer daemon adds are ignored", () => {
    expect(attentionUpdate({ ...row, sparkly_new_field: [1, 2] })).not.toBeNull();
  });
  test("optional fields an older daemon lacks are just absent", () => {
    const d = attentionUpdate(row);
    expect(d?.received_at).toBeUndefined();
    expect(d?.preview).toBeUndefined();
  });
  test("one malformed row costs only itself", () => {
    const p = page({ items: [row, { ...row, id: "nope" }, { ...row, id: 3 }, null] }, attentionUpdate);
    expect(p?.items.map((r) => r.id)).toEqual([1, 3]);
  });
  test("a page without items is not a page", () => {
    expect(page({}, attentionUpdate)).toBeNull();
    expect(page(null, attentionUpdate)).toBeNull();
  });
  test("a mistyped optional field is dropped, not trusted", () => {
    expect(attentionUpdate({ ...row, preview: 42 })?.preview).toBeUndefined();
  });
});

describe("pairResult", () => {
  test("needs a non-empty token", () => {
    expect(pairResult({ token: "sqd_x", device_id: 4, name: "Web" })).toEqual({
      token: "sqd_x",
      device_id: 4,
      name: "Web",
    });
    expect(pairResult({ token: "", device_id: 4, name: "Web" })).toBeNull();
    expect(pairResult({ device_id: 4, name: "Web" })).toBeNull();
  });
});
