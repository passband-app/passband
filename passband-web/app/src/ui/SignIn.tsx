import { CONTROL_URL, DESKTOP_DOWNLOAD_URL } from "../app/services";
import { downloadLabel, platformOf } from "../app/nudge";
import { startSignIn } from "../session/handoff";

export function SignIn() {
  const platform = platformOf(navigator.userAgent, navigator.maxTouchPoints);
  return (
    <main className="signin">
      <h1 className="wordmark">Passband</h1>
      <p className="lede">Know what needs you.</p>
      <p className="sub">
        Passband reads every message as it arrives, pulls forward the few that need you, and files
        the rest.
      </p>
      <div className="signin-actions">
        <button className="primary" onClick={() => startSignIn(CONTROL_URL)}>
          Continue with Google
        </button>
        <button onClick={() => startSignIn(CONTROL_URL, "microsoft")}>Continue with Microsoft</button>
      </div>
      <p className="signin-app">
        Prefer the app?{" "}
        <a href={DESKTOP_DOWNLOAD_URL} rel="noopener noreferrer">
          {downloadLabel(platform)}
        </a>
      </p>
    </main>
  );
}
