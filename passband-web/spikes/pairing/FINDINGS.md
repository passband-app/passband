# Spike 4: pairing the web client with no code to type

**Verdict: go.**

## The flow

1. The app starts sign-in. It mints a 256-bit `state`, keeps it in this tab's
   `sessionStorage`, and sends the browser to control with `return=web&state=…`.
2. Control does the identity step, gets a pairing code from the warden, and
   redirects to `https://<app origin>/connect#url=<tenant>&code=<code>&state=<state>`.
   The secrets ride in the **fragment**. A fragment is never sent to a server
   and never appears in a `Referer`.
3. `/connect` erases the fragment (`history.replaceState`) before it does
   anything else. It then refuses the handoff unless all of these hold:
   - the `state` matches the one this tab stored (the stored value is consumed
     whether or not it matches);
   - the server is a hosted tenant (`https://<label>.passband.email`);
   - the code has the expected shape.
4. Only then does it `POST /client/pair` with `credentials: "omit"`,
   `redirect: "error"` and `referrerPolicy: "no-referrer"`.

## Why the `state` check is required

Without it, anyone could send a link that pairs the victim's browser with a
daemon the sender controls. The victim would then read planted mail, and
anything they typed would go to the sender. `state` makes `/connect` accept
only a sign-in this tab started. This is the same reason OAuth has `state`.

## Daemon change: `SQUELCH_PAIR_ORIGINS`

- **Empty (the default):** `/client/pair` carries no CORS at all, exactly as
  before. Every self-host stays here, and the existing test proving it is
  unchanged.
- **Set:** each entry must be an exact origin. `https://` is required, except
  that `http://` is accepted for a loopback host (local dev only). A wildcard,
  `null`, a path, userinfo or a malformed port is dropped, so a typo can only
  make the list shorter. Listed origins get CORS for `POST` with
  `content-type`, a 10-minute preflight cache, and no credentials.
- For hosted tenants, the warden would set this to the web app's origin and
  nothing else.

## What the end-to-end test shows (`test.mjs`, Chromium)

- The full round trip pairs with no typing. The final URL carries no fragment,
  the code appears in no request URL, and the code is one-shot.
- A forged `/connect` link (no matching `state`) is refused before any request
  leaves the page, and the code stays valid.
- A sign-in that comes back pointing at a server outside the allowed set is
  refused, and nothing is sent to that server.
- A page on an unlisted origin can't read a minted token. Its JSON claim is
  stopped at the preflight. Its `no-cors` `text/plain` POST is rejected before
  it reaches the store, so it can't use up the code or count as a failed
  attempt against it.

Rust: 3 new tests in `squelch-api/src/pair.rs`. The original
`the_pairing_route_answers_with_no_cors_headers` still passes.

## Not built in the spike

- **Control:** accept `return=web` and `state`, then redirect to the app origin
  with the fragment. Its CSP and redirect allow-list need the app origin added.
- **Warden:** set `SQUELCH_PAIR_ORIGINS` on hosted tenants.
- **Where the token lives after pairing.** That is part of the threat model,
  not this spike.
- **Device naming.** The token's label should say which browser and OS it is,
  so the user can revoke the right one later.
