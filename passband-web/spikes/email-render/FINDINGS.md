# Spike 1: rendering mail in a sandboxed iframe

**Verdict: go.** 148 Playwright tests pass in Chromium (`npm test`).

## Tests

- **Hostile mail, 130 tests:** 26 synthetic mails × 5 setups.
  - Setups: the full pipeline; full with images on; the frame alone (client
    pre-pass off); the frame alone on a page with no app CSP; and that last one
    with images on.
  - Each test asserts that:
    - no mail script ran;
    - no request went outside the allow-list;
    - neither the page nor the frame navigated;
    - no popup opened;
    - the parent's token never appeared in a request.
- **Behaviour, 13 tests:**
  - height within 2px;
  - growth when a late image arrives;
  - quote collapse on Gmail-style and "On … wrote:" replies;
  - a bottom-posted reply left alone;
  - link capture;
  - mail CSS cannot reach the parent page;
  - tracker stripping;
  - the remote-image opt-in;
  - the input gate.
- **Performance, 5 tests:**

  | Case | Time |
  |---|---|
  | One 143 KB newsletter | sized in 43 ms |
  | 50 short replies | about 200 ms |
  | 10 newsletters | about 330 ms |

## Rules for the real client

1. **The sandbox is exactly `allow-same-origin`.** Adding `allow-scripts` to a
   same-origin frame lets the frame remove its own sandbox. A test pins this
   value.
2. **The frame's CSP `<meta>` is the first child of `<head>`.**
   - It names `base-uri 'none'` and `form-action 'none'` explicitly, because
     `default-src` covers neither.
   - The remote-image choice is enforced by this policy, so anything the
     rewrite misses still fails closed.
3. **The app's own CSP keeps `frame-src 'self'`.** It is one of the layers that
   stops the mail frame navigating itself. The others are click capture and the
   input gate.
4. **Attach when the frame's document is parsed, not on `load`.** `load` waits
   for every image, so a frame sized on `load` stays 1px tall until the slowest
   image arrives.
5. **Everything runs from the parent.** Listeners, the `ResizeObserver` and DOM
   edits all work on the sandboxed document. Nothing is injected into the frame.
6. **The app CSP's `img-src` must include the image-proxy origin.** `srcdoc`
   frames inherit the parent's policy.
7. **The frame is hidden and takes no clicks until the parent has attached to
   it** (the input gate).

## Not ported yet

All of these are parent-side work:
- fit-to-width and full-bleed layout;
- `cid:` images (depends on spike 2);
- find-in-thread.

## Still to check by hand in Safari and Firefox

- `srcdoc` combined with `sandbox="allow-same-origin"`.
- That the CSP `<meta>` is enforced, and that `srcdoc` inherits the app CSP.
- That `<meta http-equiv=refresh>` is refused.
- That `frame-src` stops the frame navigating itself.
