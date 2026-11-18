# Smart re-notify

Issue #625. A second banner for mail that still needs handling, sent when the
owner can act on it: right after they sit down at the Mac, or a few minutes
after a meeting ends. Never during a meeting, never just before one, never
outside working hours.

Behind the `renotify` hidden feature (`squelch_core::features::RENOTIFY`),
ticked per tenant on the control plane's `/admin/features`. Off, nothing below
exists: no session is written, no pass runs, and the routes 404.

## Where the code is

| Piece | File |
|---|---|
| The rules (pure, no I/O) | `squelch-core/src/renotify.rs` |
| Quick clear's estimator and planner (pure) | `squelch-core/src/quick_clear.rs` |
| Quick clear's backlog read | `squelch-core/src/store/sqlite/quick_clear.rs` |
| Sessions, candidates, busy times, banner record | `squelch-core/src/store/sqlite/renotify.rs` |
| Tunables | `RenotifyConfig` in `squelch-core/src/config.rs` (`[renotify]`, `SQUELCH_RENOTIFY_*`); quick clear has none, its numbers are constants in `quick_clear.rs` |
| Worker, frame, routes | `squelch-api/src/renotify.rs` |
| Session + frame on the Mac stream | `squelch-api/src/events.rs` |
| Spawned | `squelchd/src/bin/squelchd.rs` |

## Triggers

**Client online.** Every Mac event stream (`GET /client/events?opens=1`) is a
row in `client_sessions` for as long as it is open. When one opens and the Mac
had been disconnected for at least `min_offline_mins` (default 120), the stream
wakes the worker. The worker waits `online_settle_secs` (default 30) so the
app's own event replay lands first and a wake that sleeps again at once is
dropped. If that session is still open, or any Mac stream is (a Wi-Fi handoff
during the settle replaces the session without a second wake), it runs a pass.

A first-ever connection is onboarding, not an arrival. A second stream while
one is open is no gap at all. A daemon restart closes the sessions it left open
at startup time, before the listener binds, which can only make the next gap
look shorter. (Run any later, it could close a live session that reconnected
first, and that Mac's next blip would read as an arrival.)

**Meeting end.** Once a minute, while a Mac is listening, the worker reads the
owner's busy intervals. Only the owner's own calendars count: the primary and
any they own. A teammate's or team calendar they subscribe to is someone else's
schedule. A meeting's window opens `meeting_end_delay_mins` after
it ends (default 3) and stays open `meeting_end_window_mins` (default 10). A
block longer than `max_meeting_mins` (default 240) is not a meeting. An end that
falls inside another busy interval (back-to-back meetings) is no gap.

## The rules (`renotify::plan`)

Checked in this order. The first one that fails is the hold reason.

1. Not in a meeting.
2. No meeting starting within `meeting_guard_mins` (default 15).
3. Inside working hours in the owner's zone (`work_start_hour`..`work_end_hour`,
   default 7 to 20; the zone is the one `owner_zone` already resolves). Every
   day of the week, weekends included on purpose: a weekend's busy calendar
   events hold it exactly as a weekday's do.
4. Under `daily_budget` re-notify banners since local midnight (default 4).
   Quick clears are not counted here; they have their own cap (below).
5. The trigger is not paused by the owner's habits (below).
6. At least one candidate is due.

There are two kinds of candidate. A message can be both; it is then named
once, as due soon.

**Unhandled** (`why: "unhandled"`). The store's query: it has an `events` row,
is not an open-tracking notice or an auth alert, is received mail that is not
spam, has not been opened, done, archived or given a reminder, and has no reply
on its thread since. Its thread must also not have been taken out of For your
eyes since the first banner, by the owner's own correction or by a re-triage
(a new sender rule re-triages that sender's mail). The `events` row is the first
verdict; this is the current one. "Still open" is one SQL fragment
(`renotify_still_open_sql!`) shared by both candidate reads.

It is **due** when its first banner is between `min_age_mins` (default 30) and
`max_age_mins` (default 3 days) old, it has had fewer than `max_per_message`
re-notifies (default 2), and the cooldown since its last one has passed. The
cooldown (`cooldown_mins`, default 180) doubles for each re-notify already spent
on it. Any of those that is still a candidate went unanswered.

**Due soon** (`why: "due_soon"`). Opening a bill is not paying it. Mail in For
your eyes (read from the FYE list itself, so pins and fired reminders count)
with a due date comes back as the date nears, opened or not:

- The due time is the earliest unresolved action's due date, else the thread's
  `attention_at`. A local time is read in the zone the decision names, else the
  owner's. A bare date is `due_day_hour` (default 17:00) that day in the owner's
  zone.
- It is due from `due_lead_mins` before (default 24h) to `due_grace_mins` after
  (default 12h), and not within `min_age_mins` of the mail arriving or of
  another re-notify for it.
- Once per due date: the due time is recorded on the banner's item. A due date
  that moves is a new one. This does not count against `max_per_message`, and
  `max_age_mins` does not apply.
- Left out: auth mail, and mail marked done, archived, snoozed or replied to.

Picks are ranked by importance, plus a boost for a deadline that is close or
past, for urgent-tier mail and for due-soon mail, minus a penalty per
re-notify already spent (ordinary candidates only). At
most `max_items` (default 3) go in one banner.

## Quick clear

The same pass can send a different banner: "5 minutes clears 14 of your 23
emails". Tapping it opens a sprint queue holding exactly the items counted.
Same triggers, same gate (meeting, guard, working hours), same feature flag,
same table and tap route. Its daily budget is its own.

**One banner per arrival**, in this order (`renotify::stage` then
`renotify::finish`):

1. Due-soon mail. A bill due tomorrow outranks a tidy inbox. The backlog is
   not even read.
2. A quick clear that met its floors.
3. Plain "still waiting" mail.

A quick clear is allowed only when all of these hold:

- **A Mac that can show it is listening.** The app says so on its stream with
  `GET /client/events?opens=1&quick_clear=1`, and the daemon counts those
  streams (`RenotifyShared::quick_listeners`). An older app sends only
  `opens=1` and would drop the frame, so while only older Macs listen the
  arrival keeps its "still waiting" banner. This is what makes it safe to
  roll the daemon before the Mac release.
- Its (trigger, `quick_clear`) pair is not paused by habits.
- The last one fired at least `quick_clear::GAP_HOURS` (4) ago.
- Fewer than `quick_clear::DAILY_CAP` (2) fired since local midnight. This is
  its own budget: quick clears never spend the re-notify `daily_budget`, so a
  bill coming due in the evening still has its banner.

**A held plan stands for an hour.** When the plan holds (too few, too small
a share), the backlog is not read again for `quick_clear::HELD_RECHECK_MINS`
(60): the read is smart clear's scan, and a held plan rarely changes in
minutes. In memory only (`RenotifyShared::quick_held_until`); a restart
forgets it. The pass reports the hold as `quick_too_few` or
`quick_small_share` when nothing else was sent.

**The backlog (the denominator).** Every card on the For your eyes board
(`agent_triage::list_items("fye")`, the rows the Dock badge counts) plus the
Reading rows smart clear would take right now on stored facts alone (smart
clear's own `scan`). Other Reading mail is not counted: Passband filed it as
optional, so an unread newsletter is not a demand on the owner, and counting
it would make a true sprint read as a small one. `open_count` is the number of
distinct messages in that list, quick or not.

**What an item costs** (`quick_clear::estimate`). Eligibility first, then the
best evidence for the number:

| Item | Cost | `kind` |
|---|---|---|
| Sealed, auth or restricted mail, or a reminder pending or fired, on ANY message of the item's thread (a card is a thread) | never | |
| A Reading row smart clear takes on stored facts (`event_passed`, `offer_ended`, `ignored_sender`, `skimmed_sender`, `stale_promo`; never the model-judged `expired`) | 0 each; the one press is 5s | `smart_clear` |
| An open `reply`, `pay`, `decide` or `other` action on the thread | never | |
| An open `review` / `attend` action | 40s / 25s, summed | `review` / `attend` |
| `needs_user` with no action named | never (unknown) | |
| Receipt, delivery or account notice, nothing open | 5s | `receipt` / `delivery` / `account_service` |
| Reading content | words at 250 wpm, at least 10s | `read` |
| Anything else (informational, resolved, waiting on others) | 8s | `glance` |

A protected sender (known contact, `surface` rule, kept) is quick only with
no open action and not as a read. Then the model's own estimate
(`decision.effort`, below) replaces the heuristic's number, and a model
`reply` or `involved` task takes the item out. Then the owner's own dwell on
that sender's mail replaces both, once there are 3 reported looks since
`dwell_ledger_since` (median, plus 10s per open action). Anything over 120s
is left out.

**The plan** (`quick_clear::plan`). Sort by cost (ties newest first), fill a
300s budget, stop at the first item that does not fit. No banner when fewer
than 5 items fit or under 25% of `open_count` fits. Smart-clear rows only
ever add to what fits, and the press the client turns them into runs the same
judgement, so it clears at least what was promised (more, if its model pass
finds expired rows).

**The model's estimate.** agent-triage-v26 added an optional `effort` to
every decision: `{"seconds": n, "task": "glance|skim|quick_action|reply|long_read|involved"}`.
Nullable in the strict schema, absent on every older stored decision (it
defaults), clamped to an hour on write and on read, and dropped when it
claims zero or fewer seconds. It is never a reason to fail a turn.

**Calibration.** Each quick-clear item is stored with its predicted seconds
(`renotify_items.est_secs`, what the queue promised), its estimate before the
owner's multiplier (`raw_secs`), and its basis (`basis`). The multiplier is
learned against `raw_secs`: learned against the promise, which already has
the last multiplier in it, it would measure its own correction and swing
back toward 1 instead of settling. Handled means opened, done, or replied to
SINCE the banner: a card waiting on others usually carries the owner's own
earlier reply, which is no response to a quick clear. What it really took is read
later from the message's own state: the dwell the client reported after the
banner, else open-to-done when that was one sitting (10 minutes or less). A
smart-clear row is handled when a smart-clear press took it after the banner.
Over the last 60 days, measured model and heuristic items give a per-owner
multiplier (actual over predicted, clamped 0.5 to 2.0, 1.0 until 10 items
are measured) that the next plan applies to model and heuristic numbers. Dwell
numbers are already the owner's own clock and are not scaled.

### The frame

A `quick_clear` SSE frame on the Mac stream. Like `renotify`: no `id:`, and a
payload no `Event` decoder accepts.

```json
{"quick_clear": 12, "trigger": "client_online", "at": 1793210000,
 "budget_secs": 300, "est_secs": 284, "fit_count": 14, "open_count": 23,
 "smart_clear_count": 6,
 "items": [{"message_id": 4, "thread_id": "t4", "est_secs": 8,
            "basis": "heuristic", "kind": "receipt",
            "sender": "alice@example.com", "one_line": "Your order shipped"}]}
```

- `quick_clear` is the `renotify_batches` id; the tap is
  `POST /client/renotify/{id}/opened`, as for a re-notify.
- `at` is stamped when the frame is sent, not when the pass began: the
  backlog read sits between the two, and a Mac drops a frame more than a
  minute old.
- `items` are in queue order: smart-clear rows first (`est_secs` 0, basis
  `smart_clear`, kind `smart_clear`), then cheapest first. The client turns
  the smart-clear rows into one smart-clear press.
- `basis` is `smart_clear`, `dwell`, `model` or `heuristic`; `kind` is
  `smart_clear`, `glance`, `receipt`, `delivery`, `account_service`,
  `review`, `attend` or `read`.
- `est_secs` is the queue's total including the one press. `fit_count` is
  `items.length`.
- No account field: as with `renotify`, the stream a frame arrives on is the
  account's.
- The copy is the client's: counts first, and a percentage
  (`fit_count / open_count`) only when it is both flattering and true.

## Delivery

One `renotify` SSE frame on the Mac stream, like `open_draft`. It has no `id:`,
so it moves no cursor, and its payload is no `Event`, so an older app ignores
it:

```json
{"renotify": 12, "trigger": "client_online", "at": 1793210000,
 "items": [{"why": "due_soon", "event_id": 9, "message_id": 4, "thread_id": "t4",
            "kind": "deadline", "importance": 80, "sender": "alice@example.com",
            "one_line": "Invoice due Friday", "deadline": "2026-10-30T17:00:00Z",
            "due_at": "2026-10-30T17:00:00Z"}]}
```

`event_id` is absent for due-soon mail that never had a first banner, and
`due_at` is present only on due-soon items. The app should word the two
differently ("Still waiting" against "Due tomorrow").

It is not a second `events` row. That table is one row per message ever
(`UNIQUE(message_id)`), and the phone's push path reads it. A banner is only
recorded when a Mac was listening to receive it.

## The Mac app

`passband/Sources/Passband/Model/RenotifyBanner.swift` (the frame and the
copy) and the re-notify half of `EventBanner.swift` (the tap), posted by
`Notifier.postRenotify` from `EventStream`.

- **Freshness.** A frame whose `at` is more than a minute off the Mac's clock
  is dropped, the same rule as `open_draft` (`AgentOpenSignal.freshFor`). The
  daemon has already recorded it as fired, so a dropped frame counts as an
  ignored banner.
- **Copy.** One message keeps its first banner's title (the sender) with a
  reminder framing: "Still waiting: ..." or, for `why: due_soon`, "Due
  tomorrow: ...", "Due Friday: ...", "Past due: ...", worded from `due_at`
  (else `deadline`) in the Mac's own calendar. Several are one banner, "N
  emails still need you", naming up to two senders, with a due-soon item
  leading the body. Only due-soon mail chimes.
- **Identity.** The request id is `passband.renotify.<account>.<batch>`, and
  the userInfo has no event id. So it never replaces the first banner, and
  `ToldEvents` never swallows it. The backlog rule reads an `Event`, which
  this is not.
- **Tap.** Opens the first message (`thread_opened` with `source:
  notification`) in the banner's account, switching to it if needed, then
  posts `/opened` with that `message_id`. Errors, including a 404, are
  ignored.
- **Analytics.** `renotify_shown` and `renotify_opened` (docs/ANALYTICS.md).

## Tracking

`renotify_batches` is one row per banner, and `renotify_items` holds the
messages it named. `kind` says which banner it was (`renotify` or
`quick_clear`). A message counted in a quick clear spends none of its
re-notifies and starts no cooldown: the per-message cap and cooldown read
re-notify batches only. The app reports a tap with
`POST /client/renotify/{id}/opened` (optional body `{"message_id": n}`). The
first tap is kept. Whether each message was handled after the banner (opened,
done or archived, or replied to) is worked out on read from the message's own
state, never stored.

`GET /client/renotify/stats` serves the last 30 days by trigger, by kind
(`summary.by_kind`) and by hour of the owner's day, the paused triggers
(`summary.muted` for re-notify, `summary.muted_quick_clear`), and
`quick_clear.calibration`: per basis, items, handled, measured, and predicted
against actual seconds over the measured ones, plus the multiplier.

**Habits, first rung.** A (trigger, kind) pair whose last 8 banners were all
ignored (no tap, nothing handled) is paused for that owner until one of them
is answered. Ignoring quick clears does not pause "still waiting" banners, and
the other way round. Beyond that, only quick clear's multiplier adapts.

## Known limits

- **A zone change re-fires a date-only due date.** "Due the 30th" resolves to
  17:00 in the owner's current zone; if the zone changes, the instant does too,
  and the once-per-due-date record no longer matches.
- **Mac clock skew.** A Mac more than a minute off drops every frame, the
  daemon counts each as ignored, and after 8 the trigger pauses. `open_draft`
  has the same exposure.
- **Point queries.** Candidate reads run a few indexed queries per candidate
  under the store lock, on each pass (not each tick).
- **A quick-clear pass is a heavier read.** When a quick clear could win, the
  pass also runs smart clear's scan (which measures the busiest Reading
  senders) and reads the FYE board, under the store lock, once per pass. A
  pass runs on an arrival or a meeting end, not on every tick.
- Every duration tunable is clamped to a year, so an absurd value cannot make
  the time arithmetic panic.

## Not built yet

- **Scoring `effort` in the eval.** `squelch-eval` marks it unscored: gold
  from before agent-triage-v26 has none, and the real check is the
  calibration against the owner's dwell.
- **A quick clear between arrivals.** Like a re-notify, a quick clear only
  goes out on a trigger.

- **The model layer.** A fast model that ranks or declines within what `plan`
  allows, fed the habit summary. The rules stay authoritative.
- **Deeper per-owner tuning.** Moving the meeting-end delay and the working
  window toward the hours this owner responds in.
- **A filtered inbox for a digest.** A tap on "3 emails still need you"
  opens the first (highest-ranked) message, not a view of all three.
- **The phone.** v1 is Mac only.
- **Retrying a held arrival.** A client-online pass held for working hours or
  the budget is not tried again later. One held for a meeting gets its chance
  when that meeting ends.

## Quick clear: the Mac app

A sibling of the re-notify banner: when the owner is likely at their desk the
daemon offers a short sprint ("5 minutes clears 14 of your 23 emails"), and a
tap opens a queue holding exactly those items. The tap opens the queue, never
the inbox; anything else would make the banner clickbait.

| Piece | File |
|---|---|
| The frame, its freshness, the banner's copy | `passband/Sources/Passband/Model/QuickClearBanner.swift` |
| The tap's userInfo keys and route | `EventBanner.swift` (`quickClearTap`) |
| The queue's rules (next item, handled, end card) | `passband/Sources/Passband/Lib/QuickClearQueue.swift` |
| The sprint's state and verbs | `passband/Sources/Passband/Model/QuickClearSprint.swift` |
| The card | `passband/Sources/Passband/Views/QuickClearView.swift` |

**The frame.** One `quick_clear` SSE frame on the Mac stream, no `id:`:

```json
{"quick_clear": 12, "trigger": "client_online", "at": 1793210000,
 "budget_secs": 300, "est_secs": 284, "fit_count": 14, "open_count": 23,
 "smart_clear_count": 6,
 "items": [{"message_id": 4, "thread_id": "t4", "est_secs": 8,
            "basis": "dwell", "kind": "receipt",
            "sender": "alice@example.com", "one_line": "Your order shipped"}]}
```

Decoded leniently: only `quick_clear` (above 0) is required, plus at least one
item or a `smart_clear_count` above 0. An item that does not decode is dropped
alone; an unknown `basis` reads as `unknown` and `kind` is kept as words. A
budget is clamped to 1..60 minutes, and `open_count` is never read as less than
`fit_count`. Items are kept in the frame's order. Items carry no account: the
stream the frame came on is the account, as for every frame. `kind` is one of
`smart_clear`, `glance`, `receipt`, `delivery`, `account_service`, `review`,
`attend`, `read` (a newer word is kept as words). The `smart_clear` rows come
first with `est_secs` 0: they are what the sweep step clears, so the step is
offered for at least that many even without a `smart_clear_count`, and once
the sweep has run they are already handled. If the sweep is passed over they
come up one at a time like the rest, tagged "expired". The SSE event name is
`quick_clear`; the app decodes by shape, like every frame on this stream. The
freshness rule is re-notify's (`AgentOpenSignal.freshFor`).

**Copy.** Counts first. The percent is added only at 50% or more ("That is
61% of what is waiting."), never 100%. When everything fits it reads "6 quick
ones, about 3 minutes" ("1 quick one, under a minute"). A smart clear count
adds "First, one press clears 6 expired promos and past events." No chime, no
sender tile, no dashes.

**Identity and tap.** The request id is `passband.quickclear.<account>.<batch>`,
with no event or message id in the userInfo. The userInfo carries the frame
itself as JSON (`passband.quickclear.frame`, each line capped), so a tap after
a relaunch opens the queue the banner promised. The `smart_clear` rows are
left out of it, and the 60-item cap counts only the rest: they can number in
the hundreds and the one press clears them anyway, so the payload carries
them as `smart_clear_count`. The tap switches to the banner's account if
needed, closes the reader if one is up (the card is drawn only with no reader
over it), opens the sprint, and posts
`POST /client/renotify/{quick_clear}/opened` with an empty body (no
`message_id`: it opened the queue, not an email). Errors are ignored.

**The sprint.** A card over the board, one item at a time:

- A gentle count-up against the budget on a thin accent track that never turns
  red, and "4 of 14".
- With `smart_clear_count` above 0 the first step is one card, "Clear 6
  expired promos and past events": Enter runs the board's own smart clear
  (`AppStore.runSmartClear`, receipt and 30 second undo included), → passes.
- Then each item: e/d done (`Actions.done`, undo chip and `u`), Enter opens it
  in the reader (the card hides and is back on Esc), h reminds, → or Space
  passes, ← goes back to the last passed, u undoes, Esc stops.
- What is next is derived, never stored: an item is skipped when the store's
  `resolvedIds` holds it or a cached board row says it is done, resolved or
  parked. So an undo brings its item back to the front.
- The end card ("Cleared 12 in 4 minutes") goes up when the queue runs out or
  the budget is spent, with "Keep going" while anything is left (passed items
  come back around). No confetti.

**Analytics.** `quick_clear_shown`, `quick_clear_opened` and
`quick_clear_finished` (docs/ANALYTICS.md). Counts and seconds only.

**Limits.** Mail handled on another device is skipped only once the board's
poll has brought that row; mail that is in no cached list is not known to be
handled until it is opened. The Mac has no archive key, so the sprint's verbs
are the board's: done, remind, open.
