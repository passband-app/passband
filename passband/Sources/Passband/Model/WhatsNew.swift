// Whether this launch owes the human a changelog, and which entries.
//
// The rule is one sentence: a version's notes are shown once, on the first
// launch after taking that version, and never again. Everything below is the
// ways that sentence can be got wrong.
//
// IT NEVER MEETS A NEW USER. `tourCompleted` is the gate, and it is exactly the
// right bit: it says the app has already introduced itself to whoever is
// sitting here. A fresh install has not been introduced, so it gets the tour
// and no changelog, and the two overlays can never stack, because one runs only
// where the other has finished. It also settles the transition INTO this
// feature, which has no other honest signal: an install that predates the stamp
// but has finished the tour is an established install, owed the release it just
// took and nothing older (see ReleaseNotes.unseen).
//
// THE IPHONE IS THE SAME SENTENCE OVER A DIFFERENT TABLE. It ships on its own
// version line and reads its own notes (Lib/PhoneReleaseNotes.swift), keeps its
// own stamp (Prefs.lastSeenPhoneNotes), and shows them full screen
// (PassbandiOS/Views/WhatsNewWalk.swift) rather than as this card. It is still
// THIS object, `store.whatsNew`, and still `active`, `replay` and `dismiss`:
// everything that asks "is the changelog up" (the blur, the ads that wait their
// turn, Settings' button) asks one thing on both platforms. Only the trigger
// is spelled twice, because the phone has no tour to gate on; see
// `maybeShowOnPhone`.

import Foundation
import Observation

@MainActor
@Observable
final class WhatsNew {
    /// Env escape hatch for development and screenshots, matching the tour's.
    /// It takes the card past the STAMP, not past the "must be on the board,
    /// must have synced" part of the gate, and it fabricates nothing: with no
    /// note for this build there is still no card.
    static let forced = ProcessInfo.processInfo.environment["PASSBAND_FORCE_WHATS_NEW"] == "1"

    /// The stamp to pretend to when forced, for looking at the card somebody
    /// several releases behind gets: `PASSBAND_WHATS_NEW_SINCE=0.0.10`.
    static let forcedSince = ProcessInfo.processInfo.environment["PASSBAND_WHATS_NEW_SINCE"]

    /// Which page of the walk the card opens on, for looking at one page with
    /// no hand on the keyboard (a screenshot has none). 0, the first, unless
    /// `PASSBAND_WHATS_NEW_PAGE` says otherwise; past the end is the record.
    nonisolated static let openingPage = max(
        0, Int(ProcessInfo.processInfo.environment["PASSBAND_WHATS_NEW_PAGE"] ?? "") ?? 0)

    /// What the card draws, newest release first. Empty means no card.
    private(set) var notes: [ReleaseNote] = []

    /// Set when `notes` spans more than one release: the card opens by saying
    /// so, and names the version this install was last on.
    private(set) var catchUp: ReleaseCatchUp?

    #if os(iOS)
        /// What the phone's walk draws, newest release first. The Mac's `notes`
        /// stays empty here: nothing on a phone reads the Mac's table.
        private(set) var phoneNotes: [PhoneReleaseNote] = []

        /// Which page the walk opens on. 0 outside a staged screenshot.
        private(set) var phoneOpeningPage = 0

        var active: Bool { !phoneNotes.isEmpty }
    #else
        var active: Bool { !notes.isEmpty }
    #endif

    /// Set by closing the card, so a reconnect (which resets `lastRefresh`, the
    /// trigger) cannot raise the same notes again in one session. The stamp
    /// covers the next LAUNCH; this covers the next sync.
    private var dismissedThisSession = false

    /// Which version this copy of the app actually is. Read from the bundle
    /// rather than passband/VERSION so it cannot drift from what shipped, and
    /// nil where there is no bundled version (a test binary), for which the
    /// honest answer is to show nothing.
    private var runningVersion: String? { Self.runningVersion }

    private static var runningVersion: String? {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String
    }

    #if os(macOS)

    /// The trigger, called when the first sync of the session lands. Every
    /// clause is a reason NOT to interrupt: already up, already answered, a new
    /// user who is owed the tour instead, a tour actually running, or the human
    /// is somewhere other than the board this is shown over.
    func maybeShow() {
        guard !RehearsalMode.isEnabled else { return }
        let store = AppStore.shared
        guard !active, !dismissedThisSession else { return }
        guard Prefs.shared.tourCompleted, !store.tour.active else { return }
        guard store.activeView == .sitrep, !store.daemonDown else { return }
        guard let running = runningVersion else { return }

        let stamp = Self.forced ? Self.forcedSince : Prefs.shared.lastSeenReleaseNotes
        let unseen = ReleaseNotes.unseen(lastSeen: stamp, running: running)
        guard !unseen.isEmpty else { return }
        notes = unseen
        catchUp = ReleaseNotes.catchUp(lastSeen: stamp, running: running, shown: unseen)
        Analytics.capture("whats_new_shown", ["releases": unseen.count])
    }

    /// Settings' "what's new in this version": on demand, ignoring the stamp.
    /// ONE release, because that is the question the button asks. The back
    /// catalogue belongs in the changelog, not behind a button labelled with
    /// the word "this".
    func replay() {
        guard let running = runningVersion,
            let current = ReleaseNotes.newest(atOrBelow: running)
        else { return }
        dismissedThisSession = false
        notes = [current]
        catchUp = nil
    }

    /// Closing the card. The stamp is the newest note ACTUALLY SHOWN, never the
    /// running version: a build carrying no note for some release must leave it
    /// unstamped so a later build can still announce it, instead of swallowing
    /// it on the reader's behalf.
    func dismiss() {
        guard active else { return }
        // A forced card is a preview. Stamping from one told this Mac it had
        // read a release it had not taken yet, and the real update then
        // announced nothing.
        if !Self.forced { stamp(ReleaseNotes.newestShown(notes)) }
        notes = []
        catchUp = nil
        dismissedThisSession = true
    }

    /// Advance the stamp, never walk it back. `replay` shows a release
    /// regardless of the stamp, and closing THAT card must not re-arm the app
    /// for notes the human has already read past.
    private func stamp(_ version: String?) {
        guard let version, let seen = ReleaseVersion(version) else { return }
        if let current = Prefs.shared.lastSeenReleaseNotes.flatMap(ReleaseVersion.init),
            seen <= current
        {
            return
        }
        Prefs.shared.lastSeenReleaseNotes = version
    }

    #else

    // MARK: - the iPhone

    /// THE PHONE'S TRIGGER, called by the shell when the first sync of a
    /// session lands. The Mac's clauses, less the tour and plus what a phone
    /// can be doing instead of looking at its board: the shell says whether
    /// the Sitrep TAB is the one showing (`activeView` cannot, since Quick
    /// Look and search are tabs the store has no page for), and a reader or a
    /// sheet opened by a notification tap is somebody in the middle of
    /// something.
    ///
    /// Who is new is settled before this runs, at the Connect gate
    /// (`meetNewcomer`), so by here a missing stamp means what it means on the
    /// Mac: an install older than the feature.
    func maybeShowOnPhone(sitrepShowing: Bool) {
        guard !RehearsalMode.isEnabled else { return }
        let store = AppStore.shared
        guard !active, !dismissedThisSession else { return }
        guard sitrepShowing, !store.daemonDown else { return }
        guard store.threadId == nil, store.compose == nil, !store.modalOverlayOpen else { return }
        guard let running = runningVersion else { return }

        let arrival = PhoneReleaseNotes.arrival(
            lastSeen: Prefs.shared.lastSeenPhoneNotes, running: running, newcomer: false)
        guard case .show(let owed) = arrival else { return }
        phoneOpeningPage = 0
        phoneNotes = owed
        Analytics.capture("whats_new_shown", ["releases": owed.count])
    }

    /// The Connect gate is on screen and this install knows no mailbox:
    /// whoever is holding the phone is being introduced to Passband right now.
    /// They are stamped current HERE, before they connect, and not at their
    /// first sync: a phone killed between the two would come back as an
    /// install with a mailbox and no stamp, which is exactly what somebody who
    /// just updated looks like, and the cold open would be followed by release
    /// notes for the only version they have ever run.
    func meetNewcomer() {
        guard !RehearsalMode.isEnabled, let running = runningVersion else { return }
        let arrival = PhoneReleaseNotes.arrival(
            lastSeen: Prefs.shared.lastSeenPhoneNotes, running: running, newcomer: true)
        if case .stamp(let version) = arrival { Prefs.shared.lastSeenPhoneNotes = version }
    }

    /// What Settings' "What's new" would show, or nil when it has nothing and
    /// should not offer. A shipped build answers as the Mac does: the newest
    /// note at or below itself.
    ///
    /// A DEBUG build answers with the newest note there is, whatever it is
    /// running. An untagged local build wears the LAST release's number (see
    /// project.yml), so the note being written for the next one is always
    /// ahead of it, and the one person who needs to look at that note on a
    /// device is the one writing it.
    static var replayable: PhoneReleaseNote? {
        #if DEBUG
            PhoneReleaseNotes.all.first
        #else
            runningVersion.flatMap { PhoneReleaseNotes.newest(atOrBelow: $0) }
        #endif
    }

    /// Settings' "What's new": on demand, ignoring the stamp, one release.
    func replay() {
        guard let current = Self.replayable else { return }
        dismissedThisSession = false
        phoneOpeningPage = 0
        phoneNotes = [current]
    }

    #if DEBUG
        /// A screenshot's way in (FixtureStage): the newest note, opened on
        /// `page`. Past the end is the closing page.
        func stage(page: Int) {
            guard let current = Self.replayable else { return }
            phoneOpeningPage = max(0, page)
            phoneNotes = [current]
        }
    #endif

    /// Closing the walk, from its x or off its last page. The stamp is the
    /// newest note SHOWN, as on the Mac, and never past the build: a debug
    /// replay of a note for a release that is not out yet must not mark that
    /// release read before it ships.
    func dismiss() {
        guard active else { return }
        if let shown = PhoneReleaseNotes.newestShown(phoneNotes).flatMap(ReleaseVersion.init),
            let running = runningVersion.flatMap(ReleaseVersion.init), shown <= running
        {
            stamp(shown.text)
        }
        phoneNotes = []
        dismissedThisSession = true
    }

    /// Advance the stamp, never walk it back; see the Mac's.
    private func stamp(_ version: String) {
        guard let seen = ReleaseVersion(version) else { return }
        if let current = Prefs.shared.lastSeenPhoneNotes.flatMap(ReleaseVersion.init), seen <= current {
            return
        }
        Prefs.shared.lastSeenPhoneNotes = version
    }

    #endif
}
