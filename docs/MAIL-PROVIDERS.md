# Mail providers

Gmail is the only mail provider today, but the sync engine and the write door
reach it through two traits, so adding IMAP (issue #575) or Outlook means
writing two modules, not editing the engine.

| Seam | Trait | Gmail implementation | Picked by |
|---|---|---|---|
| Read (sync) | `squelch_core::mail::MailSource` | `squelch_core::mail::gmail::GmailSource` | `SyncEngine::source()` |
| Write (door) | `squelch_api::write::MailWriter` | `squelch_api::write::gmail::GmailWriteClient` | `handlers::write_client()` |

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
fetching and cursor encoding**. Nothing in a source persists anything or
decides policy.

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
- **Token refresh** (`credentials.rs`, `refresh_stored_token_detailed`) still
  posts to Google's token URL. Parameterizing it belongs with the first
  provider that refreshes anywhere else.
- **Calendar** (`gcal`) and **auth signals** (`extract_auth_pass` trusts only
  an `Authentication-Results` Google stamped, so any other provider's mail
  fails safe to "no verdict") are untouched.
- **The startup log line** still reads `gmail REST sync starting`; it is chosen
  by the source's provider.
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
