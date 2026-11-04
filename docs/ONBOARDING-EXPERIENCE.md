# Onboarding experience

The experience starts with relief, then opens onto the person's own mail:

1. **You're only human.** “Inbox zero every day was never realistic. You're
   only human.” The supporting line reminds you that your attention is valuable. A small pile of
   example messages accompanies the introduction.
2. **A different goal.** “Know what needs you.”
   The same cards organize into a request, a record, and a newsletter. Both
   opening beats are user-paced, with Back and Skip intro controls.
3. **Bring your inbox.** One door: Sign in with Gmail, with Use a self-hosted server
   beneath it, on the intro's own scene. A Google account with no mailbox yet is
   sent to signup from the sign-in page, so there is no separate Sign up button.
   Sign in opens the browser and leaves a compact manual pairing form available;
   a deep link emphasizes device and account names and collapses technical details.
   Hosted auth URLs carry the current light/dark appearance via the theme parameter
   introduced in #214, including when the app follows the system appearance.
   Pairing links bypass the opening and go straight to connection confirmation.
4. **Your real inbox, filling in.** There is no guided tour and no practice
   mailbox: the app should explain itself, and a tour that needed fourteen
   steps was the UI saying it did not. The first time a mailbox connects on a
   Mac while the daemon is still catching up on it (`stats.catch_up` is
   present), the real Sitrep opens blurred under one card, the first-load veil
   (`Views/FirstLoadVeil.swift`, `Model/FirstLoad.swift`). It says "Working
   through your inbox.", shows a bar of emails fetched against the total, the
   live counts as they land (emails sorted, set aside as noise, need your
   attention), and a time estimate taken from the throughput the polls
   actually saw.

## The first-load veil

- **When.** Decided once, at the first stats poll after connecting. A
  catch-up in flight raises the veil. A mailbox with mail and no catch-up goes
  straight to the board. A mailbox with neither (the daemon has not started
  walking yet) is given up to 20 seconds for a catch-up to appear. Every
  outcome sets `Prefs.firstLoadSeen`, which is still stored under the old
  `passband.pref.tourCompleted` key, so an install that finished the tour is
  treated as having had its first look. It never shows again on that Mac.
- **The board under it is live.** The veil joins `AppStore.modalOverlayOpen`,
  so the shell blurs the way it does under any modal, and the zones fill in
  through the blur.
- **The estimate is honest.** `Lib/CatchUpEstimate.swift` takes the rate over
  the last eight polls, needs at least two polls five seconds apart that saw
  the count move, and says "estimating…" until then (and whenever the count
  stands still). The line changes only when a poll lands; nothing ticks
  between polls. A count that goes backwards (a restarted walk) starts the
  samples over.
- **Leaving.** "Start now", Enter, Esc, a click on the scrim, or walking to
  another page dismisses it early. A catch-up still running carries on at the
  top of the board: the sync banner's catch-up state is the same progress row
  (`Views/CatchUpProgressRow.swift`), with the same line from the same
  estimate (`AppStore.catchUpEstimate`, fed by every stats poll), so the pace
  does not start over. A later catch-up with no veil uses that banner too,
  measured fresh. When the catch-up ends the bar is shown full
  with "All caught up." for a second, then the veil lifts.
- **Degraded states.** Daemon down: "Connection interrupted. Your inbox will
  update when we reconnect." Gmail disconnected: "Your mail connection needs
  attention in Settings before new mail can arrive." The pace is not shown
  while either holds.
- **What comes after.** The first look ending is what stamps a newcomer for
  What's New (`WhatsNew.meetNewcomer`), so the latest release's notes follow
  on the board, never over the veil.
- **Analytics.** `first_load_shown`, and `first_load_ended` with one bool,
  `early` (dismissed before the catch-up finished).

To look at the card without a fresh mailbox, launch with
`PASSBAND_FIRST_LOAD=1`: it raises the veil with synthetic numbers (a walk of
4,200 moving about twelve a second) over whatever board is connected, and
stamps nothing. After Start now the made-up walk keeps counting in the
board's banner until it ends.

What is deliberately not here: done stays the `e` key (the reader's verb bar
is unchanged), and sender rules are not taught. Teaching rules is meant to
become automatic (suggesting one, or asking whether to see more mail like
this), not a lesson.

The phone has no veil: its onboarding is the cold open (OnboardingIntroView,
inside ConnectView) and then the live app.

## The fixture inbox (debug only)

`FixtureMode`, `FixtureAPI` and `FixtureMail` are what is left of the practice
inbox's machinery: an in-memory transport answering the human-door routes with
fictional mail, behind the real API client. They exist for the phone's debug
screenshot harness (`PassbandiOS/Lib/FixtureStage.swift`, `--fixture-inbox`),
which boots onto that mail with no daemon. Nothing a customer can reach turns
it on, nothing captured while it is up is sent to analytics, and the board,
calendar and photo caches are never written from it. Credential probes refuse
the fixture transport outright (`Tests/CredentialProbeTests.swift`).
