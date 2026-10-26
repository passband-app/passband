// What a piece of auth mail ASKS of the owner, and nothing that draws.
//
// The rules of docs/AUTH-MAIL-HANDLING.md ("Clients"): which kinds put a
// question card up, for how long, which alerts are explained away by the code
// that arrived with them, which ones collapse into a "new device?" burst, and
// how a verdict kept in the old device-local ledger moves to the server. Pure
// Foundation and pure values, so the Auth page, the rail badge, the phone and
// the tests all run the one copy. `AuthDecisions` (Lib/AuthCode.swift) is the
// stateful half: it supplies the verdict these functions are handed.

import Foundation

/// The owner's ruling on one piece of auth mail. The raw values are the WIRE
/// spelling (`PUT /client/sealed/{id}/verdict`).
enum AuthVerdict: String, Sendable, Hashable {
    case mine
    case notMine = "not_mine"

    /// A value read out of the old device-local ledger, which spelled the second
    /// case "not-mine". Both spellings are read; only the wire one is written.
    init?(stored: String) {
        switch stored {
        case "mine": self = .mine
        case "not_mine", "not-mine": self = .notMine
        default: return nil
        }
    }

    /// The spelling the device-local ledger writes: the old one, so a build
    /// from before server verdicts still reads a ledger this one wrote.
    var stored: String {
        switch self {
        case .mine: "mine"
        case .notMine: "not-mine"
        }
    }
}

extension SealedMeta {
    var isCritical: Bool { critical ?? false }

    /// The server's ruling, when it sent one this build can read.
    var serverVerdict: AuthVerdict? { verdict.flatMap(AuthVerdict.init(rawValue:)) }

    /// Whether the daemon that served this row keeps verdicts itself. The
    /// `critical` key is the tell: a daemon older than the verdict route never
    /// sends it, and a newer one always does (true or false).
    var speaksVerdicts: Bool { critical != nil }
}

enum AuthPolicy {
    /// Kinds that put a question to the owner. Codes, verifications and
    /// receipts never do: each is proof of something the owner just did.
    static let questionKinds: Set<SealedKind> = [
        .loginAlert, .accountChange, .appGrant, .securityAlert, .passwordReset, .magicLink,
    ]

    /// The kinds a burst gathers: sign-ins ONLY. An account change, a grant or
    /// a security alert is what an attacker leaves behind to stay in, and an
    /// "All me" over a pile of sign-ins must never wave one of those through.
    static let burstKinds: Set<SealedKind> = [.loginAlert]

    /// How long an unruled question stays open. A sign-in from three weeks ago
    /// is history, not a decision, and counting it would pin the rail badge on
    /// a number nobody can work down.
    static let openWindow: TimeInterval = 7 * 24 * 3600
    /// Changes and grants are what an attacker does to STAY in, so they wait
    /// longer for an answer.
    static let longWindow: TimeInterval = 14 * 24 * 3600
    /// Bursts: this many distinct services inside this span.
    static let burstSpan: TimeInterval = 72 * 3600
    static let burstMinServices = 3

    /// Does this message ask a question at all (before windows and verdicts)?
    /// A critical change always does, whatever kind the message shows: a
    /// message holding both a code and a takeover-class change wears the code's
    /// kind, and the server still holds it in FYE until it is ruled on.
    static func asksQuestion(_ m: SealedMeta) -> Bool {
        if m.isCritical { return true }
        guard let kind = m.kind else { return false }
        return questionKinds.contains(kind)
    }

    /// An alert vouched for by a code, link or receipt from the same service.
    /// Never true for a critical change, whatever the row says.
    static func isExplained(_ m: SealedMeta) -> Bool {
        !m.isCritical && m.explained_by != nil
    }

    /// How long the question stays open; nil = until it is answered.
    static func window(_ m: SealedMeta) -> TimeInterval? {
        if m.isCritical { return nil }
        switch m.kind {
        case .accountChange, .appGrant, .securityAlert: return longWindow
        default: return openWindow
        }
    }

    /// THE definition of an open question, shared by the Auth page's rail, the
    /// badge on the rail's Auth button and the phone, so none of them can
    /// disagree. `verdict` is whatever `AuthDecisions` holds for the message.
    static func isOpen(_ m: SealedMeta, verdict: AuthVerdict?, now: Date = Date()) -> Bool {
        guard asksQuestion(m), !isExplained(m), verdict == nil else { return false }
        guard let window = window(m) else { return true }
        guard let received = Fmt.date(m.received_at) else { return false }
        return now.timeIntervalSince(received) <= window
    }

    // MARK: - services

    /// Second-level labels that sit under a two-letter country code as a
    /// registry, not a service (`co.uk`, `com.au`): the service is one label
    /// further in. A heuristic, enough for counting bursts; the daemon's
    /// pairing uses the public suffix list.
    /// Mailbox providers, not services: a sign-in alert from a person's own
    /// address at one of these names no service at all.
    static let freemail: Set<String> = [
        "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com",
        "yahoo.com", "icloud.com", "me.com", "mac.com", "aol.com", "proton.me",
        "protonmail.com", "gmx.com", "fastmail.com",
    ]

    static let countryRegistries: Set<String> = [
        "co", "com", "net", "org", "gov", "ac", "edu", "ne", "or",
    ]

    /// The registrable domain of a sender, as the daemon's pairing reads it:
    /// the last two labels of the address's domain, lowercased, or three under
    /// a country registry (`amazon.co.uk`). nil for anything that is not an
    /// address, for Apple's private relay (every service at once), and for a
    /// mailbox provider (`freemail`).
    static func service(_ sender: String) -> String? {
        var addr = sender
        if let open = addr.lastIndex(of: "<"), let close = addr.lastIndex(of: ">"), open < close {
            addr = String(addr[addr.index(after: open)..<close])
        }
        guard let at = addr.lastIndex(of: "@") else { return nil }
        let labels = addr[addr.index(after: at)...]
            .trimmingCharacters(in: .whitespaces)
            .lowercased()
            .split(separator: ".")
            .filter { !$0.isEmpty }
            .map(String.init)
        guard labels.count >= 2 else { return nil }
        let domain = labels.joined(separator: ".")
        if domain == "privaterelay.appleid.com" || domain.hasSuffix(".privaterelay.appleid.com") {
            return nil
        }
        let take =
            labels.count >= 3 && labels[labels.count - 1].count == 2
                && countryRegistries.contains(labels[labels.count - 2])
            ? 3 : 2
        let registrable = labels.suffix(take).joined(separator: ".")
        return freemail.contains(registrable) ? nil : registrable
    }

    // MARK: - cards

    /// Several open sign-in alerts from different services close together:
    /// what a new phone or laptop looks like from the inbox.
    struct Burst: Sendable, Hashable, Identifiable {
        /// Newest first.
        var items: [SealedMeta]
        /// Distinct services among `items`.
        var services: Int
        /// A view identity only. "Review each" is NOT remembered by this id,
        /// which changes as members are answered or age out; it is remembered
        /// by member ids (see `cards(_:expanded:)`).
        var id: String { "burst-\(items.map(\.id).min() ?? 0)" }
        var memberIds: Set<Int> { Set(items.map(\.id)) }
        var newest: Date? { items.compactMap { Fmt.date($0.received_at) }.max() }
        var allSignIns: Bool { items.allSatisfy { $0.kind == .loginAlert } }
    }

    enum Card: Sendable, Hashable, Identifiable {
        case single(SealedMeta)
        case burst(Burst)

        var id: String {
            switch self {
            case .single(let m): "m-\(m.id)"
            case .burst(let b): b.id
            }
        }
    }

    /// Group OPEN questions into bursts. Critical changes never group: each
    /// one is its own decision. Greedy over arrival order: a run of eligible
    /// alerts inside `burstSpan` of its first one becomes a burst when it
    /// spans at least `burstMinServices` services; otherwise the window slides
    /// on by one alert.
    static func bursts(_ open: [SealedMeta]) -> (bursts: [Burst], rest: [SealedMeta]) {
        let dated: [(m: SealedMeta, at: Date)] = open.compactMap { m in
            // No service, no burst: a relay or a mailbox provider could be any
            // number of services, so it neither counts toward the distinct
            // services nor joins one by itself.
            guard !m.isCritical, let kind = m.kind, burstKinds.contains(kind),
                service(m.sender) != nil,
                let at = Fmt.date(m.received_at)
            else { return nil }
            return (m, at)
        }
        .sorted { $0.at != $1.at ? $0.at < $1.at : $0.m.id < $1.m.id }

        var out: [Burst] = []
        var grouped = Set<Int>()
        var i = 0
        while i < dated.count {
            var j = i
            while j + 1 < dated.count, dated[j + 1].at.timeIntervalSince(dated[i].at) <= burstSpan {
                j += 1
            }
            let run = dated[i...j].map(\.m)
            let services = Set(run.compactMap { service($0.sender) })
            if services.count >= burstMinServices {
                out.append(Burst(items: run.reversed(), services: services.count))
                grouped.formUnion(run.map(\.id))
                i = j + 1
            } else {
                i += 1
            }
        }
        return (out, open.filter { !grouped.contains($0.id) })
    }

    /// The rail's cards, in order: critical first, then newest first.
    /// `expanded` holds the MEMBER ids of every burst the owner asked to
    /// "Review each"; a burst holding any of them is laid out as its members.
    /// Keyed by members rather than by the burst, so the burst stays open as
    /// its members are answered (it shrinks), age out, or a newer sign-in
    /// joins it (it grows).
    static func cards(_ open: [SealedMeta], expanded: Set<Int> = []) -> [Card] {
        let (bursts, rest) = bursts(open)
        var singles = rest
        var cards: [Card] = []
        for b in bursts {
            if !b.memberIds.isDisjoint(with: expanded) {
                singles += b.items
            } else {
                cards.append(.burst(b))
            }
        }
        cards += singles.map(Card.single)
        func critical(_ c: Card) -> Bool {
            if case .single(let m) = c { return m.isCritical }
            return false
        }
        func newest(_ c: Card) -> Date {
            switch c {
            case .single(let m): Fmt.date(m.received_at) ?? .distantPast
            case .burst(let b): b.newest ?? .distantPast
            }
        }
        return cards.sorted { a, b in
            let ca = critical(a)
            let cb = critical(b)
            if ca != cb { return ca }
            let na = newest(a)
            let nb = newest(b)
            return na != nb ? na > nb : a.id < b.id
        }
    }

    // MARK: - copy

    /// The two buttons on a question card: (yes, no).
    static func answers(_ m: SealedMeta) -> (mine: String, notMine: String) {
        if m.isCritical { return ("That was me", "Not me") }
        switch m.kind {
        case .passwordReset, .magicLink: return ("I asked", "I did not")
        case .appGrant: return ("I allowed it", "I didn't")
        default: return ("That was me", "Not me")
        }
    }

    /// The burst card's line under "New device?".
    static func burstLine(_ b: Burst) -> String {
        "\(b.services) services saw a new sign-in"
    }

    /// The quiet note on an explained row. `explainer` is the message that
    /// explains it, when it is still listed, and `service` the name to call
    /// that sender by.
    ///
    /// Worded to be true whichever explained it (a code the owner revealed, a
    /// sign-in link, or Google's receipt naming the service), and whether or
    /// not the explainer is still listed (it usually is not: done mail leaves
    /// the sealed list).
    static func explainedNote(explainer: SealedMeta?, service: String?) -> String {
        if explainer != nil, let service, !service.isEmpty {
            return "explained by your recent \(service) sign-in"
        }
        return "explained by a sign-in you just made"
    }

    // MARK: - asking from the reveal

    /// The question a reveal asks, when the message asks one at all.
    static func questionPrompt(_ m: SealedMeta) -> String {
        if m.isCritical { return "Was this you?" }
        switch m.kind {
        case .passwordReset, .magicLink: return "Did you ask for this?"
        case .appGrant: return "Did you allow this?"
        default: return "Was this you?"
        }
    }

    /// The answer as given, for the line that offers Undo.
    static func answeredLine(_ m: SealedMeta, _ verdict: AuthVerdict) -> String {
        let a = answers(m)
        return "You said: \(verdict == .mine ? a.mine : a.notMine)"
    }

    /// Whether a reveal (or a row) offers the question or its Undo: the
    /// message asks one and nothing explained it. Ignores the window on
    /// purpose: an old alert opened on purpose can still be answered.
    static func offersAnswer(_ m: SealedMeta) -> Bool {
        asksQuestion(m) && !isExplained(m)
    }

    /// The meta a notification tap reveals with, from what the payload
    /// carries. The panel fills in subject and kind from the reveal itself.
    /// `critical` stays nil: this says nothing about what the daemon keeps.
    static func revealMeta(messageId: Int, threadId: String, sender: String) -> SealedMeta {
        SealedMeta(
            id: messageId, thread_id: threadId, sender: sender, subject: "", kind: nil,
            received_at: "")
    }

    // MARK: - ledger migration

    /// What to do with the old device-local ledger once the daemon keeps
    /// verdicts: upload the rulings on listed messages the server has none for
    /// AND that would still be an open question without the ruling, and drop
    /// every other entry. Dropped: the server already has an answer; the
    /// message is no longer listed; it is past its window (a months-old "Not
    /// me" uploaded now would land in For your eyes on the daemon roll); or it
    /// is a critical change ruled "mine" on the old build, which asks again
    /// rather than quietly taking a takeover-class change out of FYE.
    struct Migration: Sendable, Equatable {
        /// Sorted by id, so the order is deterministic.
        var upload: [Upload]
        var drop: [Int]

        struct Upload: Sendable, Equatable {
            var id: Int
            var verdict: AuthVerdict
        }
    }

    /// nil when there is nothing to migrate yet: an empty ledger, or a daemon
    /// that does not keep verdicts (then the ledger IS the store).
    static func migration(
        local: [Int: AuthVerdict], rows: [SealedMeta], now: Date = Date()
    ) -> Migration? {
        guard !local.isEmpty, rows.contains(where: \.speaksVerdicts) else { return nil }
        let byId = Dictionary(rows.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        var upload: [Migration.Upload] = []
        var drop: [Int] = []
        for (id, verdict) in local.sorted(by: { $0.key < $1.key }) {
            if let row = byId[id], row.speaksVerdicts, row.serverVerdict == nil,
                isOpen(row, verdict: nil, now: now),
                !(row.isCritical && verdict == .mine)
            {
                upload.append(.init(id: id, verdict: verdict))
            } else {
                drop.append(id)
            }
        }
        return Migration(upload: upload, drop: drop)
    }
}

// MARK: - feed rows and events

extension AuthPolicy {
    /// The one kind a client shows, picked by priority across a decision's
    /// raw `auth.kinds` exactly as the daemon picks a sealed row's kind. The
    /// decision says `sign_in_link`; the sealed wire calls it `magic_link`.
    static func pickKind(_ raw: [String]) -> SealedKind? {
        let order = [
            "otp", "sign_in_link", "password_reset", "verification", "account_change",
            "app_grant", "login_alert", "security_alert", "signin_receipt",
        ]
        let known = raw.compactMap { order.firstIndex(of: $0) }
        if let best = known.min() {
            let name = order[best]
            return SealedKind(rawValue: name == "sign_in_link" ? "magic_link" : name)
        }
        return raw.first.map { SealedKind(rawValue: $0) }
    }

    /// The auth metadata a For-your-eyes row is drawn and revealed with, or
    /// nil when the row is not auth mail. Either signal makes it auth: the
    /// sealed row when listed (preferred, it carries the verdict), else the
    /// row's own decision, so a blank or stale sealed list (the board snapshot
    /// an account switch paints) never turns it into a normal row. Whether the
    /// daemon keeps verdicts is never read off a meta built here
    /// (`AuthDecisions.daemonKeepsVerdicts` reads the sealed list only).
    static func authMeta(for update: AttentionUpdate, sealed: SealedMeta?) -> SealedMeta? {
        if let sealed { return sealed }
        guard update.isAuthRow else { return nil }
        return SealedMeta(
            id: update.id, thread_id: update.thread_id, sender: update.sender,
            subject: update.subject ?? "", kind: pickKind(update.auth_kinds ?? []),
            received_at: update.received_at ?? update.surfaced_at ?? "",
            critical: (update.auth_critical ?? false)
                && (update.auth_kinds ?? []).contains("account_change"))
    }

    /// The same for a notification event marked `is_auth`, which carries no
    /// subject (its `one_line` is a fixed phrase, never body text).
    static func authMeta(for event: Event) -> SealedMeta {
        SealedMeta(
            id: event.message_id, thread_id: event.thread_id, sender: event.sender,
            subject: event.one_line, kind: event.sealed_kind, received_at: event.created_at)
    }

    /// Threads holding auth mail by either signal: nothing warms, prefetches
    /// or caches them (`ThreadPrefetch.refuses`).
    static func authThreadIds(standing: [AttentionUpdate], sealed: [SealedMeta]) -> Set<String> {
        var ids = Set(sealed.map(\.thread_id))
        for u in standing where u.isAuthRow { ids.insert(u.thread_id) }
        ids.remove("")
        return ids
    }
}

// MARK: - verdict writes in flight

extension AuthPolicy {
    /// A verdict written (or being written) but not yet seen in a sealed list.
    struct PendingWrite: Sendable, Equatable {
        /// nil = a clear.
        var verdict: AuthVerdict?
        /// The newest list fetch that had STARTED when the PUT came back; only
        /// a list whose fetch started after that can confirm the write. nil
        /// while the PUT is still in flight.
        var acceptedAfterFetch: Int?
    }

    /// Settle pending writes against a list whose fetch was number `fetch`.
    /// A write is dropped (the list now speaks for it) once the PUT has come
    /// back AND a list fetched after that agrees with it or no longer lists
    /// the message. A list that was already in flight when the PUT landed can
    /// still carry the old answer, and must not reopen the card.
    static func settle(
        _ pending: [Int: PendingWrite], rows: [SealedMeta], fetch: Int
    ) -> [Int: PendingWrite] {
        let byId = Dictionary(rows.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        return pending.filter { id, write in
            guard let after = write.acceptedAfterFetch, fetch > after else { return true }
            guard let row = byId[id] else { return false }
            return row.serverVerdict != write.verdict
        }
    }

    /// Whether a failed verdict PUT means "this daemon has no verdict route,
    /// keep it on the device" (quietly), rather than an error to roll back.
    /// A 404 or 405 for a message still listed as auth mail can only be a
    /// missing route: an older daemon, including one rolled back mid-session.
    static func fallsBackToLedger(status: Int, stillListed: Bool) -> Bool {
        (status == 404 || status == 405) && stillListed
    }
}
