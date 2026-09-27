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
    /// matches only. Unfinished mail ranks first, which is what the Done / Not
    /// done headers in both shells are drawn from.
    ///
    /// A cursor is an offset into one ranking, so a caller paging passes the
    /// sort and mode the hits on screen were fetched under, never the live
    /// preferences.
    static func page(
        _ term: String, cursor: String? = nil, sort: SearchSortChoice?, related: Bool
    ) async throws -> SearchPage {
        var page = try await APIClient.shared.search(
            term, limit: pageSize, cursor: cursor, mode: related ? .hybrid : .keyword, sort: sort,
            partial: true, unfinishedFirst: true)
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

extension SearchSession {
    /// THE HITS ON SCREEN ARE THE ANSWER to `term` under these rules: same
    /// words, same order, same mode, and no revision since. Both halves of the
    /// panels' contract read it — "no matches" may only be said when this is
    /// true, and a fetch is skipped only when it already is, which is what lets
    /// a reopened panel or a tab hop resume without a refetch and a flash.
    ///
    /// A failed fetch and an empty field both clear `fetchedQuery`, so neither
    /// an error nor a question nobody asked can pass for an answer.
    func answers(_ term: String, sort: SearchSortChoice, related: Bool) -> Bool {
        fetchedQuery == term && fetchedSort == sort && fetchedRelated == related
            && fetchedRevision == revision
    }
}
