import { useEffect, useState } from "react";
import {
  downloadLabel,
  loadLedger,
  markShown,
  platformOf,
  saveLedger,
  shouldShowBanner,
  type NudgeLedger,
} from "../app/nudge";
import { DESKTOP_DOWNLOAD_URL } from "../app/services";
import { DownloadIcon } from "./icons";

const storage = (() => {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
})();

function update(f: (l: NudgeLedger) => NudgeLedger): NudgeLedger {
  const next = f(loadLedger(storage));
  saveLedger(storage, next);
  return next;
}

/** Count an action (done, archive, reply). Visits are counted by the banner. */
export const nudgeLedger = {
  action: () => update((l) => ({ ...l, actions: l.actions + 1 })),
};

let countedVisit = false;

/** A click on any way to the app ends the banner for good. */
export const recordDownloadClick = () => {
  update((l) => ({ ...l, clickedDownload: true }));
};

const platform = platformOf(navigator.userAgent, navigator.maxTouchPoints);

function DownloadLink({ className, children }: { className?: string; children?: React.ReactNode }) {
  return (
    <a
      className={className}
      href={DESKTOP_DOWNLOAD_URL}
      target="_blank"
      rel="noopener noreferrer"
      onClick={recordDownloadClick}
    >
      {children ?? downloadLabel(platform)}
    </a>
  );
}

/** The case for the app, made at moments the web client has been used. */
export function DesktopBanner({ ready }: { ready: boolean }) {
  const [show, setShow] = useState(false);

  useEffect(() => {
    if (!ready) return;
    // The visit is counted HERE, right before the check, once per page load.
    // Counting it anywhere else races this effect (a child's effects run
    // before its parent's), and a first visit would read as visit zero.
    const l = countedVisit ? loadLedger(storage) : update((x) => ({ ...x, visits: x.visits + 1 }));
    countedVisit = true;
    const now = Date.now();
    if (shouldShowBanner(l, now)) {
      saveLedger(storage, markShown(l, now));
      setShow(true);
    }
  }, [ready]);

  if (!show) return null;
  return (
    <aside className="app-banner glass tint-accent" aria-label="Passband app">
      <span className="ico" aria-hidden="true">
        <DownloadIcon size={20} />
      </span>
      <div className="copy">
        <strong>Passband is better as an app.</strong>{" "}
        {platform === "ios"
          ? "Get notified only for the mail that needs you, with login codes right on your lock screen."
          : "Get notified only for the mail that needs you, see login codes float over any app, and walk your inbox in flow mode."}
      </div>
      <DownloadLink className="btn primary" />
      <button
        className="btn quiet"
        onClick={() => {
          update((l) => ({ ...l, dismissals: l.dismissals + 1 }));
          setShow(false);
        }}
      >
        Not now
      </button>
    </aside>
  );
}
