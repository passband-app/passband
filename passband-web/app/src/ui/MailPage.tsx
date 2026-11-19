import { useEffect, useMemo, useState } from "react";
import type { AttentionUpdate } from "../api/wire";
import { useInbox, type Inbox } from "../app/inbox";
import { navigate } from "../app/router";
import { relAge } from "../lib/time";
import { avatarSlot, displayName, initials, senderString } from "../sitrep/sender";
import { nudgeLedger } from "./DesktopApp";
import { ArchiveIcon, CheckIcon, PaperclipIcon } from "./icons";
import { useKeys } from "./keys";
import { usePoll } from "./usePoll";


/** Everything recent, needs-you first: the full list behind the sitrep. */
export function MailPage({
  inbox,
  onSignedOut,
  pollMs,
}: {
  inbox: Inbox;
  onSignedOut: () => void;
  pollMs: () => number;
}) {
  const s = useInbox(inbox);
  const [cursor, setCursor] = useState(0);
  const rows = useMemo(() => [...s.needsYou, ...s.rest], [s.needsYou, s.rest]);

  usePoll(() => inbox.refresh(), pollMs);

  useEffect(() => {
    if (s.signedOut) onSignedOut();
  }, [s.signedOut, onSignedOut]);

  useEffect(() => {
    if (cursor >= rows.length && rows.length > 0) setCursor(rows.length - 1);
  }, [rows.length, cursor]);

  const current = rows[cursor];
  const open = (u: AttentionUpdate) => navigate({ name: "thread", threadId: u.thread_id });
  const done = (u: AttentionUpdate) => void inbox.done(u).then((ok) => ok && nudgeLedger.action());
  const archive = (u: AttentionUpdate) => void inbox.archive(u).then((ok) => ok && nudgeLedger.action());

  useKeys({
    j: () => setCursor((c) => Math.min(c + 1, rows.length - 1)),
    ArrowDown: () => setCursor((c) => Math.min(c + 1, rows.length - 1)),
    k: () => setCursor((c) => Math.max(c - 1, 0)),
    ArrowUp: () => setCursor((c) => Math.max(c - 1, 0)),
    Enter: () => current && open(current),
    e: () => current && done(current),
    d: () => current && done(current),
  });

  const row = (u: AttentionUpdate, i: number) => (
    <MailRow
      key={u.id}
      u={u}
      selected={i === cursor}
      onHover={() => setCursor(i)}
      onOpen={() => open(u)}
      onDone={() => done(u)}
      onArchive={() => archive(u)}
    />
  );

  return (
    <div className="mail-page">
      <h1 className="page-title">Mail</h1>
      {s.stale && <p className="notice glass">Can't reach your mailbox right now. Showing what was last loaded.</p>}
      {s.loading ? (
        <p className="loading" aria-busy="true">
          Loading your mail…
        </p>
      ) : (
        <section className="list glass" aria-label="Mail">
          {s.needsYou.length > 0 && <div className="list-section">Needs you</div>}
          <ul className="eyes">{s.needsYou.map((u, i) => row(u, i))}</ul>
          <div className="list-section">Everything else</div>
          {s.rest.length === 0 ? (
            <p className="zone-empty" style={{ padding: "0 10px 8px" }}>
              No other recent mail.
            </p>
          ) : (
            <ul className="eyes">{s.rest.map((u, i) => row(u, s.needsYou.length + i))}</ul>
          )}
        </section>
      )}
    </div>
  );
}

function MailRow({
  u,
  selected,
  onHover,
  onOpen,
  onDone,
  onArchive,
}: {
  u: AttentionUpdate;
  selected: boolean;
  onHover: () => void;
  onOpen: () => void;
  onDone: () => void;
  onArchive: () => void;
}) {
  const who = senderString(u.sender, u.from_name);
  return (
    <li
      className={`eye${selected ? " selected" : ""}${u.tier === "past_due" ? " overdue" : ""}`}
      onMouseEnter={onHover}
      onClick={onOpen}
    >
      <span className={`avatar h${avatarSlot(who)}`} aria-hidden="true">
        {initials(who)}
      </span>
      <span className="who">
        {displayName(who)}
        {u.has_attachments && <PaperclipIcon size={13} label="has attachments" />}
      </span>
      <span className="line">{u.one_line || u.subject}</span>
      <span className="end">
        <span className="age">{relAge(u.received_at, Date.now())}</span>
        <span className="verbs">
          <button
            className="icon-btn"
            title="Done (e)"
            aria-label="Done"
            onClick={(ev) => {
              ev.stopPropagation();
              onDone();
            }}
          >
            <CheckIcon size={16} />
          </button>
          <button
            className="icon-btn"
            title="Archive"
            aria-label="Archive"
            onClick={(ev) => {
              ev.stopPropagation();
              onArchive();
            }}
          >
            <ArchiveIcon size={16} />
          </button>
        </span>
      </span>
    </li>
  );
}
