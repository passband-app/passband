import { useEffect, useState } from "react";
import { navigate } from "../app/router";
import { useSitrep, type SitrepStore } from "../app/sitrep";
import { toasts } from "../app/toasts";
import { summaryLine, type WalkSummary } from "../app/walk";
import type { EyesItem } from "../sitrep/model";
import { DesktopBanner, nudgeLedger } from "./DesktopApp";
import { useKeys } from "./keys";
import { SitrepView } from "./SitrepView";

const POLL_MS = 30_000;

export function SitrepPage({
  store,
  onSignedOut,
  onFlow,
  finished,
  onDismissFinished,
}: {
  store: SitrepStore;
  onSignedOut: () => void;
  /** Space: walk For your eyes from the highlighted row (or the first). */
  onFlow: (eyes: EyesItem[], highlighted?: EyesItem) => void;
  /** The walk that just ended, for the board to celebrate. */
  finished: WalkSummary | null;
  onDismissFinished: () => void;
}) {
  const s = useSitrep(store);
  const [sel, setSel] = useState(-1);
  const eyes = s.sitrep.eyes;

  useEffect(() => {
    void store.refresh();
    const tick = () => {
      if (document.visibilityState === "visible") void store.refresh();
    };
    const timer = setInterval(tick, POLL_MS);
    document.addEventListener("visibilitychange", tick);
    window.addEventListener("focus", tick);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", tick);
      window.removeEventListener("focus", tick);
    };
  }, [store]);

  useEffect(() => {
    if (s.signedOut) onSignedOut();
  }, [s.signedOut, onSignedOut]);

  useEffect(() => {
    if (sel >= eyes.length) setSel(eyes.length - 1);
  }, [eyes.length, sel]);

  const open = (threadId: string) => navigate({ name: "thread", threadId });
  const current = sel >= 0 ? eyes[sel] : undefined;
  const handlers = {
    open,
    // The web reader never shows sealed mail: the Mac reveals it once, audited.
    openSealed: () =>
      toasts.push("Sign-in codes open in the Passband app, where they can be revealed safely."),
    done: (e: (typeof eyes)[number]) => void store.done(e).then((ok) => ok && nudgeLedger.action()),
    archive: (e: (typeof eyes)[number]) =>
      void store.archive(e).then((ok) => ok && nudgeLedger.action()),
  };

  useKeys({
    j: () => setSel((i) => Math.min(i + 1, eyes.length - 1)),
    ArrowDown: () => setSel((i) => Math.min(i + 1, eyes.length - 1)),
    k: () => setSel((i) => Math.max(i - 1, 0)),
    ArrowUp: () => setSel((i) => Math.max(i - 1, 0)),
    Enter: () => current && (current.sealed ? handlers.openSealed() : open(current.threadId)),
    e: () => current && handlers.done(current),
    d: () => current && handlers.done(current),
    " ": () => onFlow(eyes, current),
  });

  if (s.loading) {
    return (
      <p className="loading" aria-busy="true">
        Reading your mail…
      </p>
    );
  }
  return (
    <>
      {s.stale && (
        <p className="notice glass">Can't reach your mailbox right now. Showing what was last loaded.</p>
      )}
      <SitrepView
        sitrep={s.sitrep}
        selected={sel}
        onSelect={setSel}
        handlers={handlers}
        onFlow={() => onFlow(eyes, current)}
        banner={
          finished ? (
            <aside className="flow-done glass tint-positive" aria-label="Flow complete">
              <strong>Flow complete.</strong> {summaryLine(finished)}
              <button className="btn quiet" onClick={onDismissFinished}>
                Close
              </button>
            </aside>
          ) : (
            <DesktopBanner ready={!s.loading} />
          )
        }
      />
    </>
  );
}
