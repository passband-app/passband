// Browse-all + search as a right-hand glass panel. It owns the modal KeyContext
// and Esc-to-close; inner views register their list keys into that same context
// and must not push a second one. Mounted only while a side view is open —
// pushing the modal context unconditionally would gate out the whole "list"
// keymap forever. The thread viewer layers above it, inset by sidePanelWidth.

import SwiftUI

struct SidePanel: View {
    @Environment(AppStore.self) private var store

    /// Search opens wide; browse remains a sidebar.
    private var expanded: Bool {
        store.sideView == .search && store.search.expanded
    }

    var body: some View {
        HStack(spacing: 0) {
            Spacer(minLength: 0)
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    Text(store.sideView.title)
                        .font(Typo.ui(14, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                    Spacer()
                    HStack(spacing: Space.s4) {
                        Kbd("Esc")
                        Text("close").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                    }
                }
                // As a strip this header sits on the window's right, nowhere
                // near the traffic lights. EXPANDED it spans the whole window
                // and covers the rail, so its leading edge lands in the strip
                // the buttons own and the title draws underneath them.
                .padding(.leading, expanded ? TopBar.dotsClearance : Space.s16)
                .padding(.trailing, Space.s16)
                .padding(.vertical, Space.scaled(13))
                .overlay(alignment: .bottom) { Hairline() }

                Group {
                    switch store.sideView {
                    case .search: SearchView()
                    case .browse: BrowseView()
                    case .none: EmptyView()
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
            .frame(width: expanded ? nil : sidePanelWidth)
            .frame(maxWidth: expanded ? .infinity : nil, maxHeight: .infinity)
            .passbandGlass(.pane, cornerRadius: 0, tint: Palette.glassTintStrong)
            .shadow(color: .black.opacity(0.24), radius: 40, x: -14)
        }
        .animation(.smooth(duration: 0.22), value: expanded)
        .keyContext(.modal)
        // Wide search is the default, so Escape closes it in one step.
        // The sidebar layout is retained for search beside an open email.
        .keyBindings(.modal, [
            KeyBinding("Escape", "close search") { store.closeSide() }
        ])
    }
}

// MARK: - search

/// Debounced search with wide results by default. Enter opens the selected
/// result, or the first settled result when none is selected; in the sidebar,
/// an unarmed Enter expands. Opening mail retains the sidebar beside the reader.
/// Query and result state live in the store so closing search can resume it.
struct SearchView: View {
    @Environment(AppStore.self) private var store
    @Environment(Prefs.self) private var prefs
    @State private var loading = false
    /// A page append is in flight. SEPARATE from `loading`: that one blanks the
    /// list behind "searching…", and an append must leave the read hits alone.
    @State private var loadingMore = false
    @FocusState private var focused: Bool
    /// The sender directory's answer to the plain words in the field, as last
    /// fetched. Drawn through `senders`, which drops the rows a newer keystroke
    /// has ruled out.
    @State private var fetchedSenders: [SenderHit] = []
    /// The armed sender row, -1 for none. BESIDE `store.search.index` rather
    /// than folded into it: that one is parked in the store and counted against
    /// the hits by everything that resumes a search, so the sender rows get an
    /// index of their own and `move` walks the two as one column.
    @State private var senderIndex = -1

    /// The senders offered above the hits: who the words in the field could be
    /// naming. Picking one lists everything they have sent, newest first.
    private var senders: [SenderHit] {
        SearchFetch.senders(fetchedSenders, stillMatching: store.search.query)
    }

    /// The terms the on-screen hits were actually fetched for — the live query
    /// can be mid-edit, and highlighting it would mark text the server never
    /// matched.
    private var terms: [String] {
        store.search.diagnostics?.terms.map(\.text) ?? []
    }

    /// The remembered queries, newest first. Read through the store rather than
    /// copied into `@State`, so a search remembered while the panel is open
    /// (opening a hit does exactly that) is on screen the moment the field is
    /// cleared again.
    private var recents: [String] { RecentSearchStore.shared.queries }

    /// THE EMPTY STATE HAS SOMETHING TO SAY: nothing has been typed, and there
    /// is history to offer. Keyed on the field being blank rather than on the
    /// hits being empty — mid-edit the hits belong to the previous query, and a
    /// list of old searches flashing over live results on every backspace is
    /// exactly the flicker `answered` exists to prevent (docs/SEARCH.md §4.2).
    private var showingRecents: Bool {
        store.search.query.trimmed.isEmpty && !recents.isEmpty
    }

    /// How many rows the arrows have to walk. ONE index arms both lists,
    /// because only one of them is ever on screen: hits need a query and
    /// recents need the absence of one.
    private var rowCount: Int {
        showingRecents ? recents.count : store.search.hits.count
    }

    /// THERE IS AN ANSWER ON SCREEN for the question on screen: the hits came
    /// back for exactly this query under exactly this order.
    ///
    /// What "no matches." is allowed to key on, and the fix for it flashing on
    /// every keystroke. Emptiness alone does not mean no matches — mid-edit the
    /// hits belong to the PREVIOUS query, and an empty list there means "not
    /// back yet". Nor does `!loading`: the flag is a race between the task
    /// starting for this keystroke and the cancelled one for the last, so it
    /// dips false between two characters even though nothing was answered.
    ///
    /// A failed fetch clears `fetchedQuery`, so an error is never mistaken for
    /// a nil result; an empty field clears it too, so the resting panel says
    /// nothing rather than "no matches." at a question nobody asked.
    private var answered: Bool {
        store.search.answers(
            store.search.query.trimmed, sort: prefs.searchSort, related: prefs.searchIncludeRelated)
    }

    var body: some View {
        @Bindable var store = store
        let expanded = store.search.expanded

        VStack(alignment: .leading, spacing: 0) {
            Field(label: "") {
                HStack(spacing: Space.s8) {
                    TextField("search mail…", text: $store.search.query)
                        .textFieldStyle(.plain)
                        .focused($focused)
                    // IN THE WELL, at its trailing edge: the wait belongs to the
                    // field, beside the words being waited on, rather than in a
                    // row of its own above the results.
                    //
                    // SPACE RESERVED, DOTS MOUNTED ONLY WHILE WAITING. Reserved,
                    // because arriving would re-lay the well and shove the text
                    // and caret leftward on every search. Mounted rather than
                    // merely faded, because the dots animate forever once they
                    // appear, and a loop running behind zero opacity is a loop
                    // that should not be running.
                    ZStack {
                        if loading { WaitDots().transition(.opacity) }
                    }
                    .frame(width: WaitDots.width)
                    .animation(.easeInOut(duration: 0.16), value: loading)
                }
            }
            .padding(.horizontal, Space.s16)
            .padding(.top, Space.s12)
            .padding(.bottom, Space.s8)

            // THE SENDER MENU, under the well, for exactly as long as the
            // trailing token is a `from:` operator being typed (see
            // FromOperator). Mounted by the fragment and the focus rather than
            // by a flag of its own, so a space, a finished address, or the
            // reader opening a hit all take it down without anybody having to
            // remember to; and mounted AFTER the field, so its arrows and Enter
            // register later than this panel's and win only while it is up.
            if focused, let fragment = FromOperator.fragment(in: store.search.query) {
                SenderSuggestions(query: $store.search.query, fragment: fragment)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, Space.s16)
                    .padding(.bottom, Space.s8)
            }

            HStack {
                // A listing has no ranking to choose between: it is date order
                // whatever the picker says, so the picker stands down rather
                // than offer a switch that does nothing.
                SearchSortPicker()
                    .disabled(store.search.showsListing)
                    .opacity(store.search.showsListing ? 0.4 : 1)
                Spacer(minLength: Space.s8)
                if expanded { askAgentButton }
            }
            .padding(.horizontal, Space.s16)
            .padding(.bottom, Space.s10)

            if !expanded {
                HStack { Spacer(); askAgentButton }
                    .padding(.horizontal, Space.s16)
                    .padding(.bottom, Space.s10)
            }

            if let error = store.search.error { BandNote(error) }
            if answered && store.search.hits.isEmpty { BandNote("no matches.") }

            // THE STRIP IS TOO NARROW FOR TWO COLUMNS (460pt), so there the
            // lane is a band ABOVE the hits; expanded, it becomes the left
            // column beside them and the results keep their reading width. Same
            // view either way — see DeeperSearchBand.
            //
            // ONE `results` CALL, IN ONE PLACE IN THIS TREE, and that is
            // structural rather than tidy: written as two branches of `if
            // expanded`, the two calls are different slots, so SwiftUI gives
            // them different identities and tears the whole ScrollView down and
            // rebuilds it on every Enter. The scroll offset went back to the
            // top, and a remount fires no `onChange(of: index)`, so a reader
            // thirty rows down who expanded landed at row one with their
            // selection off screen. Here only the band moves; the hits keep
            // their place because they never leave theirs.
            if bandMounted && !expanded {
                strippedBand
            }
            // WHERE THE HITS GO, and above the (empty) results scroller rather
            // than inside it: `results` keeps its one call site in this tree,
            // which is what stops SwiftUI re-identifying the ScrollView and
            // throwing the scroll offset away (see the note above).
            if showingRecents {
                RecentSearches(
                    queries: recents, armed: store.search.index,
                    onRun: { run($0) }, onClear: { clearRecents() }
                )
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, Space.s16)
            }
            HStack(alignment: .top, spacing: 0) {
                if bandMounted && expanded {
                    ScrollView {
                        DeeperSearchBand(expanded: true)
                            .padding(.leading, Space.s16)
                            .padding(.top, Space.s14)
                            .padding(.bottom, Space.s14)
                    }
                    .frame(minWidth: Typo.measure(320), idealWidth: Typo.measure(380), maxWidth: Typo.measure(440))
                }
                results(expanded: expanded)
            }
        }
        .keyBindings(.modal, bindings)
        // `/` mounts this panel mid-keystroke, and focus set in the pass that
        // mounts the field lands before the field is in the window, so the
        // caret never arrives. One turn later it does (SettingsView's editor
        // hit the same thing). The request counter covers `/` over a panel
        // that is already open, where there is no `onAppear` to fire.
        .onAppear { focusField() }
        .onChange(of: store.searchFocusRequest) { focusField() }
        .onChange(of: store.search.query) { _, query in
            store.search.index = -1
            senderIndex = -1
            if query.trimmed.isEmpty {
                store.resetSearchLane()
            }
        }
        // The reader steals focus while it is up. When it closes and this
        // panel is the surface again, typing must just work — without this the
        // arrows still move the selection but the keyboard is otherwise dead
        // until a mouse click, which reads as the panel being broken.
        .onChange(of: store.threadId) { _, threadId in
            if threadId == nil {
                focused = true
                store.search.expanded = true
            }
        }
        // The remembered query lands selected, so `/` serves both callers: arrow
        // down into the old results, or type to replace it.
        .onChange(of: focused) { _, on in
            guard on, !store.search.query.isEmpty else { return }
            Task { @MainActor in
                // Select-all through the responder chain has no UIKit twin worth
                // shimming; the iOS field selects its text a different way.
                #if os(macOS)
                    NSApp.sendAction(#selector(NSText.selectAll(_:)), to: nil, from: nil)
                #endif
            }
        }
        // KEYED ON THE SORT TOO, or flipping the order leaves the old ranking on
        // screen until the reader edits their query. An array because tuples do
        // not conform to Equatable and `task(id:)` needs one value.
        .task(id: [store.search.query, prefs.searchSort.rawValue,
                   String(prefs.searchIncludeRelated), String(store.search.revision)]) {
            await runSearch()
        }
        .task(id: store.search.query) {
            guard let found = await SearchFetch.senders(matching: store.search.query) else { return }
            // A different list under an armed row is a different row armed.
            if found != fetchedSenders { senderIndex = -1 }
            fetchedSenders = found
        }
    }

    private func focusField() {
        DispatchQueue.main.async { focused = true }
    }

    /// ONE BUTTON, TWO VERBS, the same two ⌘↩ has: with a conversation going
    /// it opens that conversation in ⌘K (see `openSearchLaneInChat`), and
    /// before one it asks.
    private var askAgentButton: some View {
        let opens = store.searchLaneHasConversation
        return Button { store.requestDeeperSearch() } label: {
            HStack(spacing: Space.scaled(5)) {
                Image(systemName: opens ? "arrow.up.left.and.arrow.down.right" : "sparkles")
                Text(opens ? "Open in chat"
                    : (store.searchLane.running || store.preparingSearchEvidence) ? "Searching…" : "Ask agent")
                if store.search.expanded { Kbd("⌘↩") }
            }
            .font(Typo.ui(12, weight: .medium))
        }
        .buttonStyle(.plain)
        .foregroundStyle(Palette.accentInk)
        .disabled(opens
            ? !store.canOpenSearchLaneInChat
            : !DeeperSearchPolicy.canRequest(query: store.search.query,
                choice: prefs.deeperSearch, running: store.searchLane.running || store.preparingSearchEvidence))
        .help(opens
            ? (!store.canOpenSearchLaneInChat
                ? "The chat is busy answering something else."
                : "Open this conversation in chat for the full answer and follow-ups. ⌘Return")
            : prefs.deeperSearch == .off
                ? "Enable deeper search in Settings to use the agent."
                : "Ask the agent to search and read your mail. ⌘Return")
    }

    /// The hits themselves, extracted so the strip (band above) and the
    /// expanded layout (band beside) can both draw them without a second copy.
    private func results(expanded: Bool) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 0) {
                    // WHO THE WORDS COULD BE NAMING, at the top of the results
                    // and scrolling away with them. Inside the scroller rather
                    // than pinned above it: three rows held over fifty hits is
                    // a quarter of the strip spent on an offer already declined.
                    if !senders.isEmpty {
                        sectionHeader("Senders").id(Self.sendersAnchor)
                        ForEach(Array(senders.enumerated()), id: \.element.id) { i, sender in
                            SenderRow(
                                sender: sender, selected: i == senderIndex,
                                // Tab takes the armed row, or the first.
                                tabTakes: i == max(0, senderIndex)
                            ) { show(sender) }
                        }
                    }
                    ForEach(Array(store.search.hits.enumerated()), id: \.element.id) { i, hit in
                        if let title = header(above: i) { sectionHeader(title) }
                        // One click opens: the reader sits beside this list,
                        // so opening a hit costs the results nothing.
                        HitRow(
                            hit: hit, terms: terms,
                            selected: i == store.search.index, expanded: expanded
                        ) {
                            store.search.index = i
                            senderIndex = -1
                            open()
                        }
                        .id(hit.id)
                        // Reaching the last row IS the request for the next
                        // page. On the row rather than a footer sentinel so
                        // it fires in both the strip and fullscreen, where
                        // the column widths (and so the row counts) differ.
                        .onAppear {
                            guard hit.id == store.search.hits.last?.id else { return }
                            Task { await loadMore() }
                        }
                    }
                    // Rows just stopping is indistinguishable from the end
                    // of the results, so the append announces itself.
                    if loadingMore { BandNote("loading more…") }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.leading, expanded && bandMounted ? Space.s8 : Space.s16)
                .padding(.trailing, Space.s16)
                .padding(.bottom, Space.s14)
            }
            .onAppear {
                if let hit = store.search.hits[safe: store.search.index] {
                    proxy.scrollTo(hit.id, anchor: .center)
                }
            }
            .onChange(of: expanded) { _, _ in
                if let hit = store.search.hits[safe: store.search.index] {
                    proxy.scrollTo(hit.id, anchor: .center)
                }
            }
            .onChange(of: store.search.index) { _, i in
                guard let hit = store.search.hits[safe: i] else { return }
                withAnimation(Motion.scrollFollow) {
                    proxy.scrollTo(hit.id, anchor: .center)
                }
            }
            // Arrowing back up out of the hits has to bring the senders back
            // into view, or the selection walks off the top of the list.
            .onChange(of: senderIndex) { _, i in
                guard i >= 0 else { return }
                withAnimation(Motion.scrollFollow) {
                    proxy.scrollTo(Self.sendersAnchor, anchor: .top)
                }
            }
        }
    }

    private static let sendersAnchor = "senders"

    private func sectionHeader(_ title: String) -> some View {
        Text(title)
            .font(Typo.micro)
            .textCase(.uppercase)
            .tracking(1)
            .foregroundStyle(Palette.inkDim)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Space.s14)
            .padding(.top, Space.s14)
            .padding(.bottom, Space.s6)
    }

    /// The header that goes above hit `i`, if one does. A ranked search is
    /// split where the status turns over; a listing is one run in date order,
    /// so it gets one header saying which way the dates go.
    private func header(above i: Int) -> String? {
        let hits = store.search.hits
        if store.search.showsListing { return i == 0 ? "Newest first" : nil }
        // PLAIN ORDER (a daemon that offers it): ranked as found, not split by
        // status, so there is no turn-over to label.
        if store.searchPlainOrder { return nil }
        guard i == 0 || hits[i].is_done != hits[i - 1].is_done else { return nil }
        return hits[i].is_done.map { $0 ? "Done" : "Not done" } ?? "Results"
    }

    /// The band as the STRIP draws it: above the hits, and never allowed to
    /// take the whole panel.
    ///
    /// The ceiling is the point. `show_emails` shows up to eight cards a batch
    /// and the band renders every batch the lane has shown, so one answer is
    /// around 620pt and a refined one twice that — in a 460pt strip that pushed
    /// the results list, the panel's whole reason for existing, down to zero
    /// height, with the band's own overflow clipped rather than scrollable so
    /// neither could be read. Scrolling inside a bounded frame gives the hits a
    /// floor and the cards a way to be reached; `fixedSize` is what keeps a
    /// two-line band two lines tall instead of always claiming the ceiling.
    private var strippedBand: some View {
        ScrollView {
            DeeperSearchBand(expanded: false)
        }
        .frame(maxHeight: Self.stripBandCeiling)
        .fixedSize(horizontal: false, vertical: true)
    }

    /// The most of a 460pt strip the lane may occupy. Roughly four cards, which
    /// is enough to read an answer without the hits going away.
    private static var stripBandCeiling: CGFloat { Space.scaled(320) }

    /// Whether the deeper-search band is on screen at all. It appears once THIS
    /// panel session has judged a query deeper (or has already started a lane),
    /// so a reader whose searches are all lookups never sees it; `off` mounts
    /// nothing, ever.
    private var bandMounted: Bool {
        guard prefs.deeperSearch != .off else { return false }
        return store.search.laneStarted || store.search.lastVerdict?.isDeeper == true
    }

    private var bindings: [KeyBinding] {
        [
            KeyBinding("Enter", "ask agent", meta: true, allowInInput: true) {
                store.requestDeeperSearch()
            },
            KeyBinding("ArrowDown", "next hit", allowInInput: true) { move(1) },
            KeyBinding("ArrowUp", "prev hit", allowInInput: true) { move(-1) },
            // Enter is three verbs, and which one it is follows what the row
            // under the arm actually IS: a remembered query goes back into the
            // field, a hit opens, and the bare bar expands the panel into
            // fullscreen previews.
            KeyBinding("Enter", enterDescription, allowInInput: true) {
                if showingRecents, let query = recents[safe: store.search.index] {
                    run(query)
                } else if let sender = senders[safe: senderIndex] {
                    show(sender)
                } else if store.search.index >= 0 {
                    open()
                } else if store.search.expanded, answered, !store.search.hits.isEmpty {
                    store.search.index = 0
                    open()
                } else if canExpand {
                    // Expanding is acting on the results: the reader asked for
                    // a bigger look at these hits, which is as much of an
                    // answer as opening one of them.
                    remember()
                    store.search.expanded = true
                }
            },
            // j/k also work when focus is not in the input.
            KeyBinding("j", "next hit") { move(1) },
            KeyBinding("k", "prev hit") { move(-1) },
        ] + (senders.isEmpty ? [] : [
            // Only while a sender is on offer, so Tab is otherwise the
            // system's. The armed one, or the first: the row wearing the hint.
            KeyBinding("Tab", "show this sender's mail", allowInInput: true) {
                if let sender = senders[safe: max(0, senderIndex)] { show(sender) }
            },
        ])
    }

    /// The retained sidebar can expand once there is a query to inspect.
    private var canExpand: Bool {
        if showingRecents || store.search.expanded { return false }
        return !(answered && store.search.hits.isEmpty)
    }

    /// What Enter says it will do in the help overlay, which is also the check
    /// that the branches above stay one decision: `recents[safe:]` returns nil
    /// at -1, so an unarmed empty state falls through to the expand test.
    private var enterDescription: String {
        if senders.indices.contains(senderIndex) { return "show this sender's mail" }
        if store.search.index >= 0 { return showingRecents ? "search this" : "open thread" }
        if store.search.expanded, answered, !store.search.hits.isEmpty { return "open first result" }
        return canExpand ? "expand previews" : "nothing to open"
    }

    /// Floor -1, not 0: ArrowUp from the top row disarms back to the bar.
    ///
    /// The sender rows and the hits are ONE COLUMN to the arrows, senders
    /// first, though they are armed by two indices (see `senderIndex`). The
    /// position is worked out across both and written back to whichever list
    /// it lands in, so exactly one row is ever armed.
    private func move(_ delta: Int) {
        guard !showingRecents else {
            store.search.index = max(-1, min(rowCount - 1, store.search.index + delta))
            return
        }
        let offered = senders.count
        let at = senders.indices.contains(senderIndex) ? senderIndex
            : store.search.index >= 0 ? offered + store.search.index : -1
        let to = max(-1, min(offered + rowCount - 1, at + delta))
        senderIndex = to < offered ? to : -1
        store.search.index = to < offered ? -1 : to - offered
    }

    /// List everything from one sender: the completed operator goes into the
    /// FIELD, the way a remembered query does, and the panel's own debounced
    /// task fetches it. The reader can see what was asked and narrow it.
    private func show(_ sender: SenderHit) {
        store.search.query = FromOperator.listing(of: sender.addr)
        store.search.index = -1
        senderIndex = -1
        focused = true
    }

    private func open() {
        guard let hit = store.search.hits[safe: store.search.index] else { return }
        // OPENING A HIT IS WHERE A QUERY STOPS BEING KEYSTROKES. Both callers
        // land here — the arrow-and-Enter path and a plain click on a row — so
        // the ring is written in one place.
        remember()
        Discovery.note(.searchHitOpened)
        // openThread itself collapses `expanded` — every path into the reader
        // must, so the collapse lives there rather than here.
        store.openThread(hit.thread_id, from: .search)
    }

    /// Put a submitted query in the ring. What counts as submitted, and which
    /// string is written, is `RecentSearchStore.remember`'s: the phone's Search
    /// tab records through the same call, so the two shells cannot disagree.
    private func remember() {
        RecentSearchStore.shared.remember(store.search)
    }

    /// Forget the list, and let go of it as well: the armed row is one of the
    /// rows that just went away, and an index left pointing into a list nobody
    /// can see is a silent Enter. The field takes focus back for the same
    /// reason `run` does — a button press must not leave the reader typing into
    /// nothing.
    private func clearRecents() {
        RecentSearchStore.shared.clear()
        store.search.index = -1
        focused = true
    }

    /// Run a remembered query: it goes into the FIELD, the way accepting a
    /// sender does, and the panel's own debounced task does the rest. Nothing
    /// is opened — the reader may want to narrow it first.
    private func run(_ query: String) {
        store.search.query = query
        // Disarm on the way: the armed row belonged to the recents list, and
        // leaving the index at 2 would point at hit row 2 the moment results
        // land — repurposing the next Enter into opening a stale stranger.
        store.search.index = -1
        // A click on a row moves the responder; typing has to keep working.
        focused = true
    }

    private func runSearch() async {
        let term = store.search.query.trimmed
        // Read at fetch time, not captured on mount: the panel is often built
        // before a trip to Settings and rebuilt after one.
        let sort = prefs.searchSort
        let related = prefs.searchIncludeRelated
        let revision = store.search.revision
        let sameRanking = term == store.search.fetchedQuery && sort == store.search.fetchedSort
            && related == store.search.fetchedRelated
        let selectedID = sameRanking ? store.search.hits[safe: store.search.index]?.id : nil
        // A bare operator (`from:` with the menu opening under it) is not a
        // search yet: the daemon would drop the valueless token and 400 the
        // empty query, and that refusal is not something to show a reader who
        // is halfway through typing a sender.
        guard !term.isEmpty, !FromOperator.awaitingValue(in: term) else {
            store.search.hits = []
            store.search.error = nil
            store.search.fetchedQuery = nil
            store.search.fetchedSort = nil
            store.search.diagnostics = nil
            store.search.nextCursor = nil
            // AND NOTHING IS ARMED. The index outlives the hits it was counted
            // against (it is parked in the store so `/` can resume a search),
            // so a field cleared while row 7 was armed would hand row 7 of the
            // recents list — a different list, a different length — to the very
            // next Enter.
            store.search.index = -1
            loading = false
            return
        }
        // Already holding this term's results UNDER THIS ORDER: reopening must
        // not re-fetch and flash, which is the point of hoisting the session
        // into the store. The sort is half of that test — same words ranked by
        // different rules is a different answer.
        //
        // CLEARS `loading` on the way out, because this is the path a cancelled
        // predecessor used to rely on someone else covering (backspace inside
        // the debounce window lands here). The CURRENT task owns the flag now;
        // see the cancelled exits below.
        guard !store.search.answers(term, sort: sort, related: related) else {
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
        // Every path that stops being the current search now leaves the flag to
        // whoever is (including the early return above, which is the case this
        // used to be covering for).
        guard !Task.isCancelled else { return }
        do {
            // PARTIAL, mode by preference, unfinished first and snippets
            // cleaned: all of that is `SearchFetch`, shared with the phone. The
            // agent's own searches never go through it.
            let page = try await SearchFetch.page(term, sort: sort, related: related)
            // AND AGAIN AFTER THE AWAIT, which the iOS twin has always done
            // (`MobileSearchView`). A response landing in the window between
            // `task(id:)` cancelling this task and URLSession noticing resolves
            // normally, so without this a superseded fetch stamps its hits over
            // the live ones. That much was self-correcting; `judge` below is
            // not. It would start a paid conversation about words the reader
            // deleted 200ms ago, and the next fetch would then hand the live
            // words to it as a "refinement".
            guard !Task.isCancelled, term == store.search.query.trimmed,
                  revision == store.search.revision, related == prefs.searchIncludeRelated else { return }
            store.search.hits = page.items
            store.search.diagnostics = page.diagnostics
            store.search.nextCursor = page.next_cursor
            // Fresh results land un-armed: Enter straight from the bar means
            // "show me more", not "open whatever floated to the top".
            store.search.index = selectedID.flatMap { id in page.items.firstIndex { $0.id == id } } ?? -1
            store.search.error = nil
            store.search.fetchedQuery = term
            store.search.fetchedSort = sort
            store.search.fetchedRelated = related
            store.search.fetchedRevision = revision
            // Warm the head of the page only. Search rows are read and chosen
            // from, not swept, so the rest can wait for a real click — and the
            // whole 50 would be a stampede for one open.
            for hit in page.items.prefix(5) {
                ThreadPrefetch.shared.prefetch(hit.thread_id)
            }
            // AFTER THE FETCH, NEVER MID-KEYSTROKE. The classifier is a
            // decision to spend money, so it is made once per SETTLED query —
            // past the 220ms debounce, with the daemon's own diagnostics for
            // exactly these words in hand.
            judge(term)
        } catch {
            // Cancellation surfaces here too (URLError.cancelled mid-request):
            // that is a superseded task, not a failure, and writing an error
            // would stamp the NEW search's state with the old one's obituary.
            // Nor the flag — same reason as the debounce exit above.
            guard !Task.isCancelled else { return }
            store.search.error = errText(error, "search failed")
            // Leave `fetchedQuery` nil so reopening RETRIES rather than
            // resurrecting a stale error over stale hits.
            store.search.fetchedQuery = nil
            store.search.fetchedSort = nil
            // The diagnostics go with the query they described: judging the
            // next search on the last one's counts is exactly the mistake
            // pairing them prevents.
            store.search.diagnostics = nil
            // And drop the cursor with it: it belongs to a page set this view
            // is no longer showing.
            store.search.nextCursor = nil
        }
        loading = false
    }

    /// Keyword or question, for the query whose hits are now on screen
    /// (docs/SEARCH.md §5). The verdict is recorded whatever the preference
    /// says, because it is also what mounts the band on request; only STARTING
    /// is the preference's business.
    private func judge(_ term: String) {
        // ONLY FOR WORDS THAT ARE STILL THE READER'S. Judging is where money
        // gets spent, so the contract lives here rather than only at the fetch
        // that calls it: the term is judged when it is still what the field
        // says, which is the same test `answered` applies before the panel will
        // claim the hits belong to the query on screen. A reader who typed on
        // is judged by the next settled query, one debounce away, and a lane
        // started for words they deleted would take the live ones as a
        // "refinement" of a question nobody asked.
        guard term == store.search.query.trimmed else { return }
        let verdict = SearchIntent.classify(query: term, diagnostics: store.search.diagnostics)
        store.search.lastVerdict = verdict
        // WHAT that verdict is allowed to do is `DeeperSearchPolicy`'s and not
        // this view's, because the rule about `off` also has to serve the
        // picker being flipped under a lane that is already running (see
        // ShellWatchers) — and a rule spelled twice is a rule that will
        // disagree with itself. The moves are executed here; the deciding is
        // pure and asserted in test.sh.
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

    /// Append the page after the one on screen. Cursors are only meaningful
    /// beside the term they were issued for, so this refuses to run while the
    /// bar is mid-edit (`term != fetchedQuery`) and re-checks after the await —
    /// a query that turned over in flight would otherwise splice two different
    /// searches into one list.
    private func loadMore() async {
        guard answered, !loading, !loadingMore, let cursor = store.search.nextCursor else { return }
        let term = store.search.query.trimmed
        guard !term.isEmpty, term == store.search.fetchedQuery else { return }
        // The cursor is an OFFSET INTO ONE RANKING, so the page after it has to
        // be asked for under the sort the hits on screen were ranked by — not
        // under whatever the preference says now. A sort changed mid-scroll
        // re-ranks from the top through `runSearch`, which is the only honest
        // way to serve it.
        let sort = store.search.fetchedSort
        let related = store.search.fetchedRelated ?? false
        let revision = store.search.revision
        loadingMore = true
        defer { loadingMore = false }
        do {
            let page = try await SearchFetch.page(term, cursor: cursor, sort: sort, related: related)
            guard term == store.search.fetchedQuery, store.search.nextCursor == cursor,
                  sort == store.search.fetchedSort, sort == prefs.searchSort,
                  related == store.search.fetchedRelated, related == prefs.searchIncludeRelated,
                  revision == store.search.revision else {
                return
            }
            // Deduped because the cursor is an OFFSET: mail arriving between
            // two pages shifts the window, and a repeated id is a normal
            // outcome rather than a server bug. Two rows with one id would also
            // break the ForEach.
            var seen = Set(store.search.hits.map(\.id))
            for hit in page.items where seen.insert(hit.id).inserted {
                store.search.hits.append(hit)
            }
            store.search.nextCursor = page.next_cursor
        } catch {
            // Keep the cursor and stay silent: the hits already read are worth
            // more than an error line, and scrolling the last row back into
            // view retries. The bar reports failures for the search itself.
        }
    }
}

/// One sender offered above the hits. Built on the hit row's own grid (avatar,
/// then a name, then a trailing figure) so the block reads as the top of the
/// results rather than a menu laid over them.
private struct SenderRow: View {
    let sender: SenderHit
    let selected: Bool
    /// This is the row Tab would take, so it is the one that says so.
    let tabTakes: Bool
    let onShow: () -> Void

    private var name: String? {
        sender.display_name.flatMap { $0.isEmpty ? nil : $0 }
    }

    var body: some View {
        Button(action: onShow) {
            HStack(spacing: Space.s10) {
                // Sender strings are email-derived: rendered as Text only,
                // never as markup.
                Avatar(sender: name.map { "\($0) <\(sender.addr)>" } ?? sender.addr, size: 26)
                    .accessibilityHidden(true)
                if let name {
                    Text(name)
                        .font(Typo.ui(12, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                Text(sender.addr)
                    .font(Typo.mono(11))
                    .foregroundStyle(name == nil ? Palette.ink : Palette.inkDim)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: Space.s8)
                Text(sender.msg_count == 1 ? "1 email" : "\(sender.msg_count) emails")
                    .font(Typo.ui(11))
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize()
                if tabTakes { Kbd("tab") }
            }
            .padding(.vertical, Space.scaled(7))
            .padding(.leading, Space.s16)
            .padding(.trailing, Space.s12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(selected ? Palette.accentSoft : Color.clear)
        .overlay(alignment: .bottom) { Rectangle().fill(Palette.hairline).frame(height: 0.5) }
        .overlay {
            if selected { Rectangle().strokeBorder(Palette.accent.opacity(0.6), lineWidth: 1) }
        }
        .accessibilityElement(children: .combine)
        .accessibilityHint("Shows all mail from this sender, newest first")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

private struct HitRow: View {
    let hit: SearchHit
    let terms: [String]
    let selected: Bool
    let expanded: Bool
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            HStack(alignment: .top, spacing: Space.s10) {
                Avatar(sender: hit.from_name.map { "\($0) <\(hit.from_addr)>" } ?? hit.from_addr,
                       size: 26)
                    .padding(.top, 1)
                    .accessibilityHidden(true)
                columns
            }
            .padding(.vertical, Space.scaled(11))
            .padding(.leading, Space.s16)
            .padding(.trailing, Space.s12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(selected ? Palette.accentSoft : Color.clear)
        .overlay(alignment: .leading) {
            if hit.is_done == false {
                RoundedRectangle(cornerRadius: 1)
                    .fill(Palette.accentInk)
                    .frame(width: 2)
                    .padding(.vertical, Space.s14)
                    .padding(.leading, Space.s4)
            }
        }
        .overlay(alignment: .bottom) { Rectangle().fill(Palette.hairline).frame(height: 0.5) }
        .overlay {
            if selected { Rectangle().strokeBorder(Palette.accent.opacity(0.6), lineWidth: 1) }
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(hit.is_done.map { $0 ? "Done" : "Not done" } ?? "")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var sender: some View {
        Text(hit.from_name ?? hit.from_addr)
            .font(Typo.ui(12, weight: .medium))
            .foregroundStyle(Palette.ink)
            .lineLimit(1)
    }

    private var date: some View {
        HStack(spacing: Space.scaled(5)) {
            if hit.is_done == true {
                Image(systemName: "checkmark").font(Typo.ui(10))
            }
            Text(Fmt.shortDate(hit.received_at)).font(Typo.ui(11))
        }
        .foregroundStyle(Palette.inkDim)
        .fixedSize()
        .help(Fmt.dateTime(hit.received_at))
    }

    private var subject: some View {
        Text(highlight(hit.subject, matches: hit.subject_matches ?? terms))
            .font(Typo.row)
            .foregroundStyle(Palette.ink)
            .lineLimit(1)
    }

    private var preview: some View {
        Text(highlight(hit.displaySnippet ?? hit.snippet,
                       matches: hit.snippet_matches ?? terms))
            .font(Typo.rowSub)
            .foregroundStyle(Palette.inkDim)
            .multilineTextAlignment(.leading)
    }

    // Choose column widths from the panel mode, never from a row's text.
    // Long subjects truncate instead of switching that row to a stacked layout.
    private var columns: some View {
        HStack(alignment: .firstTextBaseline, spacing: expanded ? Space.scaled(18) : Space.s12) {
            sender.frame(width: expanded ? 160 : 104, alignment: .leading)
            VStack(alignment: .leading, spacing: Space.scaled(3)) {
                subject
                preview.lineLimit(expanded ? 1 : 2)
            }
            .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
            date.frame(width: expanded ? 80 : 72, alignment: .trailing)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func highlight(_ text: String, matches: [String]) -> AttributedString {
        SearchHighlight.paint(text, matches: matches)
    }
}

// MARK: - browse

/// Browse-all (`a`) — the "radio console" survivor. Fetches ALL updates incl.
/// below-the-line (no band filter), tier-colored, ranked by importance. A
/// client-side noise-filter knob hides the noise below the line without
/// re-fetching. j/k selects, Enter opens the thread.
struct BrowseView: View {
    @Environment(AppStore.self) private var store

    @State private var browseState: Loadable<[AttentionUpdate]> = .loading
    /// Client-side min importance — the squelch knob.
    @State private var squelch: Double = 0
    @State private var index = 0

    private var all: [AttentionUpdate] { browseState.value ?? [] }

    private var visible: [AttentionUpdate] {
        all.filter { Double($0.importance) >= squelch }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Space.s10) {
                Text("Noise filter: \(Int(squelch))")
                    .font(Typo.num(11))
                    .foregroundStyle(Palette.inkDim)
                    .frame(width: Typo.measure(104), alignment: .leading)
                Slider(value: $squelch, in: 0...100, step: 5)
                    .tint(Palette.accent)
                Text("\(all.count - visible.count) below line")
                    .font(Typo.num(10))
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(width: Typo.measure(92), alignment: .trailing)
            }
            .padding(.horizontal, Space.s16)
            .padding(.vertical, Space.scaled(11))

            if browseState.isLoading {
                BandNote("loading all mail…")
            } else if let error = browseState.error {
                BandNote(error)
            } else if visible.isEmpty {
                BandNote("nothing above the noise line.")
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(spacing: 1) {
                            ForEach(Array(visible.enumerated()), id: \.element.id) { i, u in
                                BrowseRow(
                                    update: u, selected: i == index
                                ) { index = i } open: {
                                    store.openThread(u.thread_id, from: .browse)
                                }
                                .id(u.id)
                            }
                        }
                        .padding(.horizontal, Space.s12)
                        .padding(.bottom, Space.s14)
                    }
                    .onChange(of: index) { _, i in
                        guard let u = visible[safe: i] else { return }
                        withAnimation(Motion.scrollFollow) {
                            proxy.scrollTo(u.id, anchor: .center)
                        }
                    }
                }
            }
        }
        .keyBindings(.modal, bindings)
        .task { await load() }
        .onChange(of: visible.count) { _, count in
            index = min(index, max(0, count - 1))
        }
    }

    private var bindings: [KeyBinding] {
        [
            KeyBinding("j", "next") { index = min(visible.count - 1, index + 1) },
            KeyBinding("k", "prev") { index = max(0, index - 1) },
            KeyBinding("Enter", "open thread") {
                if let u = visible[safe: index] { store.openThread(u.thread_id, from: .browse) }
            },
            KeyBinding("+", "raise noise filter") { squelch = min(100, squelch + 5) },
            KeyBinding("=", "raise noise filter") { squelch = min(100, squelch + 5) },
            KeyBinding("-", "lower noise filter") { squelch = max(0, squelch - 5) },
        ]
    }

    private func load() async {
        await $browseState.load("load failed") {
            let page = try await APIClient.shared.getUpdates(UpdatesParams(limit: 500))
            // Highest importance first — the ranked board.
            return page.items.sorted { $0.importance > $1.importance }
        }
    }
}

private struct BrowseRow: View {
    let update: AttentionUpdate
    let selected: Bool
    let onSelect: () -> Void
    let open: () -> Void

    var body: some View {
        // hoverFill off: this list has never washed on hover, and 500 rows of
        // tracking area is not the place to start.
        ListRow(
            selected: selected, cornerRadius: 7, tint: Palette.tierColor(update.tier),
            hPadding: Space.scaled(9), vPadding: Space.scaled(5), hoverFill: false, action: onSelect
        ) { _, _ in
            HStack(spacing: Space.s8) {
                Circle()
                    .fill(Palette.tierColor(update.tier))
                    .frame(width: 6, height: 6)
                Text("\(update.importance)")
                    .font(Typo.num(11, weight: .semibold))
                    .foregroundStyle(Palette.importanceColor(update.importance))
                    .frame(width: Space.scaled(24), alignment: .trailing)
                Text(SenderCache.resolved(update.senderString).displayName)
                    .font(Typo.ui(11, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .frame(width: 116, alignment: .leading)
                Text(update.one_line)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text(Fmt.relAge(update.surfaced_at))
                    .font(Typo.num(10))
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(width: Space.scaled(28), alignment: .trailing)
            }
        }
        .overlay(alignment: .leading) {
            if selected {
                RoundedRectangle(cornerRadius: 1)
                    .fill(Palette.tierColor(update.tier))
                    .frame(width: 2)
            }
        }
        .simultaneousGesture(TapGesture(count: 2).onEnded { open() })
    }
}
