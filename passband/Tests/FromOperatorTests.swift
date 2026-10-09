// The `from:` operator while it is being typed: exactly when the sender menu is
// up, what it is searching for, and what accepting a sender does to the text.
// The rule is "the trailing token is the caret", and every case here is one a
// reader can produce by typing, so a failure reads as a keystroke, not a state.

import Foundation

@main
@MainActor
struct FromOperatorTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        theMenuOpensOnTheTrailingToken()
        aSpaceClosesIt()
        anEarlierOperatorIsFinishedBusiness()
        acceptingCompletesTheOperator()
        acceptingOutsideTheOperatorChangesNothing()
        bareOperatorsAreNotASearchYet()
        plainWordsAskTheDirectory()
        aHeldRowMustStillMatch()
        pickingASenderListsTheirMail()
        operatorsAloneAreAListing()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func theMenuOpensOnTheTrailingToken() {
        fragment(of: "from:", is: "")
        fragment(of: "from:d", is: "d")
        fragment(of: "from:dan@ex", is: "dan@ex")
        fragment(of: "wifi password from:abst", is: "abst")
        // The daemon's parser is case-insensitive about the operator; so is the
        // menu, or `From:` would search for the literal word and offer nobody.
        fragment(of: "From:Dan", is: "Dan")
        fragment(of: "FROM:", is: "")
        // Other tokens, other operators, and bare words open nothing.
        fragment(of: "", is: nil)
        fragment(of: "wifi", is: nil)
        fragment(of: "after:2026-01-01", is: nil)
        fragment(of: "fromage", is: nil)
        fragment(of: "from", is: nil)
        // A token that merely CONTAINS the operator is not one.
        fragment(of: "xfrom:dan", is: nil)
    }

    static func aSpaceClosesIt() {
        fragment(of: "from:dan ", is: nil)
        fragment(of: "from:dan@example.com  ", is: nil)
        fragment(of: "from:\n", is: nil)
        fragment(of: " ", is: nil)
    }

    static func anEarlierOperatorIsFinishedBusiness() {
        fragment(of: "from:dan wifi", is: nil)
        fragment(of: "from:dan from:", is: "")
        fragment(of: "from:dan after:2026 from:j", is: "j")
    }

    static func acceptingCompletesTheOperator() {
        accepting("dan@example.com", in: "from:d", gives: "from:dan@example.com ")
        accepting("dan@example.com", in: "from:", gives: "from:dan@example.com ")
        accepting(
            "abstract@convex.dev", in: "wifi password from:abst",
            gives: "wifi password from:abstract@convex.dev ")
        // The operator's spelling is normalised on accept: the daemon reads
        // either, and one spelling is easier to look at.
        accepting("dan@example.com", in: "From:D", gives: "from:dan@example.com ")
        // Only the trailing operator is rewritten.
        accepting("j@example.com", in: "from:dan from:j", gives: "from:dan from:j@example.com ")
    }

    static func acceptingOutsideTheOperatorChangesNothing() {
        accepting("dan@example.com", in: "wifi", gives: "wifi")
        accepting("dan@example.com", in: "from:dan ", gives: "from:dan ")
        accepting("dan@example.com", in: "", gives: "")
    }

    static func bareOperatorsAreNotASearchYet() {
        awaiting("from:", is: true)
        awaiting("From:", is: true)
        awaiting("after:", is: true)
        awaiting("from: before:", is: true)
        awaiting("  from:  ", is: true)
        // Anything with content is a search, even alongside a bare operator.
        awaiting("from:d", is: false)
        awaiting("wifi from:", is: false)
        awaiting("from:dan", is: false)
        awaiting("after:2026-01-01", is: false)
        awaiting("", is: false)
        awaiting("   ", is: false)
    }

    static func plainWordsAskTheDirectory() {
        senderFragment(of: "dan", is: "dan")
        senderFragment(of: "dan@ex", is: "dan@ex")
        senderFragment(of: "  Dan   Smith ", is: "Dan Smith")
        // One character matches nearly every sender, so it asks nothing.
        senderFragment(of: "d", is: nil)
        senderFragment(of: "", is: nil)
        senderFragment(of: "   ", is: nil)
        // An operator anywhere means the reader has already said who, or is
        // saying it through the from: menu.
        senderFragment(of: "from:dan", is: nil)
        senderFragment(of: "invoice from:dan", is: nil)
        senderFragment(of: "dan after:2026-01-01", is: nil)
        senderFragment(of: "From:", is: nil)
        // Four words is a description of mail, not a name.
        senderFragment(of: "abstract conference wifi", is: "abstract conference wifi")
        senderFragment(of: "abstract conference wifi password", is: nil)
    }

    static func aHeldRowMustStillMatch() {
        matches("dan@example.com", "Dan Smith", "dan", is: true)
        matches("d.smith@example.com", "Dan Smith", "DAN SM", is: true)
        matches("billing@stripe.com", nil, "Stripe", is: true)
        matches("dan@example.com", "Dan Smith", "dana", is: false)
        matches("billing@stripe.com", nil, "dan", is: false)
    }

    static func pickingASenderListsTheirMail() {
        checks += 1
        let query = FromOperator.listing(of: "dan@example.com")
        if query != "from:dan@example.com " {
            failures += 1
            print("FAIL listing(of:) = \(query.debugDescription)")
        }
        // What picking writes is a listing, has no sender left to suggest, and
        // does not reopen the from: menu under itself.
        listing(query, is: true)
        senderFragment(of: query, is: nil)
        fragment(of: query, is: nil)
    }

    static func operatorsAloneAreAListing() {
        listing("from:dan@example.com", is: true)
        listing("FROM:dan", is: true)
        listing("from:dan after:2026-01-01", is: true)
        listing("after:2026-01-01", is: true)
        // Words are ranked, so the order is the search's and not the calendar's.
        listing("invoice from:dan", is: false)
        listing("dan", is: false)
        // A bare operator is not a value.
        listing("from:", is: false)
        listing("from:dan before:", is: false)
        listing("", is: false)
    }

    // MARK: - helpers

    static func senderFragment(of query: String, is expected: String?) {
        checks += 1
        let got = FromOperator.senderFragment(in: query)
        if got != expected {
            failures += 1
            print("FAIL senderFragment(\(query.debugDescription)) = \(String(describing: got)), expected \(String(describing: expected))")
        }
    }

    static func matches(_ addr: String, _ name: String?, _ fragment: String, is expected: Bool) {
        checks += 1
        let got = FromOperator.matches(addr: addr, name: name, fragment: fragment)
        if got != expected {
            failures += 1
            print("FAIL matches(\(addr), \(String(describing: name)), \(fragment)) = \(got), expected \(expected)")
        }
    }

    static func listing(_ query: String, is expected: Bool) {
        checks += 1
        let got = FromOperator.isListing(query)
        if got != expected {
            failures += 1
            print("FAIL isListing(\(query.debugDescription)) = \(got), expected \(expected)")
        }
    }

    static func awaiting(_ query: String, is expected: Bool) {
        checks += 1
        let got = FromOperator.awaitingValue(in: query)
        if got != expected {
            failures += 1
            print("FAIL awaitingValue(\(query.debugDescription)) = \(got), expected \(expected)")
        }
    }

    static func fragment(of query: String, is expected: String?) {
        checks += 1
        let got = FromOperator.fragment(in: query)
        if got != expected {
            failures += 1
            print("FAIL fragment(\(query.debugDescription)) = \(String(describing: got)), expected \(String(describing: expected))")
        }
    }

    static func accepting(_ address: String, in query: String, gives expected: String) {
        checks += 1
        let got = FromOperator.accepting(address, in: query)
        if got != expected {
            failures += 1
            print("FAIL accepting(\(address), in: \(query.debugDescription)) = \(got.debugDescription), expected \(expected.debugDescription)")
        }
    }
}
