// The recipient-list grammar, and the one operation in it with real
// consequences: moving somebody to Bcc.
//
// This suite exists because every failure mode here is silent. A list split on
// the wrong comma reports a person who does not exist and loses the one who
// did. A move that adds without removing leaves the address in a header every
// recipient can read while the sender believes it was blind. Nothing on screen
// contradicts either of those — the composer would show exactly what it was
// told — so the only place they can be caught is here.

import Foundation

@main
@MainActor
struct RecipientsTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        splittingRespectsQuotedNames()
        theKeyIsTheBareMailbox()
        aHalfTypedFragmentIsNobody()
        movingTakesTheAddressOutOfEveryOtherField()
        movingCarriesTheDisplayName()
        movingMatchesAcrossSpellings()
        movingSomebodyAlreadyThereIsANoOp()
        typingAnAddressIntoAFieldMovesThemThere()
        removingIsIdempotent()
        countsAndSlots()
        aMovedAddressIsAFinishedRecipient()
        readingSkipsHeadersThatNamedNobody()
        readingNamesTheReaderYou()
        readingKeepsNamesLinedUpWithAddresses()
        readingMarksWhoJoinedAndWhoWasDropped()
        aMoveBetweenHeadersIsNotAChange()
        anUnreadPredecessorMarksNobody()
        aPredecessorThatNamedNobodyMarksNobody()
        aReplyDoesNotDropItsOwnAuthorOrAddTheLastOne()
        aBlindCopyNeverGhosts()
        aDroppedPayloadYieldsOnlyAddresses()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - list grammar

    static func splittingRespectsQuotedNames() {
        equal(Recipients.split(""), [], "an empty list names nobody")
        equal(Recipients.split("   ,  , "), [], "separators alone name nobody")
        equal(Recipients.split("a@x.test"), ["a@x.test"], "one address")
        equal(
            Recipients.split("a@x.test, b@y.test"), ["a@x.test", "b@y.test"],
            "two addresses")
        // THE COMMA THAT IS NOT A SEPARATOR. Splitting here would report two
        // recipients, neither of whom is real, and drop John entirely.
        equal(
            Recipients.split("\"Doe, John\" <j@x.test>, b@y.test"),
            ["\"Doe, John\" <j@x.test>", "b@y.test"],
            "a comma inside a quoted display name is part of the name")
    }

    static func theKeyIsTheBareMailbox() {
        equal(Recipients.key("bob@x.test"), "bob@x.test", "a bare address is itself")
        equal(Recipients.key("Bob Smith <bob@x.test>"), "bob@x.test", "the angle pair wins")
        equal(Recipients.key("  BOB@X.TEST "), "bob@x.test", "case-folded and trimmed")
        equal(
            Recipients.key("\"Doe, John\" <j@x.test>"), "j@x.test",
            "a quoted name does not confuse the brackets")
        // A display name may legitimately contain angle brackets; the address is
        // always the LAST pair.
        equal(
            Recipients.key("\"<the boss>\" <b@x.test>"), "b@x.test",
            "the last angle pair is the address")
    }

    static func aHalfTypedFragmentIsNobody() {
        // A live field holds a fragment mid-typing. Two fragments must never
        // compare equal, or the second one typed would silently replace the
        // first somewhere else.
        equal(Recipients.key("bo"), "", "no @ is not an address yet")
        equal(Recipients.key(""), "", "empty is nobody")
        var r = Recipients(to: "alice@x.test")
        r.move("bo", to: .bcc)
        equal(r.to, "alice@x.test", "a fragment moves nothing")
        equal(r.bcc, "", "and lands nowhere")
    }

    // MARK: - the move

    static func movingTakesTheAddressOutOfEveryOtherField() {
        var r = Recipients(to: "alice@x.test, bob@x.test", cc: "carol@x.test")
        r.move("bob@x.test", to: .bcc)
        equal(r.to, "alice@x.test", "gone from To")
        equal(r.cc, "carol@x.test", "Cc untouched")
        equal(r.bcc, "bob@x.test", "landed in Bcc")

        // THE FAILURE THIS EXISTS FOR: an add that did not remove would leave
        // bob in To as well, so his address rides in a header every recipient
        // reads while the sender believes he was blind-copied.
        equal(r.slot(of: "bob@x.test"), RecipientSlot.bcc, "in exactly one field")
        equal(r.all.count, 3, "and counted exactly once")
    }

    static func movingCarriesTheDisplayName() {
        var r = Recipients(to: "Bob Smith <bob@x.test>")
        r.move("Bob Smith <bob@x.test>", to: .cc)
        equal(r.cc, "Bob Smith <bob@x.test>", "the token moves whole")
        equal(r.to, "", "and only once")
    }

    static func movingMatchesAcrossSpellings() {
        // The two lists spell the same person differently — which is ordinary,
        // since one was derived from a header and the other typed. Matching on
        // the bare mailbox is what keeps them one person.
        var r = Recipients(to: "Bob Smith <bob@x.test>", cc: "BOB@X.TEST")
        r.move("bob@x.test", to: .bcc)
        equal(r.to, "", "the named copy went")
        equal(r.cc, "", "so did the shouted one")
        equal(r.bcc, "bob@x.test", "one person, one field")
    }

    static func aDroppedPayloadYieldsOnlyAddresses() {
        // A pill from the next field over: its own token, whole.
        equal(Recipients.dropped("Bob Smith <bob@x.test>"), ["Bob Smith <bob@x.test>"], "one pill, name and all")
        // A list, with the same person spelled twice: once, first spelling.
        equal(
            Recipients.dropped("Bob <bob@x.test>, alice@x.test, BOB@X.TEST"),
            ["Bob <bob@x.test>", "alice@x.test"], "each person once")
        // A column pasted out of a spreadsheet has newlines where commas go.
        equal(Recipients.dropped("a@x.test\nb@x.test"), ["a@x.test", "b@x.test"], "lines are separators too")
        // Prose is nobody, even prose that mentions somebody.
        equal(Recipients.dropped("call me maybe"), [], "no @, no address")
        equal(Recipients.dropped("Contact alice@x.test for details"), [], "a sentence is not an address")
    }

    static func movingSomebodyAlreadyThereIsANoOp() {
        var r = Recipients(to: "alice@x.test", bcc: "bob@x.test")
        r.move("bob@x.test", to: .bcc)
        equal(r.bcc, "bob@x.test", "still there, exactly once")
        equal(r.to, "alice@x.test", "and nobody else moved")
    }

    static func typingAnAddressIntoAFieldMovesThemThere() {
        // A recipient field writes its whole list back through `set` on every
        // keystroke. Somebody TYPED into To who was blind-copied has just been
        // moved — typing the address out is as explicit as dragging the pill —
        // and leaving the Bcc copy behind would put them in two headers, which
        // is the failure this whole type exists to prevent.
        var r = Recipients(to: "alice@x.test", bcc: "bob@x.test")
        r.set(.to, to: "alice@x.test, BOB@X.TEST")
        equal(r.to, "alice@x.test, BOB@X.TEST", "the field says what was typed")
        equal(r.bcc, "", "and they are no longer blind-copied")

        // A HALF-TYPED FRAGMENT CLAIMS NOBODY. The field's live text includes
        // whatever is under the caret, so a `set` fires on every keystroke —
        // and "bo" must not evict anyone anywhere.
        var mid = Recipients(to: "alice@x.test", cc: "bob@x.test", bcc: "carol@x.test")
        mid.set(.to, to: "alice@x.test, bo")
        equal(mid.cc, "bob@x.test", "still copied")
        equal(mid.bcc, "carol@x.test", "still blind-copied")

        // Clearing a field takes nobody with it.
        var cleared = Recipients(to: "alice@x.test", bcc: "bob@x.test")
        cleared.set(.cc, to: "")
        equal(cleared.to, "alice@x.test", "To untouched")
        equal(cleared.bcc, "bob@x.test", "Bcc untouched")
    }

    static func removingIsIdempotent() {
        var r = Recipients(to: "alice@x.test, bob@x.test")
        r.remove("bob@x.test")
        equal(r.to, "alice@x.test", "removed")
        r.remove("bob@x.test")
        equal(r.to, "alice@x.test", "removing them again is the state you asked for")
        r.remove("nobody@x.test")
        equal(r.to, "alice@x.test", "and so is removing a stranger")
    }

    static func countsAndSlots() {
        let r = Recipients(
            to: "\"Doe, John\" <j@x.test>", cc: "b@y.test, c@z.test", bcc: "d@w.test")
        equal(r.count(.to), 1, "the quoted comma is not a second person")
        equal(r.count(.cc), 2, "two copies")
        equal(r.count(.bcc), 1, "one blind copy")
        equal(r.all.count, 4, "four people in all")
        equal(r.slot(of: "C@Z.TEST"), RecipientSlot.cc, "found case-insensitively")
        equal(r.slot(of: "stranger@x.test"), nil, "a stranger is in no field")
    }

    /// The composer's field re-reads its whole list after every move, and it
    /// has to be able to tell "a finished recipient" from "somebody
    /// mid-keystroke" WITHOUT a trailing comma to go on — nothing but typing
    /// ever puts one there. `key` is that test: an address has one, a fragment
    /// does not.
    static func aMovedAddressIsAFinishedRecipient() {
        var r = Recipients(to: "alice@x.test, bob@x.test")
        r.move("bob@x.test", to: .cc)
        // No trailing comma, and there never will be — the field that renders
        // this has to pill it anyway or the move reads as half-done.
        equal(r.cc, "bob@x.test", "the value carries no separator")
        equal(Recipients.key(Recipients.split(r.cc).last ?? "").isEmpty, false, "and it is an address")

        // The other half of the same test: a real fragment has no key, which is
        // what keeps it editable instead of being pilled out from under the
        // caret.
        equal(Recipients.key("bo").isEmpty, true, "a fragment is not finished")
    }

    // MARK: - the reading side

    /// A header nobody was on draws no line, and the two ways of being on no
    /// header — never read, and read and empty — draw the same nothing. (They
    /// are NOT the same statement upstream: one is the daemon's backfill queue.)
    static func readingSkipsHeadersThatNamedNobody() {
        equal(
            Recipients.reading(audience(), me: nil).lines.count, 0,
            "read-and-named-nobody draws nothing")
        equal(
            Recipients.reading(audience(cc: "  ,  "), me: nil).lines.count, 0,
            "separators alone name nobody")
        let d = Recipients.reading(audience(to: "a@x.test", bcc: "b@x.test"), me: nil)
        equal(d.lines.count, 2, "only the headers with somebody on them")
        equal(d.lines.map(\.slot), [RecipientSlot.to, .bcc], "in header order")
        equal(d.count, 2, "two people addressed")
        equal(d.changed, false, "with nothing before it, nothing has changed")
    }

    /// The reader knows they were on the mail. Their own address spelled out
    /// among four others is the one name on the line carrying no information —
    /// and it has to be matched on the bare mailbox, because the same person is
    /// written three ways across the headers of one message.
    static func readingNamesTheReaderYou() {
        let d = Recipients.reading(
            audience(to: "You <ME@x.test>, Bob Smith <bob@y.test>", cc: "me@x.test"),
            me: "me@x.test")
        equal(names(d.lines[0]), ["you", "Bob Smith"], "shouted and named, still you")
        equal(names(d.lines[1]), ["you"], "and again on the Cc")

        // No account address yet (the first /client/stats has not landed): every
        // address renders as itself. A missing greeting, not a wrong one.
        let anon = Recipients.reading(audience(to: "You <me@x.test>"), me: nil)
        equal(names(anon.lines[0]), ["You"], "without an account address, nobody is you")
    }

    /// THE TOOLTIP AND THE LINE MUST NOT DISAGREE. The names are the readable
    /// half and the tokens are the half that says which bob@ it was, so they are
    /// built index-for-index off one split — and that split is quote-aware, or a
    /// lastname-first address book entry becomes two people who do not exist.
    static func readingKeepsNamesLinedUpWithAddresses() {
        let d = Recipients.reading(
            audience(to: "\"Doe, John\" <j@x.test>, plain@y.test"), me: nil)
        equal(d.lines.count, 1, "one header")
        equal(names(d.lines[0]), ["Doe, John", "plain@y.test"], "a name, then a bare address")
        equal(
            d.lines[0].full, "to: \"Doe, John\" <j@x.test>, plain@y.test",
            "the pointer gets the addresses, spelled as the header spelled them")
    }

    /// THE SIGNAL THIS EXISTS FOR. A thread that quietly loses a recipient looks
    /// exactly like one that did not, in every mail client there is: the message
    /// simply stops naming them, and nothing draws what is absent. The ghost is
    /// the only thing that can say it happened.
    static func readingMarksWhoJoinedAndWhoWasDropped() {
        let before = audience(to: "ann@x.test, bob@x.test", cc: "carol@x.test")
        let now = audience(to: "ann@x.test, dave@x.test", cc: "carol@x.test")
        let d = Recipients.reading(now, me: nil, previous: before)

        equal(d.added, 1, "dave joined")
        equal(d.removed, 1, "bob was dropped")
        equal(d.changed, true, "so the block opens itself")
        equal(d.count, 3, "a ghost is not a recipient of this message")

        let to = d.lines[0]
        equal(to.people.map(\.mark), [.present, .added, .removed], "in that order")
        equal(to.people.map(\.name), ["ann@x.test", "dave@x.test", "bob@x.test"], "who")
        equal(
            to.full,
            "to: ann@x.test, dave@x.test (added), bob@x.test (removed)",
            "and the pointer says which is which in words")
        equal(d.lines[1].people.map(\.mark), [.present], "carol never moved")

        // A ghost is drawn ON THE HEADER THEY LEFT, even when this message has no
        // such header of its own — otherwise dropping the whole Cc list would
        // erase the evidence along with the people.
        let cut = Recipients.reading(audience(to: "ann@x.test"), me: nil, previous: before)
        equal(cut.lines.count, 2, "the Cc line survives its own emptying")
        equal(cut.lines[1].people.map(\.mark), [.removed], "as a ghost")
        equal(cut.removed, 2, "bob and carol both went")
    }

    /// Somebody moved from To to Cc is still in the conversation. Marking that as
    /// an addition and a removal at once would cry wolf on the one signal here.
    static func aMoveBetweenHeadersIsNotAChange() {
        let before = audience(to: "ann@x.test, bob@x.test")
        let now = audience(to: "ann@x.test", cc: "Bob <BOB@X.TEST>")
        let d = Recipients.reading(now, me: nil, previous: before)
        equal(d.added, 0, "nobody joined")
        equal(d.removed, 0, "and nobody left")
        equal(d.changed, false, "so the block stays shut")
    }

    /// An unread predecessor is not evidence that everyone here is new. The
    /// daemon's recipients sweep walks a mailbox back over minutes, so mid-sweep
    /// a thread holds messages that have been read and messages that have not.
    static func anUnreadPredecessorMarksNobody() {
        let d = Recipients.reading(
            audience(to: "ann@x.test, bob@x.test"), me: nil, previous: nil)
        equal(d.added, 0, "no marks without something to compare against")
        equal(d.changed, false, "and no self-opening block")
        equal(d.lines[0].people.map(\.mark), [.present, .present], "everyone plain")
    }

    /// A newsletter that reached you as a Bcc names nobody. The reply after it
    /// is not "everyone joined".
    static func aPredecessorThatNamedNobodyMarksNobody() {
        let d = Recipients.reading(
            audience(to: "you@x.test", cc: "ann@x.test, bob@x.test"), me: nil,
            previous: audience(from: "news@x.test"))
        equal(d.added, 0, "nobody is new against a message that named nobody")
        equal(d.changed, false, "and the block stays shut")
    }

    static func names(_ line: RecipientLine) -> [String] { line.people.map(\.name) }

    /// One message's audience. `from` defaults to somebody outside every fixture
    /// list, so a test that is not about the sender is not quietly about it.
    static func audience(
        from: String = "outsider@x.test", to: String = "", cc: String = "", bcc: String = ""
    ) -> MessageAudience {
        MessageAudience(sender: from, recipients: Recipients(to: to, cc: cc, bcc: bcc))
    }

    /// THE TWO MOST ORDINARY EVENTS IN MAIL, and the two this mark would have
    /// cried wolf on. A reply does not name its own author in its To, and the
    /// message it answers did not name its author either — so a diff over
    /// recipients alone reports "Daniel left" and "Maya joined" for a plain
    /// back-and-forth, on every thread, forever.
    static func aReplyDoesNotDropItsOwnAuthorOrAddTheLastOne() {
        let first = audience(from: "maya@x.test", to: "you@x.test, daniel@x.test")
        let reply = audience(from: "daniel@x.test", to: "you@x.test, maya@x.test")
        let d = Recipients.reading(reply, me: "you@x.test", previous: first)
        equal(d.added, 0, "the author of the last message is not a new face")
        equal(d.removed, 0, "and the author of this one has not left it")
        equal(d.changed, false, "so an ordinary exchange opens nothing")
    }

    /// A blind copy is invisible to every other recipient, so the next message in
    /// the thread cannot name them and never does. Ghosting them would report a
    /// removal on every thread the user ever blind-copied anyone on — and report
    /// it to the one person who knows it did not happen.
    static func aBlindCopyNeverGhosts() {
        let mine = audience(
            from: "you@x.test", to: "maya@x.test", bcc: "Ana Beck <ana@x.test>")
        let theirs = audience(from: "maya@x.test", to: "you@x.test")
        let d = Recipients.reading(theirs, me: "you@x.test", previous: mine)
        equal(d.removed, 0, "Ana was never visible to Maya to drop")
        equal(d.changed, false, "and nothing opens")

        // The other direction still marks: blind-copying somebody IS a change to
        // who is reading, and it is the user's own doing, so they can see it.
        let opening = Recipients.reading(mine, me: "you@x.test", previous: theirs)
        equal(opening.added, 1, "Ana joined, blind or not")
        equal(
            opening.lines.last?.people.map(\.mark), [.added],
            "and the mark lands on the Bcc line")
    }

    // MARK: - assert

    static func equal<T: Equatable>(_ got: T?, _ want: T?, _ what: String) {
        checks += 1
        if got != want {
            failures += 1
            print("  FAIL \(what): got \(String(describing: got)), want \(String(describing: want))")
        }
    }
}
