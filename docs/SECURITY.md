# Squelch security model

One section per surface: the **invariant**, the **enforcement point** (`file:symbol`
— the line that actually holds it), and **what a maintainer must not break**.
Verified against the tree on 2026-07-30; §6 was written against it on 2026-08-18,
§8 on 2026-10-01.
Stale source comments are flagged, not trusted.

## 1. HTML sanitization at ingest

**Invariant.** `messages.body_html` never contains active content: no `<script>`, no
`on*` handlers, no `javascript:`/`data:` URLs, no forms, no nested browsing contexts,
no `<meta>`/`<link>`/`<base>`.

**Enforcement.** `squelch-core/src/sync/html.rs:sanitize_email_html` — a closed
`ammonia::Builder` allow-list, run once at ingest before storage; pure, no I/O,
fixture-tested in the same file. Four deliberate deviations from ammonia's
defaults: `url_schemes` narrowed to `{http, https, mailto, cid}`; `<style>` kept with
CSS verbatim (`rm_clean_content_tags` then `add_tags` — that order, or ammonia
panics); `style`/`class`/`id` added to the generic attribute set; `link_rel`
pinned to `noopener noreferrer`.

**Do not break.**
- Never widen `url_schemes`; `data:` coming back re-admits inline
  `data:text/html` payloads and is the only reason `data:` image URIs drop today.
  `cid:` is the one addition ever made, and only because it is a pointer with
  nothing behind it: it names a part of the same message (RFC 2392), reaches no
  host, and no renderer we ship resolves it. It is kept so the client can pair the
  reference with the attachment row carrying that Content-ID; dropping the `src`
  instead leaves an anonymous `<img>` nothing can resolve. The client rewrites the
  resolvable ones to `passband-cid:` and deletes the rest before any web view sees
  the body (§3), and `img-src` never names `cid:`.
- Never add a tag that creates a browsing context, submits, or loads a document
  (`iframe`, `object`, `embed`, `form`, `link`, `meta`, `base`).
- `<style>` is the ONLY raw-text tag allowed. Ammonia escapes `<` in text *and* in
  attribute values, so every surviving `<img` is a real tag — the client's regex
  image passes (§3) depend on that for correctness and for linear runtime.
- HTML never crosses the agent door; MCP serves flattened text only.
- `body_html` is **baked at ingest** — a sanitizer change does not retroactively fix
  stored mail. Delete the `sync_state` `history` row and restart `squelchd` to
  re-ingest (triage is preserved).

## 2. Rendering sandbox

**Invariant.** A body renders with no script execution, no network except the image
proxy, no navigation, no cookies, nothing on disk.

**Enforcement.** `passband/Sources/Passband/Views/EmailWebView.swift` —
five independent layers, each sufficient alone. Layer 1 is §1 above; the rest,
numbered as the source comments number them:

2. `allowsContentJavaScript = false`: page script cannot run at all. Our own injected
   `WKUserScript` is governed separately and still runs.
3. CSP injected as the FIRST child of `<head>` by
   `Coordinator.document(html:allowRemote:)`, built by `Lib/MailCSP.swift`:
   `default-src 'none'; style-src 'unsafe-inline'; img-src passband-img:
   passband-cid: data:` (`img-src passband-cid: data:` when remote images are
   off), plus `<meta name="referrer" content="no-referrer">`. No `script-src`, and
   no `http:`/`https:`/`cid:` anywhere in the policy. `passband-cid:` is
   unconditional because those bytes are a part of the same message fetched from
   the user's own daemon, so the remote-image opt-in has nothing to gate (§3).
4. Navigation refused. `Coordinator.decideNavigation` allows exactly the loads we
   started (`pendingOwnLoads` is a counter, not a flag: `loadHTMLString` is async,
   so two loads would otherwise race one permission) and cancels everything else; a
   `linkActivated` URL goes to `Opener.open`, which re-guards to http(s), and
   `FrameRelay.webView(_:createWebViewWith:…)` returns nil — no popups.
5. A shared **non-persistent** `WKWebsiteDataStore`; `baseURL: nil` for an opaque origin.

**Do not break.**
- The delegate signature
  `webView(_:decidePolicyFor:decisionHandler: @escaping @MainActor @Sendable …)`
  must match exactly. A near-miss leaves the requirement unimplemented and WebKit
  silently defaults to **allowing every navigation**.
- `FrameRelay` with no owner keeps returning `.cancel`: a pooled frame is unowned,
  and nobody is entitled to navigate it.
- `WebFramePool.Key` is `(message, allowRemote, document)` — all three, which is what
  makes a pool hit a byte-identical document under an identical CSP. Dropping
  `allowRemote` reuses a permissive frame for a strict render; dropping `message`
  leaves "one sender's mail in another's frame" to a hash collision.
- A body with **no message id** (the sealed reveal) is never pooled —
  `Coordinator.release` refuses it because `loadedPoolKey` is nil. Keep
  `WebFramePool.discard` unwiring the relay too: the content controller retains it,
  so merely dropping the frame leaves a live callback target.

## 3. Image loading and trackers

**Invariant.** Remote images may load, but only through our own audited fetch path,
never with cookies or a referrer, and never for a reference we did not mint.
Tracking pixels are stripped from mail whose sender the user has never written to.

Outbound read tracking is the mirror image of this section and is documented
separately in [TRACKING.md](TRACKING.md).

**Current behavior — VERIFIED, and not what old comments claimed.** Remote images
**load by default**: `Lib/Prefs.swift` registers `squelch.pref.loadRemoteImages =
true`, `SettingsView.swift:MailSection` exposes it as Mail → images → *Always* /
*On demand*, and `EmailWebView.allowRemote == prefs.loadRemoteImages || optedIn`.
There is no per-message `i` keystroke gate — that was the retired desktop client.
On *On demand* `img-src` collapses to `data:`, an opt-in bar appears, and **no
request is made** (the proxy scheme is absent from the policy, so the document
refuses it before the handler is reached).

**Enforcement.**
- `Lib/Trackers.swift:strip` drops `<img>` elements on strong signals only: both
  declared dimensions ≤ 2px, `display:none`/`visibility:hidden`, or a short
  path-scoped known-endpoint list. The bias is deliberately asymmetric — a missed
  tracker is one opaque referrer-less GET, a false strip visibly breaks the mail.
  **No reserialization**: every non-`<img>` byte survives verbatim, because a DOM
  round-trip would rewrite markup ammonia already vetted.
- **The strip is CONDITIONAL as of 2026-08-04.** `GET /client/thread/{id}` carries
  a per-message `sender_known`, true iff BOTH halves hold: the sender is a
  Sent-derived contact (`Store::is_known_contact` — a `contacts` row with
  `sent_count > 0`, the same predicate Stage-1 uses) AND the message's stored
  email-authentication verdict is a pass. When it is true
  `EmailWebView.Prepared.make` renders the unstripped body: someone the user
  writes to is allowed to learn they opened the mail. The bypass is scoped to
  `Trackers.strip` ALONE — `ImageProxy.rewrite`, the `passband-img:` CSP, the
  ingest sanitizer, and the remote-images default all still apply, so an allowed
  pixel still needs remote images on before it fetches. `allowTrackers` is part of
  `Prepared.cacheKey`, and `ImageWarmer` always prefetches the STRIPPED body so
  warming can never report an open the reader never made.
- **The auth half of the gate.** The `From` header is free text, so contact
  membership alone cannot authorize a tracker bypass.
  `squelch-core/src/sync/ingest.rs:extract_auth_pass` computes the verdict once at
  ingest into `messages.auth_pass`;
  `squelch-api/src/handlers.rs:get_thread` ANDs it into `sender_known`. Gated
  there and not inside `is_known_contact`, because that call also feeds triage's
  known-contact importance floor, which must not move.
  The verdict is read from the **TOPMOST** `Authentication-Results` header: Gmail
  PREPENDS its own and does not strip copies the sender wrote, so every occurrence
  below the first is attacker-controlled text. It counts only when the authserv-id
  is `mx.google.com`, a Google-written header (`X-Google-Smtp-Source`, or a
  `Received` handed off `by mx.google.com`) sits ABOVE it, and the POP marker
  `X-Gmail-Fetch-Info` is absent. Every `pass` must bind to the RFC 5322 From
  domain — `dmarc=pass` via an aligned `header.from=`, `dkim=pass` via an aligned
  signing domain. Two `From` headers yield no verdict at all.
  **NULL DENIES THE BYPASS.** Only `Some(true)` is a pass. `Some(false)` is "Gmail
  evaluated it and neither method held"; `NULL` is "never evaluated" — no Gmail
  header, an undecodable one, two `From` headers, or a row ingested before the
  column existed. Absence of proof withholds a permissive feature.
  **Nothing is backfilled.** `store/sqlite/migrate.rs` adds the column to an
  existing `messages` table with every historical row left NULL; a re-sync refills
  it through the message upsert.
  **Residual risks, accepted:**
  - The evidence is header SHAPE, not a signature — nothing in the bytes is
    cryptographically bound to Google. On an ingestion path where Gmail prepends
    nothing (POP with the marker somehow absent, or `users.messages.import`/
    `insert` performed with the user's own credentials) a sender who writes both a
    fake `Received: … by mx.google.com` and a fake verdict still gets through.
    Closing it needs the Gmail API's own delivery metadata, which this parser
    cannot see.
  - **No public-suffix list.** Alignment is equality or a dot-suffixed child of
    the signing domain, so a signature by a registry suffix such as `co.uk` would
    align with `victim.co.uk`. Not attacker-reachable — it needs a DKIM key for
    the suffix itself.
  - `header.i=` alone is accepted when `header.d=` is absent (the form Gmail
    usually emits), which DELEGATES the binding to Gmail enforcing RFC 6376 §3.5:
    an `i=` domain must be `d=` or a subdomain of it. When both are present that
    relationship is checked here instead of assumed.
- `Lib/ImageProxy.swift:rewrite` rewrites every http(s) image reference — `<img
  src>`, `style="…"` `url()`, `<style>` block CSS — to
  `passband-img://local/<hmac>?u=<encoded>`. Default-port `http://` targets are
  upgraded by splicing only the scheme to satisfy App Transport Security without
  normalizing or otherwise changing the source URL. `@import`/`@font-face` are
  skipped on purpose: they load no image, are dead under `default-src 'none'`,
  and rewriting them would hand the launch warmer requests the reader never made.
- `Model/ImageSchemeHandler.swift` is the only responder and
  `ImageProxy.original(from:)` its only parser: it requires this launch's HMAC and
  an http(s) target. The key is 256 random bits per process, never written down, so
  a signature cannot be replayed across launches. Provenance, not secrecy — mail can
  spell `passband-img:` itself in a kept `<style>` block, which `rewrite` neuters to
  `passband-img-blocked:` before minting anything.
- `Model/ImageStore.swift` owns the fetch: ephemeral session, no cookies, empty
  referrer, images only (an `image/*` header, or failing that a raster magic number
  per `Lib/ImageSniff.swift`, never svg), redirects re-guarded per hop, files named `sha256(url)`
  and a manifest holding **no URLs** — a directory listing the reader's mail URLs
  would be a readable index of their correspondence.
- **`cid:` references are the local mirror of all this.** `Lib/CidImages.swift`
  matches each `<img src="cid:…">` against this message's own attachment rows by
  normalized Content-ID (§1 keeps the scheme so there is something to match) and
  rewrites the ones that resolve to `passband-cid://local/<hmac>?a=<id>&n=<name>`.
  A reference that matches nothing, or whose part fails the same inline gate the
  attachment strip renders under (`AttachmentKinds.isInline` — stored bytes, a
  non-svg image mime, within `inlineMaxBytes`), has its **whole `<img>` tag
  dropped**: pre-migration mail resolves nothing at all, and a body with a gap
  reads as a message while one full of broken glyphs reads as a bug. The HMAC is
  over both values the parser returns and is what scopes a reference to its own
  message — without it a body could hand-write `?a=91` and read another message's
  attachment. Bytes come from the authenticated human door
  (`GET /client/attachments/{id}`), served by the same `ImageSchemeHandler` under
  the same live-set discipline, refused again over `inlineMaxBytes`.

**Do not break.**
- The order in `EmailWebView.Prepared.make`: `Trackers.strip` → `ImageRepeats`
  dedupe → the `hasNetworkImages`/`extractLinks` reads → `CidImages.rewrite` →
  `ImageProxy.rewrite` **last**. Trackers first so a pixel can never be the "first
  occurrence" that suppresses a real image; rewrite last because after it nothing
  recognises a reference as remote.
- `Prepared.cacheKey` folds in every attachment field the cid rewrite reads (id,
  content-id, downloadable, mime, size). Drop them and two bodies that share html
  but not parts collide in `PreparedBodies` — which is one message's photo pasted
  into another's.
- Never put `http:`/`https:` in `img-src`. A missed rewrite must fail closed
  instead of becoming an un-proxied request.
- Keep the signature check: without it a hand-written `url(passband-img://…)` in a
  kept `<style>` fetches a tracker while the UI says the mail has no remote content.
- `auth_pass` is three-valued and the comparison must stay `== Some(true)`. Any
  rewrite that treats NULL as a pass — `!= Some(false)`, `unwrap_or(true)`, a
  `NOT NULL DEFAULT 1` column, a backfill "so old mail isn't penalised" — silently
  grants the bypass to every row that predates the column and to every message on
  an ingestion path Gmail never stamped.
- Read the verdict from the FIRST `Authentication-Results` header, never the last.
  `Message::header_raw` resolves to the LAST occurrence — the forgeable one — and
  `headers_raw()` DROPS non-UTF-8 headers, which would let an attacker hide the
  genuine verdict and promote one they wrote.
## 4. Human and agent access after the triage rewrite

**Invariant.** Humans and internal triage models can read all their mail, including
pending assessments and actionable authentication. External user agents, including
the embedded assistant, cannot read actionable authentication secrets or their
derivatives. Informational login/security alerts are normally allowed.

**Assessment.** The triage model decides whether actual access-granting material
is present. There is no deterministic sealing detector in ingest. A missing or
stale assessment denies external access until a current allowed decision exists.
Model summaries, notification text, reasons, and evidence locations must not quote
credentials. This is a model-output requirement; it is not a claim that arbitrary
model output can be proven secret-free by a string filter.

**Enforcement.** `store/sqlite/messages.rs:external_messages_allowed_conn` checks the
current assessment and source provenance, of one message or of a set at once, under
the store lock. External whole-thread
reads require the thread's sources to be allowed. MCP and dedicated
`/client/agent/` routes use these checks for messages and derived results. Search
rehydrates results under the same guard instead of releasing stale cached snippets.
Account ownership is checked independently of access assessment.

**Archive mail (issue #422).** Mail older than the backfill window is stored
with `messages.is_archive = 1` and an `agent_message_state` row left at
`pending`: no model call is spent on it at ingest. To an external reader such a
row does not exist yet, which is a different thing from "pending" on live mail:

- The thread guard (`thread_guard_and_subject`) OMITS an archive sibling whose
  access is NULL or `pending` instead of closing the thread on it. The row is
  left out of the messages, the count and the subject (the thread is titled by
  the oldest message the reader may see). A thread of nothing but such rows is
  not found. A `restricted` sibling, archive or not, an archive row the owner
  sealed by hand, and any NON-archive pending sibling close the thread as before.
- The check is bought when an external caller surfaces the row: an agent search
  whose KEYWORD leg matches it (`search_mail`, `GET /client/agent/search`) queues
  one `access` job with trigger `archive_access`, best-ranked first, and an
  explicit thread open at the door (`get_thread`, `GET /client/agent/thread/{id}`)
  queues the SAME job for the thread's newest unassessed archive siblings, at
  most eight an open. A row the vector leg alone returned is never queued:
  nearest neighbours are not matches. A row that already has a job at its
  revision (finished, or parked on the daily budget) is not queued again; the
  one exception is a row whose latest check FAILED, which may be asked for
  again once that failure is a day old, and comes back as `archive_access`
  whatever queued it first.
- Three ceilings bound what a caller can make the daemon spend on archive mail,
  and every agent-door path that queues work on an archive row goes through the
  one function that keeps them (`request_archive_access_conn`): ten rows per
  call, twenty `archive_access` jobs outstanding per account, and
  `triage.agent.archive_access_daily_cap` (default 50) a day, charged on top of
  the total and access-lane budgets. Nothing is queued once today's access
  budget, or the total, is spent. A check that has been deferred even once loses its place
  ahead of ordinary background work. The thread READ itself (`thread_view`,
  which is also how create_draft and send_email check a reply target) queues
  nothing for an archive row, and neither does the source repair the triage
  agent's evidence tools run; `source_access` is for live mail only.
- The door waits for the checks it queued only while the refine lane says it
  can run (a poll has succeeded, a model is configured, the provider circuit is
  closed), gives up after two seconds if no worker has picked one up, and stops
  at eight seconds (MCP) or six (HTTP). `get_thread` waits the same way for the
  sibling checks its own open queued, BEFORE it applies the guard, so a thread
  is not served with a sibling missing and then closed when the verdict lands.
- UNTIL THE ROW IS `allowed` NOTHING OF IT IS RETURNED. No id, subject, sender
  or snippet, and no count or flag either. WHICH rows a search returns is the
  same set whether or not mail the caller cannot read matched: a search that
  matched only unassessed archive mail returns the same empty list as one that
  matched nothing. That is a statement about membership and it is the whole
  of the guarantee. The ORDER of the readable rows, and their `relevance`
  numbers, are not covered by it at the keyword leg (residual 3), and neither
  is how long the search took (residuals 1 and 2).
  An earlier draft reported a count of pending matches; that is an oracle (ask
  it about any word, phrase, sender or date and read the answer off mail nobody
  has cleared) and it must not come back in any form. What the response does
  carry is the same for every query: `indexed_since`, a DATE that is the walk's
  cursor (or, with no walk, where agent-readable mail first becomes dense, never
  one row's timestamp), and `archive` = `{target, walk_complete, older_mail}`
  where `older_mail` is the constant `"checked_on_first_match"`.
- HIDDEN MAIL TAKES NO PLACE IN AN EXTERNAL SEARCH. Both agent doors run one
  store method (`external_search_filtered`). Its recall legs carry a PREFILTER
  in their own SQL (`external_candidate_sql!`: the row is `allowed`, the owner
  has not sealed it, and no sibling the thread guard counts is un-allowed or
  sealed), so a row the caller cannot be shown is never ranked, windowed or
  hydrated. The keyword leg carries it in its FTS query. The vector leg does
  not use the vec0 nearest-neighbour index at all for these callers, because
  that index picks its neighbours over the whole account and can only be
  filtered afterwards: it selects the rows that pass the prefilter first, reads
  each one's stored vector, and orders them by `vec_distance_l2` (the metric
  the vec0 table ranks by), keeping the nearest 4,096 (`nearest_readable`). A
  hidden row is never measured, so no number of hidden rows, however near the
  query, moves a readable row down that list or off it. The human door keeps
  the vec0 index unchanged. The prefilter is a necessary condition and
  authorizes nothing: each
  leg's list is then cut to what the AUTHORITY allows
  (`external_messages_allowed_conn`, with its recursive check of every source a
  decision consumed, plus the thread guard `thread_allowed_with_conn`) BEFORE the legs are fused,
  and every hit is rehydrated through the same authority on the way out. Which
  readable rows come back is therefore decided over readable mail alone. Their
  order is too at the vector leg and in the fusion; at the keyword leg it is
  not quite (residual 3). Do not replace the authority
  with the prefilter, and do not loosen one without the other:
  `the_external_prefilter_is_implied_by_the_authority` holds them together.
- The rows a search asks an access check for are chosen by a separate query
  bounded at ten (`archive_surface_candidates`), before the search runs, so the
  search itself runs once. A query with no text the index can look up
  (operators alone: a sender, a date range) is not a keyword match and buys
  nothing, at either door.
- Accepted residuals. None of these returns a field of a hidden row; each is
  something a caller can infer from a response it was entitled to.
  1. SURFACING TIME. The time a search takes says that some unassessed archive
     row keyword-matched and a check was queued and waited for. Bounded by the
     ceilings above, and carries no content.
  2. KEYWORD LATENCY. A query whose words match hidden mail is slower than one
     whose words match nothing. FTS5 walks and scores every row a word
     matches before the prefilter can discard it, so the time grows with how
     many hidden rows say the word. Measured in a debug build over 4,200
     hidden rows that all match: 18 ms when one readable row matches too,
     17 ms when only hidden rows match, 0.3 ms for a word nobody says (it was
     1.3 s before the prefilter). A caller who can time its searches can
     therefore tell "some mail I cannot read says this word, and roughly how
     much" from "none does". Not bounded by the surfacing caps: no check is
     queued and none is needed for the effect.
  3. KEYWORD RANKING. Full-text relevance (bm25) is computed from word
     statistics over the whole full-text index: how many stored messages
     contain each word, and the average length of a stored message. The index
     holds all mail, readable or not. So the relative order of two READABLE
     hits, and their `relevance` numbers, can change with whether, and how
     often, a word appears in mail the caller cannot read. That includes
     unassessed archive mail, and it equally includes `restricted` and
     owner-sealed LIVE mail. In plain terms: a caller with two readable
     messages that each contain a different word can search for both words,
     see which message ranks first, and learn something about whether one of
     those words also appears in mail it cannot see. One mail carrying the
     word is enough to flip the order of two otherwise evenly matched hits.
     It is about one bit per query, a word-existence signal, never a field of
     the hidden mail. Single-word queries are affected too, through the
     average document length: enough long hidden messages reorder two
     readable hits of different lengths. It applies to keyword mode and to
     hybrid mode (the keyword leg's order feeds the fusion), not to semantic
     mode. It PREDATES the archive tier: sealed and restricted live mail sits
     in the same index and moves the same statistics; the archive adds more
     hidden mail to the index, not a new channel. It is
     NOT bounded by the surfacing caps, which limit model calls and not
     searches. Removing it means scoring against statistics of readable mail
     only, which FTS5 cannot do; that is a ranking change and has not been
     made.
  4. VECTOR LEG COST. The exact scan in `nearest_readable` reads and measures
     every readable vector of the account on each semantic or hybrid agent
     search, where the vec0 index would not. Its time depends on how much mail
     the account holds, not on the query, so it says nothing about any word.
     It also holds the store lock for that long, so live ingest and the
     human door wait behind it. Measured in a debug build over 10,000
     readable rows, one message per thread: about 245 ms for the leg, and
     more on mailboxes with long threads; a release build has not been timed. A readable row beyond the 4,096 nearest READABLE rows is still
     found by the keyword leg or not at all.
- PROMOTION is the one way `is_archive` changes, 1 to 0 and never back: the
  owner, at the human door, re-triages one archive message or corrects how it
  is filed (`promote_archive_conn`, called from `retriage_reset`'s
  single-message arm and from `correct_agent_triage_conn` for every field but
  `external_access`). Nothing at the agent door, in the walk, in a live
  re-sighting or in a heal reaches it. A promoted row is live mail in every
  respect: while its access is pending it CLOSES its thread like any live
  pending row, and it is RE-JUDGED like any live row. The triage that promotion
  queues writes access from its own decision, so a `restricted` an earlier
  access check put on the row holds only until that triage commits and may
  become `allowed`. The one verdict that is permanent is the owner's own seal
  (`external_access` corrected to true), which survives promotion and every
  later triage. Sealing or unsealing an archive row by hand does not promote it
  and queues nothing. The owner's OWN archive mail (sent, or from their
  address) is never promoted and never queued, whichever path names it.
- `cache_allowed` on the human routes is stricter than the agent guard
  (`human_cache_allowed`): the human reader is served the archive sibling, so a
  thread carrying an unassessed one is not cacheable.
- A reminder the owner sets on an archive message lists it at the human door
  like any reminded row. It does not surface at the agent door until the row
  is `allowed`.

Do not widen the omission: it is exactly `is_archive = 1` AND access NULL or
`pending` AND no owner seal (`archive_unassessed_sql!` in `store/sqlite/mod.rs`).

The human `/client/v2/messages/{id}` endpoint can open the exact message immediately.
It returns `Cache-Control: no-store`; `cache_allowed` controls client prefetch
eligibility. Human read, attachment, draft, and reply paths do not treat a legacy
`sensitivity=sealed` value as a denial. The legacy sealed list/reveal API remains a
compatibility surface, not the new access authority.

**Do not break.**

- Do not let the embedded assistant call unrestricted human read endpoints.
- Do not treat a missing assessment as allowed, or trust a client-only filter.
- Keep source provenance with derived summaries, attention, records, and drafts.
- Internal evidence readers stay account-scoped and never execute email instructions.
- Keep user message access independent from the speed or availability of triage.

See [TRIAGE-OPERATIONS.md](TRIAGE-OPERATIONS.md) for the current execution contract.

**Staged outbound attachments.** `outbound_attachments` holds composer files until
a send consumes them or their owning draft is deleted. Unclaimed uploads are
swept after a day by the next upload. These human-only routes never expose bytes
on MCP, never audit file contents, and use `no-store`. Byte responses use the same
content-type whitelist, `nosniff`, and attachment disposition as inbound files.
The outbound guard scans text and message parts at send time; binary files are
not scanned. Restricting external-agent access does not delete human drafts or
their attachments. However many drafts an account holds (one per reply target,
one per forwarded message, any number of new-message drafts), they are served
only on `/client/drafts` and the owner's own are never audited. The agent door
cannot list, read, edit or delete a draft; the one thing it can do, when the
owner allows it, is ADD one (§4c).

A staged file has a SECOND claim, `scheduled_send_id`, beside `draft_id`: the
outbox row (`scheduled_sends`, docs/SCHEDULED-SEND.md) that will send it. The
outbox is human-only, served only on `/client/scheduled`, with no MCP surface
at all (an agent's send is immediate and cannot be scheduled); unlike drafts it IS audited (ids and reasons only, never the
frozen request). A scheduled row holds its files for as long as it waits, up
to 366 days, and a failed row for 30 days more; only a cancel releases them
(back to unclaimed, the one-day clock restarted), and the compose door's
DELETE answers 404 for a file an outbox row holds. A fired row reads, sends and
consumes ONLY the files it holds, never an unclaimed one: file ids are reused
rowids, and the unclaimed arm would let somebody's fresh, unscanned upload go
out in place of a file the row lost.

**Recipients and the contact card are human-door data.** To and Cc are stored
for every message and Bcc only for the account's own sends. The store enforces
the Bcc rule in SQL, because on received mail that header is a leak or a
forgery. They ride on `ClientMessage` beside `html`; the agent door's
`SanitizedMessage` has no recipient fields at all. `GET /client/person` (the
reader's contact card) answers for one address at a time, excludes sealed mail
from every count and list, and has no MCP counterpart. The contacts seed stays
sent-only, so being copied in beside a stranger never earns them the
known-contact floor.

## 4b. Provider spam

**Invariant.** Mail the provider filed as spam is stored and readable on one
human-door page, and is otherwise absent: it never reaches an LLM, never crosses
the agent door, is never embedded, and never fires a notification.

**Why it is a security rule and not only a cost one.** Spam is attacker-authored
text selected for its ability to talk a reader into things, and a Stage-1 prompt
is a reader. It is also the bulk of what arrives, so "score it like anything
else" would mean spending the most model calls on the least trustworthy corpus.
The same reasoning is why the agent door gets **no spam at all** rather than
spam it is told to distrust: everything the agent reads is text it may act on.

**Enforcement, in order.**
- **Fetched only when asked for.** The poll loop does not walk the `SPAM` label
  at all — not on backfill, not on a history tick, not on a catch-up.
  `SyncEngine::sync_spam_window` runs when the page that shows the folder is
  opened, capped at `sync.spam_max` newest-first. That is a cost decision rather
  than a security one, but it has a security effect worth stating: the untrusted
  corpus is not continuously ingested, so on an install where nobody ever opens
  the page it is never fetched at all. The store's upsert keeps
  `is_spam = MIN(stored, incoming)`, so a message ever seen under a visible
  label cannot be hidden by a later spam sighting.
- **Never triaged.** `sync/ingest.rs` returns a neutral `tier=noise` row before
  Stage-1 — after the seal check, so a misfiled OTP is still sealed and sealed
  still outranks spam. `ingest_message` stamps the `'n/a'` stage markers, which
  is what keeps the row out of both LLM queues.
- **Never embedded.** `SyncEngine::ingest_one` returns `None` for a spam row, the
  same structural gate sealed mail gets. An embedding is a similarity claim, and
  spam is written to imitate the mail it impersonates.
- **SQL absence.** Every band, queue, count and search leg pairs `is_spam = 0`
  with its `is_sent = 0`. The one caller asking for the other side is
  `GET /client/updates?spam=only`; an unrecognized value is a 400, never a
  silent full listing.
- **The page's rows carry the mail's own words, and only there.** Nothing
  triaged a spam row, so its `one_line` is the empty string and the page would
  be a column of senders against a blank. The human door's listing
  (`human_agent_updates`) fills `subject` and `preview` (the stored
  200-character snippet) on the rows it serves, spam or not. Both are
  `Option`, skipped when `None`, so the agent door's
  `Update` never grows the keys at all — the same discipline `from_name` holds,
  for the sharper version of its reason: this is text out of the corpus written
  to talk readers into things. A person reading their own spam folder expects
  that and can weigh it. It is rendered as words on a row, never handed to a
  model, and never summarized.
- **Agent door.** `store::thread_view` (the `/mcp` shape) selects `is_spam = 0`,
  so a thread of nothing but spam is `NotFound` — the same shape sealed mail
  gets. `hybrid_search` is gated on all three of its queries.
- **Never notifies.** `triage::events::worthy_kind` returns `None` for a spam
  row as an explicit arm, not as a side effect of its neutral tier.
- **The one write.** `POST /client/actions/not_spam` removes the label in Gmail
  FIRST and only then clears the flag locally, so a refused write leaves both
  sides agreeing. There is deliberately no route the other way: squelch cannot
  show anyone the effects of training a filter it does not read.

**Guard tests.** `squelch-core/src/store/sqlite/tests/spam.rs` seeds one spam row
beside one ordinary row with overlapping search terms and walks every listing at
once, so a predicate dropped in a refactor fails there rather than in production.
The sync suite asserts the "not fetched routinely" half against the mock's CALL
LOG rather than against what landed: "no spam rows appeared" would also pass if
the walk ran and the folder happened to be empty, and the thing being avoided is
the request.

## 4c. Agent writes: drafts and sending (issue #394)

**Invariant.** An agent writes mail only as far as the mailbox owner has said
it may, only through the human door's own send pipeline, and never with more
latitude than the owner has there. With nothing chosen it may leave drafts and
cannot send: no mail leaves a mailbox on an agent's say-so unless the owner
chose that level. Nothing here changes what an agent can READ.

**The setting.** `app_settings` key `agent.write_access`, per mailbox:
`none`, `drafts`, `send`; each level includes the ones before it. A mailbox
that has never set it is at `drafts` (`AgentWriteAccess::UNSET`; the owner's
product decision, 2026-10-06). That default is safe to hold for the reason
`drafts` is the hard gate at all: a draft is a local row, marked as the
agent's, that becomes mail only when the owner presses send. What it does
cost is stated under the residual risks below.
`squelch-core/src/agent_write.rs:AgentWriteAccess::load` reads it, and a
STORED value this build does not recognize FAILS CLOSED to `none`, not to the
unset default: "the owner chose something we cannot read" is not "the owner
never chose". The type has no `Default` impl so that a failed read can never
be spelled `unwrap_or_default()`. It is set
only on the human door (`GET`/`PUT /client/settings/agent-access`, bearer-authed,
audited as `agent.write_access` with `before->after`; the ledger row is written
FIRST and a failed row leaves the setting unchanged). The agent door has no
tool or route that changes it. It is read from the store on every call, so
turning it off stops the next tool call, not the next restart.

**Enforcement.** The agent door (`squelch-mcp`) holds a `dyn AgentMailer` and
nothing else: no write credential, no message builder, no draft or outbox store
call (`squelch-mcp/src/main.rs:the_mcp_crate_never_reaches_the_draft_store_itself`
scans its source for those identifiers). The one implementation is
`squelch-api/src/agent_write.rs:ApiAgentMailer`, which `squelchd serve` hands
over via `ApiState::agent_mailer`. The standalone `squelch-mcp` binary passes
none and offers neither tool. For each call the mailer:

- re-reads the setting and refuses below the needed level (the MCP server
  also hides tools the level does not allow, but listing is a courtesy and
  this check is the gate);
- resolves a reply target by PERFORMING THE AGENT DOOR'S OWN READ
  (`readable_parent`: the message must appear in the `thread_view` that
  `get_thread` serves), so an agent can only reply to mail it can read, and a
  restricted, pending or spam id answers the same `not found` as an unknown
  one. It asks the read rather than re-listing the read's conditions: a first
  version re-listed them and left out spam (§4b);
- DECIDES THE RECIPIENTS ITSELF: the `to` the agent stated, else the parent's
  stored From, and the `cc` the agent stated or none. The owner's own reply
  honors the parent's `Reply-To` and derives copies from its headers; an
  agent's does not, because the agent is shown only a message's From and its
  human approves only what the agent shows them. A `Reply-To` nobody in that
  conversation saw must not decide where approved mail goes. The resolved
  `to`/`cc` are returned in the tool result;
- for `send_email`, runs `validate_send`, the outbound guard (§5) and
  `perform_send`, as `POST /client/actions/send` does, with `override_guard`
  hard-wired to `false`. The guard reads MORE than it does for the owner:
  recipients and subject as well as the body, and the body a second time
  with markdown emphasis marks removed (`guard_text`), since for an agent
  all of it is model output and this is the one check with no override;
- for `create_draft`, INSERTS a local draft marked `drafts.from_agent = 1`
  and never edits one: a reply whose message already has a draft is refused,
  because that row may be the owner's own unfinished writing and an agent
  cannot see it to know. At most 25 unsent agent drafts per mailbox. The
  client marks such a draft "from your agent" in the drafts list AND inside
  the composer it is restored into (`AgentDraftNotice`), with its recipient
  fields laid open: a reply draft is restored straight into the reply box
  when the owner presses `r`, so a mark only on the list would never be seen;
- refuses, for both tools, a body with no visible character, a header field
  carrying any control character or Unicode line separator, and more than 50
  recipients;
- audits every call that reaches it, refusals included, as actor `agent`
  (ids, counts and fixed reason strings only, never content). A draft and its
  ledger row are one transaction, like `set_sender_rule`. NOT audited: a
  `send_email` call the MCP server turns away for `human_approved` not being
  `true`, which never reaches the mailer; and, as on the human door, a send's
  success row is best-effort once the mail has left.

An agent's composition is deliberately narrower than the owner's: no
attachments, no forward, no send group, no read tracking, no scheduling.

**Asking for a draft on screen (`open_draft`, and `open: true` on
`create_draft`).** At `drafts` and above an agent may ask the owner's Mac to
bring one of its drafts up in the composer. This is the first thing the agent
door can do that reaches the owner's SCREEN AND KEYBOARD FOCUS, so it is held
to these rules:

- It is a request to an app, not a reach into a machine. The daemon publishes
  the draft id on an in-memory channel (`ApiState::draft_opens`); a Mac app
  that asked for it on its own authenticated event stream
  (`GET /client/events?opens=1`) receives an unpersisted, dated `open_draft`
  frame. Nothing is stored or replayed, and the app drops a frame more than a
  minute old, so a Mac that wakes from sleep has no window thrown at it. The
  phone never asks and is never told.
- Only a draft an agent wrote can be named. The mailer refuses any other id
  with the same `not found` as an unknown one, and the app checks
  `from_agent` AGAIN against the daemon's own draft list before opening
  anything.
- **It can open a draft; it must never be able to send one.** Bringing an
  app to the front under somebody who is typing delivers their next
  keystrokes to it, and in an agent's chat Cmd+Enter is "submit". So:
  a composer opened by a request has `ComposeState.keyboardSendLocked` set,
  and Cmd+Enter does not send it; only the send button does, and a click
  cannot be typed ahead. Keyboard focus is taken off the composer after it
  opens. And when the owner has anything of their own being written in
  Passband, the app does not come forward at all (their own half-written
  mail would be the thing a typed-ahead Cmd+Enter sent): a toast waits
  instead, and nothing on screen is replaced. It never switches accounts.
- It is rationed: one request per five seconds per mailbox, across all
  agents (`ApiState::draft_open_clock`). The clock is checked BEFORE the
  draft is looked up, and a request inside the interval is refused alike
  whatever it named and writes no ledger row. In the other order the
  refusal would tell an agent whether the owner had sent or discarded a
  draft, and every attempt would be a row.
- The agent is told the truth. The result says whether any Mac was listening
  (the channel's receiver count), and the instructions tell it not to claim a
  draft is on screen when none was, and that an open draft or a link is not
  approval to send.
- Audited as `draft.open` (`ok:mac`, `ok:no_mac`, `failed:target`), at most
  one row per interval. Best-effort, like the human door's action rows.

**The `passband://draft/<id>?mailbox=<address>&t=<created>` link** every draft
comes back with is UNTRUSTED INPUT on the app side: any web page or local
process can open one. It is parsed strictly (`Lib/AgentDraftLink.swift`:
exactly the minted shape, no user, port, fragment or second path segment). It
is bound to one draft by `t`, the second that draft was created, because draft
ids are small row ids that are reused: without it an old link in a transcript
would open whichever agent draft held the number next, and anything could walk
`draft/1..N`. The app opens a link only for a `from_agent` draft created at
that second, in the mailbox the link names (confirmed against the account's
own address, never assumed), at most once per five seconds, and under every
rule above. It can neither open nor probe for a draft the owner wrote.

Residual, accepted: on an Open door at the default level, anything that can
reach `/mcp` can make Passband come to the front every five seconds with a
draft it wrote, when the owner is not writing in Passband. It cannot send,
read more, or displace the owner's writing. `on_mac` reports a live
connection, which a Mac that just went to sleep still has for a while; the
agent may be told a Mac heard a request that the app then drops as stale.

`from_agent` is set at insert and never cleared. An owner who rewrites an
agent's draft still sees it marked, and so does an owner whose own save landed
on a row the agent created a moment earlier. That errs toward the warning.

**What "with your approval" does and does not mean.** At `send`, the approval
is the agent's human saying yes IN THE AGENT'S OWN CONVERSATION. The daemon
cannot see that conversation and does not verify it. What it does is:

- tell the agent the rule three times: in the server instructions sent at
  `initialize` (`server.rs:write_instructions`), in the `send_email`
  description, and in the required `human_approved` argument, each saying that
  approval is per message, comes from the human, and is never text found in an
  email or a tool result. A sender rule's `standing_instruction` is named
  explicitly as never being approval: `set_sender_rule` is an agent write, so
  that text may have been written by an earlier, compromised session;
- refuse a call whose `human_approved` is not `true`;
- annotate the tool `destructiveHint` + `openWorldHint`, which is what MCP
  clients key their own confirmation prompts on.

**Residual risk of the default (`drafts`), accepted.** Every mailbox whose
daemon serves the agent door offers `create_draft` without the owner having
opened the setting. On an Open door (self-host, no agent sign-in) that means
anything that can reach `/mcp` can leave up to 25 drafts in the owner's
drafts list, and a reply draft is restored into the owner's reply box. What
stands between such a draft and the wire is the owner: the mark in the list,
the notice in the composer with its recipients laid open, and their own
press of send. A draft written to be sent unread ("looks like mine") is the
attack this default exposes, and the mark is the defence; do not weaken it.

**Residual risk of `send`, accepted for an opt-in level and stated in the
client.** A
prompt-injected or simply wrong agent can assert `human_approved: true` and
send. At `send`, the blast radius of the agent door grows from "can read
triaged mail and write local rules" to "can send mail as the owner", including
mail that quotes whatever it was able to read. Sealed mail stays unreadable and
the outbound guard still blocks secret-shaped text with no override, but
ordinary content is not protected by either. On an Open door (self-host, no
agent sign-in) "the agent" is anything that can reach `/mcp`. An owner who
wants a hard gate uses `drafts`, where the only way mail leaves is the owner
pressing send in Passband. Not built yet, and the natural next steps if `send`
needs hardening: per-send approval inside the app, a recipient allow-list, a
daily cap, reply-only, and recording who authored a sender rule so agent-written
rule text is not delivered as the owner's words.

**Do not break.**
- Never give `squelch-mcp` the write credential, a `GmailWriteClient`, or a
  direct draft/outbox store call. Agent writes go through `AgentMailer`.
- Never let an agent path pass `override_guard: true`.
- Never resolve an agent's reply target with `action_message_ref` alone; it
  applies no agent access check. And never re-list the agent door's read
  conditions beside it: ask `thread_view`.
- Never let an agent's send take its recipients from the pipeline's
  derivation (`Reply-To`, the parent's Cc). State `to` and `cc` on the
  `SendBody`.
- Never let the unset level be `send`. Sending is only ever a choice.
- Keep an unreadable stored setting, and a failed read, meaning `none`.
- Keep the setting off the agent door: an agent that could raise its own
  level has no level.

## 5. Outbound secret guard

**Invariant.** A secret-looking outgoing body is blocked unless the caller
explicitly overrides, and the matched text is never copied anywhere — not into a
response, a log, or the audit row.

**Enforcement.** `squelch-api/src/guard.rs:scan` / `scan_kinds`, called from
`handlers::action_send` before the message leaves the process, and from
`scheduled_send::schedule` when a mail is scheduled. Detects `pem_block`,
`api_key` (vendor prefixes), `otp_code` (6–8 digits near an auth word), `long_hex`
(≥32), `long_base64` (≥40). A non-empty match set returns HTTP 422 listing only the
**kinds**; `override_guard: true` sends anyway and writes a `guard_override:<kinds>`
audit row. False positives are acceptable precisely because it is overridable.

**Forwards are scanned twice over.** Forwarding is the classic exfiltration shape —
a clean note wrapped around someone else's secret — so `handlers::forward_send`
scans the ORIGINAL as well as the note, unions the kinds and issues ONE 422 / one
`guard_override` row for both. The scan runs on the raw bytes re-fetched from Gmail
and about to be composed, never on the stored row: the stored body is a sanitized,
flattened memory of the message and the bytes on the wire are the original.

*Exactly what is scanned* on a forward: the sender's typed note, the original's
decoded text bodies — every part in mail-parser's `text_body` list — an html-to-text
conversion of its `html_body` view, **and** the contents of every `text/*` and
`message/*` attachment part. Both body views are needed because when a `text/plain`
part exists mail-parser never converts the html alternative down, so an alternative
whose text half is innocuous and whose html half carries the key would otherwise ship
unread. The text attachments are scanned because a PEM key pasted into `notes.txt`
and dragged onto a mail is the same exfiltration shape as one pasted into the body —
arguably the more natural one — and mail-parser has already transcoded each such part
to UTF-8, so reading it as text costs nothing; `message/*` parts (an attached email,
a bounce report) are plain RFC822 text and are read the same way. Two known limits,
stated rather than implied: **attachment bytes under any other mime are not scanned**
(a key inside a zip, or an `id_rsa` carrying an `application/octet-stream` mime,
passes), and **html attribute values are not scanned** (the converter yields visible
text, so a secret inside an `href` or a `data-` attribute survives). The guard is a seatbelt against the accident, not a DLP
boundary against a determined sender — it is overridable by design.

**Forwarded HTML goes out verbatim, trackers included.** The original's markup is
embedded as it arrived (the only edit is stripping the `<meta>` tags that DECLARE a
charset — a `charset` attribute or the `http-equiv="Content-Type"` spelling — which
would contradict the part's own `charset="UTF-8"`; a `<meta>` that merely mentions the
word survives). The forward's two body parts then ship
`Content-Transfer-Encoding: quoted-printable`, which is an encoding rather than an
edit: a stranger's newsletter html is routinely one line far past RFC 5322's 998-octet
limit, and re-emitting it raw put illegal lines and raw 8-bit bytes on the wire. It
decodes back byte-for-byte at the far end. All of which means the ORIGINAL sender's
tracking pixels are re-armed and will fire toward whoever the forward is addressed to.
This is deliberate and matches every mainstream mail client: passband strips trackers
out of what it RENDERS to its user, not out of what its user chooses to pass on.
Rewriting a stranger's markup on the way out would forward something other than what
arrived, and the user's decision is to send *this message*.

**Scheduled sends are judged once, when scheduled.** `scheduled_send::schedule`
runs the same scan while the user is there to answer: the note, the staged text
files, and for a forward the ORIGINAL, read on the write token at schedule time
and unioned into one verdict. `override_guard: true` persists in the frozen
request. At fire time the worker calls `perform_send` with `SendGuard::Settled`,
so nothing re-scans: the content is frozen, and a forward's original is
re-fetched from Gmail at fire (Gmail's raw bytes are immutable, so what is sent
is what was judged). A cancel hands the request back WITHOUT `override_guard`,
so a reschedule is judged again.

**Scheduled sends carry a visible fingerprint (privacy).** Every mail the outbox
fires goes out with `X-Passband-Outbox: <token>` and a `Message-ID` of
`<token@passband.app>`, where the token is 48 hex chars of fresh randomness minted
per row (docs/SCHEDULED-SEND.md §4). Recipients can see both: they reveal that the
mail was sent by Passband's scheduler, and carry nothing else, no account, mailbox,
user or row id, so two scheduled mails from one user are not linkable by them.
Interactive sends carry neither.

**Do not break.** `GuardMatch::kind()` is the only thing that may leave the process —
never return, log, or audit the matched substring. Keep the override audited; an
un-audited bypass is not a bypass we can reason about.

**Post-send echo (audit contract).** After a successful send,
`handlers::echo_sent` fetches the sent message back (`format=raw`, write token) and
ingests it locally so the thread shows the reply immediately. It is strictly
best-effort — the mail has already left, so nothing here may fail the request — and
audits under its own action `send.echo`, alongside the `send` row's own outcomes
(`rejected:confirm`, `rejected:forward_and_reply`, `rejected:empty_body` — skipped for
a forward, whose note may legitimately be empty — `blocked:guard`,
`guard_override:<kinds>`, `rejected:no_write_credential` (including the forward whose
raw fetch found the write credential dead — a 403 telling the user to re-run
`squelchd auth --write`, never the 502 that would blame Gmail), `failed:target`,
`rejected:no_recipient`, `rejected:too_large` (the original exceeds
`MAX_FORWARD_RAW_BYTES` = 20 MiB decoded, refused with a 413 before the four-to-five-x
re-encode allocates — or the send's own staged attachments total more than 25 MB),
`rejected:attachment_missing` (an `attachment_ids` entry names no staged file — swept,
deleted, or another account's — so the whole send is refused rather than going out
with fewer files than the tray showed), `rejected:attachment_cid_clash` (two staged
files carry one `content_id`, so the body's reference would resolve to a coin flip),
`failed:fetch_original` (the forwarded
original could not be read back, so nothing was sent), `rejected:compose`,
`paced` (the mailbox's Gmail budget held a call on the way, the send itself, a
forward's original or a reply's parent, so nothing was posted; answered 503),
`failed:gmail`, `ok`, `ok:forward`). A fired scheduled send's `send.echo` detail
is marked `scheduled:<id>:` exactly as its `send` row is:

| `send.echo` detail | meaning |
| --- | --- |
| `skipped:no_id` | Gmail's send response carried no message id; nothing to fetch back. |
| `skipped:sealed` | the outbound copy tripped seal detection; nothing was committed. |
| `failed:fetch` | the read-back GET timed out (5s), failed, decoded badly, or returned zero bytes. |
| `failed:ingest` | the local store write failed. |
| `ok:<local id>` | echoed; `<local id>` is the local `messages.id`. |

The echoed row goes through the SAME seal-first ingest as any other message
(`sync::ingest::ingest_sent`, `is_sent: true`), so it runs no LLM. Because seal
detection precedes the `is_sent` branch, a reply quoting an OTP trips it — and such
a copy is **not written at all** (`Ok(None)`, `skipped:sealed`). Committing it would
put a sealed row in the thread, and `thread_guard_and_subject` 404s any thread
holding one, so the echo would hide the counterparty's mail the user was reading a
second ago; skipping degrades to "the reply appears on the next backfill", the
pre-echo status quo. The read-back is also capped at **5s**
(`ECHO_FETCH_TIMEOUT_SECS`): the client's POST timeout is 15s and `action_send` has
already made two serial Gmail calls, so bookkeeping may not spend the rest of that
budget. Keep the echo's failures audited and swallowed; keep it out of core's write
surface (the fetch lives in `squelch-api/src/gmail_write.rs`, core takes bytes).

**Scheduled send (audit contract).** The outbox door audits under three actions of
its own, target = the message a reply or forward is about (`schedule`) or the row id
(`schedule.cancel`, `schedule.now`), details ids and reasons only. A FIRED send
writes the ordinary `send` row, marked with its outbox row:

| action / detail | meaning |
| --- | --- |
| `schedule` · every `send` refusal above | the same ordered refusals a send meets (`rejected:confirm`, `rejected:attachment_missing`, `rejected:no_write_credential`, `failed:target`, ...), in the same order. |
| `schedule` · `rejected:send_at` | `send_at` missing, not RFC3339, not in the future, or more than 366 days out. |
| `schedule` · `rejected:group_fanout` | an `individual` group: one minted Message-ID cannot name N messages. |
| `schedule` · `rejected:too_many_scheduled` | the account already holds 100 live outbox rows. |
| `schedule` · `failed:group` / `failed:store` | the group did not resolve / the insert failed. |
| `schedule` · `blocked:guard` / `guard_override:<kinds>` | the verdict, given at schedule time. |
| `schedule` · `ok:<id>` | scheduled; `<id>` is the outbox row. |
| `schedule.cancel` · `ok` / `rejected:sending` / `failed:not_found` | cancelled / in Gmail's hands (409) / unknown, another account's, sent or cancelled (404). |
| `schedule.now` · `ok` / `rejected:sending` / `failed:not_found` | queued for the next tick / in Gmail's hands (409) / nothing to queue (404). |
| `send` · `scheduled:<id>:<outcome>` | a fired row went out; `<outcome>` is the plain send's (`ok`, `ok:forward`, `ok:bcc:N`, ...). |
| `send` · `scheduled:<id>:recovered` | Gmail already held the row's mail, found by its minted Message-ID or its `X-Passband-Outbox` token (a lapsed lease, or a re-fire after an ambiguous failure): settled, NOT sent again. |
| `send` · `scheduled:<id>:failed:*` / `scheduled:<id>:rejected:*` / `scheduled:<id>:paced` | a fired row's refusal, the plain send's detail marked with the row. `paced` spent no attempt. |
| `send` · `scheduled:<id>:failed:request` / `failed:attachment_missing` / `failed:crash_loop` / `failed:recovery` / `failed:group_fanout` | a TERMINAL outcome the worker decided without reaching the send (so no plain send row exists to mark): an unreadable request, a lost file, a row that kept dying mid-send, a recovery or pre-fire search never able to ask Gmail, an (unreachable) fan-out. The worker writes these itself. |
| `send` · `scheduled:<id>:rejected:no_write_credential` / `failed:search` / `failed:store` | a worker-side attempt that never reached the send: no credential before the pre-fire search, the search refused (a revoked grant surfaces here, or a 4xx other than 403/429, which are "not now" like a transport error), the row's files unreadable. Each spends an attempt, and is written only when the settle moved the row (a worker that outlived its claim writes nothing). |
| `send.echo` · `scheduled:<id>:<detail>` | the echo of a fired or recovered row, the plain detail marked with the row. |

The fire-time grace deferral (`recovery:grace`) and an unreachable pre-fire
search (`recovery:unreachable`) are "not now" answers that spend no attempt and
write no ledger row; the listing's `last_error` says why the row waits.

## 6. The embedded assistant

> The dedicated `/client/agent/` read routes and source checks in §4 supersede
> references below to using ordinary human endpoints or legacy sealing.

**Invariant.** The agent inside Passband reads only what the human door serves, so
sealed mail is absent from it for the same reason it is absent from the door; and it
cannot touch the mailbox without a human tap that has already happened by the time
the call goes out.

**Enforcement.**
- **Reads inherit §4.** Every tool call in
  `passband/Sources/Passband/Assistant/AgentTools.swift` goes through `APIClient`,
  i.e. `/client/*`. The assistant cannot read a 2FA code because the door it knocks
  on has none — nothing is filtered here, and nothing needs to be. It holds no
  sealed tool: `/client/sealed` and the reveal route are not in its inventory.
- **Three tiers, and only one of them stops.** Fourteen tools: reads are never
  gated; safe writes touch this database only (status, sender rules, drafts) and are
  reversible in the app; the four that touch Gmail (archive, label, send,
  unsubscribe) route through `AgentTools.confirmed(_:_:)`, where only `.approved`
  reaches `perform`. That single funnel is the entire reason the `confirm: true`
  those calls carry is a true statement rather than a default.
- **The daemon does not take the client's word for it.** Every mutating `/client/*`
  route demands the flag itself and audits the attempt, and a send still meets the
  outbound secret guard (§5). A client that lied would be recorded doing it.
- **A half-arrived instruction is not an instruction.**
  `passband/Sources/Passband/Assistant/Assistant.swift` executes a `tool_use` only
  after its message has closed, so a partially streamed tool input never runs.
- **The key.** Self-host is BYOK and the key is read only inside
  `LLMProxy` (`passband/Sources/Passband/Model/Keychain.swift`) at call time —
  never a parameter, a return value, or an error string — over sessions that refuse
  every redirect, because URLSession carries `x-api-key` verbatim across a hop.
  Hosted holds no key in the app at all: `POST /client/assistant/messages`
  (`squelch-api/src/assistant.rs`) relays with a daemon-held credential and streams
  the bytes back, human door only, and 404s when no gateway is configured. The
  conversation body is treated like mail content and logged in neither direction.

**What a maintainer must not break.**
- **Prompt injection is the standing threat, and the gate is the answer to it.**
  Mail bodies reach this model as data, so assume the text is adversarial and
  assume it will eventually ask for an action. Everything that touches the mailbox
  must keep going through `confirmed`; a new ungated write is how this section stops
  being true.
- Do not hand the assistant a sealed route, and do not "helpfully" widen a tool's
  read to a store call that bypasses the door.
- Do not log the relay body, in either direction, at any level.
- A decline is a legitimate answer with `is_error` false. Do not turn it into a
  failure the model feels invited to retry.

## 7. Automatic unsubscribe

**Invariant.** The daemon fetches a URL a stranger chose only after the user asked
to unsubscribe from that stranger, reaches only the public internet when it does,
and takes at most one action on the page it finds: the one a person would take by
pressing its single button.

This reverses an older rule ("the server never fetches the unsubscribe link") on
purpose. The link is no more dangerous than it was; what changed is who follows
it. That move is what the guard below exists for.

**Enforcement.**
- **Never before the ask.** Nothing prefetches, previews or probes an unsubscribe
  link: an unsubscribe may complete on a plain GET. The job starts from
  `POST /client/unsubscribe` with `auto: true` (`squelch-api/src/unsub_job.rs`),
  human door only; the agent door has no unsubscribe route at all, and the
  assistant's tool goes through the confirm card (§6).
- **Not for mail we cannot trust.** Spam (`is_spam`) and mail without a passing
  authentication verdict (`auth_pass` not true) are never unsubscribed from
  automatically: fetching a spammer's link confirms the address is live, and an
  unauthenticated From line may not be the link's real author. Mail from before
  the verdict was recorded (`auth_pass` NULL) is treated the same way. All fall
  back to the browser, with the reason (`auto_refused`) for the client to say.
- **A footer link must be the sender's own.** A header link is the sender naming
  its endpoint. A link picked out of the body runs automatically only when its
  site matches the From domain's (`unsub_job::choose_plan`): a colleague's
  forward of a newsletter is authenticated mail from the colleague carrying
  someone else's footer, and following it would unsubscribe the colleague.
- **SSRF guard, in code, for every deployment** (`squelch-api/src/unsub_fetch.rs`).
  http/https only, ports 80/443 only, no userinfo. Names resolve through a resolver
  that returns PUBLIC addresses only (no loopback, private, link-local, CGNAT,
  multicast, reserved, or v4-in-v6 forms of those); reqwest connects to exactly what
  it returned, so DNS rebinding has nothing to rebind. IP literals, which never
  reach a resolver, are checked per hop. Redirects are followed by hand so every
  hop passes both checks; a 307/308 never carries a body to another host, and
  the https-first retry falls back to http only when nothing was sent. Hosted
  tenants also sit behind their NetworkPolicy; a self-host has only this, which
  is why it is not optional. One gap is inherent: a self-host on a network with
  global IPv6 can reach its LAN's devices at their global addresses on 80/443,
  because those addresses ARE public.
- **One action, and it has to prove itself** (`squelch-api/src/unsub_html.rs`). A
  form is submitted only when exactly one submit control plausibly unsubscribes
  (one lone button, or the one among several that says so), with no password or
  file field, no required choice left unmade, and NOTHING TYPED: an empty field
  stays empty, and a required one refuses the form. A link is followed only when
  its own text says unsubscribe; it may point at any public host, which is no
  more than the sender's redirects could already do. "Undo", "cancel" and
  "resubscribe" controls are never actions. Hidden text never counts as the
  page's answer. The page that comes back must say the user is unsubscribed (in
  so many words, or by the page reader's verdict); otherwise the job hands the
  link to the user. Optional surveys are never answered.
- **The page reader is fenced, cheap, capped and billed**
  (`squelch-core/src/triage/unsub_page.rs`). The page is untrusted input inside the
  usual fence; any non-answer is `unclear`, which costs the user one click. Its
  calls bill to `unsub_page` and stop at a daily cap.
- **Bounded.** 1 MiB bodies, per-request timeouts, a 45s job budget (slot wait
  included), four jobs at once, a cookie jar that lives for one job. Parsing is
  linear (a tag budget, a per-form control cap, no inner scan past its parent)
  and runs on the blocking pool. A panic inside a job still settles its row.
- **The link stays out of logs and the audit.** Audit rows carry the sender and the
  verdict; transport errors are reduced to their kind before they can be printed.

**The browser rung** (`squelch-api/src/unsub_browser.rs`, `unsub_cdp.rs`,
`squelch-core/src/triage/unsub_agent.rs`). Everything above describes the plain
reader, which holds on every daemon. A daemon that was given a Browserbase key
(every hosted tenant; a self-host only if it brings its own) has one more
rung: a page the reader gave up on for a reason a real browser could fix
(`choose_topics`, `needs_browser`, `unclear`; never a dead link) is opened in a
rented remote browser that the notify model steers. On that rung the "one
action, nothing typed" rule above is deliberately replaced by this one:

- **The page's scripts never run near the daemon.** The browser is Browserbase's,
  on their network, reached over one WebSocket. Nothing the page executes can
  reach the daemon, its volume, its loopback doors or its neighbours. The same
  per-hop URL check the fetcher uses still decides whether a link is handed over
  at all.
- **The agent has a closed vocabulary, enforced on the page and not in the
  prompt** (`unsub_browser::permit`). Press a link or button, turn a checkbox,
  radio or switch on or off, choose a select's option, fill an email field. Every
  action names a control by number, and the number is checked against the
  controls the daemon itself found, kind included. A short list of labels is
  never pressed whoever asks: resubscribe, undo, sign in, create or delete an
  account, download, buy.
- **Exactly one string can be typed**, the address the mail was sent to
  (`unsub_job::address_to_type`: the account's address, or its plus address when
  that is what the message was addressed to), and only into a field that is for
  an email address. The model's answer carries no text of its own: there is no
  action that types what the model says, and no free-text field in its output
  for a page to have it fill in. A password, a name or a card number cannot be
  entered because nothing can enter one.
- **What the model reads is what a person would see.** The page is read from an
  isolated JavaScript world the page's own scripts cannot see into. Text that is
  in the markup but not on screen (hidden, collapsed, clipped, off the page) is
  not read, because the agent's "done" is taken from this text. The page's own
  square brackets are rewritten, so it cannot draw a control that is not there.
  It goes to the model inside the usual fence.
- **Bounded and billed.** Eight steps a job, sixteen actions a step, 100 seconds
  on top of the plain ladder, two browsers at once, a daily cap on steps, and
  usage under `unsub_agent`. A step that changes nothing twice ends the job.
- **A person is asked when a person is what the page wants.** A sign-in, a
  captcha (sessions are created with captcha solving OFF), a confirmation link
  sent by mail, a question only the user can answer: each ends the job as
  `needs_you` with that reason and the original link.
- **Failure of the machinery changes nothing.** No browser to rent, a dropped
  socket, the model's cap: the outcome is whatever the plain reader had already
  decided.
- **What Browserbase learns.** The unsubscribe link (which already names the
  recipient to the sender), the page it leads to, and the address if a form asks
  for it. Sessions are created with recording and logging off. No mail, no
  credentials and no other mailbox data are reachable from the rung; it has no
  route to the store at all. Browserbase is a subprocessor for every hosted
  mailbox.

**What a maintainer must not break.**
- Do not build a second outbound client for sender-chosen URLs, or call
  `reqwest` directly on one: `Fetcher` is the guard, and a client without it is an
  SSRF primitive aimed at the daemon's own door on loopback.
- Do not prefetch an unsubscribe link to "show a preview" or to classify it early.
- Do not make `Action::submission` type anything. The page's host is the
  sender's choice at every hop, so there is no site an address could be safely
  typed into, and the account's primary address is not necessarily the one the
  list knows.
- Do not give the browser agent a way to type anything but the one address, and
  do not add a free-text field to its output. The vocabulary is the fence.
- Do not move a check out of `permit` and into the prompt. The prompt is advice
  to a model reading a hostile page; `permit` is what actually happens.
- Do not run the browser inside a tenant pod or on a self-host's own machine
  without first giving it the fetcher's network guard: a real browser loads
  subresources and runs scripts, and the public-only resolver covers neither.
- Do not turn session recording, logging or captcha solving on.
- `needs_you` sends the user to the ORIGINAL link, not a post-redirect hop that may
  depend on this job's cookies.

## 8. Agent door authentication

**Invariant.** On a hosted tenant, only the owner of that tenant's mailbox can use
its agent door, and only through a credential that cannot open the human door. A
daemon whose agent-door settings are incomplete serves nothing on `/mcp`. A
self-hosted daemon with no settings behaves exactly as it always has.

**Three modes**, chosen once at startup by `squelch-mcp/src/auth.rs:AgentDoorAuth::from_env`
and applied by `squelch-mcp/src/lib.rs:agent_door_router`, the only public way to
build the HTTP agent door (the bare rmcp service is private to the crate):

- **Open.** Neither `SQUELCH_MCP_OAUTH_ISSUER` nor `SQUELCH_MCP_PUBLIC_URL` is set.
  No auth layer at all; the only guard is rmcp's Host allow-list (loopback plus
  `SQUELCH_MCP_ALLOWED_HOSTS`). This is self-host, and it is fit only for a listener
  the owner controls the reach of. No metadata routes exist.
- **OAuth.** Both are set, each a valid `https` URL (`http` only on loopback), and
  the public URL's path is exactly `/mcp` with no query or fragment (the resource
  IS the door, so any other path could never match a token's `aud`). Every
  `/mcp` request needs a bearer token, verified below. Two unauthenticated GET routes
  publish RFC 9728 protected-resource metadata naming the issuer.
- **Misconfigured.** Exactly one is set, one does not parse, the public URL's path
  is anything but exactly `/mcp`, or OAuth is configured
  but the daemon has no real account email to bind tokens to (unset, or the
  `me@localhost` placeholder; `AgentDoorAuth::for_account`, applied inside
  `agent_door_router` so no host can skip it). `/mcp` answers 503
  to everything, no metadata is published, and startup logs which setting is at
  fault. It never falls back to Open: the likeliest way to get here is a hosted pod
  that lost one variable, and that pod's `/mcp` is published. The human door is
  unaffected.

**Roles.** The daemon is a **resource server only**. WorkOS AuthKit is the
authorization server: it runs registration, consent and token issuance, and our
control plane supplies the identity (a Google sign-in, mapped to a mailbox) through
AuthKit's Standalone Connect hop. The daemon never issues a token, never sees the
user's Google credential, and never calls AuthKit about a specific token. It
verifies AuthKit's RS256 access tokens locally against the issuer's JWKS.

**Verification, in order** (`auth.rs:Verifier::verify`). Each step is a refusal on
failure, and the cheap, local steps come before anything that touches keys:

1. An `Authorization: Bearer` header (`squelch-httpauth:parse_bearer`). Absent or
   not Bearer: 401 with `WWW-Authenticate: Bearer resource_metadata="..."`.
2. The token is at most 8 KiB. Checked before any decoding.
3. The JWT header parses, `alg` is exactly `RS256`, and there is no `crit`
   header (RFC 7515 §4.1.11: we understand no extensions, so we accept none).
   `none` does not parse, and every HS* algorithm is refused here, before any key
   is looked up, so the RSA public key can never be fed to HMAC.
4. The header has a `kid`, and the JWKS cache has an RSA signing key of at least
   2048 bits for it. See "The key set" below for when the cache refreshes. If no
   refresh has ever succeeded, or none has for 24 hours, the answer is 503, never
   a pass.
5. The signature verifies under that key.
6. `iss` equals our issuer and `aud` (a string, or an array) contains our public
   URL. Both are exact string compares after trimming one trailing `/`.
7. `exp` is present and not past, with 60 seconds of leeway; `nbf`, if present, is
   not in the future, with the same leeway.
8. **`email` equals this daemon's account email**, trimmed and ASCII-case-insensitive.
   A token with no `email` claim is refused.

Every refusal after step 1 is a 401 with `error="invalid_token"` added to the
challenge, and an empty body, with one exception: when no key set can be trusted
(step 4: never fetched, or no successful fetch for 24 hours) the answer is a 503
with no challenge, because the token was never judged. A token that passes bounds
its response (`auth.rs:response_lifetime`): the body is ended when the token's `exp`
plus the leeway passes, so a long-lived GET event stream closes and the client
must come back with a fresh token. Two clamps keep that bound from misfiring. A
FLOOR of 30 seconds lets a response opened by a token accepted late in its leeway
finish, rather than return an empty body after the tool has already run. A CAP of
24 hours applies whatever `exp` says, and the arithmetic is total, so an absurd
`exp` (`1e300`) gets a normal response instead of a panicked connection. Nothing an
agent receives outlives the credential that asked for it by more than the floor.
The library verifies the signature and nothing else:
every claim check is ours, because its defaults check `aud` and `iss` only when
present and leave `nbf` off.

**The key set** (`auth.rs:JwksClient`). Fetched lazily from `<issuer>/oauth2/jwks`
(or the override), with no redirects followed (a redirect is a failed fetch, since
the https check covered only the configured URL) and the body read in chunks that
stop at 256 KiB. A fetch happens when a `kid` is unknown or when the cached set is
older than one hour, and never more than once per 60 seconds whatever arrives: a
spray of random `kid`s cannot drive outbound fetches, and since a throttled request
does not extend the window, a newly published key is usable at most 60 seconds
after the last fetch. A successful fetch REPLACES the set, so a key the issuer
withdraws stops verifying within about an hour, and a JWKS with no usable keys
empties the set. A failed fetch keeps the old set, so an issuer blip does not log
every agent out, but only until 24 hours have passed without one success; then
nothing is trusted (503). A fresh cached key is never delayed by a refresh in
flight.

**Step 8 is the tenant boundary.** One wildcard resource indicator,
`https://*.passband.email/mcp`, covers the whole fleet, so any Passband user can
obtain a correctly signed, unexpired token whose `aud` names someone else's tenant.
`aud` says "this token is for a Passband daemon"; only `email` says whose. Without
step 8, tenant A's owner could read tenant B's mail with a token AuthKit issued
quite properly.

**The `email` claim exists only because the operator puts it there.** A WorkOS
Connect access token carries `iss`, `aud`, `sub`, `client_id`, `org_id`, `sid`,
`scope`, `jti`, `exp` and `iat`, and no `email`. The claim appears only when the
environment's JWT template is set to `{"email": {{ user.email }}}`. Without the
template every token is refused as `missing_email`, which is the fail-closed
direction: a forgotten template locks agents out, it does not let them in. `sub`
is WorkOS's own user id, not anything the control plane chose, so it cannot stand
in for the mailbox.

**The doors do not share credentials.** `/mcp` never accepts an `sqd_` device
token or the `SQUELCH_API_TOKEN` master token: neither is a JWT, so step 3 refuses
it, and nothing on the agent path consults the device-token table. `/client/*`
never accepts an agent JWT: the bearer check there knows only the master token and
issued device tokens. The two routers are merged side by side in
`squelchd:build_serve_router`, so neither inherits the other's auth, CORS or body
limits. Both directions are pinned against the real combined router in squelchd's
tests. The agent door still has no path to the write credential, and sealed
material stays structurally absent from it under §4: nothing here touches tool
logic.

**Rollout fails closed.** The warden never renders `SQUELCH_MCP_ALLOWED_HOSTS`. The
public hostname reaches the rmcp Host allow-list only from `agent_door_router`, and
only in OAuth mode. So an older daemon image behind a newly published `/mcp`
Ingress path answers 403 from the Host guard rather than serving mail without a
token.

**Logging.** A refusal is counted under one closed category (`auth.rs:Refusal`,
e.g. `wrong_mailbox`, `expired`), and only the counts are ever written. No token, no claim value and no email
address reaches a log line or a response; a misconfiguration names the setting,
not its value. Both binaries' startup lines (`squelchd serve` and the standalone
`squelch-mcp --http`) print the same mode text (`AgentDoorAuth::describe`): the
mode and, in OAuth mode, the public URL, which the metadata publishes anyway.

**Accepted costs.**
- **WorkOS can mint a token that reads a mailbox.** AuthKit holds the signing key,
  so a compromise of WorkOS (or of our WorkOS API key, which completes the identity
  hop) can produce a token that passes every step above. That is the price of not
  running our own authorization server.
- **Revocation is a WorkOS-side operator action, not instant, and not ours.** The
  daemon checks no revocation list. Access tokens are short-lived, but AuthKit
  also issues refresh tokens (`offline_access` is among the standard scopes a
  dynamically registered client gets), so an approved agent keeps access for as
  long as its grant lives on the WorkOS side. Ending that access means revoking
  the grant in WorkOS, which today is something an operator does. The
  access-token lifetime only bounds how long a revoked grant's last token keeps
  working, and the stream bound above makes that true for open streams too.

**The phishing shape that remains.** An attacker can register their own MCP client
with AuthKit, start a flow, and send the victim the resulting login link. If the
victim signs in and approves, the attacker's client gets a token for the victim's
mailbox. Client names under dynamic registration are chosen by whoever registers
the client and are not verified by anyone, so "Claude" on a consent screen proves
nothing. The defence is two screens the victim must get past: our control plane's
own confirmation page, which names the mailbox and is shown after Google sign-in
and before anything is sent to WorkOS (`/agent/auth`, then Google, then confirm,
then the POST that completes the hop), and then AuthKit's consent screen naming
the client. Both depend on the victim reading them.

**Not proxied on purpose.** The daemon does not serve
`/.well-known/oauth-authorization-server`. Clients that predate the 2025-06-18
authorization spec and expect the resource server to also be the authorization
server will not work, and that is accepted rather than worked around.

**Browser-based MCP clients are not supported.** `/mcp` sends no CORS headers, by
choice for now, so a page in a browser cannot call it. The protected-resource
metadata is CORS-open because it is public. Server-side and native clients, which
is every agent we document, are unaffected.

**Refusal logging is rate-bounded.** Refusals are unauthenticated, so they are
counted per category and written as at most one summary line per minute
(`auth.rs:RefusalLog`), never one line per request.

**What a maintainer must not break.**
- Do not drop or weaken step 8, and never swap the binding to `sub`: `sub` is
  WorkOS's user id, and `email` (from the JWT template) is the claim this daemon
  checks against its own mailbox.
- Do not remove the stream bound or the key-set maximum age and ceiling.
- Do not let a misconfiguration fall back to Open, and do not mount the agent door
  any way other than `agent_door_router`.
- Do not render `SQUELCH_MCP_ALLOWED_HOSTS` from the warden.
- Do not let the agent door accept a human door credential, or the human door an
  agent JWT.
- Do not log anything from a token, including on the 401 path.

## 9. Google Calendar

**Invariant.** Calendar data (event titles, places, descriptions, organizers,
guests' addresses, names and answers, links, calendar names) is human-door data: served on `GET /client/timeline` (and
in the answer to the user's own write) and nowhere else, never in a log line,
never in a triage prompt. Nothing calendar-related is on `/mcp` or
`/client/agent/*`. The daemon writes to a calendar only when the user creates,
edits or deletes one event from the human door; nothing writes on its own. Google
emails an event's guests only when such a write says `notify: true`; no other
input, default or code path can ask for it.

**Enforcement.**
- **Three tables, human door only.** `calendar_calendars`, `calendar_events` and
  `calendar_event_guests` (an event's attendees, deleted with it by a trigger)
  (`squelch-core/src/store/sqlite/calendar.rs`) are written by the sync loop
  (`squelch-core/src/gcal/sync.rs`) and, one row at a time after Google accepted
  it, by the user's own write (`squelch-api/src/calendar_write.rs`); they are read
  only by `handlers.rs:get_timeline` and those write handlers.
  Every query is scoped by `account_id`. A log line names a calendar by its
  position in the pass, never its id, which is usually an address.
- **Laundered and capped** (`gcal/mod.rs:read_event`). A shared calendar is written
  by other people, so its text is treated like mail: titles and places lose control,
  bidi and invisible characters and stop at 200 characters, a meeting link loses
  its query, an organizer is kept only when it is an address, ids are bounded. At
  most 25 calendars, 5000 events per calendar (the nearest to now kept), 40 pages of
  250 per calendar per pass, 8 MB per response, one listing of 5000. Google is asked
  for less in the first place: a `fields` mask names only what is read, and
  `maxAttendees=100` bounds the guest list, so an invitation anyone can send never
  delivers attachments, conference data, a guest's comment or more than 100 guests
  to the daemon. Of each guest only the address (kept only when it is one), the
  name (laundered like a title), the answer and two flags are stored; past 100,
  Google sends the user's entry alone and the event is marked truncated.
  Its description is read (the app shows it): markup becomes text, each line is
  laundered like a place, and it stops at 2000 characters.
- **`html_link` is an allow-list** (`gcal/mod.rs:google_link`): https, a host that is
  `google.com` or ends in `.google.com`, no userinfo, at most 2048 characters.
  Anything else is dropped, because the client opens it.
- **No redirects, no bodies in errors** (`gcal/client.rs`). The client refuses
  redirects (every request carries a bearer token) and reduces a failure to its
  kind (`auth`, `quota`, `unavailable`).
- **Calendar failures stay out of Gmail.** The loop is its own task; no failure of
  it touches Gmail health or sync.
- **Sealed mail feeds no ghost** (`store/sqlite/ghosts.rs:ELIGIBLE`, `may_feed`).
  A calendar ghost is built only from a message that is not sent, not spam and
  not sealed, where sealed is what the Auth page's `sealed_messages` reads today:
  a stored decision naming an auth kind (a sign-in alert, a login code), or an
  access review that is not `allowed` (`restricted`, or still `pending` after the
  message changed). The retired `triage.sensitivity` column is NOT the test: the
  agent pipeline no longer writes it. The same predicate runs at reconcile, at
  rebuild and in SQL at listing time, so a message that becomes sealed after its
  ghost was built stops serving it at once; a decision that does not parse feeds
  nothing.

**The user's own writes** (`squelch-api/src/calendar_write.rs`). `POST
/client/calendar/events`, `PATCH` and `DELETE /client/calendar/events/{calendar_id}/{id}`,
on the human door only, bearer authed, `no-store`. The order is fixed: the grant,
the body (title 1 to 200 characters, refused rather than cut; one precision; the
end after the start; place and description cut to 200 and 2000; all three lose
control and invisible characters but are otherwise sent as typed, so a link keeps
its query at Google, while the stored copy is Google's answer laundered by
`read_event`),
the calendar (one being read, on which the user is `owner` or `writer`), the
stored event and the edit guard (`gcal/mod.rs:SyncedEvent::editable`: the user's
own event, by `organizer.self`, no organizer named at all, or the mailbox's own
address; it FAILS CLOSED, so an organizer Google names whose address does not
read, stored as `organizer_unreadable`, and an event whose stored guest list
marks someone other than this calendar as organizer, are somebody else's), and
only then Google, through the same `CalendarCredentials` resolver the sync uses,
with `sendUpdates=none` unless the request asked otherwise (below). Only Google's success
changes a row, and what is stored is Google's answer read by the sync's own
`read_event`. A write is never retried. A refusal Google answered is a 502; a
write that went out and got no usable answer (a timeout or a lost connection after
sending, a 2xx body that does not read: `CallError::Uncertain`) is a 504 saying
Google may have made the change, kicks a sync and stores nothing, so the client
refreshes before offering to send again (a "tell guests" is not repeated on a
guess). A write Google accepted whose answer the daemon could not read or store
is that same 504, never a refusal. Every error body names its kind in a `code`
from a closed list (docs/CALENDAR.md section 8, "What a refusal says"), and the
app gives a pane's fields back ONLY on a code that says nothing reached Google
(or Google refused it), with that code's own status; anything else, including
no code, an unknown code, any 500 and a bare 502, freezes the pane. The whole write (token refresh, guest reread, write) runs under one 35
second deadline, below the app's 45 second wait; past it the answer is the same
504 and kick, and the app treats a wait of its own that runs out, or a
connection lost mid-request, as that 504 too. A create may name its own event id
(base32hex, 5 to 1024 characters, else a 400); the daemon reads that id before
inserting. An event already there (or found after a 409 `duplicate` from Google)
is answered, with no insert and no second invitation (audited `ok:existing ...
notified=false`), ONLY when it is the event the create would make: same
calendar, laundered title, location and notes, start, end, all-day flag and
guest set. Anything else under the id is a 409 with no write
(`rejected:id_in_use`), so an id cannot be used to claim an event with another
title, place, notes, time or guest list than the request described. What it does
NOT compare is `notify`: Google's event does not say whether its guests were
mailed, so a create asking for invitations could be answered by an event made
without them. The app never sends that, because a create is resent only from a
frozen pane and only as the exact body first sent, so a resend never changes
`notify`; a different client could, and would be told 200 with nobody invited
(the audit row says `notified=false`). A second
create with an id whose first create is still in flight (an in-memory set of
account, calendar and id, released however the first ends) is a 409 before
Google hears it (`rejected:in_flight`). This makes a duplicate from a retried
create unlikely, not impossible: Google does not guarantee to catch a taken id
at insert time and a read may lag one just made, and the read is per calendar.
The app closes the rest from its side: after an uncertain save it freezes the
pane and can only send the same body again (docs/CALENDAR.md section 5). A delete
Google answers 410 found the event gone and removes the local row; a 404 does the
same only on the primary calendar or on a calendar `calendarList.get` still
lists, not deleted, with an `accessRole` of `owner` or `writer`; otherwise it is
a 410 "that calendar is no longer available to you" that leaves the row for the
kicked sync (Google answers 404 for a calendar the user lost, and for an event
they can no longer see). A calendar list read that fails is a 502 and deletes
nothing. The app refuses a "delete and tell guests" whose calendar may no
longer tell them rather than deleting silently. Event and calendar ids of `.` or `..` (or empty) are refused at read
and again when the URL is built, because a URL path would read them as "here"
and "up one". Error bodies are fixed strings that never
echo the request; logs carry the action and an error kind, never an id. Every
attempt writes one audit row (`calendar_event_create` / `_edit` / `_delete`,
actor `client-api`, target `calendar:<calendar_id>/<event_id>`, detail `ok` or the
reason), never a title, place, description or guest's address. The target DOES
carry the calendar id, and a calendar id can be an address: the primary calendar
is stored as `primary`, but a personal calendar someone shared with the user is
named by its owner's address. Audit rows are human-door data like the rest. The
row is written even when the client disconnects mid-request: each write's checks,
Google call and audit run in a task of their own that the handler awaits, so
dropping the request does not cancel them. The `calendar:` prefix keeps the
audit listing's join to `messages` (a numeric target) from matching a calendar id
that starts with digits. The in-app assistant has no tool that reaches any of
these routes (see "The assistant's calendar tool" below).

**Guests: a human-door route can make Google email third parties**
(docs/CALENDAR.md §8, "Guests, invited by Google"). Since wave 5 the three write
routes can carry guests, and with `notify: true` Google Calendar sends each guest
its own invitation, update or cancellation. Passband writes none of that mail and
sends none through Gmail, but the effect is the same class as sending mail: it
reaches people outside the account and cannot be undone. What gates it:

- **One switch, default off.** `sendUpdates` is `all` only when the request's
  `notify` is the JSON value `true` (`?notify=true` on a delete, given once).
  Absent or `false` is `none`; any other value is a 400, not a guess
  (`gcal/write.rs:notify`, `notify_query`). The client type that reaches the URL
  (`gcal/client.rs:Notify`) has two values and defaults to the silent one, and
  `sendUpdates` is always named, never left to Google's default.
- **The human door alone.** The three routes are bearer authed on `/client`; no
  agent route or MCP tool can name a guest or set `notify`, and nothing the
  daemon does on its own (the sync, triage, ghosts) calls a write. The
  assistant's one calendar tool can put addresses in the event pane's guests
  line, where the user sees them as pills; it cannot send the request, and
  `notify` is decided only by which save control the user then presses.
- **Whose calendar.** Guests only on the user's own calendar (the primary one, or
  one they are `owner` of): `gcal/mod.rs:may_invite`. A shared calendar the user
  can write to cannot be made to invite anyone, nor to mail an update or a
  cancellation: `notify: true` on an edit or a delete there is a 400
  (`calendar_write.rs:refuse_notify_unless_own`, "guests can only be told about
  changes on your own calendar"), refused before Google hears anything. The
  client offers only the silent controls on such a calendar, with one line
  saying why.
- **Whose event.** An existing event's guest list changes only when the event is
  the user's own, the user organizes it, and the stored list is whole
  (`SyncedEvent::guest_editable`). Google replaces the attendee array on a patch,
  so the daemon never sends a list built from a truncated one: it rereads the
  event's attendees from Google inside the request, refuses if that read is
  truncated or holds a guest with no address, and hands every kept guest back
  exactly as read. An edit that names no `guests` sends no `attendees` key.
- **What a guest can be.** At most 50 per request, each one plain ASCII
  `local@domain` (no display name, quoting, comment, whitespace or control
  character), no duplicates, never the owner. The daemon sets no other attendee
  field and no invitation text; the title, place and description Google puts in
  its mail are the event's own, typed by the user.
- **What is audited.** Every attempt's row carries ` guests=<n>
  notified=<true|false>` after its outcome, on failures too (a write that timed
  out may have gone through, and is audited `failed:uncertain`), even when the
  app disconnects mid-request, and never a guest's address. Guests' addresses are in no
  log line and no error body.

What this does NOT cover: a client that sends `notify: true` when the user did
not mean it (the client's controls must say what they send, and offer it only
where the daemon would accept it; the daemon cannot tell intent), and Google's own behaviour with `sendUpdates=none`, which the reference
hedges ("some emails might still be sent") and which has not been tested against
a real account.

**The assistant's calendar tool (client, wave 6).** The embedded assistant
(§6) has one calendar tool, `create_calendar_event`, and it is a draft, not a
write (`passband/Sources/Passband/Lib/EventProposal.swift`,
`AgentTools.createCalendarEvent`).
- **It calls no route.** The tool reads the model's proposal, validates it with
  the event pane's own rules, and opens the pane on the Calendar page filled
  in. The create route is reached only by the user pressing the pane's save
  control, with the same body rules, create id and uncertain-write handling as
  an event they typed. So a prompt injection in mail the assistant read can at
  worst put a filled-in pane in front of the user. It cannot add an event or
  cause an invitation BY ITSELF. Exactly: an event is added, or an invitation
  sent, only by (a) a click on the pane's own save control, or (b) `↵` or
  `⌘↵` pressed after the user has clicked or typed inside that pane
  (`EventProposals.keysMayAct`; `↵` never saves a draft with guests in any
  case). What the claim does NOT cover is the user's own judgement: someone
  who clicks "add and invite N" on a pane whose title, place and guests an
  injected email chose has added it. The pane shows all of them whole
  (below), which is the whole of that defence.
- **It never takes the screen or the keyboard from somebody**
  (`EventProposals.landing`, `CalendarStore.proposeEvent`). A run goes on
  after the assistant's bar is closed (a citation clicked, "edit in
  composer"), so a tool call can land while the user is doing something else.
  The proposal is REFUSED, with a fixed sentence, unless the assistant's bar
  is open at that moment, and while a composer or an inline reply holds
  anything typed: mail being written is never closed for it. An email being
  read is not closed either; the pane is then parked, whole, and shown when
  the user goes to the Calendar page. The pane it opens never takes the
  caret, and `↵` and `⌘↵` do nothing in it (a toast says to click add) until
  the user has clicked or typed in it, so a key typed ahead, or meant for the
  composer or the reader that was on screen, cannot add or invite. Modelled
  on the agent door's `open_draft` (`ComposeState.keyboardSendLocked`). That
  path in turn treats an event pane holding anything (typed, parked, or with
  a write out or in doubt) as something being written: the agent's draft
  waits in drafts and Passband does not come forward over it, nor for a
  draft the composer did not actually take (`EventPaneTravel.heldByUser`,
  `agentDraftMayComeForward`).
- **The title and the place are the only text the assistant supplies.** Each
  is one line of at most 200 characters (the daemon's own caps, and what the
  pane's one-line fields show whole); longer, or with a line break or a tab
  in it, the call is refused, not trimmed (`EventProposals.oneLine`). The
  tool has no notes or description parameter, and `EventProposals.read`
  refuses a call that carries one under any of the names a model reaches for
  (`notes`, `note`, `description`, `body`, `details`, `agenda`, `summary`,
  `message`), in any case. Any OTHER field the tool does not list is refused
  by name too ("unknown field"), so nothing is dropped in silence and the
  model is told rather than left to believe something was saved. The reason
  is the guests: Google emails an event's description and place to them, and
  model-written prose must never reach other people through Passband. The
  notes line of the pane it opens is always empty.
- **It never invites.** Proposed guests are shown as guest pills. Google emails
  them only if the user presses the control labelled "add and invite N"; plain
  Enter does not save an event with guests (§ "Guests" above).
- **It does not read the calendar.** No event reaches the model. A proposal
  names a calendar or leaves it to the primary; a name that matches no writable
  calendar is refused with a fixed sentence that lists none of them. The one
  thing the tool may ask the daemon is the page's own timeline read
  (`refreshCalendar`), once, when the client holds no feed yet (the Calendar
  page has not been opened this session): the answer stays in the store and
  none of it goes to the model. A feed that still is not there is "could not
  reach the calendar", never "no calendar".
- **It never replaces the user's work.** A pane holding anything (typed, or an
  earlier proposal, on screen or parked) refuses the tool with a fixed sentence
  the model relays.
- **It follows the page.** Offered only on the Mac and only while
  `calendarPageAvailable`; with the flag off the tool definitions and system
  prompt sent to the model are the ones from before the tool existed, and a
  call naming it is answered as any unknown tool. No edit or delete tool.
  Nothing was added to `/mcp`, `/client/agent/*` or the relay.

**Who a name is: `GET /client/calendar/people` (wave 6).** The event pane reads
"with steven" out of a new event's title (a deterministic parser in the client,
no model) and asks the daemon who that most likely is, to SUGGEST a guest.
- Behind `require_calendar` with the other calendar routes (404 with the flag
  off), bearer authed on the human door, `no-store`, at most three answers. Not
  on the agent door, `/mcp` or the assistant relay.
- **A candidate is somebody the user has written to** (`contacts`, `sent_count >
  0`), never the account's own address. Receiving mail from someone never makes
  them a candidate, so mail under a matching display name cannot plant a guest.
- **A list or a help desk is not a person.** A candidate with no name from the
  user's own Sent mail is named by the newest From name on its readable mail,
  and a group or a desk the user once wrote to sends as `"Steven Ng via Team"`
  or `"Steven from Acme"`. So an address with no stored name is not offered AT
  ALL when any of its readable mail carries `List-Unsubscribe` or a From name
  of that shape (a later word that is "via", "from" or "at";
  `contacts.rs::list_shaped`). It is offered only under a name the user's own
  Sent mail gave it. What this does not catch: a relay that rewrites From to
  the bare poster's name and sends no `List-Unsubscribe`.
- **Ranking reads mail only where mail may be read.** Inbound mail counts toward
  "most recently exchanged" only when it is not sent, not spam, not sealed (the
  ghosts' `ELIGIBLE` test plus the legacy column, never `triage.sensitivity`
  alone) and passed sender authentication (`auth_pass = 1`; never evaluated
  reads as no).
- **Nothing is added by the answer.** The client draws it as a dashed
  suggestion with a checkmark and an x; it is not in the draft's guests, does
  not change the save control and is never sent until the user accepts it, and
  then only as any typed guest would be.
- A name under two characters (empty included) is a 400 with a fixed body and
  never reaches the store: one letter matches a large part of anybody's
  contacts, and every match is a probe of that person's mail under the
  store's one mutex. Over-long (more than 64 characters) or `@`-bearing input
  answers an empty list. The input is never echoed. The probes are index
  seeks on one sender with no sort (`MAX(received_at)`, or one unsorted pass
  for a contact with no stored name).

**What the event pane sends to Apple (client).** The location line of the
client's event pane suggests places from Apple's MapKit
(`MKLocalSearchCompleter`, then `MKLocalSearch` for the one picked). The text
typed into that one field leaves the device for Apple, from the client and
only once it is two characters long; the title, the times, the notes, the
calendar and the account do not, the daemon is not involved, and no location
permission is requested. Google's Places API is not used.

**The token is the union; the slot is the boundary.** Self-host
`squelchd auth --calendar` stores its token in its own credential slot
(`CredentialKind::Calendar`). Google unions grants per Cloud project and the consent
asks with `include_granted_scopes`, so that token carries EVERYTHING this OAuth
client was granted for the account, Gmail scopes included (`gmail.modify` and
`gmail.send` if `squelchd auth --write` was ever run). Asking for less cannot
narrow it; only a separate Cloud project could. What keeps calendar code away from
mail is the slot and the code paths: the calendar loop loads only the Calendar slot
(or, without one, the Read slot when it records `calendar.events`) and only calls
the Calendar API, and no mail path loads the Calendar slot. Hosted is unchanged: the
control plane seals ONE token into the Read and Write slots, as before, and a
calendar grant adds `calendar.events` and `calendarlist.readonly` to that one token.

**The loopback consent endpoint (self-host only).** `POST /client/calendar/connect`
(human door, bearer authed) lets the app's Connect button grant calendar on a
daemon with no control plane. A daemon that knows a console origin answers 409
and starts nothing; one with no OAuth client answers 409 too. Otherwise it mints a
fresh `state` and PKCE pair through `squelch_core::auth::LoopbackConsent`, the
builder `squelchd auth --calendar` uses, so the URL is the CLI's (calendar scopes,
`access_type=offline`, `prompt=consent`, `include_granted_scopes=true`,
`login_hint` set to the configured mailbox). The redirect lands on a listener bound
to `127.0.0.1` only (port 8847, or `SQUELCH_AUTH_PORT`), which refuses non-loopback
peers and serves one route, `/`. One consent is pending per daemon: a new call
replaces it, a matching redirect consumes it, it expires after ten minutes, and
the listener stops when nothing is pending. A redirect is redeemed only when its
`state` matches the pending one in constant time; a wrong or stale `state` gets a
plain page and changes nothing. The code then goes through the same guarded
exchange as every other consent: the grant must include `calendar.events`, and
Google's profile must name the configured mailbox (case-insensitive). Any refusal
stores nothing and shows a page that names no address. A good consent is stored
in the Calendar slot through the configured backend (keyring on macOS, file
elsewhere) by the same call the CLI makes, the `granted` cache is dropped, and the
calendar sync is kicked. Pages carry no scripts and no links, under
`default-src 'none'`. Logs name the event kind only, never the code, the `state`,
a token or an address. Anything on this machine can reach the listener while it is
up, but a code it delivers is useless without the PKCE verifier held in the
daemon, and only the redirect carrying the pending `state` is redeemed.

**Credentials file writes.** The calendar resolver is a third writer of the
credentials file beside the Gmail Read and Write stores, so every write goes
through one read-modify-write (`credentials.rs:update_credentials_file`) under a
process-wide mutex and an advisory lock on `<credentials file>.lock` (owner-only),
which also covers `squelchd auth` run beside a running daemon. A writer waits at
most 10 seconds for the lock and then fails like any other credential write. Each
write has its own temp name; temp files a crashed writer left are removed after a
minute.

**What a maintainer must not break.**
- Do not add a calendar route to the agent door or MCP, or put calendar text in a
  triage prompt, without a decision recorded in docs/CALENDAR.md §10.
- Do not log an event's title, place, description, organizer, attendee, link or a
  calendar id.
- Do not change a local calendar row before Google accepted the write, retry a
  write, or audit a write's text or a guest's address.
- Do not send `sendUpdates=all` from anything but a request whose `notify` is
  literally `true`, give `Notify` another default or a third value, or let a write
  leave `sendUpdates` off the URL.
- Do not build an `attendees` array from stored rows, from a truncated list, or for
  an edit that did not name `guests`; do not widen `may_invite` past the user's own
  calendars, or accept `notify: true` on a calendar it refuses; do not let
  anything derived from mail set guests or `notify`.
- Do not read an organizer that is present but unreadable as "no organizer"; the
  edit guard fails closed.
- Do not report a write whose answer was lost as refused (it may have happened),
  or run a write's Google call and audit inside the request future alone.
- Do not gate ghosts on `triage.sensitivity`; use the sealed test
  `sealed_messages` uses.
- Do not give the assistant's calendar tool a notes or description parameter,
  or let anything it supplies but the title, the place and the times reach an
  event's body; do not trim an over-long title or place, or drop a field it
  does not know, in silence.
- Do not let the assistant's calendar tool call a write route, set `notify`,
  skip the pane, replace a pane that holds anything, or be offered while the
  Calendar page is not; do not hand the model an event or a list of calendars.
- Do not let a proposed pane take the caret, act on `↵` or `⌘↵` before the
  user has clicked or typed in it, open while the assistant's bar is closed,
  or close mail being written or an email being read. Do not let an agent's
  `open_draft` bring Passband forward over an event pane that holds anything.
- Do not offer a contact with no stored name under a list-shaped From name.
- Do not let `/client/calendar/people` return somebody the user has not written
  to, read sealed, spam or auth-failed mail for its ranking, or move to the
  agent door.
- Do not let a mail path load the Calendar slot, or the calendar loop load the
  Write slot: the token in either can do more than its job.
- Do not loosen `google_link`, drop the `fields` mask or `maxAttendees`, or ask
  for more of an attendee than the mask names in a request whose answer is stored.
- Do not write the credentials file except through `update_credentials_file`.
