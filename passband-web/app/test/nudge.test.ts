import { describe, expect, test } from "vitest";
import {
  EMPTY_LEDGER,
  loadLedger,
  markShown,
  platformOf,
  shouldShowBanner,
  type NudgeLedger,
} from "../src/app/nudge";

const DAY = 24 * 60 * 60 * 1000;
const T0 = Date.UTC(2026, 10, 1);
const l = (over: Partial<NudgeLedger>): NudgeLedger => ({ ...EMPTY_LEDGER, ...over });

describe("shouldShowBanner", () => {
  test("shown on the first visit, once", () => {
    expect(shouldShowBanner(l({ visits: 0 }), T0)).toBe(false);
    const first = l({ visits: 1 });
    expect(shouldShowBanner(first, T0)).toBe(true);
    const after = markShown(first, T0);
    expect(shouldShowBanner(after, T0 + 2 * DAY)).toBe(false);
  });
  test("at most once a day, even at a milestone", () => {
    const s = markShown(l({ visits: 1 }), T0);
    expect(shouldShowBanner({ ...s, visits: 3 }, T0 + DAY - 1)).toBe(false);
    expect(shouldShowBanner({ ...s, visits: 3 }, T0 + DAY)).toBe(true);
  });
  test("the third visit is a milestone, counted once", () => {
    const s = markShown(l({ visits: 3 }), T0);
    expect(shouldShowBanner({ ...s, visits: 4 }, T0 + 5 * DAY)).toBe(false);
  });
  test("every tenth action is a milestone", () => {
    const s = markShown(l({ visits: 3, actions: 4 }), T0);
    expect(shouldShowBanner({ ...s, actions: 9 }, T0 + 2 * DAY)).toBe(false);
    expect(shouldShowBanner({ ...s, actions: 10 }, T0 + 2 * DAY)).toBe(true);
    const s2 = markShown({ ...s, actions: 10 }, T0 + 2 * DAY);
    expect(shouldShowBanner({ ...s2, actions: 19 }, T0 + 4 * DAY)).toBe(false);
    expect(shouldShowBanner({ ...s2, actions: 20 }, T0 + 4 * DAY)).toBe(true);
  });
  test("stops for good after a download click or three dismissals", () => {
    expect(shouldShowBanner(l({ visits: 1, clickedDownload: true }), T0)).toBe(false);
    expect(shouldShowBanner(l({ visits: 1, dismissals: 3 }), T0)).toBe(false);
    expect(shouldShowBanner(l({ visits: 1, dismissals: 2 }), T0)).toBe(true);
  });
});

describe("platformOf", () => {
  test("Mac, iPhone, iPad-as-Mac, other", () => {
    expect(platformOf("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")).toBe("mac");
    expect(platformOf("Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X)")).toBe("ios");
    expect(platformOf("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5)).toBe("ios");
    expect(platformOf("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")).toBe("other");
  });
});

describe("loadLedger", () => {
  test("garbage and wrong types fall back to defaults", () => {
    expect(loadLedger({ getItem: () => "{not json" })).toEqual(EMPTY_LEDGER);
    expect(loadLedger({ getItem: () => JSON.stringify({ visits: "lots", actions: 4, evil: 1 }) })).toEqual({
      ...EMPTY_LEDGER,
      actions: 4,
    });
    expect(
      loadLedger({
        getItem: () => {
          throw new Error("blocked");
        },
      }),
    ).toEqual(EMPTY_LEDGER);
  });
});
