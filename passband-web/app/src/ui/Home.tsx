import { useEffect, useMemo, useState } from "react";
import type { AttentionUpdate } from "../api/wire";
import { useInbox, type Inbox } from "../app/inbox";
import { navigate } from "../app/router";
import { client } from "../app/services";
import { relAge } from "../lib/time";
import { DesktopBanner, nudgeLedger } from "./DesktopApp";
import { useKeys } from "./keys";

const POLL_MS = 30_000;

export function Home({ inbox, onSignedOut }: { inbox: Inbox; onSignedOut: () => void }) {
  const s = useInbox(inbox);
  const [cursor, setCursor] = useState(0);
  const rows = useMemo(() => [...s.needsYou, ...s.rest], [s.needsYou, s.rest]);

  // Refresh now, every 30s while visible, and on coming back to the tab.
  useEffect(() => {
    void inbox.refresh();
    const tick = () => {
      if (document.visibilityState === "visible") void inbox.refresh();
    };
    const timer = setInterval(tick, POLL_MS);
    document.addEventListener("visibilitychange", tick);
    window.addEventListener("focus", tick);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", tick);
      window.removeEventListener("focus", tick);
    };
  }, [inbox]);

  useEffect(() => {
    if (s.signedOut) onSignedOut();
  }, [s.signedOut, onSignedOut]);

  useEffect(() => {
    if (cursor >= rows.length && rows.length > 0) setCursor(rows.length - 1);
  }, [rows.length, cursor]);

  const current = rows[cursor];
  const open = (u: AttentionUpdate) => navigate({ name: "thread", threadId: u.thread_id });
  const done = (u: AttentionUpdate) => void inbox.done(u).then((ok) => ok && nudgeLedger.action());

  useKeys({
    j: () => setCursor((c) => Math.min(c + 1, rows.length - 1)),
    ArrowDown: () => setCursor((c) => Math.min(c + 1, rows.length - 1)),
    k: () => setCursor((c) => Math.max(c - 1, 0)),
    ArrowUp: () => setCursor((c) => Math.max(c - 1, 0)),
    Enter: () => current && open(current),
    e: () => current && done(current),
    d: () => current && done(current),
  });

  return (
    <div className="page">
      <header className="masthead">
        <span className="wordmark small">Passband</span>
        <span className="account">{s.account}</span>
        <button
          className="link"
          onClick={() => void client.signOut().then(onSignedOut)}
          title="Sign out of this browser"
        >
          Sign out
        </button>
      </header>
      <DesktopBanner ready={!s.loading} />
      {s.stale && <p className="notice">Can't reach your mailbox right now. Showing what was last loaded.</p>}
      {s.loading ? (
        <p className="empty" aria-busy="true">
          Loading your mail…
        </p>
      ) : (
        <>
          <Section
            title="Needs you"
            empty="Nothing needs you right now."
            rows={s.needsYou}
            offset={0}
            cursor={cursor}
            onOpen={open}
            onDone={done}
            onArchive={(u) => void inbox.archive(u).then((ok) => ok && nudgeLedger.action())}
            onHover={setCursor}
          />
          <Section
            title="Everything else"
            empty="No other recent mail."
            rows={s.rest}
            offset={s.needsYou.length}
            cursor={cursor}
            onOpen={open}
            onDone={done}
            onArchive={(u) => void inbox.archive(u).then((ok) => ok && nudgeLedger.action())}
            onHover={setCursor}
          />
        </>
      )}
      <footer className="keys-hint">
        <kbd>j</kbd>/<kbd>k</kbd> move · <kbd>↵</kbd> open · <kbd>e</kbd> done · <kbd>u</kbd> undo
      </footer>
    </div>
  );
}

function Section(props: {
  title: string;
  empty: string;
  rows: AttentionUpdate[];
  offset: number;
  cursor: number;
  onOpen: (u: AttentionUpdate) => void;
  onDone: (u: AttentionUpdate) => void;
  onArchive: (u: AttentionUpdate) => void;
  onHover: (i: number) => void;
}) {
  const now = Date.now();
  return (
    <section className="section">
      <h2>{props.title}</h2>
      {props.rows.length === 0 ? (
        <p className="empty">{props.empty}</p>
      ) : (
        <ul className="rows">
          {props.rows.map((u, i) => {
            const index = props.offset + i;
            return (
              <li
                key={u.id}
                className={`row tier-${u.tier}${index === props.cursor ? " selected" : ""}`}
                onMouseEnter={() => props.onHover(index)}
              >
                <button className="row-main" onClick={() => props.onOpen(u)}>
                  <span className="sender">{u.from_name ?? u.sender}</span>
                  <span className="line">{u.one_line}</span>
                  {(u.deadline ?? u.deadline_date) && (
                    <span className="due">{u.deadline ?? u.deadline_date}</span>
                  )}
                  <span className="age">{relAge(u.received_at, now)}</span>
                </button>
                <span className="row-actions">
                  <button className="link" onClick={() => props.onDone(u)} title="Done (e)">
                    Done
                  </button>
                  <button className="link" onClick={() => props.onArchive(u)} title="Archive">
                    Archive
                  </button>
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
