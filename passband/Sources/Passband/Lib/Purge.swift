// THE PURGE: a walk through the senders in Reading that the owner has all but
// stopped reading, one SENDER at a time, each with the evidence for why and a
// way to end it.
//
// ONLY CONFIDENT CANDIDATES ARE ON THE DECK. A sender the evidence says is
// worth keeping, one it is unsure about, and one it knows too little about
// are simply absent: no card, no option, no mention. Being asked "do you want
// to unsubscribe from the newsletter you read every week" costs more trust
// than the whole flow earns.
//
// DECISIONS ARE STAGED, NOT SENT. An unsubscribe is a request to a third party
// and cannot be recalled, so nothing leaves until the person confirms at the
// end, and stepping back through the deck costs nothing. This file is that
// deck and its arithmetic: which senders are offered, what each may be asked,
// what a set of decisions comes to, and the sentences all of it is put in. It
// makes no request and holds no clock, which is what lets test.sh compile it
// alone; the store (`AppStore.commitPurge`) is the half that acts.

import Foundation

enum PurgeDecision: Hashable, Sendable {
    /// Ask the sender to stop. Offered when the daemon found a link.
    case unsubscribe
    /// A mute rule for the exact address, sweeping their open mail. Offered
    /// INSTEAD of unsubscribe when none of their mail carries a link.
    case block
    /// Keep part of what they send: a filtered rule on the exact address
    /// with this sentence, one of the suggestions shown on the card. It
    /// carries its sentence so it survives stepping back and the confirm page.
    case rule(String)
    /// We got this one wrong. Nothing is sent but a note to leave this
    /// sender out of the next purge.
    case keep

    enum Kind: String, CaseIterable, Sendable, Hashable {
        case unsubscribe, block, rule, keep
    }

    var kind: Kind {
        switch self {
        case .unsubscribe: .unsubscribe
        case .block: .block
        case .rule: .rule
        case .keep: .keep
        }
    }
}

/// One sender on the deck, with what the deck worked out about it.
struct PurgeCard: Identifiable, Hashable, Sendable {
    let sender: PurgeSender

    var id: String { sender.sender }

    /// How this sender is ended: an unsubscribe when there is a link to do
    /// it with, a block when there is none. Never both: a card offers one
    /// way out, in one place, on one key.
    var ending: PurgeDecision { sender.unsubscribe != nil ? .unsubscribe : .block }

    /// An unsubscribe from this sender cannot be done for the person: the
    /// daemon would hand back the sender's page to finish on. Such a decision
    /// SENDS NOTHING at commit. The daemon writes a request down the moment it
    /// is asked, and holds the sender to it 72 hours later; asking on behalf
    /// of someone who may never visit the page would have the sender accused
    /// of ignoring a request nobody made. The request goes when they follow
    /// the link (`AppStore.openPurgeSite`), and until then nothing is claimed.
    var unsubscribeIsOnTheirSite: Bool { sender.unsubscribe?.auto == false }

    /// Whether narrowing rules are suggested for this sender. Not when a
    /// rule already governs them: a second one on the same address would be
    /// two answers to one question.
    var offersRules: Bool { sender.rule == nil }

    /// A score at or above this is a sender the owner has all but stopped
    /// reading. Below it the deck does not know enough to ask.
    static let endAt = 70

    /// nil when the sender does not belong on the deck at all.
    ///
    /// A PERSON THE OWNER WRITES TO IS NEVER OFFERED FOR PURGING. That is a
    /// product rule, not a threshold. A `surface` rule is the same statement
    /// made on purpose. A sender already asked to stop is the unsubscribe
    /// tray's business. A sender KEPT in a recent purge has been answered
    /// already (read off the daemon's reason, not `kept_at`: how long an
    /// answer stands is the daemon's number).
    ///
    /// THEN ONLY THE CONFIDENT ONES. No score, or one under `endAt`, is
    /// "unsure". A top reason of `read_closely` is "worth keeping" however
    /// high volume alone pushed the score. Neither is shown.
    ///
    /// AND ONLY WHERE THERE IS SOMETHING TO OFFER. No link and already
    /// muted: there is no way to end it that has not been taken.
    init?(_ sender: PurgeSender) {
        if sender.known_contact { return nil }
        if sender.rule?.disposition == .surface { return nil }
        if sender.unsubscribed != nil { return nil }
        if sender.reasons.contains(.keptRecently) { return nil }
        guard let score = sender.score, score >= Self.endAt else { return nil }
        if case .readClosely = sender.reasons.first { return nil }
        if sender.unsubscribe == nil, sender.rule?.disposition == .squelch { return nil }
        self.sender = sender
    }

    /// Whether this card offers `decision`. A rule must carry a sentence.
    func allows(_ decision: PurgeDecision) -> Bool {
        switch decision {
        case .unsubscribe, .block: decision == ending
        case .rule(let want):
            offersRules && !want.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .keep: true
        }
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

    /// Fewer candidates than this and the purge is not offered at all: a
    /// flow with a confirm page and an end page for one sender is ceremony,
    /// and that one sender has an unsubscribe key in the reader.
    static let minimumCandidates = 3

    let cards: [PurgeCard]
    private(set) var cursor = 0
    /// Keyed by sender address. A sender with no entry is UNTOUCHED, which is
    /// not the same as kept: nobody looked at it.
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
    /// Enough candidates for the purge to be worth offering.
    var isWorthOffering: Bool { cards.count >= Self.minimumCandidates }
    var isFinished: Bool { leftEarly || cursor >= cards.count }
    var current: PurgeCard? { isFinished ? nil : cards[cursor] }
    /// The card after the one on screen, whose suggestions are worth
    /// fetching ahead. nil at the end.
    var next: PurgeCard? {
        !isFinished && cursor + 1 < cards.count ? cards[cursor + 1] : nil
    }
    /// The card `back` would return to, and what was decided about it.
    var previous: (card: PurgeCard, decision: PurgeDecision)? {
        guard !leftEarly, cursor > 0, cursor <= cards.count else { return nil }
        let card = cards[cursor - 1]
        return staged[card.id].map { (card, $0) }
    }

    /// For a quiet progress line. A position, never a count of what is left.
    var progress: Progress {
        Progress(position: min(cursor + 1, cards.count), total: cards.count)
    }

    func decision(for sender: String) -> PurgeDecision? { staged[sender] }

    /// Every staged decision, in deck order.
    var plan: [(card: PurgeCard, decision: PurgeDecision)] {
        cards.compactMap { card in staged[card.id].map { (card: card, decision: $0) } }
    }

    /// Stage `decision` for the card on screen and move on. False, and nothing
    /// changes, when the deck is over or the card does not offer that choice.
    @discardableResult
    mutating func decide(_ decision: PurgeDecision) -> Bool {
        guard let card = current, card.allows(decision) else { return false }
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

/// What the commit did about one staged decision.
enum PurgeOutcome: Hashable, Sendable {
    /// The daemon took the unsubscribe as a job. Whether it WORKED is the
    /// job's to say, later (`PurgeStatus.resolve`).
    case unsubscribing
    /// An unsubscribe that is the person's to finish on the sender's site.
    case onTheirSite
    /// Blocked: chosen, or what an unsubscribe fell back to when the link
    /// turned out not to exist after all.
    case blocked
    case ruled(String)
    case kept
    case failed
}

/// The state of an unsubscribe job, as much as the purge needs of it.
enum PurgeJob: Hashable, Sendable {
    case running
    case done
    case needsYou(reason: String?)
}

/// Where one sender stands on the end page, now.
enum PurgeStatus: Hashable, Sendable {
    /// The daemon is working on the unsubscribe.
    case running
    case unsubscribed
    /// Waiting on the person: a page to visit. `reason` is the daemon's, when
    /// it tried and could not finish; nil when it never could have.
    case needsYou(reason: String?)
    /// The person went to the sender's page. Whether they finished there is
    /// theirs to know: it is not counted as unsubscribed.
    case opened
    case blocked
    case ruled(String)
    case kept
    case failed

    /// The reason given for a sender that was never a job: its unsubscribe
    /// could only ever be finished on its own site.
    static let siteOnly = "site_only"

    /// `job` is the ledger's word on this sender, nil before its first row
    /// arrives (the job was accepted, so that is still "running"). `opened`
    /// is whether the person has followed this sender's link from the page.
    static func resolve(_ outcome: PurgeOutcome, job: PurgeJob?, opened: Bool) -> PurgeStatus {
        switch outcome {
        case .unsubscribing:
            switch job {
            case .done?: return .unsubscribed
            case .needsYou(let reason)?: return opened ? .opened : .needsYou(reason: reason)
            case .running?, nil: return .running
            }
        case .onTheirSite: return opened ? .opened : .needsYou(reason: siteOnly)
        case .blocked: return .blocked
        case .ruled(let want): return .ruled(want)
        case .kept: return .kept
        case .failed: return .failed
        }
    }
}

/// What a purge comes to. Every number here is something decided or done,
/// which is the only kind of count this flow shows anybody.
struct PurgeSummary: Hashable, Sendable {
    /// Staged: unsubscribes the daemon will try. Done: ones that went through.
    var unsubscribed = 0
    var blocked = 0
    var rules = 0
    var kept = 0
    /// Unsubscribes waiting on a visit to the sender's site. NOT in
    /// `unsubscribed` and not in `fewerPerMonth`: neither has happened.
    var needsYou = 0
    /// Followed to the sender's site from the end page. Not claimed as done.
    var opened = 0
    /// Unsubscribe jobs still running. Zero before commit.
    var running = 0
    /// Staged, tried, and did not go through. Zero before commit.
    var failed = 0
    /// Mail a month that stops arriving: unsubscribes that went through (or,
    /// staged, that the daemon will try) plus blocks. A rule narrows by an
    /// amount nobody knows, so it claims nothing.
    var fewerPerMonth = 0

    /// Nothing decided at all.
    var isEmpty: Bool { unsubscribed + blocked + rules + kept + needsYou + opened + running + failed == 0 }
    /// Nothing that changes what arrives: only keeps (or nothing).
    var changesNothing: Bool { unsubscribed + blocked + rules + needsYou + opened + running == 0 }
    /// Every unsubscribe the person chose, however it will be finished.
    var ending: Int { unsubscribed + needsYou + opened + running }
    /// Nothing is still running: the end page can say how it came out.
    var isSettled: Bool { running == 0 }

    init() {}

    /// What the staged decisions WOULD come to, for the tally and confirm.
    init(deck: PurgeDeck) {
        var perMonth = 0.0
        for (card, decision) in deck.plan {
            switch decision {
            case .unsubscribe:
                if card.unsubscribeIsOnTheirSite {
                    needsYou += 1
                } else {
                    unsubscribed += 1
                    perMonth += card.sender.signals.per_month ?? 0
                }
            case .block:
                blocked += 1
                perMonth += card.sender.signals.per_month ?? 0
            case .rule: rules += 1
            case .keep: kept += 1
            }
        }
        // Summed as fractions and rounded ONCE: five senders at 0.4 a month
        // are two fewer emails, not zero.
        fewerPerMonth = Int(perMonth.rounded())
    }

    /// What they HAVE come to, from where each sender stands now.
    init(statuses: [(card: PurgeCard, status: PurgeStatus)]) {
        var perMonth = 0.0
        for (card, status) in statuses {
            switch status {
            case .running: running += 1
            case .unsubscribed:
                unsubscribed += 1
                perMonth += card.sender.signals.per_month ?? 0
            case .needsYou: needsYou += 1
            case .opened: opened += 1
            case .blocked:
                blocked += 1
                perMonth += card.sender.signals.per_month ?? 0
            case .ruled: rules += 1
            case .kept: kept += 1
            case .failed: failed += 1
            }
        }
        fewerPerMonth = Int(perMonth.rounded())
    }
}

/// THE CONFIRM PAGE'S SHAPE: the staged decisions grouped by what will
/// happen, each sender NAMED under its group, so the one press that sends
/// everything is made looking at exactly who it is about.
enum PurgePlan {
    struct Entry: Hashable, Sendable, Identifiable {
        let card: PurgeCard
        /// The line under the name: the rule's sentence, or that this
        /// unsubscribe needs a click on the sender's site afterwards.
        var detail: String?
        var id: String { card.id }
    }

    struct Group: Hashable, Sendable, Identifiable {
        let kind: PurgeDecision.Kind
        var entries: [Entry]
        var id: PurgeDecision.Kind { kind }
    }

    /// In the order they matter, deck order within each, empty groups left
    /// out.
    static func groups(_ deck: PurgeDeck) -> [Group] {
        var by: [PurgeDecision.Kind: [Entry]] = [:]
        for (card, decision) in deck.plan {
            let detail: String?
            switch decision {
            case .unsubscribe:
                detail = card.unsubscribeIsOnTheirSite ? PurgeCopy.siteDetail : nil
            case .rule(let want): detail = want
            case .block, .keep: detail = nil
            }
            by[decision.kind, default: []].append(Entry(card: card, detail: detail))
        }
        return PurgeDecision.Kind.allCases.compactMap { kind in
            by[kind].map { Group(kind: kind, entries: $0) }
        }
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
    /// The way out of a sender, as its button names it.
    static func title(_ decision: PurgeDecision) -> String {
        switch decision {
        case .unsubscribe: "Unsubscribe"
        case .block: "Block"
        case .rule: "Add this rule"
        case .keep: "Keep them"
        }
    }

    /// The line beside the ending: what it does, said BEFORE it is chosen.
    /// A block explains itself, because it is offered in place of the thing
    /// the person expected to see.
    static func explains(_ card: PurgeCard) -> String {
        switch card.ending {
        case .block:
            return "None of their emails carry an unsubscribe link, so the only way to stop them is to block them."
        default:
            return card.unsubscribeIsOnTheirSite
                ? "Their unsubscribe only works on their own site. You will get the link at the end."
                : "Passband asks them to stop and tells you how it went."
        }
    }

    static let keepCaption = "We got this one wrong. You will not be asked again for a while."
    /// Over the suggestions. NOT a sentence the suggestion completes: they
    /// are in the owner's voice and come in any grammatical shape ("only
    /// order confirmations", "mute the marketing"), so each is shown as the
    /// thing being chosen and what choosing does is said on its own line.
    static let rulesLead = "Or keep part of it with a rule:"
    static let rulesNote = "Passband follows the rule you pick for what they send from now on."
    static let rulesLoading = "Looking at what they send"

    /// Said aloud as a decision is staged. STAGED, in the words: nothing has
    /// been sent, and a listener cannot see the page that says so.
    static func staged(_ decision: PurgeDecision, name: String) -> String {
        switch decision {
        case .unsubscribe: "Will unsubscribe from \(name)."
        case .block: "Will block \(name)."
        case .rule(let want): "Will add a rule for \(name): \(want)"
        case .keep: "Keeping \(name)."
        }
    }

    /// The confirm page's group headings.
    static func heading(_ kind: PurgeDecision.Kind) -> String {
        switch kind {
        case .unsubscribe: "Unsubscribe from"
        case .block: "Block"
        case .rule: "New rules"
        case .keep: "Keeping"
        }
    }

    /// The one line under a group's heading, where the group needs one.
    static func note(_ kind: PurgeDecision.Kind) -> String? {
        switch kind {
        case .block: "No unsubscribe link in any of their emails."
        case .rule: "Passband follows each rule for what they send from now on."
        case .keep: "Nothing changes. You will not be asked about these again for a while."
        case .unsubscribe: nil
        }
    }

    /// On a named sender in the Unsubscribe group whose unsubscribe the app
    /// cannot do by itself.
    static let siteDetail = "Needs a click on their site afterwards"

    /// "about 60 fewer emails a month". nil at zero.
    static func fewer(_ perMonth: Int) -> String? {
        guard perMonth > 0 else { return nil }
        return perMonth == 1 ? "about 1 fewer email a month" : "about \(perMonth) fewer emails a month"
    }

    /// The end page's headline, from where things stand now.
    static func headline(_ summary: PurgeSummary) -> String {
        if !summary.isSettled { return "Working through them." }
        if summary.needsYou > 0 {
            return summary.needsYou == 1 ? "1 needs you to finish." : "\(summary.needsYou) need you to finish."
        }
        if summary.fewerPerMonth > 0 {
            return summary.fewerPerMonth == 1
                ? "About 1 fewer email a month." : "About \(summary.fewerPerMonth) fewer emails a month."
        }
        if summary.rules > 0 { return summary.rules == 1 ? "1 new rule." : "\(summary.rules) new rules." }
        return "All done."
    }

    /// Over the list of senders waiting on the person.
    static let needsYouTitle = "These need you"
    static let needsYouLead =
        "Their unsubscribe has to be finished on their own site. Open each one and follow their page."
    /// What leaving does to the rows still waiting, said beside the button.
    static let laterNote =
        "Later keeps any the app already asked in the unsubscribe tray. The rest come back in the next purge."

    /// One sender's line on the end page.
    static func status(_ status: PurgeStatus) -> String {
        switch status {
        case .running: "Unsubscribing"
        case .unsubscribed: "Unsubscribed"
        case .needsYou(let reason): needsYou(reason)
        case .opened: "Opened their page"
        case .blocked: "Blocked"
        case .ruled(let want): "Rule: \(want)"
        case .kept: "Kept"
        case .failed: "Did not go through"
        }
    }

    /// Why a sender is waiting on the person, keyed by the daemon's
    /// `status_reason`. The same words the unsubscribe tray uses, so the two
    /// never describe one job differently, plus one for a sender that could
    /// only ever be finished on its own site.
    static func needsYou(_ reason: String?) -> String {
        switch reason {
        case PurgeStatus.siteOnly: "Their unsubscribe only works on their site."
        case "choose_topics": "Their page asks which emails to stop."
        case "needs_browser": "Their page only works in a browser."
        case "link_error": "Their link didn't work from here."
        default: "We couldn't confirm it went through."
        }
    }

    /// Decisions that did not go through. nil when all did.
    static func failedNote(_ failed: Int) -> String? {
        guard failed > 0 else { return nil }
        return failed == 1
            ? "1 did not go through. Nothing changed for that sender."
            : "\(failed) did not go through. Nothing changed for those senders."
    }

    /// A deck of nothing but keeps, confirmed: a toast, no end page.
    static func keptOnly(_ kept: Int) -> String {
        kept == 1 ? "Kept 1 sender." : "Kept \(kept) senders."
    }

    /// The pill was pressed and the list came back too short to offer.
    static let tooFew = "Nothing stands out to purge right now."
}

/// One of a sender's emails as the purge page lists it: enough to recognise,
/// nothing that opens.
struct PurgeEmail: Identifiable, Hashable, Sendable {
    let id: Int
    /// The summary line, already cleaned for display.
    let line: String
    /// RFC 3339, when the row carries one.
    let arrivedAt: String?
}
