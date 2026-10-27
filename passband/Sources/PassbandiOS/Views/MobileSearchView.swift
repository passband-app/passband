// THE SEARCH TAB. Full-text search over everything the daemon has ingested,
// with the same contract the Mac's side panel has: 220ms debounce, no refetch
// for a term whose results are already on screen, and pages appended as the
// last row comes into view. All of that state lives in `store.search` rather
// than here, which is what lets a tab hop leave the results exactly where they
// were — a phone switches surfaces constantly, and a search that reset every
// time you glanced at something else would be a search nobody finishes.
//
// AND IT IS ALSO THE AGENT'S FIELD. A phone has one text field per screen, so
// the field a person reaches for when they want to know something is this one,
// whether what they type is "delta receipt" or "when does my flight leave".
//
// THE DEEPER-SEARCH LANE, THE MAC'S RULE AND NOTHING ELSE (docs/SEARCH.md §5,
// §6). Every settled query is judged by `SearchIntent.classify` with the daemon's diagnostics for
// exactly those words, and `DeeperSearchPolicy.settled` decides what that
// verdict may do, the same two calls and the same order as `SidePanel.judge`.
// A question-shaped query (five words, a trailing "?", a question word, "my"),
// or three-plus words no one message carries, mounts `DeeperSearchBand` above
// the hits: under Automatic the lane starts by itself, under On request the
// band offers a button.
//
// THE "ASK THE AGENT" ROW IS THE MAC PANEL'S ⌘↩ BUTTON (`askAgentButton`):
// before a conversation it asks the lane (`requestDeeperSearch`, so the answer
// lands in the band above the hits), and once there is one it opens that
// conversation in the chat. Off in Settings, it does nothing, as on the Mac.
// There is no space-counting rule: the phone used to hand any five-word query
// to the chat, and the Mac never did. One field, one behaviour, both shells.
//
// WHAT IT SHARES WITH THE MAC'S PANEL, and does not re-implement: the fetch
// itself (`SearchFetch`: partial matching, the unfinished-first order and the
// cleaned excerpts), the "is this the answer"
// test (`SearchSession.answers`), and the recent-searches ring with its one
// recording rule (`RecentSearchStore.remember`). What is drawn with them is the
// phone's own: one column, rows sized for a thumb, the system search field.
//
// WHAT THE MAC'S PANEL HAS THAT THIS DOES NOT: the j/k cursor, the armed row,
// the Enter-to-expand fullscreen mode, the `from:` menu and the match
// highlighting. The first three are keyboard concepts with no thumb equivalent
// (a tap opens the mail, and that is the whole interaction), and
// `store.search.index` / `.expanded` are simply fields this surface never
// writes. The sender rows above the hits ARE here: typing a name or an address
// offers who it could be, and a tap lists that sender's mail newest first.

import SwiftUI

struct MobileSearchView: View {
    @Environment(AppStore.self) private var store
    @Environment(Prefs.self) private var prefs
    /// This tab's stack, as the shell knows it: `active` is whether the search
    /// tab is the one on screen, which is this shell's "panel open".
    @Environment(ReaderTab.self) private var readerTab

    /// THE MAC'S SETTING, read the same way: `prefs.searchIncludeRelated`,
    /// hidden and off by default, so the panel types in keyword mode, keeps
    /// embedding off the first-result path, and gets meaning back from the
    /// deeper-search lane (docs/SEARCH.md). This used to be pinned to hybrid,
    /// from when the phone had no lane; it has the Mac's now.
    private var related: Bool { prefs.searchIncludeRelated }

    @State private var loading = false
    /// A page append is in flight. SEPARATE from `loading`: that one blanks the
    /// list behind "searching…", and an append must leave the read hits alone.
    @State private var loadingMore = false

    /// The question this surface has pushed the agent for. Identity-per-ask
    /// rather than the string itself, so asking the same words twice in one
    /// session pushes twice instead of silently doing nothing the second time.
    @State private var ask: AgentAsk?

    /// The sender directory's answer to the words in the field, as last
    /// fetched. Drawn through `senders`, which drops the rows a newer keystroke
    /// has ruled out.
    @State private var fetchedSenders: [SenderHit] = []

    /// The senders offered above the hits: who the words could be naming.
    private var senders: [SenderHit] {
        SearchFetch.senders(fetchedSenders, stillMatching: store.search.query)
    }

    /// How many hits get their thread pulled ahead of a tap. The head of the
    /// page only: search results are read and chosen from, not swept, so the
    /// rest can wait for a real tap, and warming all fifty would be a stampede
    /// for one open.
    private static let warmCount = 5

    /// What the field currently holds, trimmed. Read from the store rather than
    /// mirrored into local state, exactly as the Mac's SearchView reads it: a
    /// local copy would have to be synced back on appear, and a re-mounted view
    /// whose empty mirror won the race against that sync would clear a session
    /// that is still on screen.
    private var term: String { store.search.query.trimmed }

    /// THERE IS AN ANSWER ON SCREEN for the question on screen, by the Mac's
    /// own test. What the no-matches copy is allowed to key on — emptiness
    /// alone means "not back yet" while the field is mid-edit, and
    /// `fetchedQuery != nil` alone means only that SOME query was once
    /// answered.
    private var answered: Bool {
        store.search.answers(term, sort: prefs.searchSort, related: related)
    }

    /// The remembered queries, newest first. Read through the store rather than
    /// copied, so a search remembered by opening a hit is already in the list
    /// when the reader comes back and clears the field.
    private var recents: [RecentEntry] { RecentSearchStore.shared.entries }

    /// The resting tab has history to offer. Keyed on the FIELD being blank,
    /// the Mac's rule: mid-edit the hits belong to the previous query, and old
    /// searches flashing over live results on every backspace is exactly the
    /// flicker `answered` exists to prevent (docs/SEARCH.md §4.2).
    private var showingRecents: Bool {
        term.isEmpty && !recents.isEmpty
    }

    /// The Mac panel's own test (`SidePanel.bandMounted`): once a query has
    /// been judged deeper, or a lane is going, and never under `off`.
    private var bandMounted: Bool {
        guard prefs.deeperSearch != .off, !term.isEmpty else { return false }
        return store.search.laneStarted || store.search.lastVerdict?.isDeeper == true
    }

    /// What the explainer promises about questions, which the setting decides.
    private var questionLine: String {
        switch prefs.deeperSearch {
        case .off: "A few words find a message. Turn on deeper search in Account to let the agent answer questions."
        case .automatic: "A few words find a message. Ask a question and the agent reads your mail to answer it."
        case .onRequest: "A few words find a message. Ask a question and the agent offers to answer it."
        }
    }

    var body: some View {
        @Bindable var store = store

        VStack(spacing: 0) {
            content
        }
            .background(Palette.canvas.ignoresSafeArea())
            .navigationTitle("Search")
            .navigationBarTitleDisplayMode(.inline)
            // Bound STRAIGHT to the store, which is what makes a tab hop free:
            // the field, the hits and the cursor all come back exactly as they
            // were left, because none of them were ever this view's to lose.
            .searchable(text: $store.search.query, prompt: "Search or ask")
            // SwiftUI cancels this on the next edit, which IS the debounce.
            // Keyed on exactly what the Mac's panel keys on: the order, the
            // mode and the session's revision each change the answer, so
            // flipping one has to re-rank what is on screen rather than wait
            // for the next keystroke. An array because `task(id:)` needs one
            // Equatable value.
            .task(id: [store.search.query, prefs.searchSort.rawValue,
                       String(store.search.revision)]) {
                await runSearch()
            }
            #if DEBUG
                // `--fixture-scene recents-restore`: the tap on the history
                // question, played once its chat has been staged.
                .task {
                    guard FixtureStage.on, let question = FixtureStage.tapsHistoryQuestion
                    else { return }
                    for _ in 0..<40 {
                        if QuestionChats.shared.snapshot(forQuestion: question) != nil {
                            reopen(question)
                            return
                        }
                        try? await Task.sleep(for: .milliseconds(250))
                    }
                }
            #endif
            .task(id: store.search.query) {
                guard let found = await SearchFetch.senders(matching: store.search.query) else { return }
                fetchedSenders = found
            }
            // The chat is a PUSH, not a tab: back lands on the results that were
            // underneath the question, and the transcript survives either way
            // because the session lives on the store.
            .navigationDestination(item: $ask) { ask in
                MobileAgentView(initialQuestion: ask.text)
            }
            // CLEARING THE FIELD ENDS THE CONVERSATION, as it does in the Mac
            // panel: empty words are not a narrowing of anything.
            .onChange(of: store.search.query) { _, query in
                if query.trimmed.isEmpty { store.resetSearchLane() }
            }
            // LEAVING THE TAB IS CLOSING THE PANEL (`AppStore.closeSide`):
            // the lane is HELD, not torn down, and coming back picks it up
            // where it stopped (`openSearch`'s resume). Keyed on the tab rather
            // than on this view's appearance, because opening a hit or pushing
            // the chat also takes this view off screen, and neither is the
            // reader walking away from the search.
            .onChange(of: readerTab.active) { _, active in
                if active {
                    store.searchLane.resume()
                } else {
                    if store.preparingSearchEvidence { store.resetSearchLane(keepingVerdict: true) }
                    store.searchLane.pause()
                }
            }
            // OFF STOPS A RUNNING LANE, the moment it is picked (the Mac's
            // ShellWatchers). Watched here because the picker is on the
            // Account page, over this tab, which stays mounted under it.
            .onChange(of: prefs.deeperSearch) { _, choice in
                let move = DeeperSearchPolicy.preferenceChanged(
                    to: choice, laneStarted: store.search.laneStarted)
                if move == .stop { store.resetSearchLane(keepingVerdict: true) }
            }
    }

    /// The lane's answer, drawn by the Mac's own band, AS THE FIRST ROW OF THE
    /// RESULTS: as tall as the answer wants, and the screen scrolls past it to
    /// the hits rather than the band scrolling inside a cap of its own.
    private var band: some View {
        DeeperSearchBand(expanded: false, openInChat: continueInChat)
            .padding(.top, 8)
    }

    /// The query's words as the daemon parsed them: what a hit is lit with
    /// when the daemon does not say which words matched (the Mac's `terms`).
    private var terms: [String] {
        store.search.diagnostics?.terms.map(\.text) ?? []
    }

    /// What sits under the agent's answer is the plain search, and says so:
    /// the small caps every list header here uses, with a rule run out from
    /// the label to the edge, so the agent's reading and the mail below it
    /// never read as one block.
    private var resultsHeader: some View {
        HStack(spacing: 10) {
            Text("All results")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.uppercase)
                .tracking(0.6)
                .fixedSize()
                .accessibilityAddTraits(.isHeader)
            Rectangle()
                .fill(Palette.hairlineStrong)
                .frame(height: 1)
                .accessibilityHidden(true)
        }
        .padding(.top, 18)
        .padding(.bottom, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - the three shapes

    @ViewBuilder
    private var content: some View {
        if showingRecents {
            recentList
        } else if store.search.hits.isEmpty {
            // The ask row rides above the empty states too, and especially
            // above "No matches" — that is the exact moment the words are worth
            // handing to something that can go look.
            if bandMounted {
                // The agent's answer first, at its full height, then the
                // search under its own header, all in one scroll. No ask row
                // while the band is up: the band is already the agent here.
                ScrollView {
                    VStack(spacing: 0) {
                        band
                        resultsHeader.padding(.horizontal, 16)
                        emptySenders
                        emptyState.frame(minHeight: 220)
                    }
                }
                .scrollDismissesKeyboard(.immediately)
            } else {
                VStack(spacing: 0) {
                    if !term.isEmpty {
                        askRow.padding(.horizontal, 16)
                        Divider().overlay(Palette.hairline)
                    }
                    emptySenders
                    emptyState
                }
            }
        } else {
            hitList
        }
    }

    /// Above "No matches" too: the name may be in nobody's subject or body
    /// and still be somebody who writes.
    private var emptySenders: some View {
        ForEach(senders) { sender in
            SenderRow(sender: sender) { show(sender) }
                .padding(.horizontal, 16)
            Divider().overlay(Palette.hairline)
        }
    }

    private var hitList: some View {
        let hits = store.search.hits
        return List {
            // Guarded on the term rather than assumed from the hits: clearing
            // the field empties them one render later, and an "Ask the agent"
            // row quoting nothing at all is a row in that gap.
            if bandMounted {
                // No row inset: the band carries its own side margin, and
                // inset twice it sat a step inside the header and the hits.
                band
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
                    .listRowInsets(EdgeInsets())
                resultsHeader.plainRow()
            } else if !term.isEmpty {
                askRow.plainRow()
            }
            ForEach(senders) { sender in
                SenderRow(sender: sender) { show(sender) }.plainRow()
            }
            // A failed refresh does NOT drop the hits already read, so the
            // error has to ride above them rather than replace them — see
            // the empty state, which carries the same line when there is
            // nothing underneath it to keep.
            //
            // A search in flight outranks the last one's error: typing again
            // after a failure should read "searching…", not the old failure.
            if loading {
                // A refetch leaves the read hits on screen — replacing them
                // with a word would blank the list on every keystroke past
                // the debounce — so the new search says so from above them.
                BandNote("searching…").plainRow()
            } else if let error = store.search.error {
                BandNote(error).plainRow()
            }
            ForEach(Array(hits.enumerated()), id: \.element.id) { i, hit in
                // A LISTING (one sender's mail, operators and no words) is
                // one run in date order under one header saying so.
                if store.search.showsListing {
                    if i == 0 { StatusHeader(title: "Newest first").plainRow() }
                }
                // NO DONE / NOT DONE HEADERS, on either shell (the Mac's
                // `header(above:)` agrees): results are one ranked list. An
                // older daemon still sends unfinished mail first until it is
                // rolled to plain order (`AppStore.searchPlainOrder`); the
                // labels do not wait for that.
                HitRow(hit: hit, terms: terms) {
                    // OPENING A HIT IS WHERE A QUERY STOPS BEING KEYSTROKES,
                    // on this shell as on the Mac: the ring's one rule, called
                    // from the one gesture here that acts on the results.
                    RecentSearchStore.shared.remember(store.search)
                    store.openThread(hit.thread_id, from: .search)
                }
                    .plainRow()
                    // Reaching the last row IS the request for the next
                    // page. There is no "more" button: a thumb is already
                    // scrolling, and a button would be one more thing to
                    // reach for at the exact moment the intent is obvious.
                    .onAppear {
                        guard hit.id == hits.last?.id else { return }
                        Task { await loadMore() }
                    }
            }
            // Rows just stopping is indistinguishable from the end of the
            // results, so the append announces itself.
            if loadingMore {
                BandNote("loading more…").plainRow()
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .environment(\.defaultMinListRowHeight, 0)
        // A results list is read with the keyboard up; scrolling it is the
        // gesture that says the typing is done.
        .scrollDismissesKeyboard(.immediately)
    }

    /// THE RESTING TAB, once there is history: the last few searches the
    /// reader acted on, where the hits will go. A tap puts the words back in
    /// the FIELD and lets the debounced task run them, the Mac's verb for the
    /// same list, and nothing is opened: a remembered search is usually the
    /// first half of the next one.
    ///
    /// The explainer rides underneath rather than being replaced, because it
    /// is the only place the field's second job is stated, and a reader with
    /// history is not necessarily one who has read it.
    private var recentList: some View {
        List {
            HStack(alignment: .firstTextBaseline) {
                Text("Recent")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.accent)
                    .textCase(.uppercase)
                    .tracking(0.6)
                Spacer(minLength: 8)
                // Visible, not a long-press: these are the reader's own words
                // about their own mail, and the way out of keeping them should
                // be findable without knowing a gesture.
                Button("Clear") { RecentSearchStore.shared.clear() }
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Palette.inkFaint)
                    .buttonStyle(.plain)
                    .accessibilityLabel("Clear recent searches")
            }
            .padding(.top, 14)
            .padding(.bottom, 4)
            .plainRow()
            ForEach(Array(recents.enumerated()), id: \.offset) { _, entry in
                RecentRow(entry: entry) {
                    // A question goes back to the agent (its saved chat when
                    // there is one); a search back in the field. The Mac's
                    // recents do the same.
                    if entry.kind == .question {
                        reopen(entry.text)
                    } else {
                        store.search.query = entry.text
                    }
                }
                .plainRow()
            }
            Text(questionLine)
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkFaintest)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 14)
                .plainRow()
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .environment(\.defaultMinListRowHeight, 0)
        .scrollDismissesKeyboard(.immediately)
    }

    /// The agent's door, sitting where the best-matching email would. It shows
    /// the words back so it is obvious WHAT would be asked, and it sends on tap
    /// rather than filling a field: reaching past every hit for this row is
    /// already the deliberate act, and asking twice for one intention is the
    /// thing that made the old handoff feel like the app taking the wheel.
    /// The Mac's two verbs on one row: with a conversation going it opens it
    /// in the chat, and before one it asks the lane, whose answer the band
    /// draws above the hits. Disabled exactly where the Mac's button is.
    private var askRow: some View {
        let opens = store.searchLaneHasConversation
        let busy = store.searchLane.running || store.preparingSearchEvidence
        let enabled = opens
            ? (store.chatHoldsSearchLane || store.canExpandSearchLane) && !busy
            : DeeperSearchPolicy.canRequest(
                query: store.search.query, choice: prefs.deeperSearch, running: busy)
        return Button {
            if opens { continueInChat() } else { store.requestDeeperSearch() }
        } label: {
            HStack(spacing: 11) {
                Image(systemName: opens ? "arrow.up.left.and.arrow.down.right" : "sparkles")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                    .frame(width: 18)
                VStack(alignment: .leading, spacing: 2) {
                    Text(opens ? "Open in chat" : busy ? "Searching…" : "Ask the agent")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(Palette.ink)
                    // Off says how to turn it on, as the Mac's help line does.
                    Text(prefs.deeperSearch == .off ? "Turn on deeper search in Account" : "“\(term)”")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaint)
                        .lineLimit(1)
                }
                Spacer(minLength: 6)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Palette.inkFaintest)
            }
            .padding(.vertical, 11)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
    }

    /// The three things that can be true with nothing to show. Ordered by which
    /// the user most needs to hear: a failure first, then the honest "nothing
    /// matched", then the idle explainer that is the tab's resting state.
    @ViewBuilder
    private var emptyState: some View {
        VStack(spacing: 10) {
            if let error = store.search.error {
                Text(error)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.warn)
                    .multilineTextAlignment(.center)
            } else if loading {
                Text("searching…")
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaintest)
            } else if answered {
                Text("No matches.")
                    .font(Typo.serif(22, weight: .medium))
                    .foregroundStyle(Palette.ink)
                Text("Nothing in the ingested mail carries those words.")
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaint)
                    .multilineTextAlignment(.center)
            } else {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 28, weight: .light))
                    .foregroundStyle(Palette.inkFaintest)
                Text("Search your mail.")
                    .font(Typo.serif(24, weight: .medium))
                    .foregroundStyle(Palette.ink)
                // Says the second thing this field does, once, where a first
                // visit will read it. Nobody discovers a field with two jobs by
                // accident, and the rule is small enough to state outright.
                Text(questionLine)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaint)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: 300)
            }
        }
        .padding(.horizontal, 24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - one sender's mail

    /// List everything from one sender: the completed operator goes into the
    /// FIELD, the way a remembered search does, and the debounced task fetches
    /// it. The reader can see what was asked and narrow it.
    private func show(_ sender: SenderHit) {
        store.search.query = FromOperator.listing(of: sender.addr)
    }

    // MARK: - the agent

    /// THE BAND'S "CONTINUE IN CHAT", on a shell with no ⌘K bar: the chat
    /// takes a copy of the lane's conversation (`AssistantSession.adopt`, the
    /// Mac's `expandSearchLaneToChat` without the bar it opens) and is pushed
    /// with nothing to send, so a follow-up there is a real follow-up. Already
    /// holding this search, the chat is just shown, follow-ups and all.
    private func continueInChat() {
        if !store.chatHoldsSearchLane {
            guard store.canExpandSearchLane else { return }
            store.adoptSearchLaneIntoChat()
        }
        ask = AgentAsk(text: nil)
    }

    /// A QUESTION FROM HISTORY: the chat it had, pushed with nothing to send,
    /// the way "continue in chat" pushes one. With no saved chat the store has
    /// asked it again in the lane instead, which this screen already shows.
    private func reopen(_ question: String) {
        if store.openQuestionFromHistory(question) { ask = AgentAsk(text: nil) }
    }

    /// Keyword or question, for the query whose hits are now on screen: the
    /// Mac panel's `judge`, line for line, because the rule about spending is
    /// `DeeperSearchPolicy`'s and a rule spelled twice will disagree with
    /// itself. Only for words still in the field: a lane started for words the
    /// reader deleted would take the live ones as a "refinement".
    private func judge(_ query: String) {
        guard query == term else { return }
        let verdict = SearchIntent.classify(query: query, diagnostics: store.search.diagnostics)
        store.search.lastVerdict = verdict
        switch DeeperSearchPolicy.settled(
            verdict: verdict, choice: prefs.deeperSearch,
            laneStarted: store.search.laneStarted)
        {
        case .nothing:
            break
        case .stop:
            store.resetSearchLane(keepingVerdict: true)
        case .refine:
            store.refineDeeperSearch()
        case .start(let trigger):
            store.startDeeperSearch(trigger: trigger)
        }
    }

    // MARK: - fetching

    /// The Mac's SearchView contract, cancellation handling and all, because
    /// it is the store's rather than the panel's. The request itself is
    /// `SearchFetch`, shared; what is here is the phone's lifecycle around it.
    private func runSearch() async {
        let query = term
        // A bare operator (`from:`) is not a search yet: the daemon drops the
        // valueless token and refuses the empty query, and that refusal is not
        // something to show a reader halfway through typing a sender.
        guard !query.isEmpty, !FromOperator.awaitingValue(in: query) else {
            store.search.hits = []
            store.search.error = nil
            store.search.fetchedQuery = nil
            store.search.fetchedSort = nil
            store.search.diagnostics = nil
            store.search.nextCursor = nil
            loading = false
            return
        }
        // Already holding this term's results UNDER THESE RULES: coming back to
        // the tab must not re-fetch and flash, which is the point of parking
        // the session in the store. Same words under a different order or mode
        // is a different answer, so changing either in Settings re-ranks on the
        // next visit instead of silently doing nothing until the reader edits
        // their query. Read at fetch time, not captured on mount.
        let sort = prefs.searchSort
        let related = related
        let revision = store.search.revision
        // CLEARS `loading` on the way out: this is the path a cancelled
        // predecessor used to rely on someone else covering. The CURRENT task
        // owns the flag now; see the cancelled exits below.
        guard !store.search.answers(query, sort: sort, related: related) else {
            loading = false
            return
        }
        loading = true
        // Debounce: a fresh keystroke cancels this task before the request.
        try? await Task.sleep(for: SearchFetch.debounce)
        // A CANCELLED TASK WRITES NOTHING. Its replacement has already set
        // `loading = true` for the keystroke that superseded it, and the two
        // resumptions are not ordered — clearing the flag here is how
        // "searching…" blinked off mid-type and let the empty state through.
        guard !Task.isCancelled else { return }
        do {
            let page = try await SearchFetch.page(query, sort: sort, related: related)
            // Re-check after the await, same as the catch does: a superseded
            // task landing late must not stamp `fetchedQuery` with a term that
            // is no longer in the field — the `answers` guard above would then
            // refuse to fetch the term that IS.
            guard !Task.isCancelled, term == query, revision == store.search.revision,
                  related == related else { return }
            store.search.hits = page.items
            store.search.diagnostics = page.diagnostics
            store.search.nextCursor = page.next_cursor
            store.search.error = nil
            store.search.fetchedQuery = query
            store.search.fetchedSort = sort
            store.search.fetchedRelated = related
            store.search.fetchedRevision = revision
            for hit in page.items.prefix(Self.warmCount) {
                ThreadPrefetch.shared.prefetch(hit.thread_id)
            }
            // After the fetch, never mid-keystroke: past the debounce, with
            // the daemon's diagnostics for exactly these words in hand.
            judge(query)
        } catch {
            // Cancellation surfaces here too (URLError.cancelled mid-request):
            // that is a superseded task, not a failure, and writing an error
            // would stamp the NEW search's state with the old one's obituary.
            // Nor the flag — same reason as the debounce exit above.
            guard !Task.isCancelled else { return }
            store.search.error = errText(error, "search failed")
            // Leave `fetchedQuery` nil so returning RETRIES rather than
            // resurrecting a stale error over stale hits, and drop the cursor
            // with it: it belongs to a page set this view is no longer showing.
            store.search.fetchedQuery = nil
            store.search.fetchedSort = nil
            store.search.diagnostics = nil
            store.search.nextCursor = nil
        }
        loading = false
    }

    /// Append the page after the one on screen. Cursors are only meaningful
    /// beside the term they were issued for, so this refuses to run while the
    /// field is mid-edit (`query != fetchedQuery`) and re-checks after the
    /// await — a query that turned over in flight would otherwise splice two
    /// different searches into one list.
    private func loadMore() async {
        guard answered, !loading, !loadingMore, let cursor = store.search.nextCursor else { return }
        let query = term
        guard !query.isEmpty, query == store.search.fetchedQuery else { return }
        // The cursor is an OFFSET INTO ONE RANKING, so the page after it is
        // asked for under the sort and mode the hits on screen were ranked by,
        // not under whatever the preferences say now.
        let sort = store.search.fetchedSort
        let related = store.search.fetchedRelated ?? false
        let revision = store.search.revision
        loadingMore = true
        defer { loadingMore = false }
        do {
            let page = try await SearchFetch.page(query, cursor: cursor, sort: sort, related: related)
            guard query == store.search.fetchedQuery, store.search.nextCursor == cursor,
                  sort == store.search.fetchedSort, related == store.search.fetchedRelated,
                  revision == store.search.revision else {
                return
            }
            // Deduped because the cursor is an OFFSET: mail arriving between two
            // pages shifts the window, and a repeated id is a normal outcome
            // rather than a server bug. Two rows with one id would also break
            // the ForEach.
            var seen = Set(store.search.hits.map(\.id))
            for hit in page.items where seen.insert(hit.id).inserted {
                store.search.hits.append(hit)
            }
            store.search.nextCursor = page.next_cursor
        } catch {
            // Keep the cursor and stay silent: the hits already read are worth
            // more than an error line, and scrolling the last row back into
            // view retries. The field reports failures for the search itself.
        }
    }
}

// MARK: - one ask

/// One push of the agent, with the words that caused it. Identity is the PUSH
/// rather than the text so `navigationDestination(item:)` treats "ask this
/// again" as a new destination — two identical questions are two questions.
private struct AgentAsk: Hashable, Identifiable {
    let id = UUID()
    /// nil is the band handing over a conversation already adopted: the push
    /// shows it and sends nothing.
    let text: String?
}

// MARK: - one hit

/// SUBJECT FIRST, and that is the phone's own order: the Mac leads with the
/// sender because its rows sit in a 300pt strip beside a reader that will show
/// you the rest, and here the row is the whole decision. Who it is from and how
/// old it is fit on one micro line under it, and the snippet gets two lines
/// because a snippet cut to one is just a longer subject.
private struct HitRow: View {
    let hit: SearchHit
    /// The query's words, for a daemon too old to say which ones matched.
    let terms: [String]
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            VStack(alignment: .leading, spacing: 3) {
                // The matched words lit the Mac's way (`SearchHighlight`).
                Text(SearchHighlight.paint(hit.subject, matches: hit.subject_matches ?? terms))
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                HStack(spacing: 6) {
                    Text(hit.from_name ?? hit.from_addr)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .lineLimit(1)
                    Text(Fmt.relAge(hit.received_at))
                        .font(Typo.num(10, weight: .medium))
                        .foregroundStyle(Palette.inkFaintest)
                }
                // The CLEANED excerpt, prepared once by `SearchFetch`: tracking
                // links shown as their readable destination, footers dropped.
                // The raw one only for a hit that somehow skipped that pass.
                Text(SearchHighlight.paint(
                    hit.displaySnippet ?? hit.snippet, matches: hit.snippet_matches ?? terms))
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaintest)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
            }
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - list headers

/// A small-caps list header ("Newest first" over a sender's listing).
private struct StatusHeader: View {
    let title: String

    init(title: String) {
        self.title = title
    }

    var body: some View {
        Text(title)
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaint)
            .textCase(.uppercase)
            .tracking(0.6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 14)
            .padding(.bottom, 2)
            .accessibilityAddTraits(.isHeader)
    }
}

// MARK: - one sender

/// One sender offered above the hits, shaped like the ask row it sits under:
/// an icon, two lines, a chevron. The strings are email-derived and rendered
/// as Text, never as markup.
private struct SenderRow: View {
    let sender: SenderHit
    let onShow: () -> Void

    private var name: String? {
        sender.display_name.flatMap { $0.isEmpty ? nil : $0 }
    }

    private var count: String {
        sender.msg_count == 1 ? "1 email" : "\(sender.msg_count) emails"
    }

    var body: some View {
        Button(action: onShow) {
            HStack(spacing: 11) {
                Image(systemName: "person.crop.circle")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Palette.inkFaint)
                    .frame(width: 18)
                VStack(alignment: .leading, spacing: 2) {
                    Text(name ?? sender.addr)
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(name == nil ? count : "\(sender.addr) · \(count)")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaint)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 6)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Palette.inkFaintest)
            }
            .padding(.vertical, 11)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint("Shows all mail from this sender, newest first")
    }
}

// MARK: - one remembered search

/// One row of the recents list, as tall as a hit row's tap target so the two
/// lists feel like one surface. The query is the reader's own words rendered
/// as Text, never as markup, and it has been through UserDefaults.
private struct RecentRow: View {
    let entry: RecentEntry
    let onRun: () -> Void

    private var question: Bool { entry.kind == .question }

    var body: some View {
        Button(action: onRun) {
            HStack(spacing: 11) {
                // A question the agent was asked wears a chat bubble; a search
                // the clock. The line is the words either way.
                Image(systemName: question ? "bubble.left" : "clock.arrow.circlepath")
                    .font(.system(size: 14))
                    .foregroundStyle(question ? Palette.accent : Palette.inkFaintest)
                    .frame(width: 18)
                Text(entry.text)
                    .font(.system(size: 15))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 6)
                Image(systemName: "arrow.up.left")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Palette.inkFaintest)
            }
            .padding(.vertical, 12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(question ? "Question: \(entry.text)" : entry.text)
        .accessibilityHint(question ? "Asks the agent again" : "Searches for this again")
    }
}
