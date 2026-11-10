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
`/client/scheduled`, never synced, never visible on `/mcp`. A new table, a
plain `CREATE TABLE IF NOT EXISTS` in `schema.sql`; the four columns added
after the table first landed (`maybe_sent`, `recover_at`, `last_attempt_at`,
`search_deferred_since`) are also in migrate.rs (`add_column_if_missing`),
so a database created by an earlier build of this branch gains them.

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
| `message_id_header` | TEXT, the `Message-ID` minted at schedule time: `<` + 48 hex chars + `@passband.app>` (rows minted before 2026-09-29 end `@passband>`; the dotless host is a plausible reason Gmail rewrote the id, §4). The 48 hex are also the row's OUTBOX TOKEN (`X-Passband-Outbox`, §4), derived as the local part before `@` WHATEVER the host, not stored |
| `sent_message_id` | INTEGER, local echo id once sent |
| `sent_thread_id` | TEXT |
| `maybe_sent` | INTEGER 0/1, STICKY: an earlier attempt may have reached Gmail (§4) |
| `recover_at` | TEXT, recovery does not ask Gmail about this `sending` row before it (§4) |
| `last_attempt_at` | TEXT, when the last attempt ENDED (its retry or failed settle; a requeue, a `failed:crash_loop` and recovery's `failed:recovery` stamp the lapsed lease's end; a pre-fire search's `failed:recovery` keeps it, since that claim sent nothing). A `maybe_sent` row is not fired before this + `RECOVERY_GRACE` (§4) |
| `search_deferred_since` | TEXT, the first "could not ask" of a pre-fire search still unanswered; the give-up clock (§4) |
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
4. A FORWARD's original is fetched NOW on the write token, so one that is
   gone or past `MAX_FORWARD_RAW_BYTES` is refused while the user is here,
   not hours later at fire time. (There is no outbound guard on this route:
   it runs on agent sends only, see docs/SECURITY.md §5.)
5. `write_client` must exist, else 403 (no point queueing a mail that can
   never go out). Then a reply's parent must resolve, else 404: the send
   route's order (400, 403, 404), so a body with two problems gets the
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
The group's mode AT SCHEDULE TIME is frozen into the request as `group_mode`
(stamped by the route, whatever a client sent; absent on rows scheduled before
it existed), for the fire's attribution (§4 step 5).

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

`scheduled_send_worker(ApiState, shutdown, timing)`, exported from
squelch-api and spawned in squelchd beside the shredder, because it needs the
WRITE credential and the sync engine is read-credential-bound by invariant.
It runs `run_scheduled_send_pass` (recover, claim, sweep) once a minute of
idleness and the claim half the moment a row comes due: after each round it
reads when the next `scheduled` row is due (`next_scheduled_send_at`) and
sleeps until then, or until the next maintenance pass, or until the schedule
and send-now handlers poke its wake (`ApiState::scheduled_wake`) for a row
just added or pulled forward. It used to tick every 10s whatever the outbox
held. It honors the shutdown watch between rounds AND between rows inside a
pass (the watch rides on `ApiState`): the send in hand settles, nothing new is
claimed. squelchd stops on Ctrl-C or SIGTERM (it is PID 1 in the image). The
store is the clock: no timer per row, so a restart costs punctuality and never
a mail.

Constants (squelch-api `scheduled_send.rs`, squelch-core `store/mod.rs`):

| name | value | why |
|---|---|---|
| `SCHEDULED_LEASE_BASE` | 180 s | the pre-fire search, the parent read (or a forward's original), the 5 s echo, each bounded by the write client's 60 s |
| lease per row | base + `send_budget(estimate)` | estimate = files × 1.37 (base64 with a CRLF every 76 chars, not a flat 4/3) + request bytes + 4 KB, plus 20 MiB × 1.37 for a forward; a 25 MB attachment set is ~15 min |
| `RECOVERY_GRACE` | 5 min | Gmail's search index trails a send; recovery only asks about a lease that lapsed this long ago, a "not found" requeues this far out, and a `maybe_sent` row is not FIRED before `last_attempt_at` + this, whatever `send_at` says (a send-now included): the fire hands the claim back, no attempt spent, `last_error = recovery:grace`, `send_at` = the grace's end |
| `RECOVERY_RETRY` | 60 s | a recovery search that could not be made is not retried before this (`recover_at`) |
| `RECOVERY_GIVE_UP` | 30 min | a `sending` row recovery never managed to ask about goes `failed:recovery` this long past its lease; a pre-fire search that stays unreachable goes `failed:recovery` this long after its first "could not ask" (`search_deferred_since`). Both keep `maybe_sent` |
| `SCHEDULED_MAX_ATTEMPTS` | 3 | the claim counts; the claim also refuses a row already at it |
| `MAX_PER_PASS` | 20 | claims per pass, and rows recovery asks about per pass: a backlog drains over rounds, interruptibly (a capped round is followed at once) |
| not-now retry | 2 min | pacing and an unreachable pre-fire search come back this far out WITHOUT spending an attempt |

Per pass (recovery and the sweep run on the worker's maintenance clock, once
a minute of idleness, measured from the end of the last pass; the claim step
runs whenever a row comes due):

1. **Recover** rows in `sending` whose `lease_until` passed at least
   `RECOVERY_GRACE` ago (a crash mid-send) and whose `recover_at` (if set) has
   come. Ask Gmail on the write token whether the mail went out: THE LOOKUP
   below. The pre-fire search (step 3) is the same lookup.
   - Found: echo it, mark `sent`, audit `send` `scheduled:<id>:recovered`. No send.
   - Not found, attempts left: back to `scheduled` at `now + RECOVERY_GRACE`
     with `maybe_sent` set. NOT sent in the same pass: the re-fire searches
     once more first (step 3), so a slow index gets two looks.
   - Not found, `attempts >= 3`: `failed`, `failed:crash_loop` (a row whose
     send keeps killing the worker stops). It stays `maybe_sent`: one miss in
     a trailing index is not proof.
   - Could not ask (no credential, search error): stays `sending`, lease
     untouched, `last_error` = `rejected:no_write_credential` or
     `recovery:unreachable`, `recover_at = now + 60s`. Past
     `RECOVERY_GIVE_UP` it goes `failed` with `failed:recovery`; such a row
     MAY have been sent, and the client says so. "Send now" on it searches
     before it sends (it is `maybe_sent`).
   At most `MAX_PER_PASS` rows are asked about per pass.
   This is why the daemon mints the Message-ID and the outbox token: they
   are the only things that make "did it go out?" answerable, and the lookup
   is the whole double-send defense.

   **THE LOOKUP** (`GmailWriteClient::find_scheduled_send(message_id,
   (after, before), stopping)`). Every fired send carries two fingerprints:
   the minted `Message-ID: <48hex@passband.app>` and a custom header
   `X-Passband-Outbox: 48hex`. The lookup asks for them in two steps:

   a. `messages.list?q=rfc822msgid:<48hex@passband.app, brackets off,
      percent-encoded>&maxResults=1&includeSpamTrash=true`. A hit returns at
      once. A miss proves NOTHING (below).
   b. The Sent scan: `messages.list?q=in:sent after:<epoch seconds>
      before:<epoch seconds>&maxResults=25&includeSpamTrash=true`, newest first, following
      `pageToken` for at most 4 pages (100 candidates), and for each
      candidate `messages.get?format=metadata&metadataHeaders=X-Passband-Outbox
      &metadataHeaders=Message-ID`. A candidate whose `X-Passband-Outbox`
      equals the token (exact, trimmed), or whose `Message-ID` is the minted
      one, is the mail. The window is `scan_window(row)`, one hour of clock
      slack (`SCAN_SLACK`) on each side:
      - FLOOR: the earliest of the row's `send_at`, `last_attempt_at` and
        `lease_until`, less one lease (a retry or requeue moves `send_at` PAST
        the attempt being asked about), less the slack.
      - CEILING: the END of the attempt being asked about, plus the slack. An
        attempt cannot have posted after its own end. The pre-fire search
        (step 3) asks about the last attempt, which ended at
        `last_attempt_at` (the row's `lease_until` there is the claim just
        taken); recovery asks about the lapsed attempt, which ended at
        `lease_until` (a `last_attempt_at` there is an EARLIER attempt). With
        neither, `send_at` plus one lease.
      The ceiling is what keeps an old row sendable: without it, a send-now of
      a row that failed a day ago scans a day of Sent mail, hits the page cap
      on any busy mailbox, and is deferred into `failed:recovery` on every
      try, burning ~2,000 quota units a cycle. Bounded both ways the window is
      about two hours plus one lease however old the row is.

   WHY TWO STEPS (live finding, 2026-09-29, real Gmail): the daemon sent a
   scheduled mail with its minted Message-ID, the mail went out and was
   echoed back into the store, and `rfc822msgid:` for the bare id (brackets
   already off, eb71173) found NOTHING at 9 and at 28 minutes. Recovery
   requeued a delivered mail as maybe-sent, one grace from a duplicate. The
   likeliest reading is that `messages.send` replaces a client-supplied
   Message-ID; the design holds either way, because step (b) reads a header
   Gmail has no reason to touch. Step (a) stays because it is one cheap call
   and, where the id survives, it answers without the scan.

   Every request of both steps goes through the write client's budget
   (`admit`, the 5 s wait cap), timeout and `settle`, like any other write
   call. Outcomes: a match is `Found`; a scan that reaches the end of the
   listing without one is `Not found`; ANY error (transport, 5xx, 4xx,
   pacing) is "could not ask"; and a scan that hits its 4-page cap with more
   pages still listed is ALSO "could not ask" (a transport-class error, so
   UNREACHABLE in step 3): it could not rule the mail out, and reading that
   as "not sent" is the double send this exists to prevent. A row in that
   state (over 100 Sent mails inside its window) ends `failed:recovery` for
   the user after `RECOVERY_GIVE_UP`. A scan cut short by shutdown (checked
   before every request after the first) is "could not ask" too.

   The scan can outrun the claim's lease (worst case 1 + 4 + 100 requests).
   That is safe: the pass loop is sequential, so recovery never touches a row
   its own worker is firing, and the lookup posts nothing
   (`SCHEDULED_LEASE_BASE`'s doc). What it must not do is push the POST past
   the lease's end, the window's ceiling on recovery: a pre-fire lookup that
   leaves less than the parent-headers call (60 s) plus the upload's budget
   is "could not ask" (`recovery:unreachable`), and nothing is sent
   (`lease_too_short_to_post`).

   WHY A DOTTED HOST: the id was minted as `<48hex@passband>` until
   2026-09-29. A dotless right-hand side is a plausible reason Gmail rewrote
   it in the live finding; under `passband.app` an id that survives is found
   by step (a) and skips the scan. The token contract is unchanged (the local
   part), so rows minted under either host are found the same way.

   `X-Passband-Outbox` IS A STABLE CONTRACT: every mail a daemon has fired is
   recognised by that exact header name and by the token being the minted
   id's 48-hex local part. Renaming either strands the recovery of any row
   in flight across the upgrade. Interactive sends (the live send route)
   carry neither the minted id nor the header: nothing ever asks about them.
2. **Claim** one due row atomically, in one IMMEDIATE transaction: the oldest
   `scheduled` row with `send_at <= now` and `attempts < 3` moves to
   `sending`, `lease_until = now + lease(row)`, `attempts + 1`. The returned
   `attempts` NAMES THE CLAIM: every settle (`sent`, retry, `failed`, requeue,
   unclaim, defer) is guarded on `state = 'sending' AND attempts = ?`, so a
   worker that outlived its lease settles nothing. Loop until none, or
   `MAX_PER_PASS`, or shutdown.
3. **Pre-fire search.** A row with `maybe_sent` set (an earlier attempt failed
   after the post, or its lease lapsed and recovery did not find it) is asked
   about through THE LOOKUP (step 1) BEFORE it is sent again. First the GRACE, enforced
   here and not by `send_at` (which a send-now moves): inside
   `last_attempt_at + RECOVERY_GRACE` the claim is handed back untried
   (`recovery:grace`). Then the search. Found: settled as in step 1, no send.
   Not found: sent. Could not ask, two kinds:
   - REFUSED (no credential, which is where a revoked write grant first
     surfaces; a 4xx other than 403 and 429): an attempt like any other,
     spent and backed off (`rejected:no_write_credential` / `failed:search`),
     `failed` after the third, still `maybe_sent`.
   - UNREACHABLE (transport, 5xx, 429, 403, pacing): nothing sent, the claim handed
     back WITHOUT spending the attempt, `last_error = recovery:unreachable`,
     next try in 2 min; `RECOVERY_GIVE_UP` after the first such answer it goes
     `failed:recovery`, still `maybe_sent`. A 403 is here, not under
     REFUSED: Gmail answers a per-user rate limit (`userRateLimitExceeded`)
     with a 403 as often as a permission refusal, and the write client drops
     the body that tells them apart once the budget has read it. A genuine
     permission 403 costs the give-up's 30 minutes before the user sees it.
   A row never marked goes straight to the send.
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
   - `paced` (Gmail's budget held a call, the send's or the forward
     original's or the reply parent's read; nothing posted): back to
     `scheduled` in 2 min, attempt handed back, `last_error = paced`, and the
     ledger row says `paced` too.
   - `maybe_sent` (a transport error or a 5xx from `messages.send`, i.e. after
     the post): the row is marked `maybe_sent` and waits at least
     `RECOVERY_GRACE`, so its re-fire's search can see it.
   - everything else (validation, no credential, the parent read, a 4xx):
     definitely not sent; retried without a search.
   If `attempts < 3`, back to `scheduled` with `send_at` pushed by
   `2^attempts` minutes (or the grace) and `last_error`; else `failed`, and a
   `maybe_sent` answer on that last attempt is KEPT on the failed row (send-now
   resets the attempts, never the flag). A 403
   (write credential gone) is a failure like any other; the row waits for a
   retry and the user sees "didn't send" plus the reason.
   Outcomes the worker decides WITHOUT reaching `perform_send` (an unreadable
   request, a lost file, a crash loop, a recovery or search that gave up, the
   pre-fire search's refusals) are written to the ledger by the worker, same
   `scheduled:<id>:<reason>` shape; the "not now" deferrals are not.
   Attribution: a fired `group_id` records the direct recipients of the ONE
   field the group's mode expanded into (`to` for a To group, `bcc` for a
   Bcc group), as frozen in the request: who the mail went to, not the
   group's members at fire time. The mode is the one it was SCHEDULED under
   (the request's `group_mode`), not the group's mode now: a Bcc group
   flipped to To since would otherwise read the frozen `to`, which for that
   mail is empty. A row without `group_mode` (scheduled before it existed)
   falls back to the group's mode now, an `individual` one reading as Bcc
   when the request carried blind copies, else To. That list may include
   non-members the user typed into the same field; addresses in the other
   field are never recorded. An EMPTY list records nothing (no
   zero-recipient `group_sends` row).
6. **Sweep**: `sent`/`cancelled` rows after 7 days, `failed` rows (and their
   files) after 30, then the staged-file sweep, so files a cancel released
   and nobody re-claimed go at their one-day TTL even if no composer runs.

Late is fine. A laptop that slept through 9:00 sends at wake. The response
to "was it very late?" is the Sent row's time, not a refusal. A cap is a
follow-up if it ever bites.

`ReplyParts` and `ForwardParts` gain an optional `message_id` and an optional
`outbox_token`; both absent = today's byte-identical output, and both are set
only when the worker fires a row (`Fire`). The write client gains
`find_by_rfc822_msgid(&str)` (step a) and `find_scheduled_send(&str,
sent_after)` (the whole lookup).

What only a live Gmail can confirm (the mock cannot): whether `messages.send`
keeps a client-supplied `Message-ID` (the 2026-09-29 finding says probably
not; the received mail's headers will settle it), that the Sent copy keeps
`X-Passband-Outbox` (custom `X-` headers are not known to be stripped), and
how long after a send `in:sent` lists it. `rfc822msgid:` takes the BARE id
(`id@host`, the `@` percent-encoded); `rfc822msgid:<id@host>` finds nothing.

## 5. The rest of the door

| route | does |
|---|---|
| `GET /client/scheduled` | rows in `scheduled`/`sending`/`failed`, oldest `send_at` first, with `to`, `cc`, `bcc` (each `""` when the request had none; a reply's `""` cc still derives at fire time), `subject`, `reply_to_message_id`, `forward_of_message_id`, `send_at`, `state`, `last_error`, `attempts`, `attachments`, plus the parent context PR #289's `DRAFT_SELECT` LEFT JOIN computes for the picker (`thread_id`, `parent_subject`, `parent_from`, `parent_from_addr`). Lift that join into a shared helper rather than pasting it |
| `DELETE /client/scheduled/{id}` | `scheduled` or `failed` → `cancelled`, releases attachment claims back to unclaimed, returns the `request` so the client can reopen it in the composer, WITHOUT `confirm` (the client sets `confirm` itself; a row queued before the guard was retired also has any old `override_guard` stripped). A `sending` row is 409 (it is in Gmail's hands). No server-side draft is created: the composer's next autosave does that through the normal keyed/by-id path, and re-claims the files (well inside the 24h unclaimed TTL) |
| `POST /client/scheduled/{id}/now` | `scheduled`/`failed` → `send_at = now`; the worker is woken for it. 202. A `maybe_sent` row still waits out `RECOVERY_GRACE` from its last attempt and is searched for first (§4 step 3): "now" moves the clock, never the double-send defense. A `sending` row is 409, the same answer a cancel gets; `sent`/`cancelled`/unknown is 404 |
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

A **clock icon** sits in the right cluster immediately left of the Send
button (user decision 2026-09-29; it began life in the left cluster beside
attach and track). No label; the tooltip reads "send later ⌘⇧L". Clicking it opens a time palette built on `RemindTimes`:
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

- Enter / click: open the mail READ-ONLY, as scheduled (user decision
  2026-09-29): a preview pane in the reader's slot showing to/cc/bcc,
  subject, the body rendered through the composer's own
  `/client/markdown/preview`, the attachments, and the fire time, with
  three verbs: `edit`, `send now`, `cancel`. Esc closes the preview and the
  mail STAYS scheduled. Only `edit` pulls it back into the composer
  (cancel + reopen with the request and time, via PR #289's
  `openDraft`/`fill(_:from:)` machinery; a reply needs its thread open, a
  forward its original). Nothing about opening a row moves it to drafts.
  The preview prints cc and bcc only when the request states them, shows an
  inline picture as its name in brackets (`[photo.png]`: there are no bytes
  to draw, and the reading frame drops a `cid:` it cannot resolve), and a
  verb the store would refuse (a row gone `sending`, a row whose last verb
  is on the wire) toasts why and leaves the preview open. Opening it takes
  the caret out of a side-strip composer, or Enter would type into it.
- **The pull-back opens in the DELETE's own turn.** From the moment the row
  is gone the only copy of the mail is in the app, and the only things the
  account switch and ⌘Q flush are composers. So nothing is awaited between
  the answer and the composer taking it (autosave armed at once); the group
  pill and the drafts check ("replaced your unsent draft…") land after, and
  only on a composer that is still that mail. The drafts list for that check
  is fetched without `settle` and leaves out the mail's own row, since its
  first save is a debounce away. A pane reply to the same parent is closed
  into drafts before the inline reply opens, so its flush goes first.
- ⌘Q while a schedule POST, a pull-back DELETE or an undo's re-POST is on
  the wire waits for the answer (bounded, 8s), then flushes whatever
  composer that answer reopened (`AppStore.outboxOnWire`).
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
- Exactly-once: a row is sent only from `sending` under a lease, and
  recovery answers via the minted Message-ID and the `X-Passband-Outbox`
  token (the §4 lookup) before any retry.
- Sealed/read-only tenants: scheduling needs the write credential at
  schedule time (403), and the worker treats a missing credential as a
  retryable failure, never a silent drop.

### Live-route changes this branch makes

`POST /client/actions/send` is shared with the worker (`perform_send`), and
a few of its answers changed with the split. For the PR description:

- **Paced parent read on a plain reply**: a Gmail-budget hold on the
  parent-headers read now answers 503 ("Gmail is pacing this mailbox") and
  audits `send` `paced`. Before, a plain reply could degrade to an
  unthreaded reply with no derived audience and be sent (in practice the
  send right behind the read was usually held by the same budget too).
- **Paced parent read on reply-all**: 503 + audit `paced`, where it was a
  502 audited `failed:gmail`.
- **Paced forward original read**: 503 + audit `paced`, where the empty
  original became a 502 audited `failed:fetch_original`.
- **`finish_send` audits `paced`** when the budget held `messages.send`
  itself (nothing posted); the response was already 503, but the ledger
  said `failed:gmail`.

Each is pinned by a `client_api` test on the live route
(`a_live_reply_whose_parent_read_is_paced_is_503_and_audited_paced`,
`a_live_forward_whose_original_read_is_paced_is_503_and_audited_paced`).
The reply test's parent has no stored From, so a read that fell through to
the unthreaded path would be a 400 (nobody to address) or, on reply-all, a
502: only the Paced arm answers 503, and the test fails if it is reverted.

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
   note (the stats gate makes either ship order safe). Rolling BACK to a
   binary without `last_attempt_at` / `search_deferred_since` (or without the
   fire-time grace) would re-fire `maybe_sent` rows without the grace and
   without the bounded search: acceptable only because the feature has never
   shipped, so no such row exists in the field yet.

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
