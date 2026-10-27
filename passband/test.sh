#!/usr/bin/env bash
# Run the Swift unit tests. swiftc-driven for the same reason build.sh is (see
# its header: xcodebuild on this machine is not to be trusted), and no XCTest
# because there is no test bundle host without it. Each suite is a @main
# executable compiled from PURE source files — the point of keeping the wire
# decoder synchronous and network-free is that it builds with just its data
# types: no app, no simulator, no key.
#
#   ./test.sh          build and run every suite
#
# The suites are independent, so they build and run N at a time (N = cores).
# Each run_suite call below only QUEUES its suite; the xargs at the bottom of
# the file is what runs the queue. A suite's output is held back and printed
# as one block when it finishes, so two suites never interleave on the
# terminal, and a failure in one does not stop the others: every red suite
# is reported, and the script exits non-zero if any was.

set -euo pipefail
cd "$(dirname "$0")"

BUILD=build/tests
mkdir -p "$BUILD"

# The analytics vocabulary is closed at RUNTIME (an unlisted event asserts in
# a debug build and is dropped in release), and nothing else at build time
# knows that. This is the check; see the script's header for the history.
echo "==> analytics-lint"
python3 ./analytics-lint.py

QUEUE=$(mktemp)
trap 'rm -f "$QUEUE"' EXIT

run_suite() {
  printf '%s\n' "$*" >> "$QUEUE"
}

# One suite start to finish: compile, run, print. Runs under xargs below, in
# its own bash, which is why it is exported along with BUILD.
run_one() {
  local name="$1"
  shift
  local log="$BUILD/$name.log"
  local status=0
  {
    echo "==> $name"
    xcrun swiftc -swift-version 6 -parse-as-library -Onone \
      -o "$BUILD/$name" Sources/Passband/Lib/RehearsalMode.swift "$@" \
      && "$BUILD/$name" \
      && case "$name" in
        # Full connection rehearsal must work even when normal onboarding was
        # completed.
        tour-controller) "$BUILD/$name" --onboarding-rehearsal --rehearse-connection ;;
      esac
  } > "$log" 2>&1 || status=$?
  cat "$log"
  return $status
}
export -f run_one
export BUILD

# The hero, peek and fire-and-forget preload must share one request.
run_suite thread-prefetch \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Model/ThreadPrefetch.swift \
  Tests/ThreadPrefetchTests.swift

run_suite calendar-visibility \
  Sources/Passband/Lib/CalendarVisibility.swift \
  Tests/CalendarVisibilityTests.swift

# The calendar page's arithmetic against the wire it reads: which rows are
# drawn, which day they land on in the user's zone, their order, how solid
# each is, what folds away, every time string, and the gate. Each is only ever
# wrong on somebody's screen at the wrong hour, so it is pinned to a fixture
# week rather than reasoned about.
run_suite agenda \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Agenda.swift \
  Tests/AgendaTests.swift

# The month view's arithmetic: the grid's days in the user's calendar, what
# each cell lists and what folds into "+N more", spans and their lanes, the
# unlikely counts, the day column's hours (the night cap, 7am, past midnight,
# the DST Sundays), the keys' moves, the range the page asks for, the
# month-first default, and the calendar colours brought into the palette.
run_suite month-grid \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Agenda.swift \
  Sources/Passband/Lib/MonthGrid.swift \
  Sources/Passband/Lib/CalendarColor.swift \
  Tests/MonthGridTests.swift

# The week view (wave 6): which seven days (the user's first weekday, the DST
# weeks), that the week is a row of the month the page asks for, overlapping
# events side by side per day, the strip of all-day items and trips in the
# month's own lanes, the draft among the blocks, widths, words and j / k.
run_suite week-grid \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Sources/Passband/Lib/Agenda.swift \
  Sources/Passband/Lib/MonthGrid.swift \
  Sources/Passband/Lib/WeekGrid.swift \
  Sources/Passband/Lib/CalendarWrite.swift \
  Sources/Passband/Lib/EventDraft.swift \
  Sources/Passband/Lib/TitleParse.swift \
  Sources/Passband/Lib/EventGuests.swift \
  Tests/WeekGridTests.swift

# The event editor: typed dates and times, what blocks a save, the exact
# body sent (timed with offsets, all-day with the INCLUSIVE last day), an
# edit's patch carrying only what moved, the route's path, the daemon's
# answer laid into the feed, and the wave-3 wire decoding.
run_suite event-draft \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Sources/Passband/Lib/Agenda.swift \
  Sources/Passband/Lib/CalendarWrite.swift \
  Sources/Passband/Lib/EventDraft.swift \
  Sources/Passband/Lib/TitleParse.swift \
  Sources/Passband/Lib/EventGuests.swift \
  Tests/EventDraftTests.swift

# The title that understands "dinner with steven on friday 4pm" (wave 6): a
# table of phrases and what each fills, the negatives that must fill nothing,
# the saved title, the names, the fields a title may and may not write, the
# way back (deleted words, Esc), and who is offered as a guest.
run_suite title-parse \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Sources/Passband/Lib/Agenda.swift \
  Sources/Passband/Lib/CalendarWrite.swift \
  Sources/Passband/Lib/EventDraft.swift \
  Sources/Passband/Lib/TitleParse.swift \
  Sources/Passband/Lib/EventGuests.swift \
  Tests/TitleParseTests.swift

# The assistant's one calendar tool (wave 6): offered only with the Calendar
# page (off, the inventory is main's), a proposal read and turned into the
# pane's own draft by the pane's own rules, guests only where the calendar may
# invite, every bad proposal an answer and never a draft, a pane in use never
# replaced, and what the model is told did NOT happen.
run_suite event-proposal \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Sources/Passband/Assistant/JSONValue.swift \
  Sources/Passband/Lib/Agenda.swift \
  Sources/Passband/Lib/CalendarWrite.swift \
  Sources/Passband/Lib/EventDraft.swift \
  Sources/Passband/Lib/TitleParse.swift \
  Sources/Passband/Lib/EventGuests.swift \
  Sources/Passband/Lib/EventProposal.swift \
  Tests/EventProposalTests.swift

# Guests on an event (wave 5), the first write that makes Google email other
# people: how the guests line is read (bare addresses, one per person, the
# cap), which calendars may invite and when the line is closed, which save
# controls a draft shows and in what words, that Enter never saves a draft
# whose save would tell anyone, that `notify` is true only from a control
# that says so, the bodies on the wire (the whole list on a patch, nothing
# from a closed line), the delete question by count, a guest's local time,
# and the wave-5 wire decoding.
run_suite event-guests \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Sources/Passband/Lib/Agenda.swift \
  Sources/Passband/Lib/CalendarWrite.swift \
  Sources/Passband/Lib/EventDraft.swift \
  Sources/Passband/Lib/TitleParse.swift \
  Sources/Passband/Lib/EventGuests.swift \
  Tests/EventGuestsTests.swift

# Drag to create on the day column: a press and a drag become a span of time.
# The snap to the quarter hour, the quarter-hour minimum, dragging upward, the
# clamp at the day's ends, the folded night's own scale, and what counts as a
# click rather than a sweep.
run_suite day-sweep \
  Sources/Passband/Lib/DaySweep.swift \
  Tests/DaySweepTests.swift

# The location field's lookup, with a fake where MapKit goes: what is asked
# and when (debounce, two characters), stale answers dropped, arrows and
# Enter, Escape closing the list and not the pane, and the rule that typed
# text is replaced only by a pick.
run_suite place-lookup \
  Sources/Passband/Lib/PlaceLookup.swift \
  Tests/PlaceLookupTests.swift

# The real poller against a cancellation-oblivious transport: mailbox switches
# must detach old requests before the next mailbox starts warming. Then its
# cadence: what each presence pulls, what it lets finish, and what it shares.
run_suite sitrep-poller \
  Sources/Passband/Lib/Concurrency.swift \
  Sources/Passband/Model/SitrepPoller.swift \
  Tests/SitrepPollerTests.swift

# The launch image warm: the rows on screen at once, the rest after a quiet
# stretch, never on Low Power Mode or a costly path, never for an old account.
run_suite image-warmer \
  Sources/Passband/Lib/Concurrency.swift \
  Sources/Passband/Model/ImageWarmer.swift \
  Tests/ImageWarmerTests.swift

run_suite onboarding-rehearsal \
  Sources/Passband/Model/OnboardingRehearsal.swift \
  Tests/OnboardingRehearsalTests.swift

run_suite rehearsal-api \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Sources/Passband/Model/OnboardingRehearsal.swift \
  Sources/Passband/Model/RehearsalAPI.swift \
  Tests/RehearsalAPITests.swift

# The shipment row with and without the order-linking fields, and the card's
# title and order line built from it.
run_suite shipment-wire \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Tests/ShipmentWireTests.swift

# Credential validation must never succeed against the fixture transport.
run_suite credential-probe \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Sources/Passband/Lib/CalendarWrite.swift \
  Sources/Passband/Model/APIClient.swift \
  Sources/Passband/Lib/ClientIdentity.swift \
  Sources/Passband/Lib/TriageTargets.swift \
  Sources/Passband/Assistant/JSONValue.swift \
  Tests/CredentialProbeTests.swift

run_suite anthropic-stream \
  Sources/Passband/Assistant/JSONValue.swift \
  Sources/Passband/Assistant/AnthropicStream.swift \
  Tests/AnthropicStreamTests.swift

# What the app tells its daemon it is. The daemon drops anything that is not
# <platform>/<dotted digits>, so the client must never build anything else.
# The plist is linked in as the binary's Info.plist so the suite can read a
# bundle version the way the app does; a bare test binary has none.
run_suite client-identity \
  Sources/Passband/Lib/ClientIdentity.swift \
  Tests/ClientIdentityTests.swift \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
  -Xlinker Tests/ClientIdentityTests.plist

# Pairing pulls in Sessions.swift only because it builds its own URLSession
# there; nothing in this suite makes a request. Same payoff as above: the code
# normalizer and the deep-link parser are pure, so they test with no daemon.
run_suite pairing \
  Sources/Passband/Lib/Sessions.swift \
  Sources/Passband/Model/Pairing.swift \
  Tests/PairingTests.swift

# The changelog and the rule that decides which of it a given install is owed.
# One file, no dependencies: ReleaseNotes.swift is kept free of SwiftUI and
# Bundle precisely so this suite and make-changelog.sh can both compile it
# alone, and so the running version arrives as an argument rather than as
# whatever bundle the binary happens to sit in. The iPhone's own table rides
# along: it is keyed by the phone's version and asks the same rules.
run_suite release-notes \
  Sources/Passband/Lib/ReleaseNotes.swift \
  Sources/Passband/Lib/PhoneReleaseNotes.swift \
  Tests/ReleaseNotesTests.swift

# One file, no dependencies at all — SubjectText is kept that way on purpose.
# The marker sanitizer is what lets a stranger's subject line sit inside the
# assistant's system prompt, so it gets asserted rather than reasoned about.
run_suite subject-text \
  Sources/Passband/Model/SubjectText.swift \
  Tests/SubjectTextTests.swift

# The attachment buckets against the wire type they bucket. WireTypes is pure
# Codable structs, so the pair builds with no app and no daemon.
run_suite attachment-kinds \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/AttachmentKinds.swift \
  Tests/AttachmentKindsTests.swift

# Where a stranger's filename lands on disk, and which renderer draws it. The
# staging rules are the last thing standing between an attachment named
# `invoice.html` and WebKit, so they are asserted rather than read.
run_suite staged-attachment \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/AttachmentKinds.swift \
  Sources/Passband/Lib/StagedAttachment.swift \
  Tests/StagedAttachmentTests.swift

# The composer's attachment markers: how a picture is placed in the body and
# taken back out, and the token the daemon's own alphabet accepts. Pure string
# work over the wire type and the mime buckets — the send-side twin of
# cid-images, and a stray edit to the marker grammar would silently make every
# dropped picture a plain file.
run_suite compose-attachments \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/AttachmentKinds.swift \
  Sources/Passband/Lib/ComposeAttachments.swift \
  Tests/ComposeAttachmentsTests.swift

# Scheduled send, the composer's half: the verb the button prints, the closed
# list a draft saves (the send time is NOT on it), the schedule body being the
# send body plus a time, and the composer an undo reopens from the daemon's
# cancel. And the Sent page's half: what an outbox row says in each state, the
# order it sits in, a failure toasting once, a cancel's undo rebuilding the
# schedule, and the tombstone ledger drafts and the outbox share. Every one of
# these is wrong only in somebody's outbox, later. And the read-only preview
# a row opens: its status line, its header, its keys, and the M2 rule (edit
# arms the save at once and says what it replaced).
# ComposeState lives in its own pure file for exactly this suite.
run_suite scheduled-send \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/APIError.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SenderIdentity.swift \
  Sources/Passband/Lib/Recipients.swift \
  Sources/Passband/Lib/AttachmentKinds.swift \
  Sources/Passband/Lib/ComposeAttachments.swift \
  Sources/Passband/Model/GroupToken.swift \
  Sources/Passband/Model/ComposeState.swift \
  Sources/Passband/Lib/ScheduledSend.swift \
  Sources/Passband/Lib/Tombstones.swift \
  Tests/ScheduledSendTests.swift

# The blocking re-triage modal's state machine. Pure value logic, so it builds
# with the wire type alone — no store, no daemon, no SwiftUI.
run_suite retriage-run \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/SyncStatus.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Model/RetriageRun.swift \
  Tests/RetriageRunTests.swift

run_suite email-images \
  Sources/Passband/Lib/ImageProxy.swift \
  Tests/EmailImagesTests.swift

# What the fetcher falls back on when a host's Content-Type will not say image:
# the opening bytes. Pinned in both directions, since nil here IS the refusal.
run_suite image-sniff \
  Sources/Passband/Lib/ImageSniff.swift \
  Tests/ImageSniffTests.swift

# The OTHER image rewrite: `cid:` references to this message's own parts. Pure
# string and url work, so it builds with the wire type and the buckets it gates
# on — no WebKit, no daemon. MailCSP rides along because the policy those urls
# are loaded under is the one line of the reading frame that can be asserted
# without standing a content process up, and Trackers because it runs FIRST in
# the real pipeline: what it takes out of a body is what neither answer here may
# still claim.
run_suite cid-images \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/AttachmentKinds.swift \
  Sources/Passband/Lib/HTMLImg.swift \
  Sources/Passband/Lib/ImageProxy.swift \
  Sources/Passband/Lib/Trackers.swift \
  Sources/Passband/Lib/CidImages.swift \
  Sources/Passband/Lib/MailCSP.swift \
  Tests/CidImagesTests.swift

# The thread minimap's window math. CoreGraphics only — the rail that draws it
# is SwiftUI, the arithmetic that aims it is not. ThreadStyle comes along
# because a bubble is a narrower measure than a card, which the guess has to
# know; the enum is kept free of SwiftUI and of the account for exactly this.
run_suite minimap-geometry \
  Sources/Passband/Lib/MinimapGeometry.swift \
  Sources/Passband/Lib/ThreadStyle.swift \
  Tests/MinimapGeometryTests.swift

# The automatic thread style: a guess about somebody's mail, so it is asserted
# fixture by fixture rather than reasoned about. Quotes comes along because the
# length test is fed by the quote splitter — a reply under forty lines of chain
# is a short message — and both files are pure Foundation for this reason.
run_suite thread-style \
  Sources/Passband/Lib/ThreadStyle.swift \
  Sources/Passband/Lib/Quotes.swift \
  Tests/ThreadStyleTests.swift

# The banking card's recency window (issue #82): 24h or since-last-open,
# whichever reaches further back. Platform rides along for the notification
# names the lifecycle plumbing observes.
run_suite sitrep-window \
  Sources/Passband/Lib/Platform.swift \
  Sources/Passband/Lib/SitrepWindow.swift \
  Tests/SitrepWindowTests.swift

# When the two-week ask fires, and every way it must not. Same shape as the
# window rule above: the decision is a pure static, so it tests without a
# daemon, a clock, or a UserDefaults this suite would have to clean up after.
run_suite share-nudge \
  Sources/Passband/Lib/Platform.swift \
  Sources/Passband/Lib/ShareNudge.swift \
  Tests/ShareNudgeTests.swift

# The discovery ledger: found on the first move, kept on the third distinct
# day, never regressing, and a blob from another build costing nobody their
# ring. Foundation only; the UserDefaults shell around it is Discovery.swift.
run_suite discovery-ledger \
  Sources/Passband/Lib/DiscoveryLedger.swift \
  Tests/DiscoveryLedgerTests.swift

# When the iPhone ask fires: two days in, never to somebody whose mailbox has
# seen the phone app, never on a daemon that has not said, and once.
run_suite testflight-nudge \
  Sources/Passband/Lib/Platform.swift \
  Sources/Passband/Lib/ShareNudge.swift \
  Sources/Passband/Lib/TestFlightNudge.swift \
  Tests/TestFlightNudgeTests.swift

# When the agent-door ad fires: a week in, once, only at a door an agent can
# actually use, never to somebody who has been to Settings > Agents or whose
# mailbox an agent has already written to, and never on the iPhone ad's heels.
run_suite agent-nudge \
  Sources/Passband/Lib/Platform.swift \
  Sources/Passband/Lib/ShareNudge.swift \
  Sources/Passband/Lib/TestFlightNudge.swift \
  Sources/Passband/Model/AgentConnect.swift \
  Sources/Passband/Lib/AgentNudge.swift \
  Tests/AgentNudgeTests.swift

# What "remind me…" resolves to. Foundation only, and every date computed
# through an injected now/calendar — which is the whole reason it is testable:
# a reminder is only ever wrong LATER, so the arithmetic has to be pinned here
# rather than discovered by an email that never came back.
# Who a message is addressed to. One file, no dependencies — Recipients.swift is
# kept free of SwiftUI precisely so the move-to-Bcc rule can be asserted without
# an app, because every way it can go wrong is invisible on screen: a list split
# on the wrong comma, or an address left in To while the sender believes it went
# out blind.
# The composer's list grammar AND the reader's side of it — one type, because
# two splitters would eventually disagree about `"Doe, John" <j@x>`. Display
# names come from SenderIdentity, which is why its (pure) dependencies ride
# along here.
run_suite recipients \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SenderIdentity.swift \
  Sources/Passband/Lib/Recipients.swift \
  Tests/RecipientsTests.swift

run_suite remind-times \
  Sources/Passband/Lib/RemindTimes.swift \
  Tests/RemindTimesTests.swift

# Which senders leave the device. `eligibleFaviconDomain` is the privacy
# boundary in SenderIdentity — a human correspondent answers nil and that nil
# is why the correspondent graph stays local — so the brand/robot heuristics
# guarding it are asserted rather than reasoned about. Pure string work;
# WireTypes and Format ride along for the Tier and the capitalizer.
# The wire contract behind the disconnected banner, and the RFC3339 shapes its
# since-when has to survive. WireTypes carries the object; Format carries the
# parser that tries both fractional and plain, which is the bug this pins.
run_suite gmail-health \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Tests/GmailHealthTests.swift

# The one derived sync/triage status and its copy. A rate-limited mailbox once
# read as "synced just now" and "You're all clear."; the precedence and the
# empty-hero rule are what stop that, so they are asserted.
run_suite sync-status \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/SyncStatus.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Tests/SyncStatusTests.swift

# The due-today count, which is now the app icon's badge as well as the sitrep's
# headline. It gets a suite because of the badge specifically: a sentence on a
# dashboard is read with the list under it and corrects itself, while a red pill
# on a home screen is read with nothing beside it at all.
run_suite need-today \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/NeedToday.swift \
  Tests/NeedTodayTests.swift

# The spam page's wire contract, against verbatim daemon responses. It exists
# because the seam it covers is checked by nobody else: these suites never build
# APIClient, and the Rust tests know nothing of Swift's types. A response type
# typed wrong there fails at the only place it can still be seen — the client
# read a decode error on a fire-and-forget POST as a failed request and reported
# that it could not reach a spam folder it had just synced perfectly.
# Auto-translate's wire (docs/TRANSLATION.md): every field tolerant, because
# the notification extension decodes the same message, and which version of a
# message the reader shows, under which frame key.
run_suite translation-wire \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/TranslationRefresh.swift \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Lib/ThreadStyle.swift \
  Sources/Passband/Lib/Quotes.swift \
  Tests/TranslationWireTests.swift

run_suite spam-wire \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Tests/SpamWireTests.swift

# The usage page's arithmetic: two sparse UTC-keyed series off the wire into
# dense, aligned windows, the rolling share and the deltas against the window
# before. Foundation only, so it builds with the wire types alone; the charts
# that draw it are the one part that cannot be asserted here.
run_suite usage-series \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/UsageSeries.swift \
  Tests/UsageSeriesTests.swift

# The clear board: the streak of clear days and the tally under the carrier.
run_suite clear-board \
  Sources/Passband/Lib/UsageSeries.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Lib/ClearBoard.swift \
  Tests/ClearBoardTests.swift

# The rules page's shape. The nesting, the labels that drop the `*@` and the
# selection order are all only reachable from a mailbox with several rules on
# one domain — the one this was built against has eleven rules on eleven
# domains and exercises none of it. So the fixture lives here instead.
run_suite rule-grouping \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SenderIdentity.swift \
  Sources/Passband/Lib/RuleGrouping.swift \
  Tests/RuleGroupingTests.swift

run_suite sender-identity \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SenderIdentity.swift \
  Tests/SenderIdentityTests.swift

# The keymap's two-pass dispatch, which is what makes `E` (done + next) a
# different verb from `e` (done) instead of the same one shouted — and what
# makes a declining guard spelled only "e" quietly lose the shifted key to an
# exact binding in another set. That failure abandons a draft, so it is asserted
# rather than remembered. KeyDispatch.swift is the whole suite: the registry and
# the algorithm are pure, and only the NSEvent bridge below them is AppKit.
# PageKeys.swift rides along: the digits and ⇧C exactly as RootView binds them.
run_suite key-dispatch \
  Sources/Passband/Keys/KeyDispatch.swift \
  Sources/Passband/Keys/PageKeys.swift \
  Tests/KeyDispatchTests.swift

# The ledger between the live event feed, the 10s poll and the person reading
# the thread they both have news about. Pure value logic — no store, no
# network — because the rule it encodes ("ask again as often as you like, tell
# them once") is the kind that is only ever wrong later.
run_suite thread-arrivals \
  Sources/Passband/Lib/ThreadArrivals.swift \
  Tests/ThreadArrivalsTests.swift

# The fork the notify lane put in the event feed: which frames are mail with a
# thread to open, and which are the fast word that a login code arrived. Both
# the decode and the routing are pure, which is the point — the alternative to
# asserting them here is discovering the answer on a lock screen. The auth
# banner's copy comes along because it is the one place a subject could reach
# a notification, and the fixture makes the subject a six-digit code.
run_suite sealed-event \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SenderIdentity.swift \
  Sources/Passband/Lib/AuthLabels.swift \
  Sources/Passband/Model/EventBanner.swift \
  Tests/SealedEventTests.swift

# What auth mail asks of the owner (docs/AUTH-MAIL-HANDLING.md, "Clients"): the
# sealed row from both daemon versions, which kinds open a question and for
# how long, explained alerts, new-device bursts, critical changes, and the move
# of the old device-local ledger up to a daemon that keeps verdicts. Pure, so
# the Auth page, the rail badge and the phone all count by the one copy.
run_suite auth-policy \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SenderIdentity.swift \
  Sources/Passband/Lib/AuthLabels.swift \
  Sources/Passband/Lib/AuthPolicy.swift \
  Tests/AuthPolicyTests.swift

# Whether a search is a lookup or a question, what the setting lets that verdict
# do, the single-slot queue that keeps the agent lane narrowing one need instead
# of restarting, and the prompt it does that under. All four are pure by design
# (docs/SEARCH.md §5, §6): the classifier decides to spend money on somebody's
# key, the policy decides whether it may, the slot's rule is only ever wrong
# later, and a prompt's dashes and data markers are invisible on screen.
# WireTypes rides along for SearchDiagnostics, SubjectText for the marker
# sanitizer the hits block frames every subject with.
run_suite search-intent \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SearchIntent.swift \
  Sources/Passband/Lib/DeeperSearch.swift \
  Sources/Passband/Lib/RefinementSlot.swift \
  Sources/Passband/Lib/SearchLanePrompt.swift \
  Tests/SearchIntentTests.swift

# The settings search. A ranking is the one kind of code where a green build
# says nothing at all: the matcher compiles whatever weights it is given, and
# only a fixture says whether "dark mode" lands on the theme switch. One file,
# Foundation only — the index is kept free of SwiftUI so this suite can hold the
# whole taxonomy up with no app behind it.
run_suite settings-search \
  Sources/Passband/Lib/SettingsSearch.swift \
  Tests/SettingsSearchTests.swift

# What the assistant says about Passband itself: the three help tools' answers.
# All three are text a model reads and repeats to a person, so the suite asks
# the questions ("how do I reply all?", "where is dark mode?") and holds the
# prose to the house style. The shortcut table and the settings index ride
# along because two of the tools ARE those tables, and the release table
# because "what's new" is read off it.
run_suite product-guide \
  Sources/Passband/Lib/ShortcutCatalog.swift \
  Sources/Passband/Lib/SettingsSearch.swift \
  Sources/Passband/Lib/ReleaseNotes.swift \
  Sources/Passband/Lib/PhoneReleaseNotes.swift \
  Sources/Passband/Lib/ProductGuide.swift \
  Tests/ProductGuideTests.swift

# The density curves: compact is the identity (which is what makes moving a
# literal onto a token safe), comfortable never shrinks anything, and neither
# curve folds two steps of the ladder onto one size.
run_suite density \
  Sources/Passband/Lib/Density.swift \
  Tests/DensityTests.swift

# The search field's `from:` operator: when the sender menu is open and what
# accepting a sender does to the text. Pure string work, Foundation only.
run_suite from-operator \
  Sources/Passband/Lib/FromOperator.swift \
  Tests/FromOperatorTests.swift

# The rule editor's sender menu: what each field text asks the sender
# directory for (people, or hosts after `*@`), and the host roll-up that
# stands in for /client/sender-domains on a daemon older than the route.
run_suite rule-pattern-suggest \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/RulePatternSuggest.swift \
  Tests/RulePatternSuggestTests.swift

# The actual onboarding controller with in-memory collaborators: the store
# stub flips the real RehearsalMode flag the way the real one does.
run_suite tour-controller \
  Sources/Passband/Lib/DiscoveryLedger.swift \
  Sources/Passband/Model/PracticeTourStep.swift \
  Sources/Passband/Model/TourController.swift \
  Tests/TourControllerTests.swift

# The ring behind the search panel's empty state: what a submitted query does to
# the remembered ones. Pure Foundation, one file — the fold is kept out of the
# store precisely so the order, the dedupe and the cap can be asserted with no
# app, no account and no UserDefaults behind them.
run_suite recent-search-ring \
  Sources/Passband/Lib/RecentSearchRing.swift \
  Tests/RecentSearchRingTests.swift

# ⌘F in the reader: matches in a string, the cursor walking them across a
# thread's messages (wrap, skip, the backwards step into an uncounted message),
# and the session reconciling its text estimate with what a frame reports.
run_suite find-in-thread \
  Sources/Passband/Lib/FindInThread.swift \
  Tests/FindInThreadTests.swift

# The reader's dwell clock, driven by hand: paused time, pass-throughs, the
# forgotten-window cap and which message a click is filed under. Pure, because
# the numbers end up as reasons shown to somebody about a sender.
run_suite attention-clock \
  Sources/Passband/Lib/AttentionClock.swift \
  Tests/AttentionClockTests.swift

run_suite search-preview \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/SearchPreview.swift \
  Tests/SearchPreviewTests.swift

# Field-scoped corrections must preserve overlapping destinations and keep
# authentication kinds separate from external access restrictions.
run_suite triage-targets \
  Sources/Passband/Lib/TriageTargets.swift \
  Tests/TriageTargetsTests.swift

# Which APNs environment a push registration declares. A token is valid in one
# environment only, and the wrong answer fails silently on somebody's lock
# screen, so the signing rule is asserted against synthetic profiles. iOS
# source, but Foundation only, which is why it compiles here.
run_suite apns-environment \
  Sources/PassbandiOS/App/APNsEnvironment.swift \
  Tests/APNsEnvironmentTests.swift

run_suite capability-probe \
  Sources/Passband/Model/APIError.swift \
  Tests/CapabilityProbeTests.swift

# The smart clear receipt: the headline, one line per reason in good-news
# terms, the plurals, and that the lines account for everything cleared even
# when a newer daemon names a reason this build has no words for.
run_suite smart-clear \
  Sources/Passband/Lib/SmartClear.swift \
  Tests/SmartClearTests.swift

# An agent's draft asked onto the screen: the passband://draft link (untrusted,
# from whatever opened the URL) and the event-stream frame that carries
# open_draft. Neither may read as the other, or as a pair link.
run_suite agent-draft-link \
  Sources/Passband/Lib/AgentDraftLink.swift \
  Tests/AgentDraftLinkTests.swift

# Which hidden features this mailbox has been let into. Off for every doubt:
# no stats, an old daemon, an empty list.
run_suite hidden-features \
  Sources/Passband/Lib/HiddenFeatures.swift \
  Tests/HiddenFeaturesTests.swift

# Reading groups canonical membership without reconstructing triage from copy.
run_suite reading \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SenderIdentity.swift \
  Sources/Passband/Lib/Reading.swift \
  Tests/ReadingTests.swift

# The Agents pane: which probe answers mean a sign-in door (an arbitrary 401
# must not), and the text it hands somebody to paste. A snippet with a token
# slot gets pasted with the slot still in, so every one is asserted, and the
# self-hosted door's snippets are pinned to what they were before. A hosted
# mailbox is never told to edit a daemon it does not have.
run_suite agent-connect \
  Sources/Passband/Lib/HostedServer.swift \
  Sources/Passband/Model/AgentConnect.swift \
  Tests/AgentConnectTests.swift

# The all-mail list: lenses and the category picker, day headings and the
# ranking inside them, the poll's head merging into the scrolled pages, and
# the arrival stamp an older daemon does not send.
run_suite mail-list \
  Sources/Passband/Model/SubjectText.swift \
  Sources/Passband/Model/WireTypes.swift \
  Sources/Passband/Lib/Format.swift \
  Sources/Passband/Lib/AsyncMemo.swift \
  Sources/Passband/Lib/SenderIdentity.swift \
  Sources/Passband/Lib/MailList.swift \
  Tests/MailListTests.swift

# The Reading card's logo extraction: which heroes are a mark on a flat field,
# where the mark is, and which images must be left whole. CoreGraphics only,
# over banners built pixel by pixel. ImageFill rides along for the colour type
# the background is reported in.
run_suite image-trim \
  Sources/Passband/Lib/ImageFill.swift \
  Sources/Passband/Lib/ImageTrim.swift \
  Tests/ImageTrimTests.swift

# Run the queue. xargs keeps going past a failed suite and exits 123 if any
# failed, which set -e turns into this script's exit status.
xargs -P "$(sysctl -n hw.ncpu)" -L 1 bash -c 'run_one "$@"' _ < "$QUEUE"
