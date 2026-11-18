import { useEffect, useRef, useState } from "react";
import { navigate } from "../app/router";
import { transport } from "../app/services";
import { checkHandoff, deviceName, takeHandoff } from "../session/handoff";

const REFUSAL_TEXT = {
  state: "This sign-in link wasn't started here. Start again from the sign-in page.",
  server: "This sign-in pointed somewhere Passband doesn't recognise, so it was stopped.",
  code: "This sign-in link is incomplete. Start again from the sign-in page.",
} as const;

export function Connect({ onPaired }: { onPaired: () => void }) {
  const [message, setMessage] = useState("Signing you in…");
  const [failed, setFailed] = useState(false);
  // StrictMode runs effects twice in development; the handoff must run once
  // (the fragment is erased and the stored state is consumed on first read).
  const ran = useRef(false);

  useEffect(() => {
    if (ran.current) return;
    ran.current = true;
    const h = takeHandoff();
    const refusal = checkHandoff(h, __ALLOW_LOOPBACK__);
    if (refusal) {
      setFailed(true);
      setMessage(REFUSAL_TEXT[refusal]);
      return;
    }
    transport
      .pair(h.url!, h.code!, deviceName())
      .then(onPaired)
      .catch(() => {
        setFailed(true);
        setMessage("That sign-in code didn't work. It may have expired; start again.");
      });
  }, [onPaired]);

  return (
    <main className="signin">
      <div className="signin-card glass">
        <h1 className="wordmark">passband</h1>
        <p className="lede" role="status">
          {message}
        </p>
        {failed && (
          <button className="btn primary lg" onClick={() => navigate({ name: "home" }, true)}>
            Back to sign in
          </button>
        )}
      </div>
    </main>
  );
}
