import { downloadLabel, platformOf } from "../app/nudge";
import { CONTROL_URL, DESKTOP_DOWNLOAD_URL } from "../app/services";
import { startSignIn } from "../session/handoff";
import { recordDownloadClick } from "./DesktopApp";

export function SignIn() {
  const platform = platformOf(navigator.userAgent, navigator.maxTouchPoints);
  return (
    <main className="signin">
      <div className="signin-card glass">
        <h1 className="wordmark">passband</h1>
        <p className="lede">Know what needs you.</p>
        <div className="actions">
          <button className="btn primary lg" onClick={() => startSignIn(CONTROL_URL)}>
            Continue with Google
          </button>
          <button className="btn lg" onClick={() => startSignIn(CONTROL_URL, "microsoft")}>
            Continue with Microsoft
          </button>
        </div>
        <p className="fine">
          Passband reads every message as it arrives, pulls forward the few that need you, and
          files the rest.{" "}
          <a
            href={DESKTOP_DOWNLOAD_URL}
            target="_blank"
            rel="noopener noreferrer"
            onClick={recordDownloadClick}
          >
            {downloadLabel(platform)}
          </a>
        </p>
      </div>
    </main>
  );
}
