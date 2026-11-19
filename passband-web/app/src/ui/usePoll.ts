// Refresh now, then on a pace set by live updates (see src/app/live.ts): often
// while the event stream is down, rarely while it is up. Coming back to the
// tab refreshes too, unless the data is already fresh.
import { useEffect, useRef } from "react";

const CHECK_MS = 15_000;
const FRESH_MS = 10_000;

export function usePoll(refresh: () => Promise<void>, pollMs: () => number): void {
  const fns = useRef({ refresh, pollMs });
  fns.current = { refresh, pollMs };
  useEffect(() => {
    let last = 0;
    const run = () => {
      last = Date.now();
      void fns.current.refresh();
    };
    run();
    const due = () => document.visibilityState === "visible" && Date.now() - last >= fns.current.pollMs();
    const back = () => {
      if (document.visibilityState === "visible" && Date.now() - last >= FRESH_MS) run();
    };
    const timer = setInterval(() => due() && run(), CHECK_MS);
    document.addEventListener("visibilitychange", back);
    window.addEventListener("focus", back);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", back);
      window.removeEventListener("focus", back);
    };
  }, []);
}
