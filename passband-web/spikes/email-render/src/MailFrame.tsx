import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { prepareMail, wrapRaw } from './prepare';
import { attachFrame, type FrameHandle } from './frame';

export interface MailFrameProps {
  html: string;
  allowRemote: boolean;
  proxyBase: string;
  /** Off only in tests: proves the frame's own layers hold alone. */
  clientPass?: boolean;
  onLink(href: string): void;
  onHeight?(px: number): void;
  onReady?(ms: number): void;
  /** Called if the frame ever leaves its srcdoc document (it is then reset). */
  onEscaped?(url: string): void;
  /** TEST ONLY: skip the parent's click capture, to exercise the escape guard. */
  testNoLinkCapture?: boolean;
  /** Time spent in the client pass (prepareMail), for the perf numbers. */
  onPrepared?(ms: number): void;
}

/**
 * One message body. The sandbox token list is the security boundary:
 * allow-same-origin so the PARENT can reach in, and nothing else, so nothing in
 * the mail can run, submit, open a window or navigate the page.
 */
export const SANDBOX = 'allow-same-origin';

export function MailFrame(props: MailFrameProps) {
  const { html, allowRemote, proxyBase, clientPass = true } = props;
  const ref = useRef<HTMLIFrameElement>(null);
  const handle = useRef<FrameHandle | null>(null);
  const started = useRef(0);
  const [height, setHeight] = useState(0);
  const [hasQuoted, setHasQuoted] = useState(false);
  const [collapsed, setCollapsed] = useState(true);
  // The frame is invisible and takes no input until the parent's click capture
  // is on it. A sandbox without allow-top-navigation still lets a frame
  // navigate ITSELF, so a click that lands before the capture would load the
  // link inside the frame.
  const [interactive, setInteractive] = useState(false);
  // Bumped to remount the iframe if it ever escapes its srcdoc document.
  const [generation, setGeneration] = useState(0);

  const prepared = useMemo(() => {
    const t0 = performance.now();
    const p = clientPass ? prepareMail(html, { allowRemote, proxyBase }) : wrapRaw(html, allowRemote, proxyBase);
    props.onPrepared?.(performance.now() - t0);
    return p;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [html, allowRemote, proxyBase, clientPass]);

  // Latest callbacks without re-attaching.
  const cbs = useRef(props);
  cbs.current = props;

  const attachedTo = useRef<Document | null>(null);

  // Attach as soon as the srcdoc document is PARSED, not on the iframe's load
  // event: load waits for every image in the frame, so a frame sized on load
  // sits at 1px until the slowest image lands. The parent polls one frame at a
  // time (no script can run inside to tell us), and load stays as a re-measure.
  const attach = () => {
    const el = ref.current;
    const doc = el?.contentDocument;
    if (!el || !doc || doc.URL !== 'about:srcdoc' || doc.readyState === 'loading' || !doc.body) return false;
    if (attachedTo.current === doc) return true;
    handle.current?.detach();
    attachedTo.current = doc;
    let first = true;
    handle.current = attachFrame(el, cbs.current.testNoLinkCapture ? { noLinkCapture: true } : {}, {
      onHeight: (px) => {
        setHeight(px);
        cbs.current.onHeight?.(px);
        if (first) {
          first = false;
          cbs.current.onReady?.(performance.now() - started.current);
        }
      },
      onQuoted: setHasQuoted,
      onLink: (href) => cbs.current.onLink(href),
    });
    setCollapsed(true);
    setInteractive(true);
    return true;
  };

  useLayoutEffect(() => {
    started.current = performance.now();
    attachedTo.current = null;
    setInteractive(false);
    let raf = 0;
    const deadline = started.current + 10_000;
    const tick = () => {
      if (attach() || performance.now() > deadline) return;
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(raf);
      handle.current?.detach();
      handle.current = null;
      attachedTo.current = null;
    };
  }, [prepared.srcdoc, generation]);

  const onLoad = () => {
    const el = ref.current;
    // contentDocument is null once the frame has navigated cross-origin.
    const url = el?.contentDocument?.URL ?? '(cross-origin)';
    if (el && url !== 'about:srcdoc' && url !== 'about:blank') {
      handle.current?.detach();
      handle.current = null;
      attachedTo.current = null;
      setInteractive(false);
      cbs.current.onEscaped?.(url);
      setGeneration((g) => g + 1);
      return;
    }
    if (!attach()) return;
    handle.current?.measure();
  };

  return (
    <div className="mail">
      <iframe
        key={generation}
        ref={ref}
        title="message"
        sandbox={SANDBOX}
        referrerPolicy="no-referrer"
        srcDoc={prepared.srcdoc}
        onLoad={onLoad}
        data-height={height}
        data-csp={prepared.csp}
        data-trackers={prepared.blockedTrackers}
        data-remote={prepared.hasRemoteImages ? '1' : '0'}
        data-interactive={interactive ? '1' : '0'}
        style={{
          display: 'block', width: '100%', height: height || 1, border: 0,
          visibility: interactive ? 'visible' : 'hidden',
          pointerEvents: interactive ? 'auto' : 'none',
        }}
      />
      {hasQuoted && (
        <button
          className="quoted-toggle"
          onClick={() => {
            const next = !collapsed;
            setCollapsed(next);
            handle.current?.setQuotesCollapsed(next);
          }}
        >
          {collapsed ? 'show quoted text' : 'hide quoted text'}
        </button>
      )}
    </div>
  );
}
