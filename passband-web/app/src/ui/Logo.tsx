// A sender's logo, or their initials. The logo comes through the daemon
// (`/client/favicon`), becomes a blob: URL on this origin, and is remembered
// per domain for the page's life, so a board with six tiles from one brand asks
// once. Any miss or failure is just the initials: a logo is decoration.
import { useEffect, useState, type ReactNode } from "react";
import { client } from "../app/services";

const known = new Map<string, string | null>();
const pending = new Map<string, Promise<string | null>>();
/** At most this many icon fetches at once across the page. */
const MAX_INFLIGHT = 4;
let inflight = 0;
const queue: (() => void)[] = [];

function slot(): Promise<() => void> {
  return new Promise((resolve) => {
    const go = () => {
      inflight++;
      resolve(() => {
        inflight--;
        queue.shift()?.();
      });
    };
    if (inflight < MAX_INFLIGHT) go();
    else queue.push(go);
  });
}

function load(domain: string): Promise<string | null> {
  const hit = known.get(domain);
  if (hit !== undefined) return Promise.resolve(hit);
  let p = pending.get(domain);
  if (!p) {
    p = (async () => {
      const release = await slot();
      try {
        const blob = await client.favicon(domain);
        // Only images become logos, whatever the response claimed to be.
        const url = blob.type.startsWith("image/") ? URL.createObjectURL(blob) : null;
        known.set(domain, url);
        return url;
      } catch {
        known.set(domain, null);
        return null;
      } finally {
        pending.delete(domain);
        release();
      }
    })();
    pending.set(domain, p);
  }
  return p;
}

export function Logo({
  domain,
  initials,
  hue,
  className,
  fallback,
}: {
  domain?: string;
  initials: string;
  hue: number;
  className: string;
  /** Drawn instead of initials when there is no logo. */
  fallback?: ReactNode;
}) {
  const [url, setUrl] = useState<string | null>(domain ? (known.get(domain) ?? null) : null);
  useEffect(() => {
    if (!domain) return;
    let live = true;
    void load(domain).then((u) => live && setUrl(u));
    return () => {
      live = false;
    };
  }, [domain]);

  if (url) {
    return (
      <span className={`${className} has-logo`} aria-hidden="true">
        <img src={url} alt="" onError={() => setUrl(null)} />
      </span>
    );
  }
  if (fallback !== undefined) {
    return (
      <span className={className} aria-hidden="true">
        {fallback}
      </span>
    );
  }
  return (
    <span className={`${className} avatar h${hue}`} aria-hidden="true">
      {initials}
    </span>
  );
}
