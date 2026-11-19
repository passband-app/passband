// Getting people from the web client into the native app.
//
// The web client is the hook; the app is the product. So the way to the app is
// always one click away in the header, and a banner makes the case at moments
// someone has actually used the thing. The banner is never a nag: at most once
// a day, it comes back only at real milestones, and it stops for good after a
// download click or three dismissals.

export type Platform = "mac" | "ios" | "other";

export function platformOf(ua: string, maxTouchPoints = 0): Platform {
  if (/iPhone|iPod/.test(ua)) return "ios";
  // iPadOS reports itself as a Mac; touch points give it away.
  if (/iPad/.test(ua) || (/Macintosh/.test(ua) && maxTouchPoints > 1)) return "ios";
  if (/Mac OS X|Macintosh/.test(ua)) return "mac";
  return "other";
}

export function downloadLabel(p: Platform): string {
  switch (p) {
    case "mac":
      return "Get Passband for Mac";
    case "ios":
      return "Get Passband for iPhone";
    case "other":
      return "Get the Passband app";
  }
}

export interface NudgeLedger {
  visits: number;
  actions: number;
  dismissals: number;
  /** Epoch ms of the last time the banner was shown. */
  lastShownAt: number;
  /** Visits/actions at the last showing, so a milestone is counted once. */
  shownAtVisits: number;
  shownAtActions: number;
  clickedDownload: boolean;
}

export const EMPTY_LEDGER: NudgeLedger = {
  visits: 0,
  actions: 0,
  dismissals: 0,
  lastShownAt: 0,
  shownAtVisits: 0,
  shownAtActions: 0,
  clickedDownload: false,
};

const DAY_MS = 24 * 60 * 60 * 1000;
export const MAX_DISMISSALS = 3;

/** Show the banner now? Pure: the caller owns the clock and the ledger. */
export function shouldShowBanner(l: NudgeLedger, now: number): boolean {
  if (l.clickedDownload || l.dismissals >= MAX_DISMISSALS) return false;
  if (l.lastShownAt && now - l.lastShownAt < DAY_MS) return false;
  // First visit: once, as soon as there is an inbox to look at.
  if (l.lastShownAt === 0) return l.visits >= 1;
  // After that, only at a milestone not yet shown for.
  const visitMilestone = l.visits >= 3 && l.shownAtVisits < 3;
  const actionMilestone = Math.floor(l.actions / 10) > Math.floor(l.shownAtActions / 10);
  return visitMilestone || actionMilestone;
}

export function markShown(l: NudgeLedger, now: number): NudgeLedger {
  return { ...l, lastShownAt: now, shownAtVisits: l.visits, shownAtActions: l.actions };
}

const KEY = "pb_nudge_v1";

/** Per-browser convenience state: losing it only means seeing the banner again. */
export function loadLedger(storage: Pick<Storage, "getItem"> | undefined): NudgeLedger {
  try {
    const raw = storage?.getItem(KEY);
    if (!raw) return { ...EMPTY_LEDGER };
    const v = JSON.parse(raw) as Partial<NudgeLedger>;
    return {
      ...EMPTY_LEDGER,
      ...Object.fromEntries(
        Object.entries(v).filter(([k, x]) => k in EMPTY_LEDGER && typeof x === typeof EMPTY_LEDGER[k as keyof NudgeLedger]),
      ),
    };
  } catch {
    return { ...EMPTY_LEDGER };
  }
}

export function saveLedger(storage: Pick<Storage, "setItem"> | undefined, l: NudgeLedger): void {
  try {
    storage?.setItem(KEY, JSON.stringify(l));
  } catch {
    // Storage blocked: the banner just shows by its defaults.
  }
}
