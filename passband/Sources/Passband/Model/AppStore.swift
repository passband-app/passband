// THE app store: one observable object holding connection state, the sitrep read
// model, selection (stable by message id across refresh), the undo queue, the
// routed view + history stack, and the overlay surfaces.
//
// DATA and coordination only — network calls live in APIClient, and the poller
// and views write their results back here.

import Foundation
import Observation
import SwiftUI

// MARK: - routed views

/// Which primary surface the rail is showing. `sitrep` is the abstracted
/// dashboard (the default on launch); `emails` is the classic band list.
/// Where the share sheet was raised from. The raw values are ANALYTICS STRINGS
/// and must appear in `Analytics.allowedStrings`, which is a closed set: a value
/// missing from it is a fatal assertion at the event site in a debug build, not
/// a silently dropped property.
enum ShareOrigin: String, Sendable, Hashable {
    /// The `gift` tile in the sidebar rail.
    case rail
    /// The button in Settings > Account.
    case settings
    /// The two-week ask.
    case nudge
}

/// WHERE A READER WAS ENTERED FROM: the surface that produced the open, as
/// `thread_opened`'s `source`. Closed and enumerable on purpose; every raw
/// value is in Analytics' string vocabulary. `reader` is the walk from one
/// queued thread to the next, an open the reader made rather than the person,
/// so a dashboard can subtract it from "threads people chose to open".
enum OpenSource: String, Sendable {
    case sitrep, sitrepCard = "sitrep_card", reading, emails, search, browse
    case assistant, deeperSearch = "deeper_search", groups, notification
    case reply, reader
    /// A ghost's "mail behind it" on the calendar page.
    case calendar
}

enum MainView: String, Sendable, Hashable, CaseIterable {
    case sitrep, calendar, emails, auth, usage, settings, process, discovery

    /// The TOP rail group — also the 1..N number-key mapping, taken from
    /// POSITION in this array. Usage/Settings are excluded so that adding one
    /// never renumbers the rest. `process` is off the rail entirely: its one door
    /// is the all-mail header's peer-review chip.
    ///
    /// Rules and Groups used to be the fourth and fifth, and are Settings panes
    /// now (`SettingsSection.rules` / `.groups`), the move Audit made before
    /// them: each is a page you visit to set something up, not one you work
    /// from, and the rail is for the second kind. They came off the END, so no
    /// digit that is left moved, and the two digits after the rail's last page
    /// still open them where they live (see `RootView.globalBindings`).
    static let mainViews: [MainView] = [.sitrep, .emails, .auth]
    /// The BOTTOM rail group, pinned below a divider.
    ///
    /// `discovery` is in neither group: its one door is the ring, alone above
    /// the divider, and it takes no number key because nobody should need to
    /// reach a field guide faster than by looking at it.
    static let bottomViews: [MainView] = [.usage, .settings]

    /// THE NUMBER KEYS AS THEY ARE RIGHT NOW: `mainViews`, with Calendar
    /// second while it is offered (Sitrep 1, Calendar 2, Emails 3 and every
    /// later page one higher), and exactly `mainViews` while it is not, so
    /// the gate being closed renumbers nobody. The rule is
    /// `CalendarGate.numbered`. ⇧C opens the calendar as well, as it did
    /// before it had a digit.
    ///
    /// `calendar` is `AppStore.calendarPageAvailable`, passed in so the rail,
    /// the menu and the keys ask the gate the same way and move together.
    static func numberedViews(calendar: Bool) -> [MainView] {
        CalendarGate.numbered(mainViews, calendar: .calendar, offered: calendar)
    }

    /// The most digits the keys ever bind: one per numbered page with the
    /// calendar offered. The last one declines while it is not.
    static var numberKeyCount: Int { mainViews.count + 1 }

    /// THE TOP RAIL AS DRAWN, each page with the digit that opens it: the
    /// hint the rail's tooltip prints, in the same order the keys are bound
    /// in (RootView.globalBindings).
    static func railViews(calendar: Bool) -> [(view: MainView, key: String)] {
        numberedViews(calendar: calendar).enumerated().map { (view: $0.element, key: "\($0.offset + 1)") }
    }

    var label: String {
        switch self {
        case .sitrep: "Sitrep"
        case .calendar: "Calendar"
        case .emails: "Emails"
        case .auth: "Auth"
        case .usage: "Usage"
        case .settings: "Settings"
        case .process: "Process"
        case .discovery: "Ways through your mail"
        }
    }

    var symbol: String {
        switch self {
        case .sitrep: "gauge.with.dots.needle.33percent"
        case .calendar: "calendar"
        case .emails: "envelope"
        case .auth: "key"
        case .usage: "waveform.path.ecg"
        case .settings: "gearshape"
        case .process: "checkmark.seal"
        // Never drawn: the rail draws the discovery ring in its place.
        case .discovery: "circle.dashed"
        }
    }
}

/// One entry in the view-history stack. Captures the routed view plus, for an
/// Emails drill-target hand-off, the selected id to re-focus.
struct HistoryEntry: Equatable, Sendable {
    var view: MainView
    var selectedId: Int?
}

/// Cap the history so a long session can't grow it without bound.
let historyCap = 50

// MARK: - mail pages

/// Which page the emails tab shows. `inbox` is the flat all-tiers list; `noise`
/// is everything triage scored as noise, narrowed to that tier BY THE DAEMON so
/// nothing has to be discarded client-side. `sent` is the odd one out: outbound
/// mail, off its own route, with none of the triage verbs (nothing triaged it,
/// and nothing can resolve it).
///
/// AND `spam`, WHICH IS NOT THE SAME THING AS `noise` however much the words
/// overlap. Noise is our verdict about mail that reached the inbox; spam is
/// Gmail's verdict about mail that never did. Nothing in Passband triaged a spam
/// row and nothing will — the page exists so the folder is reachable without
/// leaving for the browser, and so the one thing worth doing there (rescuing
/// something real out of it) can be done here.
enum MailMode: String, Sendable, Hashable, CaseIterable {
    case inbox, noise, sent, spam

    /// The three pages the segmented control always offers.
    ///
    /// SPAM IS NOT ONE OF THEM until you are on it. It is a folder you visit
    /// deliberately, roughly never, and a permanent fourth segment would give a
    /// bin of filtered mail the same standing in the chrome as the inbox. The
    /// door is the header's spam chip; the segment appears once you walk
    /// through it, because a control showing no selection at all is worse than
    /// a control that grew an option — you need to see where you are and how to
    /// get back.
    static func segments(showingSpam: Bool) -> [MailMode] {
        // No noise segment: all mail shows every message, noise labelled as
        // such, so a page of only noise is a page nobody needs.
        showingSpam ? [.inbox, .sent, .spam] : [.inbox, .sent]
    }

    /// The page's name — the header title and the segmented control both.
    var label: String {
        switch self {
        case .inbox: "all mail"
        case .noise: "noise"
        case .sent: "sent"
        case .spam: "spam"
        }
    }

    /// Server-side tier filter for the page; nil = every tier. Nil for `sent`
    /// too, but vacuously: that page never goes to /client/updates at all, so
    /// there is no tier to narrow. Nil for `spam` for a REAL reason — a spam row
    /// was never triaged, so it carries the neutral tier=noise seed, and asking
    /// for `tier=noise` here would be filtering on a value that means nothing.
    var tier: Tier? {
        switch self {
        case .inbox, .sent, .spam: nil
        case .noise: .noise
        }
    }

    /// Whether this page asks the daemon for the provider's spam folder
    /// (`spam=only`) instead of the ordinary mailbox.
    var spamOnly: Bool { self == .spam }

}

// MARK: - side views

/// The side panels. The thread drill-in is NOT one — it is the fullscreen viewer,
/// layered ABOVE these, so opening a thread from search keeps the panel mounted
/// beside it. Esc from the reader sheds the panel first (the email stays up);
/// the next Esc closes the reader.
enum SideView: Equatable, Sendable {
    case none
    case browse
    case search

    var isOpen: Bool { self != .none }

    var title: String {
        switch self {
        case .browse: "browse — all mail"
        case .search: "search"
        case .none: ""
        }
    }
}

/// How wide the right-hand panel is. ONE definition: the thread viewer insets
/// itself by exactly this much so an open panel stays visible beside the reader,
/// and two numbers drifting apart would leave a seam or a covered strip.
let sidePanelWidth: CGFloat = 460

/// The search panel's state, held OUT of the panel: SwiftUI discards a view's
/// `@State` on unmount, and parking it here is what makes `/` resumable — same
/// query, same hits, same selection, no refetch and no empty flash.
struct SearchSession: Sendable, Equatable {
    var fetchedRelated: Bool? = nil
    var revision = 0
    var fetchedRevision: Int? = nil
    var query = ""
    var hits: [SearchHit] = []
    /// The armed row. -1 = nothing armed, focus semantically in the bar: Enter
    /// expands the panel instead of opening a hit. ArrowDown arms row 0.
    var index = -1
    /// Search opens wide and collapses beside the reader when a hit opens.
    var expanded = false
    var error: String?
    /// The term `hits` actually came from, so reopening on an unchanged query
    /// skips the round-trip. nil = what is on screen is not authoritative (never
    /// fetched, or the last fetch failed), so reopening retries.
    var fetchedQuery: String?
    /// The sort those hits were ranked under. Paired with `fetchedQuery`
    /// because the skip-the-round-trip check is "same question, same rules":
    /// changing the order in Settings and reopening the panel on the same term
    /// has to re-rank, or the setting silently does nothing until the reader
    /// edits their query.
    var fetchedSort: SearchSortChoice?
    /// Whether those hits were asked for in plain ranked order
    /// (`AppStore.searchPlainOrder` at the time). Pinned for the scroll like
    /// the sort, because a cursor is an offset into ONE ranking and the flag
    /// can flip between pages; a flip re-fetches page 1 instead.
    var fetchedPlainOrder: Bool?
    /// What retrieval did with `fetchedQuery`, as the daemon reported it: the
    /// strict/any counts and each term's document frequency. Parked BESIDE the
    /// fetched query because it describes exactly that fetch — the classifier
    /// reads the pair, and a diagnostic outliving the query it belongs to would
    /// judge the wrong words. nil against an older daemon, which sends none.
    var diagnostics: SearchDiagnostics?
    /// Cursor for the page AFTER the ones in `hits`. nil = the server has no
    /// more (or nothing authoritative is on screen). Parked here with the rest
    /// so reopening resumes mid-scroll instead of dropping back to page one.
    var nextCursor: String?

    // MARK: the deeper-search lane, as the PANEL sees it
    //
    // The conversation itself is `AppStore.searchLane`, an AssistantSession,
    // and it cannot live in here: this is a struct with Equatable semantics
    // that the store copies around, and a class with identity inside one would
    // compare by pointer while behaving like shared state. So the session sits
    // beside this and its bookkeeping sits in here, as plain values that the
    // band renders and that reset with the search they describe.

    /// The last verdict, for the panel session on screen. nil until a query has
    /// settled and been judged; `.lookup` is a real answer, which is why this
    /// is not merely "is there a trigger".
    var lastVerdict: SearchIntent.Verdict?
    /// Whether the lane has been started for THIS panel session. What makes a
    /// second deeper query a refinement rather than a second conversation.
    var laneStarted = false
    /// Why it started, for the band's one line of small type. Held rather than
    /// re-derived: the query has moved on by the time the band draws, and the
    /// reason has to be the one that actually spent the money.
    var laneTrigger: SearchIntent.Trigger?
    /// The query the lane was started on, so the band's reason can count the
    /// words that triggered it rather than whatever is in the field now.
    var laneQuery = ""
    /// How often it has been narrowed since. A MIRROR of the session's own
    /// count (`AssistantSession.refinementCount`), which stays authoritative:
    /// this copy is for the panel to read without reaching through the store
    /// into a class it does not own.
    var refinementCount = 0
}

// MARK: - connection

enum ConnStatus: Sendable, Equatable {
    case loading      // reading keychain on boot
    case disconnected // no settings -> Connect screen
    case connecting   // testing a candidate URL+token
    case connected
    case error
}

/// Why a refresh failed, so the UI can distinguish "daemon unreachable" (fix:
/// start squelchd) from "token rejected" (fix: open Settings).
struct RefreshError: Equatable, Sendable {
    var message: String
    var kind: APIErrorKind

    /// True when the failure is the token, not the transport.
    var isAuthFailure: Bool { kind == .unauthorized || kind == .forbidden }
}

// MARK: - undo / toasts

/// `remind` is its own kind rather than a flavour of `done`: the forward action
/// resolves the thread AND schedules its return, so the inverse is two calls,
/// and "undo_fired kind=done" would count a reminder as a completion.
///
/// `send` is the odd one out: nothing has gone out yet. The mail is HELD for
/// the undo window and only leaves when it closes, so its "inverse" is simply
/// not sending (see `AppStore.sendWithUndo`).
///
/// `draftDelete` is held the same way: the DELETE only goes out when the
/// window closes, so its inverse is not deleting (see `AppStore.deleteDraft`).
///
/// `schedule` is NOT held: the mail is already in the daemon's outbox when its
/// toast appears, and its inverse is a real call, the DELETE that pulls it
/// back into the composer (see `AppStore.scheduleCompose`).
///
/// `scheduledCancel` is not held either: the DELETE has gone out, and its
/// inverse re-POSTs the request it handed back (see
/// `AppStore.cancelScheduledRow`).
enum UndoKind: Sendable {
    case archive, done, label, ruleDelete, groupDelete, remind, send, draftDelete, schedule,
        scheduledCancel, ghostDismiss, skip

    /// Whether firing this undo changes anything the sitrep shows, and so
    /// whether `fireUndo` un-hides a resolved id and pulls the poller. The two
    /// held kinds do not: a held send never reached the daemon and a held
    /// draft delete never touched a message, so their undo is "do nothing",
    /// and a sitrep round trip for it is pure cost.
    var touchesSitrep: Bool {
        switch self {
        case .archive, .done, .label, .ruleDelete, .groupDelete, .remind: true
        case .send, .draftDelete, .schedule, .scheduledCancel, .ghostDismiss, .skip: false
        }
    }
}

/// One step of a Space walk, kept on its undo so the undo can walk it back:
/// the email the step left comes back the way it went, and the one it landed
/// on goes back the way it came.
struct WalkRewind: Sendable, Equatable {
    enum Step: Sendable, Equatable {
        /// E: `back` flew out the top and `from` came in from `arrivedFrom`.
        case done(arrivedFrom: AppStore.ThreadEdge)
        /// S: `back` receded left while `from` slid in over it.
        case skip
    }
    /// The email the step left.
    let back: String
    /// The email it landed on, which has to still be the one on screen.
    let from: String
    let step: Step
    let id = UUID()
}

/// A queued undo. `revert` is the exact inverse call to fire on `u`/toast-click;
/// the forward action has already gone out.
struct PendingUndo: Identifiable, Sendable {
    let id = UUID()
    var kind: UndoKind
    /// The message id for mail actions; the (now-deleted) rule id for ruleDelete,
    /// and the (now-deleted) group id for groupDelete.
    var messageId: Int
    var label: String
    var createdAt: Date = Date()
    /// When the chip leaves on its own, for its countdown.
    var expiresAt: Date?
    /// Set for a Space walk's E and S: what to play backwards on undo.
    var rewind: WalkRewind?
    var revert: @Sendable () async throws -> Void
}

struct Toast: Identifiable, Sendable {
    enum Tone: Sendable { case info, error, success }
    let id = UUID()
    var text: String
    var tone: Tone
}

// MARK: - 2FA present-don't-read

/// A live countdown ring on the auth rail icon, one per freshly-arrived auth
/// message, sweeping over `ringSeconds` then removing itself. The ring plus the
/// seen-set ARE the read state — read-marking is impossible under
/// gmail.readonly.
struct AuthRing: Identifiable, Sendable, Equatable {
    var id: Int
    var startedAt: Date
}

/// A queued code-modal entry. `code` is nil when extraction failed (the modal
/// shows an "Open Auth" affordance instead). In memory only, never persisted.
struct AuthCodeEntry: Identifiable, Sendable, Equatable {
    var meta: SealedMeta
    var code: String?
    var id: Int { meta.id }
}

/// How long an auth ring sweeps before it disappears.
let ringSeconds: TimeInterval = 60

/// The email currently being reclassified by the `v` palette.
struct TriageFixTarget: Sendable, Equatable {
    var messageId: Int
    var sender: String
    var subject: String

}

/// The email currently being scheduled by the `h` palette.
///
/// Identifiable rather than Equatable (the shape TriageFixTarget takes) because
/// it carries a callback: the reader has to advance its queue after a reminder
/// lands, and the palette is a global overlay that knows nothing about which
/// surface opened it.
struct RemindTarget: Identifiable, Sendable {
    let id = UUID()
    var messageId: Int
    var sender: String
    var subject: String
    /// The reminder ALREADY on this row, when this is a reschedule. Shown so a
    /// second `h` reads as "moving it" rather than as setting a first one.
    var remindAt: String?
    /// Called after the reminder is set, on the surface that opened the
    /// palette. The reader uses it to walk its queue the way `e`/`d` do.
    var onScheduled: (@MainActor @Sendable () -> Void)?
}

/// The rule editor's open request. Three shapes: tune (`sender`), create from
/// scratch (`rule == .some(nil)`), or edit an existing rule.
struct RuleEditorRequest: Identifiable, Sendable {
    let id = UUID()
    var sender: String?
    var rule: SenderRule?
    /// Preselect a disposition (the reading zone's CTA preselects "filtered").
    var disposition: Disposition?
    var want: String?
    /// Explicit match_pattern override; wins over deriving from `sender`.
    var pattern: String?
    /// The actual persisted result and submitted body, for consumers that need
    /// to distinguish the saved behavior from the editor's initial suggestion.
    var onResult: (@MainActor @Sendable (CreatedRule, CreateRuleBody) -> Void)?
    /// Called after a successful save so the opener re-fetches its list.
    var onSaved: (@MainActor @Sendable () -> Void)?

    /// Which unsaved draft this card picks up, or nil when it must not pick one
    /// up. One per thing the card is ABOUT: a sender, an existing rule, or the
    /// blank create. A caller that hands over its own want text has already
    /// written the sentence, so a stale draft does not get to overwrite it.
    var draftKey: String? {
        if let rule { return "rule:\(rule.id)" }
        guard want == nil else { return nil }
        if let sender { return "sender:\(SenderID.address(sender))|\(pattern ?? "")" }
        return "new"
    }
}

/// What a rule editor held when it was dismissed without saving (#306), so
/// opening the same card again finds the half-written rule where it was left.
/// In memory only: a draft is a sentence someone was halfway through, not a
/// document, and it is gone at quit like the card's own state would be.
struct RuleDraft {
    var pattern: String
    var want: String
    var disposition: Disposition?
    var advanced: Bool
    /// What the card started from when this draft was written. A draft only
    /// comes back over the SAME starting point: a rule edited somewhere else
    /// since (the assistant, another device) opens on its new values, not
    /// under a draft that would quietly revert them on save.
    var origin: String = ""

    /// The fields that decide whether two cards start from the same place.
    var fingerprint: String {
        [pattern, want, disposition?.rawValue ?? "auto"].joined(separator: "\u{0}")
    }
}

/// What the group editor was opened with. `group` nil is a create; present is an
/// edit, and the editor opens on that group's own values.
struct GroupEditorRequest: Identifiable, Sendable {
    let id = UUID()
    var group: SendGroup?
    /// Seed a create with addresses the user already had in hand — the composer's
    /// "save these people as a group" path. Ignored on an edit, which has its own
    /// membership.
    var seedMembers: [GroupMember] = []
    /// Called after a successful save so the opener re-fetches its list.
    var onSaved: (@MainActor @Sendable () -> Void)?
}

/// The open thread, reduced to what the ⌘K agent needs to be told about it:
/// which email the person is looking at, and the id every thread-level verb in
/// the reader already targets. A named struct rather than a tuple because it is
/// an `@Observable` property, and lifted out of the viewer because the ask bar
/// is a modal ABOVE it with no other way to see what it holds.
struct OpenThreadSummary: Sendable, Equatable {
    /// WHICH THREAD THIS DESCRIBES. Carried so a reader can check the summary
    /// against the thread actually open rather than trust that the last writer
    /// won — a slow fetch lands whenever it lands. See `currentThreadSummary`.
    var threadId: String
    var subject: String
    /// The NEWEST message in the thread — see ThreadViewer's `newest`.
    var newestMessageId: Int
}

// MARK: - the store

/// The read model the SitrepView renders: updates bucketed by band.
/// The bands. The daemon's STATS used to ride in here too, and that defeated
/// the poller's compare-before-assign: stats carry the sync clock, the queue
/// depths and today's spend, which move on nearly every tick, so the whole
/// read model was reassigned every ten seconds and every reader of the bands
/// — the board, the rail, the inbox — re-rendered for a timestamp. Stats are
/// their own property on the store now (`AppStore.stats`).
struct SitrepData: Sendable, Equatable {
    var standing: [AttentionUpdate] = []
    var new: [AttentionUpdate] = []
    var open: [AttentionUpdate] = []
    var sealed: [SealedMeta] = []
    /// Server aggregate, including attention beyond the displayed page.
    var totalCount: Int?
}

@MainActor
@Observable
final class AppStore {
    static let shared = AppStore()

    // MARK: settings slice
    var connStatus: ConnStatus = .loading {
        didSet {
            // Not while a launch painted from disk still waits on its probe:
            // a notification tap is delivered only over a connection the
            // daemon has vouched for (`connectionVerified`).
            if connStatus == .connected, connectionVerified { Notifier.shared.connectionBecameReady() }
            if connStatus != .loading { restoringSavedConnection = false }
            if connStatus == .loading || connStatus == .connected { leftSavedConnectionWait = false }
            // A launch that lands on a gate paints no board: its interval
            // ends here, not whenever a pull finally lands.
            if connStatus == .disconnected || connStatus == .error { LaunchSignpost.end(.skipped) }
        }
    }
    /// Set to the epoch of a launch painted from disk (`restorePersistedBoard`)
    /// until its capabilities probe answers: the shell is up, but nothing has
    /// vouched yet for the credentials or the daemon's contract. A new epoch
    /// (a switch, a connect) is its own flow's to verify, and so is any new
    /// `settings`, which ends the probe (see its didSet).
    @ObservationIgnored private var unverifiedEpoch: Int?
    /// Whether the daemon has vouched for the connection on screen. False only
    /// between a launch from disk and its probe's answer; the Notifier holds
    /// taps until it is true.
    var connectionVerified: Bool { unverifiedEpoch != epoch }
    /// The loading gate is up because a saved account's daemon is being
    /// waited on (`recoverSavedConnection`), not because of a keychain read. That wait has no end of its own, so this is what lets
    /// the gate and the Accounts menu offer a way out of it.
    private(set) var restoringSavedConnection = false
    /// The Connect gate is up because the person walked away from that wait
    /// to connect something else. It opens on its choices then, not on a form
    /// filled in with the one server known not to be answering.
    private(set) var leftSavedConnectionWait = false
    /// Switching is offered from a working mailbox, and from the wait for one
    /// that is not answering. Not from the Connect gate (see PassbandCommands).
    var canSwitchAccounts: Bool { connStatus == .connected || restoringSavedConnection }
    var settings: ConnectionSettings? {
        didSet {
            capabilityRecoveryTask?.cancel()
            capabilityRecoveryTask = nil
            // The probe a launch from disk was waiting on is the one just
            // cancelled. Every writer of these probes them first (re-validate,
            // connect, a switch) or is tearing the connection down, so the
            // connection is no longer waiting on that answer.
            unverifiedEpoch = nil
        }
    }
    @ObservationIgnored private var capabilityRecoveryTask: Task<Void, Never>?
    var connError: String?

    /// A `passband://pair` link waiting to be acted on. ConnectView is its only
    /// consumer and clears it as it applies it — and applying it only ever
    /// FILLS THE FORM, never claims, so a link can be parked without it ever
    /// becoming a credential on its own.
    var pairLink: PairLink?

    /// The Add Account sheet. Raised by Settings, the rail's account menu, the
    /// Accounts menu, and by a pair link arriving at an install that already
    /// has an identity — that link names a SECOND daemon, and the sheet is
    /// where a second daemon is added. It is deliberately not a connection
    /// state: adding an account must not move `connStatus`, or the shell would
    /// unmount and the account already on screen would go with it.
    var addAccountSheetOpen = false

    /// The Share Passband sheet. Raised from three places, and hung on the shell
    /// for the same reason Add Account is: no raiser can be sure which page is
    /// on screen.
    ///
    /// Written through [`openShareSheet(from:)`] rather than directly, so the
    /// flag and the origin beside it cannot disagree. A drag-down or a Done
    /// still sets it false on its own; the origin is only ever read while it is
    /// true, so a stale one is unreachable.
    var shareSheetOpen = false

    /// WHICH SURFACE raised the sheet, for the one analytics property that
    /// makes the event worth having: whether shares come from the rail, from
    /// Settings, or from the two-week ask. Without it `invite_sent` says people
    /// share and nothing about what makes them.
    private(set) var shareOrigin: ShareOrigin = .settings

    // MARK: sitrep slice

    /// THE ONE WRITE POINT FOR THE APP ICON'S BADGE. Every path that can change
    /// how much is due — the poller's pull, a resolve, a re-triage, the wipe on
    /// disconnect — lands here, so hooking the property rather than the callers
    /// is what keeps a sixth path from silently shipping a stale number. See
    /// Model/Badge.swift.
    var sitrep = SitrepData() {
        didSet {
            // Any change, the sealed list's included: the file's auth fence is
            // built from it. The save itself skips a board that would write
            // the same bytes (`needsWrite`).
            scheduleBoardSave()
            // The threads `ThreadPrefetch` refuses follow both signals: the
            // sealed list and the feed rows' own decisions. (Verdict writes
            // settle in `AuthDecisions.listed`, which the poller calls.)
            if sitrep.sealed != oldValue.sealed || sitrep.standing != oldValue.standing {
                noteAuthThreads()
            }
            guard sitrep.standing != oldValue.standing else { return }
            // Not for the board painted from disk at launch: the icon may hold
            // a fresher count from the notification extension, and the first
            // pull sets the badge (`Badge.set` in the poller, on every pull).
            if !paintingFromDisk { Badge.refresh(sitrep.standing) }
            noteHeadlineBand()
        }
    }
    /// The hero's sentence about what For-your-eyes is ABOUT, written by the
    /// daemon's small model (`/client/v2/fye/headline`). nil until it answers,
    /// and on a daemon with no model, where the hero keeps its own line.
    /// The board file keeps it, so a new one schedules a save.
    var fyeHeadline: String? {
        didSet { if fyeHeadline != oldValue { scheduleBoardSave() } }
    }
    /// The board `fyeHeadline` was written for (see `headlineKey`). The hero
    /// keeps showing the last sentence while a newer board's is on its way:
    /// a line a few seconds stale reads better than one that flips to the
    /// fallback and back every time a row is cleared.
    var fyeHeadlineKey: String?
    /// True while a clicked-for headline is being written; the hero dims.
    var fyeHeadlineRegenerating = false
    /// The daemon's stats, apart from the bands (see SitrepData): the sync
    /// clock, the queues, the capabilities. Assigned by the poller only on
    /// change, and it changes often — so only its readers pay for that.
    var stats: StoreStats? {
        didSet { noteStats() }
    }

    /// THE VALUES THE BUSY VIEWS READ OFF THE STATS (the reader, the inbox,
    /// both boards, the shells, the composers), written here and only when
    /// they change. The stats themselves change on EVERY poll: the daemon stamps
    /// `sync.last_success_at` each time Gmail answers (every five seconds by
    /// default), so a view that read `stats?.anything`, or a property computed
    /// from it, was a reader of the whole struct and was re-evaluated every
    /// ten seconds — the reader's message stack for an address that never
    /// moves, the root view for capability flags that never move. A view that
    /// reads one of these and not the stats is redrawn only when it changes.
    /// Views that draw the sync clock or the counters themselves still read
    /// the stats, and redraw every poll as they should.
    ///
    /// Each is compared first: an assignment of the same value still tells
    /// every reader to look again.
    private func noteStats() {
        let open = CalendarGate.pageAvailable(timeline: stats?.timeline)
        if open != calendarPageAvailable {
            calendarPageAvailable = open
            // Opening, the reminders switch is asked for again: closing set it
            // to not offered (`calendarGateClosed`).
            if open { Task { await refreshCalendarReminderPref() } }
        }
        warmCalendarIfDue()
        let email = stats?.account_email
        if email != accountEmail { accountEmail = email }
        let spam = stats?.spam
        if spam != spamCount { spamCount = spam }
        let landed = stats != nil
        if landed != hasStats { hasStats = landed }
        let dead = stats?.gmail?.connected == false
        if dead != gmailDisconnected { gmailDisconnected = dead }
        let draftsV2 = stats?.drafts_v2 == true
        if draftsV2 != draftsV2Available { draftsV2Available = draftsV2 }
        let plainOrder = stats?.search_plain_order == true
        if plainOrder != searchPlainOrder { searchPlainOrder = plainOrder }
        let scheduled = stats?.scheduled_send == true
        if scheduled != scheduledSendAvailable { scheduledSendAvailable = scheduled }
        let sharing = stats?.invite_sharing == true
        if sharing != shareAvailable { shareAvailable = sharing }
        let ios = stats?.ios_connected
        if ios != iosConnected { iosConnected = ios }
        let forwarding = stats?.forwarding == true
        if forwarding != forwardingAvailable { forwardingAvailable = forwarding }
        let attachments = stats?.compose_attachments == true
        if attachments != composeAttachmentsAvailable { composeAttachmentsAvailable = attachments }
        let relay = stats?.assistant_relay == true
        if relay != relayAvailable { relayAvailable = relay }
        var waiting = 0
        if let triage = stats?.triage { waiting = max(triage.queued, 0) + max(triage.leased, 0) }
        if waiting != triageWaiting { triageWaiting = waiting }
        noteSyncStatus()
    }

    /// The mailbox's own address, for the reader's "me". See `noteStats`.
    private(set) var accountEmail: String?
    /// The spam folder's count, nil until the daemon has looked (see
    /// `spamDoorVisible`). See `noteStats`.
    private(set) var spamCount: Int?
    /// Whether any stats have landed: the board's "all clear" waits for
    /// them. See `noteStats`.
    private(set) var hasStats = false
    /// Whether the Calendar destination exists at all. Rail, Go menu, ⇧C, the
    /// number keys, the shortcuts sheet, the assistant's guide and the route
    /// all read this one property; the rule itself, whether the daemon
    /// serves the timeline, is `CalendarGate.pageAvailable`.
    ///
    /// STORED, and written by `noteStats` only when it changes (the
    /// `statsEpoch` pattern): computed off `stats` it made the rail and the
    /// Go menu re-evaluate on every poll that changed any count. Off the raw
    /// `stats`, not `calendarAccess`: it says only whether a page exists,
    /// never whose calendar is in it.
    private(set) var calendarPageAvailable = false
    /// The epoch of the poll that wrote `stats`. `stats` is not wiped on an
    /// account switch, so until the new mailbox's first poll lands it still
    /// describes the one just left; anything that must not mix two accounts
    /// (the calendar grant and its consent link, `calendarAccess`) reads
    /// `stats` only while this is the live epoch.
    var statsEpoch: Int? {
        didSet { warmCalendarIfDue() }
    }
    /// The epoch of the last stats a LIVE poll wrote. Unlike `statsEpoch`, a
    /// board restored from disk never sets it, so "the daemon has answered
    /// this session" can be told from "we painted what it said last time"
    /// (the first-load veil decides only on the former).
    @ObservationIgnored var liveStatsEpoch: Int?
    var lastRefresh: Date? {
        // `derive` reads only whether there has been one (`daemonDown`).
        didSet { if (oldValue == nil) != (lastRefresh == nil) { noteSyncStatus() } }
    }
    var refreshError: RefreshError? { didSet { noteSyncStatus() } }

    /// Down = refreshes failing AND nothing has ever loaded this session, so the
    /// routed view is replaced by the down pane (Settings excepted: it must stay
    /// reachable to fix the token/URL). Once a sync HAS landed, failures degrade
    /// to a stale-data banner and the stale view stays up.
    var daemonDown: Bool { refreshError != nil && lastRefresh == nil }

    // MARK: routing slice
    var activeView: MainView = .sitrep
    var history: [HistoryEntry] = [HistoryEntry(view: .sitrep, selectedId: nil)]
    var historyIndex = 0

    // MARK: selection slice — stable by message id
    var selectedId: Int?

    // MARK: sitrep zones, see SitrepZones
    /// A `let`, so reading `store.zones` tracks nothing: a view tracks only
    /// the zones it reads through it. Its hooks are set in `init`.
    let zones = SitrepZones()
    /// Every thread a Reading sender's window holds, which is what makes a
    /// thread "reading mail" for the reader (`ThreadViewer.isReadingMail`).
    /// STORED, rebuilt when the Reading zone changes and written only when the set
    /// does: the reader asked it several times a body, and each was a walk
    /// of every sender's every item.
    private(set) var readingThreadIds: Set<String> = []

    private func noteReadingThreads() {
        var ids = Set<String>()
        for sender in zones.reading {
            for item in sender.items { ids.insert(item.thread_id) }
        }
        if ids != readingThreadIds { readingThreadIds = ids }
    }

    // MARK: calendar page, see Model/CalendarStore.swift
    /// The last timeline that landed, held here for the reason the zones are:
    /// the page is unmounted every time you leave it, and coming back should
    /// paint the agenda it had rather than a loading line. Replaced only by a
    /// newer answer; a failed reload keeps it.
    var calendarFeed: TimelineFeed?
    /// A load has finished at least once (success or failure) for this
    /// account, so an empty feed can be told from one still on its way.
    var calendarLoaded = false
    /// The account whose calendar was last warmed (`warmCalendarIfDue`):
    /// its saved feed shown and a fresh one asked for, once per account per
    /// session. Cleared with the rest of the page's state.
    var calendarWarmedFor: UUID?
    /// Agenda or month, and where the month is. Kept here, not in the page,
    /// for the session: the page is unmounted every time you leave it, and
    /// coming back should land where you were. `calendarPlace` nil is today.
    /// The month first (`CalendarViewMode.initial`).
    var calendarMode: CalendarViewMode = .initial
    var calendarPlace: MonthPlace?
    /// The month's levels, deepest last: the selected day's column opened in
    /// place of the grid (narrow windows only), and the item whose detail is
    /// showing. Escape closes them in that order (`calendarMonthBack`).
    var calendarDayOpen = false
    var calendarMonthItem: String?
    /// A search hit to open, ONE SHOT (the phone's Events block). Written only
    /// by `openCalendarSearchHit` and cleared by the phone's calendar as it
    /// pushes the detail. Its own field because `calendarMonthItem` is also set
    /// by every event save (`landEvent`), which must not push a page.
    var calendarSearchOpen: String?
    /// A ticket to show, ONE SHOT: the card's Tickets button or a reminder's
    /// tap writes it, and `TicketPreviewHost` (Views/TicketPreview.swift),
    /// mounted once at the root, opens it in Quick Look (or Wallet) and
    /// clears it.
    var ticketPreview: TicketPreviewRequest?
    /// The owner's event reminder switch, as the daemon holds it. Fetched by
    /// the settings card (Model/CalendarReminders.swift).
    var calendarReminderPref: CalendarReminderPrefState = .unknown
    /// The connect card, while it is up. Its own property, apart from the
    /// feed: `modalOverlayOpen` reads it, and the whole shell reads that.
    var calendarConnectAsk: CalendarConnectAsk?
    /// Since Connect was last pressed, and for which account: the page says
    /// it is waiting for Google until the grant shows up on the stats poll or
    /// the consent session has certainly died (`CalendarGate.connectWaitWindow`).
    var calendarConnectWait: CalendarConnectWait?
    /// A self-host daemon has been asked for a consent link and has not
    /// answered yet: Connect does nothing meanwhile, so one press is one tab.
    var calendarConnectStarting = false
    /// The event pane, while it is open (`CalendarEditorRequest`): which
    /// event, and what its fields opened with. A PANE beside the page, like
    /// the composer (Views/CalendarEventPane.swift), so it is NOT in
    /// `modalOverlayOpen`: nothing blurs and the month beside it stays live.
    /// Written only on open and close, so the shell may read it
    /// (`eventPaneOpen`).
    var calendarEditor: CalendarEditorRequest?
    /// The pane's fields as they stand, rewritten on every keystroke. Apart
    /// from the request for the reason `compose` is apart from `composeOpen`:
    /// the shell must not re-evaluate while somebody types a title. Read by
    /// the pane, and by the calendar page for the draft's chip on the grid.
    var calendarDraft: EventDraft?
    /// A sweep is live on the day column (drag to create): Escape cancels it
    /// through this before it does anything else. Bookkeeping, never drawn.
    @ObservationIgnored var calendarSweepCancel: (() -> Void)?
    /// The one-question card before an event is deleted from Google, raised
    /// from the detail pane (the editor asks in its own footer instead).
    var calendarDeleteAsk: CalendarDeleteAsk?
    /// A write to Google is out: the card's buttons wait, so one Enter is
    /// one event.
    var calendarWriting = false
    /// Where the event pane's write stands (`EventPaneState`): editable,
    /// out, frozen after an answer that never came, or done. Reset with the
    /// pane.
    var calendarPaneState: EventPaneState = .editing
    /// What the pane last sent, kept whole while it is frozen: "send again"
    /// sends exactly this, "check again" looks for exactly this.
    var calendarPaneSent: SentWrite?
    /// Bumped by every timeline request and every local write to the feed;
    /// an answer is applied only if nothing newer has started since it was
    /// asked for. Not observed: it is bookkeeping, never drawn.
    @ObservationIgnored var calendarLoadGeneration = 0
    /// Bumped by `dropCalendarState` (an account switch, the gate closing):
    /// a write still out when it was bumped answers into nothing
    /// (`CalendarWriteTicket`). Bookkeeping, never drawn.
    @ObservationIgnored var calendarSession = 0
    /// The event pane was left with something typed in it: the request and
    /// the draft are kept, the pane is not drawn, and coming back to the
    /// Calendar page brings it back as it was (`parkEventEditor`).
    var calendarEditorParked = false
    /// The pane on screen (or parked) was filled in by THE ASSISTANT, not by
    /// the user's hands (`proposeEvent`). While it is, and until
    /// `calendarPaneEngaged`, the pane takes no keyboard and ↵ / ⌘↵ do
    /// nothing in it (`EventProposals.keysMayAct`). Reset with the pane.
    var calendarPaneProposed = false
    /// The user has clicked or typed inside a proposed pane: its keys are
    /// theirs from here.
    var calendarPaneEngaged = false
    /// When the last full zone refresh COMPLETED. nil = never loaded. OUT of
    /// the observed struct: it is a TTL stamp, and stamping it invalidated
    /// every zone view once per pass whether or not a row had changed.
    @ObservationIgnored private var zonesLoadedAt: Date?
    /// The last pass missed a zone and its one early retry is spent or
    /// pending — see `performZoneRefresh`.
    @ObservationIgnored private var zoneRetryOwed = false

    // MARK: surfaces
    var sideView: SideView = .none
    /// Survives the search panel closing — see SearchSession.
    var search = SearchSession()
    /// The fullscreen email viewer. Orthogonal to sideView (it layers above),
    /// so closing the viewer restores whatever was underneath.
    var threadId: String?
    /// The ordered list the viewer was opened FROM, so "done + next" (e/d) can
    /// advance in place. Empty when opened from a surface without a queue.
    var threadQueue: [AttentionUpdate] = []

    /// THE SPACE WALK: a run through For-your-eyes started with Space on the
    /// sitrep, alive for as long as the reader stays open. While it is, the
    /// reader shows the E/S hint; when it reaches the end of the list it
    /// becomes a `WalkSummary` and the sitrep celebrates. Any other way out of
    /// the reader (Esc, the rail) just ends it, quietly. Inside it plain `e`
    /// and `s` are `E` and `S`.
    struct Walk: Equatable {
        var startedAt = Date()
        var done = 0
        var skipped = 0
    }
    var walk: Walk? {
        didSet {
            // THREE EMAILS IN IS A WALK, not a stray Space. Noted here, on
            // the crossing, so the three places that step the tally (done,
            // remind, skip) stay one line each and cannot disagree about it.
            let before = (oldValue?.done ?? 0) + (oldValue?.skipped ?? 0)
            let now = (walk?.done ?? 0) + (walk?.skipped ?? 0)
            if before < Self.walkDeepAt, now >= Self.walkDeepAt { Discovery.note(.walkDeep) }
        }
    }
    /// How far into a walk counts as having found flow mode.
    static let walkDeepAt = 3
    /// What a FINISHED walk came to, for the sitrep's moment. Cleared by the
    /// moment itself once it has played.
    struct WalkSummary: Equatable {
        var done: Int
        var skipped: Int
        var duration: TimeInterval
        let id = UUID()
    }
    var walkFinished: WalkSummary?

    /// AN S IN FLIGHT: the page turn from one email to the next, with BOTH on
    /// screen. The outgoing reader stays mounted (its mail already loaded, so
    /// it never flashes back to "loading") and recedes left while the next
    /// one slides in over it from the right; the subject bar fades out and
    /// back in rather than sliding. RootView reads this to lay the two
    /// readers out; ThreadViewer's skip drives it and clears it.
    struct SkipTurn: Equatable {
        let outgoing: String
        /// Set when the turn crosses to ANOTHER Reading sender: whether the
        /// card turning away was on a stack. The store's queue is already the
        /// new sender's by then, and the outgoing card must keep the shape it
        /// had rather than re-dress itself halfway off the screen. nil for a
        /// turn inside one queue, where the two agree.
        var outgoingStacked: Bool?
        /// 0 = the turn has not moved yet, 1 = the new email is home.
        var progress: CGFloat = 0
        var outgoingHeader: Double = 1
        var incomingHeader: Double = 0
        /// An undone S: the outgoing card slides back off to the right, ON TOP,
        /// and the incoming one comes forward from where it receded.
        var reversed = false
    }
    var skipTurn: SkipTurn?
    /// A FLOW MODE KEY PRESSED MID-FLIGHT, kept rather than dropped. An `e`
    /// or `s` that lands while an email is still leaving or walking in used
    /// to be thrown away, so a fast reader's second tap did nothing and the
    /// walk read as stuck. One is kept (a held-down queue of ten would walk
    /// past mail nobody saw) and the reader that lands next plays it. Every
    /// way out of the reader, and every undo, drops it.
    enum WalkStep: Equatable { case done, skip }
    var queuedWalkStep: WalkStep?

    /// Keep `step` for the reader that lands next. Only in flow mode, only one.
    func queueWalkStep(_ step: WalkStep) {
        guard walk != nil, queuedWalkStep == nil else { return }
        queuedWalkStep = step
    }

    /// The upcoming emails of the open queue, nearest first, by the same rule
    /// as `nextQueuedThread`: other threads only, nothing already resolved.
    func upcomingQueuedThreads(_ n: Int) -> [AttentionUpdate] {
        guard n > 0, let threadId,
            let current = threadQueue.firstIndex(where: { $0.thread_id == threadId })
        else { return [] }
        var seen: Set<String> = [threadId]
        var out: [AttentionUpdate] = []
        for u in threadQueue.dropFirst(current + 1)
        where !resolvedIds.contains(u.id) && seen.insert(u.thread_id).inserted {
            out.append(u)
            if out.count == n { break }
        }
        return out
    }
    /// An undo asking the reader on `from` to play a walk step backwards.
    /// The reader drives the motion (see ThreadViewer.rewind), as it does
    /// every other one.
    var walkRewind: WalkRewind?
    /// An undo is being fired; the next waits for it (see `fireUndo`).
    @ObservationIgnored private var undoBusy = false
    /// True while an email is leaving and ANOTHER is about to take its place
    /// (done + next, remind + next). Only then does RootView lay its
    /// reader-coloured floor under the flight; on a departure that closes the
    /// reader, the sitrep showing through IS the destination arriving.
    var flightContinues = false

    /// Finish an S turn that is still settling, at once: called before
    /// anything else moves the reader, so the skipped card never ghosts
    /// under the next flight.
    func settleSkipTurn() {
        skipTurn = nil
    }

    /// Space on the sitrep: open the first row with the whole band as the
    /// queue, and start counting.
    func startWalk(_ threadId: String, queue: [AttentionUpdate]) {
        openThread(threadId, queue: queue, from: .sitrep)
        walk = Walk()
        // Built now, off the main thread, so the end of the walk does not
        // hitch compiling a shader at the exact moment it should feel good.
        #if os(macOS)
            PhosphorScene.walkDone.warm()
        #endif
    }

    /// The walk stepped past its last email. Close the reader and hand the
    /// tally to the sitrep, which plays the moment.
    func finishWalk() {
        let finished = walk
        closeThread()
        guard let finished, finished.done + finished.skipped > 0 else { return }
        Discovery.note(.walkFinished)
        walkFinished = WalkSummary(
            done: finished.done, skipped: finished.skipped,
            duration: Date().timeIntervalSince(finished.startedAt))
    }

    /// An undo of a walk's E or S: take the step back off the tally and ask
    /// the reader to play it backwards. Only while the walk is still on the
    /// email the step landed on; anywhere else the undo is just an undo.
    func rewindWalk(_ r: WalkRewind) -> Bool {
        // Settled, or the reader could not play it and the tally would drop
        // for a step that never moved.
        guard walk != nil, threadId == r.from, threadFlight == .settled, walkRewind == nil,
            threadQueue.contains(where: { $0.thread_id == r.back })
        else { return false }
        switch r.step {
        case .done: walk?.done = max(0, (walk?.done ?? 0) - 1)
        case .skip: walk?.skipped = max(0, (walk?.skipped ?? 0) - 1)
        }
        walkRewind = r
        return true
    }

    /// A skip's undo is nothing but the way back, so it goes when the walk
    /// does rather than sitting on screen as a key that does nothing.
    private func dropSkipUndos() {
        undos.removeAll { $0.kind == .skip }
    }

    // MARK: smart clear

    /// SMART CLEAR: one press on the Reading zone and the daemon marks expired
    /// and plainly unwanted mail done. What it took is shown as a receipt
    /// with an undo that reopens exactly that batch.
    struct SmartClearReceipt: Equatable, Identifiable {
        let id = UUID()
        /// nil when this daemon kept no batch: a receipt with no undo.
        var batchId: String?
        var cleared: Int
        var senders: Int
        var byReason: [String: Int]
        var messageIds: [Int]
        /// Why the model pass was meant to run and did not; nil when it ran
        /// or was never needed.
        var modelNote: String?
        var createdAt = Date()
        /// When the receipt, and the undo on it, leave. A clock that can be
        /// stopped: looking at the full list costs none of the window.
        var clock: SmartClearClock
        /// Every email cleared, for the full list. nil when the daemon did
        /// not send them. Mail content: it lives here and nowhere else.
        var items: [SmartClearItem]?
        /// The undo request is out.
        var undoing = false
    }
    /// The receipt's full list is up. While it is, the undo clock is stopped.
    private(set) var smartClearListOpen = false

    /// Open the full list of what the receipt on screen cleared, and stop
    /// the undo clock for as long as it is up.
    func openSmartClearList() {
        guard smartClearReceipt?.items?.isEmpty == false, !smartClearListOpen else { return }
        smartClearReceipt?.clock.pause(at: Date())
        smartClearListOpen = true
    }

    /// Leave the full list BY ANY ROUTE: the clock starts again from where
    /// it stopped. Safe to call twice, and with no list up.
    func closeSmartClearList() {
        smartClearListOpen = false
        smartClearReceipt?.clock.resume(at: Date())
    }
    /// The daemon is working on it: up to about twenty seconds.
    var smartClearWorking = false
    var smartClearReceipt: SmartClearReceipt? {
        // The list is the receipt's: it cannot outlive it by any route.
        didSet { if smartClearReceipt == nil, smartClearListOpen { smartClearListOpen = false } }
    }

    /// How long a receipt and its undo stay. Six times an ordinary undo chip:
    /// this one took a batch of mail nobody looked at one by one, and reading
    /// what it took is the point of showing it.
    static let smartClearUndoTTL: TimeInterval = 30
    /// A receipt with nothing to undo is only news.
    private static let smartClearNewsTTL: TimeInterval = 9

    /// The receipt is on screen with a live undo, so `u` means that. Only
    /// where the receipt is drawn (the bare sitrep): a key must not undo
    /// something the person cannot see.
    var smartClearUndoOffered: Bool {
        guard let receipt = smartClearReceipt else { return false }
        return receipt.batchId != nil && !receipt.undoing && showsSitrepCorner
    }

    /// The sitrep with nothing over it: where the bottom-right corner's cards
    /// (the unsubscribe tray, the smart clear receipt) are drawn.
    var showsSitrepCorner: Bool {
        activeView == .sitrep && threadId == nil && !composeOpen
    }

    /// The press. Asks the daemon, takes what it cleared off the board at
    /// once, and puts up the receipt.
    ///
    /// NEVER FROM THE FIXTURE INBOX: its mail is fixtures, and the real
    /// daemon would be asked to clean a mailbox the person is not looking at.
    func runSmartClear() async {
        guard !smartClearWorking else { return }
        guard !FixtureMode.isEnabled else {
            pushToast(SmartClearCopy.unavailable, .info)
            return
        }
        let e = epoch
        smartClearWorking = true
        await settleAttention()
        // An account switch while the looks were landing: this press was for
        // the mailbox that is gone.
        guard e == epoch else { return }
        // A second run closes the first one's undo: the daemon undoes by
        // batch, and two receipts in one corner would be two countdowns.
        smartClearReceipt = nil
        let result: SmartClearResult?
        do {
            result = try await APIClient.shared.smartClear()
        } catch let error as APIError where error.status == 429 {
            // One press at a time, daemon-side: another device got there
            // first. Its sweep is this mailbox's too, so show the result.
            guard e == epoch else { return }
            smartClearWorking = false
            pushToast(SmartClearCopy.alreadyRunning, .info)
            await refreshZones(force: true)
            return
        } catch {
            guard e == epoch else { return }
            smartClearWorking = false
            pushToast(SmartClearCopy.failed, .error)
            // A timeout is not proof nothing happened: show what the daemon
            // has now rather than what the board had before.
            await refreshZones(force: true)
            return
        }
        // The OLD account's daemon answered; its ids mean nothing here. The
        // switch's wipe already put the pill back to rest.
        guard e == epoch else { return }
        smartClearWorking = false
        guard let result else {
            pushToast(SmartClearCopy.unavailable, .info)
            return
        }
        // Counts only: never a sender, an address or a word of mail.
        Analytics.capture(
            "smart_clear",
            [
                "cleared": result.cleared,
                "senders": result.senders,
                "event_passed": result.by_reason["event_passed"] ?? 0,
                "offer_ended": result.by_reason["offer_ended"] ?? 0,
                "expired": result.by_reason["expired"] ?? 0,
                "ignored_sender": result.by_reason["ignored_sender"] ?? 0,
                "skimmed_sender": result.by_reason["skimmed_sender"] ?? 0,
                "stale_promo": result.by_reason["stale_promo"] ?? 0,
                "model_ran": result.model == "ok",
                "undoable": result.batch_id != nil,
            ])
        guard result.cleared > 0 else {
            pushToast(
                SmartClearCopy.nothing(model: result.model, detail: result.model_detail), .info)
            return
        }
        // Only a press that cleared something: an empty sweep showed nothing.
        Discovery.note(.smartCleared)
        showSmartClearReceipt(result)
        await sweepResolved(result.message_ids, epoch: e)
        guard e == epoch else { return }
        // The tiles left optimistically; this makes the daemon's list the
        // thing on screen.
        await refreshZones(force: true)
    }

    private func showSmartClearReceipt(_ result: SmartClearResult) {
        let ttl = result.batch_id == nil ? Self.smartClearNewsTTL : Self.smartClearUndoTTL
        let now = Date()
        let receipt = SmartClearReceipt(
            batchId: result.batch_id, cleared: result.cleared, senders: result.senders,
            byReason: result.by_reason, messageIds: result.message_ids,
            modelNote: SmartClearCopy.modelNote(result.model, detail: result.model_detail),
            createdAt: now, clock: SmartClearClock(startedAt: now, lasting: ttl),
            items: result.rows.map { rows in
                rows.map {
                    SmartClearItem(
                        id: $0.message_id, reason: $0.reason ?? "", sender: $0.sender,
                        senderName: $0.sender_name, subject: $0.subject, receivedAt: $0.received_at)
                }
            })
        smartClearListOpen = false
        smartClearReceipt = receipt
        let id = receipt.id
        Task { [weak self] in
            // The deadline can MOVE (the full list stops the clock), so this
            // asks the clock each time it wakes instead of sleeping the TTL.
            while let self, let live = self.smartClearReceipt, live.id == id {
                let left = live.clock.remaining(at: Date())
                if live.clock.isPaused || live.undoing {
                    // Not while its undo is out either: that request's
                    // answer still has to find the receipt it belongs to.
                    try? await Task.sleep(for: .milliseconds(400))
                } else if left <= 0 {
                    self.smartClearReceipt = nil
                    return
                } else {
                    try? await Task.sleep(for: .seconds(left))
                }
            }
        }
    }

    func dismissSmartClearReceipt() {
        guard smartClearReceipt?.undoing != true else { return }
        smartClearListOpen = false
        smartClearReceipt = nil
    }

    /// Reopen exactly what the receipt on screen cleared.
    func undoSmartClear() async {
        guard let receipt = smartClearReceipt, !receipt.undoing, let batch = receipt.batchId
        else { return }
        smartClearReceipt?.undoing = true
        let e = epoch
        do {
            let undone = try await APIClient.shared.undoSmartClear(batchId: batch)
            // Reopened on the OLD account's daemon: nothing to tell this one.
            guard e == epoch else { return }
            unresolve(Set(receipt.messageIds))
            if smartClearReceipt?.id == receipt.id { smartClearReceipt = nil }
            pushToast(SmartClearCopy.undone(reopened: undone.reopened), .info)
            Analytics.capture("smart_clear_undone", ["reopened": undone.reopened])
            // The refresh after the clear took these tiles out of the zone's
            // rows for real, so un-resolving alone shows nothing: the board
            // has to be read again. Forced, to outlast a pass already out.
            await refreshZones(force: true)
            await SitrepPoller.shared.pull()
        } catch {
            guard e == epoch else { return }
            if smartClearReceipt?.id == receipt.id { smartClearReceipt?.undoing = false }
            pushToast("Undo did not go through. Try again.", .error)
        }
    }

    /// Take a batch of resolved mail off the Reading zone AS A SWEEP: sender
    /// by sender in the order the board draws them, a few at a time, so the
    /// eye sees the zone being cleared rather than a grid that blinked. Ids
    /// not on the board are simply recorded.
    private func sweepResolved(_ ids: [Int], epoch e: Int) async {
        let all = Set(ids)
        guard !all.isEmpty else { return }
        var onBoard: [[Int]] = []
        var seen = Set<Int>()
        for sender in Reading.prune(zones.reading, resolved: resolvedIds) {
            let mine = sender.items.map(\.id).filter(all.contains)
            guard !mine.isEmpty else { continue }
            onBoard.append(mine)
            seen.formUnion(mine)
        }
        noteResolved(all.subtracting(seen))
        guard !onBoard.isEmpty else { return }
        #if os(macOS)
            let still = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        #else
            let still = UIAccessibility.isReduceMotionEnabled
        #endif
        if still {
            withAnimation(.easeOut(duration: 0.25)) { noteResolved(seen) }
            return
        }
        // At most eight waves however many tiles there are: the sweep should
        // take about half a second whether it clears four senders or forty.
        let perWave = max(1, Int((Double(onBoard.count) / 8).rounded(.up)))
        var i = 0
        while i < onBoard.count {
            guard e == epoch else { return }
            let wave = Set(onBoard[i..<min(i + perWave, onBoard.count)].joined())
            withAnimation(.smooth(duration: 0.42)) { noteResolved(wave) }
            i += perWave
            try? await Task.sleep(for: .milliseconds(60))
        }
    }
    var readingMail = false
    var readingStack: Bool {
        guard readingMail, let first = threadQueue.first else { return false }
        return threadQueue.dropFirst().contains { $0.thread_id != first.thread_id }
    }

    /// THE READING BOARD'S ORDER WHEN A TILE WAS OPENED, as sender addresses:
    /// what `E` and `S` walk once one sender's mail runs out. A snapshot for
    /// the reason `threadQueue` is one (see `Reading.next`). Empty unless the
    /// reader came in through a Reading tile.
    var readingOrder: [String] = []

    /// What the Reading zone draws: its senders, pruned of mail already
    /// resolved here.
    var readingOnBoard: [ReadingSender] { Reading.prune(zones.reading, resolved: resolvedIds) }

    /// Resolve the heroes of the Reading cards on screen. Those past the fold
    /// resolve their own as they mount, or in the launch warm's deferred half
    /// once the board has been quiet.
    private func preloadBoardHeroes() {
        HeroCache.shared.preload(readingOnBoard.prefix(BoardFold.reading).map(\.latestThreadId))
    }

    /// A Reading tile: the sender's newest email, with the rest of that sender
    /// as the queue and the rest of the board behind it.
    func openReading(_ sender: ReadingSender) {
        guard !sender.latestThreadId.isEmpty else { return }
        readingOrder = readingOnBoard.map(\.address)
        openThread(
            sender.latestThreadId, queue: sender.items, readingMail: true, from: .reading)
    }

    /// The sender after the one the reader is on. Prunes the board, so it is
    /// for a key handler to ask, not a view body.
    var nextReadingSender: ReadingSender? {
        guard readingMail, let threadId, let first = threadQueue.first else { return nil }
        return Reading.next(
            after: SenderID.address(first.sender), in: readingOrder,
            board: readingOnBoard, notThread: threadId)
    }

    /// A FLOW MODE WALK RUNS TO THE END OF THE BOARD, not the end of the board
    /// as it stood when Space was pressed. The queue is a snapshot, and the
    /// poll keeps filling the band behind it: a Space pressed before the first
    /// full load, or mail that arrived mid-walk, left rows on the board that
    /// the walk would never reach, so an `e` on the snapshot's last row closed
    /// flow mode with mail still waiting. Rows the band holds that the queue
    /// does not are appended at the end (the steps already taken keep their
    /// order, so undo still finds them). Called by the reader's step verbs
    /// just before they ask what comes next.
    func topUpWalkQueue() {
        guard walk != nil, !readingMail, threadId != nil else { return }
        let have = Set(threadQueue.map(\.thread_id))
        var seen = have
        let fresh = sitrep.standing.filter { u in
            guard !seen.contains(u.thread_id), !resolvedIds.contains(u.id),
                authMeta(for: u) == nil
            else { return false }
            seen.insert(u.thread_id)
            return true
        }
        guard !fresh.isEmpty else { return }
        threadQueue += fresh
    }

    /// Shared by the preview and advance action so resolved siblings never peek.
    var nextQueuedThread: AttentionUpdate? {
        guard let threadId,
            let current = threadQueue.firstIndex(where: { $0.thread_id == threadId })
        else { return nil }
        return threadQueue.dropFirst(current + 1).first {
            $0.thread_id != threadId && !resolvedIds.contains($0.id)
        }
    }
    /// WHERE THE READER IS IN ITS FLIGHT. `settled` is where it lives and is the
    /// default, because opening an email is navigation: a surface people enter
    /// and leave all day should just BE there. The other two are the done+next
    /// flight, and it is deliberately a position rather than a SwiftUI
    /// transition — a transition is chosen by the framework at the moment a view
    /// is inserted or removed, and when it declines to use the one you gave it
    /// you get its default crossfade and no way to argue. An offset is not
    /// negotiable.
    ///
    /// ThreadViewer drives it in two beats: `departing` lifts the email you
    /// finished off the top of the window, then the next one mounts `entering`
    /// from an edge and is walked home. See `doneAndNext`.
    var threadFlight: ThreadFlight = .settled

    enum ThreadFlight: Sendable, Equatable {
        /// In the window, where the reader belongs.
        case settled
        /// Lifted out through the top: the email you just finished.
        case departing
        case departingDown
        /// An undone same-sender done: back out the edge it came in from.
        case departingTrailing
        /// Parked one window outside the frame, waiting to be walked in.
        case entering(ThreadEdge)

        var isEntering: Bool {
            if case .entering = self { return true }
            return false
        }
    }

    /// Which side a thread arrives from. The bottom is a new subject; the
    /// trailing edge is MORE MAIL FROM THE SAME SENDER, because that is the next
    /// item in a pile rather than the next thing to think about.
    enum ThreadEdge: Sendable, Equatable {
        case bottom
        case trailing
        /// Only an undone done: the email comes back down the way it left.
        case top
    }
    /// What that thread IS, once it has landed — written by the viewer. nil
    /// while a thread is loading. READ IT THROUGH `currentThreadSummary`: this
    /// is the raw last write, and the writer is a network callback.
    var openThreadSummary: OpenThreadSummary?
    /// The summary ONLY IF it describes the thread that is open. The viewer
    /// writes it from a fetch callback, so a slow load can land after the user
    /// has moved on; `openThread` clears the stale one on the way out, and this
    /// is the second lock on the same door — the ask bar pins what it reads
    /// here into a prompt, where a subject and a message id belonging to
    /// another email would be a lie told confidently.
    var currentThreadSummary: OpenThreadSummary? {
        guard let threadId, let summary = openThreadSummary, summary.threadId == threadId
        else { return nil }
        return summary
    }
    /// Bumped when a message NEWER than the one the reader holds lands in the
    /// thread on screen — from the live event feed the instant triage emits it,
    /// or from the poll behind it. The viewer watches it and refetches
    /// without moving the reading position. See `noteThreadArrival`.
    ///
    /// A token rather than the mail itself: neither reporter carries the
    /// thread's new contents (the feed has one denormalized row, the poller
    /// reads the attention bands, which say a thread moved and not what it now
    /// holds), and the viewer is the one place that knows how to adopt a
    /// thread. It is also why nothing resets it — a counter only has to CHANGE,
    /// so a thread switch under a live token is a viewer that simply never
    /// hears about the bump it no longer cares about.
    var openThreadRefreshToken = 0
    /// Bumped by the menu's Find in Email (⌘F with a thread open). The viewer
    /// watches it and opens its find bar: the session is the reader's own
    /// state, so the store only has to say that it was asked for.
    var findInEmailTick = 0
    /// WHAT THE HUMAN HAS ALREADY BEEN TOLD about the thread they are reading.
    /// Private because `noteThreadArrival` is the only door: the refetch and
    /// the announcement have different rules, and ThreadArrivals is where the
    /// difference lives.
    private var arrivals = ThreadArrivals()
    /// HOW LONG EACH MESSAGE HAS BEEN IN FRONT OF THE READER, and what was
    /// clicked in it. Owned here for the reason `arrivals` is: `openThread`
    /// and `closeThread` are the only two doors a thread goes through, and a
    /// clock that lived in the reader view would be two clocks during an `S`
    /// page turn, when two readers are mounted at once.
    ///
    /// Untracked, because nothing draws it; a view body that happened to call
    /// one of the doors below must not become a reader of the dwell ledger.
    @ObservationIgnored private var attention = AttentionClock()
    /// The reports last sent, so a later flush queues behind them and smart
    /// clear can wait for every look to land before it judges anyone's.
    @ObservationIgnored private var attentionFlight: Task<Void, Never>?
    /// The epoch `attention` was last touched under. Message ids are one
    /// daemon's SQLite ints, so time banked against account A's message 40 is
    /// a lie about account B's message 40. Compared on every touch rather than
    /// reset beside each of the dozen places the epoch is bumped: one of those
    /// would be missed, and the failure would be silent.
    @ObservationIgnored private var attentionEpoch = 0
    var compose: ComposeState? {
        didSet {
            let open = compose != nil
            if composeOpen != open { composeOpen = open }
        }
    }
    /// WHETHER a composer is open, kept apart from WHICH: `compose` is
    /// rewritten on every keystroke, and the window shell only needs to know
    /// there is a pane to lay out beside. Reading this instead is what keeps
    /// typing from re-evaluating the whole shell.
    private(set) var composeOpen = false
    /// The pane composer opened out to a centred column over the whole page.
    /// Kept across composers on purpose: somebody who writes long mail
    /// full-size wants the next one full-size too.
    var composeExpanded = false
    /// The reader's inline reply composer. Deliberately NOT part of
    /// `modalOverlayOpen`: it is a bar inside the reading surface, not an overlay
    /// on one, so the thread behind it must stay unblurred and clickable — you
    /// answer an email while reading it.
    var inlineReply: ComposeState?
    /// A reply the thread viewer should open its composer on the moment the
    /// thread lands. Set by `openThread(replyTo:)`, so `r` on a list row is one
    /// gesture: navigate, then compose. One-shot — the viewer clears it.
    var pendingReplyMessageId: Int?
    var triageFix: TriageFixTarget?
    var remindTarget: RemindTarget?
    /// Which composer's time palette is open (the clock, or its chord). The
    /// palette writes `sendAt` into that slot and nothing else. Cleared with
    /// the composer it belongs to.
    var schedulePicker: DraftSaver.Slot?
    var ruleEditor: RuleEditorRequest?
    /// Unsaved rule-editor cards by `RuleEditorRequest.draftKey`. Nothing
    /// renders from this, so writing it on every dismiss invalidates nothing.
    @ObservationIgnored var ruleDrafts: [String: RuleDraft] = [:]
    var groupEditor: GroupEditorRequest?
    var askBarOpen = false {
        // Every way the bar closes ends the watch: it is a view of the lane
        // for as long as this one opening lasts, never a mode ⌘K reopens into.
        didSet { if !askBarOpen { askBarWatchesLane = false } }
    }
    /// THE BAR IS SHOWING THE SEARCH LANE, LIVE. ⌘↩ over a lane that is still
    /// answering cannot hand the conversation over (a running loop owns its
    /// history), so the bar draws the lane itself until the run ends and then
    /// `finishLaneHandoff` moves it into the chat, where follow-ups can go.
    private(set) var askBarWatchesLane = false
    /// The session the ⌘K bar draws: the chat, or the lane it is watching.
    var askBarSession: AssistantSession { askBarWatchesLane ? searchLane : assistant }
    /// The ⌘K agent's conversation, HELD HERE rather than in AskBar: the modal
    /// is conditionally mounted, so a view-owned session would be torn down —
    /// mid-answer — every time the bar closed. Living in the store, the
    /// transcript survives until the user asks for a new chat.
    let assistant = AssistantSession()
    /// THE SEARCH PANEL'S OWN AGENT, a second session in the search lane (see
    /// `Lane`, docs/SEARCH.md §6.1). Held here for the reason the ⌘K session is
    /// and one more: `SearchSession` is a struct the store copies, so the
    /// conversation lives beside it and only its bookkeeping goes in it.
    ///
    /// Its own session rather than a mode on the chat's, because the two are
    /// genuinely different conversations: the chat is a thread of questions
    /// somebody typed, the lane is one search being narrowed, and merging them
    /// would put a search the reader never spoke into the ⌘K transcript. The
    /// one crossing is the band's own expand button, which is the reader
    /// asking for exactly that (see `expandSearchLaneToChat`).
    let searchLane = AssistantSession(lane: .search)
    /// Bumped every time something asks for the search field's caret. A
    /// counter, not a flag: `/` over a panel that is already open changes
    /// nothing else the field could watch, and it must still take focus.
    var searchFocusRequest = 0
    var shortcutsOpen = false
    /// The first-load veil over a newly connected mailbox. Held here for the
    /// same reason the assistant is: it outlives every view it draws itself
    /// in, and what drives it (the polls) fires from the shell.
    let firstLoad = FirstLoad()

    /// The what's-new card. Beside the first-load veil on purpose: the two
    /// are mutually exclusive by construction (see WhatsNew), and keeping
    /// them adjacent is what keeps that readable.
    let whatsNew = WhatsNew()

    // MARK: drafts

    /// The account's saved drafts as last fetched, newest edits first as the
    /// daemon lists them, MINUS any whose delete is waiting out its undo. A
    /// cache for the composer's drafts control and drafts page, filled by
    /// `refreshDrafts` and by the ⌘N restore's own fetch, so the count is there
    /// without a second round trip. Held sends are NOT filtered out here,
    /// because which rows are held changes by the second; `listedDrafts` and
    /// `otherDrafts` apply that at read time.
    var draftsRows: [DraftView] = []

    // MARK: undo / toasts
    var undos: [PendingUndo] = []
    var toasts: [Toast] = []

    // MARK: 2FA
    var authRings: [AuthRing] = []
    /// Newest-first queue of code-modal entries (only otp/verification).
    var authQueue: [AuthCodeEntry] = []
    /// Set by `requestAuthPage` on the phone, which has no routed Auth page
    /// for `setView(.auth)` to reach: its shell opens the login-codes page
    /// behind the Quick Look key, then clears this (`consumeAuthPage`). A
    /// pending FLAG, read on the shell's first appearance as well as on
    /// change, because a tap that launched the app is drained the moment
    /// `connStatus` turns `.connected`, before the shell exists to see a
    /// change happen.
    private(set) var pendingAuthPage = false

    /// "Show me the auth mail", from a notification tap. The Mac routes to
    /// its Auth page; the phone opens its login-codes page (see
    /// `pendingAuthPage`). A sheet that is up (a draft, say) stays up: the
    /// page opens behind it rather than a tap discarding someone's work.
    func requestAuthPage() {
        #if os(macOS)
            setView(.auth)
        #else
            closeThread()
            pendingAuthPage = true
        #endif
    }

    /// The phone's shell has opened the page `requestAuthPage` asked for.
    func consumeAuthPage() { pendingAuthPage = false }

    /// A sealed message to open in the one-time reveal panel from somewhere that
    /// is not an auth page: an auth row in For your eyes, or a notification tap
    /// on auth mail. Mounted by ActionLayer on the Mac and by the phone's
    /// shell; the Auth pages keep their own. Cleared by the account wipe.
    ///
    /// Only those surfaces route here. The human thread view SERVES threads
    /// holding auth mail (by design), so search, All Mail, reply and a thread
    /// that mixes a code with correspondence open the normal reader.
    var revealRequest: SealedMeta?
    /// Where `revealRequest` came from, for the reveal's audit source: nil for
    /// a page the human acted on, `RevealSource.notification` for a banner tap.
    private(set) var revealSource: String?

    /// Threads that hold auth mail, per the sealed list AND the For-your-eyes
    /// rows' own decisions: what `ThreadPrefetch` refuses to warm or cache.
    /// A set, kept in step by `sitrep`'s didSet, because every prefetch asks.
    @ObservationIgnored private(set) var authThreadIds: Set<String> = []
    /// The fence a launch painted from disk carried (`PersistedBoard.authThreads`),
    /// kept in `authThreadIds` until the first pull's sealed list stands in
    /// for it (`notePullLanded`).
    @ObservationIgnored private var restoredAuthThreads: Set<String> = []

    private func noteAuthThreads() {
        authThreadIds = AuthPolicy.authThreadIds(standing: sitrep.standing, sealed: sitrep.sealed)
            .union(restoredAuthThreads)
    }

    /// The sealed metadata for one message id, when it is auth mail.
    func sealedMeta(message id: Int) -> SealedMeta? {
        sitrep.sealed.first { $0.id == id }
    }

    /// The auth metadata for a For-your-eyes row, or nil when it is not auth
    /// mail. The row's own decision decides first (`decision.auth.kinds`), so
    /// a sealed list that is blank or stale (an account-switch snapshot) never
    /// turns an auth row into a normal one; the sealed list is the second
    /// signal and, when it has the row, the source of its verdict fields.
    func authMeta(for update: AttentionUpdate) -> SealedMeta? {
        AuthPolicy.authMeta(for: update, sealed: sealedMeta(message: update.id))
    }

    func requestReveal(_ meta: SealedMeta, source: String? = nil) {
        revealSource = source
        revealRequest = meta
    }

    // MARK: read tracking
    /// The daemon's tracking answer, fetched once on connect. nil = never
    /// asked (or the daemon did not answer), which reads exactly like
    /// unconfigured: no toggle anywhere, no receipts fetched.
    var tracking: TrackingConfig?

    // MARK: translation languages
    /// The languages the owner reads, which the daemon leaves untranslated.
    /// Fetched by the settings card, not at connect: nothing else reads it.
    var translateConfig: TranslateConfigState = .unknown

    // MARK: account switching

    /// Bumped on every account switch. Every async writer of account-scoped
    /// state captures it before its first await and refuses to write once it
    /// no longer matches: a request built against account A's daemon can land
    /// after B is live, and message ids are per-daemon SQLite ints — so a
    /// stale answer does not read as stale, it reads as B's mail.
    private(set) var epoch = 0

    /// True for the length of a switch. THE re-entrancy guard: the sequence
    /// suspends twice (the draft settle, the keychain read) and a second
    /// switch starting inside one of those windows would configure the client
    /// out from under the first.
    private(set) var switching = false {
        didSet {
            if !switching { Notifier.shared.drainPendingTap() }
        }
    }
    private(set) var fixtureTransitioning = false
    /// The account a switch is asking after, until it lands or is refused.
    private(set) var switchingTo: UUID?
    /// Counts switch requests, so the newest one is the only one that lands.
    @ObservationIgnored private var switchIntent = 0

    /// Also enforced by account mutation entry points, not only menu items.
    var accountActionsBlocked: Bool {
        FixtureMode.isEnabled || fixtureTransitioning || firstLoad.active
    }
    private static let fixtureAccountError = "Accounts can't change while your inbox is loading. Press Start now, then try again."

    /// Whether `e` is still the live epoch — the post-await check every
    /// account-scoped writer outside this file makes.
    func isCurrent(_ e: Int) -> Bool { epoch == e }

    // MARK: - fye headline

    /// The owner clicked the headline: ask for a different one. An empty
    /// answer keeps the line on screen (nothing flips to the fallback), and a
    /// click while one is already being written is ignored.
    func regenerateFyeHeadline() async {
        guard !sitrep.standing.isEmpty, !fyeHeadlineRegenerating, connStatus == .connected
        else { return }
        let key = headlineKey
        let e = epoch
        fyeHeadlineRegenerating = true
        defer { fyeHeadlineRegenerating = false }
        let answer = try? await APIClient.shared.fyeHeadline(
            today: Self.headlineToday(), fresh: true, previous: fyeHeadline)
        guard isCurrent(e), headlineKey == key, let headline = answer?.headline else { return }
        fyeHeadline = headline
        fyeHeadlineKey = key
        UserDefaults.standard.set([key, headline], forKey: Self.headlineDefaultsKey)
    }

    /// The owner's local date as the headline prompt reads it.
    /// Through `Agenda.format`'s formatters, kept per zone: one made per call
    /// is what this was, and one kept on the zone it was made in would put
    /// the day out by the difference after a flight.
    private static func headlineToday() -> String {
        Agenda.format(Date(), "yyyy-MM-dd (EEEE)", .current)
    }

    /// What a headline is ABOUT: the rows in the band and what triage said of
    /// each, plus the day. Any change here is a board the old sentence may no
    /// longer describe. The day is read as it is (a kept formatter), and the
    /// rows' half is kept in `headlineBand`: this was rebuilt, a new formatter
    /// and a join of every row included, on every board body that keyed its
    /// headline task on it.
    var headlineKey: String { Self.headlineToday() + "|" + headlineBand }

    /// The rows' half of `headlineKey`, rebuilt when the band changes.
    private(set) var headlineBand = ""

    private func noteHeadlineBand() {
        let band = sitrep.standing.map { "\($0.thread_id):\($0.one_line)" }.joined(separator: "|")
        if band != headlineBand { headlineBand = band }
    }

    private static let headlineDefaultsKey = "fyeHeadline.last"

    /// Ask the daemon for the hero's sentence, once the board has held still.
    /// Called from the hero's `.task(id: headlineKey)`, so it only runs while
    /// the sitrep is ON SCREEN (a reader walked with E/S never pays for the
    /// boards it passes through) and a newer board cancels it mid-wait.
    func refreshFyeHeadline() async {
        let key = headlineKey
        guard !sitrep.standing.isEmpty else { return }
        if fyeHeadlineKey == key { return }
        // Last launch's sentence, if it was about this exact board: shown at
        // once instead of the fallback, and the daemon is not asked.
        if let saved = UserDefaults.standard.stringArray(forKey: Self.headlineDefaultsKey),
            saved.count == 2, saved[0] == key
        {
            fyeHeadline = saved[1]
            fyeHeadlineKey = key
            return
        }
        // Settle first. The poller reassigns the band as rows are cleared from
        // the list or arrive in a burst; one call for the board that stuck.
        try? await Task.sleep(for: .milliseconds(1200))
        // A FAILED call (the daemon says `settled: false`) is asked again a
        // couple of times, spaced out; it is not "nothing good to say", and
        // remembering it as that would leave the fallback up all day.
        for attempt in 0..<3 {
            if attempt > 0 { try? await Task.sleep(for: .seconds(30)) }
            let e = epoch
            guard !Task.isCancelled, connStatus == .connected else { return }
            let answer = try? await APIClient.shared.fyeHeadline(today: Self.headlineToday())
            // A click for a new line wins over this background ask: whichever
            // answered last would otherwise replace the line they asked for.
            guard !Task.isCancelled, isCurrent(e), headlineKey == key, !fyeHeadlineRegenerating
            else { return }
            // No answer at all is an old daemon or a dropped connection: keep
            // the fallback and do not retry against a route that is not there.
            guard let answer else { return }
            if answer.settled == false { continue }
            fyeHeadline = answer.headline
            fyeHeadlineKey = key
            if let headline = answer.headline {
                UserDefaults.standard.set([key, headline], forKey: Self.headlineDefaultsKey)
            }
            return
        }
    }

    private init() {
        LaunchSignpost.begin()
        // A thread holding auth mail is never fetched speculatively or kept in
        // the cache: its body is meant to arrive through the audited reveal
        // (or a reader the human opened on purpose), not a hover or a warm.
        // Keyed by `authThreadIds`, which the feed rows' own decisions feed
        // too, so a blank sealed list cannot reopen the door.
        ThreadPrefetch.shared.refuses = { AppStore.shared.authThreadIds.contains($0) }
        // The board file keeps the calendar and the shipments, so those two
        // schedule a save; Reading's threads are kept as a set for the reader.
        zones.onPersistedChange = { [weak self] in self?.scheduleBoardSave() }
        zones.onReadingChange = { [weak self] in self?.noteReadingThreads() }
        // A conversation that belongs to a question in search history is saved
        // whenever one of its runs ends; see `saveQuestionChat`.
        assistant.onRunEnded = { [weak self] in self?.saveQuestionChat(from: $0) }
        searchLane.onRunEnded = { [weak self] in self?.saveQuestionChat(from: $0) }
        // The board on disk is brought up to date whenever the app leaves the
        // screen, and before it quits. See `boardSaveMoments`.
        for moment in Self.boardSaveMoments {
            let waits = moment.waits
            let quits = moment.quits
            NotificationCenter.default.addObserver(forName: moment.name, object: nil, queue: .main) {
                [weak self] _ in
                // Directly, not via a Task, for the reason SitrepWindow gives:
                // a quit never runs the next runloop turn. Delivered on the
                // main queue, so the assumeIsolated is honest.
                MainActor.assumeIsolated {
                    self?.saveBoardNow(waitingForDisk: waits, evenIfUnchanged: quits)
                }
            }
        }
    }

    // MARK: - settings

    // MARK: - fixture inbox

    /// Put the fixture inbox on screen IN PLACE: the same shell, `FixtureMail`
    /// behind the same transport. For the phone's debug screenshot harness
    /// (`FixtureStage`) and nothing else. Shaped like `switchAccount` because
    /// it is one (the account arriving just happens to be fictional), and
    /// every answer still in flight for a live mailbox has to be disowned
    /// before a fixture row can be allowed anywhere near the read model.
    func enterFixtures() async {
        while switching {
            if Task.isCancelled { return }
            try? await Task.sleep(for: .milliseconds(25))
        }
        guard !Task.isCancelled else { return }
        switching = true
        fixtureTransitioning = true
        defer { switching = false; fixtureTransitioning = false }
        epoch &+= 1
        SitrepPoller.shared.stop()
        // The feeds too, unlike a switch: a banner for real mail would point
        // at a thread the fixture board cannot open.
        AccountManager.shared.stopAllFeeds()
        connStatus = .loading
        DraftSaver.shared.flush(.compose, compose)
        DraftSaver.shared.flush(.inlineReply, inlineReply)
        parkHeldSends()
        await DraftSaver.shared.settle()
        // Work queued during draft settlement belongs to the departing world
        // too. Fence again at the synchronous transport/read-model swap.
        epoch &+= 1
        SitrepPoller.shared.stop()
        assistant.clear()
        wipeAccountState()
        wipeAccountCaches()
        closeOverlaysForFixtures()
        FixtureMode.setEnabled(true)
        await FixtureAPI.shared.reset()
        // The fixture's calendar items keep the same ids from launch to
        // launch, and the zone retires an item a day after it was first shown.
        CalendarVisibility.shared.forget(account: CalendarVisibility.fixtureAccount)
        settings = ConnectionSettings(serverURL: "https://fixtures.invalid", apiToken: "fixtures")
        connError = nil
        connStatus = .connected
    }

    /// `--fixture-usage` (FixtureMode.stagesUsage): the fixture transport with
    /// the Understanding fixture staged, and the Usage page on screen. Staged
    /// BEFORE the fixtures are entered, so the first stats already claim the
    /// feature. Unreachable in a shipped build, where the flag is always
    /// false and the staging is compiled out.
    func stageUsagePage() async {
        guard FixtureMode.stagesUsage else { return }
        #if !PASSBAND_RELEASE
            await FixtureAPI.shared.stageUnderstanding()
        #endif
        await enterFixtures()
        setView(.usage)
    }

    /// Fill the fixture board before a scene is raised over it: bands, zones,
    /// the calendar bodies, and the inbox page. Starts the poller, which an
    /// entry from a connected state otherwise never would (`connStatus`
    /// never moved).
    func warmFixtures() async {
        guard FixtureMode.isEnabled else { return }
        let e = epoch
        SitrepPoller.shared.start()
        await SitrepPoller.shared.pull()
        guard isCurrent(e) else { return }
        await refreshZones(force: true)
        guard isCurrent(e) else { return }
        for threadID in zones.calendar.compactMap(\.thread_id) {
            ThreadPrefetch.shared.prefetch(threadID, fresh: 15 * 60)
        }
        await refreshMail(.inbox, force: true)
    }

    private func closeOverlaysForFixtures() {
        toasts = []
        askBarOpen = false
        shortcutsOpen = false
        addAccountSheetOpen = false
        shareSheetOpen = false
        ruleEditor = nil
        ruleDrafts = [:]
        remindTarget = nil
        schedulePicker = nil
    }

    func loadSettings() async {
        guard !FixtureMode.isEnabled else {
            LaunchSignpost.end(.skipped)
            return
        }
        // The index says WHICH account's credentials to read, and repairs a
        // pre-multi-account install on the way through. Off the main actor for
        // the same reason the load below is.
        let index = await AccountIndex.loadOrMigrate()
        // Into the observable mirror before anything reads it: the manager may
        // have been built — and read a pre-repair index — during boot.
        AccountManager.shared.adopt(index)
        guard let active = index.active else {
            connError = nil
            connStatus = .disconnected
            return
        }
        // The last board on disk is read beside the credentials rather than
        // after them; it is painted only once they are in.
        async let keptBoard = BoardFile.load(account: active.id)
        // Off the main actor: a keychain read can put up the system's "allow
        // access?" panel and block until answered.
        switch await SettingsStore.loadAsync(accountId: active.id) {
        case .success(let stored):
            if let stored {
                await APIClient.shared.configure(
                    baseURL: stored.serverURL, token: stored.apiToken)
                settings = stored
                // THE LAST BOARD FIRST, when one is kept (BoardFile.swift):
                // the shell mounts on it at once, and the daemon is asked
                // beside it rather than in front of it. Without one, the
                // loading gate waits for the daemon as it always has.
                // Read only when it could be painted (not, say, from the error
                // gate's retry), and every condition asked again after it.
                var kept: PersistedBoard?
                if connStatus == .loading, !FixtureMode.isEnabled { kept = await keptBoard }
                if let board = kept, AccountManager.shared.activeId == active.id,
                    connStatus == .loading, !FixtureMode.isEnabled
                {
                    restorePersistedBoard(board)
                    connError = nil
                    unverifiedEpoch = epoch
                    connStatus = .connected
                    // A link that arrived during the reads was parked under
                    // the loading gate; the shell is up now, so it is handled
                    // as the probe's success handles one on the waiting path.
                    #if os(iOS)
                        pairLink = nil
                    #else
                        if pairLink != nil { addAccountSheetOpen = true }
                    #endif
                    recoverSavedConnection(stored, accountId: active.id, behindBoard: true)
                } else {
                    recoverSavedConnection(stored, accountId: active.id)
                }

            } else {
                connStatus = .disconnected
                connError = "Your saved mailbox connection is incomplete. Reconnect to open your inbox."
            }
        case .failure:
            connStatus = .disconnected
            connError = "Passband couldn’t unlock your saved mailbox connection. Try again or reconnect."
        }
        // Then the boards nothing will paint: other accounts' gone stale, and
        // any of an account no longer here. Behind the paint, not in front of
        // it on the queue.
        BoardFile.sweep(keeping: Set(index.accounts.map(\.id)))
        FrameHeightStore.sweep(keeping: Set(index.accounts.map(\.id)))
        let accounts = Set(index.accounts.map(\.id))
        Task.detached(priority: .utility) { await CalendarDisk.shared.sweep(keeping: accounts) }
    }

    /// Take a `passband://` URL the OS handed us. Never acted on here — it is
    /// parked for a ConnectView to pick up, and that view only ever fills its
    /// form from it. Which ConnectView depends on what this install already is:
    ///
    /// - No identity yet: the Connect gate is on screen and reads it there.
    /// - Already connected: the gate is not mounted, and the link is a SECOND
    ///   daemon asking to be added, so the Add Account sheet is opened for it.
    ///   On the phone, only into a sheet that is already open — see
    ///   `PairLink.admits` for why.
    ///   Re-pairing over the identity this install holds is still impossible —
    ///   the sheet adds an account beside it, it does not replace it.
    func receivePairLink(_ url: URL) {
        guard let link = PairLink(url) else { return }
        #if os(iOS)
            let phone = true
        #else
            let phone = false
        #endif
        guard PairLink.admits(
            connected: connStatus == .connected,
            addAccountOpen: addAccountSheetOpen,
            phone: phone)
        else { return }
        // DROPPED, not parked, while an account change is in flight: a link
        // held through it would be applied over whatever came back, and by
        // then its code is a stale claim from a browser tab the person has
        // forgotten. Pairing from a link is a click away again afterwards.
        guard !accountActionsBlocked else { return }
        pairLink = link
        if connStatus == .connected { addAccountSheetOpen = true }
    }

    /// Keep a saved identity behind the loading gate during a temporary outage.
    /// Each probe has bounded short retries; longer outages retry every 30s.
    /// Credentials or account changes cancel this task and invalidate its result.
    ///
    /// `behindBoard`: the launch is ALREADY ON ITS BOARD (`restorePersistedBoard`),
    /// so this runs beside the first pull instead of in front of it, and moves
    /// nothing on screen unless the daemon refuses outright (credentials gone,
    /// a daemon too old for this build). That ends at the same error gate the
    /// wait would have. A daemon that is only not answering is the poller's to
    /// report (its failing first pull is the down pane, as for any launch
    /// whose daemon drops), and the probe keeps trying behind it, so a held
    /// notification tap and the phone's push registration still follow the
    /// first answer.
    private func recoverSavedConnection(
        _ stored: ConnectionSettings, accountId: UUID, behindBoard: Bool = false
    ) {
        capabilityRecoveryTask?.cancel()
        let recoveryEpoch = epoch
        if !behindBoard {
            connStatus = .loading
            restoringSavedConnection = true
        }
        capabilityRecoveryTask = Task { [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                guard isCurrent(recoveryEpoch), AccountManager.shared.activeId == accountId else { return }
                do {
                    try await APIClient.shared.probe(baseURL: stored.serverURL, token: stored.apiToken)
                    guard !Task.isCancelled, isCurrent(recoveryEpoch),
                        AccountManager.shared.activeId == accountId else { return }
                    if behindBoard {
                        noteConnectionVerified()
                    } else {
                        connError = nil
                        connStatus = .connected
                    }
                    // Behind a board the shell came up before this answer, and
                    // a link parked under the gate was handled then
                    // (`loadSettings`): nothing to raise or drop here.
                    #if os(iOS)
                        await PushRegistration.shared.registerAndSync()
                        guard !Task.isCancelled, isCurrent(recoveryEpoch) else { return }
                        // Parked while loading, so nobody raised the sheet for
                        // it, and the phone only takes a link into a sheet the
                        // person opened themselves (`PairLink.admits`).
                        if !behindBoard { pairLink = nil }
                    #else
                        if !behindBoard, pairLink != nil { addAccountSheetOpen = true }
                    #endif
                    return
                } catch {
                    guard !Task.isCancelled, isCurrent(recoveryEpoch),
                        AccountManager.shared.activeId == accountId else { return }
                    guard CapabilityProbeRetry.isTransient(error) else {
                        // Refused credentials take the board kept under them,
                        // painted or not (a locked phone cannot read it). Any
                        // other refusal (a captive portal's page, a build the
                        // daemon is too old for) leaves it for a launch that
                        // can reach the daemon.
                        if (error as? APIError)?.kind == .unauthorized {
                            discardBoardFile(accountId)
                            Task { await CalendarDisk.shared.forget(accountId) }
                        }
                        if behindBoard { refuseBoardFromDisk() }
                        connStatus = .error
                        connError = Self.connectErrorText(error)
                        return
                    }
                    // The text is the loading gate's; behind a board, the
                    // poller's failing pulls say it.
                    if !behindBoard {
                        connError = "Server temporarily unavailable. Retrying automatically…"
                    }
                    do { try await Task.sleep(for: .seconds(30)) }
                    catch { return }
                }
            }
        }
    }

    /// The daemon refused outright the launch that painted its board from
    /// disk. The epoch moves, so a pull still in flight lands nowhere
    /// and nothing writes the board again. What is on screen stays in memory
    /// behind the error gate (a reply being typed with it), as the Connect
    /// gate's world does, until a reconnect wipes it.
    private func refuseBoardFromDisk() {
        unverifiedEpoch = nil
        epoch &+= 1
    }

    /// A launch from disk has been answered (its probe, or a pull): taps may
    /// be delivered, and what the shell asked once at `.connected`, before
    /// anything had answered, is asked again where a failure would otherwise
    /// stand for the session (the tracking config, which nothing retries).
    private func noteConnectionVerified() {
        guard unverifiedEpoch != nil else { return }
        unverifiedEpoch = nil
        Notifier.shared.connectionBecameReady()
        if tracking == nil { Task { await refreshTrackingConfig() } }
    }

    /// Ask the saved daemon again now instead of sitting out the loop's sleep:
    /// the person has just started it and should not wait half a minute to be
    /// believed.
    func retrySavedConnectionNow() {
        guard restoringSavedConnection, !accountActionsBlocked, !switching,
            let settings, let id = AccountManager.shared.activeId
        else { return }
        connError = nil
        recoverSavedConnection(settings, accountId: id)
    }

    /// Stop waiting on the saved daemon and hand the window to the Connect
    /// gate. Nothing is forgotten: the account stays in the index with its
    /// credentials, and the gate's "Try saved connection again" comes straight
    /// back here.
    func stopWaitingForSavedConnection() {
        guard restoringSavedConnection, !accountActionsBlocked, !switching else { return }
        capabilityRecoveryTask?.cancel()
        capabilityRecoveryTask = nil
        connError = nil
        leftSavedConnectionWait = true
        connStatus = .disconnected
    }

    /// Verify candidate credentials and triage capabilities, then persist and connect.
    ///
    /// THE GATE'S path, and the first account's: it moves `connStatus`, which
    /// is what swaps the Connect screen for the shell. Adding an account to an
    /// install that already has one is `addAccount` instead, precisely because
    /// this one moves connection state the live account is standing on.
    ///
    /// `label` is the optional name from the form. Empty means "unchanged"
    /// rather than "unnamed", so a re-connect through the gate cannot silently
    /// erase a label the account already had.
    @discardableResult
    func connect(serverURL: String, apiToken: String, label: String = "") async -> Bool {
        guard !accountActionsBlocked, !switching else { return false }
        switching = true
        defer { switching = false }
        capabilityRecoveryTask?.cancel()
        capabilityRecoveryTask = nil
        connStatus = .connecting
        connError = nil
        // Probe with a throwaway config so a bad token never gets persisted.
        await APIClient.shared.configure(baseURL: serverURL, token: apiToken)
        do {
            try await APIClient.shared.requireAgentTriage()
            // Re-connecting to a daemon the index ALREADY NAMES — active or
            // not — keeps that record's id (and so its keychain slots and
            // scoped cursors); a daemon no record names mints a fresh one. The
            // gate can be reached with healthy accounts still in the index (a
            // failed switch, a denied keychain read at boot), and the two
            // wrong answers there are each other's mirror: reusing the active
            // record for a DIFFERENT daemon overwrites a survivor's
            // credentials, while minting a fresh record for a KNOWN daemon
            // splits one mailbox across two accounts and doubles its streams
            // and banners. The index entry is written only after the
            // credentials are in the keychain.
            let host = AccountRecord.host(from: serverURL)
            var account =
                AccountIndex.load().accounts.first { $0.displayHost == host } ?? AccountRecord()
            let named = label.trimmingCharacters(in: .whitespacesAndNewlines)
            if !named.isEmpty { account.label = named }
            // The name the switcher shows when there is no label, learned here
            // rather than from a keychain read later on the main actor.
            account.displayHost = AccountRecord.host(from: serverURL)
            let fresh = ConnectionSettings(serverURL: serverURL, apiToken: apiToken)
            try await SettingsStore.saveAsync(fresh, accountId: account.id).get()
            AccountIndex.upsert(account)
            // Into the observable mirror too: everything account-scoped — the
            // 2FA seen-set, the decisions ledger, the notification feeds —
            // derives its storage key (or its very existence) from
            // `AccountManager`, and a first connection is the moment that goes
            // from nothing to something.
            AccountManager.shared.reload()
            // The decisions ledger's key derives from the id that just went
            // live. Without this, a session that removed its last account and
            // connected a different one would still hold the OLD account's
            // in-memory verdicts — served under the new daemon's colliding
            // message ids, and persisted wholesale under the new key on the
            // first `set`.
            AuthDecisions.shared.reload()
            ThreadStyleLedger.shared.reload()
            RecentSearchStore.shared.reload()
            // The gate's world is empty, but not necessarily CLEAN: view state
            // parked before the disconnect — a thread id planted by a stale
            // notification tap, answers still in flight from the previous
            // daemon — would otherwise survive into this account. Same fence a
            // switch uses; costs nothing when there was truly nothing on
            // screen.
            epoch &+= 1
            wipeAccountState()
            wipeAccountCaches()
            // A record reused by host match can be an account that was parked
            // in the background, and these credentials may open a different
            // mailbox behind the same host. The one on disk too.
            boardSnapshots[account.id] = nil
            discardBoardFile(account.id)
            FrameHeightStore.shared.forget(account: account.id)
            let replaced = account.id
            Task { await CalendarDisk.shared.forget(replaced) }
            // Arriving from the Connect gate the feeds are down, so this is a
            // no-op and the `.connected` transition below is what raises them
            // — the same route the single-account build took. Kept as a
            // defensive pair with `revalidate`'s, which handles the
            // stay-connected recredential arrival.
            AccountManager.shared.restartFeeds(account.id, with: fresh)
            settings = fresh
            connStatus = .connected
            connError = nil
            #if os(iOS)
                await PushRegistration.shared.registerAndSync()
            #endif
            Analytics.capture("connect_succeeded")
            identifyPerson()
            // Fresh connection = fresh sync history; a stale lastRefresh from a
            // prior session must not make a failing daemon look recently synced.
            lastRefresh = nil
            refreshError = nil
            return true
        } catch {
            connStatus = .error
            connError = Self.connectErrorText(error)
            return false
        }
    }

    /// Settings-screen re-validate: test a candidate and, on success, persist and
    /// swap the live client — WITHOUT dropping connStatus out of "connected" on
    /// failure, so Settings stays mounted instead of bouncing to the Connect gate.
    func revalidate(serverURL: String, apiToken: String) async -> (ok: Bool, error: String?) {
        guard !accountActionsBlocked else { return (false, Self.fixtureAccountError) }
        guard !switching else { return (false, "An account change is already in progress.") }
        switching = true
        defer { switching = false }
        let prev = settings
        do {
            try await APIClient.shared.probe(baseURL: serverURL, token: apiToken)
            // Same account, new credentials — `activeOrNew` returns the live
            // record here, so this overwrites its slots rather than adding one.
            var account = AccountIndex.activeOrNew()
            // A moved daemon renames its own row in the switcher; the label,
            // if the human gave one, is untouched.
            account.displayHost = AccountRecord.host(from: serverURL)
            let fresh = ConnectionSettings(serverURL: serverURL, apiToken: apiToken)
            try await SettingsStore.saveAsync(fresh, accountId: account.id).get()
            AccountIndex.upsert(account)
            AccountManager.shared.reload()
            // The feed is still connected with the OLD token, and nothing else
            // will ever tell it otherwise: re-validating deliberately never
            // leaves `.connected`, so the transition that starts the streams
            // does not fire. A rotated token means the old connection is one
            // 401 away from a silent backoff loop, so it is replaced outright
            // — streams take their credentials at construction and keep them.
            await APIClient.shared.configure(baseURL: serverURL, token: apiToken)
            AccountManager.shared.restartFeeds(account.id, with: fresh)
            settings = fresh
            // The board on screen was read from the server these credentials
            // replaced, which may not be the one they open. Nothing vouches
            // for it until that one has been asked: the stamps go, so the
            // next tick asks for everything, and a switch away before then
            // keeps no snapshot (`keepBoardSnapshot` reads `statsEpoch`).
            statsEpoch = nil
            zonesLoadedAt = nil
            mailLoadedAt = [:]
            // And the board on disk. Nothing is written again under this
            // epoch: a pull asked of the replaced server may still land in it,
            // and the next launch must not paint that server's rows under
            // these credentials.
            boardWritesRefusedEpoch = epoch
            discardBoardFile(account.id)
            FrameHeightStore.shared.forget(account: account.id)
            let revalidated = account.id
            Task { await CalendarDisk.shared.forget(revalidated) }
            #if os(iOS)
                await PushRegistration.shared.registerAndSync()
            #endif
            Notifier.shared.connectionBecameReady()
            return (true, nil)
        } catch {
            // Restore the prior working client — a fat-fingered token must not
            // leave the app pointed at a bad config. With no prior config to
            // restore, the rejected candidate must not linger either.
            if let prev {
                await APIClient.shared.configure(baseURL: prev.serverURL, token: prev.apiToken)
            } else {
                await APIClient.shared.deconfigure()
            }
            return (false, Self.connectErrorText(error))
        }
    }

    private static func connectErrorText(_ error: Error) -> String {
        guard let api = error as? APIError else { return "connection failed" }
        switch api.kind {
        case .unauthorized: return "token rejected (401)"
        case .network: return "cannot reach that server URL"
        default: return api.message
        }
    }

    /// Whether a daemon is already one of this install's accounts. Matched on
    /// host:port, which the index already holds and no keychain read is needed
    /// for; a different spelling of the same host (localhost for 127.0.0.1)
    /// slips through, and the cost of that is the duplicate the human
    /// explicitly asked for. Public because `ConnectView` must ask BEFORE
    /// claiming a pairing code — a claim mints a device token on the daemon,
    /// and a duplicate refused after that has already spent the code and
    /// orphaned the token server-side.
    func isKnownDaemon(_ serverURL: String) -> Bool {
        let host = AccountRecord.host(from: serverURL)
        return AccountManager.shared.accounts.contains { $0.displayHost == host }
    }

    /// Add a SECOND (or fifth) account and switch to it.
    ///
    /// The whole difference from `connect` is what it refuses to touch. This
    /// install already has an identity and a mailbox on screen, so:
    ///
    /// - the probe goes through `APIClient.probe`, which builds its one request
    ///   from the candidate credentials instead of installing them — the live
    ///   account's requests keep working while a stranger's token is tested,
    ///   and a bad one is never configured to find out that it is bad;
    /// - `connStatus` never moves, so the shell stays mounted (dropping to
    ///   `.connecting` would unmount the account already on screen and flash
    ///   the Connect gate under the sheet);
    /// - the credentials land under a NEW id via `AccountManager.add`, which
    ///   writes the keychain before the index entry that names it.
    ///
    /// Only then is the world torn down, and only through the ordinary switch —
    /// the one place that knows how to do it safely.
    func addAccount(serverURL: String, apiToken: String, label: String = "") async -> (
        ok: Bool, error: String?
    ) {
        guard !accountActionsBlocked else { return (false, Self.fixtureAccountError) }
        guard !switching else { return (false, "An account change is already in progress.") }
        switching = true
        defer { switching = false }
        // ONE DAEMON IS ONE MAILBOX, so the same daemon twice is not two
        // accounts — it is every banner delivered twice and one mailbox's state
        // split across two records.
        if isKnownDaemon(serverURL) {
            return (false, "that daemon is already one of your accounts")
        }
        do {
            try await APIClient.shared.probe(baseURL: serverURL, token: apiToken)
        } catch {
            return (false, Self.connectErrorText(error))
        }
        let fresh = ConnectionSettings(serverURL: serverURL, apiToken: apiToken)
        guard
            let record = await AccountManager.shared.add(
                label: label.trimmingCharacters(in: .whitespacesAndNewlines), settings: fresh)
        else {
            // The keychain refused the write and `add` rolled it back, so
            // nothing was recorded. Nothing to undo here either.
            return (false, "could not save to the keychain")
        }
        Analytics.capture("account_added")
        identifyPerson()
        // Hold the same gate from the credential probe through persistence
        // and switching, so nothing can intercept any part of the add.
        // The switch can fail its credential read-back (a denied access panel
        // on the slots just written). The account exists either way — saying
        // `ok` while the old mailbox stays on screen would read as a failed
        // add, and the retry it invites is refused as a duplicate daemon.
        guard await performSwitch(to: record) else {
            return (false, "account added, but the keychain refused its credentials; pick it from the account switcher to retry")
        }
        #if os(iOS)
            await PushRegistration.shared.registerAndSync()
        #endif
        return (true, nil)
    }

    /// Forget one account, wherever it sits in the list. THE removal path:
    /// Settings' per-row Remove lands here (its last-account form wears the
    /// old Disconnect label), so there is exactly one answer to "what happens
    /// to the world when the mailbox on screen goes away".
    ///
    /// `AccountManager.remove` does the durable half — feed down, keychain
    /// cleared (off the main actor, a refusal parked for the boot sweep), index
    /// entry dropped, scoped defaults forgotten, the legacy slots sealed if
    /// that was the last account. What is left for here is the world on screen,
    /// and only when it belonged to the account that just went.
    func removeAccount(_ id: UUID) async {
        guard !accountActionsBlocked else { return }
        // Wait out any in-flight switch, then hold its gate for the duration:
        // a remove landing inside a switch's suspension windows would leave
        // the index, the configured client and the per-account-keyed stores
        // disagreeing about which mailbox is live — and `wasActive` below is
        // only meaningful once whoever was switching has finished doing so.
        // Both loop and flag are MainActor, so the handoff has no gap.
        while switching { try? await Task.sleep(for: .milliseconds(50)) }
        guard !accountActionsBlocked else { return }
        switching = true
        defer { switching = false }

        let wasActive = AccountManager.shared.activeId == id
        // Fenced BEFORE the index moves: `remove` suspends on keychain awaits,
        // and a sitrep pull of this daemon already in flight would otherwise
        // land under the old epoch — attributed to the survivor the index
        // reassigns `active` to, and folded into the survivor's persisted
        // seen-set (message ids are per-daemon ints; they collide).
        if wasActive {
            epoch &+= 1
            SitrepPoller.shared.stop()
        }
        await AccountManager.shared.remove(id)
        boardSnapshots[id] = nil
        // The saved headline can name people and plans from the mailbox just
        // forgotten. It is keyed by board, so it could never show for another
        // account, but it should not outlive the account either.
        UserDefaults.standard.removeObject(forKey: Self.headlineDefaultsKey)
        guard wasActive else { return }

        guard let survivor = AccountManager.shared.active else {
            // THE LAST ACCOUNT. Back to the Connect gate: every id on screen
            // addressed a daemon this install no longer holds credentials for.
            // The drafts are deliberately NOT flushed on the way out (a switch
            // flushes them) — there is nowhere to send them that we are still
            // entitled to talk to, and the human asked for this account to be
            // forgotten.
            await tearDownToGate(error: nil)
            return
        }
        // Straight to `performSwitch` rather than `AccountManager.switchTo`:
        // that one refuses a switch to the account already marked active, and
        // `AccountIndex.remove` handed `active` to this survivor on its way
        // through. The index has moved on; the store has not, and this is what
        // moves it — under the gate this function already holds. Drafts are
        // NOT flushed on the way: the daemon they would go to is the one the
        // human just asked to forget, same principle as the last-account
        // branch above.
        await performSwitch(to: survivor, flushDrafts: false, currentWorldGone: true)
    }

    // MARK: - switching accounts

    /// Make `record` the live account: tear the current world down, point the
    /// client at the new daemon, and start it up again. `connStatus` never
    /// leaves `.connected`, so the Connect gate does not flash in between —
    /// the restart at the end is by hand for exactly that reason.
    ///
    /// Entered through `AccountManager.switchTo`, which owns the "is this a
    /// real, different account" half of step (1).
    ///
    /// ACCEPTED RESIDUAL RACE, documented rather than fixed: an action Task
    /// created in the same instant as this one can read `APIClient.shared`'s
    /// config AFTER step (8) has replaced it, and send account A's verb to
    /// account B's daemon. The window is the sub-millisecond gap between that
    /// Task being created and its request being built. Closing it needs a
    /// generation INSIDE APIClient that every request checks and every caller
    /// passes — identified, not built, because it touches all 25 call sites.
    func switchAccount(to record: AccountRecord) async {
        guard !accountActionsBlocked else { return }
        // THE CHECK COMES BEFORE THE GATE, and outside it. It reads a keychain
        // slot and asks the target daemon one question; nothing of the live
        // world is touched. Held inside the gate (the old shape) a target that
        // was not answering locked every other switch out for the length of
        // its retries, in silence: one press on a dead account and the app
        // would not move to a healthy one. So the newest pick wins instead,
        // and an older one that comes back late is dropped unreported.
        switchIntent &+= 1
        let intent = switchIntent
        switchingTo = record.id
        defer { if switchIntent == intent { switchingTo = nil } }
        // One attempt, no retries: somebody is watching this one, and "that
        // server is not answering" now beats the same sentence in 15 seconds.
        //
        // NOT ASKED AT ALL of an account whose board is being held: it has
        // answered with these credentials this session, and there is a board
        // to stand on if it no longer does. The round trip was the whole of
        // what a switch back still waited for. A daemon that has died since
        // shows its last board under the stale banner, which the first pull
        // raises, and every other account is one press away as before.
        let checked = await switchPreflight(
            record, retrying: false, probing: boardSnapshots[record.id] == nil)
        guard switchIntent == intent else { return }
        guard case .ready(let next) = checked else {
            if case .refused(let why) = checked { reportSwitchFailure(why, for: record) }
            return
        }
        // (1) Never two at once — the awaits below are windows a second
        //     switch could start in. One already inside its teardown is
        //     WAITED OUT, not lost to: the rail is showing this account's
        //     mark, and a press dropped here put the old one back with
        //     nothing said. The newest pick still wins, and the world may
        //     have moved while the target was being asked, so everything is
        //     read again on the far side.
        while switching { try? await Task.sleep(for: .milliseconds(25)) }
        guard switchIntent == intent, !accountActionsBlocked,
            record.id != AccountManager.shared.activeId,
            AccountManager.shared.accounts.contains(where: { $0.id == record.id })
        else { return }
        switching = true
        // (10) Whichever path we leave by.
        defer { switching = false }
        await performSwitch(to: record, preflight: next)
    }

    private enum SwitchPreflight {
        case ready(ConnectionSettings)
        case refused(String)
    }

    /// Step (2): the target's credentials, and proof its daemon will take
    /// them, BEFORE anything is torn down — the steps that can fail. Failing
    /// here, with the old world untouched, keeps the human on a working
    /// mailbox; checking after the wipe (the old shape) turned a single denied
    /// access panel into a logout, and the gate it landed on then endangered
    /// the surviving accounts' credentials. A keychain read can raise the
    /// access panel, but nothing has been suspended or wiped yet, so blocking
    /// here is safe. The probe is built from the candidate credentials, so the
    /// live client is not touched either.
    private func switchPreflight(
        _ record: AccountRecord, retrying: Bool, probing: Bool = true
    ) async -> SwitchPreflight {
        let loaded = await AccountManager.shared.credentialLoad(for: record.id)
        guard case .ok(let next) = loaded else {
            if case .unreadable = loaded {
                return .refused("the keychain refused to unlock \(record.displayName)")
            }
            return .refused("no credentials for \(record.displayName) in the keychain")
        }
        guard probing else { return .ready(next) }
        do {
            try await APIClient.shared.probe(
                baseURL: next.serverURL, token: next.apiToken, retrying: retrying)
        } catch {
            return .refused(Self.connectErrorText(error))
        }
        return .ready(next)
    }

    /// Steps (2)–(9), gate already held. Split out so `removeAccount` — which
    /// holds the same gate across its OWN awaits — can run a switch without
    /// tripping the guard that exists to keep everyone else out.
    ///
    /// `flushDrafts: false` is the removal path's variant: the daemon those
    /// drafts would go to is the one being forgotten. The settle still runs —
    /// a debounced save already in flight must finish before the client is
    /// reconfigured, whoever it was addressed to.
    ///
    /// `preflight` is step (2) already done by the caller (`switchAccount`,
    /// which does it outside the gate).
    @discardableResult
    private func performSwitch(
        to record: AccountRecord, flushDrafts: Bool = true, currentWorldGone: Bool = false,
        preflight: ConnectionSettings? = nil
    ) async -> Bool {
        let next: ConnectionSettings
        if let preflight {
            next = preflight
        } else {
            switch await switchPreflight(record, retrying: true) {
            case .ready(let checked):
                next = checked
            case .refused(let why):
                if currentWorldGone {
                    // The removal path: the account on screen is already gone
                    // from the index, so there is no working world to stay in
                    // — the Connect gate is the honest answer.
                    await tearDownToGate(error: why)
                } else {
                    // The world on screen is intact and the credentials
                    // backing it were never touched. Stay — and say so.
                    reportSwitchFailure(why, for: record)
                }
                return false
            }
        }

        // (2b) The board on screen, kept for the way back. BEFORE the fence:
        //     whether it is this account's and whole is read off the epoch
        //     the next line retires. Not on the removal path, where the index
        //     has already handed `activeId` to the survivor and the world on
        //     screen is the forgotten account's.
        if !currentWorldGone {
            keepBoardSnapshot()
            saveBoardNow()
        }

        // (3) From here, every answer still in flight belongs to the old
        //     account and every writer that captured the old epoch is inert.
        epoch &+= 1

        // (4) Stop polling this daemon. The EventStreams are deliberately left
        //     alone — every account holds one, live or not, so that mail
        //     arriving anywhere still raises a banner. Switching changes which
        //     mailbox is on screen, not which ones are worth hearing from.
        SitrepPoller.shared.stop()

        // (5) Both composers' last save — and then WAIT for it. Reconfiguring
        //     the client underneath an in-flight draft PUT would post what the
        //     human typed into account A into account B's drafts, under an id
        //     that means something else there.
        if flushDrafts {
            DraftSaver.shared.flush(.compose, compose)
            DraftSaver.shared.flush(.inlineReply, inlineReply)
        }
        // A mail in its send hold is not sent from the account it was not
        // written in, and not dropped either: it becomes a draft there.
        parkHeldSends(save: flushDrafts)
        await DraftSaver.shared.settle()

        // (6) The ⌘K session: its transcript cites the old account's mail and
        //     its parked tool calls would act on it.
        assistant.clear()

        // (7) The world itself, in one synchronous pass so no frame is
        //     ever painted showing a mix of two mailboxes. Fenced AGAIN
        //     first, as `enterFixtures` does: a refresh that started while
        //     the drafts settled carries step (3)'s epoch and the old
        //     daemon's rows, and would write them into the world wiped here.
        epoch &+= 1
        SitrepPoller.shared.stop()
        wipeAccountState(landingOn: Self.pageKeptAcrossSwitch(activeView))
        wipeAccountCaches()

        // (8) The new credentials go live.
        settings = next
        connError = nil
        await APIClient.shared.configure(baseURL: next.serverURL, token: next.apiToken)
        AccountManager.shared.markActive(record.id)
        // AFTER `markActive`, deliberately: each ledger's UserDefaults key is
        // derived from the live account id, so reloading them with the
        // singletons in step (7) would have re-read the account that just went
        // away.
        AuthDecisions.shared.reload()
        ThreadStyleLedger.shared.reload()
        RecentSearchStore.shared.reload()
        // The board this account had when it was last on screen, if it has
        // been on screen this session. AFTER `markActive` and the client, not
        // with the wipe in step (7): a row painted while the client still
        // pointed at the old daemon could be clicked into it. Everything in
        // (9) then runs as it would over an empty world, and replaces this.
        restoreBoardSnapshot(for: record.id)

        // (9) Restart by hand. `connStatus` was never dropped, so the
        //     `.connected` transition that normally does this never fires.
        //     Fire-and-forget: a slow daemon must not hold the switch (and so
        //     the next ⌘number) open for a round trip. Both are epoch-guarded.
        SitrepPoller.shared.start()
        Task { await refreshMail(.inbox) }
        Task { await refreshTrackingConfig() }
        Notifier.shared.connectionBecameReady()
        // A switch made from the wait for a daemon that is not answering: the
        // shell was never mounted, so this is also what mounts it. The starts
        // above repeat on that transition, and all of them are idempotent.
        if connStatus != .connected { connStatus = .connected }
        return true
    }

    /// A refused switch leaves the world it started from, and says why where
    /// that world can show it. Toasts are the shell's; from the wait for a
    /// saved daemon there is no shell, and the loading gate reads `connError`.
    /// A toast, not `connError`, from the shell: that field renders only on
    /// the gates, so writing it there would show nothing now and then greet
    /// the next Add Account sheet with a stale failure.
    private func reportSwitchFailure(_ why: String, for record: AccountRecord) {
        if connStatus == .connected {
            pushToast("can’t switch to \(record.displayName): \(why)", .error)
        } else {
            connError = "\(record.displayName): \(why)"
        }
    }

    /// Tear the world down to the Connect gate: the shared tail of "the
    /// account on screen is gone" — the last account removed, or a removal's
    /// survivor unreadable. ONE copy, because a teardown-ordering fix that
    /// lands in one path and misses another leaves that path presenting a
    /// forgotten daemon's token.
    ///
    /// The ledger reloads empty the in-memory verdicts (no live account, no
    /// key), so a later `connect` cannot inherit — and then persist — a gone
    /// account's decisions under a new daemon's colliding message ids. The
    /// deconfigure is the same principle for the client itself.
    private func tearDownToGate(error: String?) async {
        epoch &+= 1
        SitrepPoller.shared.stop()
        assistant.clear()
        wipeAccountState()
        wipeAccountCaches()
        AuthDecisions.shared.reload()
        ThreadStyleLedger.shared.reload()
        RecentSearchStore.shared.reload()
        await APIClient.shared.deconfigure()
        settings = nil
        connStatus = .disconnected
        connError = error
    }

    // MARK: - board snapshots

    /// One account's board as it stood when the app last switched away from
    /// it: what a switch back paints at once instead of an empty board and
    /// three round trips. The READ MODEL only, and only the part a first
    /// frame is made of. Nothing that acts (undos, held sends, overlays) and
    /// nothing a daemon is still answering for: `resolvedIds` in particular
    /// stays behind, because a resolve still on the wire at the switch can
    /// fail, and the id would then hide its mail for the rest of the session.
    ///
    /// The id collision the wipe exists for does not apply: a snapshot is
    /// held under its account's id, swapped in whole, and only ever shown
    /// while the client points at the daemon that minted its ids.
    private struct BoardSnapshot {
        var sitrep: SitrepData
        var stats: StoreStats?
        var fyeHeadline: String?
        var fyeHeadlineKey: String?
        var zones: SitrepZoneCache
        var mailPages: [MailMode: [AttentionUpdate]]
        var mailKeyset: Bool
        var selectedId: Int?
        var lastRefresh: Date?
    }

    /// IN MEMORY ONLY, and only for accounts that are NOT live: an entry is
    /// made when its account leaves the screen and taken when it comes back.
    /// The copy a LAUNCH paints is kept on disk by `BoardFile`, under its own
    /// rules (no sealed list, no auth mail, no money cards, no stats; see that
    /// file and `boardToKeep`), and is painted by the same `paintBoard`.
    @ObservationIgnored private var boardSnapshots: [UUID: BoardSnapshot] = [:]

    /// True from a snapshot's repaint until the daemon has answered for the
    /// bands and the zones: the board on screen is what this mailbox held the
    /// last time it was looked at, not what it holds now. Read by whatever
    /// must not treat the difference between the two as something the person
    /// just did (the clear chime).
    private(set) var boardIsSnapshot = false

    /// The poller's word that the board is the daemon's again: asked after
    /// a pull and the zone pass behind it. NOT after a pass that missed a
    /// zone, which kept the snapshot's rows for it; the retry confirms.
    func noteBoardConfirmed() {
        guard boardIsSnapshot, zonesLoadedAt != nil, !zoneRetryOwed else { return }
        boardIsSnapshot = false
    }

    /// Keep the live board under the live account's id, when there is one
    /// worth keeping (`liveSnapshot`).
    private func keepBoardSnapshot() {
        guard let id = AccountManager.shared.activeId, let board = liveSnapshot() else { return }
        boardSnapshots[id] = board
    }

    /// The live board as a snapshot, when there is one worth keeping: a board
    /// the daemon has answered for under THIS epoch. Stats stamped with an
    /// older epoch, or never, mean the rows on screen cannot be vouched for (a
    /// switch made before the first pull landed, the wait for a daemon that is
    /// not answering), and a snapshot of that would repaint an empty or
    /// half-loaded board as if it were the mailbox. The one shape both copies
    /// are cut from: the in-memory one a switch back repaints, and the one on
    /// disk (`boardToKeep`).
    private func liveSnapshot() -> BoardSnapshot? {
        guard connStatus == .connected, !FixtureMode.isEnabled, statsEpoch == epoch,
            lastRefresh != nil
        else { return nil }
        var bands = sitrep
        // Sealed mail is login codes: minutes old is expired, and a stale
        // list would be handed to watchers that raise prompts. Asked for
        // again every time.
        bands.sealed = []
        return BoardSnapshot(
            sitrep: bands, stats: stats, fyeHeadline: fyeHeadline, fyeHeadlineKey: fyeHeadlineKey,
            zones: zones.value,
            // The head of each page: the scroll refills the rest, and the
            // head is what the refresh behind the repaint merges into. ONLY
            // from a daemon that pages by arrival. One that pages by cursor
            // gets its cursor from a head fetched over an EMPTY page, and a
            // repainted head would leave the scroll with nowhere to go.
            mailPages: mailKeyset ? Self.pageHeads(mailPages) : [:],
            mailKeyset: mailKeyset, selectedId: selectedId, lastRefresh: lastRefresh)
    }

    /// Paint `id`'s kept board over the world `wipeAccountState` just
    /// emptied. The TTL stamps stay nil, so every refresh a switch starts
    /// still asks; each lands through its usual compare-and-assign and the
    /// snapshot is simply the rows they compare against.
    private func restoreBoardSnapshot(for id: UUID) {
        guard let kept = boardSnapshots.removeValue(forKey: id) else { return }
        paintBoard(kept)
    }

    /// Paint `kept` over an emptied world: a switch back's snapshot, or the
    /// board from disk at launch (`restorePersistedBoard`).
    private func paintBoard(_ kept: BoardSnapshot) {
        sitrep = kept.sitrep
        stats = kept.stats
        // These ARE this account's stats, which is all the stamp claims. A
        // board from disk has none, and stays unstamped until a pull.
        if kept.stats != nil { statsEpoch = epoch }
        fyeHeadline = kept.fyeHeadline
        fyeHeadlineKey = kept.fyeHeadlineKey
        zones.replace(with: kept.zones)
        mailPages = kept.mailPages.mapValues { Loadable(value: $0, error: nil, isLoading: false) }
        mailKeyset = kept.mailKeyset
        selectedId = kept.selectedId
        // The account's own last sync, not the one just left: a daemon that
        // has since gone quiet shows this board as stale, which it is.
        lastRefresh = kept.lastRefresh
        boardIsSnapshot = true
        // What a zone pass does once its rows are in, done for these rows:
        // the caches were wiped with the last account, and the cards are on
        // screen a round trip before the pass that would refill them.
        preloadBoardHeroes()
        warmZoneThreads()
    }

    // MARK: - the board on disk

    /// When the board on disk is brought up to date besides a change: leaving
    /// the screen, and quitting. `waits`: the save returns only once the file
    /// is written, because the process may never run again (a quit; the phone
    /// suspended, then killed, in the background). `quits`: written even when
    /// nothing has changed since the last write, so the last answer's time on
    /// disk is the session's own.
    private struct BoardSaveMoment: Sendable {
        let name: Notification.Name
        let waits: Bool
        let quits: Bool
    }
    #if os(macOS)
        private static let boardSaveMoments = [
            BoardSaveMoment(name: Platform.didResignActiveNotification, waits: false, quits: false),
            BoardSaveMoment(name: Platform.willTerminateNotification, waits: true, quits: true),
        ]
    #else
        private static let boardSaveMoments = [
            BoardSaveMoment(name: Platform.didResignActiveNotification, waits: false, quits: false),
            BoardSaveMoment(
                name: UIApplication.didEnterBackgroundNotification, waits: true, quits: false),
            BoardSaveMoment(name: Platform.willTerminateNotification, waits: true, quits: true),
        ]
    #endif

    /// How long after a change to the board it is written: changes come in
    /// runs (a pull, then the zone pass behind it, then a page), and the board
    /// is written once for the run.
    private static let boardSaveDelay: Duration = .seconds(20)
    @ObservationIgnored private var boardSaveTask: Task<Void, Never>?
    /// True only while `restorePersistedBoard` assigns the board from disk.
    @ObservationIgnored private var paintingFromDisk = false
    /// An epoch under which nothing is written to disk (`revalidate`).
    /// Also obeyed by the calendar's saved feed (`refreshCalendar`).
    @ObservationIgnored var boardWritesRefusedEpoch: Int?
    /// Something on the board changed since the last write.
    @ObservationIgnored private var boardDirty = true
    /// The last write: whose, under which epoch, and what.
    @ObservationIgnored private var boardWritten: (account: UUID, epoch: Int, board: PersistedBoard)?

    /// The epoch a pull last landed under. `boardToKeep` writes nothing until
    /// it is the live one, so the file is never a board nobody has vouched
    /// for this session (one repainted from disk or a switch) under a fresh
    /// date.
    @ObservationIgnored private var bandsAnsweredEpoch: Int?

    private func noteBoardAnswered(epoch e: Int) {
        guard e == epoch, bandsAnsweredEpoch != e else { return }
        bandsAnsweredEpoch = e
        // An answer can leave the board exactly as it was (the first pull
        // finding the board from disk unchanged), which assigns nothing and so
        // schedules nothing; the file is due all the same.
        scheduleBoardSave()
    }

    /// The poller's word that a pull landed under epoch `e`: the bands are the
    /// daemon's. The fence carried from disk gives way to the live sealed
    /// list, and the launch interval ends if a board from disk did not end it.
    ///
    /// A pull that landed is the daemon answering these credentials on its v2
    /// routes, so a launch from disk still waiting on its probe (which may be
    /// sleeping out a transient failure) is waiting no longer.
    func notePullLanded(epoch e: Int) {
        guard isCurrent(e) else { return }
        if !restoredAuthThreads.isEmpty {
            restoredAuthThreads = []
            noteAuthThreads()
        }
        if unverifiedEpoch == e { noteConnectionVerified() }
        noteBoardAnswered(epoch: e)
        LaunchSignpost.end(FixtureMode.isEnabled ? .skipped : .network)
    }

    /// What a save would do with the live board (`PersistedBoard.keeping`):
    /// `liveSnapshot`'s rules, nothing under an epoch whose writes are refused,
    /// and only what the daemon has answered for this session
    /// (`bandsAnsweredEpoch`, and a page only once it has loaded,
    /// `mailLoadedAt`, so a page painted from disk and not asked again is
    /// never restamped).
    private func boardToKeep() -> PersistedBoard.Keeping {
        guard boardWritesRefusedEpoch != epoch, let board = liveSnapshot(),
            let lastRefresh = board.lastRefresh
        else { return .nothing }
        let answered = LiveBoard.Answered(
            bands: bandsAnsweredEpoch == epoch, pages: Set(mailLoadedAt.keys.map(\.rawValue)))
        var pages: [String: [AttentionUpdate]] = [:]
        for (mode, rows) in board.mailPages { pages[mode.rawValue] = rows }
        return PersistedBoard.keeping(
            LiveBoard(
                standing: board.sitrep.standing, totalCount: board.sitrep.totalCount,
                fyeHeadline: board.fyeHeadline, fyeHeadlineKey: board.fyeHeadlineKey,
                headlineKey: headlineKey, calendar: board.zones.calendar,
                shipments: board.zones.shipments, mailPages: pages, mailKeyset: board.mailKeyset,
                authThreads: authThreadIds, lastRefresh: lastRefresh, answered: answered))
    }

    /// Write the board to disk a little after it changes. One write for a run
    /// of changes; what is written is the board when the delay is up.
    private func scheduleBoardSave() {
        boardDirty = true
        guard boardSaveTask == nil, connStatus == .connected, !FixtureMode.isEnabled else { return }
        boardSaveTask = Task { [weak self] in
            try? await Task.sleep(for: Self.boardSaveDelay)
            guard let self, !Task.isCancelled else { return }
            self.boardSaveTask = nil
            self.saveBoardNow()
        }
    }

    /// Write the live account's board to disk now: the app leaving the
    /// screen or quitting, a switch away, a scheduled save coming due.
    /// Nothing when there is no board worth keeping (`boardToKeep`), and
    /// nothing when the file already holds this board from this session
    /// (`needsWrite`), unless `evenIfUnchanged`.
    func saveBoardNow(waitingForDisk: Bool = false, evenIfUnchanged: Bool = false) {
        boardSaveTask?.cancel()
        boardSaveTask = nil
        // Whatever this call does or skips, a write queued before it (the
        // resign just ahead of the phone's backgrounding) is on disk before a
        // waiting caller goes on.
        defer { if waitingForDisk { BoardFile.flush() } }
        guard let id = AccountManager.shared.activeId else { return }
        // What the file holds, as far as this session knows: its last write
        // for this account and epoch, if that write landed.
        var last: PersistedBoard?
        if let written = boardWritten, written.account == id, written.epoch == epoch,
            !BoardFile.lastWriteFailed(account: id)
        {
            last = written.board
        }
        // Nothing has changed since, and the file's date is recent: nothing
        // is built (every focus change on the Mac comes through here).
        if !evenIfUnchanged, !boardDirty, let last, let lastRefresh,
            !PersistedBoard.restampDue(lastRefresh, written: last.lastRefresh)
        {
            return
        }
        switch boardToKeep() {
        case .nothing:
            return
        case .discard:
            boardDirty = false
            discardBoardFile(id)
        case .write(let board):
            boardDirty = false
            guard board.needsWrite(over: last, forced: evenIfUnchanged) else { return }
            boardWritten = (id, epoch, board)
            BoardFile.save(board, account: id)
        }
    }

    /// Delete `id`'s board on disk, and forget that it was written, so the
    /// next save writes a new one whatever has changed.
    private func discardBoardFile(_ id: UUID) {
        boardWritten = nil
        BoardFile.discard(account: id)
    }

    /// Paint the board from disk over the empty world of a launch, through
    /// the paint a switch back uses: the board is a snapshot until the daemon
    /// answers for it, and every refresh still asks. Its auth fence goes up
    /// with it, before any row can be hovered or warmed. ROWS ONLY: no stats
    /// and no `lastRefresh`, so the shell mounts as on a launch with no board
    /// for everything that waits on the daemon's first answer (the
    /// capabilities, the first sync's prompts, the down pane or stale banner,
    /// a switch's snapshot, which needs a board answered this session). The
    /// Reading zone and the money zones are not on disk, so they come with the
    /// first zone pass; the selection is not kept across a launch.
    private func restorePersistedBoard(_ board: PersistedBoard) {
        restoredAuthThreads = Set(board.authThreads)
        noteAuthThreads()
        var diskZones = SitrepZoneCache()
        diskZones.calendar = board.calendar
        diskZones.shipments = board.shipments
        var pages: [MailMode: [AttentionUpdate]] = [:]
        for (key, rows) in board.mailPages {
            if let mode = MailMode(rawValue: key) { pages[mode] = rows }
        }
        paintingFromDisk = true
        paintBoard(
            BoardSnapshot(
                sitrep: SitrepData(
                    standing: board.standing, new: [], open: [],
                    sealed: [], totalCount: board.totalCount),
                stats: nil, fyeHeadline: board.fyeHeadline, fyeHeadlineKey: board.fyeHeadlineKey,
                zones: diskZones, mailPages: pages, mailKeyset: board.mailKeyset, selectedId: nil,
                lastRefresh: nil))
        paintingFromDisk = false
        LaunchSignpost.end(.disk)
    }

    /// The page a switch leaves the person on: the one they were on. A switch
    /// changes which mailbox is on screen, not where in the app they are, and
    /// `MainView` names a page and nothing a daemon minted, so it is the one
    /// piece of navigation that can cross. The rest of the history cannot.
    ///
    /// Two exceptions. Process is reached only from the all-mail peer-review
    /// chip, so its way back is Emails. The calendar's gate is read off the
    /// new account's stats, which have not arrived when this runs, so
    /// `route(to:)` sends it to the sitrep like any closed gate.
    ///
    /// MAC ONLY. The phone maps a route onto a tab only for Sitrep and Mail,
    /// so keeping `activeView` there jumped Quick Look and Search to whichever
    /// of the two was visited last. It keeps its old answer, the sitrep,
    /// until its tabs are taught to stay put on their own.
    private static func pageKeptAcrossSwitch(_ view: MainView) -> MainView {
        #if os(iOS)
            return .sitrep
        #else
            return view == .process ? .emails : view
        #endif
    }

    /// Bumped by every wipe, in the same pass. The routed pages are keyed on
    /// it (RootView), so the page left standing over a new world is a new
    /// instance: its `@State` held the last daemon's rows and ids, and its
    /// `.task` load has to run again. NOT the live account id, which moves
    /// at a different moment (a removal hands it to the survivor before the
    /// survivor's preflight, and a switch only after the client is
    /// reconfigured), leaving the old page live over the new world between.
    private(set) var pageGeneration = 0

    /// Everything in this store that belongs to ONE account. Message ids are
    /// per-daemon SQLite ints, so "looks plausible" is precisely the failure
    /// mode being prevented here: A's row 412 is B's row 412.
    ///
    /// `landingOn` is the page the world comes back up on: the sitrep, except
    /// for a switch (see `pageKeptAcrossSwitch`). The history is reset either
    /// way, because every entry in it carries the old account's selection.
    private func wipeAccountState(landingOn page: MainView = .sitrep) {
        boardIsSnapshot = false
        // And the fence rebuilt without it now: the didSet below rebuilds it
        // only when the bands or the sealed list change, and an empty band
        // emptied again does not.
        restoredAuthThreads = []
        noteAuthThreads()
        // A reveal up over the old account would keep showing its body, and
        // one not yet mounted would reveal the same int on the NEW daemon.
        revealRequest = nil
        revealSource = nil
        // The unsubscribe tray's rows and buttons belong to the old account:
        // "Finish on their site" would tell the NEW daemon about a job it has
        // never heard of.
        UnsubscribeJobs.shared.reset()
        // A smart clear in flight was asked of the old daemon, and a receipt's
        // undo names a batch the new one never made.
        smartClearWorking = false
        smartClearReceipt = nil
        smartClearListOpen = false
        // And the ignored-unsubscribe notice, for the same reason.
        UnsubscribeViolations.shared.reset()
        sitrep = SitrepData()
        // Its own line since the stats left SitrepData. The old daemon's
        // stats over the new mailbox's empty bands read as "all clear": the
        // quiet board played for a mailbox that had not answered yet, and a
        // draft save took the old daemon's capabilities for the new one's.
        stats = nil
        fyeHeadline = nil
        fyeHeadlineKey = nil
        zones.replace(with: SitrepZoneCache())
        // AND THE TTL STAMP, which no longer rides inside the struct above.
        // Left standing, it vouches for the rows just thrown away: the new
        // account's first refreshes are skipped as "still fresh", and the
        // zone cards cannot ask for themselves because an empty rail is
        // never mounted.
        zonesLoadedAt = nil
        zoneRetryOwed = false
        // The calendar page's state is the account's: the feed, the item
        // open in the month, the connect card, the editor and the delete
        // question were about the last mailbox's calendars, and a write still
        // out answers into nothing. The mode and the month are the user's,
        // and stay.
        dropCalendarState()
        // The parked refresh tasks go too, not just their results: a joiner
        // arriving after the switch would otherwise wait on — and adopt the
        // silence of — a pass that has already been disowned.
        zoneRefresh = nil
        mailPages = [:]
        mailLoadedAt = [:]
        mailPagingState = [:]
        // The sent page is its own cache, and it is the old daemon's rows and
        // ids as much as the others. Kept, it repainted as the new account's
        // Sent, and a refresh inside its TTL (or one joining its fetch) never
        // replaced it.
        sentRows = .loading
        sentLoadedAt = nil
        sentRefresh = nil
        // A spam fetch asked of the old daemon says nothing about this one's.
        spamSyncRequestedAt = nil
        mailKeyset = false
        mailRefreshes = [:]
        reminderRows = .loading
        remindersLoadedAt = nil
        remindersRefresh = nil
        reminderFilter = false
        mailFilter = .all
        // The panel's state AND the conversation beside it. A lane left running
        // here would finish a search of the old mailbox into the new one's
        // panel, cards and all — it holds thread ids that mean something else
        // under the account that just went away.
        resetSearchLane()
        search = SearchSession()
        resolvedIds = []
        selectedId = nil
        // A fresh sync history: a `lastRefresh` from the old account must not
        // make a silent new one look recently synced (the rule `connect` keeps).
        lastRefresh = nil
        refreshError = nil
        // The thread viewer and both composers, by assignment rather than
        // through `closeThread`/`closeCompose`: those flush drafts, and step
        // (4) already saved and settled everything there was to save.
        threadId = nil
        // The ledger goes with it: what the other mailbox's reader had already
        // been told about says nothing about this one's.
        arrivals.reset(to: nil)
        threadQueue = []
        readingMail = false
        readingOrder = []
        pendingReplyMessageId = nil
        focusedMessageId = nil
        focusedMessageView = nil
        compose = nil
        inlineReply = nil
        sideView = .none
        // Overlays holding an account-scoped id. A rule editor left open
        // across a switch would save the old account's rule to the new
        // account's daemon.
        triageFix = nil
        remindTarget = nil
        schedulePicker = nil
        discardAsk = nil
        mailtoAsk = nil
        ruleEditor = nil
        // A draft's pattern names the old account's senders and rule ids.
        ruleDrafts = [:]
        groupEditor = nil
        // Every revert closure targets the old account's daemon.
        undos = []
        // The drafts cache is the old account's rows, and a delete still in
        // its hold is aimed at the old account's daemon: cancelled, not fired.
        // (The timer checks the epoch too; this is what keeps it from even
        // waking up to.)
        draftsRows = []
        for pending in pendingDraftDeletes.values { pending.timer.cancel() }
        pendingDraftDeletes = [:]
        // The generation and the in-flight fetch list are NOT reset: an old
        // account's fetch still in the air removes its own entry when it
        // lands, and a rewound counter would let it prune the new account's
        // tombstones early.
        draftTombstones.clear()
        // The outbox is the old daemon's rows, ids and all, and so is what has
        // already toasted. A pull-back or cancel still on the wire reports to
        // nobody (each checks the epoch).
        scheduledRows = []
        scheduledLoadedAt = nil
        scheduledRefresh = nil
        scheduledTombstones.clear()
        scheduledFailures.clear()
        scheduledFailuresAccount = nil
        scheduledGroupNames = ScheduledGroupNames()
        outboxKnownEmpty = false
        scheduledBusy = []
        scheduledPreviewId = nil
        authRings = []
        authQueue = []
        tracking = nil
        translateConfig = .unknown
        calendarReminderPref = .unknown
        ticketPreview = nil
        // `route(to:)` first: it is what turns a closed calendar into the
        // sitrep, and the history has to name the page actually shown.
        route(to: page)
        pageGeneration &+= 1
        history = [HistoryEntry(view: activeView, selectedId: nil)]
        historyIndex = 0
    }

    /// The per-account caches that live OUTSIDE this store. Every one of them
    /// is keyed by something a daemon minted — message ids, attachment ids,
    /// thread ids — so every one of them collides across accounts.
    ///
    /// Deliberately NOT wiped: `PreparedBodies` (keyed by a hash of the html
    /// itself), `ImageStore`'s files (keyed by sha256 of the url) and
    /// `HeroDisk`'s thumbnails (sha256 of the hero url, plus a sender-address
    /// index). All are content-addressed, so a cross-account hit is the same bytes. The image
    /// store's message-id PINS do go ambiguous across accounts, which
    /// mis-prioritises eviction and nothing else — accepted.
    private func wipeAccountCaches() {
        ThreadPrefetch.shared.wipe()
        // The queue of frames being rendered ahead: thread ids, one daemon's.
        FramePrewarmer.shared.wipe()
        // Two memos keyed on message ids: which prepared body a message has,
        // and which parts its body already shows.
        PreparedKeyMemo.shared.wipe()
        InBodyImages.shared.wipe()
        HeroCache.shared.wipe()
        EventVisuals.shared.wipe()
        AttachmentThumbs.shared.wipe()
        ComposeAttach.wipe()
        // The staged preview files, for the same reason and with more teeth: a
        // survivor here is another account's attachment served from this
        // account's id, straight into a preview.
        AttachmentFiles.shared.wipe()
        FrameHeights.shared.wipeAll()
        // The reader's live-frame pool. In EmailWebCore, which both targets
        // compile, so no platform fence.
        WebFramePool.shared.wipeAll()
        ImageWarmer.shared.resetForSwitch()
        // The clear chime is earned by clearing a board, and what armed it
        // was the other mailbox's. Left armed, switching from a busy account
        // to a quiet one plays it for mail nobody handled.
        QuietBoard.shared.armed = false
        // AuthDecisions.reload() is NOT here — see switchAccount step (8).
    }

    // MARK: - routing + history

    /// Browser-style history push: truncate any forward entries past the
    /// cursor, append, cap the length (dropping oldest), point at the new tail.
    private func pushHistory(_ entry: HistoryEntry) {
        var trimmed = Array(history.prefix(historyIndex + 1))
        trimmed.append(entry)
        if trimmed.count > historyCap { trimmed.removeFirst(trimmed.count - historyCap) }
        history = trimmed
        historyIndex = trimmed.count - 1
    }

    /// Whether the last route came from the POINTER: the rail slides its selector
    /// for a click (a continuous gesture the eye follows) and snaps for
    /// everything else (`3` is instantaneous; animating it just puts 300ms
    /// between the keypress and the answer). Read by the rail in the same update
    /// that carries the new `activeView`, so the two cannot disagree.
    private(set) var routeWasPointer = false

    /// EVERY route goes through here, so the flag can never be left describing
    /// the previous one — assigning `activeView` directly would inherit whatever
    /// the last click set and slide the rail for a keyboard route.
    private func route(to view: MainView, viaPointer: Bool = false) {
        // A route to a calendar page that is not offered (the gate, a history
        // entry from before it closed) lands on the sitrep instead.
        let view = view == .calendar && !calendarPageAvailable ? .sitrep : view
        // On change only: the repeat-press no-op in setView re-routes to the
        // same view, and that is not a navigation worth an event.
        if view != activeView { Analytics.screen(view.rawValue) }
        routeWasPointer = viaPointer
        activeView = view
        routeAsks &+= 1
    }

    /// Bumped by every route. The phone's shell answers a route with a tab and
    /// has to hear the call itself: it keeps `activeView` true only for the
    /// tabs that have one, so a route to the view already named changes
    /// nothing else (the same reason `searchAsks` exists).
    private(set) var routeAsks = 0

    func setView(_ view: MainView, viaPointer: Bool = false) {
        guard view != .calendar || calendarPageAvailable else { return }
        // Navigating ANYWHERE dismisses an open thread viewer: the rail is visible
        // beside it, so a click there means "leave this email", not "change the
        // page underneath the overlay". Through closeThread() so the reader's
        // inline reply is torn down with it, wherever the exit was taken from.
        closeThread()
        // No-op on the same view+selection, so a repeat press cannot spam
        // identical history entries.
        let cur = history.indices.contains(historyIndex) ? history[historyIndex] : nil
        if let cur, cur.view == view, cur.selectedId == selectedId {
            route(to: view, viaPointer: viaPointer)
            return
        }
        route(to: view, viaPointer: viaPointer)
        pushHistory(HistoryEntry(view: view, selectedId: selectedId))
    }

    /// Settings, ON A NAMED PANE. The sub-nav's section is a PREFERENCE (where
    /// you were last), so a caller that means one particular pane has to write
    /// it before routing or it lands wherever the last visit left off — which
    /// is why `A` for the audit log goes through here rather than setView.
    ///
    /// Writing the pref is also the honest half: the pane it opens is now the
    /// pane Settings remembers, exactly as if the row had been clicked.
    func openSettings(_ section: SettingsSection) {
        Prefs.shared.settingsSection = section
        setView(.settings)
    }

    /// Asks the active account's row to open its server + token fields. Read
    /// and cleared by `AccountRow`, the one place those fields live.
    var credentialsRequested = false

    /// Where a rejected token sends you: the Account pane with the live
    /// account's credentials already open, so the fix is the first thing on
    /// screen rather than a verb you have to know to press.
    func openCredentials() {
        credentialsRequested = true
        openSettings(.account)
    }

    /// Switch to the Emails view showing one PAGE — the header's noise count and
    /// the sitrep's noise affordances are both this.
    func openMail(_ mode: MailMode) {
        mailMode = mode
        setView(.emails)
    }

    /// Switch to the Emails view with a specific update selected — the sitrep's
    /// "view" affordances hand off to the band list with the right row focused.
    func viewInEmails(_ id: Int) {
        // A hand-off always targets a signal row, which the noise page does not
        // hold: land on the inbox or the highlight would have nothing to find.
        mailMode = .inbox
        route(to: .emails)
        selectedId = id
        closeThread()
        pushHistory(HistoryEntry(view: .emails, selectedId: id))
    }

    /// The person-level facts PostHog cohorts on (see `Analytics.identify`):
    /// how many mailboxes, and the thread style. Called at launch, on every
    /// activation, and whenever the account list changes, so the person row
    /// tracks the install rather than its first day.
    func identifyPerson() {
        Analytics.identify([
            "accounts": AccountManager.shared.accounts.count,
            "thread_style": Prefs.shared.threadStyle.rawValue,
        ])
    }

    /// The record zones' shared click routing: open the row's thread, falling
    /// back to the emails-page jump when an older daemon sent no thread_id —
    /// the click always does SOMETHING.
    func openRecord(thread: String?, message: Int) {
        if let thread, !thread.isEmpty {
            openThread(thread, from: .sitrepCard)
        } else {
            viewInEmails(message)
        }
    }

    var canGoBack: Bool { historyIndex > 0 }
    var canGoForward: Bool { historyIndex < history.count - 1 }

    func goBack() {
        guard historyIndex > 0 else { return }
        historyIndex -= 1
        let entry = history[historyIndex]
        route(to: entry.view)
        selectedId = entry.selectedId
    }

    func goForward() {
        guard historyIndex < history.count - 1 else { return }
        historyIndex += 1
        let entry = history[historyIndex]
        route(to: entry.view)
        selectedId = entry.selectedId
    }

    // MARK: - selection

    /// Flat, band-ordered id list the keymap uses for j/k movement.
    var orderedIds: [Int] {
        (sitrep.standing + sitrep.new + sitrep.open).map(\.id)
    }

    var selectedUpdate: AttentionUpdate? {
        guard let selectedId else { return nil }
        return (sitrep.standing + sitrep.new + sitrep.open).first { $0.id == selectedId }
    }

    func moveSelection(_ delta: Int) {
        let ids = orderedIds
        guard !ids.isEmpty else { return }
        let idx = selectedId.flatMap { ids.firstIndex(of: $0) } ?? -1
        selectedId = ids[min(max(idx + delta, 0), ids.count - 1)]
    }

    // MARK: - surfaces

    /// The exact message requested by a notification or another direct link.
    /// It may still be pending triage and never needs a feed row to open.
    var focusedMessageId: Int?
    var focusedMessageView: ClientThreadView?

    /// WHAT THE READER MOUNTS WITH, decided here, once, at `openThread` — the
    /// cached thread and the style and landing index resolved for it — so
    /// that `ThreadViewer.init` can seed its state without reading anything
    /// observable. It cannot: a view's initializer runs inside its PARENT's
    /// body evaluation, and every observable read made there is tracked by
    /// the parent. Resolving the style in the initializer made the whole
    /// window shell a reader of the reading zone and the focus request, and
    /// re-ran the style guess over every message each time the shell was
    /// evaluated. UNTRACKED, deliberately: the reader reads it in `init`.
    struct ReaderSeed {
        let threadId: String
        let view: ClientThreadView
        let style: ThreadStyle
        let index: Int
    }
    @ObservationIgnored private(set) var readerSeed: ReaderSeed?

    func openMessage(_ messageId: Int, from source: OpenSource, auth: SealedMeta? = nil) {
        // AUTH MAIL OPENS IN THE REVEAL, decided BEFORE `getMessage`: that read
        // carries the body, unaudited, and must not run first. `auth` is the
        // caller's own knowledge (an event marked `is_auth`); the sealed list
        // is the second signal.
        if let meta = sealedMeta(message: messageId) ?? auth {
            requestReveal(meta)
            return
        }
        let openedEpoch = epoch
        Task {
            do {
                let envelope = try await APIClient.shared.getMessage(messageId)
                guard isCurrent(openedEpoch) else { return }
                openThread(envelope.thread.thread_id, focusMessage: messageId, from: source)
                // Direct targets may contain credentials. Keep them only in
                // active reader state, never the reusable prefetch cache.
                focusedMessageView = envelope.thread
            } catch {
                guard isCurrent(openedEpoch) else { return }
                refreshError = RefreshError(message: errText(error, "Could not open this email"), kind: .unknown)
            }
        }
    }

    /// Open the fullscreen reader. `replyTo` is the unified `r` verb: the message
    /// id the reader should open its inline composer on once the thread loads.
    func openThread(
        _ threadId: String, queue: [AttentionUpdate] = [], replyTo: Int? = nil,
        entering edge: ThreadEdge? = nil, focusMessage: Int? = nil, readingMail: Bool = false,
        from source: OpenSource
    ) {
        // from_noise: an open from below the squelch line — someone digging for
        // mail the triage muted, which is the false-negative signal. source:
        // which surface earned the open (see OpenSource).
        Analytics.capture(
            "thread_opened",
            [
                "via_reply": replyTo != nil,
                "from_noise": queue.first { $0.thread_id == threadId }?.placement == "noise",
                "source": source.rawValue,
            ])
        // Opening is acknowledged by the reader once the exact message is
        // displayed. Fetching or prefetching a sibling is not a read.
        focusedMessageId = focusMessage
        focusedMessageView = nil
        // A DIFFERENT thread drops the summary NOW rather than when the new one
        // lands: in that gap the reader is showing thread B while this still
        // described A, and the ask bar would pin B's id under A's subject and
        // A's message id. Same-thread reopens keep it — the viewer is already
        // mounted and may never re-adopt.
        if self.threadId != threadId {
            openThreadSummary = nil
            // The message that was in focus is not any more, whatever the new
            // thread turns out to hold. A same-thread reopen (a notification
            // for the thread already up, an undo) leaves the stint running:
            // the reader is still looking at it.
            attentionBlur()
        }
        // The reader's slot holds one thing: a thread replaces a preview.
        scheduledPreviewId = nil
        self.threadId = threadId
        // The arrivals ledger follows the thread, and declines to forget on a
        // REOPEN of the same one — see ThreadArrivals.reset.
        arrivals.reset(to: threadId)
        // A SPACE WALK LIVES ON ONE QUEUE. Its own steps (E, S, h, the arrows)
        // hand the same list back; anything else that opens a thread (a
        // notification, a citation, the side panel) is a different journey,
        // and a walk carried into it would celebrate mail it never held.
        if walk != nil, queue.map(\.thread_id) != threadQueue.map(\.thread_id) {
            walk = nil
            queuedWalkStep = nil
            dropSkipUndos()
        }
        self.threadQueue = queue
        self.readingMail = readingMail
        // The board's order belongs to a walk that began on a Reading tile;
        // the reader's own steps pass `readingMail` back and keep it.
        if !readingMail { readingOrder = [] }
        // Any open ends an S turn that is still settling: the turn's outgoing
        // card would otherwise linger (or sit inert, if this re-opens it)
        // until the skip's own timer cleared it. A skip sets its turn AFTER
        // this call, so its own open does not cancel it.
        skipTurn = nil
        flightContinues = false
        // The seed, after `readingMail` (the style reads it) and before the
        // view mounts. A direct open (`openMessage`) has no cached view yet
        // and hands the reader its copy through `focusedMessageView` instead.
        // NOT A COPY THAT LACKS THE MESSAGE THIS OPEN NAMES (the reply it is
        // for, the message it lands on): the reader goes to the daemon for that
        // one (ThreadViewer.load), and a seed it then replaces is a flash.
        readerSeed = ThreadPrefetch.shared.cached(
            threadId, holding: [replyTo, focusMessage].compactMap { $0 }
        ).map { view in
            ReaderSeed(
                threadId: threadId, view: view,
                style: ThreadViewer.resolveStyle(
                    threadId: threadId, messages: view.messages,
                    readingMail: ThreadViewer.isReadingMail(threadId, self)),
                index: ThreadOpeningFocus.index(
                    messageIds: view.messages.map(\.id), requested: focusMessage))
        }
        // An ordinary open puts the reader straight in the window — opening an
        // email is a jump. Only done+next passes an edge, and it walks the
        // reader home itself a frame later.
        self.threadFlight = edge.map { .entering($0) } ?? .settled
        // HERE, not in the search panel's open path: the reader can be opened
        // from anywhere (ask-bar cards, citations) while fullscreen search sits
        // underneath, and it insets by only the STRIP — a still-expanded panel
        // would peek out as a sliced-off edge behind it.
        search.expanded = false
        // Both cleared unconditionally: moving to ANOTHER thread (h/l, done+next)
        // must not carry the previous one's draft or its pending reply request
        // into a thread they do not belong to. The draft is SAVED on the way out
        // rather than dropped — the reply is keyed to the message it answers, so
        // walking away from it and coming back restores it.
        DraftSaver.shared.flush(.inlineReply, inlineReply)
        inlineReply = nil
        pendingReplyMessageId = replyTo
    }

    func closeThread() {
        // Every way out of the reader ends a Space walk; only `finishWalk`
        // turns one into a celebration, and it reads the walk before this.
        walk = nil
        queuedWalkStep = nil
        dropSkipUndos()
        walkRewind = nil
        skipTurn = nil
        flightContinues = false
        // The preview lives in the reader's slot, so every way out of the
        // reader (Esc, the rail, an account switch) is a way out of it too.
        scheduledPreviewId = nil
        focusedMessageId = nil
        focusedMessageView = nil
        threadId = nil
        arrivals.reset(to: nil)
        attentionBlur()
        threadQueue = []
        readingMail = false
        readingOrder = []
        // The flight is deliberately left where it is: the last email of a queue
        // is mid-departure when this runs, and there is nothing left to put back
        // in the window. `openThread` resets it for whatever comes next.
        openThreadSummary = nil
        DraftSaver.shared.flush(.inlineReply, inlineReply)
        inlineReply = nil
        pendingReplyMessageId = nil
    }

    // MARK: - reading attention

    /// Milliseconds on a clock that only goes forward and does not run while
    /// the machine sleeps. Wall time does neither: a timezone change or an NTP
    /// step mid-read would hand the dwell clock a negative or an hour-long
    /// stint, and a lid closed on an open email would count the night.
    private static func attentionNow() -> UInt64 {
        DispatchTime.now().uptimeNanoseconds / 1_000_000
    }

    /// The clock, for the live epoch. Anything it held for another account is
    /// dropped unsent: by the time this runs the client is talking to a
    /// different daemon, and there is no longer anywhere honest to send it.
    private func withAttention<T>(_ body: (inout AttentionClock, UInt64) -> T) -> T {
        if attentionEpoch != epoch {
            attention = AttentionClock()
            attentionEpoch = epoch
        }
        return body(&attention, Self.attentionNow())
    }

    /// A RECEIVED message is at the reader's focus. Called on every geometry
    /// tick of the selected card, so the clock answers whether anything
    /// closed and only then is a flush worth a task.
    ///
    /// The fixture inbox never starts a stint: its mail is fixtures, and its
    /// ids would be reported against whatever real message shares them.
    func attentionFocus(_ messageId: Int) {
        guard !FixtureMode.isEnabled else { return }
        if withAttention({ $0.focus(messageId, now: $1) }) { flushAttention() }
    }

    /// Nothing that counts is at the focus any more: the reader closed, moved
    /// to another thread, went to the background, or scrolled onto a message
    /// the user wrote.
    func attentionBlur() {
        if withAttention({ $0.blur(now: $1) }) { flushAttention() }
    }

    /// The scene stopped being active (another app, another window). The
    /// stint stays open, and stops earning until `attentionResume`.
    func attentionPause() { withAttention { $0.pause(now: $1) } }

    func attentionResume() { withAttention { $0.resume(now: $1) } }

    /// A link in a received message's BODY was activated. Sent at once rather
    /// than with the stint: the click usually hands the user to a browser, and
    /// "when they come back and close the reader" may be never.
    func attentionClick(_ messageId: Int) {
        guard !FixtureMode.isEnabled else { return }
        withAttention { clock, _ in clock.click(messageId) }
        flushAttention()
    }

    /// Send what is owed, best effort and off to the side: nothing awaits
    /// this, a failure is dropped inside `reportAttention`, and there is no
    /// retry. Sequential so an old daemon's first 404 stops the rest; fenced
    /// on the epoch per report, because the ids mean nothing to the next
    /// account's daemon.
    private func flushAttention() {
        let reports = withAttention { clock, _ in clock.drain() }
        guard !reports.isEmpty, !FixtureMode.isEnabled else { return }
        let e = epoch
        let prior = attentionFlight
        attentionFlight = Task {
            await prior?.value
            for report in reports {
                guard isCurrent(e) else { return }
                await APIClient.shared.reportAttention(
                    messageId: report.messageId, dwellMs: report.dwellMs,
                    linkClicks: report.linkClicks)
            }
        }
    }

    /// Close any open stint and wait until every look is reported. Smart
    /// clear judges a sender by how long their mail was looked at, so a read
    /// still in this process when the press goes out would be judged a
    /// glance. Bounded by the reports' own request timeouts.
    func settleAttention() async {
        attentionBlur()
        await attentionFlight?.value
    }

    // MARK: - live arrivals

    /// A frame off the live event feed, offered to the thread on screen.
    ///
    /// THE ACCOUNT IS AN ARGUMENT and never read from here. One event stream
    /// runs per account — including the ones nobody is looking at, which is the
    /// entire reason they are held — and thread ids and message ids alike are
    /// one daemon's SQLite ints. A background mailbox whose ids happened to
    /// collide with the live thread's would otherwise pull a stranger's mail
    /// into the email on screen.
    func noteLiveEvent(_ event: Event, accountId: UUID) {
        guard AccountManager.shared.activeId == accountId else { return }
        // A read receipt is not new mail: nothing joined the thread, and
        // `sender` on one of those rows is the user's own address. The reader
        // keeps its receipts fresh on its own schedule.
        guard event.kind != .opened else { return }
        // A poll slowed down because nobody is looking catches up now.
        SitrepPoller.shared.noteLiveEvent()
        // The fastest word that a cached thread is behind: drop the copy now
        // rather than at the pull this asks for.
        ThreadPrefetch.shared.retire(CollectionOfOne((event.thread_id, event.message_id)))
        noteThreadArrival(
            thread: event.thread_id, message: event.message_id, sender: event.sender)
    }

    /// NEW MAIL IN THE THREAD SOMEBODY IS READING — the one door, and both
    /// reporters come through it: the event feed the moment triage emits, and
    /// the poll that is the only one to see an arrival the feed missed or
    /// never carried.
    ///
    /// Two things follow and they are decided separately (see ThreadArrivals).
    /// The REFETCH is a token bump the viewer answers by adopting a fresh copy
    /// in place — that is what puts the reply into the thread under the
    /// reader's eyes, without taking the page away from them. The ANNOUNCEMENT
    /// is the toast, and it is the only thing that says so out loud: the system
    /// banner is deliberately suppressed while the app is frontmost with a
    /// window up (see Notifier.presentation), which is precisely the case a
    /// person reading the thread is in.
    func noteThreadArrival(thread: String, message: Int, sender: String) {
        let verdict = arrivals.admit(
            thread: thread, message: message, held: currentThreadSummary?.newestMessageId)
        guard verdict.refetch else { return }
        openThreadRefreshToken &+= 1
        guard verdict.announce else { return }
        // The banner's rule, not the row's: this toast fires in exactly the
        // case the system banner is suppressed (app frontmost, thread open),
        // and the same arrival must not be announced under two names.
        let name = SenderID.readableName(sender)
        pushToast(name.isEmpty ? "new message in this thread" : "new message from \(name)")
    }

    /// True while a modal owns the screen; the surfaces under it blur so the modal
    /// reads as focus rather than as a new page. Toasts are deliberately absent (a
    /// toast must never defocus the app), as are the side panels, the thread
    /// viewer, the compose pane and the calendar's event pane: those are
    /// surfaces you work beside, not overlays on one. The reader's `inlineReply` is absent for the same reason:
    /// it is part of the viewer, and blurring the email you are answering would
    /// be absurd.
    /// The first-load veil is one: the board filling in behind it is the
    /// point, and the blur is what says "not yet" without hiding it.
    /// The smart clear list is NOT here: it is a page in the content area
    /// beside the real rail, opaque over the board, and a blur would smear
    /// the rail it sits next to.
    var modalOverlayOpen: Bool {
        askBarOpen || shortcutsOpen
            || triageFix != nil || remindTarget != nil || schedulePicker != nil
            || ruleEditor != nil
            || groupEditor != nil
            || !authQueue.isEmpty || retriage != nil || retriageAsk != nil
            || reconnectAsk != nil || discardAsk != nil || liveMailtoAsk != nil
            || calendarCardUp
            || calendarDeleteAsk != nil
            || firstLoad.active || whatsNew.active
    }

    // MARK: - sitrep zones

    /// Refresh every zone CONCURRENTLY, replacing rows only once new ones arrive,
    /// and skip entirely while the last refresh is still fresh — that is what
    /// makes a revisit instant instead of five round-trips. `force` is for callers
    /// that just changed the data (an explicit sync, a rule save).
    func refreshZones(force: Bool = false) async {
        if !force, let loadedAt = zonesLoadedAt,
            Date().timeIntervalSince(loadedAt) < Self.zoneTTL
        {
            return
        }
        // JOIN an in-flight refresh rather than starting a second one. The TTL
        // cannot do this job: `loadedAt` is stamped only when a refresh COMPLETES,
        // and on a cold load all five zones plus the dashboard sail past it from
        // their own `.task` and each fire a full copy of the five requests.
        //
        // A FORCED caller does not stop there: a pass already in flight may have
        // read the daemon BEFORE the change it forced for landed.
        if let running = zoneRefresh {
            await running.value
            if !force { return }
        }
        let refresh = Task { await performZoneRefresh() }
        zoneRefresh = refresh
        await refresh.value
        // Only clear our OWN marker: a forced caller can have replaced it while we
        // were suspended, and nil-ing that one lets the next joiner start a third
        // redundant pass.
        if zoneRefresh == refresh { zoneRefresh = nil }
    }

    /// In-flight zone refresh, so concurrent callers share one pass.
    private var zoneRefresh: Task<Void, Never>?

    /// One zone's answer, tagged so completion order can drive the writes.
    /// nil rows = that endpoint failed and its zone keeps what it had.
    private enum ZoneAnswer: Sendable {
        case shipments([Shipment]?)
        case reading([ReadingSender]?)
        case records(RecordsFeedZones?)
    }

    private func performZoneRefresh() async {
        let e = epoch
        // Three independent fetches racing in one group, each zone written the
        // moment ITS fetch answers — completion order, not a fixed await
        // order, which is what actually keeps one wedged endpoint riding out
        // its timeout from holding every later zone's paint hostage. Every
        // write is individually fenced on the epoch, so a switch landing
        // mid-pass cannot show half of each mailbox; a failing endpoint (nil
        // rows) leaves its own zone's last good rows rather than blanking the
        // column.
        var missed = false
        await withTaskGroup(of: ZoneAnswer.self) { group in
            group.addTask {
                .shipments(try? await APIClient.shared.getShipments(includeDelivered: true))
            }
            group.addTask { .reading(await ReadingFeed.load()) }
            group.addTask {
                // Derived HERE, off the main actor: three walks over up to
                // a thousand records, every 45 seconds.
                guard let feed = try? await APIClient.shared.getFeed(destination: "records", limit: 1000)
                else { return .records(nil) }
                return .records(RecordsFeedZones(feed))
            }
            for await answer in group {
                guard e == epoch else {
                    group.cancelAll()
                    return
                }
                // ASSIGN ONLY ON CHANGE, every one: most passes find the
                // rows they found 45 seconds ago, and an equal write still
                // runs the zone's hooks (a board save, the reading threads).
                switch answer {
                case .shipments(let rows?):
                    if rows != zones.shipments { zones.shipments = rows }
                case .reading(let rows?):
                    if rows != zones.reading { zones.reading = rows }
                    if !zones.readingLoaded { zones.readingLoaded = true }
                case .records(let records?):
                    if records.receipts != zones.receipts { zones.receipts = records.receipts }
                    if records.banking != zones.banking { zones.banking = records.banking }
                    if records.calendar != zones.calendar { zones.calendar = records.calendar }
                case .shipments, .reading, .records: missed = true
                }
            }
        }
        guard e == epoch else { return }
        // A PASS WITH A FAILED FETCH EARNS ONE EARLY RETRY. That zone kept
        // its old rows, and stamping over it made one dropped request a
        // 45-second hole: on a mailbox just switched to, an empty Reading
        // zone that every later tick declined to ask about again. So the
        // first miss clears the stamp and the next poll tick asks again.
        //
        // ONE, not one per tick: the poller awaits this pass inside its pull,
        // so an endpoint that fails every time would otherwise be asked every
        // ten seconds forever (the records feed is a thousand rows), and one
        // that times out would hold every pull for the length of the timeout.
        // A second miss in a row goes back to the ordinary cadence.
        if missed, !zoneRetryOwed {
            zoneRetryOwed = true
            zonesLoadedAt = nil
        } else {
            zoneRetryOwed = missed
            zonesLoadedAt = Date()
        }

        preloadBoardHeroes()
        warmZoneThreads()
        // The reading half of the launch image warm's input; the bands are the
        // other half, and it starts once both have landed.
        ImageWarmer.shared.noteZonesLanded()
    }

    /// How long a zone refresh stays good: long enough that flipping between views
    /// is free, short enough that a dashboard left open still tracks the mail (the
    /// sitrep poll, every 10s while frontmost, drives the bands, which change
    /// minute to minute).
    private static let zoneTTL: TimeInterval = 45

    /// Dismiss one shipment from the Shipments zone.
    ///
    /// OPTIMISTIC, then authoritative: the card leaves the rail on the click,
    /// because a records rail is polled on a 45s TTL and waiting for the round
    /// trip would read as the menu item doing nothing. The forced refresh behind
    /// it is what makes the daemon's list — not this local edit — the thing on
    /// screen a moment later.
    ///
    /// NO UNDO, deliberately: clearing hides a row rather than destroying it, and
    /// the daemon brings it straight back when the carrier or a new email reports
    /// something new. A failed request puts the row back instead.
    func clearShipment(_ id: Int) async {
        let e = epoch
        guard let index = zones.shipments.firstIndex(where: { $0.id == id }) else { return }
        let row = zones.shipments[index]
        zones.shipments.remove(at: index)
        do {
            try await APIClient.shared.clearShipment(id)
            // The write landed on the OLD account's daemon; this account's rail
            // knows nothing about it and must not be refreshed on its behalf.
            guard e == epoch else { return }
            Analytics.capture("shipment_cleared")
            pushToast("cleared \(Self.shipmentLabel(row)) · it returns if there's news", .info)
            await refreshZones(force: true)
        } catch {
            guard e == epoch else { return }
            // Put it back where it was — but only if it is still missing: a zone
            // refresh can have landed while the request was in flight, and that
            // list is newer than this snapshot of one row.
            if !zones.shipments.contains(where: { $0.id == id }) {
                zones.shipments.insert(row, at: min(index, zones.shipments.count))
            }
            if let api = error as? APIError, api.kind == .notFound {
                // Two ways to earn this: a row this account no longer has, or a
                // daemon older than the route. The daemon's own word for both is
                // "not found", which is not a sentence anyone can act on, so say
                // the one thing that fixes the likelier of the two.
                pushToast("could not clear that package · update squelchd", .error)
            } else {
                pushToast(errText(error, "could not clear that package"), .error)
            }
        }
    }

    /// What a cleared shipment is CALLED in its toast: the card's own title rule
    /// (item name, else the merchant, else "Package").
    private static func shipmentLabel(_ s: Shipment) -> String { s.displayTitle }

    // MARK: - dev re-triage

    /// A dev re-triage in flight. NON-NIL BLOCKS THE APP: the modal covers
    /// everything and takes every click, because a re-triage rewrites tiers and
    /// bands underneath whatever page you would otherwise be reading.
    var retriage: RetriageRun?

    /// A trailing-days re-triage the human has ASKED for and not yet confirmed.
    ///
    /// Held HERE rather than in `RetriageButton` for two reasons. The button is a
    /// text chip inside a masthead HStack, and an `OverlayScrim` mounted there
    /// would be clipped to the width of the chip. And there are two copies of
    /// that button (the sitrep's masthead, the emails bar), so a dialog owned by
    /// one of them would be the wrong one half the time. Every other modal in
    /// this app is a field on the store that `ActionLayer` draws; so is this.
    var retriageAsk: Int?

    /// Ask before the window goes away. The run is minutes long, blocks the whole
    /// app and spends on the model, and the chip that starts it sits a few points
    /// from the sync stamp: one confirm is the difference between a re-triage and
    /// a misclick you have to sit through (#210).
    func askRetriage(days: Int) {
        guard retriage == nil else { return }
        retriageAsk = days
    }

    func cancelRetriageAsk() {
        retriageAsk = nil
    }

    /// Confirmed: drop the question first, so the blocking modal replaces the
    /// dialog rather than stacking on top of one that is still asking.
    func confirmRetriage() async {
        guard let days = retriageAsk else { return }
        retriageAsk = nil
        await startRetriage(days: days)
    }

    /// How often the modal asks the daemon where it is. A re-triage takes
    /// minutes and the poll is one indexed aggregate, so this is about how alive
    /// the counter should FEEL, not about cost.
    private static let retriagePollSeconds: UInt64 = 2

    /// Kick a re-triage of the trailing `days` and watch it to the end.
    ///
    /// The kick's own `reset` seeds the total so the modal opens with a real
    /// number instead of a zero that ticks up; the first poll then replaces it
    /// with the SERVER's total, which is the authority (a second kick, or a
    /// per-message re-triage from the fix palette, joins the same run).
    func startRetriage(days: Int) async {
        guard retriage == nil else { return }
        retriageAsk = nil
        let e = epoch
        retriage = RetriageRun()
        do {
            let result = try await APIClient.shared.retriage(.days(days))
            guard e == epoch else { return endRetriage() }
            // Nothing to do is not a run: opening a blocking modal over zero
            // rows would be a wall the user has to dismiss to learn nothing
            // happened.
            guard result.reset > 0 else {
                retriage = nil
                pushToast("nothing to re-triage in the last \(days)d", .info)
                return
            }
            retriage?.total = result.reset
        } catch {
            retriage = nil
            pushToast(errText(error, "re-triage failed"), .error)
            return
        }
        await watchRetriage(epoch: e)
    }

    /// Poll until the queues drain, then let the board catch up. Ends by itself
    /// on completion; `endRetriage` is the human's way out from the modal, and
    /// the `retriage != nil` guards are what make that immediate — the server
    /// keeps working either way, this only stops WATCHING.
    ///
    /// EVERY RESUME IS FENCED ON THE EPOCH, like every other write in this file.
    /// An account switch mid-run points `APIClient` at a DIFFERENT daemon, and
    /// without the fence this loop would go on polling — counting the new
    /// mailbox's re-triage, or reading its empty `total` as this run finishing
    /// and closing the modal on a success toast about mail it never touched.
    private func watchRetriage(epoch e: Int) async {
        while retriage != nil {
            try? await Task.sleep(for: .seconds(Self.retriagePollSeconds))
            guard retriage != nil, e == epoch else { return endRetriage() }
            do {
                let p = try await APIClient.shared.retriageProgress()
                guard retriage != nil, e == epoch else { return endRetriage() }
                retriage?.adopt(p)
            } catch {
                guard retriage != nil, e == epoch else { return endRetriage() }
                // A daemon too old to answer cannot be waited on: say so and
                // leave the modal standing on its "close" button rather than
                // spinning against a route that will never exist.
                if let api = error as? APIError, api.kind == .notFound {
                    retriage?.unsupported = true
                    return
                }
                retriage?.failure = errText(error, "lost the daemon")
                return
            }
            if retriage?.finished == true { break }
        }
        guard retriage != nil, e == epoch else { return endRetriage() }
        // The whole point of the wait: the tiers on screen are the OLD verdicts
        // until this lands.
        let count = retriage?.total ?? 0
        retriage = nil
        // The same join-then-pull a triage correction uses: a poll already in
        // flight may have read the daemon mid-run, so its rows are not the ones
        // to settle on.
        await SitrepPoller.shared.refreshAfterCorrection()
        pushToast("re-triaged \(count) email\(count == 1 ? "" : "s")", .success)
    }

    /// Stop watching. The daemon's queues are untouched — they drain on their
    /// own — so this is only about giving the window back.
    func endRetriage() {
        retriage = nil
    }

    // MARK: - the flat mail pages

    /// The page size, for the head the poll refreshes and for every older
    /// page the infinite scroll asks for.
    private static let mailLimit = 100
    /// How many rows get their thread warmed ahead of any click. A bounded
    /// prefix: warming all 500 would stampede the daemon for mail nobody scrolls
    /// to, and hover warming picks up anything past it.
    private static let mailWarmRows = 40
    /// Only long enough to collapse a mount and a poll tick landing together;
    /// staleness past it is harmless because a reload keeps the rows on screen.
    private static let mailTTL: TimeInterval = 5

    /// The emails tab's pages, HELD HERE RATHER THAN IN THE VIEW. `@State` is
    /// discarded on navigate-away, so a view-owned list refetched from nothing on
    /// every visit and showed "loading mail…" over an empty page each time. Held in
    /// the store they survive navigation, and `Loadable` keeps the last rows through
    /// a reload, so even a stale revisit paints rows first and updates underneath.
    private var mailPages: [MailMode: Loadable<[AttentionUpdate]>] = [:] {
        didSet { scheduleBoardSave() }
    }

    /// The head of each loaded page: what a snapshot keeps of `mailPages`.
    private static func pageHeads(
        _ pages: [MailMode: Loadable<[AttentionUpdate]>]
    ) -> [MailMode: [AttentionUpdate]] {
        pages.compactMapValues { $0.value.map { Array($0.prefix(mailLimit)) } }
    }

    /// Which page the emails tab is on. Here for the same reason as the pages
    /// themselves: a trip to the sitrep and back has to land where you left off.
    var mailMode: MailMode = .inbox
    /// Freshness and in-flight fetch, PER PAGE — one shared stamp would let the
    /// inbox's TTL swallow the noise page's first fetch. Kept out of the
    /// `Loadable`s, which the list reads: these are bookkeeping, not rows.
    private var mailLoadedAt: [MailMode: Date] = [:]

    /// Where each page's infinite scroll stands.
    struct MailPaging: Equatable {
        /// The offset cursor for a daemon that cannot page by arrival.
        var nextCursor: String?
        /// Nothing older to fetch.
        var exhausted = false
        var loadingMore = false
        /// When the last older-page fetch failed. The scroll does not retry
        /// on its own for a while after, or a sentinel left on screen would
        /// re-request in a tight loop against a daemon that is down.
        var failedAt: Date?
    }
    private static let mailRetryAfter: TimeInterval = 15

    /// Whether this daemon pages by arrival (`before`) and so can be asked for
    /// a mailbox-bounded head. Learned from the first rows that carry
    /// `received_at`; until then the head asks for the default window.
    private var mailKeyset = false
    private var mailPagingState: [MailMode: MailPaging] = [:]
    func mailPaging(_ mode: MailMode) -> MailPaging { mailPagingState[mode] ?? MailPaging() }

    /// THE INFINITE SCROLL: fetch the page older than the last row loaded and
    /// append it. By arrival (`before`) on a daemon that sends `received_at`,
    /// which does not shift as new mail lands and reaches past the 30-day
    /// window; by the offset cursor on one that does not, inside that window.
    /// One in flight per page, and none once the mailbox has run out.
    func loadMoreMail(_ mode: MailMode) async {
        var paging = mailPaging(mode)
        if let failed = paging.failedAt, Date().timeIntervalSince(failed) < Self.mailRetryAfter {
            return
        }
        guard !paging.exhausted, !paging.loadingMore,
            let last = mailPages[mode]?.value?.last
        else { return }
        var params = UpdatesParams(tier: mode.tier, limit: Self.mailLimit, spamOnly: mode.spamOnly)
        if let at = last.received_at {
            params.before = at
            params.beforeId = last.id
        } else if let cursor = paging.nextCursor {
            params.cursor = cursor
        } else {
            paging.exhausted = true
            mailPagingState[mode] = paging
            return
        }
        paging.loadingMore = true
        mailPagingState[mode] = paging
        let e = epoch
        do {
            // PEEK: an older page is fetched AHEAD of the eye (and through rows
            // a lens or the category picker hides), so listing it is not the
            // user seeing it. A row is stamped when it is actually opened.
            let fetched = try await APIClient.shared.getUpdates(params, peek: true)
            guard e == epoch else { return }
            var added = 0
            withMailPage(mode) { page in
                guard var rows = page.value else { return }
                let have = Set(rows.map(\.id))
                let fresh = fetched.items.filter { !have.contains($0.id) }
                added = fresh.count
                rows.append(contentsOf: MailList.sortedByArrival(fresh))
                page.value = rows
            }
            paging = mailPaging(mode)
            paging.nextCursor = fetched.next_cursor
            // Short page, or a page of nothing new (an older daemon that
            // ignored `before` and answered the head again): the end.
            paging.exhausted = fetched.items.count < Self.mailLimit || added == 0
            paging.failedAt = nil
        } catch {
            guard e == epoch else { return }
            paging = mailPaging(mode)
            paging.failedAt = Date()
        }
        paging.loadingMore = false
        mailPagingState[mode] = paging
    }

    /// Clear a failed older-page fetch so the next scroll (or the footer's
    /// Try again) asks straight away.
    func retryOlderMail(_ mode: MailMode) {
        mailPagingState[mode, default: MailPaging()].failedAt = nil
        Task { await loadMoreMail(mode) }
    }

    /// Back to the head on page re-entry, when the daemon can page by arrival:
    /// rows deep in the scroll are only refreshed by being fetched again, and
    /// re-entry is where a stale status or a missing pill would be noticed.
    /// The scroll refills them as you go.
    func trimMailToHead(_ mode: MailMode) {
        guard mailKeyset, let rows = mailPages[mode]?.value, rows.count > Self.mailLimit else {
            return
        }
        withMailPage(mode) { $0.value = Array(rows.prefix(Self.mailLimit)) }
        mailPagingState[mode] = MailPaging()
    }
    private var mailRefreshes: [MailMode: Task<Void, Never>] = [:]

    /// One page's rows. Never fetched reads as LOADING, so the first paint of a
    /// page shows "loading" rather than claiming it is empty.
    ///
    /// EVERY PAGE BUT `.sent`, which holds a different wire type and lives in
    /// `sentPage`; asking here for it answers a permanent "loading".
    func mailPage(_ mode: MailMode) -> Loadable<[AttentionUpdate]> {
        mailPages[mode] ?? .loading
    }

    /// Same TTL-plus-join shape as `refreshZones`, for the same reasons: a
    /// revisit inside the window is free, and concurrent callers share one pass
    /// instead of each firing a 500-row fetch.
    func refreshMail(_ mode: MailMode, force: Bool = false) async {
        if !force, let loadedAt = mailLoadedAt[mode],
            Date().timeIntervalSince(loadedAt) < Self.mailTTL
        {
            return
        }
        if let running = mailRefreshes[mode] {
            await running.value
            if !force { return }
        }
        let refresh = Task { await performMailRefresh(mode) }
        mailRefreshes[mode] = refresh
        await refresh.value
        if mailRefreshes[mode] == refresh { mailRefreshes[mode] = nil }
    }

    /// Mark a cached `done` row open again on every mail page: the local half
    /// of an undone resolve, until the next poll says the same.
    func reopenInMail(_ messageId: Int) {
        for mode in MailMode.allCases {
            withMailPage(mode) { page in
                guard var rows = page.value,
                    let i = rows.firstIndex(where: { $0.id == messageId && $0.status == .done })
                else { return }
                rows[i].status = .open
                rows[i].resolved_at = nil
                page.value = rows
            }
        }
    }

    /// Drop a message from the cached pages outright, for mail that has LEFT
    /// the listing (rescued out of spam), not mail that is merely finished:
    /// finished mail stays, dimmed. Every page, not just the one on screen.
    func removeFromMail(_ messageId: Int) {
        for mode in MailMode.allCases {
            mailPages[mode]?.value?.removeAll { $0.id == messageId }
        }
    }

    private func performMailRefresh(_ mode: MailMode) async {
        let e = epoch
        // The spinner is for a page with nothing on it. A refresh under rows
        // already showing has nothing to spin, and flipping the flag twice a
        // tick re-rendered the list twice a tick.
        withMailPage(mode) { if $0.value == nil { $0.isLoading = true } }
        let fetchedAt = Date()
        do {
            // A MAILBOX-BOUNDED head once the daemon is known to page by
            // arrival: the newest `mailLimit` rows of all time, so the head's
            // last row is a true floor for the merge and the keyset. The
            // default 30-day window would end the scroll for anyone with less
            // than a page of recent mail, and its out-of-window reminder rows
            // would pose as the floor.
            let bounded = mailKeyset
            let fetched = try await APIClient.shared.getUpdates(
                UpdatesParams(
                    since: bounded ? "1970-01-01T00:00:00Z" : nil,
                    tier: mode.tier, limit: Self.mailLimit, spamOnly: mode.spamOnly))
            // The rows belong to the account that asked for them. Both exits
            // below return rather than fall through, so the `isLoading` write
            // at the bottom is only ever reached by the live epoch.
            guard e == epoch else { return }
            releaseReopened(fetched.items, fetchedAt: fetchedAt)
            // DONE MAIL STAYS IN THE CACHE; the page shows it dimmed.
            //
            let keyset = mailKeyset || fetched.items.contains { $0.received_at != nil }
            mailKeyset = keyset
            var headRows = fetched.items
            if keyset && !bounded {
                // A window-bounded head (the first, before the daemon was
                // known): drop the old reminder rows the window's OR clause
                // lets in, or the oldest of them would pose as the floor and
                // the scroll would page from months back.
                let cutoff = Date().addingTimeInterval(-29 * 86_400).timeIntervalSince1970
                headRows.removeAll { u in
                    (u.remind_at != nil || u.reminded_at != nil) && MailList.arrivalTS(u) < cutoff
                }
            }
            let head = MailList.sortedByArrival(headRows)
            let full = fetched.items.count >= Self.mailLimit
            let merge = MailList.mergeHead(
                loaded: mailPages[mode]?.value, head: head, headIsFull: full)
            withMailPage(mode) { page in
                // Only touch the value when the list actually CHANGED: the poll
                // re-runs this, and an identical assignment re-renders the list.
                if merge.rows != page.value { page.value = merge.rows }
                page.error = nil
            }
            if merge.reset {
                // Exhausted only when a MAILBOX-bounded head came back short
                // (that is the whole mailbox), or on a daemon that cannot page
                // by arrival and has nothing past its window.
                mailPagingState[mode] = MailPaging(
                    nextCursor: fetched.next_cursor,
                    exhausted: !full && (bounded || !keyset))
            } else if full, mailPaging(mode).exhausted, keyset {
                // The mailbox grew past one head: there is more again.
                mailPagingState[mode, default: MailPaging()].exhausted = false
            }
            mailLoadedAt[mode] = Date()
            // Every row is a message, so this is where a reply landing in a
            // cached thread is noticed, whichever row it is.
            ThreadPrefetch.shared.retire(fetched.items.lazy.map { ($0.thread_id, $0.id) })
            // Pull the head rows' threads before any click, so an open renders
            // from cache. Runs on the launch warm too, not just on a visit.
            // Mail still to do first: it is what gets opened. Runs every poll
            // tick on the page, and asks only for threads not already cached.
            ThreadPrefetch.shared.warm(
                merge.rows.lazy.filter { $0.status != .done }
                    .prefix(Self.mailWarmRows).map(\.thread_id), immediate: 5)
        } catch {
            guard e == epoch else { return }
            withMailPage(mode) { $0.error = errText(error, "load failed") }
        }
        withMailPage(mode) { $0.isLoading = false }
    }

    /// Mutate one page in place. Every call is its own access, which is the point:
    /// a local copy held across the fetch would write back over a `removeFromMail`
    /// that landed while the request was in flight.
    private func withMailPage(
        _ mode: MailMode, _ mutate: (inout Loadable<[AttentionUpdate]>) -> Void
    ) {
        let before = mailPages[mode] ?? .loading
        var page = before
        mutate(&page)
        // Written back only when the mutation changed something: the poll
        // comes through here three times a tick, and an identical write is a
        // re-render of every row.
        if page != before { mailPages[mode] = page }
    }

    // MARK: - the sent page

    /// The sent page's rows, in their OWN cache rather than a third `mailPages`
    /// entry. `SentItem` is a different fact: recipients instead of a sender,
    /// opens instead of a tier, and no status at all — forcing it into
    /// `AttentionUpdate` would mean inventing triage answers for mail nothing
    /// triaged. It lives in the store for the same reason the other two pages
    /// do: `@State` dies on navigate-away, and this list would refetch from
    /// blank on every visit.
    private var sentRows: Loadable<[SentItem]> = .loading
    /// Freshness + in-flight fetch, the sent half of `mailLoadedAt`/`mailRefreshes`.
    private var sentLoadedAt: Date?
    private var sentRefresh: Task<Void, Never>?

    var sentPage: Loadable<[SentItem]> { sentRows }

    /// THE WHOLE SENT PAGE: the sent listing and the outbox pinned above it,
    /// side by side, so one refresh fetches both (docs/SCHEDULED-SEND.md §6).
    func refreshSent(force: Bool = false) async {
        async let outbox: Void = refreshScheduled(force: force)
        await refreshSentMail(force: force)
        await outbox
    }

    /// Same TTL-plus-join shape as `refreshMail`, for the same reasons: a
    /// revisit inside the window is free, and a mount landing on the same tick
    /// as the poll costs one fetch rather than two.
    private func refreshSentMail(force: Bool = false) async {
        if !force, let loadedAt = sentLoadedAt,
            Date().timeIntervalSince(loadedAt) < Self.mailTTL
        {
            return
        }
        if let running = sentRefresh {
            await running.value
            if !force { return }
        }
        let refresh = Task { await performSentRefresh() }
        sentRefresh = refresh
        await refresh.value
        if sentRefresh == refresh { sentRefresh = nil }
    }

    private func performSentRefresh() async {
        let e = epoch
        if sentRows.value == nil { sentRows.isLoading = true }
        do {
            let page = try await APIClient.shared.listSent(limit: Self.mailLimit)
            // The rows belong to the account that asked for them; the wipe
            // has already put the page back to loading for the one after.
            guard e == epoch else { return }
            // THE SERVER'S ORDER IS THE ORDER (received_at DESC, id DESC): no
            // client-side sort, and no `resolvedIds` subtraction either — sent
            // mail is a record of what went out, and none of the triage verbs
            // can take a row off it.
            //
            // Assigned only on a real change, like the mail pages: the poll
            // re-runs this, and an identical assignment re-renders every row.
            if page.items != sentRows.value { sentRows.value = page.items }
            if sentRows.error != nil { sentRows.error = nil }
            sentLoadedAt = Date()
            // A reply sent from another client is a new row here, and the
            // thread's copy predates it.
            ThreadPrefetch.shared.retire(page.items.lazy.map { ($0.thread_id, $0.id) })
            ThreadPrefetch.shared.warm(
                page.items.prefix(Self.mailWarmRows).map(\.thread_id), immediate: 5)
        } catch {
            guard e == epoch else { return }
            sentRows.error = errText(error, "load failed")
        }
        if sentRows.isLoading { sentRows.isLoading = false }
    }

    // MARK: - the pending-reminder filter

    /// Whether the all-mail page is narrowed to mail with a reminder waiting.
    /// A FILTER, not a fourth MailMode: the segments are three pages of mail,
    /// and this is a lens over one of them that the Escape ladder sheds first.
    var reminderFilter = false

    /// The all-mail page's other lenses (important / deadlines / attachments).
    /// Separate from `reminderFilter` because that one swaps the DATA SOURCE
    /// and these only narrow the rows already on screen; the filter bar keeps
    /// the two mutually exclusive.
    var mailFilter: MailFilter = .all

    // MARK: - the provider's spam folder

    /// When this session last ASKED the daemon to fetch the spam folder, nil
    /// until it has. Paired with `stats.spam_synced_at` (when the daemon last
    /// FINISHED one) it is the whole state machine for the spam page's copy:
    /// a request newer than the stamp means a fetch is outstanding.
    private var spamSyncRequestedAt: Date?

    /// How long a spam fetch may be outstanding before the page stops saying it
    /// is still checking and admits it did not work.
    ///
    /// The daemon stamps `spam_synced_at` only on SUCCESS, which is what makes
    /// the stamp trustworthy and also what makes this cap necessary: a failed
    /// fetch moves nothing, so without a deadline the page would spin forever
    /// on a revoked credential. Two minutes is well past a capped fetch of a
    /// few hundred messages and well short of a user's patience.
    private static let spamSyncDeadline: TimeInterval = 120

    /// Whether the daemon is (as far as this client can tell) still fetching.
    var spamFetchInFlight: Bool {
        guard let asked = spamSyncRequestedAt else { return false }
        if Date().timeIntervalSince(asked) > Self.spamSyncDeadline { return false }
        return !spamSyncLanded(since: asked)
    }

    /// Whether a fetch was asked for, ran out its deadline, and never landed.
    /// The page says so rather than showing an empty list as though it were an
    /// answer.
    var spamFetchStalled: Bool {
        guard let asked = spamSyncRequestedAt else { return false }
        guard Date().timeIntervalSince(asked) > Self.spamSyncDeadline else { return false }
        return !spamSyncLanded(since: asked)
    }

    /// Whether the folder has EVER been fetched. Nil stamp means the daemon has
    /// not looked, which is the normal state — it does not track that folder.
    var spamEverSynced: Bool { stats?.spam_synced_at != nil }

    private func spamSyncLanded(since: Date) -> Bool {
        guard let stamp = stats?.spam_synced_at, let at = Fmt.date(stamp) else {
            return false
        }
        return at >= since
    }

    /// Whether to offer the spam door at all.
    ///
    /// ALWAYS, ONCE THE DAEMON CAN ANSWER — which is a change forced by making
    /// the fetch lazy. The chip used to be gated on `spam > 0`, and that count
    /// is now zero until somebody opens the page, which is only reachable
    /// through the chip: a door that appears once you have been through it.
    ///
    /// So the rule is the one Gmail itself uses: the folder is a place that
    /// exists whether or not anything is in it, and the door to it is always
    /// there. The count rides along only when there is one. The single
    /// condition left is that the daemon is new enough to have the field at
    /// all — `nil` means it cannot serve the page, and offering a door to a
    /// 400 would be worse than not offering one.
    var spamDoorVisible: Bool { spamCount != nil }

    /// Ask the daemon to go and fetch the spam folder. Called when the page is
    /// OPENED, not on the recurring poll: the daemon does not track that folder,
    /// so this is a real round trip to Gmail and it should cost one per visit
    /// rather than one every ten seconds.
    func refreshSpamFolder() async {
        let asked = Date()
        do {
            try await APIClient.shared.refreshSpam()
            spamSyncRequestedAt = asked
        } catch {
            // A daemon too old for the route, or an unreachable one. Leave the
            // request unrecorded so the page does not claim to be checking
            // something nobody was asked to check; the ordinary refresh error
            // banner already covers an unreachable daemon.
            spamSyncRequestedAt = nil
        }
    }

    /// Parked mail, in its OWN cache for the same reason the sent page has one:
    /// these rows are all `done`, so `refreshMail`'s pages — which every list
    /// filters through `resolvedIds` — would hide every one of them the moment
    /// it was set. Ordered by the DAEMON (remind_at ASC, soonest first), and
    /// never re-sorted here.
    private var reminderRows: Loadable<[AttentionUpdate]> = .loading
    private var remindersLoadedAt: Date?
    private var remindersRefresh: Task<Void, Never>?

    /// Counts every optimistic write to the list above. A GET that left before
    /// one landed is answering a question about a list that no longer exists,
    /// and its rows are OLDER than the local ones: without this, cancelling a
    /// reminder while the poll's fetch is in flight puts the row back on screen
    /// for the rest of the tick, which reads as the cancel not working.
    private var remindersMutations = 0

    var remindersPage: Loadable<[AttentionUpdate]> { reminderRows }

    /// Same TTL-plus-join shape as `refreshMail` and `refreshSent`.
    func refreshReminders(force: Bool = false) async {
        if !force, let loadedAt = remindersLoadedAt,
            Date().timeIntervalSince(loadedAt) < Self.mailTTL
        {
            return
        }
        if let running = remindersRefresh {
            await running.value
            if !force { return }
        }
        let refresh = Task { await performRemindersRefresh() }
        remindersRefresh = refresh
        await refresh.value
        if remindersRefresh == refresh { remindersRefresh = nil }
    }

    private func performRemindersRefresh() async {
        let e = epoch
        let mutations = remindersMutations
        if reminderRows.value == nil { reminderRows.isLoading = true }
        do {
            // `peek` because this page is a REVIEW of things already dealt
            // with: showing a parked row is not the app surfacing that mail,
            // and stamping the seen-ledger here would spend the one "new"
            // state the row still has left for when the reminder fires.
            let page = try await APIClient.shared.getUpdates(
                UpdatesParams(limit: Self.mailLimit, remindersPending: true), peek: true)
            // The rows belong to the account that asked for them. Both exits
            // below return rather than fall through, so the `isLoading` write
            // at the bottom is only ever reached by the live epoch: an account
            // switch mid-fetch would otherwise park A's rows in B's lens, where
            // Backspace deletes a reminder on B's daemon by colliding id.
            guard e == epoch else { return }
            // And to the list as it stood when the fetch left. A cancel that
            // landed while it was in flight is the newer truth of the two.
            guard mutations == remindersMutations else {
                reminderRows.isLoading = false
                return
            }
            if page.items != reminderRows.value { reminderRows.value = page.items }
            if reminderRows.error != nil { reminderRows.error = nil }
            remindersLoadedAt = Date()
        } catch {
            guard e == epoch else { return }
            reminderRows.error = errText(error, "load failed")
        }
        if reminderRows.isLoading { reminderRows.isLoading = false }
    }

    /// Land a reminder on the local rows without waiting for a round trip: the
    /// row joins (or moves within) the parked list at its new place in the
    /// queue, and leaves every surface that shows unfinished mail.
    ///
    /// The insert keeps the server's own order — remind_at ascending — so the
    /// optimistic list and the next fetch agree on where the row sits.
    func noteReminder(_ update: AttentionUpdate, remindAt: String) {
        remindersMutations &+= 1
        var row = update
        row.remind_at = remindAt
        row.reminded_at = nil
        row.status = .done
        var rows = (reminderRows.value ?? []).filter { $0.id != update.id }
        let due = Fmt.date(remindAt) ?? .distantFuture
        let at = rows.firstIndex { (Fmt.date($0.remind_at) ?? .distantFuture) > due } ?? rows.count
        rows.insert(row, at: at)
        reminderRows.value = rows
    }

    /// Drop a row from the parked list — a cancelled reminder, or an undone one.
    func dropReminder(_ messageId: Int) {
        remindersMutations &+= 1
        reminderRows.value?.removeAll { $0.id == messageId }
    }

    /// Make the next read of the parked list go to the daemon. For the caller
    /// that changed a reminder without holding the row it changed.
    func invalidateReminders() { remindersLoadedAt = nil }

    /// The cached row for a message id, wherever it currently sits. A lookup
    /// rather than a parameter because the surfaces that act on a message do not
    /// all hold one: the reader has `ClientMessage`s, and a thread opened from
    /// search has no queue behind it at all. nil means genuinely uncached, which
    /// callers treat as "ask the daemon", never as "no such mail".
    func update(id: Int) -> AttentionUpdate? {
        for band in [sitrep.standing, sitrep.new, sitrep.open] {
            if let hit = band.first(where: { $0.id == id }) { return hit }
        }
        for mode in MailMode.allCases {
            if let hit = mailPage(mode).value?.first(where: { $0.id == id }) { return hit }
        }
        return reminderRows.value?.first { $0.id == id }
    }

    /// Preload the emails behind the records the columns show, so clicking one
    /// opens instantly.
    private func warmZoneThreads() {
        ThreadPrefetch.shared.warm(zones.calendar.prefix(6).compactMap(\.thread_id), immediate: 2)
        ThreadPrefetch.shared.warm(zones.banking.prefix(6).compactMap(\.thread_id), immediate: 2)
        ThreadPrefetch.shared.warm(zones.shipments.prefix(6).compactMap(\.thread_id), immediate: 2)
        // Receipts rotate at local midnight, so match the cache TTL to that.
        let midnight =
            Calendar.current.nextDate(
                after: Date(), matching: DateComponents(hour: 0, minute: 0),
                matchingPolicy: .nextTime) ?? Date().addingTimeInterval(3600)
        let ttl = max(60, midnight.timeIntervalSinceNow)
        for id in zones.receipts.compactMap(\.thread_id) {
            ThreadPrefetch.shared.prefetch(id, fresh: ttl)
        }
    }

    func openSide(_ view: SideView) { sideView = view }

    /// Close the panel, and HOLD the deeper-search lane rather than losing it
    /// (docs/SEARCH.md §6.3). Nothing is torn down: the in-flight model turn
    /// finishes, no tool runs after it, and the conversation sits with its
    /// transcript and cards until the panel is opened again. A pause on an idle
    /// session is a no-op, so this is safe to call unconditionally.
    func closeSide() {
        if preparingSearchEvidence { resetSearchLane(keepingVerdict: true) }
        searchLane.pause()
        sideView = .none
    }

    /// Bumped by every `openSearch`. The Mac answers that call with
    /// `sideView`; the phone's shell answers it with a tab, and needs to hear
    /// the call itself, since a repeat of the same seed changes nothing else.
    private(set) var searchAsks = 0

    /// Open search. By default it RESUMES the last one; `seed` forces a fresh
    /// term (`f` on a row or in the reader, seeding `from:<address>`).
    func openSearch(seed: String? = nil) {
        searchAsks &+= 1
        // Give wide search the window; closing the reader saves its draft.
        if threadId != nil { closeThread() }
        // A seed matching what is ALREADY fetched keeps the session: the hits
        // on screen are authoritative for exactly that term, and nilling
        // `fetchedQuery` here would not refetch anyway (the panel's task is
        // keyed on `query`, which does not change) — it would just blank the
        // highlights and kill the cursor under live results.
        if let seed, seed != search.fetchedQuery {
            search.query = seed
            search.index = -1
            // Nil BOTH: the fetched term is what gates the refetch, and a cursor
            // from the old term would page a search that is no longer on screen.
            search.fetchedQuery = nil
            search.diagnostics = nil
            search.nextCursor = nil
            // A SEED IS A DIFFERENT SEARCH, not a narrowing of the one being
            // held: `f` on a row asks "what else is from this person", which
            // has nothing to do with the question the lane was answering. So
            // the conversation goes rather than resuming (docs/SEARCH.md §6.3).
            resetSearchLane()
        } else {
            // Restore the field text in case the bar was edited since the fetch
            // (a seed that matches what is already on screen is the same
            // search, so it keeps everything, the lane included).
            if let seed { search.query = seed }
            // The same search, coming back: pick the lane up at the boundary it
            // was holding at.
            searchLane.resume()
        }
        // Search starts wide; the strip remains available beside an open email.
        search.expanded = true
        sideView = .search
        searchFocusRequest &+= 1
    }

    /// The band can say one line; the chat can say everything. Offered once
    /// the lane has answered and while the ⌘K chat is not mid-run, since the
    /// handoff replaces whatever the chat was holding.
    var canExpandSearchLane: Bool {
        !searchLane.running && !preparingSearchEvidence && !assistant.running
            && !searchLane.transcript.isEmpty
    }

    /// CONTINUE THE DEEPER SEARCH IN ⌘K. The chat takes a copy of the lane's
    /// conversation, so a follow-up there is a real follow-up, and the search
    /// panel stays as it was underneath for Esc to come back to.
    func expandSearchLaneToChat() {
        // ALREADY THERE: the chat holds this search as it stands, maybe with
        // follow-ups on top. Copying it again would wipe those, so just show it.
        if chatHoldsSearchLane {
            askBarOpen = true
            return
        }
        guard canExpandSearchLane else { return }
        adoptSearchLaneIntoChat()
        askBarOpen = true
    }

    /// The ⌘K chat is this search's conversation, taken over since the lane
    /// last said anything.
    var chatHoldsSearchLane: Bool {
        assistant.adoptedThrough != nil && assistant.adoptedThrough == searchLane.itemMark
    }

    /// Whether opening the lane in chat can do anything right now. The one
    /// refusal is a busy chat holding some OTHER conversation.
    var canOpenSearchLaneInChat: Bool {
        chatHoldsSearchLane || !assistant.running
    }

    /// Whether ⌘↩ in search means "open what the lane said" rather than "ask".
    var searchLaneHasConversation: Bool {
        search.laneStarted && !searchLane.transcript.isEmpty
    }

    /// ⌘↩ WITH A CONVERSATION ALREADY GOING: show it rather than start another.
    /// Idle, it is the expand button. Still answering, the bar watches the lane
    /// and takes it over when the run ends. A busy ⌘K chat is left alone, since
    /// either route would end with this replacing it mid-answer.
    func openSearchLaneInChat() {
        guard searchLaneHasConversation, canOpenSearchLaneInChat else { return }
        if searchLane.running && !chatHoldsSearchLane {
            askBarOpen = true
            askBarWatchesLane = true
        } else {
            expandSearchLaneToChat()
        }
    }

    /// The watched lane has finished: the chat takes the conversation over.
    /// Called by the bar whenever the lane's run state settles, so a no-op
    /// unless it is watching and the run really is over.
    func finishLaneHandoff() {
        guard askBarWatchesLane, !searchLane.running else { return }
        askBarWatchesLane = false
        adoptSearchLaneIntoChat()
    }

    /// THE CHAT TAKES THE LANE'S CONVERSATION, and with it the question that
    /// conversation answers, so the follow-ups asked here are what tapping
    /// that question in history brings back. Every handoff goes through here
    /// (both of the Mac's and the phone's "continue in chat"), so none can move
    /// the conversation and leave its question behind.
    ///
    /// THE CHAT'S SAVED COPY STILL WINS (the rule `saveQuestionChat` keeps).
    /// If the chat was already the conversation behind this same question
    /// before adopting, its follow-ups are what history saved for it, and the
    /// lane's copy (the lane narrowed on underneath since) is not written over
    /// them ON DISK. The LIVE chat does hold the lane's copy now (`adopt`
    /// replaces it, as it always has), so tapping the question in history this
    /// session opens that, and the chat's next run end saves it over the old
    /// follow-ups: asking "continue in chat" again is the reader choosing the
    /// newer conversation. What this rule spares is only the saved copy, in
    /// case nothing more is asked.
    func adoptSearchLaneIntoChat() {
        let heldSameQuestion = chatQuestionKey != nil && chatQuestionKey == laneQuestionKey
        let before = assistant.conversationId
        assistant.adopt(searchLane)
        // `adopt` refuses while either side is running; nothing moved then.
        guard assistant.conversationId != before else { return }
        chatQuestion = laneQuestionKey.map {
            QuestionTag(key: $0, conversation: assistant.conversationId)
        }
        guard !heldSameQuestion else { return }
        saveQuestionChat(from: assistant)
    }

    // MARK: - question chats

    /// A conversation's claim to be the chat behind a question in search
    /// history: the question's key, and which conversation the session was
    /// holding when the claim was made. A session that has since been cleared
    /// (a new chat, a lane reset or restarting itself, an account switch) holds
    /// a different conversation, so the claim lapses on its own and no path
    /// that replaces a conversation has to remember to drop it.
    private struct QuestionTag {
        var key: String
        var conversation: Int
    }

    private var laneQuestion: QuestionTag?
    private var chatQuestion: QuestionTag?

    private var laneQuestionKey: String? {
        guard let tag = laneQuestion, tag.conversation == searchLane.conversationId else {
            return nil
        }
        return tag.key
    }

    private var chatQuestionKey: String? {
        guard let tag = chatQuestion, tag.conversation == assistant.conversationId else {
            return nil
        }
        return tag.key
    }

    /// A run has ended in one of the two sessions: keep its conversation as
    /// the chat behind its question, if it has one. THE CHAT WINS: once the
    /// reader has taken a question's conversation into the chat, that is the
    /// copy tapping the question opens, and the lane narrowing on underneath
    /// does not overwrite it.
    private func saveQuestionChat(from session: AssistantSession) {
        let key: String?
        if session === assistant {
            key = chatQuestionKey
        } else if session === searchLane {
            let lane = laneQuestionKey
            key = lane == chatQuestionKey ? nil : lane
        } else {
            key = nil
        }
        guard let key, let snapshot = session.snapshot() else { return }
        QuestionChats.shared.save(snapshot, forKey: key)
    }

    /// A QUESTION TAPPED IN SEARCH HISTORY: bring back the chat it had, in the
    /// ⌘K chat, rather than ask it all over again. True means the chat now
    /// holds that conversation and the caller should show it (the Mac's bar,
    /// the phone's push); false means there was nothing to bring back and the
    /// question has been asked again instead (`askAgainFromHistory`).
    ///
    /// A CHAT MID-ANSWER IS NEVER REPLACED. If it is answering this very
    /// question it is simply shown; if it is busy with something else, the
    /// question is asked again in search, which leaves that run alone (the
    /// same refusal the lane's "continue in chat" makes).
    func openQuestionFromHistory(_ text: String) -> Bool {
        let key = QuestionChats.key(forQuestion: text)
        if chatQuestionKey == key {
            askBarWatchesLane = false
            RecentSearchStore.shared.rememberQuestion(text)
            return true
        }
        // A snapshot the chat cannot take up (see `canRestore`: a tool it no
        // longer offers) is as good as none: the question is asked again.
        guard !assistant.running, let snapshot = QuestionChats.shared.snapshot(forKey: key),
            assistant.canRestore(snapshot)
        else {
            askAgainFromHistory(text)
            return false
        }
        restoreQuestionChat(snapshot, key: key)
        // Opened again is asked again, as far as the ring's order goes.
        RecentSearchStore.shared.rememberQuestion(text)
        return true
    }

    private func restoreQuestionChat(_ snapshot: ConversationSnapshot, key: String) {
        askBarWatchesLane = false
        assistant.restore(snapshot)
        chatQuestion = QuestionTag(key: key, conversation: assistant.conversationId)
    }


    /// ⌘⇧K and the bar's own button. Ends a watch first, so "new chat" can
    /// never clear the search lane it happened to be showing.
    func newAskChat() {
        askBarWatchesLane = false
        assistant.clear()
        askBarOpen = true
    }

    private var searchEvidenceTask: Task<Void, Never>?
    var preparingSearchEvidence: Bool { searchEvidenceTask != nil }

    /// A question from search history, asked again: its words go back in the
    /// field and to the agent. With deeper search off it is simply searched.
    func askAgainFromHistory(_ text: String) {
        search.query = text
        search.index = -1
        requestDeeperSearch()
    }

    /// Run the words currently in the field, even before keyword results arrive.
    func requestDeeperSearch() {
        if searchLaneHasConversation {
            openSearchLaneInChat()
            return
        }
        guard DeeperSearchPolicy.canRequest(query: search.query,
            choice: Prefs.shared.deeperSearch, running: searchLane.running || preparingSearchEvidence) else { return }
        resetSearchLane(keepingVerdict: true)
        startDeeperSearch(trigger: .requested)
    }

    /// START THE DEEPER SEARCH on the query the panel has just answered, with
    /// the hits it answered with. Called by the classifier's automatic path and
    /// by the band's own button, so the two cannot disagree about what starting
    /// means (docs/SEARCH.md §6.1, §6.5).
    ///
    /// The event carries the trigger and the model tier and NOTHING ELSE: the
    /// query is the reader's own words about their own mail, and no part of it,
    /// nor of what the search found, goes near telemetry.
    func startDeeperSearch(trigger: SearchIntent.Trigger) {
        let query = trigger == .requested ? search.query.trimmed : (search.fetchedQuery ?? search.query.trimmed)
        guard Prefs.shared.deeperSearch != .off, !query.isEmpty, !search.laneStarted else { return }
        search.laneStarted = true
        search.laneTrigger = trigger
        search.laneQuery = query
        search.refinementCount = 0
        captureLaneStart(trigger)
        // Both entry points await authorized keyword evidence. Human panel hits
        // may include restricted mail and cannot be forwarded to the agent.
        let evidenceEpoch = epoch
        searchEvidenceTask = Task { [weak self] in
            guard let self else { return }
            do {
                var term = query
                while !Task.isCancelled {
                    let page = try await APIClient.shared.search(term, limit: 50,
                        mode: .keyword, partial: true, forAgent: true)
                    try Task.checkCancellation()
                    guard self.epoch == evidenceEpoch else { return }
                    let current = self.search.query.trimmed
                    guard !current.isEmpty, Prefs.shared.deeperSearch != .off else {
                        self.resetSearchLane(keepingVerdict: true)
                        return
                    }
                    if term != current { term = current; continue }
                    self.search.laneQuery = term
                    self.searchLane.sendAuthorizedSearch(term, hits: page.items)
                    // The conversation's first message is a line of search
                    // history, drawn as a question (both shells), and this
                    // conversation is the chat that line brings back.
                    RecentSearchStore.shared.rememberQuestion(term)
                    self.laneQuestion = QuestionTag(
                        key: QuestionChats.key(forQuestion: term),
                        conversation: self.searchLane.conversationId)
                    self.searchEvidenceTask = nil
                    return
                }
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled, self.epoch == evidenceEpoch else { return }
                self.searchEvidenceTask = nil
                self.search.laneStarted = false
                self.search.error = "Could not load search evidence: \(error.localizedDescription)"
            }
        }
    }

    /// ONE CONVERSATION, ONE EVENT, from the two places a conversation begins:
    /// a query the classifier started a lane for, and the restart past the
    /// twelfth narrowing. Written once so the second cannot quietly stop
    /// counting, which is how a lane that restarts itself all afternoon reads
    /// as one search in the funnel.
    ///
    /// The trigger is OPTIONAL because a restart's own query need not be
    /// question-shaped: somebody who has narrowed twelve times may be down to
    /// two plain words. Absent rather than invented, and rather than the stale
    /// trigger of the conversation that just ended: the vocabulary is closed
    /// and names signals, and "no signal, they just kept typing" is not one.
    private func captureLaneStart(_ trigger: SearchIntent.Trigger?) {
        var properties: [String: Any] = [
            "model": Prefs.shared.searchLaneModel.shortLabel.lowercased()
        ]
        if let trigger { properties["trigger"] = trigger.analyticsValue }
        Analytics.capture("search_deeper_started", properties)
    }

    /// EVERY SETTLED QUERY AFTER THE FIRST is the same need narrowed, whatever
    /// the classifier makes of it on its own: somebody who typed a question and
    /// then deleted a word has not stopped asking the question. So this is
    /// called for every settled query once the lane is going, and the session
    /// decides whether it lands at a boundary or as the next turn.
    func refineDeeperSearch() {
        guard search.laneStarted, searchEvidenceTask == nil else { return }
        let query = search.fetchedQuery ?? search.query.trimmed
        guard !query.isEmpty else { return }
        // The session says which of the three things it did, because from out
        // here a restart and an untouched conversation look identical: both
        // leave the narrowing count at zero.
        switch searchLane.refine(query, hits: search.hits) {
        case .none, .duplicate:
            // The same words the model already has (the sort flipped, a failed
            // search was retried). Nothing narrowed, so nothing here moves
            // either — least of all the count, which would otherwise walk to
            // thirteen on queries nobody typed.
            return
        case .queued:
            search.refinementCount = searchLane.refinementCount
        case .restart:
            // A DIFFERENT SEARCH, past the twelfth narrowing: the session threw
            // the conversation away and opened a new one on these words. So
            // everything that describes a conversation starts over with it. The
            // trigger is re-derived from the verdict for THESE words, because
            // the one on file was the signal that started a conversation which
            // no longer exists, and the band prints it as the reason the lane is
            // running. And it counts as a start, or the funnel hears about the
            // first search of an afternoon and none of the rest.
            search.laneTrigger = search.lastVerdict?.trigger
            search.laneQuery = query
            search.refinementCount = searchLane.refinementCount
            captureLaneStart(search.laneTrigger)
            // A new conversation is a new line of history, with its own chat,
            // exactly as `startDeeperSearch` records the first one.
            RecentSearchStore.shared.rememberQuestion(query)
            laneQuestion = QuestionTag(
                key: QuestionChats.key(forQuestion: query),
                conversation: searchLane.conversationId)
        }
    }

    /// Tear the deeper-search conversation down and forget that one ever ran
    /// for this panel session. The band's "new" control and a seeded search
    /// both land here: one door, because the session and the values that
    /// describe it must never disagree about whether a lane is running.
    ///
    /// `keepingVerdict` is the difference between the two callers. The band's
    /// "new" is about the CONVERSATION, not about the query: the words in the
    /// bar are still the same question, so the verdict stays and the band stays
    /// with it, offering to run again. A seed is a different search altogether
    /// (`f` asks "what else is from this person"), so its verdict goes too and
    /// the band unmounts until the new query has been judged.
    func resetSearchLane(keepingVerdict: Bool = false) {
        searchEvidenceTask?.cancel()
        searchEvidenceTask = nil
        searchLane.clear()
        search.laneStarted = false
        search.laneTrigger = nil
        search.laneQuery = ""
        if !keepingVerdict { search.lastVerdict = nil }
        search.refinementCount = 0
    }

    func openCompose(_ state: ComposeState) {
        // ONE PANE AT A TIME: the composer and the calendar's event pane
        // claim the same half of the window (`yieldEventPaneToCompose`).
        guard yieldEventPaneToCompose() else { return }
        var next = state
        // Seeded HERE rather than by the caller, so every way into a composer
        // starts at the account's default and none can forget to.
        next.includeTracker = trackingDefault
        // The signature, below where typing starts. Only into an empty body — a
        // caller arriving with content composed that content, not a signature.
        if next.body.isEmpty { next.body = Prefs.shared.signatureSeed }
        // A composer with NO PARENT addresses itself: there is nothing for the
        // daemon to derive an audience from, so its three fields are the whole
        // answer from the first keystroke.
        //
        // A REPLY opened into this pane — the agent's hand-off — keeps deriving,
        // UNLESS it arrived carrying a copy list. That case is the hand-off of a
        // send the model addressed and the person read on the confirm card, and
        // leaving it deriving would drop those addresses on the floor between
        // the card and the composer.
        if next.replyToMessageId == nil || !next.cc.isEmpty || !next.bcc.isEmpty {
            next.recipientsStated = true
        }
        // Whatever it opened with is what "untouched" means for the autosave.
        next.seededRecipients = next.recipients
        // The side panel and the composer both claim the window's right-hand
        // strip, and the panel draws ABOVE the composer: left open, the two
        // stack in one place and the reader insets past both. Writing wins, so
        // the panel goes (held, not lost: see `closeSide`).
        if sideView.isOpen { closeSide() }
        compose = next
        DraftSaver.shared.noteOpened(.compose)
        Analytics.capture("compose_opened", ["kind": next.analyticsKind])
    }

    /// A mailto: link clicked while a composer with something in it was up:
    /// the question of whether to put that one away and start this one.
    /// Drawn by `ActionLayer` (Mac) and the pane's own dialog (phone), like
    /// `discardAsk`. Tied to the composer it asked about by id.
    struct MailtoAsk: Equatable {
        let link: Mailto
        let composerId: UUID
        /// False only for a forward an older daemon cannot keep, which the
        /// question then has to own up to.
        let keepsDraft: Bool
    }
    var mailtoAsk: MailtoAsk?

    /// The ask, only while the composer it asked about is still the one up.
    /// Every reader goes through this: an ask left behind by a composer an
    /// agent's draft replaced must neither draw a card nor hold keys.
    var liveMailtoAsk: MailtoAsk? {
        guard let ask = mailtoAsk, ask.composerId == compose?.id else { return nil }
        return ask
    }

    /// A mailto: link clicked in a message: a new composer, addressed and
    /// titled as the link asks. Nothing restores into it: the link said who
    /// to write to, and an unrelated half-written draft would say otherwise.
    ///
    /// A composer already up with nothing in it is simply replaced. One with
    /// writing in it is never blanked out by a click: the person is asked
    /// first, and a yes keeps it in drafts the way Esc does.
    func openComposeMailto(_ url: String) {
        // A new click is a new question; an older one left behind is dropped.
        mailtoAsk = nil
        guard let link = Mailto(url) else { return }
        guard let open = compose else {
            openMailto(link)
            return
        }
        // A send in flight owns its composer; there is nothing to put away.
        guard !open.sending else { return }
        // ASK ONLY ABOUT WRITING: what the person typed, not what a composer
        // opened holding. A composer seeded by an earlier link (or restored
        // from a draft) and left alone is not "the one you're writing", and
        // its close saves nothing a question could promise. Put away through
        // `closeCompose` all the same, so a typed-then-cleared composer's
        // pending blank save still deletes its row.
        let writing =
            (DraftSaver.shared.isTouched(.compose) && DraftSaver.hasContent(open))
            || !open.attachments.isEmpty
        guard writing else {
            closeCompose()
            openMailto(link)
            return
        }
        // The card has no field, so the caret would stay in the mail under it
        // and keys would edit it blind (`askDiscardCompose` does the same).
        #if os(macOS)
            for window in NSApp.windows where window.firstResponder is NSText {
                window.makeFirstResponder(nil)
            }
        #endif
        // A reply keeps its own row, keyed by its parent, on any daemon. New
        // mail and forwards need `drafts_v2`: an older daemon has ONE
        // new-message slot, which the link's composer would save over.
        mailtoAsk = MailtoAsk(
            link: link, composerId: open.id,
            keepsDraft: open.replyToMessageId != nil || draftsV2Available)
    }

    func cancelMailtoAsk() {
        mailtoAsk = nil
    }

    /// Confirmed: the open composer goes to drafts (an Esc, toast and all)
    /// and the link's composer takes its place. Only for the composer the
    /// question was about; one replaced or sent meanwhile agreed to nothing.
    func confirmMailtoAsk() {
        guard let ask = mailtoAsk else { return }
        mailtoAsk = nil
        // The composer asked about went away on its own: nothing left to put
        // away, and the person did say yes to the link.
        guard let open = compose else {
            openMailto(ask.link)
            return
        }
        guard open.id == ask.composerId, !open.sending else { return }
        closeCompose()
        openMailto(ask.link)
    }

    private func openMailto(_ link: Mailto) {
        var state = ComposeState()
        state.to = link.to.joined(separator: ", ")
        state.cc = link.cc.joined(separator: ", ")
        state.bcc = link.bcc.joined(separator: ", ")
        state.subject = link.subject
        // The link's body is a suggestion the sender of the mail wrote, so the
        // signature still goes under it (`openCompose` only seeds an EMPTY one).
        if !link.body.isEmpty { state.body = link.body + Prefs.shared.signatureSeed }
        openCompose(state)
    }

    /// `c` / ⌘N — the new-message composer, RESTORED, but only from the recent
    /// past. A `drafts_v2` daemon holds any number of new-message drafts, so
    /// "the" draft to restore is the most recently edited one, and only when
    /// that edit was within `restoreWindow`: a ⌘N minutes after an Esc is
    /// picking up where you left off, a ⌘N tomorrow is a new mail. Older drafts
    /// live on the pane's drafts page. An older daemon keeps its one slot and
    /// today's rule.
    ///
    /// THE LANDING RULE, on a `drafts_v2` daemon: ⌘N ALWAYS OPENS THE
    /// COMPOSER. A plain new draft edited in the last five minutes is
    /// restored into it a round-trip later (see `restoreNewMessage`); older
    /// drafts are only NOTED, by the header's "N drafts" control, never
    /// opened onto. An earlier cut landed a ⌘N with older drafts waiting on
    /// the drafts page instead; the user asked for the composer every time
    /// (2026-09-29): a key that sometimes opens a list is a key you cannot
    /// type into blind.
    ///
    /// The restore only ever fills the untouched blank composer this call
    /// opened, and only while it is still on the composer page: typing into
    /// it, or toggling to the list, before the fetch lands wins.
    ///
    /// A SECOND ⌘N while the pane is on its drafts page flips it back to the
    /// composer, as it is. On the composer page a repeat does nothing.
    func openComposeNew() {
        // A composer already up is not something a repeated `c` — or the menu
        // item behind ⌘N — gets to blank out. Same rule as the inline reply.
        // Except for the page it is on: ⌘N from the drafts list means the
        // composer, and the header's back control says so.
        if var open = compose {
            if open.page == .drafts {
                open.page = .compose
                compose = open
            }
            return
        }
        // Asked here as well as in `openCompose`: a composer the event pane
        // refused must not go looking for a draft to restore into nothing.
        guard yieldEventPaneToCompose() else { return }
        let state = ComposeState()
        openCompose(state)
        let opened = state.page
        let openedId = state.id
        Task { await restoreNewMessage(openedOn: opened, composerId: openedId) }
    }

    /// The newest plain new-message draft edited within `restoreWindow`, not
    /// owned by a held send and not waiting out a delete: rule 1's candidate.
    /// `drafts_v2` only; the one-slot rule lives in `restoreNewMessage`.
    private func recentNewDraft(in rows: [DraftView]) -> DraftView? {
        // Plain new mail only (a reply or forward row belongs to its parent),
        // never one a held send owns or one whose delete is in its undo
        // window, and the NEWEST edit of what is left.
        let candidates = rows.filter {
            $0.reply_to_message_id == nil && $0.forward_of_message_id == nil
                && !isHeld($0) && !isDeleting($0.id)
        }
        guard
            let newest = candidates.max(by: {
                (Fmt.date($0.updated_at) ?? .distantPast)
                    < (Fmt.date($1.updated_at) ?? .distantPast)
            }),
            let edited = Fmt.date(newest.updated_at),
            Date().timeIntervalSince(edited) <= Self.restoreWindow
        else { return nil }
        return newest
    }

    /// `f` in the reader — pass THIS message on to someone else, in the pane
    /// composer. The pane rather than the reader's inline slot because a
    /// forward needs the two fields a reply does not have: a recipient (nobody
    /// is derivable from the original) and a subject.
    ///
    /// THE WHOLE MESSAGE comes in, and only its `id` goes out: the daemon
    /// quotes the original and re-attaches its files from that id alone. The
    /// rest is what the composer SHOWS — the subject it titles itself with and
    /// the quoted body it renders underneath the note (see
    /// `ComposeState.forwardedMessage`).
    ///
    /// `fallbackSubject` is the CONVERSATION's title, used only when the message
    /// carries no subject header of its own (a daemon too old to send one). The
    /// message's own header wins, because that is the header the daemon quotes:
    /// titling the forward from the thread would name one email while sending
    /// another, on exactly the threads where the two differ (a retitled reply, a
    /// list that stamps its own prefix). Resolved HERE rather than at the call
    /// site so the outgoing subject and the shown one cannot drift.
    ///
    /// Its drafts:
    ///
    /// - No `restoreNewMessage`. Those rows are plain new mail, and adopting
    ///   one here would drop a stranger's half-written mail into a forward of
    ///   something else — and take its subject line with it.
    /// - `restoreForward` instead, on a `drafts_v2` daemon: the row keyed by
    ///   THIS message's id. KEYED PER TARGET, like a reply's, so `f` twice on
    ///   the same message reopens the same draft rather than starting a second
    ///   one, and the drafts page's forward row lands here too (`openDraft`).
    /// - On an older daemon no autosave at all: its drafts wire cannot record
    ///   what is being forwarded, so a saved forward would come back as an
    ///   ordinary new message with the original silently gone. See
    ///   `DraftSaver.save`.
    func openComposeForward(message: ClientMessage, fallbackSubject: String) {
        // A composer already up is not something a repeated `f` gets to blank
        // out — the same rule `openComposeNew` and the inline reply keep.
        guard compose == nil else { return }
        // REFUSE RATHER THAN HOPE. An outdated daemon does not reject a forward,
        // it ignores the field it does not know and answers 200 "sent" having
        // mailed the covering note alone — the original, its quote and its
        // files gone, with the sender told it went. That is a lie a toast cannot
        // walk back, so the composer never opens at all. See
        // `forwardingAvailable` for why nil counts as no.
        guard forwardingAvailable else {
            // Two states hide behind one refusal and only one is the daemon's
            // fault: before the first /client/stats lands there is no verdict
            // yet, and sending the user to a terminal to "update squelchd"
            // over a half-second race would be the wrong sentence. Both still
            // REFUSE — hope is what mails the note alone.
            pushToast(
                stats == nil
                    ? "still connecting · try forward again in a moment"
                    : "this daemon cannot forward mail · update squelchd",
                .error)
            return
        }
        var state = ComposeState()
        state.forwardOfMessageId = message.id
        // THE MESSAGE'S OWN SUBJECT, with the caller's thread title as the
        // fallback — see the note above on why the message wins.
        let subject = message.subject ?? fallbackSubject
        // The subject is SENT, not derived: the daemon only titles a forward
        // itself when the field is absent, and the composer shows one, so what
        // is on screen has to be what goes out.
        state.subject = ComposeCopy.forwardSubject(subject)
        // Nil rather than "" when the original was untitled, so the quote's
        // header has one thing to test.
        let original = subject.trimmed
        state.forwardedSubject = original.isEmpty ? nil : original
        // The reader's copy of what the daemon will quote, for the composer to
        // show. Never sent — see `ComposeState.forwardedMessage`.
        state.forwardedMessage = message
        // Through `openCompose` so the tracker default and the signature are
        // seeded by the one place that seeds them, and the open is counted.
        openCompose(state)
        Task { await restoreForward(message.id) }
    }

    /// Fill the just-opened forward composer from the draft keyed to the
    /// message it forwards. The sibling of `restoreReply`, and silent on
    /// failure like it: a restore that cannot happen leaves the fresh forward
    /// the reader asked for.
    ///
    /// Everything the row holds comes back (to, cc, bcc, subject, body,
    /// files): unlike a reply, a forward derives no audience from anything,
    /// so the draft's addressees are the whole of it.
    private func restoreForward(_ messageId: Int) async {
        let e = epoch
        // Only a `drafts_v2` daemon keeps forward rows at all; asking an older
        // one would list drafts for nothing. Nil stats is "not asked yet", not
        // "no", so an `f` pressed right after launch waits for the answer.
        guard await DraftSaver.shared.awaitVerdict(), draftsV2Available else { return }
        // SETTLE FIRST, as `refreshDrafts` does: a forward of this message
        // closed a moment ago flushed its last save fire-and-forget, and a
        // list taken before it lands restores the text as it was a keystroke
        // earlier (or finds no row at all).
        await DraftSaver.shared.settle()
        guard e == epoch, let rows = await fetchDrafts() else { return }
        // Another account's drafts are not this composer's to restore, and
        // `forward_of_message_id` is a per-daemon id, so one WOULD match.
        guard e == epoch else { return }
        // The fetch doubles as the drafts button's count; see `draftsRows`.
        adoptDraftsRows(rows)
        // A forward of this message in the hold owns this row until its send
        // deletes it, and one whose delete is in its undo window is gone as
        // far as the reader knows.
        guard let draft = rows.first(where: { $0.forward_of_message_id == messageId }),
            !isHeld(draft), !isDeleting(draft.id)
        else { return }
        // Must still be THE forward we opened, untouched: closed, replaced, or
        // typed into in the meantime all mean the draft has missed its window.
        // Untouched is measured against what `openComposeForward` SEEDED (its
        // `Fwd:` subject and the signature), not against empty, which a
        // forward never is: `hasContent` already reads it that way. A file
        // still uploading is not staged yet, so the tray is checked too.
        guard var next = compose, next.forwardOfMessageId == messageId, next.draftId == nil,
            !DraftSaver.hasContent(next), next.attachments.isEmpty
        else { return }
        // `fill` after `openCompose`, as for new mail: the draft's addressees
        // are what somebody typed, not the seed.
        fill(&next, from: draft)
        compose = next
    }

    /// How recent a new-message draft's last edit must be for ⌘N to restore it
    /// (the user's call). Past this the composer opens blank and the draft
    /// waits on the drafts page.
    static let restoreWindow: TimeInterval = 5 * 60

    /// Fill the just-opened new-message composer from its saved draft. Silent on
    /// failure: a restore that cannot happen leaves a blank composer, which is
    /// what the reader asked for anyway.
    private func restoreNewMessage(openedOn opened: ComposePage, composerId: UUID) async {
        let e = epoch
        // No verdict on the daemon yet, no rule to apply: which rows count as
        // candidates depends on whether it keeps one slot or many. WAIT for it
        // (bounded, like a save does) rather than open blank: a `c` in the
        // first half second after launch restored its draft before there was
        // a verdict to ask about, and must still. Every "still the blank
        // composer" guard below runs after the wait, so typing meanwhile wins.
        guard await DraftSaver.shared.awaitVerdict(), e == epoch else { return }
        // OLD DAEMON: not while a new message is in the send HOLD. Its draft row
        // is the one slot, still there until the send deletes it, and restoring
        // it here would put mail that is about to go out into a fresh composer,
        // one ⌘Enter from going out twice. (On a `drafts_v2` daemon the held
        // row is excluded by `isHeld` below instead, which leaves every OTHER
        // draft restorable.)
        if !draftsV2Available, holdsDraft(replyTo: nil) { return }
        // SETTLE FIRST, as `refreshDrafts` does: the composer ⌘N replaced
        // flushed its last save fire-and-forget, and the newest row is exactly
        // the one that flush is writing.
        await DraftSaver.shared.settle()
        guard e == epoch, let rows = await fetchDrafts() else { return }
        // Another account's drafts are not this composer's to restore.
        guard e == epoch else { return }
        // The fetch doubles as the drafts button's count; see `draftsRows`.
        adoptDraftsRows(rows)
        let draft: DraftView?
        if draftsV2Available {
            draft = recentNewDraft(in: rows)
        } else {
            guard !holdsDraft(replyTo: nil),
                let only = rows.first(where: { $0.reply_to_message_id == nil })
            else { return }
            draft = only
        }
        // Must still be THE blank composer we opened: closed, replaced by a
        // reply, or typed into in the meantime all mean the draft has missed its
        // window, and overwriting live keystrokes is worse than not restoring.
        // "Untouched" rather than empty: the composer opens holding the seeded
        // signature, and a seed nobody typed must not block the restore.
        //
        // A FORWARD is not a candidate either, and it would otherwise pass every
        // test above: `f` opens a composer with no reply parent, no draft id and
        // an untouched body, so a `c` fired a moment earlier could land its
        // restore in it and quietly re-address someone else's mail. The subject
        // this seeds would survive as the only visible trace.
        // And THE SAME composer, by identity: a blank one swapped for a
        // mailto: link's passes every test below, and filling it would
        // re-address the mail the link asked for.
        guard var next = compose, next.id == composerId,
            next.replyToMessageId == nil, next.forwardOfMessageId == nil,
            next.draftId == nil, !DraftSaver.hasContent(next),
            // A file dropped in the meantime is composing too, even before it
            // finishes staging: overwriting the tray with the draft's would
            // orphan its upload and cut its marker.
            next.attachments.isEmpty
        else { return }
        // THE RESTORE FILLS ONLY A PAGE NOBODY HAS TOUCHED SINCE (see
        // `openComposeNew`): a reader who toggled to the drafts list in the
        // meantime keeps it. A page toggle is a touch like a keystroke, and
        // filling a composer the reader has left is second-guessing them.
        // Nothing recent: the blank composer stays exactly as opened, whatever
        // older drafts the fetch listed; the header's count notes them.
        let corrected = next.page == opened
        guard let draft, corrected else { return }
        fill(&next, from: draft)
        next.page = .compose
        compose = next
        await resolveDraftGroup()
    }

    /// Copy a saved new-message draft into a composer. AFTER `openCompose`, on
    /// purpose, at both call sites: `openCompose` records what the composer
    /// opened with as its `seededRecipients`, and a draft's own addressees
    /// must not become that seed. They are what somebody TYPED, so emptying the
    /// body of a draft that still names a recipient has to read as a draft, not
    /// as a blank composer the autosave may discard.
    private func fill(_ next: inout ComposeState, from draft: DraftView) {
        next.to = draft.to
        // Nil is a daemon too old to carry them, not an emptied field — leave
        // the composer's own (empty) values rather than writing "" over them,
        // which reads identically here but would not if the seeding rule ever
        // changes.
        next.cc = draft.cc ?? next.cc
        next.bcc = draft.bcc ?? next.bcc
        next.subject = draft.subject
        next.body = draft.body
        // The files the draft claimed, already staged — no upload to wait on.
        next.attachments = (draft.attachments ?? []).map(ComposeAttachment.init)
        next.draftId = draft.id
        next.fromAgent = draft.from_agent == true
    }

    // MARK: - drafts list

    /// Whether this daemon keeps many new-message drafts by id. Same shape as
    /// `forwardingAvailable`: read off the sitrep stats, STRICTLY `== true`.
    /// nil (no stats yet, or a daemon too old to say) hides the drafts button
    /// and keeps ⌘N on the one-slot rule, because the by-id routes would 404
    /// on an old daemon. Note `DraftSaver.save` tells the two nil cases apart;
    /// this flag does not need to. Stored: see `noteStats`.
    private(set) var draftsV2Available = false

    /// Whether panel search asks for plain ranked order: no Not done / Done
    /// split, the owner's sent mail kept by `include_sent`. Read off the
    /// stats, STRICTLY `== true`, so an older daemon (or no stats yet) keeps
    /// the grouped ask that brings sent mail into its keyword results.
    /// A first page reads it live; a page after the first passes the value
    /// that page was fetched under (`SearchSession.fetchedPlainOrder`), since
    /// a cursor is an offset into one ranking and the flag can flip mid-scroll
    /// (the first stats landing, a daemon roll). Stored: see `noteStats`.
    private(set) var searchPlainOrder = false

    /// Fetch the drafts list into `draftsRows`. The drafts page calls it on open.
    ///
    /// SETTLES FIRST: a composer closed a moment ago flushed its last save as a
    /// fire-and-forget task, and listing before it lands would show the draft
    /// as it was a keystroke ago, or not at all. Silent on failure: the cache
    /// keeps what it had.
    func refreshDrafts() async {
        let e = epoch
        await DraftSaver.shared.settle()
        guard e == epoch, let rows = await fetchDrafts(), e == epoch else { return }
        adoptDraftsRows(rows)
    }

    /// Store a fetched list. `fetchDrafts` already took out rows that are
    /// gone as far as the reader knows; this is only the cache write.
    private func adoptDraftsRows(_ rows: [DraftView]) {
        draftsRows = rows
    }

    /// Draft ids that left the daemon this account session, and which list
    /// fetches may still carry them. A fetch that left BEFORE a row went may
    /// still list it, and adopting it would resurrect a draft the reader
    /// deleted (or sent) and watched go; one that left after lists whatever
    /// that id is NOW, which is a new row: `drafts.id` is reused. The whole
    /// rule, and why a permanent set was wrong, is `Tombstones`.
    private var draftTombstones = Tombstones()

    /// The one way this store lists drafts: the daemon's rows minus every one
    /// that is gone as far as the reader knows (see `isGone`), filtered HERE,
    /// while this fetch's own entry still holds its tombstones in place. Nil
    /// on failure, which every caller treats as "keep what you had".
    private func fetchDrafts() async -> [DraftView]? {
        let start = draftTombstones.beginFetch()
        defer { draftTombstones.endFetch(start) }
        guard let rows = try? await APIClient.shared.listDrafts() else { return nil }
        return rows.filter { !isGone($0.id, fetchedAt: start) }
    }

    /// A draft row just left the daemon (its DELETE came back, or its send
    /// did): stamp its tombstone with a fresh generation, so fetches already
    /// in the air still drop it and later ones do not.
    private func noteDraftGone(_ id: Int) {
        draftTombstones.gone(id)
    }

    /// Whether a row a fetch that left at generation `start` listed is gone
    /// as far as the reader knows: its delete is in the undo window, or it
    /// left the daemon after that fetch did.
    private func isGone(_ id: Int, fetchedAt start: Int) -> Bool {
        if pendingDraftDeletes[id] != nil { return true }
        return draftTombstones.hides(id, fetchedAt: start)
    }

    /// Whether this draft id's delete is in its undo window or on the wire.
    /// Rows from `fetchDrafts` are already filtered; this is for the checks
    /// made after, when a delete may have started since the list landed.
    private func isDeleting(_ id: Int) -> Bool {
        pendingDraftDeletes[id] != nil || draftTombstones.isLeaving(id)
    }

    /// What the drafts page lists: the cache minus rows a held or sending mail
    /// owns. Those are mail on its way out, and opening one would put it in a
    /// composer one ⌘Enter from going out twice. Inert rows STAY (the drafts page
    /// marks them "editing"); see `isInert`.
    var listedDrafts: [DraftView] { draftsRows.filter { !isHeld($0) } }

    /// The drafts the page can actually open: `listedDrafts` minus the ones
    /// already open in a composer. Its count is the header control's number.
    var otherDrafts: [DraftView] { listedDrafts.filter { !isInert($0) } }

    /// Whether this row is the draft an open composer (the pane OR the
    /// reader's inline reply) is editing, by id or by its reply/forward key.
    /// The drafts page shows it marked "editing" and will neither open nor delete
    /// it.
    ///
    /// Delete is the one that matters: the composer's close flush would
    /// faithfully re-create the row it just lost (or edit the dead id and
    /// fall back to a create), so "deleted" would last until the next Esc.
    /// The id is read through `DraftSaver.currentDraftId`, which also knows a
    /// first save that came back after the slot last changed.
    func isInert(_ draft: DraftView) -> Bool {
        [compose, inlineReply].compactMap { $0 }.contains { open in
            if DraftSaver.shared.currentDraftId(of: open) == draft.id { return true }
            if let parent = draft.reply_to_message_id, open.forwardOfMessageId == nil,
                open.replyToMessageId == parent
            {
                return true
            }
            if let original = draft.forward_of_message_id,
                open.forwardOfMessageId == original
            {
                return true
            }
            return false
        }
    }

    /// Whether a held or sending mail owns this row. Its send deletes it; until
    /// then neither the drafts page nor the ⌘N restore may offer it.
    ///
    /// By id first, from every place a held mail's id can be: its own state
    /// (held, releasing, or on the wire), and `detachedDrafts`, where the id
    /// lands when the save that minted it was still in flight at send time
    /// (so the state's own `draftId` is nil). Then by reply or forward key,
    /// which is how a keyed row was always matched and still covers one whose
    /// id never reached the client.
    func isHeld(_ draft: DraftView) -> Bool {
        if waitingSends.contains(where: { $0.state.draftId == draft.id }) { return true }
        if firingDraftIds.contains(draft.id) { return true }
        if detachedDrafts.values.contains(where: { $0 == draft.id }) { return true }
        if let parent = draft.reply_to_message_id, holdsDraft(replyTo: parent) { return true }
        if let original = draft.forward_of_message_id, holdsDraft(forwardOf: original) {
            return true
        }
        return false
    }

    /// Open a saved draft from the drafts page.
    ///
    /// A REPLY opens where it belongs: the thread, with the inline reply
    /// restored through the ordinary `r` chain (`pendingReplyMessageId` →
    /// the viewer → `openInlineReply` → `restoreReply`). Nothing new, so a
    /// reply opened from the drafts page and one opened by `r` cannot differ.
    ///
    /// PLAIN NEW MAIL loads into THIS pane, on its composer page. The one
    /// being replaced is FLUSHED, not closed: it autosaves like any Esc
    /// would, and it will be on the drafts page next time. (The open
    /// composer's own draft is inert, so "open the draft I am already in"
    /// cannot reach here.)
    ///
    /// A FORWARD needs the original it forwards, which the row names only by
    /// id: the composer shows the message underneath the note, and titles
    /// itself from it. So the message is fetched FIRST, and only once it is in
    /// hand is the pane closed (flushed, like any Esc) and the forward opened
    /// through `openComposeForward`, whose `restoreForward` fills it from this
    /// row exactly as a second `f` on that message would. A failed fetch
    /// leaves the pane as it was, still on the drafts page, with a toast:
    /// closing first would have left the person with nothing on screen.
    func openDraft(_ draft: DraftView) {
        guard !isInert(draft), !isHeld(draft), pendingDraftDeletes[draft.id] == nil else { return }
        if let original = draft.forward_of_message_id {
            let e = epoch
            Task {
                let envelope = try? await APIClient.shared.getMessage(original)
                // Another account's message ids mean nothing here.
                guard e == epoch else { return }
                guard let envelope,
                    let message = envelope.thread.messages.first(where: {
                        $0.id == envelope.message_id
                    })
                else {
                    pushToast("that message is no longer here", .error)
                    return
                }
                // The person moved on during the fetch (back to a composer
                // page, say): what is on screen now is theirs, not this
                // click's to replace.
                if let open = compose, open.page != .drafts { return }
                // Closed only NOW, with the original in hand: the forward
                // opener refuses over an open composer.
                closeCompose()
                openComposeForward(message: message, fallbackSubject: envelope.thread.subject)
            }
            return
        }
        if let parent = draft.reply_to_message_id {
            // No thread = a parent this daemon no longer describes (or one too
            // old to say). There is nowhere to open the reply under.
            guard let thread = draft.thread_id else {
                pushToast("could not find the email that draft replies to", .error)
                return
            }
            closeCompose()
            openThread(thread, replyTo: parent, from: .reply)
            return
        }
        // Through `closeCompose`, so the one being replaced says "kept in
        // drafts" exactly when an Esc on it would have.
        closeCompose()
        // Opened BLANK, then filled, the order `restoreNewMessage` keeps: see
        // `fill` for why the draft's recipients must not be the seed.
        // `openCompose` also marks the slot opened, so a glance that types
        // nothing writes nothing and does not bump the draft's `updated_at`.
        // A fresh state, so the pane leaves the drafts page for the composer.
        openCompose(ComposeState())
        guard var next = compose else { return }
        fill(&next, from: draft)
        next.page = .compose
        // A saved draft states its own audience: it was a new message when it
        // was typed, and new messages are addressed by hand.
        next.recipientsStated = true
        compose = next
        Task { await resolveDraftGroup() }
    }

    /// A draft delete waiting out its undo window. See `deleteDraft`.
    private struct PendingDraftDelete {
        var draft: DraftView
        /// Where the row sat in `draftsRows`, so an undo puts it back in place
        /// rather than at the end of the list.
        var index: Int
        var undoId: UUID
        var timer: Task<Void, Never>
    }

    /// Keyed by draft id. Separate from `undos` because `PendingUndo` has no
    /// expiry hook: its entry is simply dropped after five seconds, and the
    /// DELETE has to go out at exactly that moment, so the hold keeps its own
    /// timer (the `sendWithUndo` shape).
    private var pendingDraftDeletes: [Int: PendingDraftDelete] = [:]

    /// Delete a draft from the drafts page, with the app's five-second undo.
    ///
    /// HELD, not deleted-then-recreated: the row leaves the list now and the
    /// DELETE goes out when the window closes, so an undo is only ever "do not
    /// delete" and the draft comes back with its own id, attachments and
    /// `updated_at` intact. Recreating it would mint a new row and re-stage
    /// files the delete had already released to the sweep.
    ///
    /// Refused for an inert row (the draft an open composer is editing), whose
    /// close flush would bring it back; see `isInert`. And for a held one,
    /// whose send deletes it anyway and whose undo would reopen it.
    func deleteDraft(_ draft: DraftView) {
        guard !isInert(draft), !isHeld(draft), pendingDraftDeletes[draft.id] == nil
        else { return }
        let id = draft.id
        let e = epoch
        let index = draftsRows.firstIndex { $0.id == id } ?? draftsRows.count
        draftsRows.removeAll { $0.id == id }
        let timer = Task { [weak self] in
            try? await Task.sleep(for: .seconds(Self.draftDeleteHold))
            guard !Task.isCancelled else { return }
            await self?.fireDraftDelete(id, epoch: e)
        }
        // `messageId: -1`, NOT the draft id: `fireUndo` un-resolves the entry's
        // id on success, and draft ids share the Int space with message ids, so
        // a draft id there would un-hide an unrelated message.
        let undoId = pushUndo(kind: .draftDelete, messageId: -1, label: Self.draftLabel(draft)) {
            [weak self] in
            guard let self, await self.keepDraft(id) else { throw HoldError.alreadySent }
        }
        pendingDraftDeletes[id] = PendingDraftDelete(
            draft: draft, index: index, undoId: undoId, timer: timer)
    }

    /// "deleted draft: Lunch plans". Names the draft, because the undo toast
    /// is read by somebody wondering whether they deleted the right one.
    private static func draftLabel(_ draft: DraftView) -> String {
        let subject = draft.subject.trimmed
        if !subject.isEmpty { return "deleted draft: \(Fmt.truncate(subject, 40))" }
        if let parent = draft.parent_subject?.trimmed, !parent.isEmpty {
            return "deleted draft: re \(Fmt.truncate(parent, 40))"
        }
        return "deleted draft"
    }

    /// Undo: the DELETE never goes out and the row goes back where it was.
    /// False when the window already closed, so the toast says the undo failed
    /// rather than claiming a deleted draft was kept.
    private func keepDraft(_ id: Int) -> Bool {
        guard let pending = pendingDraftDeletes.removeValue(forKey: id) else { return false }
        pending.timer.cancel()
        if !draftsRows.contains(where: { $0.id == id }) {
            draftsRows.insert(pending.draft, at: min(pending.index, draftsRows.count))
        }
        return true
    }

    /// The window closed: delete it for real. Removed from the table FIRST,
    /// on the main actor, so an undo landing now finds nothing and says so.
    private func fireDraftDelete(_ id: Int, epoch e: Int) async {
        guard let pending = pendingDraftDeletes.removeValue(forKey: id) else { return }
        undos.removeAll { $0.id == pending.undoId }
        // An account switch inside the hold: the id belongs to the OLD
        // daemon's table and means a different row, or nothing, here.
        guard e == epoch else { return }
        // The row may have become LIVE during the hold: delete a reply draft,
        // press `r` on its parent, type, and the debounce PUTs by key into
        // this very row (it is still on the daemon). The DELETE would take
        // that fresh text and its files with it. A held send owns its row the
        // same way. Either one means the draft is kept after all: back in the
        // list, and no error, because nothing failed.
        if isInert(pending.draft) || isHeld(pending.draft) {
            if !draftsRows.contains(where: { $0.id == id }) {
                draftsRows.insert(pending.draft, at: min(pending.index, draftsRows.count))
            }
            return
        }
        // Hidden from every fetch while on the wire, until it lands.
        draftTombstones.leaving(id)
        do {
            try await APIClient.shared.deleteDraft(id)
            if e == epoch { noteDraftGone(id) }
        } catch let error as APIError where error.kind == .notFound {
            // Already gone (sent, or deleted elsewhere). What was asked for.
            if e == epoch { noteDraftGone(id) }
        } catch {
            guard e == epoch else { return }
            // The row is still on the daemon, so it goes back in the list
            // rather than hiding until the next fetch pretends it came back.
            draftTombstones.revive(id)
            if !draftsRows.contains(where: { $0.id == id }) {
                draftsRows.insert(pending.draft, at: min(pending.index, draftsRows.count))
            }
            pushToast(errText(error, "could not delete that draft"), .error)
        }
    }

    /// Turn a restored draft's `#slug` token back into a group.
    ///
    /// The token is all the draft could keep — the drafts row has no group
    /// column, and giving it one would put the audience in two places that can
    /// disagree. So the composer re-resolves on restore, and a token that no
    /// longer names anything (deleted, or renamed, which changes the slug) simply
    /// does not resolve: the pill renders as a problem and the daemon refuses the
    /// send. Both are correct. Guessing which audience someone meant is not a
    /// thing to do with an irreversible action.
    private func resolveDraftGroup() async {
        let e = epoch
        guard let slug = compose.map({ GroupToken.firstSlug(in: $0.to) }) ?? nil,
            let group = await GroupToken.resolve(slug),
            e == epoch,
            var next = compose,
            // Still the same composition: a restore that raced a keystroke must
            // not stamp a group onto whatever is in the slot now.
            GroupToken.firstSlug(in: next.to) == slug
        else { return }
        next.groupId = group.id
        next.groupMode = group.mode
        next.groupName = group.name
        compose = next
    }

    func closeCompose() {
        // An unsent draft outlives its composer: the closing values go out as one
        // fire-and-forget save. Before the slot is cleared — `flush` reads it.
        let closing = compose
        let queued = DraftSaver.shared.flush(.compose, closing)
        // SAY SO, now that drafts are many and older ones live behind a
        // button: an Esc that quietly kept the mail reads exactly like one
        // that threw it away. Only when a row actually holds it: this close
        // QUEUED a save (the composer was typed into; an agent hand-off
        // counts, see `AskBar`), or an earlier save already made the row and
        // it is still there, still holding content. Never for a composer
        // nothing was saved from, whose toast would promise a row that does
        // not exist. A forward only on a `drafts_v2` daemon, which is the
        // only one that keeps it. The pane only; the inline reply comes back
        // by itself the next time `r` is pressed on the same message.
        if let closing, DraftSaver.hasContent(closing),
            queued || DraftSaver.shared.currentDraftId(of: closing) != nil,
            closing.forwardOfMessageId == nil || draftsV2Available
        {
            pushToast("kept in drafts")
        }
        compose = nil
        if schedulePicker == .compose { schedulePicker = nil }
        // Refresh the cache behind the close (it settles first, so it sees
        // this flush land), so the drafts button's count is right the next
        // time a composer opens rather than one close behind. Epoch-guarded
        // inside. Only when this close could have CHANGED the set: it queued
        // a write, the composer already had a row, or it was closed from the
        // drafts page (where rows are opened and deleted). A glance at an
        // empty composer changes nothing and costs no round trip.
        if let closing,
            queued || DraftSaver.shared.currentDraftId(of: closing) != nil
                || closing.page == .drafts
        {
            Task { await refreshDrafts() }
        }
    }

    // MARK: - discarding a composer

    /// The trash can's question, on screen: the identity of the composer it
    /// was asked about. A field here that `ActionLayer` draws, like every
    /// other modal, because the pane is half a window wide and a scrim
    /// mounted inside it would dim only the pane.
    var discardAsk: UUID?

    /// The composer's trash can. Esc keeps the mail (`closeCompose`); this is
    /// the other door, the one that does not, so it asks first: there is no
    /// undo behind it. A composer holding nothing has nothing to lose and
    /// nothing to ask about, and simply closes.
    func askDiscardCompose() {
        guard let open = compose, !open.sending else { return }
        guard DraftSaver.hasContent(open) || !open.attachments.isEmpty else {
            discardCompose()
            return
        }
        // The card has no field of its own, so the caret would stay in the
        // mail under the scrim and every key the card does not bind (letters,
        // Backspace, ⌘V) would edit it blind. The pane puts it back if the
        // answer is no. Whichever window holds a text responder, not only
        // the key one: the trash can is clickable in a window that is not.
        #if os(macOS)
            for window in NSApp.windows where window.firstResponder is NSText {
                window.makeFirstResponder(nil)
            }
        #endif
        discardAsk = open.id
    }

    func cancelDiscardAsk() {
        discardAsk = nil
    }

    /// Confirmed. Only for the composer the question was about: one that
    /// was replaced while the card was up never agreed to anything.
    func confirmDiscard() {
        guard let asked = discardAsk else { return }
        discardAsk = nil
        // A send that started while the card was up owns the composer now.
        guard compose?.id == asked, compose?.sending != true else { return }
        discardCompose()
    }

    /// Close the pane and keep NOTHING: no close flush, and the row an
    /// earlier autosave made goes too (`DraftSaver.discard`, which waits out
    /// a save still on the wire so the row it mints is deleted, not missed).
    /// `closeCompose` minus the save, plus the delete.
    private func discardCompose() {
        guard let closing = compose else { return }
        let hadContent = DraftSaver.hasContent(closing) || !closing.attachments.isEmpty
        DraftSaver.shared.discard(.compose, closing)
        compose = nil
        if schedulePicker == .compose { schedulePicker = nil }
        // The mirror of "kept in drafts": the two doors out must not read
        // alike. Silent for a blank composer, which deleted nothing.
        if hadContent { pushToast("message deleted") }
        // Settles first, so it lists what is left once the delete landed.
        Task { await refreshDrafts() }
    }

    /// Delete the row a discarded composer had been autosaving into. Called
    /// by `DraftSaver.discard`, on the slot's own save chain. Immediate, with
    /// no undo window: the confirm card was the second thought. True once the
    /// row is gone (a 404 counts: gone is what was asked for).
    func deleteDiscardedDraft(_ id: Int) async -> Bool {
        let e = epoch
        draftsRows.removeAll { $0.id == id }
        // Hidden from every fetch while on the wire, until it lands.
        draftTombstones.leaving(id)
        do {
            try await APIClient.shared.deleteDraft(id)
            if e == epoch { noteDraftGone(id) }
            return true
        } catch let error as APIError where error.kind == .notFound {
            if e == epoch { noteDraftGone(id) }
            return true
        } catch {
            guard e == epoch else { return false }
            // Still on the daemon, so the next list shows it again rather
            // than the pane having promised a delete that did not happen.
            draftTombstones.revive(id)
            pushToast(errText(error, "could not delete that draft"), .error)
            return false
        }
    }

    /// Open the reader's inline reply on a specific message. Never resets a
    /// composer that is already open — a draft is not something a repeated `r`
    /// gets to throw away.
    ///
    /// `replyAll` is the Enter key's answer to `r`'s: same composer, same
    /// ceremony, and the daemon does the widening. It is fixed here rather than
    /// toggled later, so what the header says is what the opening keystroke
    /// asked for.
    func openInlineReply(replyTo messageId: Int, replyAll: Bool = false) {
        guard inlineReply == nil else { return }
        // Same rule as the pane composer: a reply gets the whole reader, not
        // the sliver left beside an open search strip.
        if sideView.isOpen { closeSide() }
        inlineReply = ComposeState(
            replyToMessageId: messageId, body: Prefs.shared.signatureSeed,
            includeTracker: trackingDefault, replyAll: replyAll)
        DraftSaver.shared.noteOpened(.inlineReply)
        // Both ways in (the reader's `r` and the list's hand-off) come through
        // here, so the restore is wired once.
        Task { await restoreReply(messageId) }
    }

    /// Fill the just-opened inline composer from the draft keyed to this parent.
    ///
    /// The BODY, plus any copy lists the draft holds. A reply still derives its
    /// `to` and its `Re: <subject>` from the parent, but a Bcc never derives
    /// from anything — so a saved one has to come back, or closing a reply to
    /// think about it would silently un-blind-copy somebody.
    private func restoreReply(_ messageId: Int) async {
        let e = epoch
        // Same rule as the new message: a reply to this message in the hold
        // owns this draft row until its send deletes it.
        guard !holdsDraft(replyTo: messageId) else { return }
        // SETTLE FIRST, as `refreshDrafts` does: an inline reply to this
        // message closed a moment ago flushed fire-and-forget, and the list
        // must see that save, not the one before it.
        await DraftSaver.shared.settle()
        // A row whose delete is in its undo window (or went out) is gone as
        // far as the reader knows; restoring it would undo the delete.
        guard e == epoch,
            let rows = await fetchDrafts(),
            let draft = rows.first(where: { $0.reply_to_message_id == messageId }),
            !holdsDraft(replyTo: messageId), !isDeleting(draft.id)
        else { return }
        // Another account's drafts are not this composer's to restore — and
        // `reply_to_message_id` is a per-daemon id, so one WOULD match.
        guard e == epoch else { return }
        // Untouched, not empty: the seeded signature and the derived audience
        // must not block the restore, but an edited recipient must.
        guard var next = inlineReply, next.replyToMessageId == messageId, next.draftId == nil,
            !DraftSaver.hasContent(next), next.attachments.isEmpty
        else { return }
        next.body = draft.body
        // Only a draft that actually recorded a copy list gets to state one:
        // restoring `""` over an unseeded composer would flip
        // `recipientsStated` on a reply that has not learned its audience yet.
        if let cc = draft.cc, let bcc = draft.bcc, !cc.isEmpty || !bcc.isEmpty {
            next.cc = cc
            next.bcc = bcc
            next.to = draft.to.isEmpty ? next.to : draft.to
            next.recipientsStated = true
        }
        // AN AGENT'S DRAFT STATES ITS AUDIENCE, always. The agent chose who
        // this goes to, so the fields are laid open with exactly what it
        // chose rather than left showing the reply's derived recipient over
        // a `to` the owner never saw.
        if draft.from_agent == true {
            next.to = draft.to.isEmpty ? next.to : draft.to
            next.cc = draft.cc ?? next.cc
            next.bcc = draft.bcc ?? next.bcc
            next.recipientsStated = true
            next.fromAgent = true
        }
        next.attachments = (draft.attachments ?? []).map(ComposeAttachment.init)
        next.draftId = draft.id
        inlineReply = next
    }

    // MARK: - an agent asks for its draft on screen

    /// The agent door's `open_draft`, relayed over `accountId`'s event stream.
    func openAgentDraft(_ signal: AgentOpenSignal, accountId: UUID) {
        // A request that sat in a sleeping Mac's connection is not a request
        // any more.
        // The Mac's alone: the phone never subscribes to these (see
        // `EventStream`), and is told by a push instead.
        #if os(macOS)
            guard signal.isFresh(now: Int(Date().timeIntervalSince1970)) else { return }
            Task { await showAgentDraft(signal.open_draft, accountId: accountId, link: nil) }
        #endif
    }

    /// When a `passband://draft` link was last acted on. Links come from
    /// whatever opened the URL, and each one costs a fetch and may take the
    /// screen: the daemon spaces an agent's requests five seconds apart, and
    /// a link gets the same spacing here, where the daemon cannot see it.
    private var lastDraftLinkAt: Date?

    /// A `passband://draft` link somebody clicked. Always about the mailbox on
    /// screen: the link's `mailbox` is checked against it rather than used to
    /// go looking through other accounts.
    func openAgentDraft(_ link: AgentDraftLink) {
        guard connStatus == .connected, let active = AccountManager.shared.activeId else { return }
        if let last = lastDraftLinkAt, Date().timeIntervalSince(last) < 5 { return }
        lastDraftLinkAt = Date()
        Task { await showAgentDraft(link.draftId, accountId: active, link: link) }
    }

    /// A tap on the phone's "your agent left a draft" banner. The push named
    /// the draft by id and creation second; it opens only if this account's
    /// daemon still holds an agent's draft created at exactly that second.
    /// Delivered by `Notifier` once `accountId` is the account on screen.
    func openAgentDraftFromPush(draftId: Int, created: Int, accountId: UUID) {
        Task { await showAgentDraft(draftId, accountId: accountId, link: nil, pushCreated: created) }
    }

    /// ⌘Enter was pressed on a composer whose keyboard send is locked.
    func noteKeyboardSendLocked() {
        pushToast("Your agent opened this draft. Read it, then click send.")
    }

    /// Bring one of an agent's drafts up in the composer, and Passband to the
    /// front with it.
    ///
    /// THE REQUEST IS NOT TRUSTED TO NAME ANYTHING BUT AN ID. What opens is the
    /// draft the daemon holds under that id, and only when the daemon marks it
    /// `from_agent`: neither a signal nor a link can open (or probe for) a
    /// draft the owner wrote. A link must also carry that draft's creation
    /// second, so an old link cannot open whatever draft got the id next.
    ///
    /// IT NEVER TAKES A COMPOSER SOMEBODY IS WRITING IN, AND THEN IT DOES NOT
    /// COME FORWARD EITHER. Activating Passband under somebody typing in
    /// another app delivers their next keystrokes here: with their own
    /// half-written mail in the composer, a typed-ahead ⌘Enter would send it.
    /// So when anything of theirs is being written, nothing moves; a toast
    /// waits for them. The one composer that does not count as theirs is an
    /// agent's draft still exactly as the agent left it, which the next
    /// request may replace.
    ///
    /// WHAT IT OPENS CANNOT BE SENT FROM THE KEYBOARD, for the same reason:
    /// see `ComposeState.keyboardSendLocked`. And the keyboard is taken off
    /// the composer after it opens, so text typed ahead lands nowhere rather
    /// than in the draft's fields.
    ///
    /// `link` is set for a link, which somebody clicked and so deserves an
    /// answer when it leads nowhere; a relayed signal that leads nowhere says
    /// nothing.
    private func showAgentDraft(
        _ draftId: Int, accountId: UUID, link: AgentDraftLink?, pushCreated: Int? = nil
    ) async {
        guard !FixtureMode.isEnabled, !accountActionsBlocked else { return }
        let asked = link != nil || pushCreated != nil
        guard AccountManager.shared.activeId == accountId else {
            pushToast("Your agent left a draft in another account. Switch to it to open the draft.")
            return
        }
        if let mailbox = link?.mailbox {
            // Checked against an address this app actually KNOWS. Right
            // after launch or a switch it may not have one yet, so it is
            // asked for; a link whose mailbox cannot be confirmed opens
            // nothing, since draft ids repeat across mailboxes.
            var own = stats?.account_email
            if own == nil { own = (try? await APIClient.shared.getStats())?.account_email }
            let known = own?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() ?? ""
            guard AccountManager.shared.activeId == accountId else { return }
            guard known == mailbox else {
                pushToast(
                    known.isEmpty
                        ? "Could not check which account that draft belongs to. Try the link again."
                        : "That draft belongs to another account. Switch to it and open the link again.")
                return
            }
        }
        let e = epoch
        let rows = await fetchDrafts()
        // The account on screen must still be the one this was for: the
        // drafts just listed are whichever daemon the client is pointed
        // at NOW.
        guard e == epoch, AccountManager.shared.activeId == accountId else { return }
        guard let rows else {
            if asked { pushToast("Could not reach the server to open that draft.", .error) }
            return
        }
        guard let draft = rows.first(where: { $0.id == draftId }), draft.from_agent == true,
            link?.names(draftCreatedAt: draft.created_at) ?? true,
            pushCreated.map({ EventBanner.draftSecond(draft.created_at) == $0 }) ?? true,
            !isDeleting(draft.id)
        else {
            if asked { pushToast("That draft is not here any more.") }
            return
        }
        // Already the one on screen, or already on its way out.
        guard !isInert(draft), !isHeld(draft) else { return }
        // Mail being written, OR AN EVENT (verify7 D5): a pane with
        // something in it, parked or frozen, is the user's writing too.
        // Coming forward over it would hand a typed-ahead ↵ or ⌘↵ to a
        // pane that adds an event and can invite its guests.
        let writing = mailBeingWritten(agentRows: rows) || eventPaneHeldByUser
        guard !writing else {
            pushToast("Your agent left a draft. It is in your drafts when you are done here.")
            return
        }
        openDraft(draft)
        // NOTHING COMES FORWARD FOR A DRAFT THE COMPOSER DID NOT TAKE.
        // `openDraft` can decline (the composer refused, the parent gone)
        // and a forward opens only after a fetch.
        lockKeyboardSend(draftId: draft.id, replyTo: draft.reply_to_message_id)
        var holds = agentDraftOnScreen(draft)
        if !holds, draft.forward_of_message_id != nil {
            for _ in 0..<12 where !holds {
                try? await Task.sleep(for: .milliseconds(150))
                guard e == epoch else { return }
                holds = agentDraftOnScreen(draft)
                lockKeyboardSend(draftId: draft.id, replyTo: draft.reply_to_message_id)
            }
        }
        #if os(iOS)
            // The phone has no window to bring forward and no keyboard
            // typing into another app: the tap already put Passband on
            // screen. Marked once more for a reply, whose inline composer
            // fills a moment after the thread opens.
            try? await Task.sleep(for: .milliseconds(700))
            guard e == epoch else { return }
            lockKeyboardSend(draftId: draft.id, replyTo: draft.reply_to_message_id)
        #endif
        #if os(macOS)
            // Asked again after any wait: an event may have been started.
            guard
                EventPaneTravel.agentDraftMayComeForward(writing: eventPaneHeldByUser, composerHoldsIt: holds)
            else { return }
            // Marked on whichever slot the draft landed in. A reply restores
            // into the inline composer a moment later, so the mark is set
            // again once that fetch has had time to fill it.
            lockKeyboardSend(draftId: draft.id, replyTo: draft.reply_to_message_id)
            // The window may be closed (the app keeps running so the feed
            // stays up); activation alone does not bring a closed one back.
            MainWindow.show()
            NSApp.activate(ignoringOtherApps: true)
            try? await Task.sleep(for: .milliseconds(700))
            guard e == epoch else { return }
            lockKeyboardSend(draftId: draft.id, replyTo: draft.reply_to_message_id)
            MainWindow.find()?.makeFirstResponder(nil)
        #endif
    }

    /// A composer or an inline reply holds something somebody typed. The one
    /// that does not count is an agent's draft still exactly as the agent
    /// left it (known only with the drafts list in hand: `agentRows`), which
    /// the next request may replace. Shared by the agent door's `open_draft`
    /// and the assistant's event proposal (`proposeEvent`): neither ever
    /// closes mail that is being written.
    func mailBeingWritten(agentRows rows: [DraftView]? = nil) -> Bool {
        [compose, inlineReply].compactMap { $0 }.contains { open in
            // An agent's draft nobody has touched is not somebody writing.
            if open.fromAgent, let id = open.draftId, let row = rows?.first(where: { $0.id == id }),
                row.body == open.body, open.attachments.isEmpty
            {
                return false
            }
            return DraftSaver.hasContent(open) || !open.attachments.isEmpty
        }
    }

    /// The composer (or, for a reply, the reader its inline composer fills
    /// into) really holds this draft.
    private func agentDraftOnScreen(_ draft: DraftView) -> Bool {
        if draft.forward_of_message_id != nil {
            return compose?.forwardOfMessageId == draft.forward_of_message_id && compose?.page != .drafts
        }
        if draft.reply_to_message_id != nil {
            return threadId != nil && threadId == draft.thread_id
        }
        return compose?.draftId == draft.id
    }

    /// Set `keyboardSendLocked` on the composer holding this draft: the pane,
    /// for new mail, or the inline reply to its parent.
    private func lockKeyboardSend(draftId: Int, replyTo parent: Int?) {
        if var pane = compose, pane.draftId == draftId, !pane.keyboardSendLocked {
            pane.keyboardSendLocked = true
            compose = pane
        }
        if let parent, var reply = inlineReply, reply.replyToMessageId == parent, !reply.keyboardSendLocked {
            reply.keyboardSendLocked = true
            inlineReply = reply
        }
    }

    func closeInlineReply() {
        DraftSaver.shared.flush(.inlineReply, inlineReply)
        inlineReply = nil
        if schedulePicker == .inlineReply { schedulePicker = nil }
    }

    // MARK: - undo send

    /// How long a send is HELD before it goes out, which is the whole of the
    /// undo: the same window every other undo toast gets.
    static let sendHold: TimeInterval = 5

    /// A send waiting out its hold. The composer that made it is already gone
    /// from the screen; this is everything needed to either fire it or put it
    /// back.
    private struct HeldSend {
        var state: ComposeState
        var slot: DraftSaver.Slot
        var override: Bool
        /// The thread the reader was on, for putting an inline reply back
        /// where it was typed. See `putBack`.
        var threadId: String?
        /// The account it was written in. An account switch inside the hold
        /// must not let the mail leave from the OTHER account.
        var epoch: Int
        var undoId = UUID()
    }

    private var heldSends: [UUID: HeldSend] = [:]
    /// Every composer identity whose mail is held OR on the wire, and the
    /// draft ids that saves in flight at send time came back with for them
    /// (see `DraftSaver.adopt`). The double Optional is the save's own answer:
    /// `.some(nil)` is a draft it deleted.
    private var holding: Set<UUID> = []
    private var detachedDrafts: [UUID: Int?] = [:]

    func noteDetachedDraft(_ id: Int?, composer: UUID) {
        guard holding.contains(composer) else { return }
        detachedDrafts[composer] = .some(id)
    }

    /// Mails whose hold has ended, waiting for draft saves to settle before
    /// they fire. Still the switch's to park (see `parkHeldSends`): until the
    /// request is built, the account can change under them.
    private var releasing: [UUID: HeldSend] = [:]
    /// Scheduled mails between the composer closing and their POST leaving:
    /// `fireSchedule` is waiting for the draft saves to settle. The composer
    /// is gone and its last keystrokes live only here (`noteSent` dropped the
    /// debounced save), so the switch and quit park these exactly as they
    /// park `releasing`, or the tail of the typing goes with the account.
    private var scheduling: [UUID: HeldSend] = [:]

    /// Every mail the store holds that has not reached the wire, whatever
    /// door it is leaving by. What `holdsDraft` and `isHeld` read.
    private var waitingSends: [HeldSend] {
        Array(heldSends.values) + Array(releasing.values) + Array(scheduling.values)
    }
    /// The reply keys of mails ON THE WIRE, whose draft rows exist until the
    /// send returns. Forwards are not in it (their key is a different column,
    /// and nil here means the old daemon's new-message slot); see
    /// `firingForwardKeys`.
    private var firingReplyKeys: [Int?] = []
    /// The forward keys of mails ON THE WIRE, for the same reason: a forward
    /// row on a `drafts_v2` daemon lives until its send returns and deletes it.
    private var firingForwardKeys: [Int] = []
    /// The draft ids of mails ON THE WIRE, for the same reason: a `drafts_v2`
    /// new-message row is matched by id, not by a key, and between leaving
    /// `releasing` and the send returning its state is in neither table
    /// `isHeld` reads.
    private var firingDraftIds: [Int] = []

    /// Whether any mail is waiting out its hold or waiting to fire. What quit
    /// asks before it lets the app go (see `AppDelegate`).
    var hasHeldSends: Bool { !heldSends.isEmpty || !releasing.isEmpty || !scheduling.isEmpty }

    /// Outbox requests whose ANSWER IS THE MAIL, on the wire right now: a
    /// schedule POST (a failure puts the mail back in a composer), a pull-back
    /// DELETE (the answer is the request the composer reopens from) and an
    /// undo's re-POST (a failure reopens it). Not parked, because there is
    /// nothing to park: the mail is inside the request. ⌘Q WAITS for these
    /// instead (bounded, see `AppDelegate`) and then flushes whatever
    /// composer the answer opened. Each caller holds the count until the
    /// composer has the mail, not just until the response lands.
    private(set) var outboxOnWire = 0

    /// Until nothing in `outboxOnWire` is left. Unbounded; the caller bounds.
    func awaitOutboxOnWire() async {
        while outboxOnWire > 0 {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    /// Whether a held or sending mail owns the draft row for this reply key
    /// (nil = the ONE new-message row of an older daemon; on a `drafts_v2`
    /// daemon new mail is matched by id instead, see `isHeld`). A forward's
    /// row is a different key; see `holdsDraft(forwardOf:)`.
    private func holdsDraft(replyTo: Int?) -> Bool {
        waitingSends.contains {
            $0.state.replyToMessageId == replyTo && $0.state.forwardOfMessageId == nil
        } || firingReplyKeys.contains(replyTo)
    }

    /// Whether a held or sending forward owns the draft row keyed to this
    /// forwarded message (`drafts_v2` daemons; older ones never have one).
    private func holdsDraft(forwardOf original: Int) -> Bool {
        waitingSends.contains {
            $0.state.forwardOfMessageId == original
        } || firingForwardKeys.contains(original)
    }

    /// The state as the send must see it: with any draft id a late save
    /// brought back.
    private func withDetachedDraft(_ state: ComposeState) -> ComposeState {
        var state = state
        if let late = detachedDrafts.removeValue(forKey: state.id) { state.draftId = late }
        return state
    }

    /// PARK every held send as a draft, now: the account is changing under
    /// it. Sending early would take away the undo the sender was promised;
    /// dropping it would lose the mail. Called before the switch's `settle`,
    /// which waits for these saves before the client is reconfigured.
    ///
    /// `save: false` is the account-REMOVAL path, same as its `flushDrafts`:
    /// the daemon the draft would go to is the one being forgotten.
    ///
    /// Each one SAYS so. The sender pressed send and saw "sending to…"; a mail
    /// that then quietly became a draft would be believed sent.
    ///
    /// `quitting: true` is ⌘Q inside the hold (see `AppDelegate`): the same
    /// park, but no toast, because the window it would appear in is going
    /// away. The mail is on the drafts page on the next launch.
    ///
    /// A SCHEDULED MAIL is parked too while it waits for the draft saves to
    /// settle (`scheduling`): its last keystrokes exist nowhere else. From the
    /// moment its POST leaves it is not here any more: the draft row the
    /// autosave wrote is still there until the POST consumes it, and from
    /// the 201 on the mail is the daemon's, in the outbox, cancellable from
    /// Sent on any account. An account switch inside the POST is caught by
    /// `fireSchedule`'s epoch check and reported.
    func parkHeldSends(save: Bool = true, quitting: Bool = false) {
        let scheduled = Set(scheduling.keys)
        let parked = Array(heldSends) + Array(releasing) + Array(scheduling)
        heldSends = [:]
        releasing = [:]
        scheduling = [:]
        for (key, held) in parked {
            let verb = scheduled.contains(key) ? "not scheduled" : "not sent"
            undos.removeAll { $0.id == held.undoId }
            let state = withDetachedDraft(held.state)
            holding.remove(state.id)
            if !save {
                pushToast("\(verb): its account was removed", .error)
            } else if state.forwardOfMessageId != nil && !draftsV2Available {
                // Not saved on this daemon (see `DraftSaver.save`), so no
                // promise of one.
                if !quitting {
                    pushToast("\(verb): you switched accounts while it was waiting", .error)
                }
            } else {
                DraftSaver.shared.saveDetached(held.slot, state)
                if !quitting {
                    pushToast(
                        "\(verb): you switched accounts. It is in that account's drafts", .error)
                }
            }
        }
    }

    /// A reply typed in the reader went out and is already in the local store
    /// (the daemon echoed it), so that thread's reader refetches and shows it.
    /// The send outlives the composer now, so the reader cannot be handed a
    /// callback; it watches this instead.
    struct SendEcho: Equatable {
        let threadId: String
        let id = UUID()
    }
    private(set) var lastSendEcho: SendEcho?

    /// THE SEND: close the composer now, send in `sendHold` seconds unless the
    /// toast's undo (or `u`) puts it back first.
    ///
    /// There is no review step in front of this any more. The hold is what
    /// replaced it: a send you did not mean is undone by one keystroke, and the
    /// outbound guard still runs on the real send, reopening the composer with
    /// its verdict when it blocks.
    ///
    /// The draft is NOT flushed on the way out. The daemon's copy stays as the
    /// autosave left it until the send deletes it (`draftId` rides along), and
    /// a flush racing the send could put it back after the delete. An undo or a
    /// failure reopens the composer with everything, so nothing typed is lost
    /// either way.
    func sendWithUndo(_ slot: DraftSaver.Slot, override: Bool = false) {
        guard var state = slot == .compose ? compose : inlineReply, !state.sending else { return }
        state.error = nil
        DraftSaver.shared.noteSent(slot)
        switch slot {
        case .compose: compose = nil
        case .inlineReply: inlineReply = nil
        }
        let key = UUID()
        var held = HeldSend(
            state: state, slot: slot, override: override, threadId: threadId, epoch: epoch)
        // `messageId: -1`: this undo resolves nothing, so the id it un-hides
        // on firing must match no message.
        held.undoId = pushUndo(kind: .send, messageId: -1, label: Self.holdLabel(state)) {
            [weak self] in
            guard let self, await self.unhold(key) else { throw HoldError.alreadySent }
        }
        heldSends[key] = held
        holding.insert(state.id)
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(Self.sendHold))
            await self?.release(key)
        }
    }

    private enum HoldError: Error { case alreadySent }

    /// "sending to alice@example.com" — the toast names who it is for, because
    /// that is the question somebody reaching for undo is asking.
    private static func holdLabel(_ state: ComposeState) -> String {
        if state.groupMode == .individual, let name = state.groupName {
            return "sending to \(name)"
        }
        if let first = state.recipients.tokens(.to).first {
            let more = state.recipients.tokens(.to).count - 1
            return more > 0 ? "sending to \(first) +\(more)" : "sending to \(first)"
        }
        return state.replyToMessageId != nil ? "sending reply" : "sending"
    }

    /// Undo: the send never happens and the composer comes back. False when
    /// the hold already ended, so the toast says the undo failed rather than
    /// claiming a mail that left was stopped.
    private func unhold(_ key: UUID) -> Bool {
        guard var held = heldSends.removeValue(forKey: key) else { return false }
        held.state = withDetachedDraft(held.state)
        holding.remove(held.state.id)
        putBack(held, error: nil)
        return true
    }

    /// The hold ran out: the mail goes. Removed from `heldSends` FIRST and on
    /// the main actor, so an undo landing now finds nothing and cannot also
    /// reopen a composer for a mail that is on the wire.
    private func release(_ key: UUID) async {
        guard let waiting = heldSends.removeValue(forKey: key) else { return }
        undos.removeAll { $0.id == waiting.undoId }
        // A DRAFT SAVE STILL ON THE WIRE owns the id this send has to carry,
        // or the daemon keeps a draft of mail that went out. Wait for it, then
        // take what it brought back. Parked in `releasing` meanwhile, where an
        // account switch starting now still finds it.
        releasing[key] = waiting
        await DraftSaver.shared.settle()
        // Gone = an account switch parked it, and said so.
        guard var held = releasing.removeValue(forKey: key) else { return }
        held.state = withDetachedDraft(held.state)
        holding.remove(held.state.id)
        guard held.epoch == epoch else {
            // An account change that parked nothing (the gate teardown): the
            // daemon it was written for is not configured any more, so there
            // is nowhere to send it or save it. Say so, at least.
            pushToast("not sent: the account changed while it was waiting", .error)
            return
        }
        // Until the send returns, this reply or forward key still owns its
        // draft row (see `holdsDraft`). Exactly one of the two tables.
        let forwardKey = held.state.forwardOfMessageId
        let replyKey = forwardKey == nil ? held.state.replyToMessageId : nil
        if let forwardKey {
            firingForwardKeys.append(forwardKey)
        } else {
            firingReplyKeys.append(replyKey)
        }
        // And by id, which is how a new-message row is known (see `isHeld`).
        let firingId = held.state.draftId
        if let firingId { firingDraftIds.append(firingId) }
        defer {
            if let forwardKey {
                if let i = firingForwardKeys.firstIndex(of: forwardKey) {
                    firingForwardKeys.remove(at: i)
                }
            } else if let i = firingReplyKeys.firstIndex(of: replyKey) {
                firingReplyKeys.remove(at: i)
            }
            if let firingId, let i = firingDraftIds.firstIndex(of: firingId) {
                firingDraftIds.remove(at: i)
            }
        }
        let outcome = await ComposeSubmit.fire(held.state, override: held.override)
        // The account changed while it was on the wire: whatever came back
        // belongs to the OLD account, and its message ids mean nothing here.
        // Reopening it would point the retry at the new one, so the outcome
        // is only reported.
        guard held.epoch == epoch else {
            switch outcome {
            case .sent: pushToast("sent", .success)
            case .guardBlocked: pushToast("not sent: the outbound guard stopped it", .error)
            case .forbidden: pushToast("not sent: \(ComposeCopy.noWriteCredential)", .error)
            case .failure(let text): pushToast("not sent: \(text)", .error)
            }
            return
        }
        switch outcome {
        case .sent(let result):
            // The daemon resolved the replied-to update; without this the row
            // sits in its band until the next poll, reading as a no-op.
            if let repliedTo = held.state.replyToMessageId { noteResolved(repliedTo) }
            // The send took its draft row with it; the cached list drops it now
            // rather than offering sent mail until the next fetch. And a
            // tombstone, as a DELETE gets: a list that left before the send
            // returned still carries the row, and `isHeld` stopped covering it
            // the moment the send came back.
            if let sentDraft = held.state.draftId {
                draftsRows.removeAll { $0.id == sentDraft }
                noteDraftGone(sentDraft)
            }
            pushToast("sent", .success)
            // Only an INLINE reply names its thread: the pane's mail (a new
            // message, a forward) starts a thread of its own, and refetching
            // whatever happens to be open would move somebody's reading.
            if result.echo_message_id != nil, held.slot == .inlineReply,
                let thread = held.threadId
            {
                lastSendEcho = SendEcho(threadId: thread)
            }
        case .guardBlocked(let kinds):
            var state = held.state
            state.guardKinds = kinds
            putBack(HeldSend(
                state: state, slot: held.slot, override: false, threadId: held.threadId,
                epoch: held.epoch), error: nil, blocked: true)
        case .forbidden:
            putBack(held, error: ComposeCopy.noWriteCredential)
        case .failure(let text):
            putBack(held, error: text)
        }
    }

    /// Nothing in it a person put there: no draft behind it, fields and body
    /// as they opened, no files. Such a composer is REPLACED by a returning
    /// held send rather than counted as occupying its slot — the common case
    /// being `c` pressed for the next mail and then undo pressed for this one.
    /// A forward is never blank; it carries the original.
    private func isBlankComposer(_ c: ComposeState) -> Bool {
        c.draftId == nil && c.forwardOfMessageId == nil && c.recipients == c.seededRecipients
            && c.subject.isEmpty && Prefs.shared.isBodyUntouched(c.body) && c.attachments.isEmpty
    }

    /// Reopen a held send's composer: after an undo, or when the send came back
    /// refused.
    ///
    /// Back WHERE IT WAS when that place is still there — the pane, or the
    /// inline reply under the thread it was typed in. An inline reply renders
    /// only under its own thread, so one whose reader has moved on goes to the
    /// pane instead, which carries the reply shape too. A slot holding a
    /// BLANK composer counts as free (see `isBlankComposer`).
    ///
    /// With both slots holding real work nothing is overwritten, and it goes
    /// to the daemon as a draft instead, UNLESS an open composer shares its
    /// draft row. A reply's row is one per parent, so saving there would be
    /// overwritten by that composer's next keystroke, and "it is in your
    /// drafts" would be a lie told about mail about to be lost. Then the toast
    /// says it could not be kept. A forward's row is one per forwarded
    /// message, so the same holds for a forward of the same message open in
    /// the pane. New mail shares a row only on an older daemon's one nil-key
    /// slot; on a `drafts_v2` daemon it parks as a row of its own (by its id,
    /// or a fresh one), so it is always kept. A forward is saved only on a
    /// `drafts_v2` daemon (see `DraftSaver.save`), so elsewhere it cannot be
    /// kept either.
    private func putBack(_ held: HeldSend, error: String?, blocked: Bool = false) {
        guard held.epoch == epoch else { return }
        var state = held.state
        state.sending = false
        state.error = error
        let reason = blocked ? "the outbound guard stopped it" : error
        if held.slot == .inlineReply, threadId == held.threadId,
            inlineReply.map(isBlankComposer) ?? true
        {
            inlineReply = state
            DraftSaver.shared.noteChange(.inlineReply)
        } else if compose.map(isBlankComposer) ?? true {
            // Back into the pane's strip, so the side panel goes for the same
            // reason `openCompose` closes it: the two cannot share the strip.
            if sideView.isOpen { closeSide() }
            compose = state
            DraftSaver.shared.noteChange(.compose)
        } else {
            let sharesRow = [compose, inlineReply].compactMap { $0 }.contains { open in
                if let original = state.forwardOfMessageId {
                    return open.forwardOfMessageId == original
                }
                return (state.replyToMessageId != nil || !draftsV2Available)
                    && open.forwardOfMessageId == nil
                    && open.replyToMessageId == state.replyToMessageId
            }
            let savable = state.forwardOfMessageId == nil || draftsV2Available
            if savable && !sharesRow {
                DraftSaver.shared.saveDetached(held.slot, state)
                // The header's count says "it is in your drafts" too; it
                // settles first, so it sees this save land.
                Task { await refreshDrafts() }
                pushToast("not sent: \(reason ?? "undone"). It is in your drafts", .error)
            } else {
                pushToast(
                    "not sent: \(reason ?? "undone"). Another draft is open in its place, "
                        + "so it could not be kept", .error)
            }
            return
        }
        if let reason { pushToast("not sent: \(reason)", .error) }
    }

    // MARK: - scheduled send

    /// Whether the composers may offer a send time. Same shape and same reason
    /// as `composeAttachmentsAvailable`: a daemon without an outbox would 404
    /// the click, so every schedule door (the clock, its chord, the palette)
    /// is withheld. nil (no stats yet) counts as no. Stored: see `noteStats`.
    private(set) var scheduledSendAvailable = false

    /// Set (or, with nil, clear) when a composer's mail goes.
    ///
    /// NOT AN EDIT: no `DraftSaver.noteChange`, because `sendAt` is an intent
    /// and a draft saves words (see `DraftPayload`). Nor does it relock a guard
    /// override: the verdict judged the mail, and a time is not the mail.
    func setSendAt(_ slot: DraftSaver.Slot, _ date: Date?) {
        switch slot {
        case .compose:
            guard var next = compose else { return }
            next.sendAt = date
            next.error = nil
            compose = next
        case .inlineReply:
            guard var next = inlineReply else { return }
            next.sendAt = date
            next.error = nil
            inlineReply = next
        }
    }

    /// ⌘↵ WITH A TIME SET: the mail goes to the daemon's outbox NOW, and the
    /// daemon sends it then.
    ///
    /// NO HOLD. `sendWithUndo` holds a send five seconds because a sent mail
    /// has no other undo; a scheduled one is cancellable from Sent until it
    /// goes, so holding it would only be five seconds in which quitting the
    /// app could lose it. The composer closes, the POST leaves, and the toast
    /// that follows carries the undo: DELETE the row and reopen the composer.
    ///
    /// Everything that can refuse it (validation, the files, the outbound
    /// guard, the write credential) answers on this POST, while the sender is
    /// here, and every refusal reopens the composer through `putBack` exactly
    /// as a failed send does, time still set: a guard verdict makes ⌘⇧↵
    /// "schedule anyway".
    ///
    /// The draft is not flushed on the way out, for the reason `sendWithUndo`
    /// gives: the POST names it and the daemon deletes it in the same
    /// transaction that files the mail.
    func scheduleCompose(_ slot: DraftSaver.Slot, override: Bool = false) {
        guard let state = slot == .compose ? compose : inlineReply, !state.sending,
            let sendAt = state.sendAt
        else { return }
        // A time that went by while the composer sat open. The daemon would
        // refuse it; refusing here keeps the mail on screen.
        guard sendAt > Date() else {
            switch slot {
            case .compose: compose?.error = ScheduleCopy.timePassed
            case .inlineReply: inlineReply?.error = ScheduleCopy.timePassed
            }
            return
        }
        var closing = state
        closing.error = nil
        DraftSaver.shared.noteSent(slot)
        switch slot {
        case .compose: compose = nil
        case .inlineReply: inlineReply = nil
        }
        if schedulePicker == slot { schedulePicker = nil }
        let held = HeldSend(
            state: closing, slot: slot, override: override, threadId: threadId, epoch: epoch)
        // Parked in `scheduling` until the POST leaves, where an account
        // switch or a quit starting now still finds it (see `parkHeldSends`).
        let key = UUID()
        scheduling[key] = held
        Task { [weak self] in await self?.fireSchedule(key, at: sendAt) }
    }

    private func fireSchedule(_ key: UUID, at sendAt: Date) async {
        // A DRAFT SAVE STILL ON THE WIRE owns the id this POST has to name, or
        // the daemon files the mail and leaves its draft behind (a new-message
        // create that lands after the POST would be a second copy of mail
        // that is already scheduled). Same wait `release` makes.
        await DraftSaver.shared.settle()
        // Gone = an account switch or a quit parked it as a draft, and said so.
        guard let held = scheduling.removeValue(forKey: key) else { return }
        guard held.epoch == epoch else {
            pushToast("not scheduled: the account changed while it was on its way", .error)
            return
        }
        // ON THE WIRE from here to the end: ⌘Q waits for the answer (see
        // `outboxOnWire`), because a POST that dies puts the mail back in a
        // composer, and that composer is the only copy of the last keystrokes.
        outboxOnWire += 1
        defer { outboxOnWire -= 1 }
        var state = held.state
        // The id `DraftSaver` knows, not the slot's copy: a create that
        // landed after the composer closed is only in its `resolved` map.
        let draftId = DraftSaver.shared.currentDraftId(of: state)
        state.draftId = draftId
        // Until the POST returns, this composer's draft row is mail on its
        // way out: the drafts page must not offer it, and `r` on the same
        // parent must not restore it (see `isHeld`, `holdsDraft`). The same
        // three tables a firing send uses.
        let forwardKey = state.forwardOfMessageId
        let replyKey = forwardKey == nil ? state.replyToMessageId : nil
        if let forwardKey { firingForwardKeys.append(forwardKey) } else { firingReplyKeys.append(replyKey) }
        if let draftId { firingDraftIds.append(draftId) }
        let outcome = await ComposeSubmit.schedule(
            state, at: sendAt, override: held.override, draftId: draftId)
        if let forwardKey {
            if let i = firingForwardKeys.firstIndex(of: forwardKey) { firingForwardKeys.remove(at: i) }
        } else if let i = firingReplyKeys.firstIndex(of: replyKey) {
            firingReplyKeys.remove(at: i)
        }
        if let draftId, let i = firingDraftIds.firstIndex(of: draftId) { firingDraftIds.remove(at: i) }
        // The account changed while it was on the wire: the outcome belongs
        // to the OLD account and is only reported, as `release` does.
        guard held.epoch == epoch else {
            switch outcome {
            case .scheduled: pushToast(ScheduleCopy.scheduledToast(sendAt), .success)
            case .guardBlocked: pushToast("not scheduled: the outbound guard stopped it", .error)
            case .forbidden(let text), .failure(let text):
                pushToast("not scheduled: \(text)", .error)
            }
            return
        }
        let back = HeldSend(
            state: state, slot: held.slot, override: false, threadId: held.threadId,
            epoch: held.epoch)
        switch outcome {
        case .scheduled(let created):
            // The schedule consumed its draft row: drop it from the cache and
            // tombstone it, as a send does, so a list in the air cannot
            // offer mail that is already in the outbox.
            if let draftId {
                draftsRows.removeAll { $0.id == draftId }
                noteDraftGone(draftId)
            }
            // A new row in the outbox: the Sent page and the tick look again.
            noteOutboxChanged()
            // The listing has no `group_id`, so a fan-out's row would have no
            // name to show; this app knows it, for as long as it runs.
            scheduledGroupNames.set(
                created.id, state.groupMode == .individual ? state.groupName : nil)
            let e = epoch
            // `messageId: -1`: this undo resolves nothing on the sitrep.
            pushUndo(kind: .schedule, messageId: -1, label: ScheduleCopy.scheduledToast(sendAt)) {
                [weak self] in
                guard let self else { throw HoldError.alreadySent }
                try await self.unschedule(created.id, held: back, sendAt: sendAt, epoch: e)
            }
        case .guardBlocked(let kinds):
            var blocked = back
            blocked.state.guardKinds = kinds
            putBack(blocked, error: nil, blocked: true)
        case .forbidden(let text), .failure(let text):
            putBack(back, error: text)
        }
    }

    /// THE SCHEDULE'S UNDO: take the row back out of the outbox and reopen
    /// its composer, rebuilt from what the daemon was going to send
    /// (`ComposeState.reopened`) with the time still set, through the same
    /// `putBack` a failed send takes. Its files come back in the tray; the
    /// cancel released their claim and the reopened composer's autosave
    /// (armed by `putBack`) claims them again.
    ///
    /// A 409 is the worker having claimed the row already: the mail is going,
    /// and `fireUndo` says "already sent" (`HoldError.alreadySent`, the same
    /// signal a send whose hold ran out gives).
    private func unschedule(_ id: Int, held: HeldSend, sendAt: Date, epoch e: Int) async throws {
        let cancelled: ScheduledCancel
        do {
            cancelled = try await APIClient.shared.cancelScheduled(id)
        } catch {
            if ScheduleUndo.verdict(error) == .alreadySent { throw HoldError.alreadySent }
            throw error
        }
        guard e == epoch else { return }
        // Off the Sent page too, if a listing already carried it.
        scheduledRows.removeAll { $0.id == id }
        scheduledTombstones.gone(id)
        // A request the daemon could no longer parse comes back null. The row
        // is cancelled either way, so reopen from what this app remembers
        // rather than lose the mail: the same builder, fed the body this
        // composer would have sent.
        let request =
            cancelled.request
            ?? (try? JSONDecoder().decode(
                ScheduledRequest.self,
                from: JSONEncoder().encode(
                    SendBody(compose: held.state, override: held.override, draftId: nil))))
            ?? ScheduledRequest()
        var back = held
        back.state = ComposeState.reopened(
            request: request, attachments: cancelled.attachments, sendAt: sendAt, base: held.state)
        putBack(back, error: nil)
    }

    // MARK: - the outbox, on the Sent page

    /// The daemon's outbox as last fetched (`scheduled`, `sending`, `failed`),
    /// next to go first, MINUS any row the reader has pulled back or cancelled
    /// (see `scheduledTombstones`). The Sent page pins these above the sent
    /// mail. Filled by `refreshScheduled`, which `refreshSent` runs beside the
    /// sent listing so one Sent refresh fetches both.
    private(set) var scheduledRows: [ScheduledSendView] = []
    /// Freshness + in-flight fetch, the outbox's half of `sentLoadedAt`/`sentRefresh`.
    private var scheduledLoadedAt: Date?
    private var scheduledRefresh: Task<Void, Never>?
    /// Outbox ids that left the daemon this account session (cancelled, or
    /// pulled back into a composer). `scheduled_sends.id` is reused like
    /// `drafts.id`, so this is the generation-tagged ledger, NEVER a permanent
    /// set; the rule is `Tombstones`.
    private var scheduledTombstones = Tombstones()
    /// Which failed rows have toasted, for this account: loaded from and saved
    /// to `scheduledFailuresKey` by `freshFailures`, so a failure toasts once,
    /// not once per launch. `scheduledFailuresAccount` is whose they are; nil
    /// = not loaded yet (a fresh launch, or an account switch).
    private var scheduledFailures = ScheduledFailures()
    private var scheduledFailuresAccount: UUID?
    private static let scheduledFailuresKeyBase = "passband.scheduled.failuresToasted"
    /// The group name of each fan-out this app scheduled, by outbox id, for
    /// as long as the listing carries the row (`ScheduledGroupNames`).
    private(set) var scheduledGroupNames = ScheduledGroupNames()
    /// The last listing came back EMPTY and nothing has been scheduled since:
    /// the sitrep tick has nothing to watch and skips the request. Everything
    /// that can put a row in the outbox clears it.
    private var outboxKnownEmpty = false
    /// Rows with a pull-back, cancel or send-now on the wire. A second Enter
    /// on the same row while its DELETE is out would fire a second DELETE and
    /// report a 404 for mail that is sitting in a composer.
    private var scheduledBusy: Set<Int> = []

    /// Same TTL-plus-join shape as `refreshSent`, for the same reasons. A
    /// daemon without an outbox is never asked (the route would 404).
    func refreshScheduled(force: Bool = false) async {
        guard scheduledSendAvailable else { return }
        if !force, let loadedAt = scheduledLoadedAt,
            Date().timeIntervalSince(loadedAt) < Self.mailTTL
        {
            return
        }
        if let running = scheduledRefresh {
            await running.value
            if !force { return }
        }
        let refresh = Task { await performScheduledRefresh() }
        scheduledRefresh = refresh
        await refresh.value
        if scheduledRefresh == refresh { scheduledRefresh = nil }
    }

    /// The sitrep tick's ask, so a row that FAILS while the person is
    /// elsewhere still toasts. There is no event for it (docs/SCHEDULED-SEND.md
    /// §5: no SSE), and the Sent page only refreshes while it is on screen.
    /// Slowed to one ask per `ScheduledPoll.emptyEvery` once the outbox is
    /// known empty, so an account that never schedules anything pays one
    /// request per five minutes, not one per tick, while a row another device
    /// scheduled still turns up (and toasts, if it fails).
    func pollScheduled() async {
        guard scheduledSendAvailable,
            ScheduledPoll.due(knownEmpty: outboxKnownEmpty, loadedAt: scheduledLoadedAt)
        else { return }
        await refreshScheduled()
    }

    /// `scheduledFailures.fresh`, against the ledger of the account live NOW,
    /// written back when it changed. Scoped like the other per-account
    /// ledgers (`RecentSearchStore`): ids mean nothing across daemons.
    private func freshFailures(_ rows: [ScheduledSendView]) -> [ScheduledSendView] {
        guard let account = AccountManager.shared.activeId else {
            return scheduledFailures.fresh(rows)
        }
        let key = AccountIndex.scopedKey(Self.scheduledFailuresKeyBase, account)
        if scheduledFailuresAccount != account {
            scheduledFailures = ScheduledFailures(
                keys: UserDefaults.standard.stringArray(forKey: key) ?? [])
            scheduledFailuresAccount = account
        }
        let fresh = scheduledFailures.fresh(rows)
        if !fresh.isEmpty { UserDefaults.standard.set(scheduledFailures.keys, forKey: key) }
        return fresh
    }

    private func performScheduledRefresh() async {
        let e = epoch
        let start = scheduledTombstones.beginFetch()
        let namesSince = scheduledGroupNames.generation
        let listed = try? await APIClient.shared.listScheduled()
        // Filtered while this fetch's own entry still holds its tombstones in
        // place, then released, on EVERY path (an old account's fetch too).
        let kept = listed?.filter { !scheduledTombstones.hides($0.id, fetchedAt: start) }
        scheduledTombstones.endFetch(start)
        // Silent on failure, like the drafts cache: the rows on screen stay.
        guard e == epoch, let kept else { return }
        let next = ScheduledRowCopy.ordered(kept)
        // Assigned only on a real change: the tick re-runs this.
        if next != scheduledRows { scheduledRows = next }
        // A name whose row left the listing goes with it: the id is reused.
        // From `listed`, not `kept`: a tombstoned row is not gone yet.
        scheduledGroupNames.prune(listed: Set((listed ?? []).map(\.id)), since: namesSince)
        // A PREVIEWED ROW THAT LEFT: sent by the worker, or cancelled from
        // another device. The preview would otherwise vanish mid-read with
        // nothing said; this says why.
        if let id = scheduledPreviewId, !next.contains(where: { $0.id == id }) {
            scheduledPreviewId = nil
            pushToast(ScheduledRowCopy.noLongerScheduled)
        }
        scheduledLoadedAt = Date()
        outboxKnownEmpty = listed?.isEmpty ?? false
        // ONCE PER ROW. The first listing of a run toasts a row that failed
        // while the app was closed, which is the point: nobody has been told
        // yet. One that already toasted, in this run or an earlier one, stays
        // quiet.
        for row in freshFailures(next) {
            pushToast(ScheduledRowCopy.failedToast(row), .error)
        }
    }

    /// Something was just put in (or back in) the outbox: the tick must look
    /// again, and the next Sent refresh must not be swallowed by the TTL.
    private func noteOutboxChanged() {
        outboxKnownEmpty = false
        scheduledLoadedAt = nil
    }

    /// Take a row off the page NOW, before its DELETE lands, and hide it from
    /// any listing in the air. Returns where it sat, for putting it back.
    private func holdScheduledRow(_ id: Int) -> Int {
        let index = scheduledRows.firstIndex { $0.id == id } ?? scheduledRows.count
        scheduledRows.removeAll { $0.id == id }
        scheduledTombstones.leaving(id)
        scheduledBusy.insert(id)
        return index
    }

    /// The DELETE did not take the row: it is still on the daemon, so it goes
    /// back where it was rather than hiding until a listing says otherwise.
    private func restoreScheduledRow(_ row: ScheduledSendView, at index: Int) {
        scheduledTombstones.revive(row.id)
        if !scheduledRows.contains(where: { $0.id == row.id }) {
            scheduledRows.insert(row, at: min(index, scheduledRows.count))
        }
    }

    /// What a failed outbox verb says, and whether the row is still there.
    /// 409 is the worker holding it (Gmail's hands); 404 is sent, cancelled
    /// elsewhere, or swept: either way no longer scheduled.
    private func reportScheduledFailure(_ error: Error, _ fallback: String) -> Bool {
        let api = error as? APIError
        if api?.status == 409 {
            pushToast(ScheduledRowCopy.alreadySending, .error)
            return true
        }
        if api?.kind == .notFound {
            pushToast(ScheduledRowCopy.noLongerScheduled, .error)
            return false
        }
        pushToast(errText(error, fallback), .error)
        return true
    }

    /// The preview's `edit` (and the row's context-menu "edit"): PULL IT BACK
    /// into the composer it came from. DELETE the row, and open the request it
    /// hands back with its time still set, so ⌘↵ reschedules and Esc keeps it
    /// in drafts. Enter / click on the row only PREVIEWS it
    /// (`openScheduledPreview`); this is the one door that takes it out of the
    /// outbox.
    ///
    /// `openDraft`'s branching, fed from the request instead of a `DraftView`:
    ///
    /// - A FORWARD needs the original it forwards, for the composer to show
    ///   under the note. Fetched FIRST, before the DELETE, so nothing slow
    ///   stands between the row leaving the outbox and the composer opening.
    ///   A failed fetch still opens the forward, without the quote on
    ///   screen: the daemon quotes it from the id either way, and the mail is
    ///   already out of the outbox, so there is nowhere else for it to go.
    /// - A REPLY opens where it belongs: its thread, with the inline reply
    ///   filled. One whose parent this daemon no longer describes (no
    ///   `thread_id`) opens in the pane instead, which takes replies (the
    ///   agent's hand-off does the same), rather than being lost.
    /// - NEW MAIL loads into the pane. One already open there is closed
    ///   through `closeCompose`, which keeps it in drafts, as `openDraft` does.
    ///
    /// A `sending` row takes none of this (nor `s` nor `x`): it is in Gmail's
    /// hands, and the daemon would 409 each of them anyway.
    func pullBackScheduled(_ row: ScheduledSendView) {
        guard row.state != "sending", !scheduledBusy.contains(row.id) else { return }
        let e = epoch
        let index = holdScheduledRow(row.id)
        Task {
            let base = await forwardBase(row.forward_of_message_id)
            guard e == epoch else { return }
            // ON THE WIRE until the composer holds it: ⌘Q waits for the
            // answer, which is the mail (see `outboxOnWire`).
            outboxOnWire += 1
            defer { outboxOnWire -= 1 }
            let cancelled: ScheduledCancel
            do {
                cancelled = try await APIClient.shared.cancelScheduled(row.id)
            } catch {
                guard e == epoch else { return }
                scheduledBusy.remove(row.id)
                if reportScheduledFailure(error, "could not pull that back") {
                    restoreScheduledRow(row, at: index)
                } else {
                    scheduledTombstones.gone(row.id)
                }
                await refreshScheduled(force: true)
                return
            }
            // The account changed while the DELETE was out: the row is
            // cancelled on the daemon just left, and this mailbox's composer
            // is no place for it.
            guard e == epoch else {
                pushToast("the account changed: that mail was cancelled, not reopened", .error)
                return
            }
            scheduledBusy.remove(row.id)
            scheduledTombstones.gone(row.id)
            // NOTHING AWAITED FROM HERE TO THE OPEN: the row is gone from the
            // daemon and this is the only copy of the mail until a composer
            // holds it (see `openPulledBack`). The group comes after.
            let request = cancelled.request ?? Self.fallbackRequest(row)
            var state = ComposeState.reopened(
                request: request, attachments: cancelled.attachments,
                sendAt: Fmt.date(row.send_at), base: base)
            // Said before anyone presses a key: an earlier try that may have
            // reached Gmail, or a time already gone by (a failed row's).
            state.error = ScheduledCancelUndo.reopenError(row)
            scheduledGroupNames.take(row.id)
            openPulledBack(state, threadId: row.thread_id, request: request, fetchBase: false)
        }
    }

    // MARK: - the read-only preview

    /// The outbox row open READ-ONLY in the reader's slot
    /// (Views/ScheduledPreview.swift), by id so the preview follows the row
    /// through each poll (a retry, a failure) instead of freezing the copy it
    /// opened with. Mutually exclusive with `threadId`: `openThread` clears
    /// it, and `closeThread` (every way out of the reader) does too.
    private(set) var scheduledPreviewId: Int?

    /// The previewed row as last listed. nil while none is open, and for a
    /// row that has left the listing (the refresh closes the preview then).
    var scheduledPreviewRow: ScheduledSendView? {
        guard let id = scheduledPreviewId else { return nil }
        return scheduledRows.first { $0.id == id }
    }

    /// Enter / click on an outbox row: SHOW it, as scheduled (user decision
    /// 2026-09-29). Nothing about opening it cancels it, pulls it into a
    /// composer or moves it to drafts; only the preview's `edit` does that.
    /// A `sending` row opens too: it has a status to show, just no verbs.
    func openScheduledPreview(_ row: ScheduledSendView) {
        if threadId != nil { closeThread() }
        #if os(macOS)
            // A side-strip composer holding the caret would keep the preview's
            // keys (KeyMonitor.isEditing): Enter would type a newline into it.
            // Its words stay; only the focus moves.
            NSApp.keyWindow?.makeFirstResponder(nil)
        #endif
        scheduledPreviewId = row.id
    }

    /// Whether an outbox verb would be refused right now, SAID when it is: a
    /// `sending` row is in Gmail's hands, and one whose last verb is still
    /// on the wire is `scheduledBusy`. The preview asks this BEFORE it closes,
    /// so a refused verb leaves the preview open with a reason, rather than
    /// closing it and doing nothing.
    func refusesScheduledVerb(_ row: ScheduledSendView) -> Bool {
        if row.state == "sending" {
            pushToast(ScheduledRowCopy.alreadySending)
            return true
        }
        if scheduledBusy.contains(row.id) {
            pushToast(ScheduledRowCopy.stillWorking)
            return true
        }
        return false
    }

    /// Esc on the preview, or the way out after one of its verbs. The mail
    /// stays exactly where it was.
    func closeScheduledPreview() {
        scheduledPreviewId = nil
    }

    /// The group a request's `group_id` names, as the daemon describes it now:
    /// what `ComposeState.reopened` rebuilds a fan-out's pill from. nil for a
    /// request without one, and for a group that is gone or unreachable.
    private func groupOf(_ request: ScheduledRequest) async -> SendGroup? {
        guard let id = request.group_id else { return nil }
        return try? await APIClient.shared.group(id)
    }

    /// `s` on an outbox row: send it on the worker's next tick. 202 is QUEUED,
    /// not sent, so the toast says "sending now" and the listing shows it go.
    func sendScheduledNow(_ row: ScheduledSendView) {
        guard row.state != "sending", !scheduledBusy.contains(row.id) else { return }
        let e = epoch
        scheduledBusy.insert(row.id)
        Task {
            do {
                try await APIClient.shared.sendScheduledNow(row.id)
                guard e == epoch else { return }
                pushToast(ScheduledRowCopy.sendingNow, .success)
            } catch {
                guard e == epoch else { return }
                _ = reportScheduledFailure(error, "could not send it now")
            }
            scheduledBusy.remove(row.id)
            await refreshScheduled(force: true)
        }
    }

    /// `x` / Backspace on an outbox row: cancel it outright, with the app's
    /// five-second undo.
    ///
    /// NOT HELD, unlike a draft delete, and deliberately: a held cancel of a
    /// row due inside the window would let the worker send it while the
    /// person watches the toast that says it is cancelled. So the DELETE goes
    /// out now and the undo puts the mail back, from the request the DELETE
    /// handed back (`uncancelScheduled`). The toast appears when the DELETE
    /// lands, because until then there is nothing to undo.
    func cancelScheduledRow(_ row: ScheduledSendView) {
        guard row.state != "sending", !scheduledBusy.contains(row.id) else { return }
        let e = epoch
        let index = holdScheduledRow(row.id)
        Task {
            let cancelled: ScheduledCancel
            do {
                cancelled = try await APIClient.shared.cancelScheduled(row.id)
            } catch {
                guard e == epoch else { return }
                scheduledBusy.remove(row.id)
                if reportScheduledFailure(error, "could not cancel that") {
                    restoreScheduledRow(row, at: index)
                } else {
                    scheduledTombstones.gone(row.id)
                }
                await refreshScheduled(force: true)
                return
            }
            guard e == epoch else { return }
            scheduledBusy.remove(row.id)
            scheduledTombstones.gone(row.id)
            // The fan-out's name rides with the undo: the row's id is free
            // now, and a listing inside the undo window would drop it.
            let groupName = scheduledGroupNames.take(row.id)
            // `messageId: -1`: this undo resolves nothing on the sitrep, and
            // an outbox id shares the Int space with message ids.
            pushUndo(
                kind: .scheduledCancel, messageId: -1,
                label: ScheduledRowCopy.cancelledLabel(row)
            ) { [weak self] in
                guard let self else { throw HoldError.alreadySent }
                try await self.uncancelScheduled(row, cancelled, groupName: groupName, epoch: e)
            }
        }
    }

    /// THE CANCEL'S UNDO: put the mail back in the outbox at the time it had,
    /// by re-POSTing the request the DELETE returned (a new row, a new id;
    /// the files it held are re-claimed by the same POST).
    ///
    /// NEVER SOONER THAN ASKED. A time already here (a failed row, or one due
    /// inside the undo window) is not re-POSTed, since the daemon refuses the
    /// past and "now" would make an undo send mail. That mail reopens in the
    /// composer, time shown and flagged, for the person to pick again. A
    /// re-POST the daemon refuses (the guard judges it afresh, since a
    /// verdict does not survive a cancel) reopens it the same way, so an
    /// undo that could not reschedule never costs the mail.
    private func uncancelScheduled(
        _ row: ScheduledSendView, _ cancelled: ScheduledCancel, groupName: String?, epoch e: Int
    ) async throws {
        guard e == epoch else { throw HoldError.alreadySent }
        let request = cancelled.request ?? Self.fallbackRequest(row)
        let sendAt = Fmt.date(row.send_at)
        // A cancelled row as the composer it reopens into. Built and opened
        // in ONE TURN, nothing awaited in between: the forwarded original
        // and the fan-out's group arrive after (`openPulledBack`).
        func reopen(_ adjust: (inout ComposeState) -> Void) {
            var state = ComposeState.reopened(
                request: request, attachments: cancelled.attachments, sendAt: sendAt)
            adjust(&state)
            openPulledBack(state, threadId: row.thread_id, request: request, fetchBase: true)
        }
        // NOT A ROW GMAIL MAY ALREADY HAVE. The daemon searched Gmail before
        // sending such a row again; the re-POST would be a new row with no
        // history, sent blind. It reopens instead, saying why.
        if !ScheduledCancelUndo.mayHaveReachedGmail(row), let sendAt,
            ScheduledCancelUndo.canReschedule(at: sendAt)
        {
            // ON THE WIRE until it lands or the composer holds the mail: ⌘Q
            // waits for it (see `outboxOnWire`).
            outboxOnWire += 1
            defer { outboxOnWire -= 1 }
            do {
                let created = try await APIClient.shared.scheduleSend(
                    ScheduledCancelUndo.body(request), at: sendAt)
                guard e == epoch else { return }
                scheduledGroupNames.set(created.id, groupName)
                noteOutboxChanged()
                await refreshScheduled(force: true)
                return
            } catch {
                guard e == epoch else { throw error }
                // RETURNS, not rethrows: the mail is back on screen and its
                // composer says what went wrong. An "undo failed" toast beside
                // it would be a second, contradicting account of one event.
                reopen { state in
                    if let api = error as? APIError, api.kind == .guardBlocked {
                        // As `putBack` does for a blocked send: the verdict
                        // shows, and ⌘⇧↵ reads "schedule anyway".
                        state.guardKinds = api.guardKinds ?? parseGuardKinds(api.message)
                    } else {
                        state.error = errText(error, "could not schedule it again")
                    }
                }
                return
            }
        }
        reopen { state in
            state.error = ScheduledCancelUndo.reopenError(row) ?? ScheduleCopy.timePassed
        }
    }

    /// The display half of a forward's composer (the original under the note,
    /// and the subject it is titled from), fetched by id. nil for anything
    /// that is not a forward, and for an original no longer here.
    private func forwardBase(_ original: Int?) async -> ComposeState? {
        guard let original,
            let envelope = try? await APIClient.shared.getMessage(original),
            let message = envelope.thread.messages.first(where: { $0.id == envelope.message_id })
        else { return nil }
        var base = ComposeState()
        let subject = (message.subject ?? envelope.thread.subject).trimmed
        base.forwardedSubject = subject.isEmpty ? nil : subject
        base.forwardedMessage = message
        return base
    }

    /// A cancel whose request the daemon could not parse answers `null`. The
    /// listing row is the next best record of the mail: its words, its parent,
    /// its files and whatever copy lists it states (a daemon older than the
    /// row's `cc`/`bcc` says nothing, and a blank list is left unstated).
    /// (Such a row fails as `failed:request` at fire time anyway.)
    private static func fallbackRequest(_ row: ScheduledSendView) -> ScheduledRequest {
        var r = ScheduledRequest()
        r.reply_to_message_id = row.reply_to_message_id
        r.forward_of_message_id = row.forward_of_message_id
        r.to = row.to.isEmpty ? nil : row.to
        if let cc = row.cc, !cc.isEmpty { r.cc = cc }
        if let bcc = row.bcc, !bcc.isEmpty { r.bcc = bcc }
        r.subject = row.subject.isEmpty ? nil : row.subject
        r.body = row.body
        r.body_format = "markdown"
        r.attachment_ids = row.attachments.map(\.id)
        return r
    }

    /// Open a composer rebuilt from the outbox. The reply goes into its
    /// thread's inline composer, anything else into the pane, and in both
    /// the autosave is ARMED (`noteChange`, as `putBack` does): the cancel
    /// released the files' claim, and the draft the save writes is what
    /// claims them again before the unclaimed sweep can.
    ///
    /// SYNCHRONOUS, ON PURPOSE. It runs in the same turn the DELETE's answer
    /// arrived in: until a composer holds the mail, the mail is nowhere (the
    /// row is gone, its files unclaimed). A composer is what the account
    /// switch flushes and what ⌘Q flushes; a local variable across an `await`
    /// is neither, and an earlier version lost the mail to a switch or a quit
    /// in exactly that gap (the group lookup, the drafts refresh and its
    /// settle). Everything slow runs AFTER, in `finishPulledBack`.
    ///
    /// ALWAYS ARMED, EVEN OVER SOMEBODY'S WORDS. A reply's draft row is one
    /// per parent, a forward's one per original (and new mail's one, on a
    /// daemon without `drafts_v2`), so the save PUTs this mail over an unsent
    /// draft to the same thread. The mail is already out of the outbox, and a
    /// save held for the first keystroke made Esc-without-typing lose it, so
    /// the scheduled mail WINS and a toast says what it replaced
    /// (`PullBackDraft.replacedToast`). A pane composer replying to the same
    /// parent is closed FIRST, so its flush goes out before this mail's save
    /// and cannot land on top of it.
    ///
    /// `fetchBase`: a forward reopened without its original on screen (the
    /// undo's path) fetches it after, and it is patched in.
    private func openPulledBack(
        _ state: ComposeState, threadId: String?, request: ScheduledRequest, fetchBase: Bool
    ) {
        // The composers holding words BEFORE this one takes a slot: their
        // close flush writes the same keyed row, so they are rivals.
        let rivals = [compose, inlineReply].compactMap { $0 }.filter(DraftSaver.hasContent)
        switch PullBackDraft.route(state, threadId: threadId, pane: compose) {
        case .inline(let threadId, let closePane):
            if closePane { closeCompose() }
            // Through `openThread`, which flushes any inline reply already
            // open (it is somebody's words) before this one takes the slot.
            openThread(threadId, from: .reply)
            inlineReply = state
            DraftSaver.shared.noteChange(.inlineReply)
        case .pane:
            // Through `closeCompose`, so the one being replaced says "kept in
            // drafts" exactly when an Esc on it would have.
            closeCompose()
            if sideView.isOpen { closeSide() }
            compose = state
            DraftSaver.shared.noteChange(.compose)
        }
        Analytics.capture("compose_opened", ["kind": state.analyticsKind])
        let e = epoch
        Task { [weak self] in
            await self?.finishPulledBack(
                state, request: request, rivals: rivals, fetchBase: fetchBase, epoch: e)
        }
    }

    /// The pulled-back composer's slow half, run once it is on screen: say
    /// what it replaced, then lend it the group's pill and the forwarded
    /// original. Each lands only on a composer that is STILL the pulled-back
    /// one (`PullBackDraft.stillOpen`); one sent, closed or replaced in the
    /// meantime is left alone.
    ///
    /// The drafts list is fetched FIRST and without `settle`: this mail's
    /// own save is a second away (the debounce), and a list taken after it
    /// landed would show this mail on the keyed row, not the words it
    /// replaced. The row it did write, once known, is left out
    /// (`ownDraftId`); a rival still flushing is in `rivals`.
    private func finishPulledBack(
        _ state: ComposeState, request: ScheduledRequest, rivals: [ComposeState],
        fetchBase: Bool, epoch e: Int
    ) async {
        let keyedNewMail = !draftsV2Available
        if PullBackDraft.keyed(state, keyedNewMail: keyedNewMail) {
            let rows = await fetchDrafts()
            guard e == epoch else { return }
            if PullBackDraft.stillOpen(state.id, pane: compose, inline: inlineReply) != nil,
                let toast = PullBackDraft.replacedToast(
                    state, drafts: rows ?? [], open: rivals,
                    ownDraftId: DraftSaver.shared.currentDraftId(of: state),
                    keyedNewMail: keyedNewMail)
            {
                pushToast(toast)
            }
        }
        let group = await groupOf(request)
        let base = fetchBase ? await forwardBase(state.forwardOfMessageId) : nil
        guard e == epoch, group != nil || base != nil else { return }
        func patch(_ open: inout ComposeState) {
            if let group { open.adoptGroup(group, request: request) }
            if let base, open.forwardedMessage == nil {
                open.forwardedMessage = base.forwardedMessage
                open.forwardedSubject = base.forwardedSubject
            }
        }
        switch PullBackDraft.stillOpen(state.id, pane: compose, inline: inlineReply) {
        case .pane?:
            if var open = compose {
                patch(&open)
                if open != compose { compose = open; DraftSaver.shared.noteChange(.compose) }
            }
        case .inline?:
            if var open = inlineReply {
                patch(&open)
                if open != inlineReply {
                    inlineReply = open
                    DraftSaver.shared.noteChange(.inlineReply)
                }
            }
        case nil:
            break
        }
    }

    // MARK: - assistant relay

    /// Whether the ⌘K assistant can go through the daemon instead of a local
    /// key. Read off the sitrep stats — /client/stats is already the connect
    /// probe and SitrepPoller keeps it fresh, so no extra fetch exists for
    /// this. A self-host daemon never says `assistant_relay`, and nil means
    /// the switch is simply not offered (same posture as `trackingAvailable`).
    /// Stored: see `noteStats`.
    private(set) var relayAvailable = false

    // MARK: - gmail connection

    /// Whether this mailbox's Gmail credential has stopped working.
    ///
    /// `== false` and not `!= true` on purpose, and it is the whole posture of
    /// this flag: nil means the daemon did not say (too old, or a door wired
    /// without a metrics registry), and a daemon that cannot see the credential
    /// must not have its silence rendered as an alarm. A false alarm here sends
    /// somebody through a Google consent screen for nothing.
    /// Both boards ask at the top of their bodies. Stored: see `noteStats`.
    private(set) var gmailDisconnected = false

    /// A mailbox catch-up in flight, as `(done, total)`.
    ///
    /// Nil is the normal state. When it is not nil, triage is blocked upstream
    /// and anything waiting on a verdict is waiting on this — which is the one
    /// thing a stalled-looking progress bar needs to be able to say.
    var catchUp: (done: Int, total: Int)? {
        guard let c = stats?.catch_up, c.total > 0 else { return nil }
        return (c.done, c.total)
    }

    /// Where to re-consent, when the daemon offered a link.
    ///
    /// Hosted only. A self-host mailbox is repaired with `squelchd auth` at a
    /// shell, so nil is the branch that shows the instruction instead of a
    /// button, never an error.
    var gmailReconnectURL: String? {
        guard let raw = stats?.gmail?.reconnect_url, Opener.isHTTP(raw) else { return nil }
        return raw
    }

    /// A reconnect the human has pressed, parked on "which Google account".
    ///
    /// Google's account chooser lists every account signed in to the browser,
    /// and nothing on it says which one this mailbox is. Pick the wrong one and
    /// the consent succeeds for a mailbox Passband is not serving, so the
    /// banner stays up and it looks like reconnecting does not work. One line
    /// naming the address before the browser opens is the whole fix.
    var reconnectAsk: ReconnectAsk?

    struct ReconnectAsk: Hashable {
        let url: String
        let email: String
    }

    /// The banner's Reconnect. Asks first when the daemon told us the address;
    /// on a daemon too old to say, there is nothing true to put in the dialog,
    /// so it opens the link exactly as it did before.
    func askReconnect() {
        guard let url = gmailReconnectURL else { return }
        let email = stats?.account_email?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !email.isEmpty else { return Opener.open(url) }
        reconnectAsk = ReconnectAsk(url: url, email: email)
    }

    func cancelReconnectAsk() { reconnectAsk = nil }

    func confirmReconnect() {
        guard let ask = reconnectAsk else { return }
        reconnectAsk = nil
        Opener.open(ask.url)
    }

    /// How long the mailbox has been dark, for the banner's subtitle.
    ///
    /// `Fmt.date` and not a formatter of its own: the daemon emits fractional
    /// seconds on some fields and not others, and a bare `ISO8601DateFormatter`
    /// parses only one of those shapes — silently returning nil for the other,
    /// which here would quietly drop "expired 3 hours ago" and leave the vaguer
    /// sentence with nothing announcing the loss. `Fmt` tries both, memoizes,
    /// and holds the lock these formatters need; this is read every render.
    var gmailDisconnectedSince: Date? { Fmt.date(stats?.gmail?.disconnected_since) }

    // MARK: - sync status

    /// THE one answer to "is Passband working": daemon reachability, Gmail
    /// credential, Gmail throttling or errors, catch-up and triage blockers,
    /// in that precedence. Every surface that talks about sync health reads
    /// this rather than re-deriving it. See Model/SyncStatus.swift.
    /// STORED, for the reason the values in `noteStats` are: the board's
    /// banner, its status line and the empty-board hero all ask. Re-derived
    /// whenever an input moves (the stats, the refresh error, the refresh
    /// time: everything `derive` and `daemonDown` read) and written only when
    /// the answer differs, which in a healthy mailbox is never.
    private(set) var syncStatus: SyncStatus = .healthy
    /// `derive` reads the clock for one thing, a cooldown's end; this looks
    /// again at that instant, so the banner does not wait for the next poll
    /// (as long as five minutes with the window hidden) to say it is over.
    @ObservationIgnored private var syncStatusExpiry: Task<Void, Never>?
    @ObservationIgnored private var syncStatusExpiresAt: Date?

    /// THE ONE ESTIMATE OF A CATCH-UP'S PACE, fed by every stats poll and
    /// read by both the first-load veil and the board's banner, so pressing
    /// Start now hands the bar to the board without it starting over at
    /// "estimating…". Reset when no walk is running, so a later catch-up (a
    /// reconnect, a long absence) is measured fresh.
    private(set) var catchUpEstimate = CatchUpEstimate()

    /// `PASSBAND_FIRST_LOAD`'s made-up walk, and nothing else: the one hook
    /// the preview needs, so the banner after Start now has a pace to show.
    /// Read in exactly one place, `noteSyncStatus`.
    @ObservationIgnored private var previewCatchUp: CatchUpProgress?

    func stageSyntheticCatchUp(_ progress: CatchUpProgress?) {
        guard FirstLoad.forced else { return }
        previewCatchUp = progress
        noteSyncStatus()
    }

    private func noteSyncStatus() {
        let now = Date()
        // The pace: one sample per poll while a walk runs, reset when none is.
        // Present means running, even with done == total: the daemon meets
        // its inbox count before it extends the walk by Sent (FirstLoadRule).
        let walk = previewCatchUp ?? stats?.catch_up
        if let c = walk, c.total > 0 {
            catchUpEstimate.record(done: c.done, at: now)
        } else if !catchUpEstimate.samples.isEmpty {
            catchUpEstimate.reset()
        }
        var status = SyncStatus.derive(daemonDown: daemonDown, stats: stats, now: now, date: Fmt.date)
        if let c = previewCatchUp, c.done < c.total { status = .catchingUp(done: c.done, total: c.total) }
        if status != syncStatus { syncStatus = status }
        var expiry: Date?
        if case .coolingDown(let until?, _) = status, until > now { expiry = until }
        // The same end already armed: leave its timer be.
        guard expiry != syncStatusExpiresAt else { return }
        syncStatusExpiry?.cancel()
        syncStatusExpiry = nil
        syncStatusExpiresAt = expiry
        if let expiry {
            syncStatusExpiry = Task { @MainActor [weak self] in
                try? await Task.sleep(for: .seconds(expiry.timeIntervalSince(now)))
                guard !Task.isCancelled, let self else { return }
                self.syncStatusExpiresAt = nil
                self.noteSyncStatus()
            }
        }
    }

    /// When the DAEMON last heard from Gmail. Nil on a daemon too old to say,
    /// and then callers fall back to the client's own poll time.
    var daemonLastSync: Date? { Fmt.date(stats?.sync?.last_success_at) }

    /// Jobs waiting for, or running through, triage right now. 0 when unknown.
    /// Stored: see `noteStats`.
    private(set) var triageWaiting = 0


    // MARK: - invite sharing

    /// Raise the share sheet, remembering what raised it.
    func openShareSheet(from origin: ShareOrigin) {
        shareOrigin = origin
        shareSheetOpen = true
    }

    /// Whether this daemon can mint and send invites. Read off the same sitrep
    /// stats the relay switch is, for the same reason: /client/stats is already
    /// the connect probe. A self-host daemon, a daemon too old to say, and a
    /// hosted tenant nobody has turned sharing on for all read as false, and
    /// false means the button is never offered and the two-week nudge never
    /// fires. Stored: see `noteStats`.
    private(set) var shareAvailable = false

    /// The hidden features that are on for this mailbox. Read off the same
    /// stats as the capability flags above, and for a different reason than
    /// they are: those say what the DAEMON can do, this says what an operator
    /// has let this PERSON see. See `HiddenFeatures`.
    var hiddenFeatures: HiddenFeatures { HiddenFeatures(daemon: stats?.features) }

    /// Whether to show `feature` here. False until the daemon says otherwise.
    func hasFeature(_ feature: HiddenFeature) -> Bool { hiddenFeatures.has(feature) }

    /// The daemon's answer to "has an iPhone app ever talked to this mailbox",
    /// or nil when it has not given one yet (or is too old to). Drives the
    /// TestFlight ask; see `TestFlightNudge`. Stored: see `noteStats`.
    private(set) var iosConnected: Bool?

    // MARK: - forwarding

    /// Whether this daemon understands `forward_of_message_id`. Read off the
    /// same sitrep stats `relayAvailable` is, for the same reason: /client/stats
    /// is already the connect probe and SitrepPoller keeps it fresh, so this
    /// capability costs no fetch of its own.
    ///
    /// STRICTLY `== true`. nil covers two states — stats not fetched yet, and a
    /// daemon too old to answer — and BOTH have to refuse. The failure this
    /// prevents is not an error dialog: an old daemon silently ignores the
    /// unknown field, sends the covering note by itself and returns 200 "sent",
    /// so hoping would mean mailing a stranger an empty "look at this" and
    /// telling the sender it worked. A send has no undo, so the only safe read
    /// of "we do not know" is no. Stored: see `noteStats`.
    private(set) var forwardingAvailable = false

    /// Whether the composers may offer to attach files. Same shape and same
    /// reason as `forwardingAvailable`: a daemon that does not know
    /// `attachment_ids` ignores the key and mails the words without the
    /// files, answering 200 — so the affordance is withheld rather than
    /// offered as a lie. nil (no stats yet) counts as no. Stored: see
    /// `noteStats`.
    private(set) var composeAttachmentsAvailable = false

    // MARK: - read tracking

    /// Whether a composer may offer the pixel at all. A daemon with no tracking
    /// base_url ignores `include_tracker` on every send, so offering a switch
    /// there would be offering a lie.
    var trackingAvailable: Bool { tracking?.configured == true }

    /// What a freshly-opened composer starts at. Unconfigured wins over the
    /// stored preference — a default of "on" that cannot happen is still off.
    var trackingDefault: Bool { trackingAvailable && tracking?.default_enabled == true }

    /// Read the daemon's tracking answer. Silent on failure: `tracking` staying
    /// nil means the feature simply is not offered this session, which is the
    /// same thing an unconfigured daemon means.
    func refreshTrackingConfig() async {
        let e = epoch
        let next = try? await APIClient.shared.getTrackingConfig()
        guard e == epoch else { return }
        tracking = next
    }

    /// Persist the default for future composers. The response is the daemon's
    /// post-write view, so `configured` is refreshed alongside it.
    func setTrackingDefault(_ enabled: Bool) async throws {
        tracking = try await APIClient.shared.setTrackingDefault(enabled)
    }

    /// Read the languages left untranslated. A 404 is a daemon older than the
    /// setting and draws a quiet hint, kept apart from a failed read, which
    /// must not look like "not offered".
    func refreshTranslateConfig() async {
        let e = epoch
        do {
            let next = try await APIClient.shared.getTranslateConfig()
            guard e == epoch else { return }
            translateConfig = .loaded(next)
        } catch {
            guard e == epoch else { return }
            translateConfig = translateConfig.failed(notFound: (error as? APIError)?.kind == .notFound)
        }
    }

    /// Replace the whole set. Throws so the card can say why; the stored
    /// answer only moves to what the daemon confirms.
    func setReadLanguages(_ codes: [String]) async throws {
        let e = epoch
        let next = try await APIClient.shared.setReadLanguages(codes)
        guard e == epoch else { return }
        translateConfig = .loaded(next)
    }

    func openTriageFix(_ target: TriageFixTarget) { triageFix = target }
    func closeTriageFix() { triageFix = nil }

    func openRemind(_ target: RemindTarget) { remindTarget = target }
    func closeRemind() { remindTarget = nil }

    func openRuleEditor(_ request: RuleEditorRequest) {
        var request = request
        if FixtureMode.isEnabled {
            let onSaved = request.onSaved
            let savedEpoch = epoch
            request.onSaved = { [weak self] in
                guard let self, self.epoch == savedEpoch else { return }
                onSaved?()
                Task { @MainActor [weak self] in
                    guard let self, self.epoch == savedEpoch else { return }
                    await SitrepPoller.shared.refreshAfterCorrection()
                }
            }
        }
        ruleEditor = request
    }
    func closeRuleEditor() { ruleEditor = nil }

    func openGroupEditor(_ request: GroupEditorRequest) { groupEditor = request }
    func closeGroupEditor() { groupEditor = nil }

    // MARK: - undo

    private static let undoTTL: TimeInterval = 5

    /// How long a draft DELETE waits before going out: exactly as long as its
    /// undo toast lives, so the toast can never outlast the row (an undo that
    /// fails) or vanish while the delete is still revocable.
    static let draftDeleteHold: TimeInterval = undoTTL

    @discardableResult
    func pushUndo(
        kind: UndoKind, messageId: Int, label: String, rewind: WalkRewind? = nil,
        revert: @escaping @Sendable () async throws -> Void
    ) -> UUID {
        var entry = PendingUndo(
            kind: kind, messageId: messageId, label: label, rewind: rewind, revert: revert)
        entry.expiresAt = entry.createdAt.addingTimeInterval(Self.undoTTL)
        undos.append(entry)
        let id = entry.id
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(Self.undoTTL))
            self?.undos.removeAll { $0.id == id }
        }
        return id
    }

    /// Undo the given (or most recent) queued action.
    func fireUndo(_ id: UUID? = nil) async {
        // One at a time: `u u` on two walk steps has to walk back the first
        // before the second can find the reader where its step landed.
        // `u` means stop: a step kept from a fast tap must not run after it.
        queuedWalkStep = nil
        while undoBusy { try? await Task.sleep(for: .milliseconds(16)) }
        undoBusy = true
        defer { undoBusy = false }
        // A walk step still moving (an E's email in flight, a rewind not yet
        // played) has not landed, and its own undo may not even exist yet.
        // Wait for it, so `u` takes the step that just happened and finds the
        // reader where that step put it. Bounded: a slow daemon holds the
        // flight, and the undo should not wait on it forever.
        let deadline = Date().addingTimeInterval(1.5)
        while walk != nil, walkRewind != nil || threadFlight != .settled, Date() < deadline {
            try? await Task.sleep(for: .milliseconds(16))
        }
        let e = epoch
        let entry: PendingUndo?
        if let id {
            entry = undos.first { $0.id == id }
        } else {
            entry = undos.last
        }
        guard let entry else { return }
        undos.removeAll { $0.id == entry.id }
        do {
            try await entry.revert()
            // The revert landed on the OLD account's daemon; nothing about it
            // is news the new one's surfaces should be told.
            guard e == epoch else { return }
            // The message is open again, so it must stop being filtered out of
            // every list that hides resolved ids — otherwise an undo looks like
            // it did nothing until the next poll. Harmless for undo kinds that
            // never resolved anything: the id simply is not in the set.
            // Only for a kind that resolved something: see `touchesSitrep`.
            if entry.kind.touchesSitrep {
                resolvedIds.remove(entry.messageId)
                // The mail page may already hold the server's `done` row (a
                // poll landed inside the undo window), and the next fetch is
                // up to a TTL away; reopen it there too so the undo shows now.
                reopenInMail(entry.messageId)
            }
            // A walk step that walks back says so by moving; a toast on top of
            // the email returning would be the same news twice. A skip that
            // cannot walk back (the walk has moved on) changed nothing at all.
            let rewound = entry.rewind.map(rewindWalk) ?? false
            if !rewound && entry.kind != .skip { pushToast("undone: \(entry.label)", .info) }
            Analytics.capture("undo_fired", ["kind": String(describing: entry.kind)])
            // The bands dropped the row optimistically and the next poll is up
            // to a tick out — pull now, or the undo reads as broken on the sitrep.
            if entry.kind.touchesSitrep { await SitrepPoller.shared.pull() }
        } catch {
            guard e == epoch else { return }
            // A schedule whose row the worker already claimed: the mail is
            // going, and the toast says so rather than "undo failed:
            // scheduled for …", which would read as still scheduled.
            if entry.kind == .schedule, case HoldError.alreadySent = error {
                pushToast(ScheduleUndo.alreadySentToast, .error)
                return
            }
            pushToast("undo failed: \(entry.label)", .error)
        }
    }

    // MARK: - toasts

    @discardableResult
    func pushToast(_ text: String, _ tone: Toast.Tone = .info) -> UUID {
        let toast = Toast(text: text, tone: tone)
        toasts.append(toast)
        // Auto-dismiss so the stack cannot accumulate; undos own their own 5s
        // window and a click target.
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(6))
            self?.toasts.removeAll { $0.id == toast.id }
        }
        return toast.id
    }

    func dismissToast(_ id: UUID) { toasts.removeAll { $0.id == id } }

    // MARK: - 2FA

    /// Start a 60s countdown ring for a freshly-arrived auth message.
    /// Deduped: one ring per id; re-arming restarts the sweep.
    func pushAuthRing(_ id: Int) {
        authRings.removeAll { $0.id == id }
        authRings.append(AuthRing(id: id, startedAt: Date()))
    }

    func expireAuthRing(_ id: Int) { authRings.removeAll { $0.id == id } }

    /// Enqueue a code-modal entry, newest-first, deduped by id.
    func pushAuthCode(_ entry: AuthCodeEntry) {
        guard !authQueue.contains(where: { $0.meta.id == entry.meta.id }) else { return }
        authQueue.insert(entry, at: 0)
    }

    /// Pop the front (currently-shown) code-modal entry on dismiss.
    func dismissAuthCode() {
        if !authQueue.isEmpty { authQueue.removeFirst() }
    }

    // MARK: - band mutation (optimistic)

    /// Ids resolved during this session. The bands in `sitrep` update in place,
    /// but a list holding its own snapshot (EmailsView reloads only on the
    /// `lastRefresh` poll) would keep showing a resolved row for up to a full poll —
    /// which reads as the action not working. This is the shared record such a
    /// list filters against; the poll still supplies the truth.
    private(set) var resolvedIds: Set<Int> = [] {
        didSet {
            for id in resolvedIds.subtracting(oldValue) { resolvedAt[id] = Date() }
            for id in oldValue.subtracting(resolvedIds) { resolvedAt[id] = nil }
        }
    }
    /// When each id joined `resolvedIds`, so a later poll can tell "the server
    /// has not caught up yet" from "the server reopened it since".
    private var resolvedAt: [Int: Date] = [:]

    /// Let the server's word win over a resolve made this session, for the one
    /// case where it reopens mail on its own: a reminder that fired. Only for a
    /// fetch that STARTED well after the resolve, so a poll racing the resolve's
    /// own write cannot hand the row back.
    fileprivate func releaseReopened(_ rows: [AttentionUpdate], fetchedAt: Date) {
        for u in rows where u.status != .done && u.reminded_at != nil {
            guard let at = resolvedAt[u.id], fetchedAt.timeIntervalSince(at) > 30 else { continue }
            resolvedIds.remove(u.id)
        }
    }

    /// Optimistically pull a message id out of whatever band holds it and keep
    /// the selection valid (advance to the next row, else the previous).
    /// Returns a `restore` thunk that re-inserts the removed rows on failure.
    func removeFromBands(_ messageId: Int) -> () -> Void {
        let prev = sitrep
        resolvedIds.insert(messageId)
        // Compute the next selection BEFORE mutating, using the flat order.
        let orderBefore = orderedIds
        let posBefore = orderBefore.firstIndex(of: messageId) ?? 0

        sitrep.standing.removeAll { $0.id == messageId }
        sitrep.new.removeAll { $0.id == messageId }
        sitrep.open.removeAll { $0.id == messageId }

        if selectedId == messageId {
            let after = orderedIds
            if after.isEmpty {
                selectedId = nil
            } else {
                selectedId = after[min(max(posBefore, 0), after.count - 1)]
            }
        }

        // The bands this hands back are the account's that was live when the
        // row left them. A verb that fails after a switch must not lay them
        // over the mailbox that is on screen by then.
        let e = epoch
        return { [weak self] in
            guard let self, self.epoch == e else { return }
            self.sitrep.standing = prev.standing
            self.sitrep.new = prev.new
            self.sitrep.open = prev.open
            self.selectedId = messageId
            // An undone resolve must be undone EVERYWHERE, or the row returns to
            // the bands while the mail page keeps filtering it out.
            self.resolvedIds.remove(messageId)
        }
    }

    /// Record a resolve that did NOT go through the bands: the reader can finish a
    /// thread it was not opened from a queue with, and that mail may still be
    /// sitting in a list behind it.
    func noteResolved(_ messageId: Int) {
        resolvedIds.insert(messageId)
        sitrep.standing.removeAll { $0.id == messageId }
        sitrep.new.removeAll { $0.id == messageId }
        sitrep.open.removeAll { $0.id == messageId }
    }

    /// The same for a batch, as ONE change to `resolvedIds`: every insert
    /// fires its observers, and a smart clear can name dozens of ids.
    func noteResolved(_ ids: Set<Int>) {
        guard !ids.isEmpty else { return }
        resolvedIds.formUnion(ids)
        sitrep.standing.removeAll { ids.contains($0.id) }
        sitrep.new.removeAll { ids.contains($0.id) }
        sitrep.open.removeAll { ids.contains($0.id) }
    }

    /// The reverse, for an undo that reopened a batch on the daemon: stop
    /// hiding exactly these ids everywhere a resolved id is hidden, as
    /// `fireUndo` does for one. The caller refreshes what it needs to.
    func unresolve(_ ids: Set<Int>) {
        guard !ids.isEmpty else { return }
        resolvedIds.subtract(ids)
        for id in ids { reopenInMail(id) }
    }

    /// Done on a whole Reading sender at once: the tile's `e`, and the reader's
    /// held `e` on that sender's stack. The tile leaves now and every message
    /// is marked in PARALLEL, so a stack of twenty lands in one round trip's
    /// time rather than twenty. One toast, and no undo on this path.
    func markReadingDone(_ items: [AttentionUpdate], sender: String) async {
        let ids = Set(items.map(\.id))
        zones.reading.removeAll { $0.items.contains { ids.contains($0.id) } }
        let landed = await withTaskGroup(of: Int?.self) { group in
            for item in items {
                group.addTask {
                    do {
                        try await APIClient.shared.setStatus(item.id, .done)
                        // No undo, so nothing re-pins: unpin as each one lands.
                        await ImageStore.shared.release(messageId: item.id)
                        return item.id
                    } catch {
                        return nil
                    }
                }
            }
            var landed: [Int] = []
            for await id in group { if let id { landed.append(id) } }
            return landed
        }
        // Record each as resolved, not just gone from the zone: the same
        // message can be sitting in a band or on the mail page, and this path
        // never went through removeFromBands. Only the ones that LANDED — a
        // resolved id is hidden for the rest of the session.
        for id in landed { noteResolved(id) }
        let failed = items.count - landed.count
        if failed > 0 {
            pushToast("some marks failed; refresh to re-sync", .error)
        } else {
            pushToast("done: \(SenderCache.resolved(sender).displayName) (\(items.count))", .info)
        }
    }

    /// The same, for every message from one sender: the client half of the
    /// server's `resolve_sender`, so unsubscribing or blocking empties that
    /// sender NOW rather than one poll later.
    ///
    /// Matched on the BARE ADDRESS, because the bands carry raw sender strings
    /// ("Name <addr>") while the caller has whatever the action was performed
    /// against. Everything the server resolved must be recorded here too — a row
    /// that stays in `sitrep` but is done on the server reappears on no refresh
    /// and vanishes on the next, which reads as a glitch.
    func noteSenderResolved(_ senderAddr: String) {
        let target = SenderID.address(senderAddr)
        guard !target.isEmpty else { return }
        let matches = { (u: AttentionUpdate) in SenderID.address(u.sender) == target }
        for u in sitrep.standing + sitrep.new + sitrep.open where matches(u) {
            resolvedIds.insert(u.id)
        }
        // ONE union, not an insert per row: the scroll can hold thousands of
        // rows, and every insert fires `resolvedIds`' observers.
        var ids = Set<Int>()
        for mode in MailMode.allCases {
            for u in mailPage(mode).value ?? [] where matches(u) { ids.insert(u.id) }
        }
        resolvedIds.formUnion(ids)
        sitrep.standing.removeAll(where: matches)
        sitrep.new.removeAll(where: matches)
        sitrep.open.removeAll(where: matches)
    }

    /// Optimistically pull a message out of the STANDING band ONLY — the one a tier
    /// correction empties. Standing is tier-defined server-side (`tier IN
    /// ('past_due','deadline') AND status != 'done'`) while `new`/`open` come from
    /// surfaced_at and status, which a tier correction leaves alone; a fresh
    /// deadline email sits in both, so pulling all three blanks it out of Attention
    /// until the refresh. No restore thunk — the write has already succeeded.
    func removeFromStanding(_ messageId: Int) {
        sitrep.standing.removeAll { $0.id == messageId }
    }
}

extension GroupToken {
    /// Re-resolve a token to the group it names, for a draft restored after the
    /// composer that wrote it is long gone.
    ///
    /// Matches on the SLUG exactly, against the daemon's own search. A rename
    /// changes the slug, so a restored draft naming the old one resolves to
    /// nothing — which is correct and is why the composer renders an unresolved
    /// pill as a problem rather than quietly picking the nearest match. Guessing
    /// which audience someone meant is not a thing to do with an irreversible
    /// action.
    static func resolve(_ slug: String) async -> SendGroup? {
        guard let hits = try? await APIClient.shared.searchGroups(slug, limit: 25) else {
            return nil
        }
        return hits.first { $0.slug == slug }
    }
}
