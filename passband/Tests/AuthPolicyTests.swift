// What a piece of auth mail asks of the owner (docs/AUTH-MAIL-HANDLING.md,
// "Clients"): the sealed row decoded from both daemon versions, which kinds
// put a question up and for how long, which alerts the code beside them
// explains, how new-device alerts collapse into one burst, and how the old
// device-local ledger moves up to a daemon that keeps verdicts.
//
// Every one of these is only ever wrong quietly: a question that never opens
// is an account takeover nobody was asked about, and one that never closes is
// a badge nobody can work down.

import Foundation

@main
@MainActor
struct AuthPolicyTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        anOldDaemonsRowDecodesWithoutTheNewKeys()
        aNewDaemonsRowCarriesCriticalVerdictAndExplainer()
        anUnreadableVerdictIsNoVerdictNotAFailedList()
        newKindsDecodeAndUnknownOnesKeepTheirString()
        newKindsHaveLabelsWithoutDashes()
        clearingAVerdictPutsAnExplicitNull()
        questionsOpenByKindAndWindow()
        criticalStaysOpenAndIsNeverExplained()
        aVerdictClosesTheQuestion()
        threeServicesInThreeDaysAreABurst()
        twoServicesAreNot()
        criticalNeverGroups()
        aBurstMustFitInsideItsWindow()
        cardsPutCriticalFirstAndExpandOnRequest()
        servicesAreRegistrableDomains()
        answersAndNotesSayTheRightThing()
        theLedgerMigratesOnlyToADaemonThatKeepsVerdicts()
        theLedgerReadsBothSpellings()
        reviewEachSurvivesAShrinkingBurst()
        migrationSkipsClosedAndCriticalMine()
        aStaleListCannotSettleAWrite()
        aMissingRouteFallsBackQuietly()
        aDisagreeingListResendsOnceThenShowsTheServer()
        feedRowsAreAuthByTheirOwnDecision()
        eventsBecomeRevealableMeta()
        sendersWithNoServiceNeverMakeABurst()
        theRevealAsksAndOffersUndo()
        clearAllSparesCriticalChanges()

        print("\(checks - failures)/\(checks) checks passed")
        if failures > 0 { exit(1) }
    }

    // MARK: - fixtures

    static let now = Fmt.date("2026-10-26T12:00:00Z")!

    static func ago(_ seconds: TimeInterval) -> String {
        ISO8601DateFormatter().string(from: now.addingTimeInterval(-seconds))
    }

    static let hour: TimeInterval = 3600
    static let day: TimeInterval = 24 * 3600

    static func meta(
        _ id: Int, _ kind: SealedKind?, from sender: String = "alerts@github.com",
        age: TimeInterval = 3600, critical: Bool? = false, verdict: String? = nil,
        explainedBy: Int? = nil
    ) -> SealedMeta {
        SealedMeta(
            id: id, thread_id: "t\(id)", sender: sender, subject: "subject \(id)", kind: kind,
            received_at: ago(age), critical: critical, verdict: verdict, explained_by: explainedBy)
    }

    static func decode(_ json: String) -> [SealedMeta]? {
        try? JSONDecoder().decode([SealedMeta].self, from: Data(json.utf8))
    }

    // MARK: - decoding

    static func anOldDaemonsRowDecodesWithoutTheNewKeys() {
        let rows = decode("""
            [{"id":1,"thread_id":"a","sender":"no-reply@github.com","subject":"New sign-in","kind":"login_alert","received_at":"2026-10-26T10:00:00Z"}]
            """)
        expect(rows?.count == 1, "the old row shape still decodes")
        let m = rows?.first
        expect(m?.critical == nil && m?.verdict == nil && m?.explained_by == nil, "absent keys are nil")
        expect(m?.speaksVerdicts == false, "an old daemon does not keep verdicts")
        expect(m?.isCritical == false, "absent critical is not critical")
    }

    static func aNewDaemonsRowCarriesCriticalVerdictAndExplainer() {
        let rows = decode("""
            [{"id":2,"thread_id":"b","sender":"no-reply@accounts.google.com","subject":"Recovery phone changed","kind":"account_change","received_at":"2026-10-26T10:00:00Z","critical":true,"verdict":"not_mine","explained_by":null},
             {"id":3,"thread_id":"c","sender":"noreply@github.com","subject":"New sign-in","kind":"login_alert","received_at":"2026-10-26T10:00:00Z","critical":false,"verdict":null,"explained_by":9}]
            """)
        expect(rows?.count == 2, "the new row shape decodes")
        expect(rows?[0].isCritical == true && rows?[0].speaksVerdicts == true, "critical row")
        expect(rows?[0].serverVerdict == .notMine, "not_mine reads as notMine")
        expect(rows?[1].speaksVerdicts == true, "critical:false still says the daemon keeps verdicts")
        expect(rows?[1].serverVerdict == nil && rows?[1].explained_by == 9, "explainer id survives")
    }

    static func anUnreadableVerdictIsNoVerdictNotAFailedList() {
        let rows = decode("""
            [{"id":4,"thread_id":"d","sender":"a@b.com","subject":"s","kind":"login_alert","received_at":"2026-10-26T10:00:00Z","critical":false,"verdict":"maybe"}]
            """)
        expect(rows?.count == 1, "a verdict this build has never heard of does not fail the list")
        expect(rows?.first?.serverVerdict == nil, "and reads as no verdict")
    }

    static func newKindsDecodeAndUnknownOnesKeepTheirString() {
        expect(SealedKind(rawValue: "account_change") == .accountChange, "account_change")
        expect(SealedKind(rawValue: "app_grant") == .appGrant, "app_grant")
        expect(SealedKind(rawValue: "signin_receipt") == .signinReceipt, "signin_receipt")
        for k in [SealedKind.accountChange, .appGrant, .signinReceipt] {
            expect(SealedKind(rawValue: k.rawValue) == k, "\(k.rawValue) round-trips")
        }
        let rows = decode("""
            [{"id":5,"thread_id":"e","sender":"a@b.com","subject":"s","kind":"wallet_card","received_at":"2026-10-26T10:00:00Z"}]
            """)
        expect(rows?.first?.kind == .unknown("wallet_card"), "an unknown kind keeps its raw string")
        expect(rows?.first?.kind?.rawValue == "wallet_card", "and re-encodes verbatim")
        expect(AuthCopy.label(.unknown("wallet_card")) == "Auth message", "and is labelled generically")
        expect(!AuthPolicy.isOpen(meta(5, .unknown("wallet_card")), verdict: nil, now: now), "an unknown kind asks nothing")
    }

    static func newKindsHaveLabelsWithoutDashes() {
        expect(AuthCopy.label(.accountChange) == "Account change", "account change label")
        expect(AuthCopy.label(.appGrant) == "App access", "app grant label")
        expect(AuthCopy.label(.signinReceipt) == "Sign-in receipt", "receipt label")
        let b = AuthPolicy.Burst(items: [], services: 4)
        let copy = [
            AuthCopy.label(.accountChange), AuthCopy.label(.appGrant), AuthCopy.label(.signinReceipt),
            AuthPolicy.burstLine(b),
            AuthPolicy.explainedNote(explainer: nil, service: nil),
            AuthPolicy.explainedNote(explainer: meta(1, .otp), service: "GitHub"),
            AuthPolicy.questionPrompt(meta(1, .passwordReset)), AuthPolicy.questionPrompt(meta(1, .appGrant)),
            AuthPolicy.questionPrompt(meta(1, .loginAlert)),
            AuthPolicy.answeredLine(meta(1, .appGrant), .notMine),
        ] + [SealedKind.loginAlert, .appGrant, .passwordReset].flatMap { k in
            let a = AuthPolicy.answers(meta(1, k))
            return [a.mine, a.notMine]
        }
        for s in copy {
            expect(!s.contains("\u{2014}") && !s.contains("\u{2013}"), "no em or en dash: \(s)")
        }
    }

    static func clearingAVerdictPutsAnExplicitNull() {
        let clear = try? JSONEncoder().encode(SealedVerdictBody(verdict: nil))
        expect(clear.map { String(decoding: $0, as: UTF8.self) } == #"{"verdict":null}"#, "null is on the wire")
        let mine = try? JSONEncoder().encode(SealedVerdictBody(verdict: AuthVerdict.notMine.rawValue))
        expect(mine.map { String(decoding: $0, as: UTF8.self) } == #"{"verdict":"not_mine"}"#, "not_mine is the wire spelling")
    }

    // MARK: - isOpen

    static func open(_ m: SealedMeta, _ verdict: AuthVerdict? = nil) -> Bool {
        AuthPolicy.isOpen(m, verdict: verdict, now: now)
    }

    static func questionsOpenByKindAndWindow() {
        expect(open(meta(1, .loginAlert, age: 3 * day)), "a 3-day sign-in alert is open")
        expect(!open(meta(1, .loginAlert, age: 8 * day)), "an 8-day sign-in alert is history")
        expect(!open(meta(1, .passwordReset, age: 8 * day)), "resets keep the 7-day window")
        expect(open(meta(1, .magicLink, age: 6 * day)), "sign-in links ask")
        for k in [SealedKind.accountChange, .appGrant, .securityAlert] {
            expect(open(meta(1, k, age: 10 * day)), "\(k.rawValue) waits 14 days")
            expect(!open(meta(1, k, age: 15 * day)), "\(k.rawValue) closes after 14")
        }
        for k in [SealedKind.otp, .verification, .signinReceipt] {
            expect(!open(meta(1, k, age: hour)), "\(k.rawValue) never asks")
        }
        expect(!open(meta(1, nil, age: hour)), "no kind asks nothing")
        expect(!open(meta(1, .loginAlert, age: hour, explainedBy: 2)), "an explained alert asks nothing")
        var bad = meta(1, .loginAlert)
        bad.received_at = "yesterday-ish"
        expect(!open(bad), "an unreadable stamp is not an open question")
    }

    static func criticalStaysOpenAndIsNeverExplained() {
        expect(open(meta(1, .accountChange, age: 60 * day, critical: true)), "critical has no window")
        expect(open(meta(1, .accountChange, critical: true, explainedBy: 2)), "critical is never explained")
        expect(open(meta(1, .otp, critical: true)), "critical asks even under a code's kind")
        expect(!AuthPolicy.isExplained(meta(1, .accountChange, critical: true, explainedBy: 2)), "isExplained agrees")
    }

    static func aVerdictClosesTheQuestion() {
        expect(!open(meta(1, .loginAlert), .mine), "mine closes")
        expect(!open(meta(1, .loginAlert), .notMine), "not mine closes the card too (it moves to FYE)")
        expect(!open(meta(1, .accountChange, critical: true), .mine), "even a critical one")
    }

    // MARK: - bursts

    static func threeServicesInThreeDaysAreABurst() {
        let open = [
            meta(1, .loginAlert, from: "no-reply@accounts.google.com", age: 60 * hour),
            meta(2, .loginAlert, from: "noreply@github.com", age: 30 * hour),
            meta(3, .loginAlert, from: "security@mail.slack.com", age: 1 * hour),
            meta(4, .passwordReset, from: "noreply@stripe.com", age: 2 * hour),
        ]
        let (bursts, rest) = AuthPolicy.bursts(open)
        expect(bursts.count == 1, "three services within 72h are one burst")
        expect(bursts.first?.services == 3, "counting services")
        expect(bursts.first?.items.map(\.id) == [3, 2, 1], "newest first")
        expect(bursts.first?.allSignIns == true, "all sign-ins")
        expect(rest.map(\.id) == [4], "a reset never joins a burst")
        expect(AuthPolicy.burstLine(bursts[0]) == "3 services saw a new sign-in", "the line")

        // Changes, grants and security alerts never make up a burst, however many.
        for k in [SealedKind.accountChange, .appGrant, .securityAlert] {
            let mixed = Array(open.prefix(2)) + [meta(5, k, from: "x@dropbox.com", age: 2 * hour)]
            expect(AuthPolicy.bursts(mixed).bursts.isEmpty, "\(k.rawValue) is not the third service")
            let only = [
                meta(6, k, from: "a@x.com", age: 3 * hour), meta(7, k, from: "b@y.com", age: 2 * hour),
                meta(8, k, from: "c@z.com", age: 1 * hour),
            ]
            expect(AuthPolicy.bursts(only).bursts.isEmpty, "three \(k.rawValue)s never group")
        }

        // A newer arrival from a fourth service joins, and the id holds.
        let grown = open + [meta(6, .loginAlert, from: "x@apple.com", age: 0.5 * hour)]
        let g = AuthPolicy.bursts(grown).bursts.first
        expect(g?.services == 4 && g?.id == bursts.first?.id, "a growing burst keeps its id")
    }

    static func twoServicesAreNot() {
        let open = [
            meta(1, .loginAlert, from: "a@google.com", age: 5 * hour),
            meta(2, .loginAlert, from: "b@google.com", age: 4 * hour),
            meta(3, .loginAlert, from: "c@github.com", age: 3 * hour),
        ]
        let (bursts, rest) = AuthPolicy.bursts(open)
        expect(bursts.isEmpty, "three alerts from two services are not a burst")
        expect(rest.count == 3, "and stay single")
    }

    static func criticalNeverGroups() {
        let open = [
            meta(1, .loginAlert, from: "a@google.com", age: 5 * hour),
            meta(2, .loginAlert, from: "c@github.com", age: 3 * hour),
            meta(3, .accountChange, from: "d@apple.com", age: 2 * hour, critical: true),
        ]
        let (bursts, rest) = AuthPolicy.bursts(open)
        expect(bursts.isEmpty, "a critical change does not make up the third service")
        expect(rest.count == 3, "everything stays single")
        let four = open + [meta(4, .loginAlert, from: "e@slack.com", age: 1 * hour)]
        let r = AuthPolicy.bursts(four)
        expect(r.bursts.count == 1 && !r.bursts[0].items.contains { $0.isCritical }, "critical stays out of a burst")
        expect(r.rest.map(\.id) == [3], "and keeps its own card")
    }

    static func aBurstMustFitInsideItsWindow() {
        let open = [
            meta(1, .loginAlert, from: "a@google.com", age: 6 * day),
            meta(2, .loginAlert, from: "b@github.com", age: 3 * day),
            meta(3, .loginAlert, from: "c@slack.com", age: 1 * hour),
        ]
        expect(AuthPolicy.bursts(open).bursts.isEmpty, "three services across six days are not a burst")
    }

    static func cardsPutCriticalFirstAndExpandOnRequest() {
        let open = [
            meta(1, .loginAlert, from: "a@google.com", age: 5 * hour),
            meta(2, .loginAlert, from: "b@github.com", age: 4 * hour),
            meta(3, .loginAlert, from: "c@slack.com", age: 3 * hour),
            meta(4, .appGrant, from: "d@google.com", age: 1 * hour),
            meta(5, .accountChange, from: "e@apple.com", age: 9 * day, critical: true),
        ]
        let cards = AuthPolicy.cards(open)
        expect(cards.count == 3, "critical + app grant + one burst")
        expect(cards.first?.id == "m-5", "critical first, however old")
        expect(cards[1].id == "m-4", "then newest first")
        if case .burst = cards[2] {} else { expect(false, "the burst is the third card") }
        guard case .burst(let burst) = cards[2] else { return }
        let expanded = AuthPolicy.cards(open, expanded: burst.memberIds)
        expect(expanded.count == 5, "Review each lays the burst out as its members")
        expect(expanded.map(\.id) == ["m-5", "m-4", "m-3", "m-2", "m-1"], "in the same order")
    }

    static func servicesAreRegistrableDomains() {
        expect(AuthPolicy.service("no-reply@accounts.google.com") == "google.com", "last two labels")
        expect(AuthPolicy.service("Google <no-reply@accounts.google.com>") == "google.com", "a display form")
        expect(AuthPolicy.service("NoReply@GitHub.COM") == "github.com", "lowercased")
        expect(AuthPolicy.service("not an address") == nil, "not an address")
        expect(AuthPolicy.service("orders@amazon.co.uk") == "amazon.co.uk", "a country registry keeps three")
        expect(AuthPolicy.service("news@bbc.co.uk") != AuthPolicy.service("orders@amazon.co.uk"), "two co.uk services differ")
        expect(AuthPolicy.service("x@mail.example.com.au") == "example.com.au", "com.au")
        expect(AuthPolicy.service("x@example.io") == "example.io", "a plain two-letter TLD keeps two")
        expect(AuthPolicy.service("abc@privaterelay.appleid.com") == nil, "a relay is no service")
    }

    // MARK: - copy

    static func answersAndNotesSayTheRightThing() {
        expect(AuthPolicy.answers(meta(1, .passwordReset)) == ("I asked", "I did not"), "reset")
        expect(AuthPolicy.answers(meta(1, .magicLink)) == ("I asked", "I did not"), "sign-in link")
        expect(AuthPolicy.answers(meta(1, .appGrant)) == ("I allowed it", "I didn't"), "app grant")
        expect(AuthPolicy.answers(meta(1, .loginAlert)) == ("That was me", "Not me"), "sign-in")
        expect(AuthPolicy.answers(meta(1, .accountChange, critical: true)) == ("That was me", "Not me"), "critical")
        expect(
            AuthPolicy.explainedNote(explainer: meta(9, .otp), service: "GitHub")
                == "explained by your GitHub sign-in", "a code")
        expect(
            AuthPolicy.explainedNote(explainer: meta(9, .signinReceipt), service: "Google")
                == "explained by your Google sign-in", "a receipt")
        expect(
            AuthPolicy.explainedNote(explainer: meta(9, .magicLink), service: nil)
                == "explained by a sign-in of yours", "no name")
        expect(
            AuthPolicy.explainedNote(explainer: nil, service: "GitHub")
                == "explained by a sign-in of yours", "explainer no longer listed (the usual case)")
    }

    // MARK: - ledger migration

    static func theLedgerMigratesOnlyToADaemonThatKeepsVerdicts() {
        let local: [Int: AuthVerdict] = [1: .mine, 2: .notMine, 3: .mine, 4: .mine]
        let oldRows = [meta(1, .loginAlert, critical: nil), meta(2, .loginAlert, critical: nil)]
        expect(AuthPolicy.migration(local: local, rows: oldRows) == nil, "an old daemon: the ledger stays the store")
        expect(AuthPolicy.migration(local: [:], rows: [meta(1, .loginAlert)]) == nil, "nothing to move")

        let rows = [
            meta(1, .loginAlert),  // listed, unruled: upload
            meta(2, .accountChange, critical: true),  // listed, unruled: upload
            meta(3, .loginAlert, verdict: "not_mine"),  // the server already has one: drop
            // 4 is no longer listed: drop
        ]
        let plan = AuthPolicy.migration(local: local, rows: rows, now: now)
        expect(plan?.upload == [.init(id: 1, verdict: .mine), .init(id: 2, verdict: .notMine)], "uploads the unruled")
        expect(plan?.drop == [3, 4], "drops what the server has or no longer lists")
    }

    static func theLedgerReadsBothSpellings() {
        expect(AuthVerdict(stored: "not-mine") == .notMine, "the old ledger's spelling")
        expect(AuthVerdict(stored: "not_mine") == .notMine, "the wire spelling")
        expect(AuthVerdict(stored: "mine") == .mine, "mine")
        expect(AuthVerdict(stored: "nope") == nil, "garbage")
        expect(AuthVerdict.notMine.stored == "not-mine", "writes the spelling an older build reads")
        for v in [AuthVerdict.mine, .notMine] {
            expect(AuthVerdict(stored: v.stored) == v, "\(v) round-trips the ledger")
        }
    }

    // MARK: - fix round

    static func reviewEachSurvivesAShrinkingBurst() {
        var open = [
            meta(1, .loginAlert, from: "a@google.com", age: 6 * hour),
            meta(2, .loginAlert, from: "b@github.com", age: 5 * hour),
            meta(3, .loginAlert, from: "c@slack.com", age: 4 * hour),
            meta(4, .loginAlert, from: "d@apple.com", age: 3 * hour),
        ]
        guard case .burst(let b)? = AuthPolicy.cards(open).first else {
            expect(false, "four sign-ins are a burst")
            return
        }
        var expanded = b.memberIds  // "Review each"
        expect(AuthPolicy.cards(open, expanded: expanded).count == 4, "laid out as members")
        // Answer the oldest: still 3 services, the old burst id changes.
        open.removeFirst()
        expect(AuthPolicy.cards(open, expanded: expanded).count == 3, "stays expanded after answering the oldest")
        expect(AuthPolicy.cards(open).count == 1, "the same three collapse without the review")
        // A newer sign-in joins: still expanded, newcomer laid out too.
        open.append(meta(5, .loginAlert, from: "e@dropbox.com", age: 1 * hour))
        expect(AuthPolicy.cards(open, expanded: expanded).count == 4, "stays expanded as it grows")
        // Answer down to below a burst: plain singles either way.
        open = Array(open.suffix(2))
        expect(AuthPolicy.cards(open, expanded: expanded).count == 2, "below a burst, singles")
        expanded = []
        expect(AuthPolicy.cards(open).count == 2, "and without review too")
    }

    static func migrationSkipsClosedAndCriticalMine() {
        let local: [Int: AuthVerdict] = [1: .notMine, 2: .mine, 3: .notMine, 4: .mine]
        let rows = [
            meta(1, .loginAlert, age: 40 * day),  // past its window: drop
            meta(2, .accountChange, age: 3 * day, critical: true),  // critical "mine": drop, asks again
            meta(3, .accountChange, age: 3 * day, critical: true),  // critical "not mine": upload
            meta(4, .otp, age: hour),  // never a question: drop
        ]
        let plan = AuthPolicy.migration(local: local, rows: rows, now: now)
        expect(plan?.upload == [.init(id: 3, verdict: .notMine)], "only the still-open, non-mine-critical one")
        expect(plan?.drop == [1, 2, 4], "the rest are dropped")
    }

    static func aStaleListCannotSettleAWrite() {
        typealias W = AuthPolicy.PendingWrite
        func settle(_ p: [Int: W], _ rows: [SealedMeta], _ fetch: Int) -> AuthPolicy.Settled {
            AuthPolicy.settle(p, rows: rows, fetch: fetch)
        }
        let inFlight: [Int: W] = [1: W(verdict: .mine, acceptedAfterFetch: nil)]
        let agrees = [meta(1, .loginAlert, verdict: "mine")]
        let stale = [meta(1, .loginAlert, verdict: nil)]
        expect(settle(inFlight, agrees, 9) == .init(pending: inFlight, resend: []), "a PUT still in flight is never settled")
        let accepted: [Int: W] = [1: W(verdict: .mine, acceptedAfterFetch: 4)]
        expect(settle(accepted, agrees, 4) == .init(pending: accepted, resend: []), "a list already in flight cannot confirm")
        expect(settle(accepted, agrees, 5) == .init(pending: [:], resend: []), "a later list that agrees settles it")
        expect(settle(accepted, [], 5) == .init(pending: [:], resend: []), "a later list without the message settles it")
        let clear: [Int: W] = [1: W(verdict: nil, acceptedAfterFetch: 4)]
        expect(settle(clear, stale, 5).pending.isEmpty, "a clear settles on a null verdict")
    }

    /// The Undo race: the server ended on "mine" though the latest intent is
    /// a clear. Resolved deterministically: resend once, then the server wins.
    static func aDisagreeingListResendsOnceThenShowsTheServer() {
        typealias W = AuthPolicy.PendingWrite
        let undo: [Int: W] = [1: W(verdict: nil, acceptedAfterFetch: 4)]
        let serverMine = [meta(1, .loginAlert, verdict: "mine")]
        let first = AuthPolicy.settle(undo, rows: serverMine, fetch: 5)
        expect(first.resend == [1], "the latest intent is sent once more")
        expect(first.pending[1] == W(verdict: nil, acceptedAfterFetch: nil, resent: true), "and waits for its own PUT again")
        var afterResend = first.pending
        afterResend[1]?.acceptedAfterFetch = 6
        let second = AuthPolicy.settle(afterResend, rows: serverMine, fetch: 7)
        expect(second.resend.isEmpty && second.pending.isEmpty, "still disagreeing: dropped, the server's answer shows")
        let fixed = AuthPolicy.settle(afterResend, rows: [meta(1, .loginAlert, verdict: nil)], fetch: 7)
        expect(fixed.pending.isEmpty && fixed.resend.isEmpty, "agreeing after the resend: settled")
        expect(AuthPolicy.settle(first.pending, rows: serverMine, fetch: 9).pending == first.pending, "a resend in flight is never judged")
    }

    static func aMissingRouteFallsBackQuietly() {
        expect(AuthPolicy.fallsBackToLedger(status: 404, stillListed: true), "404 on a listed auth row: no route")
        expect(AuthPolicy.fallsBackToLedger(status: 405, stillListed: true), "405 too")
        expect(!AuthPolicy.fallsBackToLedger(status: 404, stillListed: false), "404 on a message no longer listed is a real refusal")
        expect(!AuthPolicy.fallsBackToLedger(status: 500, stillListed: true), "a server error rolls back")
    }

    static func feedRowsAreAuthByTheirOwnDecision() {
        expect(AuthPolicy.pickKind(["login_alert", "otp"]) == .otp, "priority, not order")
        expect(AuthPolicy.pickKind(["sign_in_link", "password_reset"]) == .magicLink, "sign_in_link is the sealed magic_link")
        expect(AuthPolicy.pickKind(["signin_receipt", "account_change"]) == .accountChange, "change before receipt")
        expect(AuthPolicy.pickKind(["brand_new"]) == .unknown("brand_new"), "unknown stays unknown")
        expect(AuthPolicy.pickKind([]) == nil, "none")

        let json = """
            {"message_id":42,"thread_id":"t42","from_addr":"no-reply@accounts.google.com","subject":"Recovery phone changed","received_at":"2026-10-26T10:00:00Z",
             "decision":{"kinds":["authentication_security"],"destinations":[],"summary":"s","reason":"r","auth":{"kinds":["account_change"],"evidence":[],"critical":true}},
             "attention":{"show_in_fye":true,"state":"needs_user","summary":"Google says your recovery phone changed.","factors":{"urgency":1,"action_need":1,"personal_relevance":1,"importance":1}},"score":1}
            """
        let item = try? JSONDecoder().decode(AgentFeedItem.self, from: Data(json.utf8))
        let row = item?.row
        expect(row?.isAuthRow == true, "decision.auth.kinds marks the row")
        let m = row.flatMap { AuthPolicy.authMeta(for: $0, sealed: nil) }
        expect(m?.id == 42 && m?.kind == .accountChange && m?.isCritical == true, "an empty sealed list still yields auth meta")
        let listed = meta(42, .accountChange, critical: true, verdict: "not_mine")
        expect(row.flatMap { AuthPolicy.authMeta(for: $0, sealed: listed) }?.serverVerdict == .notMine, "the sealed row wins when listed")
        expect(row.map { AuthPolicy.authThreadIds(standing: [$0], sealed: []) } == ["t42"], "its thread is refused for prefetch")

        let weird = json.replacingOccurrences(
            of: #""auth":{"kinds":["account_change"],"evidence":[],"critical":true}"#, with: #""auth":"???""#)
        let tolerant = try? JSONDecoder().decode(AgentFeedItem.self, from: Data(weird.utf8))
        expect(tolerant != nil && tolerant?.row.isAuthRow == false, "a malformed auth block does not fail the feed row")
        let plain = json.replacingOccurrences(
            of: #","auth":{"kinds":["account_change"],"evidence":[],"critical":true}"#, with: "")
        let p = try? JSONDecoder().decode(AgentFeedItem.self, from: Data(plain.utf8))
        expect(p != nil && p.map { AuthPolicy.authMeta(for: $0.row, sealed: nil) } == .some(nil), "no auth block, not auth")
    }

    static func eventsBecomeRevealableMeta() {
        let event = try? JSONDecoder().decode(Event.self, from: Data("""
            {"id":9,"message_id":77,"thread_id":"t77","kind":"surfaced","tier":"signal","importance":0,"sender":"no-reply@github.com","one_line":"Sign-in alert","created_at":"2026-10-26T10:00:00Z","is_auth":true}
            """.utf8))
        let m = event.map(AuthPolicy.authMeta(for:))
        expect(m?.id == 77 && m?.thread_id == "t77" && m?.subject == "Sign-in alert", "an auth event reveals message 77")
    }

    static func sendersWithNoServiceNeverMakeABurst() {
        let relays = [
            meta(1, .loginAlert, from: "a1@privaterelay.appleid.com", age: 3 * hour),
            meta(2, .loginAlert, from: "b2@privaterelay.appleid.com", age: 2 * hour),
            meta(3, .loginAlert, from: "c3@privaterelay.appleid.com", age: 1 * hour),
        ]
        expect(AuthPolicy.bursts(relays).bursts.isEmpty, "three relay senders are not three services")
        let mixed = [
            meta(1, .loginAlert, from: "a@google.com", age: 4 * hour),
            meta(2, .loginAlert, from: "b@github.com", age: 3 * hour),
            meta(3, .loginAlert, from: "someone@gmail.com", age: 2 * hour),
            meta(4, .loginAlert, from: "x9@privaterelay.appleid.com", age: 1 * hour),
        ]
        let r = AuthPolicy.bursts(mixed)
        expect(r.bursts.isEmpty, "freemail and relay never make up the third service")
        expect(r.rest.count == 4, "and nobody is swallowed")
        expect(AuthPolicy.service("someone@gmail.com") == nil, "a mailbox provider is no service")
        let real = mixed + [meta(5, .loginAlert, from: "c@slack.com", age: 0.5 * hour)]
        let b = AuthPolicy.bursts(real).bursts.first
        expect(b?.services == 3 && b?.memberIds == [1, 2, 5], "a burst of real services leaves the relay and freemail out")
    }

    static func theRevealAsksAndOffersUndo() {
        expect(AuthPolicy.offersAnswer(meta(1, .loginAlert)), "a sign-in asks")
        expect(!AuthPolicy.offersAnswer(meta(1, .loginAlert, explainedBy: 2)), "an explained one does not")
        expect(AuthPolicy.offersAnswer(meta(1, .loginAlert, age: 30 * day)), "an old one opened on purpose can still be answered")
        expect(!AuthPolicy.offersAnswer(meta(1, .otp)), "a code does not ask")
        expect(AuthPolicy.offersAnswer(meta(1, .otp, critical: true)), "unless it is also a critical change")
        expect(AuthPolicy.questionPrompt(meta(1, .passwordReset)) == "Did you ask for this?", "reset prompt")
        expect(AuthPolicy.questionPrompt(meta(1, .appGrant)) == "Did you allow this?", "grant prompt")
        expect(AuthPolicy.answeredLine(meta(1, .appGrant), .mine) == "You said: I allowed it", "answered grant")
        expect(AuthPolicy.answeredLine(meta(1, .loginAlert), .notMine) == "You said: Not me", "answered sign-in")
        let tap = AuthPolicy.revealMeta(messageId: 7, threadId: "t7", sender: "a@b.com")
        expect(tap.id == 7 && !tap.speaksVerdicts, "a tap's meta never claims the daemon keeps verdicts")
    }

    static func clearAllSparesCriticalChanges() {
        // Account changes never burst, so each is its own card: a provider's
        // pile of them is the case this exists for.
        let six = (1...6).map { meta($0, .accountChange, from: "no-reply@aws.amazon.com") }
        expect(!AuthPolicy.showsClearAll(AuthPolicy.cards(six)), "six cards fit the rail: no clear all")
        let pile = six + [meta(7, .accountChange, from: "no-reply@aws.amazon.com")]
        expect(AuthPolicy.showsClearAll(AuthPolicy.cards(pile)), "a seventh card brings clear all")
        let burst = (1...9).map { i in
            meta(i, .loginAlert, from: "x@svc\(i).com", age: Double(i) * 3600)
        }
        expect(!AuthPolicy.showsClearAll(AuthPolicy.cards(burst)), "counted collapsed: one burst is one card")

        let mixed = pile + [meta(8, .accountChange, critical: true)]
        let cleared = AuthPolicy.clearable(mixed)
        expect(cleared.count == 7, "everything but the critical change is cleared")
        expect(!cleared.contains { $0.isCritical }, "a critical change is never cleared in bulk")
        expect(
            AuthPolicy.clearAllLine(cleared: 7, kept: 1).hasSuffix("1 critical change stays open for you to answer one by one."),
            "the confirmation names what stays")
        expect(!AuthPolicy.clearAllLine(cleared: 2, kept: 0).contains("critical"), "and says nothing when nothing stays")
        expect(!AuthPolicy.clearAllLine(cleared: 7, kept: 2).contains("\u{2014}"), "no em dashes in copy")
    }

    // MARK: -

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }
}
