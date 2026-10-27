// WHERE THE RING LIVES: device-local, per ACCOUNT, and nowhere near the daemon.
// The same shape as ThreadStyleLedger and AuthDecisions, for the same two
// reasons — there is no server field for this, and the thing being remembered
// belongs to one mailbox. A query typed at the work account has no business
// surfacing under the personal one's search field, which is exactly what a
// single global list would do.
//
// The FOLD is `RecentSearchRing`, which is pure and asserted in test.sh; this
// file is only its storage and its account scoping. Nothing here decides what a
// ring should contain.

import Foundation
import SwiftUI

@MainActor
@Observable
final class RecentSearchStore {
    static let shared = RecentSearchStore()

    /// Base name of the stored list; the live key is this scoped to the active
    /// account. nil when there is no live account — nothing to read, nowhere to
    /// write, which is the Connect gate's state.
    private static let keyBase = "passband.search.recents"
    private static var key: String? {
        guard let id = AccountManager.shared.activeId else { return nil }
        return AccountIndex.scopedKey(keyBase, id)
    }

    /// Newest first. Read straight from views: @Observable means the empty
    /// state re-renders when a search is remembered under it.
    private(set) var queries: [String] = []

    /// The ring read back as searches and questions (`RecentEntry`), which is
    /// what both shells draw.
    var entries: [RecentEntry] { queries.map(RecentEntry.init(stored:)) }

    /// The ring alone: QuestionChats prunes against this store, so touching
    /// it from inside this one's first initialisation would recurse.
    private init() { queries = Self.stored() }

    private static func stored() -> [String] {
        key.flatMap { UserDefaults.standard.stringArray(forKey: $0) } ?? []
    }

    /// Re-read for whatever account is live NOW. Called by the account switch
    /// AFTER the new id is committed, beside the other ledgers: this key is
    /// derived from that id, so reloading any earlier re-reads the account that
    /// just went away.
    func reload() {
        queries = Self.stored()
        // The chats are scoped the same way and change account with the ring.
        QuestionChats.shared.reload()
    }

    /// Remember a query the reader has ACTED on. Callers pass the term the hits
    /// on screen were fetched for, never the live field text — see the panel's
    /// `remember()` and docs/SEARCH.md §4.2.
    func record(_ query: String) {
        guard let key = Self.key else { return }
        let next = RecentSearchRing.adding(query, to: queries)
        // Nothing moved: the ring refused it, or it is already the newest.
        // Skipping the write keeps re-opening the same search from touching
        // disk on every hit.
        guard next != queries else { return }
        queries = next
        UserDefaults.standard.set(next, forKey: key)
        // A question pushed off the end takes its saved chat with it.
        QuestionChats.shared.prune()
    }

    /// Put a submitted search in the ring, which is the one rule both shells
    /// follow: SUBMITTED MEANS FETCHED AND ACTED ON. Callers are the gestures
    /// that act on results (opening a hit on either shell, expanding the Mac's
    /// panel); this decides what is recorded. THE FETCHED TERM, never the live
    /// field text: mid-edit those are two different strings, and only one was
    /// answered (docs/SEARCH.md §4.2). A nil fetched query (the fetch failed,
    /// or the tap beat it home) is not a search anybody has seen the results
    /// of, so nothing is written.
    func remember(_ session: SearchSession) {
        guard let term = session.fetchedQuery else { return }
        record(term)
    }

    /// Put a question the agent was asked from the search field in the ring:
    /// its FIRST message, the words the conversation began with. Called once
    /// per conversation, when those words actually go to the agent; the
    /// narrowings typed after it are the same question and are not lines.
    func rememberQuestion(_ text: String) {
        let words = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !words.isEmpty else { return }
        record(RecentEntry(kind: .question, text: words).stored)
    }

    /// Forget the lot. Search terms are the reader's own words about their own
    /// mail, so the list that keeps them has to have a way out that is not
    /// "delete the app" — the button is in the empty state's own header, beside
    /// what it clears.
    func clear() {
        queries = []
        // The chats behind the questions are the same words and more.
        QuestionChats.shared.clear()
        guard let key = Self.key else { return }
        UserDefaults.standard.removeObject(forKey: key)
    }
}
