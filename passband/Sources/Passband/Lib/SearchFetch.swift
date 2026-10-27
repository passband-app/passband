// THE SEARCH FIELD'S FETCH, ONE COPY FOR BOTH SHELLS. The Mac's side panel and
// the phone's Search tab ask the daemon the same question the same way, and
// they used to do it from two hand-ported copies — which is how the phone kept
// passing no `mode`, no `partial` and raw snippets for weeks after the Mac had
// moved on (docs/SEARCH.md §4). What a panel search IS lives here; what each
// shell draws with the answer stays in its own view.
//
// Deliberately NOT in SearchPreview.swift, which test.sh compiles alone
// against its own suite and so cannot reach SearchHit or the client.

import Foundation

enum SearchFetch {
    /// Long enough that a normal typing run makes one request, short enough
    /// that a pause feels like an answer rather than a wait.
    static let debounce = Duration.milliseconds(220)

    /// One page of hits. Wide enough to scroll, and the cursor brings the rest.
    static let pageSize = 50

    /// One page for the field, with each hit's excerpt already cleaned for
    /// display.
    ///
    /// PARTIAL, because this fires while somebody is still typing: the trailing
    /// token is matched as a prefix, so "wif" finds "wifi" instead of nothing.
    /// The MODE follows the reader's "include related" preference rather than
    /// being left to the daemon, so turning it off really does mean keyword
    /// matches only.
    ///
    /// THE ORDER IS THE RANKING, with no status split, on a daemon that
    /// advertises it (`AppStore.searchPlainOrder`): done mail is not pushed
    /// below unfinished mail, and `include_sent` keeps the owner's sent mail
    /// that the grouped ask used to bring in. An older daemon gets the old ask,
    /// unfinished mail first, because there that flag is what keeps sent mail
    /// in a keyword search; the Done / Not done headers are drawn from that.
    /// `SearchOrder.ask` holds the rule, so test.sh can reach it.
    ///
    /// A LISTING (`FromOperator.isListing`: operators and no words, which is
    /// what picking a sender writes) is never grouped on either daemon. Nothing
    /// is ranked there, the reader asked for one person's mail in order, and
    /// splitting it by status would put last year's unfinished mail above this
    /// morning's.
    ///
    /// A cursor is an offset into one ranking, so a caller paging passes the
    /// sort, mode and order the hits on screen were fetched under, never the
    /// live preferences. `plainOrder` nil reads `AppStore.searchPlainOrder`
    /// live, which is only right for a first page; a caller that keeps the
    /// value (`SearchSession.fetchedPlainOrder`) reads it first and passes it.
    static func page(
        _ term: String, cursor: String? = nil, sort: SearchSortChoice?, related: Bool,
        plainOrder: Bool? = nil
    ) async throws -> SearchPage {
        let plain: Bool
        if let plainOrder { plain = plainOrder } else { plain = await AppStore.shared.searchPlainOrder }
        let order = SearchOrder.ask(term, plainOrder: plain)
        var page = try await APIClient.shared.search(
            term, limit: pageSize, cursor: cursor, mode: related ? .hybrid : .keyword, sort: sort,
            partial: true, unfinishedFirst: order.unfinishedFirst, includeSent: order.includeSent)
        // Prepared once here rather than per render: the cleaner runs a regex
        // over every URL in the excerpt, and a row redraws far more often than
        // a page arrives.
        page.items = page.items.map { hit in
            var hit = hit
            hit.displaySnippet = SearchPreview.clean(hit.snippet)
            return hit
        }
        return page
    }
}

extension SearchFetch {
    /// How many senders ride above the hits. A short list on purpose: this is
    /// an offer at the top of the results, and the results are still the point.
    static let senderRows = 3

    /// Same as the `from:` menu's: one request per typing run.
    private static let senderDebounce = Duration.milliseconds(120)

    /// The senders to offer above the hits for what is in the field, or nil
    /// when this task was superseded and must write nothing. Empty when the
    /// query is not a sender's to answer (`FromOperator.senderFragment`), and
    /// empty on a failed fetch: the offer is a convenience, the search under it
    /// reports its own errors.
    static func senders(matching query: String) async -> [SenderHit]? {
        guard let fragment = FromOperator.senderFragment(in: query) else { return [] }
        try? await Task.sleep(for: senderDebounce)
        guard !Task.isCancelled else { return nil }
        let found = (try? await APIClient.shared.senders(fragment, limit: senderRows)) ?? []
        return Task.isCancelled ? nil : found
    }

    /// The fetched senders that still answer the live field. The fetch trails
    /// the typing by a debounce and a round trip, and a row the last keystroke
    /// ruled out must not sit there armed in the meantime.
    static func senders(_ fetched: [SenderHit], stillMatching query: String) -> [SenderHit] {
        guard let fragment = FromOperator.senderFragment(in: query) else { return [] }
        return fetched.filter {
            FromOperator.matches(addr: $0.addr, name: $0.display_name, fragment: fragment)
        }
    }
}

extension SearchSession {
    /// The hits on screen are a listing: one newest-first run, not a ranking
    /// split by status. Read from the query they were FETCHED for, because
    /// mid-edit the field is already about something else.
    var showsListing: Bool {
        fetchedQuery.map(FromOperator.isListing) ?? false
    }

    /// THE HITS ON SCREEN ARE THE ANSWER to `term` under these rules: same
    /// words, same sort, same mode, same plain-or-grouped order (a daemon
    /// that starts advertising plain order mid-session re-fetches), and no
    /// revision since. Both halves of the
    /// panels' contract read it — "no matches" may only be said when this is
    /// true, and a fetch is skipped only when it already is, which is what lets
    /// a reopened panel or a tab hop resume without a refetch and a flash.
    ///
    /// A failed fetch and an empty field both clear `fetchedQuery`, so neither
    /// an error nor a question nobody asked can pass for an answer.
    func answers(_ term: String, sort: SearchSortChoice, related: Bool, plainOrder: Bool) -> Bool {
        fetchedQuery == term && fetchedSort == sort && fetchedRelated == related
            && fetchedPlainOrder == plainOrder && fetchedRevision == revision
    }
}
