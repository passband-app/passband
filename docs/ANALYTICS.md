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
| `full` | Everything: adds the action verbs (`compose_opened`, `email_labeled`, `whats_new_shown`, `search_deeper_started`) and every `via` / `surface` / `source` property still rides on the M events. On the Mac, also session replay (below) and its two frustration events. |

Every event captured while the practice inbox is up carries `practice: true`,
except the three onboarding-funnel events, which are the real thing. `daily`
events and milestones are not emitted at all from the practice inbox.

## Session replay (Mac, `full` only)

PostHog has no macOS replay, so the app speaks its WEB replay format by
hand: `$snapshot` events to `/s/` (gzipped, a lane of their own in
`PostHogClient`, `$snapshot_source: web`) carrying rrweb records, under the
same `$session_id` as the events, so a recording in PostHog shows the
session's events on its timeline. Each full snapshot is a tiny HTML page:
`<body>` is the window, and every box is an absolutely positioned `<div>` (an
icon is an `<img>` of our own render).

**Not the mobile format.** PostHog's mobile "wireframes" format looks like the
natural fit and is what this first shipped with. PostHog accepts mobile-format
uploads from a custom client with `200 OK` and then never stores them
(verified 2026-10-28: the same 20-second probe session played in web format
and was "not found" in mobile format, same key, same project). Do not switch
back without re-running that probe.
Code: `Lib/SessionReplay.swift` (when to record) and
`Lib/ReplayWireframe.swift` (what a frame is).

**What a frame is.** The window's Core Animation layer tree, reduced to
rectangles. Read per layer: frame, visibility, opacity, clipping, and only
WHETHER it has a fill, border or corner radius. Every box is drawn in one of
a few fixed greys (`ReplayWireframe.panelPaint` and friends) whatever its real
color, because some fills are derived from mail (a reading tile's logo
background, an avatar's hue, a triage-colored pill) and even luminance told
them apart. The window background is the one real paint (the theme). Never
read: `contents` (rendered text, avatars, images) or a `CATextLayer`'s
string. Web views, text fields and text views (`isKind(of:)` on the layer's
view, so app subclasses count) are one box each and the walk never enters
them, on screen or off. A text run becomes a bar snapped outward to an 8pt
grid on all four edges, and every other box snaps outward to the same grid
(web views and text fields to a 32pt one), so no exact size survives: a
recipient pill's width would otherwise be its address's length, and a web
view's height (it sizes to the mail) a fingerprint of the newsletter.
Tooltip and menu windows are never drawn (a tooltip can be a sender).

**What the layout still says.** Which cards and icons appear is chosen by
triage, so a recording shows what kinds of mail were on screen (a bill card,
a shipment row, a lock icon) and when. The Settings hint, privacy.html and
trust.html say so; do not let them drift back to "nothing about your mail".

**Icons.** A small bitmap (48pt or less) is read LOCALLY by
`Lib/ReplayIcons.swift` to ask one question: does its shape clearly match one
of the app's own icon symbols (`ReplayIconNames`, generated by
`passband/replay-icons.py` from `systemName:`/`systemImage:` lines and
`symbol`/`icon`/`glyph` properties, minus every letter-, digit- and
shape-like symbol, since a probe matched "A" to `a`, "B" to `bold` and a
ringed avatar to `circle`)? Multicolor bitmaps (emoji, logos) are never
matched, and only leaf layers are read. The match needs cosine 0.95 or better on a 20×20
ink mask and a 0.02 lead over the next symbol. If it matches, the frame
carries OUR render of that symbol in grey, a data URL the validator accepts
only because this process registered it. If not, the layer stays a box. The
layer type alone cannot be trusted: SwiftUI draws avatar initials, dates and
icons the same way. Measured on the sitrep, real icons score 0.97 to 0.995,
text under 0.8; a photo in a circle matches several filled symbols about
equally and fails the margin. `test.sh` fails if a `systemName:` literal is
missing from the list, or the generated file is stale.

Sheets, popovers and panels over the window are drawn
onto the same canvas. The meta record's `href` is the current `$screen_name`.

**The guard.** `Analytics.replay` runs `ReplayWireframe.validate` on every
record: each key must be an rrweb field name, and each String one of the five
tags we emit (`html` `head` `body` `div` `img`), a style string in exactly the
grammar `ReplayWireframe` writes with grey `#LLLLLLAA` paints only, the screen
URL `https://passband.app/app/<screen>` for an `allowedStrings` screen name, or
an icon data URL this process rendered. Anything else drops the
record (and asserts in debug). `Tests/ReplayWireframeTests.swift` holds the
contract against a layer tree with a subject, an avatar, a hued pill and a web
view in it.

**When.** A walk every 0.5 s on the main thread, plus one per click; a frame
is sent only when it changed, at most once a second. Nothing while the app is
in the background or the window is hidden, and nothing after 2 minutes
without a click, key or scroll, so an idle open window does not keep a
session alive. A click after a pause sends a full frame first. Clicks are
rrweb mouse interactions on `<body>` (down, up, click) at canvas points from
the window's top-left.

**Activity.** PostHog's recordings list hides recordings with 5 or fewer
active seconds by default, and counts them from pointer records, not frames
(PostHog#91388). So the pointer is sampled each tick, and any
movement, scroll, key or click since the last tick goes out as one rrweb
mouse-move at the pointer's position. Never which key.

| Event | Properties | Question |
|---|---|---|
| `$rageclick` | `x`, `y`, `$screen_name` | Three clicks within 30pt inside a second. PostHog's name, so the recordings list filters on it. |
| `$dead_click` | `x`, `y`, `$screen_name` | A click after which the wireframe did not change for 1.5 s. Heuristic: a click that only moves focus, drags the window, or swaps an icon reads as dead. |

Neither counts a click inside a web view or text field (a triple-click there
selects; a link may have opened the browser), and a click that opens a menu is
never dead (menus are not drawn).

The `/s/` body is posthog-ios's shape: a bare JSON array, each event with
its own `api_key`, a `uuid`, and `distinct_id` repeated in `properties`.
Buffered frames are dropped, not sent, once the level falls below `full`.

PostHog side: the project must have session recording turned on. These are
web recordings for billing.

## Identity

An install mints a random UUID once. A hosted pairing link carries `aid`, the
control plane's opaque per-person id; the app adopts it as `distinct_id` after
a successful claim and connect, with one `$create_alias` folding the anonymous
history in. First aid wins per install. `Analytics.isHosted` is that adoption
flag.

Hosted people are named in PostHog by control, not by the app: signup sends one
`$set` with `email`, `name` and `hosted: true` on the `aid`
(`squelch-control/src/posthog.rs`), and `squelch-control analytics identify`
backfills everyone who signed up. The apps never send an address, so a
self-hosted install stays a bare UUID. The control plane never sees an event.

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
| `calendar_reminder_shown` (M) | `lead_minutes`, `all_day`, `has_line`, `has_ticket`, `ghost` | An event reminder posted by the app off its event feed: the Mac, or a phone without push (docs/CALENDAR.md, "Reminders and tickets"). `lead_minutes` is the plan's lead (0 when the daemon sent none); `has_line` is that the banner carried the model's line, `has_ticket` that it carried "Show tickets", `ghost` that it was for an event guessed from mail. Posted, not seen. Shown over the app too, unlike mail banners. A frame dropped as stale, or for a mailbox whose switch is off, is not counted. The phone's pushed reminders are drawn by its notification extension and are not counted at all. |
| `calendar_reminder_opened` (M) | `target`: `attachment` `mail` `event` `none`; `show_tickets` | A reminder tapped, on either platform. `target` is what it opened (an attachment in Quick Look, the email in the reader, the event on the Calendar page, or nothing but the app); `show_tickets` is that the "Show tickets" action was pressed rather than the banner. |

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
