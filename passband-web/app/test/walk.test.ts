import { describe, expect, test } from "vitest";
import {
  WalkStore,
  current,
  move,
  rewind,
  startWalk,
  step,
  summarize,
  summaryLine,
  walkQueue,
} from "../src/app/walk";
import type { EyesItem } from "../src/sitrep/model";

const item = (id: number, over: Partial<EyesItem> = {}): EyesItem => ({
  id,
  threadId: `t${id}`,
  sender: `S${id}`,
  initials: "S",
  avatarHue: 0,
  line: "x",
  hasAttachment: false,
  overdue: false,
  ...over,
});
const eyes = [item(1), item(2, { sealed: true }), item(3), item(4)];

describe("the queue", () => {
  test("sealed mail is never on the walk", () => {
    expect(walkQueue(eyes).map((e) => e.id)).toEqual([1, 3, 4]);
  });
  test("starts at the highlighted row, else the first", () => {
    expect(current(startWalk(eyes, item(3), 0)!)?.id).toBe(3);
    expect(current(startWalk(eyes, undefined, 0)!)?.id).toBe(1);
    // A highlighted sealed row is not on the walk: start from the top.
    expect(current(startWalk(eyes, item(2), 0)!)?.id).toBe(1);
  });
  test("an empty board has no walk", () => {
    expect(startWalk([item(9, { sealed: true })], undefined, 0)).toBeNull();
  });
});

describe("stepping", () => {
  test("E and S count and move on; the last one finishes", () => {
    let w = startWalk(eyes, undefined, 0)!;
    let r = step(w, "done");
    expect([r.finished, current(r.walk)?.id, r.walk.done]).toEqual([false, 3, 1]);
    r = step(r.walk, "skip");
    expect([r.finished, current(r.walk)?.id, r.walk.skipped]).toEqual([false, 4, 1]);
    r = step(r.walk, "done");
    expect(r.finished).toBe(true);
    expect(summarize(r.walk, 125_000)).toEqual({ done: 2, skipped: 1, durationMs: 125_000 });
  });
  test("arrows move without deciding, and stop at the ends", () => {
    const w = startWalk(eyes, undefined, 0)!;
    expect(move(w, -1)).toBe(w);
    const fwd = move(move(move(w, 1), 1), 1);
    expect(current(fwd)?.id).toBe(4);
    expect(fwd.done + fwd.skipped).toBe(0);
  });
});

describe("undo", () => {
  test("rewinds onto the email just behind and takes the step off the tally", () => {
    const w = step(startWalk(eyes, undefined, 0)!, "done").walk;
    const back = rewind(w, "done", 1);
    expect([current(back)?.id, back.done]).toEqual([1, 0]);
  });
  test("elsewhere it only corrects the tally", () => {
    const w = move(step(startWalk(eyes, undefined, 0)!, "done").walk, 1);
    const back = rewind(w, "done", 1);
    expect([current(back)?.id, back.done]).toEqual([4, 0]);
  });
});

describe("summary", () => {
  test("nothing decided, nothing to celebrate", () => {
    expect(summarize(startWalk(eyes, undefined, 0)!, 10)).toBeNull();
  });
  test("wording", () => {
    expect(summaryLine({ done: 4, skipped: 1, durationMs: 130_000 })).toBe(
      "4 done, 1 saved for later, in 2 minutes.",
    );
    expect(summaryLine({ done: 1, skipped: 0, durationMs: 20_000 })).toBe("1 done, in under a minute.");
  });
});

describe("WalkStore", () => {
  test("Esc ends early and keeps the tally", () => {
    let t = 0;
    const s = new WalkStore(() => t);
    s.start(eyes);
    s.step("done");
    t = 60_000;
    s.end();
    expect(s.snapshot()).toEqual({ walk: null, finished: { done: 1, skipped: 0, durationMs: 60_000 } });
    s.dismissSummary();
    expect(s.snapshot().finished).toBeNull();
  });
});
