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
| Sessions, candidates, busy times, banner record | `squelch-core/src/store/sqlite/renotify.rs` |
| Tunables | `RenotifyConfig` in `squelch-core/src/config.rs` (`[renotify]`, `SQUELCH_RENOTIFY_*`) |
| Worker, frame, routes | `squelch-api/src/renotify.rs` |
| Session + frame on the Mac stream | `squelch-api/src/events.rs` |
| Spawned | `squelchd/src/bin/squelchd.rs` |

## Triggers

**Client online.** Every Mac event stream (`GET /client/events?opens=1`) is a
row in `client_sessions` for as long as it is open. When one opens and the Mac
had been disconnected for at least `min_offline_mins` (default 120), the stream
wakes the worker. The worker waits `online_settle_secs` (default 30) so the
app's own event replay lands first and a wake that sleeps again at once is
dropped. If the session is still open, it runs a pass.

A first-ever connection is onboarding, not an arrival. A second stream while
one is open is no gap at all. A daemon restart closes the sessions it left open
at startup time, which can only make the next gap look shorter.

**Meeting end.** Once a minute, while a Mac is listening, the worker reads the
owner's busy intervals. A meeting's window opens `meeting_end_delay_mins` after
it ends (default 3) and stays open `meeting_end_window_mins` (default 10). A
block longer than `max_meeting_mins` (default 240) is not a meeting. An end that
falls inside another busy interval (back-to-back meetings) is no gap.

## The rules (`renotify::plan`)

Checked in this order. The first one that fails is the hold reason.

1. Not in a meeting.
2. No meeting starting within `meeting_guard_mins` (default 15).
3. Inside working hours in the owner's zone (`work_start_hour`..`work_end_hour`,
   default 7 to 20; the zone is the one `owner_zone` already resolves).
4. Under `daily_budget` banners since local midnight (default 4).
5. The trigger is not paused by the owner's habits (below).
6. At least one candidate is due.

There are two kinds of candidate. A message can be both; it is then named
once, as due soon.

**Unhandled** (`why: "unhandled"`). The store's query: it has an `events` row, is not an open-tracking
notice or an auth alert, is received mail that is not spam, has not been opened,
done, archived or given a reminder, and has no reply on its thread since.

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
  reminder framing: "Still waiting: ..." or, for `reason: deadline`, "Due
  tomorrow: ...", "Due Friday: ...", "Past due: ...", read in the Mac's own
  calendar. Several are one banner, "N emails still need you", naming up to
  two senders, with a deadline leading the body. Only a deadline chimes.
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
messages it named. The app reports a tap with
`POST /client/renotify/{id}/opened` (optional body `{"message_id": n}`). The
first tap is kept. Whether each message was handled after the banner (opened,
done or archived, or replied to) is worked out on read from the message's own
state, never stored.

`GET /client/renotify/stats` serves the last 30 days by trigger and by hour of
the owner's day, plus the paused triggers.

**Habits, first rung.** A trigger whose last 8 banners were all ignored (no tap,
nothing handled) is paused for that owner until one of them is answered.
Nothing else adapts yet.

## Not built yet

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
