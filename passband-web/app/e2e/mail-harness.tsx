// E2E ONLY (built in `--mode e2e`, never in production): mounts the real
// MailFrame under the app's own CSP and Trusted Types, with fixture mail and a
// fake ImageSource that records every call.
//
//   ?mail=<id>  &images=1 (opted in from the start)  &optin=1 (a button opts in)
//   &thread=<n> (n copies)  &plain=1 (no html: the text path)
import { useState } from "react";
import { createRoot } from "react-dom/client";
import type { ClientMessage } from "../src/api/wire";
import { MailFrame, type ImageSource } from "../src/mail/MailFrame";
import { CID_ATTACHMENTS, FUNCTIONAL, HOSTILE, IMAGES, newsletter } from "./mail-corpus";

declare global {
  interface Window {
    __mail: {
      links: string[];
      remote: string[];
      attachments: number[];
      hasRemote: boolean[];
    };
    __pwned?: string;
  }
}
window.__mail = { links: [], remote: [], attachments: [], hasRemote: [] };

// A secret the mail must never reach.
try {
  localStorage.setItem("pb-token", "secret-device-token");
} catch {
  /* storage off */
}

const q = new URLSearchParams(location.search);
const id = q.get("mail") ?? "gmail-reply";
const thread = Math.max(1, Number(q.get("thread") ?? "1"));

function htmlFor(name: string): string {
  if (name === "newsletter") return newsletter();
  return (
    IMAGES[name] ?? FUNCTIONAL[name] ?? HOSTILE.find((m) => m.id === name)?.html ?? `<p>unknown ${name}</p>`
  );
}

function png(w: number, h: number): Promise<Blob> {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const g = c.getContext("2d")!;
  g.fillStyle = "#7ab";
  g.fillRect(0, 0, w, h);
  return new Promise((r) => c.toBlob((b) => r(b!), "image/png"));
}

const images: ImageSource = {
  async attachment(attachmentId) {
    window.__mail.attachments.push(attachmentId);
    if (attachmentId === 42) return new Blob(["<svg xmlns='http://www.w3.org/2000/svg'/>"], { type: "image/svg+xml" });
    return png(40, 40);
  },
  async remote(url, signal) {
    window.__mail.remote.push(url);
    if (url.includes("late.png")) {
      await new Promise((r) => setTimeout(r, 800));
      if (signal?.aborted) throw new DOMException("aborted", "AbortError");
      return png(600, 400);
    }
    return png(10, 10);
  },
};

function message(i: number): ClientMessage {
  const plain = q.get("plain") === "1";
  return {
    id: 1000 + i,
    from_addr: "ann@mail.example",
    from_name: "Ann Example",
    received_at: "2026-11-10T10:00:00Z",
    content: plain ? `<b>not bold</b> & plain text\nsecond line` : "text",
    html: plain ? undefined : htmlFor(id),
    attachments: CID_ATTACHMENTS,
  };
}

function App() {
  const [allow, setAllow] = useState(q.get("images") === "1");
  return (
    <div style={{ width: 720 }}>
      {q.get("optin") === "1" && (
        <button id="load-images" onClick={() => setAllow(true)}>
          Load images
        </button>
      )}
      {Array.from({ length: thread }, (_, i) => (
        <MailFrame
          key={i}
          message={message(i)}
          images={images}
          allowRemoteImages={allow}
          onHasRemoteImages={(h) => window.__mail.hasRemote.push(h)}
          onLink={(href) => window.__mail.links.push(href)}
        />
      ))}
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
