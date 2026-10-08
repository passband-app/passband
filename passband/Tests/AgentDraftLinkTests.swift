import Foundation

@main
struct AgentDraftLinkTests {
    static func link(_ s: String) -> AgentDraftLink? { URL(string: s).flatMap(AgentDraftLink.init) }

    static func main() {
        var checks = 0
        func check(_ ok: Bool, _ what: String) {
            precondition(ok, what)
            checks += 1
        }
        // The link the daemon mints (squelch-core `draft_link`).
        let minted = link("passband://draft/12?mailbox=me%2Bwork%40example.com&t=1790000000")
        check(minted?.draftId == 12, "the id is read")
        check(minted?.created == 1_790_000_000, "the creation second is read")
        check(minted?.mailbox == "me+work@example.com", "the mailbox is decoded")
        check(link("passband://draft/12?t=1790000000")?.mailbox == nil, "a link without a mailbox still parses")
        check(link("PASSBAND://DRAFT/7?t=5")?.draftId == 7, "scheme and host are case-insensitive")
        check(
            link("passband://draft/7?mailbox=ME%40Example.com&t=5")?.mailbox == "me@example.com",
            "the mailbox is lowercased")
        check(link("passband://draft/7?mailbox=&t=5")?.mailbox == nil, "an empty mailbox is none")

        // THE LINK IS BOUND TO ONE DRAFT by the second it was created. Ids are
        // reused; the time is what an old link, or a guessed one, lacks.
        let bound = link("passband://draft/12?t=1790000000")!
        check(bound.names(draftCreatedAt: "2026-09-21T14:13:20+00:00"), "the same second matches")
        check(bound.names(draftCreatedAt: "2026-09-21T14:13:20.987654+00:00"), "fractions of it match")
        check(bound.names(draftCreatedAt: "2026-09-21T14:13:20Z"), "Z spelling matches")
        check(!bound.names(draftCreatedAt: "2026-09-21T14:13:21+00:00"), "the next second does not")
        check(!bound.names(draftCreatedAt: "2026-09-21T14:13:19.999+00:00"), "nor the one before")
        check(!bound.names(draftCreatedAt: "not a date"), "an unreadable time is never a match")
        check(!bound.names(draftCreatedAt: ""), "nor an empty one")

        // Anything that is not exactly the minted shape is not a draft link.
        for bad in [
            "passband://pair?server=https://x.example&code=ABCD",
            "passband://draft",
            "passband://draft/",
            "passband://draft/12",
            "passband://draft/12?mailbox=a%40b.c",
            "passband://draft/12?t=",
            "passband://draft/12?t=0",
            "passband://draft/12?t=-5",
            "passband://draft/12?t=1e9",
            "passband://draft/12?t=5&t=6",
            "passband://draft/12?t=5&mailbox=a%40b.c&mailbox=x%40y.z#frag",
            "passband://draft/0?t=5",
            "passband://draft/-3?t=5",
            "passband://draft/012?t=5",
            "passband://draft/12/?t=5",
            "passband://draft//12?t=5",
            "passband://draft/12/send?t=5",
            "passband://draft/12abc?t=5",
            "passband://draft/1e3?t=5",
            "passband://draft/9999999999999?t=5",
            "passband://draft/%31%32?t=5",
            "passband://draft/12?t=5#frag",
            "passband://user:pw@draft/12?t=5",
            "passband://draft:99/12?t=5",
            "passband://drafts/12?t=5",
            "passband:draft/12?t=5",
            "passband:///draft/12?t=5",
            "https://draft/12?t=5",
            "passband://draft/١٢?t=5",
        ] {
            check(link(bad) == nil, "\(bad) must not parse")
        }
        // A repeated mailbox is no mailbox, not the first one.
        check(link("passband://draft/12?t=5&mailbox=a%40b.c&mailbox=x%40y.z")?.mailbox == nil, "repeated mailbox")

        // The stream frame.
        let signal = AgentOpenSignal.parse(#"{"open_draft":12,"at":1000}"#)
        check(signal?.open_draft == 12 && signal?.at == 1000, "the signal is read")
        check(signal?.isFresh(now: 1000) == true, "fresh at the moment it was made")
        check(signal?.isFresh(now: 1060) == true, "and a minute on")
        check(signal?.isFresh(now: 1061) == false, "not after that: a Mac waking up drops it")
        check(signal?.isFresh(now: 4600) == false, "nor an hour later")
        check(signal?.isFresh(now: 939) == false, "nor from a clock far ahead")
        check(AgentOpenSignal.parse(#"{"open_draft":12}"#)?.isFresh(now: 5) == true, "an undated one is taken as fresh")
        for bad in [
            #"{"open_draft":0}"#, #"{"open_draft":-1}"#, #"{"open_draft":"12"}"#, #"{"open_draft":1.5}"#,
            #"{"id":4,"kind":"urgent"}"#, "", "open_draft",
        ] {
            check(AgentOpenSignal.parse(bad) == nil, "\(bad) is not a signal")
        }
        print("ok: \(checks) agent draft link checks passed")
    }
}
