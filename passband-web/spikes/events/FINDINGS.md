# Spike 3: reading `/client/events` from a browser

**Verdict: go.** A cross-origin `fetch()` stream carrying a bearer token does
everything the Mac's `EventStream` does.

- **Code:** `src/sse.ts` (parser, line splitter, event decode, cursor rules) and
  `src/stream.ts` (connection loop).
- **Unit tests:** `npx vitest run`, 25 tests.
- **End to end:** `SPIKE_DB=<db> node e2e/run.mjs`, 9 checks in Chromium.
  Add `LONG=1` for the 33-second keep-alive check.

## Rules for the real client

1. **Use a `fetch` stream, never `EventSource`.** `EventSource` cannot send an
   `Authorization` header.
2. **Do not send `opens=1`.** That flag tells the daemon a Mac is listening.
3. **Keep the Mac's cursor rules.**
   - With no stored cursor, connect without `after=` (live events only, no
     backlog).
   - The cursor only moves forward.
   - A frame that fails to decode still advances the cursor if it has an id.
   - Dedupe events by id.
4. **Bring a 60-second inactivity watchdog.** `fetch` has none. The daemon sends
   a keep-alive every 15 seconds, so a silent stream is a dead one.
5. **React to page lifecycle.**
   - On `resume` or `pageshow`, reconnect at once and catch up with `after=`.
   - When the page becomes visible or the browser comes back `online`, cut any
     backoff wait short.
6. **One stream per account per browser, not per tab.** Elect one tab with Web
   Locks and share frames with the others over `BroadcastChannel`.
7. **The daemon's `/client` CORS must name its request headers.** Today it
   answers `Access-Control-Allow-Headers: *`. The Fetch spec says that wildcard
   does not cover `Authorization`. List the headers explicitly:
   - `authorization`
   - `content-type`
   - `x-passband-client`
8. **Set `Access-Control-Max-Age`.** Without it, every request and every
   reconnect pays for a preflight.

## Daemon behaviour

The daemon only wakes an open stream from its in-process notifier. An event
written by another process reaches the client on its next reconnect.

## Still to check by hand

- Real background tabs: Chrome's Memory Saver, Firefox and Safari. Headless
  Chromium never hides or freezes a page, so the lifecycle handling was tested
  with synthetic events.
- That a stream pinged every 15 seconds survives the hosted ingress path.
