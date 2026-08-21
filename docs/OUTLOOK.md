# Outlook support: design + implementation plan

Status: PLANNED (this doc is the spec; nothing below is implemented).
Scope: self-host daemon first (P0–P4). The hosted signup funnel (squelch-control)
is P5 and gets its own design pass later.

## Summary

Squelch's expensive machinery is already provider-blind: ingest takes raw RFC822
bytes (`squelch-core/src/sync/ingest.rs`), triage/sealing/extractors/search
never see the transport, and ~1,100 of `gmail_write.rs`'s 1,316 non-test lines
are RFC822 construction that ports as-is. One daemon serves one account, so
provider is a **config-level choice per daemon** — no runtime multi-provider
dimension, and the client's existing multi-daemon model covers Gmail + Outlook
side by side for free.

The work is two trait extractions plus a Microsoft Graph implementation of each:

- `MailSource` (read transport) in `squelch-core/src/mail/` — the ~1,100
  Gmail-specific lines of `sync/mod.rs` move behind it.
- `MailWriter` (write ops) in `squelch-api/src/write/` — the ~200 API-call
  lines of `gmail_write.rs` move behind it; the MIME builders stay shared.

## Decisions (made; revisit only if implementation contradicts them)

1. **Provider is per-daemon config.** `provider = "gmail" | "outlook"` on the
   account config, default `"gmail"`. Everything downstream (endpoints, scopes,
   source/writer construction, refresh URL) derives from it.
2. **Keep the `gmail_msg_id` column name.** It means "provider-native message
   id" from now on; a schema comment says so. Renaming touches ~17 files for
   zero behavior. Same for the Rust field name. Cleanup ticket later if it
   grates.
3. **Graph uses immutable ids everywhere.** `Prefer: IdType="ImmutableId"` on
   EVERY Graph request, from the very first backfill call. Without it, a move
   (archive, trash) changes the message id and orphans our
   `UNIQUE(account_id, gmail_msg_id)` row. This is non-negotiable and must be
   baked into the one place GraphSource builds requests, not sprinkled.
4. **Send is draft-then-send**, not `sendMail`. `POST /me/sendMail` with MIME
   returns 202 and NO id — the api's post-send echo and audit ids die. Instead:
   `POST /me/messages` with `Content-Type: text/plain`, body = base64 MIME
   (creates a draft, returns the message with its immutable id), then
   `POST /me/messages/{id}/send`. Threading needs no thread param: the
   In-Reply-To/References headers already in our MIME do it; `GraphWriter`
   ignores the `thread_id` argument.
5. **Archive/trash are folder moves.** `POST /me/messages/{id}/move` with
   `destinationId` = well-known folder `"archive"` / `"deleteditems"`. The
   `MailWriter` trait is shaped around intent (archive/trash), never labels.
6. **The `/client/actions/label` endpoint** maps only `UNREAD` add/remove on
   Outlook (`PATCH /me/messages/{id}` `{"isRead": true|false}`). Any other
   label id → 400 `"labels are a Gmail concept; this account is Outlook"`.
   Gmail behavior unchanged.
7. **Auth stays on the `oauth2` crate.** Microsoft identity platform v2.0 is
   standard OAuth2 + PKCE. Entra "Mobile and desktop applications" apps are
   PUBLIC clients: no secret exists and sending one is rejected — so
   `OAuthClientConfig.client_secret` becomes `Option<String>`; Google-path
   validation still requires it, Microsoft-path validation requires its
   absence-tolerance (present = config error, to catch a confused operator).
8. **Metrics: reuse the existing counters** (`squelchd_gmail_api_errors_total`
   etc.) for Graph errors in v1. The name is cosmetically wrong for Outlook but
   dashboards reference it; a coordinated rename is separate debt. `GmailErrorKind`
   may be renamed `ProviderErrorKind` internally without touching metric names.
9. **`auth_pass` gets an Outlook trust arm, fixture-first.** Microsoft's
   `Authentication-Results` headers are historically nonstandard (authserv-id
   omitted or varying: consumer outlook.com vs EOP vs the newer `mx.microsoft`).
   Do NOT hardcode guesses: P2 includes capturing real headers from a live
   outlook.com account into test fixtures, then extending
   `extract_auth_pass`/`google_wrote` (generalized to a per-provider
   trusted-writer matcher) against those fixtures. Until that lands, Outlook
   rows get `auth_pass = NULL`, which safely reads as "no bypass".
10. **Self-addressed mail:** Exchange has no Gmail label model — a
    self-addressed message is TWO distinct items (Inbox copy + SentItems copy)
    with different ids. Two rows (visible + neutral sent) is semantically
    correct here. The Gmail INBOX-first/subtract-ids invariant is
    Gmail-specific and moves into `mail/gmail.rs` with the rest of the walk.
11. **Broker (`auth --broker`) and hosted funnel are deferred.** `squelchd auth
    --broker` on an Outlook account → clear error. `auth --export/--import`
    (transfer) IS supported in P1: the blob carries tokens, not provider; the
    daemon's config decides, and `verify_transfer_credential`'s profile check
    is parameterized like everything else.

## Trait seams

### `MailSource` — squelch-core/src/mail/mod.rs

The seam sits above HTTP but below the poll loop's cadence/triage logic,
because *cursor semantics* differ across providers (one mailbox-wide historyId
vs per-folder deltaLinks), not just URLs. `SyncEngine` keeps: poll cadence,
backoff, reminder/stage1/stage2/extract/revisit passes, budgets, embedder
backfill, metrics stamping. The source owns: listing, delta walking, raw
fetching, cursor encoding.

```rust
/// Opaque, provider-encoded sync cursor. Gmail: decimal historyId.
/// Graph: JSON { "inbox": "<deltaLink>", "sent": "<deltaLink>" }.
pub type Cursor = String;

pub struct NewMessageRef {
    pub id: String,          // provider-native message id
    pub is_sent: bool,       // which walk produced it
}

pub struct PollOutcome {
    pub new_messages: Vec<NewMessageRef>,   // inbox refs ordered before sent refs
    pub cursor: Cursor,                     // commit AFTER ingest succeeds
}

pub struct RawMessage {
    pub raw: Vec<u8>,                       // RFC822 bytes
    pub thread_id: Option<String>,          // Gmail threadId / Graph conversationId
    pub internal_date: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait MailSource: Send + Sync {
    /// First run: enumerate the backfill window for both mailboxes AND
    /// establish the initial cursor in one motion (see "cursor gap" below).
    async fn initial_listing(&self, window_days: u32) -> Result<PollOutcome>;

    /// One incremental tick from `cursor`. CoreError::NotFound signals an
    /// expired cursor (Gmail historyId 404 / Graph 410 resyncRequired) and the
    /// engine falls back to `initial_listing`.
    async fn poll_changes(&self, cursor: &Cursor) -> Result<PollOutcome>;

    async fn fetch_raw(&self, id: &str) -> Result<RawMessage>;

    /// To/Cc pairs (addr, display name) for one message — the sent-recipients
    /// backfill. Gmail keeps its synthesize-headers + mail-parser path inside
    /// the impl; Graph maps structured toRecipients/ccRecipients directly.
    async fn fetch_recipients(&self, id: &str) -> Result<Vec<(String, Option<String>)>>;

    /// Whole-Sent-mailbox recipient sweep for the one-time contacts harvest.
    async fn list_sent_recipient_summaries(&self)
        -> Result<Vec<SentRecipientSummary>>; // { sent_at, recipients }

    /// Provider-side inbox unread counts. Graph has message counts only;
    /// its impl returns the same number for both fields.
    async fn unread_counts(&self) -> Result<(u64, u64)>;
}
```

Notes for the implementer:

- `poll_changes` returning refs (not raw bodies) preserves today's shape:
  the engine still calls `fetch_raw_and_ingest`-equivalent per id, sequential,
  and can subtract ids already stored (see Graph isRead-churn note below).
- **Ordering invariant preserved at the trait level**: `new_messages` lists
  inbox refs before sent refs. The Gmail impl additionally subtracts inbox ids
  from the sent batch (its self-addressed dedup); Graph doesn't need to.
- **Cursor commit stays in the engine**, after ingest, exactly like today's
  commit-once-at-max discipline. The trait hands the cursor back; the engine
  persists it.
- 404-on-fetch_raw for a deleted message stays `CoreError::NotFound` — the
  sent-recipients backfill's "" semantics depend on it.

### Cursor storage

`sync_state` stores INTEGERs; Graph deltaLinks are long opaque URLs. Add (via
the idempotent `schema.sql`, no migration machinery needed):

```sql
CREATE TABLE IF NOT EXISTS sync_cursor (
    account_id INTEGER NOT NULL,
    key        TEXT NOT NULL,      -- 'cursor'
    value      TEXT NOT NULL,
    PRIMARY KEY(account_id, key)
);
```

The engine reads/writes the opaque cursor here. **Legacy fallback (required):**
when `sync_cursor` has no row but `sync_state` key `history` does, the Gmail
path adopts the legacy historyId as its cursor and writes it forward — an
existing install must NOT re-backfill after the refactor. Test this explicitly.

### `MailWriter` — squelch-api/src/write/mod.rs

```rust
#[async_trait]
pub trait MailWriter: Send + Sync {
    async fn archive(&self, id: &str) -> Result<(), WriteError>;
    async fn trash(&self, id: &str) -> Result<(), WriteError>;
    /// Gmail: labels verbatim. Outlook: UNREAD only, else WriteError::Invalid.
    async fn modify(&self, id: &str, add: &[String], remove: &[String]) -> Result<(), WriteError>;
    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<SentRef, WriteError>;
    async fn fetch_raw(&self, id: &str) -> Result<FetchedRaw, WriteError>;
    async fn parent_headers(&self, id: &str) -> Result<ParentHeaders, WriteError>;
}
```

`write_client()` in `handlers.rs` returns the provider's impl (provider reaches
`ApiState` from daemon config). Every MIME builder
(`build_reply_rfc822`, `build_forward_rfc822`, `derive_reply_recipients`,
`parse_forwarded_original`, …) stays where it is, shared, untouched.

## Graph endpoint map

All requests: `https://graph.microsoft.com/v1.0`, bearer token,
`Prefer: IdType="ImmutableId"` header, redirects refused, same timeout
discipline as the Gmail client.

| Operation | Gmail today | Graph |
|---|---|---|
| Incremental sync | `history.list` (one cursor, label-filtered ×2) | `GET /me/mailFolders/{inbox\|sentitems}/messages/delta?$select=id,receivedDateTime,isRead` — one deltaLink PER folder; follow `@odata.nextLink`, persist `@odata.deltaLink` |
| Cursor expired | 404 → catch-up | 410 Gone (`resyncRequired`) → `initial_listing`; the 410's `Location` header carries a fresh delta URL |
| Backfill listing | `messages?labelIds=X&q=newer_than:Nd` | The INITIAL delta round IS a full-folder snapshot: run it, ingest items with `receivedDateTime >=` window client-side, keep the deltaLink. Listing + cursor in one motion — no cursor gap, unlike Gmail's list-then-profile |
| Raw RFC822 | `messages/{id}?format=raw` (base64url) | `GET /me/messages/{id}/$value` (raw bytes, NOT base64) |
| internalDate | `internalDate` ms-epoch string | `receivedDateTime` / `sentDateTime` from the delta `$select` (ISO8601) — carry it from the listing to avoid a second metadata GET |
| Thread id | `threadId` | `conversationId` (subject-heuristic grouping on Exchange; acceptable — `fallback_thread_id` exists) |
| Recipients metadata | `format=metadata&metadataHeaders=To,Cc` | `?$select=toRecipients,ccRecipients,sentDateTime` — structured JSON, no RFC2047 |
| Unread counts | `labels/INBOX` messagesUnread/threadsUnread | `GET /me/mailFolders/inbox?$select=unreadItemCount` (message count only; write it to both columns) |
| Archive | modify: remove `INBOX` | `POST /me/messages/{id}/move` `{"destinationId":"archive"}` |
| Trash | modify: add `TRASH` | move → `"deleteditems"` |
| Mark (un)read | modify: `UNREAD` label | `PATCH /me/messages/{id}` `{"isRead": …}` |
| Send | `messages/send` (raw b64 + threadId) → id echo | draft-then-send (decision 4) → id from draft creation |
| Parent headers | `format=metadata`, To/Cc/etc | `?$select=internetMessageHeaders`, filter the same names |
| Profile / identity | `gmail/v1/users/me/profile` | `GET /me` → `mail ?? userPrincipalName` (MSA accounts can have null `mail`; userPrincipalName is the address-shaped fallback — verify live in P1) |
| Throttling | 403-with-reason / 429 | 429 + `Retry-After` (~10k req/10min/mailbox); classify → `Quota` |

**Graph-specific ingest guard (perf, important):** delta rounds surface items
on ANY change, including `isRead` flips from the user reading mail elsewhere.
Without a guard, every read email re-fetches its full MIME every poll. Before
fetching raw, the Graph path (or the engine, generically) checks
`store.has_message(account_id, id)` and skips known ids. Items tagged
`@removed` are ignored (we don't process deletions today). This needs a small
new `Store` method — indexed, exists already via the unique key.

## Auth (P1)

Endpoints: `https://login.microsoftonline.com/common/oauth2/v2.0/authorize` and
`…/token`. `/common` serves both personal (outlook.com/hotmail) and work/school
accounts.

Scope sets (mirror `AuthScopes`):

- Read: `https://graph.microsoft.com/Mail.Read`, `User.Read`, `offline_access`
- Write: `https://graph.microsoft.com/Mail.ReadWrite`,
  `…/Mail.Send`, `User.Read`, `offline_access`

`offline_access` is REQUIRED for a refresh token — Microsoft has no
`access_type=offline` equivalent; omit it and auth "succeeds" with no refresh
token and dies in an hour. Assert its grant like `check_scope_grant` does.

Work items:

- `ProviderEndpoints { auth_url, token_url, profile: ProfileCheck }` threaded
  through `run_auth_flow`, `verify_transfer_credential`, and
  `refresh_stored_token_detailed` (which today hardcodes `GOOGLE_TOKEN_URL` at
  `credentials.rs:157` — every refresh in the daemon goes through there, so
  this is load-bearing, not cosmetic).
- `OAuthClientConfig.client_secret: Option<String>` (decision 7). The oauth2
  crate builds fine without a secret; PKCE already in place.
- Config: a `[microsoft]` client section beside the existing Google one
  (`client_id` only), env override `SQUELCH_MS_CLIENT_ID`, `provider` field on
  the account, provider-aware `require_client()` validation with error text
  naming the right console.
- `refresh_error_message` gets a Microsoft `invalid_grant` arm: the likely
  causes are revocation and the MSA ~90-day-inactive refresh-token expiry, and
  the current message's "publish the OAuth consent screen" advice is
  Google-only and actively misleading for Outlook.
- Loopback redirect works unchanged: Entra native apps accept
  `http://localhost` with any port.
- `auth --broker` on provider=outlook: refuse with a named error (broker's
  `validate.rs` host-pins Google; its MS arm is backlog).

**Entra app registration (operator does once, docs/GETTING-STARTED.md gets a
section):** portal.azure.com → App registrations → New. Supported account
types: "Accounts in any organizational directory and personal Microsoft
accounts". Platform: "Mobile and desktop applications", redirect URI
`http://localhost`. API permissions (delegated): `Mail.Read`, `Mail.ReadWrite`,
`Mail.Send`, `User.Read`, `offline_access`. No admin consent needed for
personal accounts. Copy the Application (client) ID into config. No secret.
Note: unverified-publisher consent warning appears unless the publisher is
MPN-verified — acceptable for self-host, revisit for hosted.

## Phases

Each phase = one reviewable PR-sized wave; commit per verified wave.

### P0 — Extract the seams (pure refactor, zero behavior change)

- `squelch-core/src/mail/{mod.rs,gmail.rs}`: trait + move the Gmail transport
  (`get_json`/`send_get`/`bearer_get`/`token_for_request`, `history_walk*`,
  `catch_up`, `list_message_ids`, harvest/backfill Gmail calls,
  `refresh_inbox_unread`'s fetch, `fetch_profile_history_id`,
  `classify_gmail_status`, `urlencode`, the response structs, `parse_history_id`,
  `history_poll_decision`, `advance_history_cursor`, `subtract_ids`).
  `SyncEngine<S, C>` gains the source; triage passes untouched.
- `squelch-api/src/write/{mod.rs,gmail.rs}`: `MailWriter` + `GmailWriteClient`
  behind it; MIME builders stay in a shared module.
- Cursor storage: `sync_cursor` table + legacy `sync_state('history')` adoption.
- MockGmail tests keep passing UNMODIFIED in assertion content (they may move
  files). The sync-invariant suite (cursor commit-once, INBOX-first ordering,
  self-addressed race, expired-cursor fallback, outage-cannot-starve-reminders)
  is the acceptance gate.
- Acceptance: full workspace green; a live Gmail daemon on this build syncs
  from its existing cursor without re-backfilling.

### P1 — Provider plumbing + Microsoft auth

- `provider` config field; `ProviderEndpoints`; optional client_secret;
  refresh-path parameterization; transfer verify parameterization;
  `squelchd auth` for provider=outlook end-to-end (manual E2E: real
  outlook.com account, read + write consents, token refresh observed).
- Acceptance: `squelchd auth` + `auth --write` complete against a personal
  outlook.com account; stored blobs refresh; Gmail path byte-identical.

### P2 — GraphSource (read)

- `squelch-core/src/mail/graph.rs`: delta walks, initial-listing-as-delta,
  `$value` fetch, ImmutableId preference, has_message skip guard, 410 resync,
  429/Retry-After classification, unread counts, recipients/harvest methods.
- `MockGraph` axum harness mirroring MockGmail: delta pages with nextLink /
  deltaLink, `$value`, folder counts, 410 with Location, 429.
- Port the applicable sync-invariant tests to the Graph twin (two-folder
  cursor commit, resync fallback, isRead-churn no-refetch, removed-items
  ignored, self-addressed = two rows).
- Fixture capture: real outlook.com `Authentication-Results` headers (consumer
  at minimum; EOP if reachable) → `extract_auth_pass` Outlook arm, generalized
  as per-provider trusted-writer matching. Until merged, NULL is safe.
- Acceptance: live E2E — fresh outlook.com daemon backfills, triages, polls;
  mail sent from the Outlook web UI lands via the sentitems delta.

### P3 — GraphWriter (write)

- `squelch-api/src/write/graph.rs`: draft-then-send, move-based archive/trash,
  isRead modify mapping (else 400), `$value` fetch_raw,
  internetMessageHeaders parent_headers. Echo path works via the draft id.
- Reuses the existing write-path test harness pattern (`mock_once`).
- Acceptance: live E2E — archive/trash/reply/forward/send from Passband
  against the Outlook daemon; forward rebuilds the original; audit rows carry
  ids; the sent echo ingests.

### P4 — Periphery

- Daemon advertises `provider` in `/client/stats` (additive). Passband:
  provider-aware copy ("Gmail Trash" → "Trash", AuthView shred text,
  ConnectView self-host copy). Check every new user-facing string against the
  no-em-dashes rule and the closed analytics vocabulary (new analytics strings
  are fatal in debug builds).
- Docs: GETTING-STARTED Outlook section, SECURITY.md note that the two-door
  argument maps to `Mail.Read` vs `Mail.ReadWrite`+`Mail.Send` (and that
  Microsoft, like Google, may union grants per app — verify live; do not
  assume token scope enforces the split, same stance as today).
- README provider matrix; `docs/HOSTED.md` note (below).
- Backlog tickets: broker MS arm, metric-name generalization,
  `gmail_msg_id` rename, Graph change-notification push (pairs with issue
  #112's Gmail watch work — same relay shape, build adjacent).

### P5 — Hosted funnel (separate design doc when scheduled)

squelch-control is ~1,000 Google-shaped lines whose security arguments
(reverse tenant lookup, the no-oracle property, console hop, CSP `form-action`)
must be RE-ARGUED for a second IdP, not re-typed. Sketch: `provider` column on
tenants, an MS consent arm in `oauth.rs`, funnel copy, warden per-tenant config
gains provider, Bifrost/LLM side unchanged. Strategic note: Microsoft has no
CASA-equivalent gate on mail scopes — publisher verification (MPN) is the only
hurdle — so Outlook may be the cheaper road to an uncapped hosted tier than
Gmail's restricted-scope assessment. Worth pricing before investing in CASA.

## Risks and sharp edges (implementer: read twice)

1. **ImmutableId from request one.** A single default-id request that stores a
   row poisons the unique key for that message's lifetime.
2. **The isRead-churn refetch guard** is the difference between a quiet poll
   and re-downloading the mailbox every 45s on an actively-read account.
3. **Draft-then-send leaves a draft behind if `/send` fails.** Handle: on send
   failure after draft creation, best-effort delete the draft, surface the
   error; never retry into a second draft silently.
4. **MS Authentication-Results is fixture-driven** (decision 9). Do not guess
   authserv-ids from docs.
5. **Legacy cursor adoption** (P0): an existing Gmail install must not
   re-backfill. Test the fallback explicitly before the fleet ever sees this.
6. **`refresh_stored_token_detailed` is the single refresh choke point** — the
   hardcoded Google URL there would silently break every Outlook refresh while
   tests that mock the endpoint stay green. Parameterize it first in P1 and
   grep for any other `GOOGLE_TOKEN_URL` caller.
7. **conversationId quirks**: Exchange occasionally groups aggressively by
   subject. Accept; the reader's thread view already tolerates imperfect
   threading, and `fallback_thread_id` covers absent ids.
8. **MSA refresh tokens expire after ~90 days of disuse.** A daemon polls
   constantly so this bites only stopped daemons; the invalid_grant message
   should name re-auth, not Google console advice.
9. **410 handling must read the Location header** for the fresh delta URL
   rather than reconstructing one, per Graph's contract.

## Estimates (Opus implementation, review rounds included)

| Phase | Size |
|---|---|
| P0 refactor | 1–1.5 days |
| P1 auth | ~1 day |
| P2 Graph read | 2–3 days |
| P3 Graph write | 1–1.5 days |
| P4 periphery | 0.5–1 day |
| P5 hosted | separate; ~1–2 weeks when scheduled |

Wall-clock to a working self-host Outlook daemon with live E2E: **~1.5–2
weeks**. P0 is worth doing even if Outlook slips — it makes the transport
testable and the codebase honest about what is Gmail and what is squelch.
