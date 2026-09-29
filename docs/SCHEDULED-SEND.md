# Scheduled send

Compose a mail, pick a time, and the daemon sends it then. The mail waits in
the daemon, not the client, so a closed laptop (local daemon) or a closed
lid anywhere (hosted daemon) does not lose it.

Locked 2026-09-26. §2 through §5 are the contract the build waves carry.

## 1. Why not drafts

Drafts (PR #289) are now many per account, addressed by id, with one per
reply target and one per forwarded message. So "one draft per account" is no
longer the reason. The reason is the KEYED UPSERT: `PUT /client/drafts` edits
a row by its key (`Reply(parent)`, `Forward(parent)`, or "newest keyless row"
for an old app), and DraftSaver autosaves through those keys. A scheduled
reply parked as a draft row would be silently rewritten the next time the
user opened a reply to the same message, and an older app's key-less PUT
could overwrite a scheduled new message. A frozen, guard-checked request with
a lease and a state machine must be unreachable by any autosave path, so it
is its own table. Scheduling CONSUMES the draft it came from (§3 step 7);
cancelling hands the request back to the composer, whose next autosave makes
it a draft again.

## 2. Data

### `scheduled_sends`

HUMAN-DOOR ONLY, like `drafts` and `outbound_attachments`: served on
`/client/scheduled`, never synced, never visible on `/mcp`. A new table, so a
plain `CREATE TABLE IF NOT EXISTS` in `schema.sql` (no migrate.rs column).

| column | notes |
|---|---|
| `id` | INTEGER PRIMARY KEY |
| `account_id` | the daemon's one account |
| `request` | TEXT, the `SendBody` JSON exactly as `POST /client/actions/send` would have taken it, `confirm` forced true, `draft_id` stripped (see §3) |
| `send_at` | TEXT RFC3339 UTC |
| `state` | `scheduled` / `sending` / `sent` / `failed` / `cancelled` |
| `lease_until` | TEXT, set while `sending` |
| `attempts` | INTEGER, default 0 |
| `last_error` | TEXT, redacted reason (never content) |
| `message_id_header` | TEXT, the `Message-ID` minted at schedule time: `<` + 48 hex chars + `@passband>` |
| `sent_message_id` | INTEGER, local echo id once sent |
| `sent_thread_id` | TEXT |
| `maybe_sent` | INTEGER 0/1, STICKY: an earlier attempt may have reached Gmail (§4) |
| `recover_at` | TEXT, recovery does not ask Gmail about this `sending` row before it (§4) |
| `created_at`, `updated_at` | TEXT |

Index: `(account_id, state, send_at)`.

`request` is JSON rather than columns so that every field `SendBody` grows
later is scheduled for free, and so the worker deserializes and calls the
same function the handler calls with zero mapping code. The listing handler
deserializes to expose `to`, `subject`, `reply_to_message_id`,
`forward_of_message_id` to the client.

### `outbound_attachments.scheduled_send_id`

New nullable column (migrate.rs, `add_column_if_missing`), a second claim
beside `draft_id`. PR #289 spelled the claim predicate once as
`outbound::CLAIMED` (`draft_id IS NOT NULL`); this becomes
`draft_id IS NOT NULL OR scheduled_send_id IS NOT NULL`, and `sweep` gains a
second orphan arm (rows whose `scheduled_send_id` names no row). The comment
on `CLAIMED` already says exactly this.
Scheduling moves the claim from the draft to the row; the send consumes the
files exactly as today (`discard_sent_attachments`); cancel releases them
back to unclaimed (they then follow the composer's normal lifetime because
the cancelled request is reopened in the composer and re-autosaved).

## 3. Scheduling (the request)

`POST /client/scheduled` body = `SendBody` + `send_at`.

At schedule time the daemon does everything `action_send` does BEFORE the
Gmail call, because the user is present to answer:

1. Same validation order as `action_send` (confirm, not both reply and
   forward, non-empty body, forward needs `to`, `reply_all` needs a parent,
   no unresolved `#group`, no group on reply/forward).
2. `send_at` must be in the future, else 400 (a past date is a client bug,
   same rule as reminders). Also `send_at` at most 1 year out, counted as
   366 days so the same date next year is accepted across a leap day.
3. Every `attachment_ids` entry resolves, else 400 `rejected:attachment_missing`.
4. The outbound guard runs NOW. A verdict is a 422 exactly as on a send;
   `override_guard: true` persists in `request` and the fire path does NOT
   re-scan (content is frozen, the answer would be the same). For a FORWARD
   the scan today also covers the original message, which `forward_send`
   fetches on the write token mid-send; so the schedule handler fetches the
   original itself, unions its kinds with the note's and the attachments',
   and judges once here. The worker then calls `perform_send` with
   `SendGuard::Settled` for every kind of send.
5. `write_client` must exist, else 403 (no point queueing a mail that can
   never go out). Then a reply's parent must resolve, else 404: the send
   route's order (400, 422, 403, 404), so a body with two problems gets the
   answer a send would give. The lookup is unfiltered, like the send's: a
   sealed parent resolves.
6. Mint `message_id_header`.
7. Insert the row `scheduled` (at most 100 live rows per account, counted in
   the same transaction, else 400 `rejected:too_many_scheduled`); move
   attachment claims; `delete_draft` by
   `draft_id` if given (the draft is consumed by scheduling, not by sending:
   from here the row IS the composition). The client passes
   `DraftSaver.currentDraftId(of:)`, not `state.draftId`: PR #289 resolves a
   late create's id into `resolved`, and only that accessor sees it.
8. Audit `schedule` with ids only.

Response `201 {"id", "send_at"}`.

Group `individual` fan-out is refused for v1 (`400 rejected:group_fanout`);
`to`/`bcc` mode groups are fine because the composer already expanded them.

Step 1 of the build (a01c6f3) fixed the shapes the handler and worker use:
`validate_send(&SendBody) -> Result<(), SendFailure>` is pure (the caller
audits), `perform_send(state, body, attachments, SendGuard, message_id)
-> Result<SendOutcome, SendFailure>`, and `SendFailure.reason` is the audit
detail string (`failed:gmail`, `rejected:no_write_credential`, ...), which
is exactly what `last_error` stores. Fan-out keeps answering 200 (the doc
comment on `group_id` saying 202 was never true). `find_by_rfc822_msgid`
passes `includeSpamTrash=true` so a mail the user trashed inside the lease
window still counts as sent.

## 4. Firing (the worker)

`run_scheduled_send_pass(&ApiState)`, exported from squelch-api and spawned
in squelchd beside the shredder, because it needs the WRITE credential and
the sync engine is read-credential-bound by invariant. Ticks every 10s,
honors the shutdown watch between ticks AND between rows inside a pass (the
watch rides on `ApiState`): the send in hand settles, nothing new is claimed.
squelchd stops on Ctrl-C or SIGTERM (it is PID 1 in the image). The tick is
the clock: no timer per row, so a restart costs punctuality and never a mail.

Constants (squelch-api `scheduled_send.rs`, squelch-core `store/mod.rs`):

| name | value | why |
|---|---|---|
| `SCHEDULED_LEASE_BASE` | 180 s | the pre-fire search, the parent read (or a forward's original), the 5 s echo, each bounded by the write client's 60 s |
| lease per row | base + `send_budget(estimate)` | estimate = files × 4/3 + request bytes + 4 KB, plus 20 MiB × 4/3 for a forward; a 25 MB attachment set is ~15 min |
| `RECOVERY_GRACE` | 5 min | Gmail's search index trails a send; recovery only asks about a lease that lapsed this long ago, and a "not found" requeues this far out |
| `RECOVERY_RETRY` | 60 s | a recovery search that could not be made is not retried before this (`recover_at`) |
| `RECOVERY_GIVE_UP` | 30 min past the lease | a `sending` row recovery never managed to ask about goes `failed:recovery` |
| `SCHEDULED_MAX_ATTEMPTS` | 3 | the claim counts; the claim also refuses a row already at it |
| `MAX_PER_PASS` | 20 claims | a backlog drains over ticks, interruptibly |
| not-now retry | 2 min | pacing and an unreachable pre-fire search come back this far out WITHOUT spending an attempt |

Per tick:

1. **Recover** rows in `sending` whose `lease_until` passed at least
   `RECOVERY_GRACE` ago (a crash mid-send) and whose `recover_at` (if set) has
   come. Search Gmail on the write token for `rfc822msgid:<header>`.
   - Found: echo it, mark `sent`, audit `send` `scheduled:<id>:recovered`. No send.
   - Not found, attempts left: back to `scheduled` at `now + RECOVERY_GRACE`
     with `maybe_sent` set. NOT sent in the same pass: the re-fire searches
     once more first (step 3), so a slow index gets two looks.
   - Not found, `attempts >= 3`: `failed`, `failed:crash_loop` (a row whose
     send keeps killing the worker stops).
   - Could not ask (no credential, search error): stays `sending`, lease
     untouched, `last_error` = `rejected:no_write_credential` or
     `recovery:unreachable`, `recover_at = now + 60s`. Past
     `RECOVERY_GIVE_UP` it goes `failed` with `failed:recovery`; such a row
     MAY have been sent, and the client says so. "Send now" on it searches
     before it sends (it is `maybe_sent`).
   This is why the daemon mints the Message-ID: it is the only thing that
   makes "did it go out?" answerable, and it is the whole double-send defense.
2. **Claim** one due row atomically, in one IMMEDIATE transaction: the oldest
   `scheduled` row with `send_at <= now` and `attempts < 3` moves to
   `sending`, `lease_until = now + lease(row)`, `attempts + 1`. The returned
   `attempts` NAMES THE CLAIM: every settle (`sent`, retry, `failed`, requeue,
   unclaim, defer) is guarded on `state = 'sending' AND attempts = ?`, so a
   worker that outlived its lease settles nothing. Loop until none, or
   `MAX_PER_PASS`, or shutdown.
3. **Pre-fire search.** A row with `maybe_sent` set (an earlier attempt failed
   after the post, or its lease lapsed and recovery did not find it) is asked
   about by Message-ID BEFORE it is sent again. Found: settled as in step 1,
   no send. Not found: sent. Could not ask: nothing sent, the claim handed
   back WITHOUT spending the attempt, `last_error = recovery:unreachable`,
   next try in 2 min. A row never marked goes straight to the send.
4. **Send** through `perform_send(state, body, files, SendGuard::Settled,
   Some(Fire))`. Recipients, subject and threading derive from the parent at
   fire time, as today. `group_id` is ATTRIBUTION ONLY at fire time: the
   frozen `to`/`bcc` lists are what go out; a group flipped to `individual`
   since is recorded as the one message it went as (never fanned out), and a
   group deleted since costs only the history entry, never the mail. The row
   reads, sends and consumes ONLY the files it holds (never an unclaimed row
   that happens to carry a reused id).
5. **Settle**: success → `sent`, `sent_message_id`/`sent_thread_id` from the
   echo, and the ONE `send` row `perform_send` wrote reads
   `scheduled:<id>:<outcome>`. Failure → `perform_send` wrote the ledger row,
   marked `scheduled:<id>:<reason>`, and classified it:
   - `paced` (Gmail's budget held the call, nothing posted): back to
     `scheduled` in 2 min, attempt handed back, `last_error = paced`.
   - `maybe_sent` (a transport error or a 5xx from `messages.send`, i.e. after
     the post): the row is marked `maybe_sent` and waits at least
     `RECOVERY_GRACE`, so its re-fire's search can see it.
   - everything else (validation, no credential, the parent read, a 4xx):
     definitely not sent; retried without a search.
   If `attempts < 3`, back to `scheduled` with `send_at` pushed by
   `2^attempts` minutes (or the grace) and `last_error`; else `failed`. A 403
   (write credential gone) is a failure like any other; the row waits for a
   retry and the user sees "didn't send" plus the reason.
6. **Sweep**: `sent`/`cancelled` rows after 7 days, `failed` rows (and their
   files) after 30, then the staged-file sweep, so files a cancel released
   and nobody re-claimed go at their one-day TTL even if no composer runs.

Late is fine. A laptop that slept through 9:00 sends at wake. The response
to "was it very late?" is the Sent row's time, not a refusal. A cap is a
follow-up if it ever bites.

`build_reply_rfc822` gains an optional `message_id` on `ReplyParts`; absent
= today's byte-identical output. The write client gains one method:
`find_by_rfc822_msgid(&str) -> Option<gmail id>`.

Two things only a live Gmail can confirm (the mock cannot): that
`messages.send` keeps a client-supplied `Message-ID` rather than replacing it,
and that `rfc822msgid:<...>` with the brackets percent-encoded finds the
message (and how long after the send it starts to).

## 5. The rest of the door

| route | does |
|---|---|
| `GET /client/scheduled` | rows in `scheduled`/`sending`/`failed`, oldest `send_at` first, with `to`, `subject`, `reply_to_message_id`, `forward_of_message_id`, `send_at`, `state`, `last_error`, `attempts`, `attachments`, plus the parent context PR #289's `DRAFT_SELECT` LEFT JOIN computes for the picker (`thread_id`, `parent_subject`, `parent_from`, `parent_from_addr`). Lift that join into a shared helper rather than pasting it |
| `DELETE /client/scheduled/{id}` | `scheduled` or `failed` → `cancelled`, releases attachment claims back to unclaimed, returns the `request` so the client can reopen it in the composer, WITHOUT `override_guard` and `confirm` (the client sets `confirm` itself, and a reschedule is judged by the guard again). A `sending` row is 409 (it is in Gmail's hands). No server-side draft is created: the composer's next autosave does that through the normal keyed/by-id path, and re-claims the files (well inside the 24h unclaimed TTL) |
| `POST /client/scheduled/{id}/now` | `scheduled`/`failed` → `send_at = now`; the next tick sends it. 202. A `sending` row is 409, the same answer a cancel gets; `sent`/`cancelled`/unknown is 404 |
| `/client/stats` | gains `scheduled_send: true`. The client hides every schedule door without it (same trap as `compose_attachments`: an old daemon must not 404 a click) |

Rows in `sent` and `cancelled` are swept after 7 days, `failed` rows after
30. Every response on these routes is `no-store`, an extractor's own
rejection (a malformed body, a non-numeric id) included: one route layer sets
it.

No SSE: the `events` table is UNIQUE per `message_id` and a scheduled mail
has none until it is sent. The client polls the listing on the 10s sitrep
tick behind the usual 5s TTL. A newly `failed` row toasts once per app run.

## 6. Client

### Composer (Mac footer, InlineReply header line, iOS header)

The **cancel button is gone**. Esc closes, and the footer stops saying it.

A **clock** joins attach and track on the left: those three are "how this
mail goes out." Clicking it opens a time palette built on `RemindTimes`:
typed natural language ("tomorrow 9am", "monday", "in 2h") plus a pinned
list of picks, every row showing its absolute time, re-pinned if a pick has
gone by. Picking a time:

- the clock becomes a chip `Tue 9:00 ×`. `×` clears it.
- the Send verb reads `schedule ⌘↵`. Same key. The button says what it does.
- `send anyway ⌘⇧↵` (guard verdict showing) becomes `schedule anyway`.

⌘↵ with a time set POSTs `/client/scheduled` immediately, closes the
composer, and toasts "scheduled for Tue 9:00" with undo. Undo = DELETE and
reopen the composer with the returned request and the time still set. There
is no 5s hold for a scheduled mail: it is cancellable from Sent any time.

A 422 guard verdict or a 4xx reopens the composer exactly as a failed send
does (`putBack`).

`ComposeState.sendAt: Date?` is the only new state. `DraftSaver` does not
persist it (a draft is an autosave of words, not an intent). Account switch
clears it with the composer.

iOS: the clock sits beside Send in the header. Long-press on Send as a
second door is a follow-up.

### Sent

Scheduled rows sit **pinned on top of Sent**, above the newest sent mail,
in a group headed "scheduled" (hidden when empty). Each row: to, subject,
and the fire time as "Tue 9:00 · in 3h". A `failed` row shows "didn't send"
in `Palette.danger` with the reason. Keys on a scheduled row:

- Enter / click: pull it back into the composer (DELETE + open with the
  request and time). The row disappears; ⌘↵ in the composer reschedules.
  Opening reuses PR #289's `openDraft`/`fill(_:from:)` machinery (a reply
  needs its thread open, a forward its original; both are handled there),
  fed from the returned request instead of a `DraftView`.
- `s` (or the row's "send now" verb): `POST .../now`.
- `x` / delete: cancel outright, toast with undo (undo = re-POST the
  returned request). Same shape as PR #289's `pendingDraftDeletes` +
  `keepDraft` + `fireDraftDelete`: generalize that trio over the two kinds
  rather than growing a second copy.

Rows are `ScheduledRow`, a sibling of `SentRow`; j/k walk both groups as one
list. The Sent refresh fetches both listings in one `refreshSent`.
`ScheduledRow` shows the same who / subject / snippet / file-count line the
picker's `DraftRow` shows; extract those sub-views into one summary view
both rows use, and only the trailing cell differs (fire time and state here,
edit time there).

## 7. Daemon refactor that comes first

`action_send` today is one axum handler that validates, guards, resolves the
write client, builds RFC822, sends, and settles. Split it:

- `validate_send(&SendBody) -> Result<(), ApiError>` (the ordered checks)
- `perform_send(state, body: &SendBody, message_id: Option<&str>) -> Result<SendOutcome, SendFailure>`
  (write client → derive → render → build → `client.send` → `finish_send`)
- `action_send` = validate → guard → perform. Byte-identical behavior.

`group_send::run`/`send_one` is the existing precedent for sending outside a
request.

## 8. Invariants

- `account_id` on every query; the daemon is one account, but `store_call`
  still injects it.
- Never log or audit `request` content: ids, states, reasons only.
- `/mcp` never sees `scheduled_sends` or its attachments (two-door).
- The guard runs at schedule time and its `override_guard` is honored at
  fire time; nothing re-scans.
- Exactly-once: a row is sent only from `sending` under a lease, and
  recovery answers via the minted Message-ID before any retry.
- Sealed/read-only tenants: scheduling needs the write credential at
  schedule time (403), and the worker treats a missing credential as a
  retryable failure, never a silent drop.

## 9. Phasing

1. **Daemon**: §7 refactor (byte-identical, tests green), then table + store
   fns + attachment claim, routes, worker with recovery, stats gate. Tests:
   claim atomicity, lease expiry recovery both branches, retry backoff,
   cancel releases claims, sweep, guard at schedule time.
2. **Mac client**: clock + chip + palette, verb flip, POST + undo toast,
   Sent pinned group with three verbs, failed toast. Seen on screen.
3. **Follow-up PR, immediately after**: undo-send moves onto the outbox.
   `sendWithUndo` becomes `POST /client/scheduled` with `send_at = now + 5s`
   and undo = DELETE + `putBack`. Kills the memory-only `heldSends`, and
   with it the Cmd-Q-inside-the-hold silent drop, `parkHeldSends` (both the
   account-switch and the `quitting:` arm), PR #289's
   `applicationShouldTerminate` wait (marked TEMPORARY there for exactly
   this), `isHeld`/`holdsDraft`, `firingDraftIds`, and the epoch guard. The worker tick would need to drop to ~2s for the hold to
   feel like 5s; or the client POSTs with `now` and the row carries a
   `not_before` the worker honors. Decide then.
4. Later: iOS long-press on Send, a very-late cap, daemon roll + release
   note (the stats gate makes either ship order safe).

## 10. What PR #289 (drafts) left for this

Reviewed 2026-09-28 against `origin/drafts`. The nudges landed: Cancel is
gone from the footer, the picker sits in the header with the "how a mail
goes out" reasoning written down, the claim predicate is one constant, and
quit-parking is a thin `parkHeldSends(quitting:)` marked temporary. Drafts
merges first; this branch (docs only so far) rebases.

Reused directly:

- daemon: `CLAIMED` + `sweep` (one edit each), `delete_draft` by id, the
  `DRAFT_SELECT` parent-context join, the `drafts_v2` stats-gate shape,
  `no_store()` on every response.
- client: `openDraft`/`fill(_:from:)` to pull a request back into the
  composer, the `pendingDraftDeletes` undo trio, `DraftRow`'s summary line,
  `DraftSaver.currentDraftId(of:)`, `awaitVerdict` for the stats gate.

Not touched by drafts, still owed here: the `perform_send` split, the
outbox table and worker, Message-ID minting and the `rfc822msgid` lookup,
the time palette (from `RemindTimes`), the Sent pinned group.

Footer conflict is now trivial: drafts removed the Cancel button; this adds
the clock on the left. Different hunks.
