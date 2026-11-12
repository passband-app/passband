import { createRoot } from 'react-dom/client';
import { MailFrame } from './MailFrame';
import { FUNCTIONAL, HOSTILE, newsletter } from './corpus';

// Test harness. ?mail=<id>  &images=1  &raw=1 (client pass off)  &thread=<n>
const q = new URLSearchParams(location.search);
const id = q.get('mail') ?? 'gmail-reply';
const allowRemote = q.get('images') === '1';
const clientPass = q.get('raw') !== '1';
const thread = Number(q.get('thread') ?? '0');
const noLinkCapture = q.get('nolinkcapture') === '1';
const PROXY = 'https://proxy.passband.test/client/image?u=';

// A secret the mail must never reach.
try { localStorage.setItem('pb-token', 'secret-device-token'); } catch { /* storage off */ }

declare global {
  interface Window {
    __spike: { escaped: string[]; links: string[]; heights: number[]; ready: number[]; prepare: number[]; t0: number };
    __pwned?: string;
  }
}
window.__spike = { escaped: [], links: [], heights: [], ready: [], prepare: [], t0: performance.now() };

function htmlFor(name: string): string {
  if (name === 'newsletter') return newsletter();
  return FUNCTIONAL[name] ?? HOSTILE.find((m) => m.id === name)?.html ?? `<p>unknown mail ${name}</p>`;
}

function App() {
  const n = thread > 0 ? thread : 1;
  return (
    <div style={{ width: 720 }}>
      {Array.from({ length: n }, (_, i) => (
        <MailFrame
          key={i}
          html={htmlFor(id)}
          allowRemote={allowRemote}
          proxyBase={PROXY}
          clientPass={clientPass}
          onLink={(href) => window.__spike.links.push(href)}
          onHeight={(h) => { if (i === 0) window.__spike.heights.push(h); }}
          onReady={() => window.__spike.ready.push(performance.now() - window.__spike.t0)}
          onPrepared={(ms) => window.__spike.prepare.push(ms)}
          onEscaped={(u) => window.__spike.escaped.push(u)}
          testNoLinkCapture={noLinkCapture}
        />
      ))}
    </div>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
