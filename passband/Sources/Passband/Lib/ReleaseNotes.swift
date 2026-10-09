// THE CHANGELOG, and the only hand-authored copy of it.
//
// Everything else is generated FROM this table: docs/CHANGELOG.md, the body of
// a GitHub release, and whatever the site grows later (Tools/ChangelogTool.swift
// behind make-changelog.sh). The direction is the point. The What's New card
// reads this table directly, so the notes a human is shown in the app cannot be
// stale; generating the other way round would put a codegen step between
// writing a note and shipping it, and a stale generated file ships silently.
// XcodeGen already teaches that lesson once per new file.
//
// PURE ON PURPOSE: no SwiftUI, no Bundle, no store. Which version this build
// actually is arrives as a parameter, which is what lets the test suite and the
// markdown generator compile this file on its own.
//
// HOUSE RULES for the prose, because it is user-facing copy:
//   * no em dashes
//   * say what the human can now do, not what the commit did
//   * every item names its surface, because the three parts ship separately:
//     the Mac app updates itself, the phone comes through TestFlight, the
//     daemon is rolled (hosted) or pulled as an image (self-host), and a note
//     nobody can locate is a note nobody trusts

import Foundation

/// A dotted release version, ordered by component. Failable rather than
/// lenient: a stamp that does not parse is not a version this code should
/// silently rank, and every caller already has an honest answer for nil.
struct ReleaseVersion: Comparable, CustomStringConvertible {
    /// The dotted components, as written. Missing trailing components compare
    /// as zero, so 0.0.4 and 0.0.4.0 are the same release.
    let parts: [Int]
    let text: String

    init?(_ text: String) {
        let fields = text.split(separator: ".", omittingEmptySubsequences: false)
        let parsed = fields.map { Int($0) }
        guard !parsed.isEmpty, !parsed.contains(where: { $0 == nil }) else { return nil }
        self.parts = parsed.map { $0! }
        self.text = text
    }

    static func < (lhs: Self, rhs: Self) -> Bool {
        for index in 0..<max(lhs.parts.count, rhs.parts.count) {
            let left = index < lhs.parts.count ? lhs.parts[index] : 0
            let right = index < rhs.parts.count ? rhs.parts[index] : 0
            if left != right { return left < right }
        }
        return false
    }

    /// Equality is by ORDER, not by spelling: a stamp written as "0.0.4" by one
    /// build has to satisfy a build calling itself "0.0.4.0".
    static func == (lhs: Self, rhs: Self) -> Bool { !(lhs < rhs) && !(rhs < lhs) }

    var description: String { text }
}

/// Which part of Passband a note landed in. The Mac app updates itself through
/// Sparkle; the phone comes through TestFlight or the App Store; the daemon is
/// rolled onto hosted tenants or pulled as an image on a self-host box. One
/// list, three surfaces, so a reader knows whether a change is already theirs
/// or arrives with their next daemon.
enum ReleaseSurface: String, CaseIterable, Sendable {
    case app, ios, daemon

    var label: String {
        switch self {
        case .app: "Mac"
        case .ios: "iPhone"
        case .daemon: "Daemon"
        }
    }
}

/// The animation a headliner's page plays in the What's New walk. A KEY, not a
/// drawing: this file stays free of SwiftUI, and the card maps each case to
/// its scene (Views/Vignette/ReleaseVignettes.swift). The markdown ignores it.
enum ReleaseVignette: String, CaseIterable, Sendable {
    case mailList, flow, unsubscribe, clearStack
    case smartBlock
    case loginCode, agentWrite, smartUnsubscribe
    /// Not a drawn scene: the agent ad's own conversation, live.
    case agentSession
}

/// One bullet in a release.
struct ReleaseItem: Sendable {
    let surface: ReleaseSurface
    let text: String
    /// The line under a headliner's title on its page of the walk: how to use
    /// the thing, in one sentence. A key written in backticks is set as a key.
    let detail: String?
    /// An SF Symbol makes this item a HEADLINER: the What's New card gives it
    /// a page of its own in the walk. The author picks them, a few per release, because no rule can
    /// tell the feature people updated for from the fix they will never
    /// notice. The markdown ignores it.
    let symbol: String?
    /// The picture on that page. Without one the page draws the symbol.
    let vignette: ReleaseVignette?

    init(
        _ surface: ReleaseSurface, _ text: String, detail: String? = nil, symbol: String? = nil,
        vignette: ReleaseVignette? = nil
    ) {
        self.surface = surface
        self.text = text
        self.detail = detail
        self.symbol = symbol
        self.vignette = vignette
    }
}

/// One shipped version, keyed by the MAC APP's version. Daemon and phone work
/// ride in the release they shipped alongside rather than being keyed on their
/// own: the daemon has no screen to read notes on, and a second key here would
/// be a second thing to get wrong for a difference nobody can see. Both still
/// carry their own version and tag (daemon-X, passband-ios-X); those are
/// numbered on their own cadence, and the phone's is expected to drift as its
/// UX does.
///
/// THE PHONE'S OWN WHAT'S NEW IS NOT READ FROM HERE. It has a table of its own,
/// keyed by the phone's version and written for a thumb
/// (Lib/PhoneReleaseNotes.swift). The `.ios` items below stay: this is the
/// changelog, and the changelog still says what the iPhone got.
struct ReleaseNote: Identifiable, Sendable {
    /// Marketing version, matching passband/VERSION for that tag.
    let version: String
    /// ISO date, printed as written.
    let date: String
    /// The release in one line, in the app's own voice.
    let headline: String
    let items: [ReleaseItem]

    var id: String { version }
    var semver: ReleaseVersion? { ReleaseVersion(version) }

    /// The bullets for one surface, in authored order.
    func items(on surface: ReleaseSurface) -> [ReleaseItem] {
        items.filter { $0.surface == surface }
    }

    var digest: ReleaseDigest { ReleaseDigest(self) }
}

/// One release, or every release somebody skipped, arranged for READING rather
/// than for the record. The table is
/// written per surface because that is how the parts ship; read straight off it,
/// the same feature appears twice ("Schedule an email for later" on the Mac and
/// again on the phone) and "Bug fixes" weighs as much as the headline. This is
/// the card's arrangement, and it lives here, next to the table and free of
/// SwiftUI, so the test suite can hold it to that.
///
/// SEVERAL RELEASES ARE ONE READ. Somebody who skipped an update is walked
/// through every headliner they missed, newest release first, and then handed
/// one list per surface rather than a list per release: the same arrangement,
/// with the same folding, over more rows.
struct ReleaseDigest: Sendable {
    struct Headliner: Sendable, Identifiable {
        let text: String
        /// The page's second line, when the table wrote one.
        let detail: String?
        let symbol: String
        /// The page's animation, when the table named one.
        let vignette: ReleaseVignette?
        /// Every surface the same words landed on, in surface order.
        let surfaces: [ReleaseSurface]
        /// The release that brought it; the newest one, when two said it.
        let version: String
        var id: String { text }
    }

    struct Section: Sendable, Identifiable {
        let surface: ReleaseSurface
        /// The specific items, in authored order.
        let items: [String]
        /// The catch-alls ("Bug fixes"), said once in a footnote instead.
        let extras: [String]
        var id: ReleaseSurface { surface }
    }

    let headliners: [Headliner]
    /// Surfaces with anything left to say, in surface order.
    let sections: [Section]

    /// Filler every release carries. True, but not news: the card says it once,
    /// quietly, after the things that are.
    static let catchAlls: Set<String> = ["bug fixes", "ui improvements", "visual improvements"]

    init(_ note: ReleaseNote) { self.init([note]) }

    /// `notes` newest first, which is the order the walk and the lists keep.
    init(_ notes: [ReleaseNote]) {
        // A headliner claims its words on EVERY surface and in EVERY release
        // given, symbol or not, so a feature that shipped on the Mac in one
        // release and the phone in the next is one page tagged with both.
        var headliners: [Headliner] = []
        for note in notes {
            for item in note.items {
                guard let symbol = item.symbol else { continue }
                let key = Self.key(item.text)
                guard !headliners.contains(where: { Self.key($0.text) == key }) else { continue }
                let surfaces = ReleaseSurface.allCases.filter { surface in
                    notes.contains { $0.items(on: surface).contains { Self.key($0.text) == key } }
                }
                headliners.append(
                    Headliner(
                        text: item.text, detail: item.detail, symbol: symbol, vignette: item.vignette,
                        surfaces: surfaces, version: note.version))
            }
        }
        let claimed = Set(headliners.map { Self.key($0.text) })

        self.headliners = headliners
        self.sections = ReleaseSurface.allCases.compactMap { surface in
            // Said once per surface: "Bug fixes" in three releases running is
            // still one footnote.
            var said = Set<String>()
            let left = notes.flatMap { $0.items(on: surface).map(\.text) }.filter {
                !claimed.contains(Self.key($0)) && said.insert(Self.key($0)).inserted
            }
            let items = left.filter { !Self.catchAlls.contains(Self.key($0)) }
            let extras = left.filter { Self.catchAlls.contains(Self.key($0)) }
            guard !items.isEmpty || !extras.isEmpty else { return nil }
            return Section(surface: surface, items: items, extras: extras)
        }
    }

    /// "Plus UI improvements and bug fixes." Mid-sentence, so every word is
    /// lowered except an acronym: "UI" survives, the table's "Bug Fixes" does not.
    static func footnote(_ extras: [String]) -> String? {
        guard !extras.isEmpty else { return nil }
        let words = extras.map { text in
            text.split(separator: " ").map { word in
                word.count > 1 && word.allSatisfy(\.isUppercase) ? String(word) : word.lowercased()
            }.joined(separator: " ")
        }
        let joined =
            words.count == 1
            ? words[0]
            : words.dropLast().joined(separator: ", ") + " and " + words.last!
        return "Plus \(joined)."
    }

    /// What makes two lines "the same words": case, edge whitespace and
    /// trailing punctuation do not count.
    static func key(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines.union(.punctuationCharacters)).lowercased()
    }
}

/// The greeting for somebody who took more than one release at once: which
/// version they were reading notes for last, and which one they are on now.
struct ReleaseCatchUp: Sendable, Equatable {
    let from: String
    let to: String

    var title: String { "Thank you for updating to \(to)!" }
    var line: String { "We have been hard at work, and here's what's new since you were on \(from)." }
}

enum ReleaseNotes {
    /// THE TABLE. Newest first, and the only place a release note is written.
    static let all: [ReleaseNote] = [
        ReleaseNote(
            version: "0.0.14",
            date: "2026-10-08",
            headline: "Hands Free",
            items: [
                ReleaseItem(
                    .app, "Login codes come to you",
                    detail: "A sign-in code floats over the app you are in. Copy it without opening Passband",
                    symbol: "key", vignette: .loginCode),
                ReleaseItem(
                    .app, "Agent MCP can write",
                    detail: "Your agent drafts, you send. Choose how much it may do in Settings, under Agents",
                    symbol: "square.and.pencil", vignette: .agentWrite),
                ReleaseItem(
                    .app, "Smart unsubscribe",
                    detail: "`u` now gets through preference pages and forms on its own",
                    symbol: "wand.and.stars", vignette: .smartUnsubscribe),
                ReleaseItem(.app, "Profile photo"),
                ReleaseItem(.app, "Delete drafts"),
                ReleaseItem(.app, "Better reply composer"),
                ReleaseItem(.app, "Better toasts"),
                ReleaseItem(.app, "Muse support"),
                ReleaseItem(.app, "UX improvements"),
                ReleaseItem(.app, "Bug fixes"),
                ReleaseItem(.daemon, "Reads PINs and passcodes as login codes"),
                ReleaseItem(.daemon, "Lets your agent draft and send, when you allow it"),
                ReleaseItem(.daemon, "Finishes unsubscribes that need a form"),
                ReleaseItem(.daemon, "Bug fixes"),
            ]),
        ReleaseNote(
            version: "0.0.13",
            date: "2026-10-05",
            headline: "Cleanup Time",
            items: [
                ReleaseItem(
                    .app, "Connect your agents",
                    detail: "Hosted accounts can sign Claude or ChatGPT in to their mailbox from Settings, under Agents",
                    symbol: "link", vignette: .agentSession),
                ReleaseItem(
                    .app, "Smart block",
                    detail: "When a sender ignores your unsubscribe, block just the mail you did not want",
                    symbol: "hand.raised", vignette: .smartBlock),
                ReleaseItem(.app, "Ask ⌘K how Passband works"),
                ReleaseItem(.app, "`⌘F` to find text in an email"),
                ReleaseItem(.app, "E and S walk Reading from one sender to the next"),
                ReleaseItem(.app, "Better rule writing"),
                ReleaseItem(.app, "Auto update"),
                ReleaseItem(.app, "Better UX"),
                ReleaseItem(.app, "Bug fixes"),
                ReleaseItem(.ios, "Bug fixes"),
                ReleaseItem(.daemon, "Suggests a smart block for senders who ignore an unsubscribe"),
                ReleaseItem(.daemon, "Bug fixes"),
            ]),
        ReleaseNote(
            version: "0.0.12",
            date: "2026-10-01",
            headline: "Philipp's Update",
            items: [
                ReleaseItem(
                    .app, "\"All Mail\" overhaul",
                    detail: "Subjects, dates, day headings, filters",
                    symbol: "list.bullet.rectangle", vignette: .mailList),
                ReleaseItem(
                    .app, "Flow mode",
                    detail: "Press `space` and clear your inbox faster",
                    symbol: "wind", vignette: .flow),
                ReleaseItem(
                    .app, "Zero-effort Unsubscribe",
                    detail: "Pressing `u` handles 98% of unsubscribing",
                    symbol: "minus.circle", vignette: .unsubscribe),
                ReleaseItem(
                    .app, "Clear a stack",
                    detail: "Hold `e` to clear every email in a Reading stack",
                    symbol: "square.stack.3d.up", vignette: .clearStack),
                ReleaseItem(.app, "Reading puts articles above promotions"),
                ReleaseItem(.app, "Better logos in Reading cards"),
                ReleaseItem(.app, "j and k wrap around on the sitrep. Arrow keys work on Rules and Groups"),
                ReleaseItem(.app, "Backspace removes a recipient in a reply"),
                ReleaseItem(.app, "Name an account Personal, Work or anything else"),
                ReleaseItem(.app, "Click a sender to view their sender profile"),
                ReleaseItem(.app, "Bug fixes"),
                ReleaseItem(.ios, "All mail has day headings and filters, and finished mail stays visible"),
                ReleaseItem(.ios, "Reading puts articles above promotions"),
                ReleaseItem(.ios, "Reading shows every sender, folded after 8"),
                ReleaseItem(.ios, "Name an account Personal, Work or anything else"),
                ReleaseItem(.ios, "Bug fixes"),
                ReleaseItem(.daemon, "Unsubscribes for you"),
                ReleaseItem(.daemon, "Triage sorts Reading into articles and promotions"),
                ReleaseItem(.daemon, "Writes the sitrep headline, and only ever good news"),
                ReleaseItem(.daemon, "Cards carry the sender's real name"),
                ReleaseItem(.daemon, "Bug fixes"),
            ]),
        ReleaseNote(
            version: "0.0.11",
            date: "2026-09-29",
            headline: "Ethan's Update",
            items: [
                ReleaseItem(.app, "Schedule an email for later", symbol: "clock.badge"),
                ReleaseItem(.app, "Better draft email UI"),
                ReleaseItem(.app, "Forwards are kept as drafts too"),
                ReleaseItem(.app, "Click a sender to see their contact card", symbol: "person.crop.circle"),
                ReleaseItem(.app, "To, Cc and Bcc show in the reader"),
                ReleaseItem(.app, "Opening an email is instant", symbol: "bolt"),
                ReleaseItem(.app, "Click a rule to edit it. A half-written rule survives Esc"),
                ReleaseItem(.app, "Drag a recipient from To to Cc or Bcc"),
                ReleaseItem(.app, "Space opens the first thing that needs you"),
                ReleaseItem(.app, "e while hovering a package clears it until its next update"),
                ReleaseItem(.app, "Wide emails fit the window"),
                ReleaseItem(.app, "UI improvements"),
                ReleaseItem(.app, "Bug fixes"),
                ReleaseItem(.ios, "Schedule an email for later"),
                ReleaseItem(.ios, "Keep as many drafts as you want"),
                ReleaseItem(.ios, "Wide emails fit the screen"),
                ReleaseItem(.ios, "One chip for autopay bills"),
                ReleaseItem(.daemon, "Scheduled emails wait in the daemon and send on time"),
                ReleaseItem(.daemon, "Triage runs on Opus 5.5", symbol: "sparkles"),
                ReleaseItem(.daemon, "New accounts get triaged while the first sync is still running"),
                ReleaseItem(.daemon, "Suggests a rule for a sender from how you treat their mail"),
                ReleaseItem(.daemon, "Contact details for the contact card"),
                ReleaseItem(.daemon, "Bug fixes"),
            ]),
        ReleaseNote(
            version: "0.0.10",
            date: "2026-09-27",
            headline: "The iPhone catches up.",
            items: [
                ReleaseItem(.app, "Visual Improvements"),
                ReleaseItem(.app, "Email subheaders are displayed better"),
                ReleaseItem(.app, "Search experience tuning"),
                ReleaseItem(.app, "Bug Fixes"),
                ReleaseItem(.ios, "New intro"),
                ReleaseItem(.ios, "Attach files to emails", symbol: "paperclip"),
                ReleaseItem(.ios, "Undo send for 5s", symbol: "arrow.uturn.backward"),
                ReleaseItem(.ios, "Search remembers recent searches and finds partial matches"),
                ReleaseItem(.ios, "\"Newsletters\" is now named \"Reading\""),
                ReleaseItem(.ios, "Chat-style emails are bubbles", symbol: "bubble.left.and.bubble.right"),
                ReleaseItem(.ios, "Notifications show a readable name and your login code"),
                ReleaseItem(.ios, "Tapping a login-code notification opens your codes"),
                ReleaseItem(.ios, "Settings overhaul", symbol: "gearshape"),
            ]),
        ReleaseNote(
            version: "0.0.8",
            date: "2026-09-25",
            headline: "Biggest update by far. Now we're a real product?",
            items: [
                ReleaseItem(.app, "Rebuilt onboarding"),
                ReleaseItem(.app, "Attach files to emails", symbol: "paperclip"),
                ReleaseItem(.app, "Undo send for 5s", symbol: "arrow.uturn.backward"),
                ReleaseItem(.app, "New email composer"),
                ReleaseItem(.app, "Completely overhauled search functionality", symbol: "magnifyingglass"),
                ReleaseItem(.app, "Automatic agentic search"),
                ReleaseItem(.app, "\"Newsletters\" is now named \"Reading\""),
                ReleaseItem(.app, "Better reading experience"),
                ReleaseItem(.app, "New compose and search buttons"),
                ReleaseItem(.app, "Better package tracking"),
                ReleaseItem(.app, "Zoom with Cmd+, Cmd- and Cmd-0"),
                ReleaseItem(.app, "Audit log is moved to settings"),
                ReleaseItem(.daemon, "Rebuilt triage from the ground up", symbol: "wand.and.stars"),
                ReleaseItem(.daemon, "Instant notifications for important emails only"),
                ReleaseItem(.daemon, "For Your Eyes is ranked with importance AND recency"),
                ReleaseItem(.daemon, "Shipments record the item name, merchant and order numbers"),
                ReleaseItem(
                    .daemon,
                    "Login-code emails produce \"Your <service> login code is <code>\" pushes when "
                        + "the code appears verbatim in the email"),
                ReleaseItem(
                    .daemon,
                    "get_inbox_updates and get_thread include the sender's standing instruction "
                        + "(your sender rule's text) with the mail it applies to"),
                ReleaseItem(
                    .daemon, "Plus substantial performance improvements and plenty of bug fixes!"),
            ]),
        ReleaseNote(
            version: "0.0.7",
            date: "2026-09-08",
            headline: "Deeper search, gmail spam, and 2FA codes the moment they land.",
            items: [
                ReleaseItem(
                    .app,
                    "View gmail spam - now you can see your gmail spam tab directly. gmail "
                        + "categorizes this, not Passband"),
                ReleaseItem(.app, "Better spoof sender protection"),
                ReleaseItem(.app, "Fun usage page"),
                ReleaseItem(.app, "The greeting knows your name"),
                ReleaseItem(
                    .app,
                    "Auth mail arrival is instant. Get 2FA codes without waiting for an LLM to "
                        + "see it"),
                ReleaseItem(.app, "The dock icon shows how many things are due today"),
                ReleaseItem(.app, "Rules are grouped by what they do, then by domain"),
                ReleaseItem(.app, "Groups no longer report a delivery shortfall on individual sends"),
                ReleaseItem(.app, "Page headers have a subtitle"),
                ReleaseItem(.app, "Rules, audit and groups pages fill the window again"),
                ReleaseItem(.ios, "New email button in the top right"),
                ReleaseItem(.ios, "Auth page button moved to quick look"),
                ReleaseItem(.ios, "A live login code shows at the top of sitrep"),
                ReleaseItem(.ios, "The composer has been redesigned for better ios usage"),
                ReleaseItem(
                    .ios,
                    "The tracking-pixel switch is desktop only. The account default still "
                        + "decides and review still names a tracked send."),
                ReleaseItem(.ios, "Push banners for auth mail happen right away"),
                ReleaseItem(
                    .ios,
                    "The app icon shows how many things are due today. Turn on Badges in "
                        + "Settings if you don't see it"),
                ReleaseItem(.ios, "Small UX improvements"),
                ReleaseItem(.ios, "All mac improvements above"),
                ReleaseItem(.daemon, "Notifications arrive when the mail does"),
                ReleaseItem(
                    .daemon,
                    "A fast lane decides buzz or no-buzz at ingest on Haiku 4.5. The "
                        + "deliberate lane still owns the rest of triage."),
                ReleaseItem(
                    .daemon,
                    "Sealed mail gets a body-free notification path, kind-derived phrase only"),
                ReleaseItem(.daemon, "A re-ingest can no longer undo a seal you set by hand."),
                ReleaseItem(
                    .daemon,
                    "Provider spam exists locally now and is structurally excluded from every "
                        + "Passband triage feature"),
                ReleaseItem(
                    .daemon,
                    "Empty bodies are no longer stored when a sender's plain-text part is blank "
                        + "beside a full HTML part. This leads to better triage"),
                ReleaseItem(
                    .daemon,
                    "Extractors refuse an empty body instead of guessing an amount from the "
                        + "subject, with their own skip stamp."),
                ReleaseItem(.daemon, "Receipt totals parse the whole number and round it"),
                ReleaseItem(.daemon, "Better deterministic 2FA detection"),
                ReleaseItem(
                    .daemon,
                    "Newsletters don't show in FYE just because you emailed support once"),
                ReleaseItem(.daemon, "Support for usage page"),
                ReleaseItem(.daemon, "Bills get the same total parsing fix as receipts"),
                ReleaseItem(.daemon, "Long bank names are no longer dropped"),
                ReleaseItem(
                    .daemon, "Messages skipped for an empty body get extracted once the body arrives"),
                ReleaseItem(.daemon, "Sealing a message removes its receipt"),
                ReleaseItem(
                    .app,
                    "Deeper search. When keywords aren't enough, the agent searches alongside "
                        + "the results and points at the answer. Automatic, on request, or off "
                        + "in Settings"),
                ReleaseItem(.app, "Type from: in search to pick from the people who write to you"),
                ReleaseItem(.app, "Search results say why they matched"),
                ReleaseItem(.app, "Search-this-sender moved from s in the reader to f in the list"),
                ReleaseItem(
                    .daemon, "Search finds the best partial match instead of nothing. Exact matches still win"),
                ReleaseItem(.daemon, "Search matches word forms. password finds passwords"),
                ReleaseItem(.daemon, "Subject words count more than body words in search"),
                ReleaseItem(.daemon, "Quoted phrases search for the exact phrase"),
                ReleaseItem(.daemon, "Search no longer stalls the daemon on a big mailbox"),
                ReleaseItem(.daemon, "Various bug fixes"),
            ]),
        ReleaseNote(
            version: "0.0.6",
            date: "2026-08-27",
            headline: "Move through emails faster, better search, and notifications to your phone.",
            items: [
                ReleaseItem(
                    .app,
                    "Recency is a factor in search. Old mail is demoted. A clearly better old "
                        + "match still wins. Configurable."),
                ReleaseItem(.app, "Settings has a search field."),
                ReleaseItem(
                    .app,
                    "Shift-E (and Shift-D) finish the email and open the next one. Plain e and "
                        + "d finish it and close."),
                ReleaseItem(.app, "Immediately see new emails for a thread you have open"),
                ReleaseItem(.app, "CC and BCC everywhere an email is written"),
                ReleaseItem(
                    .app,
                    "When your mailbox is catching up after a week away, the re-triage screen "
                        + "says so and counts (1,240 of 4,500 messages) rather than telling you "
                        + "the counter has not moved in a while."),
                ReleaseItem(.app, "Cmd-Enter saves a rule"),
                ReleaseItem(
                    .app,
                    "The check now chip is gone from Shipments. Carriers are polled on a "
                        + "schedule."),
                ReleaseItem(.app, "No more goddam emdashes"),
                ReleaseItem(.ios, "Mobile notifications finally!!"),
                ReleaseItem(.ios, "Switch accounts, and add one"),
                ReleaseItem(.ios, "Mark emails as done in the thread"),
                ReleaseItem(.ios, "Conversational threads look like message bubbles"),
                ReleaseItem(.ios, "CC and BCC in the composer and the inline reply"),
                ReleaseItem(.ios, "Send to groups that you made in the desktop app"),
                ReleaseItem(.ios, "Attachments open in Quick Look and can be saved to Files."),
                ReleaseItem(.ios, "All desktop updates"),
                ReleaseItem(.daemon, "More easily send mail to yourself"),
                ReleaseItem(
                    .daemon,
                    "A CC you emptied stays empty rather than being refilled from the parent."),
                ReleaseItem(.daemon, "Better metrics reporting"),
                ReleaseItem(
                    .daemon,
                    "Push notifications have account names, so a phone with two mailboxes "
                        + "never shows one mailbox's business under the other's name"),
                ReleaseItem(.daemon, "Better semantic search embedding"),
                ReleaseItem(
                    .daemon,
                    "Clearer shipment item names determined by the triage model"),
            ]),
        ReleaseNote(
            version: "0.0.5",
            date: "2026-08-25",
            headline:
                "Groups you can address as one, and a mailbox that says so when it stops working.",
            items: [
                ReleaseItem(
                    .app,
                    "Address a group by name. Groups sits at the bottom of the rail on Cmd-6, "
                        + "holds a named audience, and a Groups button beside the To line puts "
                        + "one into an email."),
                ReleaseItem(
                    .app,
                    "How a group is addressed is settled once, when you make it: everybody on the "
                        + "To line, everybody blind, or one separate email per person. The review "
                        + "before you send says which of the three you are about to do."),
                ReleaseItem(
                    .app,
                    "A group shows what has already gone to it, including the mail you sent those "
                        + "people before the group existed."),
                ReleaseItem(
                    .app,
                    "The composer has a Bcc row, on the messages you want one on."),
                ReleaseItem(
                    .app,
                    "When your mailbox stops working, Passband says so. An expired Google sign-in "
                        + "used to look exactly like nobody writing to you; now a banner says what "
                        + "broke and since when, and on a hosted account it carries the link that "
                        + "repairs it."),
                ReleaseItem(
                    .app,
                    "Re-triage runs in front of you with the queue counting down, instead of a "
                        + "toast over a board still showing the old verdicts."),
                ReleaseItem(
                    .app,
                    "One click on a row in Auth opens the mail the code arrived in."),
                ReleaseItem(
                    .app,
                    "Shipment cards read as one line, and asking one to check now no longer takes "
                        + "the app down with it."),
                ReleaseItem(
                    .app,
                    "The assistant's Opus setting is Opus 5, and a preference saved on the older "
                        + "model moves forward by itself."),
                ReleaseItem(
                    .app,
                    "Inviting a friend hands them the waitlist for now. An invite cannot be "
                        + "redeemed until Google's review clears, so the sheet says so rather than "
                        + "minting a code that would bounce."),
                ReleaseItem(
                    .daemon,
                    "Sending to a group one person at a time happens in the background, so a "
                        + "twelve person list goes out without the send timing out, and the Groups "
                        + "page watches it happen."),
                ReleaseItem(
                    .daemon,
                    "A send that reached some of a group and not the rest says so, and says how "
                        + "many of each."),
                ReleaseItem(
                    .daemon,
                    "Blind copies go out blind and are still recorded, so a bcc-only send lists in "
                        + "your sent mail with everyone it actually reached."),
                ReleaseItem(
                    .daemon,
                    "A model outage no longer costs you the rest of the day. A call the gateway "
                        + "turns away for free is refunded to your triage budget, instead of "
                        + "burning the day's allowance in minutes and leaving mail unjudged until "
                        + "midnight."),
                ReleaseItem(
                    .daemon,
                    "The assistant answers again on hosted accounts, whichever model you picked "
                        + "for it."),
                ReleaseItem(
                    .daemon,
                    "A hosted mailbox that lost its Google consent is reconnected by the person "
                        + "who owns it, from the banner, rather than by somebody reaching into the "
                        + "cluster on their behalf."),
                ReleaseItem(
                    .daemon,
                    "Your daemon notices the moment a mailbox loses its sign-in and remembers "
                        + "since when, which is what the banner in the app is reading."),
            ]),
        ReleaseNote(
            version: "0.0.4",
            date: "2026-08-24",
            headline: "Forwarding, reminders, invites, and mail that renders like its sender meant it to.",
            items: [
                ReleaseItem(
                    .app,
                    "Forward what you are reading. The f key in the reader opens a composer with the "
                        + "original already in it, quote and attachments and all, and shows you "
                        + "exactly what is going along before you send it."),
                ReleaseItem(
                    .app,
                    "Park an email until later. The h key asks when, in the words you would use (\"next "
                        + "tuesday\", \"the 24th\"), and the thread comes back at the top of your "
                        + "board on the day you named."),
                ReleaseItem(
                    .app,
                    "Invite a friend. The share sheet writes the mail, shows you exactly what is "
                        + "going out, and sends it from your own mailbox under your own name."),
                ReleaseItem(
                    .app,
                    "Threads read the way you want them to. Email cards, chat bubbles, or "
                        + "Automatic, which picks per thread from how the conversation actually "
                        + "reads."),
                ReleaseItem(
                    .app,
                    "Embedded images render where their sender put them, instead of collecting at "
                        + "the bottom as attachments."),
                ReleaseItem(
                    .app,
                    "Notification banners carry the sender's own mark, a brand's logo or a "
                        + "correspondent's initials, so you know who wrote before you open it."),
                ReleaseItem(
                    .app,
                    "The window reads as one bar. The email's subject sits up beside the traffic "
                        + "lights, and the mail bar stops shuffling when you switch pages."),
                ReleaseItem(
                    .app,
                    "A long thread scrolls without stutter, however far back it goes."),
                ReleaseItem(
                    .app,
                    "Unsubscribing or blocking a sender closes the email and moves you on, rather "
                        + "than leaving you holding the thing you just got rid of."),
                ReleaseItem(
                    .app,
                    "The banking card clears once you have actually seen it, and stays cleared "
                        + "across a restart."),
                ReleaseItem(
                    .app,
                    "The s key in the reader searches everything from that sender."),
                ReleaseItem(
                    .app,
                    "This card. New versions say what they brought, once, and never again."),
                ReleaseItem(
                    .daemon,
                    "Senders writing in Chinese, Japanese or Korean arrive as their names instead "
                        + "of a row of question marks."),
                ReleaseItem(
                    .daemon,
                    "Login codes and 2FA mail stay sealed even when they are worded oddly enough "
                        + "to dodge the usual patterns."),
                ReleaseItem(
                    .daemon,
                    "A triage outage no longer costs you mail. Rows stay queued when the model is "
                        + "unreachable and get judged when it comes back, instead of being filed "
                        + "on a guess."),
                ReleaseItem(
                    .daemon,
                    "Reminders and forwarding are served by the daemon, so both work from any "
                        + "client you have paired."),
                ReleaseItem(
                    .daemon,
                    "New mail reaches the screen in seconds. Gmail is polled every five seconds "
                        + "rather than every forty-five, so a login code is in front of you about "
                        + "as fast as it arrives."),
                ReleaseItem(
                    .daemon,
                    "An invite you send goes out through your own Gmail, and the address you sent "
                        + "it to never leaves your machine."),
            ]),
        ReleaseNote(
            version: "0.0.3",
            date: "2026-08-18",
            headline: "Updates that take one click, and a reader that holds still.",
            items: [
                ReleaseItem(
                    .app,
                    "A new version arrives as a card in the window with a single Update button "
                        + "that installs it and relaunches, instead of two dialogs asking the same "
                        + "question twice."),
                ReleaseItem(
                    .app,
                    "Attachments open in Quick Look, the same panel the Finder uses. Photos render "
                        + "in the column and open on a click."),
                ReleaseItem(
                    .app,
                    "A thread reads oldest to newest with the newest parked at the top, and the "
                        + "rail beside it holds still while you scroll."),
                ReleaseItem(
                    .app,
                    "Settings stamps the version in the corner, selectable, because that is the "
                        + "first thing any bug report asks for."),
                ReleaseItem(
                    .daemon,
                    "Triage verdicts expire, so a sender you have since taught it about is judged "
                        + "again rather than on a months-old opinion."),
                ReleaseItem(
                    .daemon,
                    "A pattern match no longer decides a tier by itself, and asking for a "
                        + "second opinion buys the model more context rather than just more time."),
                ReleaseItem(
                    .daemon,
                    "Asking for a re-triage outranks the age cutoff, so an old thread can still be "
                        + "reconsidered."),
            ]),
        ReleaseNote(
            version: "0.0.2",
            date: "2026-08-14",
            headline: "Packages that track themselves, and a gate that asks the right question first.",
            items: [
                ReleaseItem(
                    .app,
                    "The connect screen asks where your mail should run before it asks for a "
                        + "credential, so the answer you give matches the install you have."),
                ReleaseItem(
                    .app,
                    "Newsletter images load over plain HTTP, which is where a surprising amount of "
                        + "newsletter art still lives."),
                ReleaseItem(
                    .app,
                    "The shipment card says what the carrier said, not just what the email "
                        + "claimed."),
                ReleaseItem(
                    .daemon,
                    "Package tracking talks to four carriers directly, and a shipment goes quiet "
                        + "seven days after it lands."),
                ReleaseItem(
                    .daemon,
                    "A failed sign-in no longer forfeits the message it was working on."),
            ]),
        ReleaseNote(
            version: "0.0.1",
            date: "2026-08-12",
            headline: "The first build.",
            items: [
                ReleaseItem(.app, "Passband, on your desk."),
                ReleaseItem(.daemon, "squelchd, triaging your mail on a machine you control."),
            ]),
    ]

    /// The releases a human last stamped at `lastSeen` has not read, newest
    /// first.
    ///
    /// Bounded ABOVE by the running build on purpose: this table can describe a
    /// version newer than the copy compiled around it (a note written before the
    /// tag lands), and announcing features that are not installed is worse than
    /// announcing them late.
    ///
    /// NO STAMP is one of two people. A fresh install, who should be reading the
    /// tour rather than a back catalogue, and an install that predates this
    /// feature and is owed the release it just took. The caller separates them
    /// (see WhatsNew.maybeShow); the seed here is the second reading, floored at
    /// the release BEFORE the running one so this build announces itself and
    /// nothing older.
    static func unseen(lastSeen: String?, running: String) -> [ReleaseNote] {
        ReleaseLedger.unseen(all, lastSeen: lastSeen, running: running)
    }

    /// The greeting owed with `shown`, or nil for the ordinary one-release
    /// update, which needs no explaining. `lastSeen` is the stamp, so "since
    /// you were on" names the last release this install read notes for.
    static func catchUp(lastSeen: String?, running: String, shown: [ReleaseNote]) -> ReleaseCatchUp? {
        guard shown.count > 1, let lastSeen, ReleaseVersion(lastSeen) != nil else { return nil }
        return ReleaseCatchUp(from: lastSeen, to: running)
    }

    /// What to stamp once the card is closed: the newest note actually SHOWN,
    /// never the running build. A copy of the app carrying no note for some
    /// release must leave that release unstamped so a later build can still
    /// announce it, rather than swallowing it on the reader's behalf.
    static func newestShown(_ notes: [ReleaseNote]) -> String? {
        ReleaseLedger.newestShown(notes)
    }

    /// The newest release strictly older than `version`, or nil when this is
    /// the oldest one on record.
    static func previous(before version: ReleaseVersion) -> ReleaseVersion? {
        ReleaseLedger.previous(all, before: version)
    }

    /// The newest note this build is at or past, for "what's new in this
    /// version" asked directly. At or BELOW rather than exactly equal: a build
    /// cut without a note of its own (a hotfix, a version bumped ahead of the
    /// writing) should answer with the last thing that actually shipped rather
    /// than with nothing.
    static func newest(atOrBelow running: String) -> ReleaseNote? {
        ReleaseLedger.newest(all, atOrBelow: running)
    }
}

/// A note keyed by the version of whatever shipped it.
protocol VersionedNote {
    var version: String { get }
}

extension ReleaseNote: VersionedNote {}

/// THE SELECTION RULES, over any table of versioned notes. There are two
/// tables: the one above, keyed by the Mac app's version, and the iPhone's own
/// (Lib/PhoneReleaseNotes.swift), keyed by the phone's. They are numbered on
/// different lines and say different things, but which notes an install is
/// owed, what closing them stamps and what Settings shows again are the same
/// questions with the same wrong answers, so both tables ask them here. What
/// each rule is FOR is written on `ReleaseNotes`' wrapper of it.
enum ReleaseLedger {
    static func unseen<Note: VersionedNote>(_ all: [Note], lastSeen: String?, running: String) -> [Note] {
        guard let ceiling = ReleaseVersion(running) else { return [] }
        let floor = lastSeen.flatMap(ReleaseVersion.init) ?? previous(all, before: ceiling)
        return ordered(all)
            .filter { $0.version <= ceiling }
            .filter { pair in
                guard let floor else { return true }
                return pair.version > floor
            }
            .map(\.note)
    }

    static func newestShown<Note: VersionedNote>(_ notes: [Note]) -> String? {
        ordered(notes).first?.note.version
    }

    static func previous<Note: VersionedNote>(_ all: [Note], before version: ReleaseVersion) -> ReleaseVersion? {
        ordered(all).first { $0.version < version }?.version
    }

    static func newest<Note: VersionedNote>(_ all: [Note], atOrBelow running: String) -> Note? {
        guard let ceiling = ReleaseVersion(running) else { return nil }
        return ordered(all).first { $0.version <= ceiling }?.note
    }

    /// A table with unparseable rows dropped and the order enforced rather
    /// than trusted. Both are authored newest first; this is what guarantees
    /// it.
    private static func ordered<Note: VersionedNote>(_ all: [Note]) -> [(note: Note, version: ReleaseVersion)] {
        all.compactMap { note in ReleaseVersion(note.version).map { (note: note, version: $0) } }
            .sorted { $0.version > $1.version }
    }
}
