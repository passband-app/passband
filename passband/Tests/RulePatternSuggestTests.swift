// The rule editor's sender menu, as typing: what each field text asks the
// directory for, what the answers become, and what an older daemon's page of
// senders rolls up to. Every case is one an owner can produce at the keyboard.

import Foundation

@main
@MainActor
struct RulePatternSuggestTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        plainTextAsksForPeopleAndHosts()
        starAtAsksForHostsAlone()
        aHandWrittenGlobAsksForNothing()
        peopleComeFirstThenHosts()
        theFieldsOwnTextIsNotAnOffer()
        anOlderDaemonsSendersRollUpByHost()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func plainTextAsksForPeopleAndHosts() {
        ask("stripe", is: .senders("stripe", domains: "stripe"))
        ask("  Dan Smith ", is: .senders("Dan Smith", domains: "Dan Smith"))
        // An address in progress already has its `@`: it names a mailbox, and
        // no host contains it.
        ask("receipts@str", is: .senders("receipts@str", domains: nil))
        ask("", is: .nothing)
        ask("   ", is: .nothing)
    }

    static func starAtAsksForHostsAlone() {
        // Right after `*@` the owner has not said which host: the busiest.
        ask("*@", is: .domains(""))
        ask("*@str", is: .domains("str"))
        ask("*@e.stripe.com", is: .domains("e.stripe.com"))
        // A bare `@` is the same question without the star.
        ask("@", is: .domains(""))
        ask("@stripe", is: .domains("stripe"))
    }

    static func aHandWrittenGlobAsksForNothing() {
        ask("*", is: .nothing)
        ask("*stripe*", is: .nothing)
        ask("billing*@stripe.com", is: .nothing)
        ask("*@*.stripe.com", is: .nothing)
        ask("*@a@b", is: .nothing)
        ask("*@stripe com", is: .nothing)
    }

    static func peopleComeFirstThenHosts() {
        let rows = RulePatternSuggest.suggestions(
            typed: "stripe",
            senders: [
                sender("receipts@stripe.com", "Stripe", 9),
                sender("dan@stripe.com", "", 2),
            ],
            domains: [
                SenderDomainHit(domain: "stripe.com", sender_count: 2, msg_count: 11),
                SenderDomainHit(domain: "e.stripe.com", sender_count: 1, msg_count: 4),
            ])
        expect(
            rows.map(\.pattern),
            ["receipts@stripe.com", "dan@stripe.com", "*@stripe.com", "*@e.stripe.com"],
            "people, then hosts")
        expect(rows[0].kind, .sender(name: "Stripe"), "a name rides along")
        expect(rows[1].kind, .sender(name: nil), "an empty name is no name")
        expect(rows[2].kind, .domain(host: "stripe.com", senders: 2), "a host says how wide")
        expect(rows[2].messages, 11, "and how much mail")
    }

    static func theFieldsOwnTextIsNotAnOffer() {
        let domains = [
            SenderDomainHit(domain: "stripe.com", sender_count: 2, msg_count: 11),
            SenderDomainHit(domain: "stripe.community", sender_count: 1, msg_count: 1),
        ]
        expect(
            RulePatternSuggest.suggestions(typed: "*@Stripe.com ", senders: [], domains: domains)
                .map(\.pattern),
            ["*@stripe.community"], "a finished pattern is not under a menu of itself")
        expect(
            RulePatternSuggest.suggestions(
                typed: "dan@stripe.com", senders: [sender("dan@stripe.com", "Dan", 2)],
                domains: []
            ).count, 0, "nor a finished address")
        // The directory holds one row per address, but nothing here should
        // depend on that.
        expect(
            RulePatternSuggest.suggestions(
                typed: "dan",
                senders: [sender("dan@a.com", "Dan", 2), sender("Dan@a.com", "Dan", 1)],
                domains: []
            ).count, 1, "one row per pattern")
    }

    static func anOlderDaemonsSendersRollUpByHost() {
        expect(RulePatternSuggest.fallbackQuery("str"), "@str", "hosts starting with it")
        expect(RulePatternSuggest.fallbackQuery(""), "@", "or every sender")
        // Ranked by volume, as the directory answers.
        let page = [
            sender("a@gmail.com", "", 5),
            sender("receipts@Stripe.com", "", 4),
            sender("b@gmail.com", "", 3),
            sender("support@stripe.com", "", 3),
            sender("news@e.stripe.com", "", 2),
            sender("odd", "the @str team", 9),
        ]
        let all = RulePatternSuggest.domains(from: page, matching: "", limit: 8)
        expect(all.map(\.domain), ["gmail.com", "stripe.com", "e.stripe.com"], "by volume")
        expect(all.map(\.msg_count), [8, 7, 2], "summed over the page")
        expect(all[0].sender_count, nil, "a page cannot count a host's senders")
        // The query matched a display name too; a name is not a host.
        let some = RulePatternSuggest.domains(from: page, matching: "STR", limit: 8)
        expect(some.map(\.domain), ["stripe.com"], "hosts starting with the fragment")
        expect(
            RulePatternSuggest.domains(from: page, matching: "", limit: 1).map(\.domain),
            ["gmail.com"], "the limit is honoured")
        // A tie keeps the directory's order.
        let tie = RulePatternSuggest.domains(
            from: [sender("x@b.com", "", 1), sender("x@a.com", "", 1)], matching: "", limit: 8)
        expect(tie.map(\.domain), ["b.com", "a.com"], "a tie keeps the ranking")
    }

    // MARK: - helpers

    static func sender(_ addr: String, _ name: String, _ count: Int) -> SenderHit {
        SenderHit(
            addr: addr, display_name: name, msg_count: count,
            last_received_at: "2026-10-01T00:00:00Z")
    }

    static func ask(_ typed: String, is expected: RulePatternSuggest.Ask) {
        expect(RulePatternSuggest.ask(for: typed), expected, "ask(\(typed.debugDescription))")
    }

    static func expect<T: Equatable>(_ got: T, _ expected: T, _ what: String) {
        checks += 1
        if got != expected {
            failures += 1
            print("FAIL \(what): got \(got), expected \(expected)")
        }
    }
}
