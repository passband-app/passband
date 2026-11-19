// A mailto: link out of a message, read into a composer's fields. The link is
// untrusted mail content, so what it may NOT fill matters as much as what it
// may.

import Foundation

@main
struct MailtoTests {
    static func main() {
        aBareAddress()
        theQueryFillsTheFields()
        caseAndPlusSurvive()
        oneAddressOneField()
        junkIsDropped()
        otherHeadersAreIgnored()
        longFieldsAreCut()
        reviewFindings()
        aLinkFromAnotherApp()
        whenALinkFromAnotherAppOpens()
        print("mailto: ok")
    }

    static func aBareAddress() {
        let m = Mailto("mailto:ada@example.com")
        precondition(m?.to == ["ada@example.com"])
        precondition(m?.subject == "" && m?.body == "")
        precondition(Mailto.isMailto("MAILTO:ada@example.com"))
        precondition(!Mailto.isMailto("https://example.com/mailto:x"))
        precondition(Mailto("https://example.com") == nil)
    }

    static func theQueryFillsTheFields() {
        let m = Mailto(
            "mailto:a@x.com,b@y.org?cc=c@z.io&bcc=d@w.net&subject=Hello%20there&body=line%201%0D%0Aline%202")
        precondition(m?.to == ["a@x.com", "b@y.org"])
        precondition(m?.cc == ["c@z.io"])
        precondition(m?.bcc == ["d@w.net"])
        precondition(m?.subject == "Hello there")
        precondition(m?.body == "line 1\nline 2", "CRLF becomes a newline")
        // No path, everything in ?to=.
        precondition(Mailto("mailto:?to=q@x.com&subject=hi")?.to == ["q@x.com"])
    }

    static func caseAndPlusSurvive() {
        let m = Mailto("mailto:me+tag@x.com?SUBJECT=a+b")
        precondition(m?.to == ["me+tag@x.com"], "a + in an address is a character")
        precondition(m?.subject == "a+b")
        precondition(Mailto("mailto:a%40x.com")?.to == ["a@x.com"], "an encoded @ decodes")
    }

    static func oneAddressOneField() {
        let m = Mailto("mailto:a@x.com?to=A@x.com&cc=a@x.com,b@x.com&bcc=b@x.com,c@x.com")
        precondition(m?.to == ["a@x.com"], "duplicates fold, case-insensitively")
        precondition(m?.cc == ["b@x.com"])
        precondition(m?.bcc == ["c@x.com"])
    }

    static func junkIsDropped() {
        // A bare mailto: is a blank message, as Mail opens one.
        let blank = Mailto("mailto:")
        precondition(blank != nil && blank!.to.isEmpty && blank!.cc.isEmpty && blank!.bcc.isEmpty
            && blank!.subject.isEmpty && blank!.body.isEmpty, "a bare mailto: opens a blank composer")
        precondition(Mailto("mailto:?") != nil, "and so does a bare query")
        precondition(Mailto("mailto:not-an-address") == nil)
        let noOne = Mailto("mailto:?subject=hi")
        precondition(noOne?.to == [] && noOne?.subject == "hi", "a subject alone still opens")
        precondition(Mailto("mailto:a@x.com,%3Cevil%3E@x.com,b@nodot")?.to == ["a@x.com"])
        precondition(Mailto("mailto:a@x.com?subject=one%0Atwo")?.subject == "one two",
            "a subject is one line")
    }

    static func otherHeadersAreIgnored() {
        let m = Mailto("mailto:a@x.com?from=boss@x.com&in-reply-to=%3Cid%3E&attach=/etc/passwd")
        precondition(m == Mailto("mailto:a@x.com"))
        // The first subject wins, a second does not overwrite it.
        precondition(Mailto("mailto:a@x.com?subject=one&subject=two")?.subject == "one")
    }

    static func longFieldsAreCut() {
        let long = String(repeating: "x", count: 20_000)  // under hrefLimit twice over
        let m = Mailto("mailto:a@x.com?subject=\(long)&body=\(long)")
        precondition(m?.subject.count == Mailto.subjectLimit)
        precondition(m?.body.count == Mailto.bodyLimit)
    }

    static func reviewFindings() {
        // An encoded comma is decoded once, inside its address, not split on.
        precondition(Mailto("mailto:a@x.com?cc=x%252Cevil@e.com")?.cc == ["x%2Cevil@e.com"],
            "a doubly encoded comma is decoded once and never becomes a separator")
        precondition(Mailto("mailto:a@x.com?cc=b%2Cc@x.com")?.cc == [], "a comma is not in an address")
        // The fragment is not a field.
        precondition(Mailto("mailto:a@b.com?to=c@d.com#frag")?.to == ["a@b.com", "c@d.com"])
        precondition(Mailto("mailto:a@b.com#x")?.to == ["a@b.com"])
        // Across fields, case-insensitively.
        let m = Mailto("mailto:a@b.com?cc=A@B.COM&bcc=a@B.com")
        precondition(m?.to == ["a@b.com"] && m?.cc == [] && m?.bcc == [])
        // Hidden characters.
        precondition(Mailto("mailto:a%E2%80%AEmoc.x@y.com")?.to == nil, "RLO in an address refuses it")
        precondition(Mailto("mailto:a%E2%80%8B@y.com") == nil, "ZWSP in an address refuses it")
        let s = Mailto("mailto:a@x.com?subject=a%E2%80%AEb%00c&body=x%0Ay%09z%E2%80%AE")
        precondition(s?.subject == "abc", "controls and bidi leave the subject")
        precondition(s?.body == "x\ny\tz", "a body keeps newlines and tabs, nothing hidden")
        // Group tokens and broken domains.
        precondition(Mailto("mailto:%23team@x.com") == nil)
        precondition(Mailto("mailto:a@b..com") == nil)
        // Bounded: too long is refused, too many recipients is cut.
        precondition(Mailto("mailto:a@x.com?body=" + String(repeating: "x", count: Mailto.hrefLimit)) == nil)
        let many = (0..<500).map { "u\($0)@x.com" }.joined(separator: ",")
        let big = Mailto("mailto:\(many)?cc=\(many)")
        precondition(big?.to.count == Mailto.recipientLimit && big?.cc == [])
    }

    /// REGRESSION PIN, not a new path: a link from another app reaches
    /// `Mailto` as `URL.absoluteString`, and must read exactly as the same
    /// link inside a message does (the parser is shared; this pins that the
    /// URL round trip does not change what it sees).
    static func aLinkFromAnotherApp() {
        let url = URL(string: "mailto:ada@example.com?cc=bo@example.org&subject=Lunch%20on%20Friday&body=Are%20you%20free%3F")!
        let m = Mailto(url.absoluteString)
        precondition(m?.to == ["ada@example.com"])
        precondition(m?.cc == ["bo@example.org"])
        precondition(m?.subject == "Lunch on Friday")
        precondition(m?.body == "Are you free?")
        precondition(Mailto(URL(string: "MAILTO:ada@example.com")!.absoluteString)?.to == ["ada@example.com"])
        let sneaky = URL(string: "mailto:ada@example.com?from=boss@example.com&attach=%2Fetc%2Fhosts")!
        precondition(Mailto(sneaky.absoluteString) == Mailto("mailto:ada@example.com"))
        precondition(Mailto(URL(string: "mailto:")!.absoluteString)?.to == [], "blank, not refused")
    }

    /// Links from another app, held for a mailbox: in order, a few at a
    /// time, one composer at a time, never past a minute without one, and
    /// never dropped without a reason to show.
    static func whenALinkFromAnotherAppOpens() {
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)

        // Connected and verified, nothing open: the link opens at once.
        var inbox = MailtoInbox()
        precondition(inbox.add("mailto:a@x.com", at: t0) == nil)
        var r = inbox.settle(connected: true, occupant: .none, gate: false, now: t0)
        precondition(r.open == "mailto:a@x.com" && r.dropped.isEmpty && inbox.isEmpty)

        // A cold launch: held while the connection is unverified...
        inbox = MailtoInbox()
        _ = inbox.add("mailto:a@x.com", at: t0)
        r = inbox.settle(connected: false, occupant: .none, gate: false, now: t0.addingTimeInterval(5))
        precondition(r.open == nil && r.dropped.isEmpty && !inbox.isEmpty, "held, not dropped")
        precondition(inbox.nextExpiry == t0.addingTimeInterval(MailtoInbox.holdFor))
        // ...and opened once it is verified.
        r = inbox.settle(connected: true, occupant: .none, gate: false, now: t0.addingTimeInterval(8))
        precondition(r.open == "mailto:a@x.com" && inbox.isEmpty)

        // Never verified within the hold: dropped, with a reason.
        inbox = MailtoInbox()
        _ = inbox.add("mailto:a@x.com", at: t0)
        r = inbox.settle(connected: false, occupant: .none, gate: false, now: t0.addingTimeInterval(59))
        precondition(r.dropped.isEmpty, "a second short is still held")
        r = inbox.settle(connected: false, occupant: .none, gate: false, now: t0.addingTimeInterval(60))
        precondition(r.open == nil && r.dropped == [.expired] && inbox.isEmpty, "expired")
        precondition(inbox.nextExpiry == nil)

        // The Connect screen: no mailbox, every held link dropped.
        inbox = MailtoInbox()
        _ = inbox.add("mailto:a@x.com", at: t0)
        _ = inbox.add("mailto:b@x.com", at: t0)
        r = inbox.settle(connected: false, occupant: .none, gate: true, now: t0)
        precondition(r.open == nil && r.dropped == [.noMailbox, .noMailbox] && inbox.isEmpty)

        // FIFO, three deep: a fourth is refused, not swapped in.
        inbox = MailtoInbox()
        for (i, h) in ["mailto:a@x.com", "mailto:b@x.com", "mailto:c@x.com"].enumerated() {
            precondition(inbox.add(h, at: t0.addingTimeInterval(Double(i))) == nil)
        }
        precondition(inbox.add("mailto:d@x.com", at: t0) == .full, "a fourth is refused")
        // One composer at a time, oldest first.
        r = inbox.settle(connected: true, occupant: .none, gate: false, now: t0)
        precondition(r.open == "mailto:a@x.com")
        // Written in: kept, and the waiting links are SAID to wait, once.
        r = inbox.settle(connected: true, occupant: .writing, gate: false, now: t0.addingTimeInterval(1))
        precondition(r.open == nil && r.dropped.isEmpty && r.waiting,
            "a composer with writing in it is kept, and the wait is announced")
        r = inbox.settle(connected: true, occupant: .writing, gate: false, now: t0.addingTimeInterval(600))
        precondition(r.open == nil && r.dropped.isEmpty && !r.waiting,
            "announced once, and no clock runs on a person writing")
        r = inbox.settle(connected: true, occupant: .none, gate: false, now: t0.addingTimeInterval(601))
        precondition(r.open == "mailto:b@x.com")
        r = inbox.settle(connected: true, occupant: .none, gate: false, now: t0.addingTimeInterval(602))
        precondition(r.open == "mailto:c@x.com" && inbox.isEmpty)

        // Left untouched: the next link replaces it, at once.
        inbox = MailtoInbox()
        _ = inbox.add("mailto:a@x.com", at: t0)
        _ = inbox.add("mailto:b@x.com", at: t0)
        r = inbox.settle(connected: true, occupant: .none, gate: false, now: t0)
        precondition(r.open == "mailto:a@x.com")
        r = inbox.settle(connected: true, occupant: .untouched, gate: false, now: t0)
        precondition(r.open == "mailto:b@x.com" && !r.waiting && inbox.isEmpty,
            "an untouched composer is replaced by the next link, never a silent hold")
        // A link that arrives while writing is told too, even after an
        // earlier one was.
        inbox = MailtoInbox()
        _ = inbox.add("mailto:a@x.com", at: t0)
        precondition(inbox.settle(connected: true, occupant: .writing, gate: false, now: t0).waiting)
        _ = inbox.add("mailto:b@x.com", at: t0)
        precondition(inbox.settle(connected: true, occupant: .writing, gate: false, now: t0).waiting,
            "a new link held behind writing is announced")
        precondition(!MailtoInbox.waitingCopy.contains("\u{2014}"))

        // Every drop has words, plain and without an em dash.
        for d in [MailtoInbox.Drop.noMailbox, .expired, .full] {
            let line = MailtoInbox.copy(d)
            precondition(line.hasPrefix("Couldn't open that email link.") && !line.contains("\u{2014}"))
        }
    }
}
