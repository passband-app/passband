// THE PURGE: a walk through the Reading zone one SENDER at a time, worst
// offenders first, each with the evidence for why the mail is probably no
// longer wanted and four things to do about it.
//
// DECISIONS ARE STAGED, NOT SENT. An unsubscribe is a request to a third party
// and cannot be recalled, so nothing leaves until the person confirms at the
// end, and stepping back through the deck costs nothing. This file is that
// deck and its arithmetic: which senders are offered at all, what each may be
// asked, what is suggested, and the sentences the evidence is put in. It makes
// no request and holds no clock, which is what lets test.sh compile it alone;
// the store (`AppStore.commitPurge`) is the half that acts.

import Foundation

enum PurgeDecision: String, Sendable, Hashable, CaseIterable {
    /// Ask the sender to stop. The daemon marks their open mail done too.
    case unsubscribe
    /// A mute rule for the exact address, sweeping their open mail.
    case mute
    /// Stay subscribed, mark what is open as done, and remember the choice so
    /// the next purge does not ask about the same sender.
    case clear
    /// Leave everything exactly as it is.
    case skip
}

/// One sender on the deck, with what the deck worked out about it.
struct PurgeCard: Identifiable, Hashable, Sendable {
    let sender: PurgeSender
    let allowed: Set<PurgeDecision>
    /// The choice the evidence points at, or nil when it points nowhere. A
    /// nudge, never a default the confirm step applies by itself.
    let recommended: PurgeDecision?

    var id: String { sender.sender }

    /// An unsubscribe from this sender cannot be done for the person: the
    /// daemon would hand back the sender's page to finish on. Such a decision
    /// SENDS NOTHING at commit. The daemon writes a request down the moment it
    /// is asked, and holds the sender to it 72 hours later; asking on behalf
    /// of someone who may never visit the page would have the sender accused
    /// of ignoring a request nobody made. The request goes when they follow
    /// the link (`AppStore.openPurgeSite`), and until then nothing is claimed.
    var unsubscribeIsOnTheirSite: Bool { sender.unsubscribe?.auto == false }

    /// A score at or above this is a sender the owner has all but stopped
    /// reading: suggest ending it.
    static let endAt = 70
    /// From here up to `endAt` the mail is piling up but the evidence is not
    /// strong enough to suggest leaving: suggest clearing it and staying.
    static let clearAt = 40

    /// nil when the sender does not belong on the deck at all.
    ///
    /// A PERSON THE OWNER WRITES TO IS NEVER OFFERED FOR PURGING. That is a
    /// product rule, not a threshold: the deck exists for mail nobody asked
    /// for, and one card suggesting someone unsubscribe from their sister
    /// costs more trust than the whole flow earns. A `surface` rule is the
    /// same statement made on purpose. And a sender already asked to stop is
    /// the unsubscribe tray's business; asking twice is not this flow's job.
    ///
    /// A sender KEPT in a recent purge has been answered already. Read off the
    /// daemon's reason and not off `kept_at`: how long an answer stands is the
    /// daemon's number, and a copy of it here would be a second one.
    init?(_ sender: PurgeSender) {
        if sender.known_contact { return nil }
        if sender.rule?.disposition == .surface { return nil }
        if sender.unsubscribed != nil { return nil }
        if sender.reasons.contains(.keptRecently) { return nil }

        var allowed: Set<PurgeDecision> = [.clear, .skip]
        // Offered only when the daemon found a link to do it with. A button
        // that ends in "no link after all" is worse than no button.
        if sender.unsubscribe != nil { allowed.insert(.unsubscribe) }
        // A mute rule already governs them: a second one changes nothing.
        if sender.rule?.disposition != .squelch { allowed.insert(.mute) }

        self.sender = sender
        self.allowed = allowed
        self.recommended = Self.recommend(sender, allowed: allowed)
    }

    private static func recommend(_ sender: PurgeSender, allowed: Set<PurgeDecision>)
        -> PurgeDecision?
    {
        guard let score = sender.score, score >= clearAt else { return nil }
        // The score can still run high on volume alone for mail that is read
        // start to finish. When the strongest thing the daemon has to say is
        // that it IS read, suggesting it end would be arguing with the
        // evidence printed right above the button.
        let readClosely: Bool
        if case .readClosely = sender.reasons.first { readClosely = true } else { readClosely = false }
        guard score >= endAt, !readClosely else { return .clear }
        if allowed.contains(.unsubscribe) { return .unsubscribe }
        if allowed.contains(.mute) { return .mute }
        // Already muted and no link: all that is left to do is tidy up.
        return .clear
    }
}

/// The deck: the cards in the daemon's order, where the person is in them,
/// and what they have said so far.
struct PurgeDeck: Hashable, Sendable {
    struct Progress: Hashable, Sendable {
        /// 1-based, and it stops at `total`: a finished deck reads "9 of 9".
        var position: Int
        var total: Int
    }

    let cards: [PurgeCard]
    private(set) var cursor = 0
    /// Keyed by sender address. A sender with no entry is UNTOUCHED, which is
    /// not the same as skipped: nobody looked at it.
    private(set) var staged: [String: PurgeDecision] = [:]
    /// Left before the last card. Kept apart from the cursor so `back` from
    /// the confirm step returns to the card they left on, not the last one.
    private(set) var leftEarly = false

    /// The daemon's order is kept. It is already worst first, and the reasons
    /// on each card are the ones that order was built from.
    init(senders: [PurgeSender]) {
        cards = senders.compactMap(PurgeCard.init)
    }

    var isEmpty: Bool { cards.isEmpty }
    var isFinished: Bool { leftEarly || cursor >= cards.count }
    var current: PurgeCard? { isFinished ? nil : cards[cursor] }

    /// For a quiet "3 of 9". A position, never a count of what is left.
    var progress: Progress {
        Progress(position: min(cursor + 1, cards.count), total: cards.count)
    }

    func decision(for sender: String) -> PurgeDecision? { staged[sender] }

    /// Every staged decision, in deck order.
    var plan: [(card: PurgeCard, decision: PurgeDecision)] {
        cards.compactMap { card in staged[card.id].map { (card: card, decision: $0) } }
    }

    /// The staged unsubscribes that end on the sender's own site, in deck
    /// order: mail cleared at commit, the request itself left for the visit.
    var siteBound: [PurgeCard] {
        plan.filter { $0.decision == .unsubscribe && $0.card.unsubscribeIsOnTheirSite }.map(\.card)
    }

    /// Stage `decision` for the card on screen and move on. False, and nothing
    /// changes, when the deck is over or the card does not offer that choice:
    /// a key bound to "unsubscribe" must do nothing on a card with no link,
    /// not stage something the commit would have to invent an answer for.
    @discardableResult
    mutating func decide(_ decision: PurgeDecision) -> Bool {
        guard let card = current, card.allowed.contains(decision) else { return false }
        staged[card.id] = decision
        cursor += 1
        return true
    }

    /// Step to the previous card and take its decision back, so it is asked
    /// again rather than shown pre-answered. From a deck left early this
    /// returns to the card that was on screen, which had no decision to undo.
    /// False at the first card.
    @discardableResult
    mutating func back() -> Bool {
        if leftEarly {
            leftEarly = false
            return true
        }
        guard cursor > 0 else { return false }
        cursor -= 1
        staged[cards[cursor].id] = nil
        return true
    }

    /// Stop here. What was decided stands and every later card is untouched.
    mutating func finishEarly() {
        guard !isFinished else { return }
        leftEarly = true
    }
}

/// What happened to one staged decision at commit.
enum PurgeOutcome: Hashable, Sendable {
    /// What was DONE, which is not always what was asked: an unsubscribe that
    /// found no link after all falls back to a mute. `emailsCleared` is what
    /// actually left Reading.
    case applied(PurgeDecision, emailsCleared: Int)
    /// An unsubscribe that is the person's to finish on the sender's site.
    /// Their open mail was cleared; they are still subscribed.
    case onTheirSite(emailsCleared: Int)
    case failed
}

/// What a purge comes to. Every number here is something that went away,
/// which is the only kind of count this flow shows anybody.
struct PurgeSummary: Hashable, Sendable {
    var unsubscribed = 0
    var muted = 0
    var cleared = 0
    /// Senders waiting on a visit to their site. NOT in `unsubscribed` and
    /// not in `fewerPerMonth`: neither has happened yet, and the end card
    /// does not get to claim them.
    var onTheirSite = 0
    var skipped = 0
    /// Staged, tried, and did not go through. Zero before commit.
    var failed = 0
    /// Open mail that leaves Reading: every decision but skip takes the
    /// sender's open mail with it.
    var emailsCleared = 0
    /// Mail a month that stops arriving, from the senders ended or muted.
    /// Clearing does not count: they are still subscribed.
    var fewerPerMonth = 0

    /// Nothing but skips (or nothing at all): there is nothing to confirm and
    /// nothing to celebrate.
    var isEmpty: Bool { unsubscribed + muted + cleared + onTheirSite == 0 }

    /// What the staged decisions WOULD come to, for the confirm step.
    init(deck: PurgeDeck) {
        self.init(deck: deck, outcomes: nil)
    }

    /// What they DID come to. A decision with no outcome was never tried (the
    /// commit was cut short) and counts as failed: it did not happen.
    init(deck: PurgeDeck, outcomes: [String: PurgeOutcome]) {
        self.init(deck: deck, outcomes: Optional(outcomes))
    }

    private init(deck: PurgeDeck, outcomes: [String: PurgeOutcome]?) {
        // Summed as fractions and rounded ONCE: five senders at 0.4 a month
        // are two fewer emails, not zero.
        var perMonth = 0.0
        for (card, staged) in deck.plan {
            if staged == .skip {
                skipped += 1
                continue
            }
            // nil = it ends on the sender's site.
            let done: PurgeDecision?
            let emails: Int
            if let outcomes {
                switch outcomes[card.id] {
                case .applied(let applied, let count)?:
                    done = applied
                    emails = count
                case .onTheirSite(let count)?:
                    done = nil
                    emails = count
                case .failed?, nil:
                    failed += 1
                    continue
                }
            } else {
                // The same split the commit will make, so the confirm step
                // and the end card never disagree about what was promised.
                done = staged == .unsubscribe && card.unsubscribeIsOnTheirSite ? nil : staged
                emails = card.sender.open_count
            }
            emailsCleared += emails
            switch done {
            case nil: onTheirSite += 1
            case .unsubscribe:
                unsubscribed += 1
                perMonth += card.sender.signals.per_month ?? 0
            case .mute:
                muted += 1
                perMonth += card.sender.signals.per_month ?? 0
            case .clear: cleared += 1
            case .skip: skipped += 1
            }
        }
        fewerPerMonth = Int(perMonth.rounded())
    }
}

/// The evidence, in words. One short sentence per reason, numbers rounded to
/// what a person would say out loud: these are observations about somebody's
/// own reading, and "4,217 ms" is a measurement where "4 seconds" is a fact.
enum PurgeCopy {
    static func sentence(_ reason: PurgeReason) -> String {
        switch reason {
        case .rarelyOpened(let opened, let received):
            return opened <= 0 ? "Opened none of \(received)" : "Opened \(opened) of \(received)"
        case .clearedUnopened(let count):
            return "Cleared \(count) without opening"
        case .skimmed(_, let words, let dwellMs):
            return "About \(Self.words(words)) each, read for \(duration(ms: dwellMs))"
        case .briefLooks(let dwellMs):
            // "about less than a second" is not a sentence.
            let about = dwellMs < 500 ? "" : "about "
            return "Opened for \(about)\(duration(ms: dwellMs)) each"
        case .neverClicked:
            return "Never clicked a link"
        case .highVolume(let perMonth):
            let n = Int(perMonth.rounded())
            return n < 1 ? "Less than 1 a month" : "About \(n) a month"
        case .readClosely:
            return "You read most of each one"
        case .knownContact:
            return "You write to this address"
        case .wantedByRule:
            return "One of your rules asks for this sender"
        case .keptRecently:
            return "You chose to keep these"
        case .notEnoughHistory:
            return "Not enough history yet"
        }
    }

    /// The span the evidence covers, to sit beside it: "in the last 34 days".
    /// The DAYS, not "since you started using Passband": the ledger's age is
    /// what the counts are honestly about, and the app has no other phrase
    /// for its own history to stay consistent with.
    static func window(ledgerDays: Int) -> String {
        ledgerDays <= 1 ? "in the last day" : "in the last \(ledgerDays) days"
    }

    /// Seconds under a minute, then minutes.
    static func duration(ms: Int) -> String {
        let seconds = Int((Double(max(0, ms)) / 1000).rounded())
        if seconds < 1 { return "less than a second" }
        if seconds < 60 { return seconds == 1 ? "1 second" : "\(seconds) seconds" }
        let minutes = Int((Double(seconds) / 60).rounded())
        return minutes == 1 ? "1 minute" : "\(minutes) minutes"
    }

    /// A length nobody counted exactly: to the nearest 50 above 200 words and
    /// the nearest 10 below, where 50 would turn 120 into 100.
    static func words(_ count: Int) -> String {
        let count = max(0, count)
        let step = count > 200 ? 50 : (count >= 20 ? 10 : 1)
        let rounded = Int((Double(count) / Double(step)).rounded()) * step
        return rounded == 1 ? "1 word" : "\(grouped(rounded)) words"
    }

    /// 1200 as "1,200". By hand so the sentence is the same on every machine
    /// the tests run on, whatever its locale.
    private static func grouped(_ n: Int) -> String {
        var out = ""
        for (i, digit) in String(n).reversed().enumerated() {
            if i > 0, i % 3 == 0 { out.append(",") }
            out.append(digit)
        }
        return String(out.reversed())
    }
}

// MARK: - what the deck, the confirm step and the end card say

/// How the evidence's own numbers are drawn as marks. A row of 23 marks with
/// one lit says "opened 1 of 23" faster than the sentence does, but only
/// while each mark is still an email: past `cap` the row is drawn at `cap`
/// and the lit share is kept in proportion, never rounded away to nothing or
/// up to everything.
enum PurgeMarks {
    static let cap = 24

    static func split(lit: Int, of total: Int, cap: Int = PurgeMarks.cap) -> (lit: Int, total: Int) {
        let total = max(0, total)
        let lit = min(max(0, lit), total)
        guard total > cap else { return (lit, total) }
        var scaled = Int((Double(lit) / Double(total) * Double(cap)).rounded())
        // One opened in two hundred is still one: a row with none lit would
        // say "never". And one unopened is still one.
        if lit > 0 { scaled = max(1, scaled) }
        if lit < total { scaled = min(cap - 1, scaled) }
        return (scaled, cap)
    }

    /// A month as thirty days, with about `perMonth` of them carrying mail.
    static func days(perMonth: Double) -> (lit: Int, total: Int) {
        (min(30, max(0, Int(perMonth.rounded()))), 30)
    }
}

extension PurgeCopy {
    /// The four choices as the card names them.
    static func title(_ decision: PurgeDecision) -> String {
        switch decision {
        case .unsubscribe: "Unsubscribe"
        case .mute: "Mute"
        case .clear: "Clear"
        case .skip: "Skip"
        }
    }

    /// The few words under each choice: what it does, or why it is not on
    /// offer. An unsubscribe the app cannot finish says so HERE, before it is
    /// chosen, not on the end card after.
    static func caption(_ decision: PurgeDecision, on card: PurgeCard) -> String {
        let allowed = card.allowed.contains(decision)
        switch decision {
        case .unsubscribe:
            if !allowed { return "no link found" }
            return card.unsubscribeIsOnTheirSite ? "you finish on their site" : "ask them to stop"
        case .mute:
            return allowed ? "hide what they send" : "already muted"
        case .clear:
            return "stay subscribed"
        case .skip:
            return "leave as is"
        }
    }

    /// Said aloud as a decision is staged. STAGED, in the words: nothing has
    /// been sent, and a listener cannot see the card that says so.
    static func staged(_ decision: PurgeDecision, name: String) -> String {
        switch decision {
        case .unsubscribe: "Will unsubscribe from \(name)."
        case .mute: "Will mute \(name)."
        case .clear: "Will clear \(name) and stay subscribed."
        case .skip: "Skipped \(name)."
        }
    }

    /// The confirm step: what one press will do, a line per kind, in the
    /// order they matter. Kinds with nothing staged are left out.
    static func plan(_ summary: PurgeSummary) -> [String] {
        var out: [String] = []
        let ending = summary.unsubscribed + summary.onTheirSite
        if ending > 0 { out.append("Unsubscribe from \(ending)") }
        if summary.muted > 0 { out.append("Mute \(summary.muted)") }
        if summary.cleared > 0 { out.append("Clear \(summary.cleared) and stay subscribed") }
        return out
    }

    /// Unsubscribes the app cannot finish, said plainly BEFORE the press.
    /// nil when there are none.
    static func siteNote(_ summary: PurgeSummary) -> String? {
        let n = summary.onTheirSite
        guard n > 0 else { return nil }
        let ending = summary.unsubscribed + summary.onTheirSite
        if ending == 1 {
            return "That one needs a visit to the sender's site afterwards. Until then you are still subscribed."
        }
        let which = n == ending ? (n == 2 ? "Both" : "All \(n)") : "\(n) of them"
        let verb = n == 1 ? "needs" : "need"
        let those = n == 1 ? "that one" : "those"
        return "\(which) \(verb) a visit to the sender's site afterwards. Until then you are still subscribed to \(those)."
    }

    /// "31 emails leave Reading". nil when none do.
    static func leaving(_ emails: Int) -> String? {
        guard emails > 0 else { return nil }
        return emails == 1 ? "1 email leaves Reading" : "\(emails) emails leave Reading"
    }

    /// "about 60 fewer a month". nil at zero: clearing ends nothing.
    static func fewer(_ perMonth: Int) -> String? {
        guard perMonth > 0 else { return nil }
        return "about \(perMonth) fewer a month"
    }

    /// The end card's headline: the best true thing about what happened.
    static func finished(_ summary: PurgeSummary) -> String {
        if summary.fewerPerMonth > 0 {
            return summary.fewerPerMonth == 1
                ? "About 1 fewer email a month." : "About \(summary.fewerPerMonth) fewer emails a month."
        }
        if summary.emailsCleared > 0 {
            return summary.emailsCleared == 1
                ? "1 email cleared." : "\(summary.emailsCleared) emails cleared."
        }
        return "Reading is lighter."
    }

    /// Decisions that did not go through. nil when all did.
    static func failedNote(_ failed: Int) -> String? {
        guard failed > 0 else { return nil }
        return failed == 1
            ? "1 did not go through and is still in Reading."
            : "\(failed) did not go through and are still in Reading."
    }
}
