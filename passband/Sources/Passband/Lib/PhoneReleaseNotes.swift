// THE IPHONE'S WHAT'S NEW, and the only hand-authored copy of it.
//
// A TABLE OF ITS OWN, not a reading of the Mac's. The phone ships on its own
// version line (PassbandiOS MARKETING_VERSION in project.yml, tagged
// passband-ios-X), so "what did this update bring" is a question about a
// PHONE version, and the Mac's table (Lib/ReleaseNotes.swift) cannot answer
// it: its 0.0.8 is a Mac release from September. The two also say different
// things. A Mac note teaches a key; the phone has no keys, and the same
// feature is a swipe or a hold here. Overlap in the wording is fine and is not
// a reason to derive one from the other. The Mac's table keeps its `.ios`
// items: that one is the changelog, and it still says what the iPhone got.
//
// WHAT A NOTE IS, HERE. The phone shows a release full screen, one feature a
// page, paged by swipe (PassbandiOS/Views/WhatsNewWalk.swift). So a note is
// written as that walk: a few FEATURES that each get a page and a vignette,
// then the ALSO lines the closing page lists. No surfaces: everything in this
// table is the phone.
//
// PURE ON PURPOSE, for the reason ReleaseNotes is: no SwiftUI, no Bundle, no
// store, so the suite compiles it alone and the running version arrives as a
// parameter. The selection rules are ReleaseLedger's, shared with the Mac's
// table, because which notes an install is owed is one question however the
// notes are written.
//
// HOUSE RULES for the prose: no em dashes, no keys (there are none), say what
// the thumb does, and name a control by the word on it.

import Foundation

/// The animation a feature's page plays. A KEY, not a drawing, for the reason
/// `ReleaseVignette` is one: this file stays free of SwiftUI, and the walk
/// maps each case to its scene (PassbandiOS/Views/PhoneVignettes.swift).
enum PhoneVignette: String, CaseIterable, Sendable {
    case chainReply, messageSwipe, clearStack, remind
}

/// One page of the walk: a thing the phone can now do, and how.
struct PhoneFeature: Sendable, Identifiable {
    /// The page's headline, set in the serif. A phrase, no full stop.
    let title: String
    /// The line under it: the gesture, in one sentence.
    let detail: String
    /// The page's picture when no vignette is drawn for it.
    let symbol: String
    let vignette: PhoneVignette?

    var id: String { title }

    init(_ title: String, detail: String, symbol: String, vignette: PhoneVignette? = nil) {
        self.title = title
        self.detail = detail
        self.symbol = symbol
        self.vignette = vignette
    }
}

/// One line of the closing page: something else the release brought that did
/// not need a page to explain.
struct PhoneAside: Sendable, Identifiable {
    let text: String
    /// Its glyph in the list. The catch-alls ("Bug fixes") carry none: they
    /// are said once, in the footnote, as on the Mac's card.
    let symbol: String?

    var id: String { text }

    init(_ text: String, symbol: String? = nil) {
        self.text = text
        self.symbol = symbol
    }
}

/// One shipped version of the iPhone app, keyed by the PHONE's version.
struct PhoneReleaseNote: VersionedNote, Identifiable, Sendable {
    /// Marketing version, matching the passband-ios-X tag.
    let version: String
    /// ISO date, printed as written.
    let date: String
    /// The release's name, in the app's own voice. It rides every page.
    let headline: String
    /// The pages, in the order they are walked. Three to five: fewer is not a
    /// walk, and more is the list again with pictures.
    let features: [PhoneFeature]
    let also: [PhoneAside]

    var id: String { version }

    /// The closing page's list, the catch-alls taken out.
    var asides: [PhoneAside] {
        also.filter { !ReleaseDigest.catchAlls.contains(ReleaseDigest.key($0.text)) }
    }

    /// "Plus bug fixes.", or nil when the release claimed none.
    var footnote: String? {
        ReleaseDigest.footnote(
            also.map(\.text).filter { ReleaseDigest.catchAlls.contains(ReleaseDigest.key($0)) })
    }
}

enum PhoneReleaseNotes {
    /// THE TABLE. Newest first, and the only place a phone note is written.
    static let all: [PhoneReleaseNote] = [
        PhoneReleaseNote(
            version: "0.0.10",
            date: "2026-10-27",
            headline: "Less to Wade Through",
            features: [
                PhoneFeature(
                    "Smart clear",
                    detail: "Tap smart clear on Reading to clear what has expired and what you never open. Undo puts all of it back.",
                    symbol: "checkmark.rectangle.stack"),
                PhoneFeature(
                    "Search asks the agent for you",
                    detail: "Type a question in Search and the agent looks through your mail for the answer, right above your results.",
                    symbol: "sparkle.magnifyingglass"),
                PhoneFeature(
                    "Mail in your language",
                    detail: "Email written in a language you do not read arrives translated. Tap show original to see it as sent.",
                    symbol: "character.bubble"),
            ],
            also: [
                PhoneAside(
                    "The person icon opens your accounts, with Settings at the bottom",
                    symbol: "gearshape"),
                PhoneAside("Delete a message you are writing with the trash in the corner", symbol: "trash"),
                PhoneAside("Choose the languages you read in Account, under Mail", symbol: "globe"),
                PhoneAside("Press and drag to select text in plain emails", symbol: "text.cursor"),
                PhoneAside("Bug fixes"),
            ]),
        PhoneReleaseNote(
            version: "0.0.9",
            date: "2026-10-10",
            headline: "Faster to Find",
            features: [
                PhoneFeature(
                    "Find mail by who sent it",
                    detail: "Type a name or an address in Search and tap the sender to see their mail, newest first.",
                    symbol: "person.crop.circle"),
                PhoneFeature(
                    "Long threads keep up",
                    detail: "The next messages are drawn before you scroll to them, and a thread you reopen lands on its newest message.",
                    symbol: "text.line.last.and.arrowtriangle.forward"),
                PhoneFeature(
                    "Notifications on a locked phone",
                    detail: "A banner now says who wrote and what about while the phone is locked, and tapping one opens the app.",
                    symbol: "bell.badge"),
            ],
            also: [
                PhoneAside("Tap Search and the keyboard is already up", symbol: "magnifyingglass"),
                PhoneAside("The assistant looks harder before it says it found nothing", symbol: "sparkles"),
                PhoneAside("Images that some senders label wrongly now load", symbol: "photo"),
                PhoneAside(
                    "A way back to sign in when your mailbox is not answering",
                    symbol: "arrow.uturn.backward"),
                PhoneAside("Bug fixes"),
            ]),
        PhoneReleaseNote(
            version: "0.0.8",
            date: "2026-10-05",
            headline: "Catching Up",
            features: [
                PhoneFeature(
                    "Reply where you finish reading",
                    detail: "Every email now ends in reply and reply all, and you write the reply right under it.",
                    symbol: "arrowshape.turn.up.left", vignette: .chainReply),
                PhoneFeature(
                    "Answer one message",
                    detail: "Swipe any message in a thread to the left for Reply all, Reply and Forward.",
                    symbol: "arrowshape.turn.up.left.2", vignette: .messageSwipe),
                PhoneFeature(
                    "Clear a stack",
                    detail: "Hold the checkmark to finish every email in a Reading stack. A tap still finishes one.",
                    symbol: "square.stack.3d.up", vignette: .clearStack),
                PhoneFeature(
                    "Remind me later",
                    detail: "Swipe a row to the right and choose Remind, or tap remind me in an open email.",
                    symbol: "bell", vignette: .remind),
            ],
            also: [
                PhoneAside(
                    "Rules and Groups, under the person icon on the Sitrep",
                    symbol: "line.3.horizontal.decrease"),
                PhoneAside(
                    "The Sitrep says when mail has stopped arriving, with a Reconnect button",
                    symbol: "bolt.horizontal"),
                PhoneAside(
                    "Smart block: when a sender ignores your unsubscribe, block just the mail you did not want",
                    symbol: "hand.raised"),
                PhoneAside("Track opens on a reply", symbol: "eye"),
                PhoneAside("Bug fixes"),
            ])
    ]

    /// The releases an install stamped at `lastSeen` has not read, newest
    /// first, and none newer than the build that is running. See
    /// `ReleaseNotes.unseen` for why each bound is where it is.
    static func unseen(lastSeen: String?, running: String) -> [PhoneReleaseNote] {
        ReleaseLedger.unseen(all, lastSeen: lastSeen, running: running)
    }

    /// What closing the walk stamps: the newest note SHOWN.
    static func newestShown(_ notes: [PhoneReleaseNote]) -> String? {
        ReleaseLedger.newestShown(notes)
    }

    /// The newest note this build is at or past: Settings' "What's new".
    static func newest(atOrBelow running: String) -> PhoneReleaseNote? {
        ReleaseLedger.newest(all, atOrBelow: running)
    }

    /// What the first sync of a session owes whoever is holding the phone.
    enum Arrival {
        /// Raise the walk on these, newest first.
        case show([PhoneReleaseNote])
        /// Say nothing, and write this version down as read.
        case stamp(String)
        case nothing
    }

    /// THE PHONE'S GATE. The Mac asks `tourCompleted`: has the app already
    /// introduced itself to whoever is sitting here. The phone has no tour, so
    /// it asks the thing the tour stood for. A NEWCOMER is somebody this
    /// install met at the Connect gate with no mailbox on it; they are owed
    /// the cold open, not release notes, and they are stamped current without
    /// a word so the release they installed is never announced to them.
    /// Everybody else is read by their stamp, exactly as on the Mac: no stamp
    /// on an install that already had a mailbox is one that predates this
    /// feature, owed the release it just took and nothing older.
    ///
    /// The newcomer is stamped with the RUNNING version, not the newest note
    /// at or below it. They have seen everything this build is, whether or
    /// not a note was written for it, and a stamp below the build would let a
    /// note written late announce itself to them after all.
    static func arrival(lastSeen: String?, running: String, newcomer: Bool) -> Arrival {
        guard ReleaseVersion(running) != nil else { return .nothing }
        if newcomer, lastSeen.flatMap(ReleaseVersion.init) == nil { return .stamp(running) }
        let owed = unseen(lastSeen: lastSeen, running: running)
        return owed.isEmpty ? .nothing : .show(owed)
    }
}
