// One message body.
//
// HTML mail renders in an iframe whose sandbox token list is the security
// boundary: `allow-same-origin` so the PARENT can reach in (to measure, collapse
// quotes, catch clicks and fill in images), and no `allow-scripts`, so nothing
// in the mail can run, submit or navigate the page. `allow-popups` and
// `allow-popups-to-escape-sandbox` exist for Safari alone: WebKit fires no
// listener inside this frame, so the parent's click capture never sees a link,
// and the link has to open itself, in a new tab with no opener and no referrer
// (prepare.ts sets that on every link). With no script in the frame, a popup
// can only come from a reader's click on a link. Its document is
// built by `prepare()` and loaded only through `setFrameDocument()` (the app's
// one Trusted Types policy). Plain-text mail is shown as text, never parsed.
//
// The app page's CSP carries `frame-src 'self'`, and that is load-bearing: a
// sandbox without allow-top-navigation still lets a frame navigate ITSELF, and
// frame-src is one of the three things (with the click capture in frame.ts and
// the input gate below) that keep a link from loading inside the frame.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ClientMessage } from "../api/wire";
import { attachFrame, type FrameHandle } from "./frame";
import { FrameImages, cidIndex, type ImageSource } from "./images";
import { prepareMail } from "./prepare";
import { setFrameDocument } from "./trusted";

export type { ImageSource } from "./images";

export interface MailFrameProps {
  message: ClientMessage;
  images: ImageSource;
  /** The reader opted in to remote images for this message. */
  allowRemoteImages: boolean;
  /** The mail has remote images the reader could choose to load. */
  onHasRemoteImages?: (has: boolean) => void;
  /** A link the reader clicked (http/https only); the app opens it. */
  onLink: (url: string) => void;
}

/** Exactly this, and nothing more. A test pins it. */
export const SANDBOX = "allow-same-origin allow-popups allow-popups-to-escape-sandbox";

/** Heights already measured, so a reopened message paints at its final size. */
const rememberedHeights = new Map<number, number>();
const PLACEHOLDER_PX = 120;

export function MailFrame(props: MailFrameProps) {
  if (!props.message.html) return <pre className="mail-text">{props.message.content}</pre>;
  return <LazyFrame {...props} html={props.message.html} />;
}

/**
 * Long threads mount their frames as they come near the viewport. The space
 * is reserved first (the remembered height, or a small placeholder), so the
 * page does not jump as frames arrive.
 */
function LazyFrame(props: MailFrameProps & { html: string }) {
  const box = useRef<HTMLDivElement>(null);
  const [near, setNear] = useState(false);

  useEffect(() => {
    const el = box.current;
    if (!el || near) return;
    if (typeof IntersectionObserver === "undefined") {
      setNear(true);
      return;
    }
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setNear(true);
          io.disconnect();
        }
      },
      { rootMargin: "800px 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [near]);

  return (
    <div ref={box} className="mail" data-message={props.message.id}>
      {near ? (
        <HtmlFrame {...props} />
      ) : (
        <div style={{ height: rememberedHeights.get(props.message.id) ?? PLACEHOLDER_PX }} />
      )}
    </div>
  );
}

function HtmlFrame(props: MailFrameProps & { html: string }) {
  const { html, message } = props;
  const ref = useRef<HTMLIFrameElement>(null);
  const handle = useRef<FrameHandle | null>(null);
  const images = useRef<FrameImages | null>(null);
  const attachedTo = useRef<Document | null>(null);
  const [height, setHeight] = useState(rememberedHeights.get(message.id) ?? 0);
  const [hasQuoted, setHasQuoted] = useState(false);
  const [collapsed, setCollapsed] = useState(true);
  // The frame is invisible and takes no input until the parent's click capture
  // is on it: a click that landed earlier could load a link inside the frame.
  const [interactive, setInteractive] = useState(false);
  // Bumped to replace the iframe if it ever leaves its srcdoc document.
  const [generation, setGeneration] = useState(0);

  const prepared = useMemo(() => prepareMail(html), [html]);
  const cids = useMemo(() => cidIndex(message.attachments), [message.attachments]);

  // Latest props without re-attaching.
  const latest = useRef(props);
  latest.current = props;

  useEffect(() => {
    latest.current.onHasRemoteImages?.(prepared.remotes.length > 0);
  }, [prepared]);

  // Attach as soon as the srcdoc document is PARSED, not on the iframe's load
  // event: load waits for every image, so a frame sized on load would sit at
  // 1px until the slowest image arrived. No script can run inside to tell us,
  // so the parent checks once per animation frame.
  const attach = (): boolean => {
    const el = ref.current;
    const doc = el?.contentDocument;
    if (!el || !doc || doc.URL !== "about:srcdoc" || doc.readyState === "loading" || !doc.body) return false;
    if (attachedTo.current === doc) return true;
    handle.current?.detach();
    images.current?.dispose();
    attachedTo.current = doc;
    const h = attachFrame(el, {
      onHeight: (px) => {
        rememberedHeights.set(latest.current.message.id, px);
        setHeight(px);
      },
      onQuoted: setHasQuoted,
      onLink: (href) => latest.current.onLink(href),
    });
    handle.current = h;
    const imgs = new FrameImages(doc, latest.current.images, cids, prepared.remotes, () => {
      h?.watchImages();
      h?.measure();
    });
    images.current = imgs;
    void imgs.loadInline();
    if (latest.current.allowRemoteImages) void imgs.loadRemote();
    setCollapsed(true);
    setInteractive(true);
    return true;
  };

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    attachedTo.current = null;
    setInteractive(false);
    setFrameDocument(el, prepared.srcdoc);
    let raf = 0;
    const deadline = performance.now() + 10_000;
    const tick = () => {
      if (attach() || performance.now() > deadline) return;
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(raf);
      handle.current?.detach();
      handle.current = null;
      images.current?.dispose();
      images.current = null;
      attachedTo.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prepared, generation]);

  // Opting in after the frame is up fills the images in place, no reload.
  useEffect(() => {
    if (props.allowRemoteImages) void images.current?.loadRemote();
  }, [props.allowRemoteImages]);

  const onLoad = () => {
    const el = ref.current;
    // contentDocument is null once the frame has navigated cross-origin.
    const url = el?.contentDocument?.URL ?? "(cross-origin)";
    if (el && url !== "about:srcdoc" && url !== "about:blank") {
      // The frame left its document. Throw it away and build a fresh one.
      handle.current?.detach();
      handle.current = null;
      images.current?.dispose();
      images.current = null;
      attachedTo.current = null;
      setInteractive(false);
      setGeneration((g) => g + 1);
      return;
    }
    if (attach()) handle.current?.measure();
  };

  return (
    <>
      <iframe
        key={generation}
        ref={ref}
        title={`message from ${message.from_name ?? message.from_addr}`}
        sandbox={SANDBOX}
        referrerPolicy="no-referrer"
        onLoad={onLoad}
        data-height={height}
        data-csp={prepared.csp}
        data-trackers={prepared.blockedTrackers}
        data-remote={prepared.remotes.length}
        data-interactive={interactive ? "1" : "0"}
        style={{
          display: "block",
          width: "100%",
          // Until measured, hold the reserved space: a 1px frame would pull
          // every frame below it into range at once.
          height: height || PLACEHOLDER_PX,
          border: 0,
          borderRadius: 8,
          visibility: interactive ? "visible" : "hidden",
          pointerEvents: interactive ? "auto" : "none",
        }}
      />
      {hasQuoted && (
        <button
          className="link quoted-toggle"
          onClick={() => {
            const next = !collapsed;
            setCollapsed(next);
            handle.current?.setQuotesCollapsed(next);
          }}
        >
          {collapsed ? "show quoted text" : "hide quoted text"}
        </button>
      )}
    </>
  );
}
