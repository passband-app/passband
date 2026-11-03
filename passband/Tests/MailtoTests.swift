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
        precondition(Mailto("mailto:") == nil, "asks for nothing")
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
}
