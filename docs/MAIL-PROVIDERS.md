# Mail providers

Two providers today: Gmail and Outlook (Microsoft Graph, [below](#outlook-microsoft-graph)).
The sync engine and the write door reach either through two traits, so adding
IMAP (issue #575) means writing two modules, not editing the engine.

| Seam | Trait | Gmail | Outlook | Picked by |
|---|---|---|---|---|
| Read (sync) | `squelch_core::mail::MailSource` | `mail::gmail::GmailSource` | `mail::graph::GraphSource` | `SyncEngine::source()` |
| Write (door) | `squelch_api::write::MailWriter` | `write::gmail::GmailWriteClient` | `write::graph::GraphWriteClient` | `handlers::write_client()` |

Both are picked from the account's `accounts.provider` column
(`squelch_core::mail::MailProvider`). It is `'gmail'` on every existing row
(the column default, added by the normal migration). **An unknown value is an
error, never Gmail**: a database a newer build set up an IMAP account in is
refused by this build for that account, everywhere, before any request leaves
with that account's credential.

## Who owns what

The engine keeps **cadence, ingest, triage and cursor persistence**: when to
poll, which folders in which order, what gets stored as sent or spam, what
notifies, when the cursor commits. A source owns **listing, the change feed,
fetching and cursor encoding**. A source decides no policy, and the cursor
stays the engine's to persist. What a source may keep is provider-private
lookup state the engine never reads: the IMAP source records which folder and
UID each message was last seen at (`imap_locations`, behind the narrow
`MessageLocations` trait the store implements), because an IMAP stable id is a
hash and cannot be turned back into a place to fetch from.

**Setting up an IMAP account** (self-host only; sending goes over SMTP):
`squelchd auth --imap [--ca-file PEM]` prompts for host, TLS mode, port,
username and app password, logs in and lists the folders, and only then
stores: the server (host, port, TLS mode, username, and the CA certificate's
bytes, never its path) together with the password in the credential backend's
`<email>#imap` slot (the same keyring entry or sealed file the OAuth tokens
use), then `accounts.provider = 'imap'` last. Nothing about where the password
is sent lives in the database, so a database writer cannot redirect it. It is
refused for an account that already has mail, a cursor or a Google grant. A
slot from an earlier phase-2 build (password only, server in `app_settings`
under `imap_account`) is moved into the slot at the next start and the setting
deleted. An IMAP account never touches Google: the profile fetch, calendar
sync and shredder are skipped for it.

`MailSource` is derived from what the engine actually asked Gmail for:

| Method | Gmail call | Engine callers |
|---|---|---|
| `list(Listing, cap, Urgency)` | `messages.list` (label + `q`) | backfill, catch-up, spam sync, sent-contacts harvest, archive walk (both directions) |
| `current_cursor()` | `users.getProfile` → `historyId` | backfill, catch-up |
| `cursor_is_usable(&cursor)` | historyId > 0 | `poll_once` |
| `changes(&cursor, Folder)` | `history.list` (`labelId`, `messageAdded`) | the incremental walk, INBOX then SENT |
| `merge_cursors(a, b)` | max of two historyIds | the walk's single commit |
| `fetch_raw(id, Urgency)` | `messages.get?format=raw`, base64url-decoded | every ingest path, the blank-body heal |
| `fetch_headers(id, names)` | `messages.get?format=metadata&metadataHeaders=…` | sent-contacts harvest, recipients backfill |
| `inbox_unread()` | `users.labels.get(INBOX)` | the poll tick's unread count |
| `wait_for_change(cursor, max_wait)` | (default: sleep `max_wait`) | the poll lane, between ticks |

`wait_for_change` is the one method that is new rather than moved: a source
that can be told about changes (IMAP `IDLE`) returns early and the poll lane
ticks at once; Gmail keeps the default, which is the same sleep as before. It
must be cancel-safe, because the lane drops it when a manual refresh, a spam
page request or shutdown arrives.

`Urgency` replaces the engine's old `GmailRetry`: `Live` is one attempt,
`Window` (backfill, catch-up) waits a rate-limit refusal out in place so
position is kept, `Background` (the archive walk) is one attempt whose
refusals are counted for the walk rather than shown as a sync error.

`Placement` is what a fetch says about where the message is filed (Gmail's
`INBOX`/`SENT` labels). The engine's `Mailbox::sighting_is_sent` decides from
it; a provider that reports nothing hands the decision back to the folder the
batch was listed from, exactly as a Gmail fetch without labels always did.

## The cursor

The engine stores whatever `SyncCursor` the source hands it in `sync_cursor`
(one row per account) and never looks inside. Gmail's is the decimal
historyId; an IMAP source's would carry `UIDVALIDITY` and a `UID` (and
`MODSEQ`) per folder.

For a Gmail account the legacy `sync_state` `'history'` row **is** the
cursor, because three things depend on it: the stats page's
`last_history_id`, a rolled-back build (which reads nothing else), and an
operator's hand on it (the re-ingest drill in [SECURITY.md](SECURITY.md) §1
deletes it; lowering it replays history; 0 forces a catch-up). So a read
returns the legacy row exactly as the engine always read it, every write lands
in both rows in one transaction, and the `sync_cursor` row is a mirror that is
never read for a Gmail account. A Gmail cursor that is not a historyId is
refused on write. No other provider's cursor is ever parsed or mirrored.

**Downgrades past the first non-Gmail account are not safe.** A build from
before `accounts.provider` ignores the column and would sync an IMAP account
as Gmail. When the first other provider ships, it should also bump something
older builds refuse to open, or say plainly that downgrading is unsupported.

`Store::has_message` exists and is **unwired on purpose**. When a source wires
it, it goes on the incremental path only: the backfill and the catch-up
re-fetch mail already stored, and that re-fetch is how a sanitizer change
reaches `body_html`, which is baked at ingest.

## Gmail-specific on purpose (phase 1)

These stay Gmail-shaped in the engine or around it, deliberately:

- **The Google profile** (name and photo, `sync/profile.rs`) is the People
  API, not Gmail, and is fetched on the poll tick. It is cosmetic and already
  fails quietly without its scope. An off path per provider is phase 4.
- **The quota budget** (`GmailBudget`, `SyncEngine::with_gmail_budget`,
  `squelchd_gmail_*` metrics). The budget is shared by the sync engine, the
  sweeps engine and the write door for one mailbox, and the engine hands it to
  the Gmail source. Metric names are unchanged because renaming them breaks
  dashboards and alerts for no gain. Off for other providers in phase 4.
- **The archive walk's reasoning** is written in Gmail's terms (`-in:inbox
  -in:sent`, a day behind the last good poll), and expressed to the source as
  `Listing::Window { filed_only }`. A provider without a server-side search
  needs its own way to answer it; the engine logic above it does not change.
- **`messages.gmail_msg_id`** keeps its name. Other providers store their own
  id in it (IMAP: a synthetic id, see #575). A rename touches 169 references
  for no behavior.
- **Token refresh** (`credentials.rs`, `refresh_stored_token_detailed`)
  posts to the client's issuer's token URL: Google's, or Microsoft's for an
  Outlook daemon (see [OAuth](#oauth)).
- **Calendar** (`gcal`) and **auth signals** (`extract_auth_pass` trusts only
  an `Authentication-Results` Google stamped, so any other provider's mail
  fails safe to "no verdict") are untouched.
- **The startup log line** reads `gmail REST sync starting` (or `outlook
  graph`); it is chosen by the source's provider.
- **In the writer:** `MailWriter::modify` takes Gmail label ids, and
  `FetchedRaw.raw_b64` stays base64url (its callers decode it, and moving the
  decode into the writer would change how a bad body is reported in the
  forward and send-echo paths). `SCAN_SLACK`, the scheduled-send scan's
  allowance for Gmail's `internalDate` skew, is imported from
  `write::gmail`. `WriteError`'s text still says "gmail api status".
- **The write door's provider read** is one store call per write action. An
  unknown provider is a 501 ("not supported by this build; upgrade
  squelchd"), never a Gmail writer; the callers' audit detail for it is still
  their existing `rejected:no_write_credential`.

## Adding a provider

1. Add the `MailProvider` variant. Every `match` on it is exhaustive, so the
   compiler lists the places that have to pick an implementation (the engine's
   `source()`, the door's `write_client()`, the startup log line).
2. Implement `MailSource` in `squelch-core/src/mail/<provider>.rs`: listing,
   change feed, raw fetch, header fetch, unread counts, cursor encoding, and
   `wait_for_change` if the protocol can push. Report `Placement` from the
   provider's folders, return `CoreError::NotFound` for a message that is gone
   and `ChangesError::Expired` for a cursor the server can no longer replay
   from (IMAP: a `UIDVALIDITY` change).
3. Implement `MailWriter` in `squelch-api/src/write/<provider>.rs`. The RFC 822
   composition in `write/mime.rs` is provider-neutral and shared.
4. Give the provider an off path for the Gmail-only features above.

## Outlook (Microsoft Graph)

Built on the seams above: `squelch_core::mail::graph::GraphSource` (read),
`squelch_api::write::graph::GraphWriteClient` (write), and Microsoft OAuth
in `auth/mod.rs` and `credentials.rs`. Self-host only. A daemon serves one
account, so the provider is daemon config:

```toml
# account_email is optional for Outlook: auth saves the one Microsoft names
provider = "outlook"
# Only when not using Passband's built-in Entra app:
# microsoft_client_id = "00000000-0000-0000-0000-000000000000"
```

`provider` is a config-file key with no environment override. At startup
every daemon entry point calls `SqliteStore::bind_account_provider`: a fresh
account (still the `'gmail'` column default, nothing ever synced) takes the
configured provider once; anything else that disagrees is refused, because a
mailbox never changes provider.

### Read: `GraphSource`

| Method | Graph |
|---|---|
| `list(Folder)` | `GET /me/mailFolders/{inbox,sentitems,junkemail}/messages?$select=id&$orderby=receivedDateTime desc[&$filter=receivedDateTime ge …]` |
| `list(Window)` | `GET /me/messages?$filter=receivedDateTime ge … and lt …&$select=id,parentFolderId,isDraft`, dropping drafts and anything in Drafts, Deleted Items or Junk; `filed_only` also drops Inbox and Sent Items. Folder ids are looked up once per source |
| `current_cursor` | **no request.** The cursor is `{"v":1,"inbox":<link>,"sent":<link>}`, and a fresh one holds each folder's INITIAL delta round filtered to `receivedDateTime ge now - 30m` (`CURSOR_OVERLAP`). `cursor_before_window` is `true`, so the engine mints it BEFORE a backfill or catch-up lists its window and stores it after: the first walk replays everything since the listing began, however long the window took, and stored ids are skipped |
| `changes` | follows `@odata.nextLink` to the `@odata.deltaLink`, drops `@removed` entries, replaces that folder's link. `mailbox_unchanged` is always `false` |
| `merge_cursors` | each folder's link from whichever walk moved it relative to `since` |
| `cursor_is_usable` | decodes, version 1, both links on the Graph base |
| `fetch_raw` | metadata (`conversationId`, `receivedDateTime`, `parentFolderId`) then `$value` (raw MIME, not base64), capped at 64 MB |
| `fetch_headers` | the structured `toRecipients` / `ccRecipients` / `bccRecipients` / `from` / `replyTo`, rendered as header values |
| `inbox_unread` | `unreadItemCount` for both numbers (Graph has no thread count) |
| `changes_report_updates` | `true` (below) |
| `wait_for_change` | the default sleep. Graph's push needs a public HTTPS webhook |

Rules that hold on every request:

- **`Prefer: IdType="ImmutableId"`**, set in the one place a request is
  built. Without it a move changes the id and orphans the row.
- **Every followed URL starts with the Graph base.** Delta and next links
  come back from Graph and sit in the database, and the bearer token goes
  wherever they point, so a foreign one is refused before a request exists.
- **`410 Gone` is `ChangesError::Expired`**, so the engine's catch-up mints a
  fresh cursor. The `Location` Graph sends is ignored: it would start a
  full-folder enumeration, which the catch-up's window listing already covers.
- **`429` / `503` with `Retry-After`** is waited out in place for
  `Urgency::Window` (up to the retry cap), returned for `Live`, and counted in
  `SourceRefusals` for `Background`. No `GmailBudget`. Errors are recorded
  under a closed set of `graph_*` reasons and never carry Graph's text.

**Changed mail, not just new mail.** Graph's delta names a message again on
every read-state flip or flag. `MailSource::changes_report_updates` (default
`false`, Gmail unchanged) makes the engine's INCREMENTAL walk skip ids it
already stores as live mail (`Store::has_message`: a spam or archive-tier row
reads as absent, so a move from Junk into the inbox re-ingests and heals),
before any body is fetched; never on a
backfill or catch-up, which re-fetch on purpose (`body_html` is baked at
ingest). For such a source a walk whose two rounds saw no entry at all
(`Changes::saw_changes`, removals included, since deleting unread mail moves
the count) reports the mailbox idle, so the poll lane's idle ladder works.
Every request also asks `odata.maxpagesize=100`: delta rounds otherwise page
10 at a time.

**Sent is who wrote it.** `Placement.in_sent` is Sent Items OR a `From` equal
to the account address, because Exchange's Sent Items does not follow a
message the user files elsewhere the way Gmail's SENT label does.

**Self-addressed mail** is two Exchange items (the Inbox copy and the Sent
Items copy) with different ids, so it is two rows: one visible, one neutral
sent. That is what the mailbox holds.

### Write: `GraphWriteClient`

| Method | Graph |
|---|---|
| `archive` / `trash` / `not_spam` | `POST /me/messages/{id}/move` to `archive` / `deleteditems` / `inbox` |
| `modify` | `UNREAD` is `isRead`, `STARRED` is `flag.flagStatus`, removing `INBOX` is an archive. Any other label refuses the whole request (400) before a call |
| `send` | **draft, then send**: `POST /me/messages` with the MIME base64 as `text/plain`, then `POST /me/messages/{id}/send`. `sendMail` answers `202` with no id; this way the draft's (immutable) id is the sent item's, so the audit row and the send echo keep it. A definite refusal (a 4xx other than 408, or a 429) deletes its draft; a 5xx, a 408 or a dropped connection leaves it, since the mail may have gone and the draft's id may now name the Sent Items copy. Over Graph's 4 MB request limit is refused before a draft exists (upload sessions are not built) |
| `parent_headers` | `internetMessageId`, `References` from `internetMessageHeaders`, structured recipients |
| `fetch_raw` | `$value`, re-encoded base64url for `FetchedRaw` |
| `find_by_rfc822_msgid` | `$filter=internetMessageId eq '…' and isDraft eq false`: a send whose outcome was unknown leaves a draft carrying the Message-ID, and finding it would record unsent mail as sent |
| `find_scheduled_send` | that lookup, then Sent Items filtered by `sentDateTime` in the window, reading each page's `internetMessageHeaders` for the outbox header. Same page cap, shutdown check, and "any doubt is `Err`" as Gmail's |

A `429` is `WriteError::Paced` with Graph's `Retry-After`.

### OAuth

`OAuthClientConfig.issuer` (`Google` or `Microsoft`) follows `provider` and
decides every endpoint, scope set and identity check:

- **Endpoints**: `login.microsoftonline.com/common/oauth2/v2.0/{authorize,token}`,
  personal and work or school accounts alike. `refresh_stored_token_detailed`,
  the one refresh every token goes through, uses the issuer's token URL.
- **Public client**: an Entra "Mobile and desktop" app has no secret. None is
  sent on the exchange or the refresh, in the body or a Basic header.
- **One consent, both mail slots** (as Gmail's `gmail.modify` since #618):
  `Mail.ReadWrite`, `Mail.Send`, `User.Read`, `Calendars.ReadWrite`,
  `offline_access`. Calendar is asked up front so the Graph calendar (not
  built yet) needs no second consent; no slot requires it, and a mail refresh
  asks only for the mail scopes. Read needs
  `Mail.ReadWrite` or `Mail.Read`; Write needs `Mail.ReadWrite` and
  `Mail.Send`. Either spelling (with or without the
  `https://graph.microsoft.com/` prefix) satisfies a requirement. An exchange
  with no refresh token stores nothing: without `offline_access` there is
  none and the credential dies within the hour.
- **A token from the other company is refused** before it is used or
  refreshed (`credentials::check_token_issuer`, read off its recorded
  scopes): the same address moved between Workspace and Microsoft 365 with
  the old credential in its slot, or a blob exported under the other
  provider. A token with no recorded scopes predates scope recording and is
  Google's: a Gmail daemon accepts it, an Outlook daemon refuses it (every
  Microsoft token is minted with scopes).
- **The refresh** sends the consent's scopes, which Microsoft's v2 refresh
  documents as required.
- **The account check** asks Graph `/me`: `mail`, else an address-shaped
  `userPrincipalName` (`mail` is null on some personal accounts).
- **Transfer blobs** (`--export` / `--import`) verify against Microsoft. The
  exporting machine's config needs `provider = "outlook"` too.
- **`accounts.provider` is bound** by every daemon entry point and by the
  `squelch-api` binary (`ApiState::from_config`), before a writer is picked.
- **Refused for Outlook**: `--broker` (it relays Google's consent), the
  calendar consent and its first-run offer, and calendar credentials in the
  API state, so a Microsoft token is never offered to Google.
- **The client id** is `PASSBAND_MICROSOFT_CLIENT_ID`, built in (a public
  client's id is not a secret), overridden by `microsoft_client_id`. The
  built-in app is registered as multi-tenant plus personal accounts, a
  "Mobile and desktop" platform with redirect `http://127.0.0.1`, and the
  four delegated scopes above.

### Still open

0. **Addresses: handled, with one gap.** Seen live 2026-10-28: a personal
   account can sign in as `x@company.com`, own the alias `y@outlook.com`, and
   send as whichever is primary; `/me` names only the primary (and lags an
   alias change by hours), and Exchange REWRITES a From that is not the
   primary to the hidden `outlook_<hex>@outlook.com`. So:
   - `account_email` is optional for Outlook. `squelchd auth` without it
     takes the address Microsoft names and saves it to the config.
   - A re-consent is the same mailbox when its Microsoft account id matches
     the stored credential's, whatever address Microsoft uses today.
   - Exchange picks the From itself: it enforces the CURRENT primary
     whatever the draft says (seen live 2026-10-29: a draft From the old
     primary went out as the new one). `/me` lags that change by a day, so
     every send writes the From of the newest Sent Items message (else
     `/me`'s `mail`) into the draft. The From the daemon stores for the echo
     is read back from the sent copy, so it is always what really went out.
   - Mail from the configured address, `/me`'s `mail`, the sign-in name, or
     the newest sent From counts as the owner's own.
   The gap: a configured alias that is NOT primary is still the address the
   daemon is filed under, while mail goes out as the primary. The client does
   not yet show "Sends as ...".
1. **Live end to end: mostly verified 2026-10-28** on a personal Microsoft
   account (a scratch daemon, Passband's built-in Entra app). Seen working:
   the `http://127.0.0.1` loopback redirect (registered under "Mobile and
   desktop", NOT Web), the token endpoint echoing prefixed scope names,
   backfill and the delta cursor (two inbound mails, one per poll, immutable
   ids, conversation threads, the sender's UTC offset), a reply, a scheduled
   reply, and archive. The two send facts the design leans on both hold:
   **the draft's immutable id survives the send** (the echo row's id is the
   Sent Items id, so no second sent row), and **Exchange keeps the minted
   `Message-ID` and `X-Passband-Outbox`** on a MIME draft, with
   `internetMessageHeaders` populated on Sent Items. Graph accepts
   `internetMessageId eq … and isDraft eq false`, and the well-known
   `archive` folder exists. A plain send (no minted id) goes out with
   Exchange's own `Message-ID`. Still unseen live: removals and moves in the
   delta, the 410 resync, throttling, trash, not-spam, and the read and flag
   changes.
   **Mail goes out as the account's primary address**, the sign-in address
   on a personal account (see 0), not an `@outlook.com` alias. When that
   address's domain does not authorize Outlook's servers, receivers may
   score it as spoofed.
2. **Auth signals.** `extract_auth_pass` trusts only Google's
   `Authentication-Results`, so Outlook rows get `NULL` (safe: no bypass).
   Microsoft's authserv ids must be captured from real mail first.
3. **Large sends** (over about 3 MB of MIME) need Graph upload sessions.
4. **Downgrades are unsafe.** A build from before `accounts.provider` would
   sync an Outlook account as Gmail and send its tokens to Google. Do not roll
   an Outlook daemon back past this build.
5. **Client copy.** Passband still says "Gmail" in places (Trash, the auth
   pane); `/client/stats` does not advertise the provider yet.
6. **Hosted** has Outlook in a stacked PR ("Hosted signup with Outlook"):
   the confidential "Passband Hosted" Entra app, tenants keyed by Microsoft
   account id (a Microsoft login is never matched to a mailbox by address),
   and an Outlook signup filed under the address mail ARRIVES at, not
   Microsoft's sign-in label. See `docs/HOSTED.md`, "Outlook on hosted".
7. **No profile photo** for Outlook accounts (Graph `/me/photo` is not wired).
8. **A body Exchange cannot render as MIME** fails the batch every poll
   (any non-404 error on `$value` does), which would hold the cursor. If live
   mail shows such items, map that error to `RawBody::Undecodable`.
9. **A 404 on a fetch** (mail deleted between the delta round and the
   fetch) reads as an expired cursor and costs a catch-up, as on Gmail; more
   likely on Graph.
10. **The archive window's exclusions** are the top-level Drafts, Deleted
    Items and Junk only: mail in their subfolders, Outbox or Sync Issues is
    listed.
11. **The account check** trusts `/me` `mail`, which a tenant admin sets.
12. **A label edit can half-apply**: the read/flag PATCH lands, then the
    archive move fails. Retrying is safe.
13. **Listings page by `nextLink`**, likely `$skip`-based: mail deleted or
    moved out during a window listing can shift a never-touched message out
    of the pages. Ids only, so the window is short; the archive walk does not
    look back.
14. **`MailboxNotEnabledForRESTAPI`** may arrive as a 404, which reads as an
    expired cursor (a catch-up, then a generic failure) rather than the
    named reason.
15. **The two copies of a self-addressed message** share a
    `conversationId`, so the thread reader may show it twice.
16. **The re-ingest drill** (SECURITY.md §1) for an Outlook account deletes
    the `sync_cursor` row, not the legacy `sync_state` `'history'` row.
17. **Copy says Gmail** in the door's errors ("Gmail is pacing this
    mailbox") and audit details (`failed:gmail`).
