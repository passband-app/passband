// Search, as on the Mac: results arrive as you type, the matched words lit, a
// sender offered above the hits, and the last few searches under an empty field.
// Every string drawn here came from somebody's mail, so highlights are React
// elements around plain text — never HTML.
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { client } from "../app/services";
import { relAge } from "../lib/time";
import { accepting, listing } from "../search/fromOperator";
import { highlight } from "../search/highlight";
import { adding, loadRing, saveRing } from "../search/ring";
import { SearchSession, type ShownHit } from "../search/session";
import { avatarSlot, displayName, eligibleFaviconDomain, initials, senderString } from "../sitrep/sender";
import { isTyping } from "./keys";
import { Logo } from "./Logo";
import "./search.css";

const storage = (() => {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
})();

function SearchGlyph() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" aria-hidden="true">
      <circle cx="10.5" cy="10.5" r="6.5" />
      <path d="M15.5 15.5L20 20" />
    </svg>
  );
}

function Lit({ text, matches }: { text: string; matches?: string[] }) {
  return (
    <>
      {highlight(text, matches).map((s, i) =>
        s.hit ? (
          <mark key={i} className="hl">
            {s.text}
          </mark>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </>
  );
}

type Row = { kind: "sender"; addr: string; name?: string; count: number } | { kind: "hit"; hit: ShownHit } | { kind: "recent"; text: string };

export function SearchPage({
  initialQuery,
  onOpen,
  onSignedOut,
}: {
  initialQuery?: string;
  onOpen: (threadId: string) => void;
  onSignedOut: () => void;
}) {
  const session = useMemo(() => new SearchSession(client), []);
  const s = useSyncExternalStore(session.subscribe, session.snapshot);
  const [text, setText] = useState(initialQuery ?? "");
  const [ring, setRing] = useState(() => loadRing(storage));
  const [cursor, setCursor] = useState(-1);
  const field = useRef<HTMLInputElement>(null);

  useEffect(() => {
    field.current?.focus();
    if (initialQuery) session.setQuery(initialQuery);
    return () => session.dispose();
  }, [session, initialQuery]);

  useEffect(() => {
    if (s.signedOut) onSignedOut();
  }, [s.signedOut, onSignedOut]);

  const change = (q: string) => {
    setText(q);
    setCursor(-1);
    session.setQuery(q);
  };

  const remember = (q: string) => {
    const next = adding(q, ring);
    setRing(next);
    saveRing(storage, next);
  };

  const empty = text.trim() === "";
  const rows: Row[] = empty
    ? ring.map((t) => ({ kind: "recent", text: t }))
    : [
        ...s.senders.map((x) => ({ kind: "sender" as const, addr: x.addr, name: x.display_name, count: x.msg_count })),
        ...s.hits.map((h) => ({ kind: "hit" as const, hit: h })),
      ];

  const activate = (row: Row | undefined) => {
    if (!row) return;
    if (row.kind === "recent") {
      change(row.text);
    } else if (row.kind === "sender") {
      // Inside a `from:` the pick completes the operator; from plain words it
      // lists that sender's mail.
      change(s.senderMenu ? accepting(row.addr, text) : listing(row.addr));
      field.current?.focus();
    } else {
      remember(text);
      onOpen(row.hit.thread_id);
    }
  };

  const onFieldKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setCursor((c) => Math.min(c + 1, rows.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((c) => Math.max(c - 1, -1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      // Enter with nothing highlighted opens the first hit.
      activate(cursor >= 0 ? rows[cursor] : rows.find((r) => r.kind === "hit"));
    } else if (e.key === "Escape") {
      e.preventDefault();
      if (text) change("");
      else field.current?.blur();
    }
  };

  // j/k and Enter when the field is not focused (never while typing).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || isTyping(e.target)) return;
      if (e.key === "j" || e.key === "ArrowDown") setCursor((c) => Math.min(c + 1, rows.length - 1));
      else if (e.key === "k" || e.key === "ArrowUp") setCursor((c) => Math.max(c - 1, 0));
      else if (e.key === "Enter" && cursor >= 0) activate(rows[cursor]);
      else if (e.key === "/") {
        e.preventDefault();
        field.current?.focus();
        return;
      } else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const answeredEmpty = !empty && s.status === "done" && s.answered === text && s.hits.length === 0;
  const now = Date.now();
  const firstHit = s.senders.length;

  return (
    <div className="search-page">
      <label className="search-field glass">
        <SearchGlyph />
        <input
          ref={field}
          type="search"
          value={text}
          placeholder="Search your mail"
          aria-label="Search your mail"
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => change(e.target.value)}
          onKeyDown={onFieldKey}
        />
        {s.status === "loading" && <span className="search-spin" aria-label="Searching" />}
      </label>

      {empty ? (
        ring.length > 0 && (
          <section className="search-recent" aria-label="Recent searches">
            <div className="list-section">
              Recent
              <button
                className="link"
                onClick={() => {
                  setRing([]);
                  saveRing(storage, []);
                }}
              >
                Clear
              </button>
            </div>
            <ul>
              {ring.map((t, i) => (
                <li key={t}>
                  <button className={`recent${i === cursor ? " selected" : ""}`} onClick={() => change(t)}>
                    {t}
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )
      ) : (
        <>
          {s.status === "waiting" && s.senders.length === 0 && (
            <p className="search-note">Type a name or an address.</p>
          )}
          {s.status === "error" && <p className="notice glass">Search isn't reachable right now. Try again in a moment.</p>}
          {(s.senders.length > 0 || s.hits.length > 0) && (
            <section className="list glass search-results" aria-label="Search results">
              {s.senders.length > 0 && (
                <>
                  <div className="list-section">{s.senderMenu ? "From" : "Senders"}</div>
                  <ul className="eyes">
                    {s.senders.map((x, i) => {
                      const who = senderString(x.addr, x.display_name);
                      return (
                        <li
                          key={x.addr}
                          className={`eye sender-row${i === cursor ? " selected" : ""}`}
                          onMouseEnter={() => setCursor(i)}
                          onClick={() => activate({ kind: "sender", addr: x.addr, name: x.display_name, count: x.msg_count })}
                        >
                          <Logo
                            className="avatar"
                            domain={eligibleFaviconDomain(who) ?? undefined}
                            initials={initials(who)}
                            hue={avatarSlot(who)}
                          />
                          <span className="who">{displayName(who)}</span>
                          <span className="line">{x.addr}</span>
                          <span className="end">
                            <span className="age">
                              {x.msg_count} {x.msg_count === 1 ? "email" : "emails"}
                            </span>
                          </span>
                        </li>
                      );
                    })}
                  </ul>
                </>
              )}
              {s.hits.length > 0 && (
                <>
                  {s.senders.length > 0 && <div className="list-section">Mail</div>}
                  <ul className="eyes">
                    {s.hits.map((h, i) => {
                      const who = senderString(h.from_addr, h.from_name);
                      const index = firstHit + i;
                      return (
                        <li
                          key={h.id}
                          className={`eye hit${index === cursor ? " selected" : ""}${h.is_done ? " done" : ""}`}
                          onMouseEnter={() => setCursor(index)}
                          onClick={() => activate({ kind: "hit", hit: h })}
                        >
                          <Logo
                            className="avatar"
                            domain={eligibleFaviconDomain(who) ?? undefined}
                            initials={initials(who)}
                            hue={avatarSlot(who)}
                          />
                          <span className="who">{h.from_me ? "Me" : displayName(who)}</span>
                          <span className="line">
                            <span className="subj">
                              <Lit text={h.subject || "(no subject)"} matches={h.subject_matches} />
                            </span>
                            {h.preview && (
                              <span className="snip">
                                {" "}
                                <Lit text={h.preview} matches={h.snippet_matches} />
                              </span>
                            )}
                          </span>
                          <span className="end">
                            {h.is_done && <span className="pill neutral">done</span>}
                            <span className="age">{relAge(h.received_at, now)}</span>
                          </span>
                        </li>
                      );
                    })}
                  </ul>
                  {s.nextCursor && s.answered === text && (
                    <button className="btn quiet more" onClick={() => void session.more()}>
                      More results
                    </button>
                  )}
                </>
              )}
            </section>
          )}
          {answeredEmpty && s.senders.length === 0 && (
            <p className="search-note">
              No mail matches “{text.trim()}”.
              {s.indexedSince && ` Search covers mail since ${s.indexedSince.slice(0, 10)}.`}
            </p>
          )}
        </>
      )}
      <footer className="keys-hint">
        <kbd>/</kbd> search · <kbd>↑</kbd>/<kbd>↓</kbd> move · <kbd>↵</kbd> open · <kbd>esc</kbd> clear
      </footer>
    </div>
  );
}
