# Product analytics

What Passband reports to PostHog, why each event exists, and the rules that
keep mail out of it. The code is `passband/Sources/Passband/Lib/Analytics.swift`;
this file is the spec the dashboards are built against. Change them together.

## The invariant

Nothing content-shaped leaves the app, and that is enforced rather than
observed. Every event name must be in `allowedEvents` and every String
property value in `allowedStrings`, or the value is dropped (and a debug build
asserts). Bools and ints pass freely: cardinality that small cannot carry mail.
Email data (subjects, senders, bodies, labels, questions, queries) could only
ever leave as a free-form string, so free-form strings cannot leave at all.

The one bypass is the hosted `analytics_id`, adopted as `distinct_id` after a
UUID shape check. Read `Analytics.adopt` before considering a second.

`passband/analytics-lint.py` (run by `test.sh`, so by CI) fails the build on an
event captured outside the vocabulary, a vocabulary entry nothing captures, and
a string literal used as a property value that is not in `allowedStrings`.
Values that arrive through `.rawValue` are the enum's business; when you add an
enum case that reaches analytics, add its raw value to `allowedStrings` in the
same change.

Only release builds report. The PostHog key is compiled in under
`PASSBAND_RELEASE`; a dev build sends nothing unless `PASSBAND_POSTHOG_KEY` is
set in its environment, which is also how to test a change here (point
`PASSBAND_POSTHOG_HOST` at a local listener and read the batches).

## Levels

Settings > Privacy. Read from UserDefaults on every event, so a change applies
to the next capture.

| Level | Sends |
|---|---|
| `none` | Nothing. The hosted id still flips on adoption so a later opt-in arrives as the same person. |
| `minimal` | App opens, screen views, sessions, the counter events below marked M, milestones, and the person properties. |
| `full` | Everything: adds the action verbs (`compose_opened`, `email_labeled`, `whats_new_shown`, `search_deeper_started`) and every `via` / `surface` / `source` property still rides on the M events. |

Every event captured while the practice inbox is up carries `practice: true`,
except the three onboarding-funnel events, which are the real thing. `daily`
events and milestones are not emitted at all from the practice inbox.

## Identity

An install mints a random UUID once. A hosted pairing link carries `aid`, the
control plane's opaque per-person id; the app adopts it as `distinct_id` after
a successful claim and connect, with one `$create_alias` folding the anonymous
history in. First aid wins per install. PostHog never sees an address; the
control plane never sees an event. `Analytics.isHosted` is that adoption flag.

## Person properties

Sent as `$set` on every launch and activation, and whenever the account list
changes (`AppStore.identifyPerson`). Cohort keys, not behavior; ride at
`minimal`.

| Property | Values | Question |
|---|---|---|
| `hosted` | bool | Hosted tenant or self-run daemon. |
| `accounts` | int | How many mailboxes this install runs. |
| `telemetry_level` | `full` `minimal` `none` | Who opted down. |
| `thread_style` | `auto` `classic` `bubbles` | Which reader people settle on. |

Platform, OS version and app version are on every event's envelope already
(`$os_name`, `$device_type`, `$app_version`).

## Milestones

`milestone { kind, day }`, derived inside `Analytics.capture` from the event
that proves it, once per install per kind, stamped in UserDefaults. `day` is
whole days since the install's first reporting launch. The table in
`Analytics.milestones` is the whole definition; a call site cannot forget one.

| kind | Proved by |
|---|---|
| `first_connect` | `connect_succeeded` |
| `first_triage` | `triage_digest` with `total > 0` |
| `first_open` | `thread_opened` |
| `first_action` | `email_done` or `email_archived` |
| `first_send` | `compose_send` with `outcome: sent` |
| `first_rule` | `rule_created` that was not an edit |
| `first_ask` | `assistant_asked` |
| `first_search` | `search_deeper_started` (a `full`-level event, so not at `minimal`) |

Joined with the control plane's `signed_up` and `activated` stamps, this is the
funnel: invited, signed up, paired, connected, triaged, opened, acted, sent.

## Events

M = rides at `minimal`. Properties are bools and ints unless a closed set is
named.

### Lifecycle and funnel

| Event | Properties | Question |
|---|---|---|
| `Application Opened` / `Backgrounded` (M) | | Sessions. PostHog derives duration from `$session_id` (UUIDv7, 30 idle minutes). |
| `$screen` (M) | `$screen_name`: MainView | Which pages get visited. |
| `connect_succeeded` (M) | | The daemon answered with a valid token. |
| `connection_lost` / `connection_restored` (M) | | Transitions only; a healthy poll is not an event. |
| `account_added` (M) | | Second and later mailboxes. |
| `catchup_completed` (M) | `messages`, `minutes` | How long a new install stares at "catching up". Measured by the active mailbox's poller from the first stats poll that reported a backfill to the first that did not; the start survives a relaunch. A backfill shorter than one poll interval is never seen, and a background account's is not measured. |
| `tour_started` (M) | `source`: `first_run` `settings` `rehearsal` | Onboarding funnel. |
| `tour_completed` / `tour_skipped` (M) | `step` | Where the practice inbox loses people. |
| `whats_new_shown` | `releases` | |

### Triage

| Event | Properties | Question |
|---|---|---|
| `triage_digest` (M, daily) | `total`, `sealed`, `band_standing`, `band_new`, `band_open`, `tier_past_due`, `tier_deadline`, `tier_signal`, `tier_noise` | Volume: the denominator for the correction rate and the surfaced-vs-squelched split. |
| `triage_corrected` (M) | `axis`, `to`: the daemon's TriageAxis vocabulary | What triage gets wrong, by axis. |
| `thread_opened` (M) | `via_reply`, `from_noise`, `source` | `from_noise` is the false-negative signal: the opened row's own placement was noise, so someone dug below the squelch line. `source` is which surface earned the open (below). |
| `email_done` / `email_archived` / `email_not_spam` (M) | `via`, `surface` | The verbs, and how and where they were input (below). |
| `email_remind` (M) | | Reminder set from the palette. No origin yet: the palette is opened by key and chosen by key or click, and the choice is what fires. |
| `email_reopened`, `email_labeled` | | In the vocabulary and in `Actions`, but nothing in the app calls `Actions.reopen` or `Actions.label`. Dead until a surface does. |
| `undo_fired` (M) | `kind`: `archive` `done` `label` `ruleDelete` `groupDelete` `remind` `send` | Undo rate per verb, against the verb's own count. |
| `sealed_revealed` (M) | | Sealed mail opened. |
| `shipment_cleared` | | The card's context-menu clear. |
| `notification_opened` (M) | `has_thread` | Banner taps. |
| `renotify_shown` (M) | `trigger`: `client_online` `meeting_end`; `items`, `due_soon`, `foreground` | A smart re-notify banner posted (docs/RENOTIFY.md). `items` is how many messages it named; `due_soon` is whether any of them came back because its due date is near; `foreground` is that the app was frontmost with its window up, so the system filed it into Notification Center with no banner. Posted, not seen: Focus and Do Not Disturb are invisible from here. A frame dropped as stale is not counted. |
| `renotify_opened` (M) | `trigger`, `items` | A re-notify banner tapped. Kept apart from `notification_opened` so that one still means first banners only; the open it leads to is `thread_opened` with `source: notification`. |

**`thread_opened.source`** (`OpenSource`): `sitrep` (for-your-eyes rows),
`sitrep_card` (record zones: banking, shipments, receipts, calendar),
`reading` (a Reading sender's stack), `emails` (the list, any mode), `search`
(a search hit), `browse` (the browse-all panel), `assistant` (a citation or
card in the ask bar), `deeper_search` (the reader's search strip), `groups`
(a group's send history), `notification` (banner or push), `reply` (`r` on a
row: opens the reader on its composer), `reader` (the walk from one queued
thread to the next: an open the reader made, not the person; subtract it from
"threads people chose to open"), `tour` (the practice inbox's examples).

**`via` / `surface`** on the verbs (`Actions.Via`, `Actions.Surface`):
`via` is how the verb was input: `key`, `click`, `swipe` (a phone rail), `assistant` (an agent tool; no surface). `surface` is
the page: `sitrep`, `emails`, `reader`. Keyboard adoption is
`via = key` over all verbs; where triage happens is `surface`.

### Compose

| Event | Properties | Question |
|---|---|---|
| `compose_opened` | `kind`: `new` `reply` `forward` | |
| `compose_send` (M) | `kind`, `outcome`: `sent` `guard_blocked` `forbidden` `failure`; `override`, `tracked`, `copied`, `blind`, `attachments`, `inline_images`; `group_mode`: `to` `bcc` `individual` | Whether sends go, what shape they take. Nothing about who. |

### Search and assistant

| Event | Properties | Question |
|---|---|---|
| `assistant_asked` (M) | `model`: `haiku` `opus`; `transport`: `relay` `byok` | Questions typed in chat. Lane turns are deliberately not asks. |
| `search_deeper_started` | `model`; `trigger`: `question` `no_strict_hits` `requested` | Which rule started a deeper lane. One per conversation, including restarts. |
| `thread_live_arrival` (M) | `followed` | Mail landed in the open thread; was the reader carried to it. |

Neither search nor the assistant has an outcome event yet. That is the next
PR: `search_settled { turns, hits, opened, lane }` and
`assistant_answered { tool_calls, cited, error }`.

### Rules, groups, sharing

| Event | Properties | Question |
|---|---|---|
| `rule_created` (M) | `has_want`, `edit`, `suggested`, `from_suggestion`; `disposition`: `surface` `squelch` `filtered` | Does the card's suggested sentence get taken. |
| `rule_deleted` (M) | `disposition` | |
| `block_rule_created` (M) | `smart` (bool, only when true) | `x` on a sender, or the ignored-unsubscribe notice. `smart` is the notice's filtered rule in place of a mute. |
| `group_created` / `group_updated` / `group_deleted` (M) | `mode`: `to` `bcc` `individual`; `members` | Audience shape and size. Never who. |
| `invite_sent` (M) | `count`, `failed`; `source`: `rail` `settings` `nudge` | Which surface drives shares. |
| `invite_nudge_accepted` / `invite_nudge_dismissed` (M) | | The two-week ask. |
| `ios_nudge_closed` (M) | `copied` | The TestFlight ask closing, and whether the link was copied. A QR scan leaves no trace, so this is a floor on uptake. |
| `agent_nudge_closed` (M) | `opened` | The Agent MCP ad closing, and whether its button went to Settings > Agents. |

## Dashboards to build

None are defined in the repo; PostHog holds them. The six that cover the
questions above:

1. **Activation funnel.** `milestone` by kind, as a funnel per person, split
   by `hosted`. Join to control-plane signups by `distinct_id`. `day` on each
   step is time-to-value.
2. **Habit.** DAU/WAU, sessions per person per day, verbs per session
   (`email_done` + `email_archived` + `compose_send`), retention by weekly
   cohort of `first_connect`.
3. **Triage quality.** `triage_corrected` / `triage_digest.total`;
   `thread_opened` with `from_noise` / all opens; `email_not_spam`;
   `undo_fired` by kind over the matching verb.
4. **Surfaces.** `thread_opened` by `source` (minus `reader`); verbs by
   `surface`; `via = key` share over time per person (keyboard adoption);
   `$screen` by name.
5. **Compose and search.** `compose_send` outcome mix and attachment use;
   `assistant_asked` by model and transport; `search_deeper_started` by
   trigger.

6. **Mac vs iOS.** Every event already carries `$os_name` (`macOS` / `iOS`),
   compiled per target, so no event is needed for this, only a breakdown.
   Filter `practice` is not set on all of them.
   - Reach: unique users of `Application Opened`, weekly, broken down by
     `$os_name`.
   - Overlap: the same series with breakdown off and a formula, `A + B - C`
     where A and B are the event filtered to each `$os_name` and C is
     unfiltered: the people on both. Honest for hosted users only, where both
     installs adopt the same `aid`; a self-run person on two devices is two
     people.
   - Depth: total count of `thread_opened` (minus `source = reader`),
     `email_done` + `email_archived`, and `compose_send` with `outcome = sent`,
     each divided by unique users, by `$os_name`. This is the usage RATE:
     actions per active person per week on each app.
   - Stickiness: `Application Opened` stickiness (days active per week) by
     `$os_name`.
   - Sessions: session count and median `$session_duration` by `$os_name`.

   The operator's view of the same question, per tenant and with no opt-out,
   is the Mac acted / iOS acted columns on the Grafana tenant table.

## Adding an event

1. Name it in `allowedEvents`, and in `minimalEvents` if it measures whether
   the product works rather than which feature was used.
2. Every String value it can carry goes in `allowedStrings`, including enum
   raw values.
3. Capture it. If it proves a first-time milestone, add a row to
   `Analytics.milestones` instead of capturing a milestone yourself.
4. Add it to this file with the question it answers.
5. `./passband/test.sh` runs the lint. A debug build still asserts at the call
   site if a value slips past, which is the check the lint cannot do for
   `.rawValue` values.
