import { describe, expect, test, vi } from "vitest";
import type { Client } from "../src/api/client";
import { ApiError } from "../src/api/transport";
import type { AttentionUpdate } from "../src/api/wire";
import { Inbox, composeLists } from "../src/app/inbox";
import { ToastStore } from "../src/app/toasts";

let nextId = 1;
const u = (over: Partial<AttentionUpdate> = {}): AttentionUpdate => ({
  id: nextId++,
  thread_id: `t${nextId}`,
  tier: "signal",
  importance: 3,
  sender: "a@example.com",
  one_line: "x",
  reason: "y",
  status: "new",
  received_at: "2026-11-10T10:00:00Z",
  ...over,
});

describe("composeLists", () => {
  test("needs-you is standing plus non-noise new, deduped, ranked overdue first", () => {
    const overdue = u({ tier: "past_due", importance: 1 });
    const due = u({ tier: "deadline", importance: 5 });
    const hi = u({ importance: 9 });
    const noise = u({ tier: "noise" });
    const { needsYou, rest } = composeLists([due, hi], [overdue, hi, noise], [hi, noise]);
    expect(needsYou.map((x) => x.id)).toEqual([overdue.id, due.id, hi.id]);
    expect(rest.map((x) => x.id)).toEqual([noise.id]);
  });
  test("done mail is in neither list", () => {
    const d = u({ status: "done" });
    const { needsYou, rest } = composeLists([d], [], [d]);
    expect(needsYou).toEqual([]);
    expect(rest).toEqual([]);
  });
  test("ties break by newest arrival", () => {
    const older = u({ received_at: "2026-11-01T00:00:00Z" });
    const newer = u({ received_at: "2026-11-09T00:00:00Z" });
    expect(composeLists([older, newer], [], []).needsYou.map((x) => x.id)).toEqual([
      newer.id,
      older.id,
    ]);
  });
});

function fakeClient(rows: AttentionUpdate[]) {
  const live = new Map(rows.map((r) => [r.id, r]));
  const c = {
    stats: vi.fn(async () => ({ account_email: "me@example.com" })),
    updates: vi.fn(async (p: { band?: string }) => ({
      items: p.band === "standing" ? [...live.values()].filter((r) => r.status !== "done") : [],
    })),
    setStatus: vi.fn(async (id: number, s: AttentionUpdate["status"]) => {
      const r = live.get(id)!;
      live.set(id, { ...r, status: s });
      return { status: "ok" };
    }),
    archive: vi.fn(async (id: number) => {
      live.delete(id);
      return { status: "ok" };
    }),
    label: vi.fn(async (id: number) => {
      const r = rows.find((x) => x.id === id)!;
      live.set(id, r);
    }),
  };
  return c;
}

describe("verbs", () => {
  test("done removes at once and its undo reopens the mail", async () => {
    const a = u();
    const c = fakeClient([a]);
    const ts = new ToastStore();
    const inbox = new Inbox(c as unknown as Client, ts);
    await inbox.refresh();
    expect(inbox.snapshot().needsYou).toHaveLength(1);

    const p = inbox.done(a);
    expect(inbox.snapshot().needsYou).toHaveLength(0);
    expect(await p).toBe(true);
    expect(c.setStatus).toHaveBeenCalledWith(a.id, "done");

    expect(await ts.undoLatest()).toBe(true);
    expect(c.setStatus).toHaveBeenLastCalledWith(a.id, "open");
    expect(inbox.snapshot().needsYou.map((x) => x.id)).toEqual([a.id]);
  });

  test("archive's undo puts the INBOX label back", async () => {
    const a = u();
    const c = fakeClient([a]);
    const ts = new ToastStore();
    const inbox = new Inbox(c as unknown as Client, ts);
    await inbox.refresh();
    await inbox.archive(a);
    await ts.undoLatest();
    expect(c.label).toHaveBeenCalledWith(a.id, ["INBOX"]);
  });

  test("a failed call puts the row back and says so", async () => {
    const a = u();
    const c = fakeClient([a]);
    c.setStatus.mockRejectedValueOnce(new ApiError("network"));
    const ts = new ToastStore();
    const inbox = new Inbox(c as unknown as Client, ts);
    await inbox.refresh();
    expect(await inbox.done(a)).toBe(false);
    expect(inbox.snapshot().needsYou.map((x) => x.id)).toEqual([a.id]);
    expect(ts.snapshot().at(-1)).toMatchObject({ tone: "error", text: "done failed" });
  });

  test("a 401 on refresh means signed out, not stale", async () => {
    const c = fakeClient([]);
    c.stats.mockRejectedValueOnce(new ApiError("unauthorized", 401));
    const inbox = new Inbox(c as unknown as Client, new ToastStore());
    await inbox.refresh();
    expect(inbox.snapshot()).toMatchObject({ signedOut: true, stale: false });
  });

  test("concurrent refreshes share one load", async () => {
    const c = fakeClient([u()]);
    const inbox = new Inbox(c as unknown as Client, new ToastStore());
    await Promise.all([inbox.refresh(), inbox.refresh(), inbox.refresh()]);
    expect(c.stats).toHaveBeenCalledTimes(1);
  });
});
