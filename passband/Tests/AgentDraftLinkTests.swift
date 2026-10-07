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
        let minted = link("passband://draft/12?mailbox=me%2Bwork%40example.com")
        check(minted?.draftId == 12, "the id is read")
        check(minted?.mailbox == "me+work@example.com", "the mailbox is decoded")
        check(link("passband://draft/12")?.mailbox == nil, "a link without a mailbox still parses")
        check(link("PASSBAND://DRAFT/7")?.draftId == 7, "scheme and host are case-insensitive")
        check(link("passband://draft/7?mailbox=ME%40Example.com")?.mailbox == "me@example.com", "the mailbox is lowercased")
        check(link("passband://draft/7?mailbox=")?.mailbox == nil, "an empty mailbox is none")

        // Anything that is not exactly a draft id is not a draft link.
        for bad in [
            "passband://pair?server=https://x.example&code=ABCD",
            "passband://draft",
            "passband://draft/",
            "passband://draft/0",
            "passband://draft/-3",
            "passband://draft/12/send",
            "passband://draft/12abc",
            "passband://draft/1e3",
            "passband://draft/9999999999999",
            "passband://draft/%31%32",
            "passband://drafts/12",
            "https://draft/12",
            "passband://draft/١٢",
        ] {
            check(link(bad) == nil, "\(bad) must not parse")
        }

        // The stream frame.
        check(AgentOpenSignal.parse(#"{"open_draft":12}"#)?.open_draft == 12, "the signal is read")
        for bad in [
            #"{"open_draft":0}"#, #"{"open_draft":-1}"#, #"{"open_draft":"12"}"#, #"{"open_draft":1.5}"#,
            #"{"id":4,"kind":"urgent"}"#, "", "open_draft",
        ] {
            check(AgentOpenSignal.parse(bad) == nil, "\(bad) is not a signal")
        }
        print("ok: \(checks) agent draft link checks passed")
    }
}
