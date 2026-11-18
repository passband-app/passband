import type { ReactNode } from "react";
import { downloadLabel, platformOf } from "../app/nudge";
import { navigate, type Route } from "../app/router";
import { client, DESKTOP_DOWNLOAD_URL } from "../app/services";
import { recordDownloadClick } from "./DesktopApp";
import { DownloadIcon, GaugeIcon, MailIcon, SignOutIcon } from "./icons";

const platform = platformOf(navigator.userAgent, navigator.maxTouchPoints);

/** The window chrome: an icon rail on the left, a top bar over the page. */
export function Shell({
  route,
  where,
  account,
  onSignedOut,
  children,
}: {
  route: Route;
  where: string;
  account?: string;
  onSignedOut: () => void;
  children: ReactNode;
}) {
  const at = (name: Route["name"]) => route.name === name;
  return (
    <div className="shell">
      <nav className="rail" aria-label="Passband">
        <button
          className={`rail-btn${at("home") ? " active" : ""}`}
          aria-label="Sitrep"
          aria-current={at("home") ? "page" : undefined}
          title="Sitrep"
          onClick={() => navigate({ name: "home" })}
        >
          <GaugeIcon size={20} />
        </button>
        <button
          className={`rail-btn${at("mail") ? " active" : ""}`}
          aria-label="Mail"
          aria-current={at("mail") ? "page" : undefined}
          title="Mail"
          onClick={() => navigate({ name: "mail" })}
        >
          <MailIcon size={20} />
        </button>
        <span className="spacer" />
        <a
          className="rail-btn app"
          href={DESKTOP_DOWNLOAD_URL}
          target="_blank"
          rel="noopener noreferrer"
          title={downloadLabel(platform)}
          aria-label={downloadLabel(platform)}
          onClick={recordDownloadClick}
        >
          <DownloadIcon size={20} />
        </a>
        <span className="divider" />
        <button
          className="rail-btn"
          title="Sign out of this browser"
          aria-label="Sign out"
          onClick={() => void client.signOut().then(onSignedOut)}
        >
          <SignOutIcon size={20} />
        </button>
      </nav>
      <div className="main">
        <div className="inner">
        <header className="topbar">
          <span className="wordmark">
            passband<span className="where">{where}</span>
          </span>
          <span className="grow" />
          {account && <span className="who">{account}</span>}
          <a
            className="btn"
            href={DESKTOP_DOWNLOAD_URL}
            target="_blank"
            rel="noopener noreferrer"
            onClick={recordDownloadClick}
          >
            <DownloadIcon size={15} />
            {downloadLabel(platform)}
          </a>
        </header>
        {children}
        </div>
      </div>
    </div>
  );
}
