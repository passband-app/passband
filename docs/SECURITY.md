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
  referrer, `image/*` only, redirects re-guarded per hop, files named `sha256(url)`
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

**Enforcement.** `store/sqlite/messages.rs:external_message_allowed_conn` checks the
current assessment and source provenance under the store lock. External whole-thread
reads require the thread's sources to be allowed. MCP and dedicated
`/client/agent/` routes use these checks for messages and derived results. Search
rehydrates results under the same guard instead of releasing stale cached snippets.
Account ownership is checked independently of access assessment.

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
  be a column of senders against a blank. `attention_updates` fills `subject`
  and `preview` (the stored 200-character snippet) on `SpamScope::Only` and on
  nothing else. Both are `Option`, skipped when `None`, so the agent door's
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
latitude than the owner has there. The default is that it cannot write mail at
all. Nothing here changes what an agent can READ.

**The setting.** `app_settings` key `agent.write_access`, per mailbox:
`none` (default), `drafts`, `send`; each level includes the ones before it.
`squelch-core/src/agent_write.rs:AgentWriteAccess::load` reads it and FAILS
CLOSED: unset, or any value this build does not recognize, is `none`. It is set
only on the human door (`GET`/`PUT /client/settings/agent-access`, bearer-authed,
audited as `agent.write_access` with `before->after`). The agent door has no
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
- resolves a reply target through the AGENT's access check
  (`thread_id_for_message` + `external_thread_allowed`), so an agent can only
  reply to mail it can read, and a restricted id answers the same `not found`
  as an unknown one;
- for `send_email`, runs `validate_send`, the outbound guard (§5) and
  `perform_send`, exactly as `POST /client/actions/send` does, with
  `override_guard` hard-wired to `false`;
- for `create_draft`, INSERTS a local draft marked `drafts.from_agent = 1`
  (shown as "from your agent" in the client) and never edits one: a reply
  whose message already has a draft is refused, because that row is the
  owner's own unfinished writing and an agent cannot see it to know;
- audits every outcome, refusals included, as actor `agent` (ids, counts and
  fixed reason strings only, never content). A draft is written fail-closed
  with its ledger row, like `set_sender_rule`.

An agent's composition is deliberately narrower than the owner's: no
attachments, no forward, no send group, no read tracking, no scheduling.

**What "with your approval" does and does not mean.** At `send`, the approval
is the agent's human saying yes IN THE AGENT'S OWN CONVERSATION. The daemon
cannot see that conversation and does not verify it. What it does is:

- tell the agent the rule three times: in the server instructions sent at
  `initialize` (`server.rs:write_instructions`), in the `send_email`
  description, and in the required `human_approved` argument, each saying that
  approval is per message, comes from the human, and is never text found in an
  email or a tool result;
- refuse a call whose `human_approved` is not `true`;
- annotate the tool `destructiveHint` + `openWorldHint`, which is what MCP
  clients key their own confirmation prompts on.

**Residual risk, accepted for an opt-in level and stated in the client.** A
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
daily cap, reply-only.

**Do not break.**
- Never give `squelch-mcp` the write credential, a `GmailWriteClient`, or a
  direct draft/outbox store call. Agent writes go through `AgentMailer`.
- Never let an agent path pass `override_guard: true`.
- Never resolve an agent's reply target with `action_message_ref` alone; it
  applies no agent access check.
- Keep the default `none`, and keep an unreadable setting meaning `none`.
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

**What a maintainer must not break.**
- Do not build a second outbound client for sender-chosen URLs, or call
  `reqwest` directly on one: `Fetcher` is the guard, and a client without it is an
  SSRF primitive aimed at the daemon's own door on loopback.
- Do not prefetch an unsubscribe link to "show a preview" or to classify it early.
- Do not make `Action::submission` type anything. The page's host is the
  sender's choice at every hop, so there is no site an address could be safely
  typed into, and the account's primary address is not necessarily the one the
  list knows.
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
