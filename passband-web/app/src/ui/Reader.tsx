import { useEffect, useMemo, useRef, useState } from "react";
import { ApiError } from "../api/transport";
import type { AttentionUpdate, ClientMessage, ThreadView } from "../api/wire";
import type { Inbox } from "../app/inbox";
import { navigate } from "../app/router";
import { client } from "../app/services";
import { UNDO_MS, toasts } from "../app/toasts";
import { relAge } from "../lib/time";
import { MailFrame, type ImageSource } from "../mail/MailFrame";
import { nudgeLedger } from "./DesktopApp";
import { ArchiveIcon, BackIcon, CheckIcon, ForwardIcon, ReplyIcon } from "./icons";
import { useKeys } from "./keys";
import { openLink } from "./links";

const images: ImageSource = {
  attachment: (id, signal) => client.attachmentBlob(id, signal),
  remote: (url, signal) => client.remoteImageBlob(url, signal),
};

/** The message the thread's verbs act on: the newest one someone else sent. */
export function actionTarget(thread: ThreadView): ClientMessage | undefined {
  const inbound = thread.messages.filter((m) => !m.is_sent);
  return (inbound.length ? inbound : thread.messages).reduce<ClientMessage | undefined>(
    (best, m) => (!best || m.id > best.id ? m : best),
    undefined,
  );
}

function asUpdate(inbox: Inbox, m: ClientMessage, threadId: string): AttentionUpdate {
  return (
    inbox.find(m.id) ?? {
      id: m.id,
      thread_id: threadId,
      tier: "signal",
      importance: 0,
      sender: m.from_addr,
      from_name: m.from_name,
      one_line: m.subject ?? "",
      reason: "",
      status: "open",
    }
  );
}

/** Flow mode, when this thread is a step of a walk (src/app/walk.ts). */
export interface ReaderWalk {
  /** 1-based. */
  position: number;
  total: number;
  done: number;
  skipped: number;
  step: (kind: "done" | "skip") => void;
  move: (by: 1 | -1) => void;
  end: () => void;
}

export function Reader({
  threadId,
  inbox,
  onSignedOut,
  walk,
}: {
  threadId: string;
  inbox: Inbox;
  onSignedOut: () => void;
  walk?: ReaderWalk;
}) {
  const [thread, setThread] = useState<ThreadView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [replying, setReplying] = useState(false);

  useEffect(() => {
    const ctl = new AbortController();
    setThread(null);
    setError(null);
    client
      .thread(threadId)
      .then((t) => {
        if (ctl.signal.aborted) return;
        setThread(t);
        void client.opened(threadId).catch(() => {});
      })
      .catch((e: unknown) => {
        if (ctl.signal.aborted) return;
        if (e instanceof ApiError && (e.kind === "unauthorized" || e.kind === "signed_out")) {
          onSignedOut();
        } else if (e instanceof ApiError && e.kind === "not_found") {
          setError("This conversation is no longer in your mailbox.");
        } else {
          setError("Couldn't load this conversation. Check your connection and try again.");
        }
      });
    return () => ctl.abort();
  }, [threadId, onSignedOut]);

  const target = useMemo(() => (thread ? actionTarget(thread) : undefined), [thread]);
  const back = () => navigate({ name: "home" });
  const act = async (verb: "done" | "archive") => {
    if (!target || !thread) return;
    const u = asUpdate(inbox, target, thread.thread_id);
    const ok = verb === "done" ? await inbox.done(u) : await inbox.archive(u);
    if (ok) {
      nudgeLedger.action();
      back();
    }
  };

  // In a walk, done moves on rather than going back to the board, and the
  // arrows step through the queue without deciding (the Mac's E, S, ←/→).
  useKeys(
    walk
      ? {
          Escape: () => (replying ? setReplying(false) : walk.end()),
          e: () => walk.step("done"),
          d: () => walk.step("done"),
          s: () => walk.step("skip"),
          ArrowRight: () => walk.move(1),
          l: () => walk.move(1),
          ArrowLeft: () => walk.move(-1),
          h: () => walk.move(-1),
          r: () => setReplying(true),
        }
      : {
          Escape: () => (replying ? setReplying(false) : back()),
          e: () => void act("done"),
          d: () => void act("done"),
          r: () => setReplying(true),
        },
  );

  return (
    <div className={`reader${walk ? " walking" : ""}`}>
      {walk && (
        <div className="flow-bar" aria-label="Flow mode">
          <span className="flow-label">Flow</span>
          <span className="flow-track" aria-hidden="true">
            <span
              className="flow-fill"
              style={{ width: `${((walk.position - 1) / Math.max(walk.total, 1)) * 100}%` }}
            />
          </span>
          <span className="flow-pos">
            {walk.position} of {walk.total}
          </span>
        </div>
      )}
      <div className="reader-bar">
        <button className="btn quiet" onClick={walk ? walk.end : back}>
          <BackIcon size={16} /> {walk ? "End flow" : "Back"} <kbd>esc</kbd>
        </button>
        <span className="grow" />
        {thread &&
          (walk ? (
            <>
              <button
                className="icon-btn"
                onClick={() => walk.move(-1)}
                aria-label="Previous"
                title="Previous (←)"
                disabled={walk.position <= 1}
              >
                <BackIcon size={16} />
              </button>
              <button
                className="icon-btn"
                onClick={() => walk.move(1)}
                aria-label="Next"
                title="Next (→)"
                disabled={walk.position >= walk.total}
              >
                <ForwardIcon size={16} />
              </button>
              <button className="btn" onClick={() => setReplying(true)}>
                <ReplyIcon size={15} /> Reply <kbd>r</kbd>
              </button>
              <button className="btn" onClick={() => walk.step("skip")}>
                Later <kbd>s</kbd>
              </button>
              <button className="btn primary" onClick={() => walk.step("done")}>
                <CheckIcon size={15} /> Done, next <kbd>e</kbd>
              </button>
            </>
          ) : (
            <>
              <button className="btn" onClick={() => void act("done")}>
                <CheckIcon size={15} /> Done <kbd>e</kbd>
              </button>
              <button className="btn" onClick={() => void act("archive")}>
                <ArchiveIcon size={15} /> Archive
              </button>
              <button className="btn primary" onClick={() => setReplying(true)}>
                <ReplyIcon size={15} /> Reply <kbd>r</kbd>
              </button>
            </>
          ))}
      </div>
      {error && <p className="notice glass">{error}</p>}
      {!thread && !error && (
        <p className="loading" aria-busy="true">
          Loading…
        </p>
      )}
      {thread && (
        <>
          <h1 className="subject" key={`s-${thread.thread_id}`}>
            {thread.subject || "(no subject)"}
          </h1>
          {replying && target && (
            <ReplyBox
              to={target}
              onClose={() => setReplying(false)}
              onSent={() => {
                nudgeLedger.action();
                setReplying(false);
              }}
            />
          )}
          <ol className="messages">
            {[...thread.messages].reverse().map((m) => (
              <MessageCard key={m.id} m={m} />
            ))}
          </ol>
        </>
      )}
    </div>
  );
}

function MessageCard({ m }: { m: ClientMessage }) {
  const [allowRemote, setAllowRemote] = useState(false);
  const [hasRemote, setHasRemote] = useState(false);
  return (
    <li className="message glass">
      <div className="from">
        <span className="name">{m.from_name ?? m.from_addr}</span>
        {m.from_name && <span className="addr">{m.from_addr}</span>}
        <span className="when">{relAge(m.received_at, Date.now())}</span>
      </div>
      {hasRemote && !allowRemote && (
        <button className="btn images" onClick={() => setAllowRemote(true)}>
          Load images
        </button>
      )}
      <MailFrame
        message={m}
        images={images}
        allowRemoteImages={allowRemote}
        onHasRemoteImages={setHasRemote}
        onLink={openLink}
      />
      {m.attachments.filter((a) => !a.content_id).length > 0 && (
        <ul className="attachments">
          {m.attachments
            .filter((a) => !a.content_id)
            .map((a) => (
              <li key={a.id}>
                <AttachmentLink id={a.id} name={a.filename} size={a.size} />
              </li>
            ))}
        </ul>
      )}
    </li>
  );
}

/**
 * Attachments are DOWNLOADED, never shown on this origin: the bytes become a
 * blob that the browser saves under the attachment's name, so nothing from a
 * sender (an HTML or SVG file, say) is ever rendered as part of the app.
 */
function AttachmentLink({ id, name, size }: { id: number; name: string; size: number }) {
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    try {
      const blob = await client.attachmentBlob(id);
      const url = URL.createObjectURL(new Blob([blob], { type: "application/octet-stream" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = name;
      a.rel = "noopener";
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 10_000);
    } catch {
      toasts.push("couldn't download that attachment", "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <button className="btn" onClick={() => void save()} disabled={busy}>
      {name} <span className="size">{formatSize(size)}</span>
    </button>
  );
}

function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** Plain reply. Sent after a five-second hold, as on the Mac; undo puts the text back. */
function ReplyBox({
  to,
  onClose,
  onSent,
}: {
  to: ClientMessage;
  onClose: () => void;
  onSent: () => void;
}) {
  const [text, setText] = useState("");
  const [held, setHeld] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  useEffect(() => area.current?.focus(), []);

  const send = () => {
    const body = text.trim();
    if (!body || held) return;
    setHeld(true);
    let cancelled = false;
    const timer = setTimeout(() => {
      if (cancelled) return;
      client
        .reply(to.id, body)
        .then(() => {
          toasts.push(`sent to ${to.from_name ?? to.from_addr}`);
          onSent();
        })
        .catch(() => {
          setHeld(false);
          toasts.push("couldn't send. Your reply is still here.", "error");
        });
    }, UNDO_MS);
    toasts.push("sending…", "info", async () => {
      cancelled = true;
      clearTimeout(timer);
      setHeld(false);
      area.current?.focus();
    });
  };

  return (
    <div className="reply glass">
      <label htmlFor="reply-body">
        Reply to {to.from_name ?? to.from_addr}
      </label>
      <textarea
        id="reply-body"
        ref={area}
        value={text}
        disabled={held}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            send();
          }
        }}
        rows={6}
      />
      <div className="reply-actions">
        <button className="btn primary" onClick={send} disabled={held || !text.trim()}>
          Send <kbd>⌘↵</kbd>
        </button>
        <button className="btn quiet" onClick={onClose} disabled={held}>
          Cancel
        </button>
      </div>
    </div>
  );
}
