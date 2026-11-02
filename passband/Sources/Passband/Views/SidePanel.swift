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
                .padding(.leading, expanded ? TopBar.dotsClearance : Space.panel)
                .padding(.trailing, Space.panel)
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
    /// The calendar's answer to the field (docs/CALENDAR.md §10.1), as last
    /// fetched, and the query it answered: rows for a query the field no
    /// longer holds are not drawn.
    @State private var fetchedEvents: [AgendaItem] = []
    @State private var eventsTotal = 0
    @State private var eventsQuery: String?
    /// The field text `eventsQuery` was asked from (with `in:calendar` and
    /// all), which is what opening one of its rows remembers.
    @State private var eventsField: String?
    /// The limit `eventsQuery` was fetched under.
    @State private var eventsLimit = 0
    /// "N more" was taken: the block shows every fetched row.
    @State private var eventsExpanded = false
    /// The armed row of the Events block, -1 for none. Its own index for the
    /// reason `senderIndex` has one; the "N more" row counts as the last.
    @State private var eventIndex = -1

    /// The senders offered above the hits: who the words in the field could be
    /// naming. Picking one lists everything they have sent, newest first.
    private var senders: [SenderHit] {
        SearchFetch.senders(fetchedSenders, stillMatching: store.search.query)
    }

    /// The field asks for events only (`in:calendar`): no mail search runs.
    private var eventsOnly: Bool {
        CalendarSearch.eventsOnly(store.search.query)
    }

    /// The Events block as drawn: nothing while the Calendar page is not
    /// this mailbox's or the field asks the calendar nothing. While a new
    /// answer is on its way the last one stays, so the block does not vanish
    /// and the list jump on every keystroke; it is replaced when the answer
    /// lands. Events only lists every fetched row.
    private var eventBlock: CalendarSearch.Block {
        guard !showingRecents, eventsQuery != nil,
              CalendarSearch.eventQuery(store.search.query.trimmed) != nil
        else { return CalendarSearch.Block(shown: [], more: 0) }
        return CalendarSearch.block(fetchedEvents, total: eventsTotal, expanded: eventsExpanded || eventsOnly)
    }

    /// How many rows of the arrow column the block holds: its events, and
    /// "N more" when there is one.
    private var eventEntries: Int {
        let block = eventBlock
        return block.shown.count + (block.more > 0 ? 1 : 0)
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
    private var recents: [RecentEntry] { RecentSearchStore.shared.entries }

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
            store.search.query.trimmed, sort: prefs.searchSort, related: prefs.searchIncludeRelated,
            plainOrder: store.searchPlainOrder)
    }

    var body: some View {
        @Bindable var store = store
        let expanded = store.search.expanded

        VStack(alignment: .leading, spacing: 0) {
            // Its words on the panel's TEXT EDGE (`PanelRow`): the capsule on
            // the gutter, the words `hitPadding` in, where the recents, the
            // section headers and the hits' avatars start, so typing the
            // first letter does not move the column.
            Field(label: "", capsule: true, inset: PanelRow.hitPadding) {
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
            .padding(.horizontal, Space.panel)
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
                    .padding(.horizontal, Space.panel)
                    .padding(.bottom, Space.s8)
            }

            HStack {
                // A listing has no ranking to choose between: it is date order
                // whatever the picker says, so the picker stands down rather
                // than offer a switch that does nothing.
                // Events only has its one order too (by time).
                SearchSortPicker()
                    .disabled(store.search.showsListing || eventsOnly)
                    .opacity(store.search.showsListing || eventsOnly ? 0.4 : 1)
                Spacer(minLength: Space.s8)
                if expanded { askAgentButton }
            }
            .padding(.horizontal, Space.panel)
            .padding(.bottom, Space.s10)

            // Events only runs no mail search, so there is nothing for the
            // mail agent to go deeper on (the phone hides its ask row too).
            if !expanded && !eventsOnly {
                HStack { Spacer(); askAgentButton }
                    .padding(.horizontal, Space.panel)
                    .padding(.bottom, Space.s10)
            }

            if let error = store.search.error { BandNote(error) }
            if eventsOnly {
                if CalendarSearch.eventQuery(store.search.query.trimmed) == nil {
                    BandNote(CalendarSearch.emptyNote(store.search.query))
                } else if eventsQuery == CalendarSearch.eventQuery(store.search.query.trimmed),
                          fetchedEvents.isEmpty {
                    BandNote("no events.")
                }
            } else if answered && store.search.hits.isEmpty && eventBlock.shown.isEmpty {
                BandNote("no matches.")
            }

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
                    entries: recents, armed: store.search.index,
                    onRun: { run($0) }, onClear: { clearRecents() }
                )
                .frame(maxWidth: .infinity, alignment: .leading)
                // The hits' envelope (`PanelRow.hitInset`), so the recents
                // and the hits that replace them share one fill edge and one
                // text edge.
                .padding(.horizontal, PanelRow.hitInset)
            }
            HStack(alignment: .top, spacing: 0) {
                if bandMounted && expanded {
                    ScrollView {
                        DeeperSearchBand(expanded: true)
                            .padding(.leading, Space.panel)
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
            eventIndex = -1
            eventsExpanded = false
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
                   String(prefs.searchIncludeRelated), String(store.search.revision),
                   String(store.searchPlainOrder)]) {
            await runSearch()
        }
        .task(id: store.search.query) {
            // Events only asks no sender directory: its words name events.
            if eventsOnly {
                fetchedSenders = []
                senderIndex = -1
                return
            }
            guard let found = await SearchFetch.senders(matching: store.search.query) else { return }
            // A different list under an armed row is a different row armed.
            if found != fetchedSenders { senderIndex = -1 }
            fetchedSenders = found
        }
        .task(id: [store.search.query, String(eventsExpanded)]) {
            await runEventSearch()
        }
    }

    /// The Events block's fetch, beside the mail search and never waiting on
    /// it. Debounced like it; a superseded fetch writes nothing. Expanding
    /// ("N more") or events only asks again for the whole list. A FAILURE
    /// (an older daemon's 404 included) clears the block and leaves
    /// `eventsQuery` unset, so the next pass asks again rather than reading
    /// the blip as "no events"; it is never an error over the mail results.
    private func runEventSearch() async {
        let field = store.search.query.trimmed
        // No Events block while this mailbox has no Calendar page: the
        // daemon 404s the search, and there is no page to open a hit on.
        guard store.calendarPageAvailable, let asked = CalendarSearch.eventQuery(field) else {
            clearEvents()
            return
        }
        // In hand already, unless the list now wants more than was asked.
        let limit = CalendarSearch.limit(eventsOnly: eventsOnly, expanded: eventsExpanded)
        guard asked != eventsQuery || limit > eventsLimit else { return }
        try? await Task.sleep(for: SearchFetch.debounce)
        guard !Task.isCancelled else { return }
        let page: CalendarSearchPage
        do {
            page = try await APIClient.shared.calendarSearch(asked, limit: limit)
        } catch {
            guard !Task.isCancelled else { return }
            clearEvents()
            return
        }
        guard !Task.isCancelled, asked == CalendarSearch.eventQuery(store.search.query.trimmed) else { return }
        fetchedEvents = CalendarSearch.items(page, calendar: .current)
        eventsTotal = page.total
        eventsQuery = asked
        eventsField = field
        eventsLimit = limit
    }

    private func clearEvents() {
        fetchedEvents = []
        eventsTotal = 0
        eventsQuery = nil
        eventsField = nil
        eventsLimit = 0
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
                                // The rule under a row gives way to the
                                // selection fill on either side of it.
                                // The last one's neighbour is the first hit
                                // (or that hit's header).
                                ruled: i != senderIndex && i + 1 != senderIndex
                                    && !(i == senders.count - 1
                                        && (store.search.index == 0 || header(above: 0) != nil
                                            || eventEntries > 0)),
                                // Tab takes the armed row, or the first.
                                tabTakes: i == max(0, senderIndex)
                            ) { show(sender) }
                        }
                    }
                    // WHAT THE CALENDAR HOLDS for these words (docs/CALENDAR.md
                    // §10.1): a block of its own, by time, because an event's
                    // date is when it happens and a hit's is when it arrived.
                    let block = eventBlock
                    if eventEntries > 0 {
                        sectionHeader("Events").id(Self.eventsAnchor)
                        ForEach(Array(block.shown.enumerated()), id: \.element.id) { i, item in
                            EventSearchRow(
                                item: item, selected: i == eventIndex,
                                ruled: i != eventIndex && i + 1 != eventIndex
                                    && !(i == eventEntries - 1 && (store.search.index == 0 || header(above: 0) != nil))
                            ) {
                                eventIndex = i
                                openEvent(item)
                            }
                            .id(Self.eventRowId(i))
                        }
                        if block.more > 0 {
                            MoreEventsRow(count: block.more, selected: eventIndex == block.shown.count) {
                                expandEvents()
                            }
                            .id(Self.eventRowId(block.shown.count))
                        }
                        if let cut = block.cut { BandNote(cut) }
                    }
                    ForEach(Array(store.search.hits.enumerated()), id: \.element.id) { i, hit in
                        if let title = header(above: i) { sectionHeader(title) }
                        // One click opens: the reader sits beside this list,
                        // so opening a hit costs the results nothing.
                        HitRow(
                            hit: hit, terms: terms,
                            selected: i == store.search.index, expanded: expanded,
                            // No rule against the selection fill, above or
                            // below it, and none under a row a header follows.
                            ruled: i != store.search.index && i + 1 != store.search.index
                                && header(above: i + 1) == nil
                        ) {
                            store.search.index = i
                            senderIndex = -1
                            eventIndex = -1
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
                // `PanelRow.hitInset`: the fill on the panel gutter, the
                // words `hitPadding` inside it. Beside the expanded band the
                // band column is the margin, so the fill sits just off it.
                .padding(.leading, expanded && bandMounted ? Space.s8 : PanelRow.hitInset)
                .padding(.trailing, PanelRow.hitInset)
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
            // Each row its own anchor, so arrowing down a long list (events
            // only, or an expanded block) keeps the armed row in view; the
            // first row brings the header with it.
            .onChange(of: eventIndex) { _, i in
                guard i >= 0 else { return }
                withAnimation(Motion.scrollFollow) {
                    if i == 0 {
                        proxy.scrollTo(Self.eventsAnchor, anchor: .top)
                    } else {
                        proxy.scrollTo(Self.eventRowId(i), anchor: .center)
                    }
                }
            }
        }
    }

    private static let sendersAnchor = "senders"
    private static let eventsAnchor = "events"
    private static func eventRowId(_ i: Int) -> String { "event-\(i)" }

    private func sectionHeader(_ title: String) -> some View {
        Text(title)
            .font(Typo.micro)
            .textCase(.uppercase)
            .tracking(1)
            .foregroundStyle(Palette.inkDim)
            .frame(maxWidth: .infinity, alignment: .leading)
            // Inside the list's inset, so on the rows' word edge (the
            // avatars' leading edge).
            .padding(.horizontal, PanelRow.hitPadding)
            .padding(.top, Space.s14)
            .padding(.bottom, Space.s6)
    }

    /// The header that goes above hit `i`, if one does: only a listing gets
    /// one, saying which way its dates run. A ranked search is ONE list, with
    /// no Done / Not done split; an older daemon still sends unfinished mail
    /// first until it is rolled to plain order (`AppStore.searchPlainOrder`),
    /// and the labels do not wait for that.
    private func header(above i: Int) -> String? {
        store.search.showsListing && i == 0 ? "Newest first" : nil
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
                guard !eventsOnly else { return }
                store.requestDeeperSearch()
            },
            KeyBinding("ArrowDown", "next hit", allowInInput: true) { move(1) },
            KeyBinding("ArrowUp", "prev hit", allowInInput: true) { move(-1) },
            // Enter is three verbs, and which one it is follows what the row
            // under the arm actually IS: a remembered query goes back into the
            // field, a hit opens, and the bare bar expands the panel into
            // fullscreen previews.
            KeyBinding("Enter", enterDescription, allowInInput: true) {
                if showingRecents, let entry = recents[safe: store.search.index] {
                    run(entry)
                } else if let sender = senders[safe: senderIndex] {
                    show(sender)
                } else if eventIndex >= 0 {
                    if let item = eventBlock.shown[safe: eventIndex] { openEvent(item) } else { expandEvents() }
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
        if eventIndex >= 0 {
            return eventBlock.shown.indices.contains(eventIndex) ? "open in calendar" : "show more events"
        }
        if store.search.index >= 0 { return showingRecents ? "search this" : "open thread" }
        if store.search.expanded, answered, !store.search.hits.isEmpty { return "open first result" }
        return canExpand ? "expand previews" : "nothing to open"
    }

    /// Floor -1, not 0: ArrowUp from the top row disarms back to the bar.
    ///
    /// The sender rows, the Events block and the hits are ONE COLUMN to the
    /// arrows, in that order, though they are armed by three indices (see
    /// `senderIndex`, `eventIndex`). The position is worked out across all of
    /// them and written back to whichever list it lands in, so exactly one row
    /// is ever armed.
    private func move(_ delta: Int) {
        guard !showingRecents else {
            store.search.index = max(-1, min(rowCount - 1, store.search.index + delta))
            return
        }
        let offered = senders.count
        let events = eventEntries
        let at = senders.indices.contains(senderIndex) ? senderIndex
            : (0..<events).contains(eventIndex) ? offered + eventIndex
            : store.search.index >= 0 ? offered + events + store.search.index : -1
        let to = max(-1, min(offered + events + rowCount - 1, at + delta))
        senderIndex = to < offered ? to : -1
        eventIndex = (offered..<offered + events).contains(to) ? to - offered : -1
        store.search.index = to < offered + events ? -1 : to - offered - events
    }

    /// Open one event of the block on the Calendar page. Opening is where a
    /// query stops being keystrokes, as it is for a hit.
    private func openEvent(_ item: AgendaItem) {
        // The words these rows answered, not the mail fetch's (which events
        // only never makes, and which may still be on an older term).
        if let field = eventsField { RecentSearchStore.shared.rememberEventSearch(field) }
        store.openCalendarSearchHit(item)
    }

    /// "N more": the block shows every row, asking the daemon for the whole
    /// list (`runEventSearch` is keyed on this), and the arm stays on the row
    /// that took the place of "N more".
    private func expandEvents() {
        eventsExpanded = true
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
    private func run(_ entry: RecentEntry) {
        // A question goes back to the agent; a search goes back in the field.
        // Its saved chat comes back in the ⌘K bar when there is one; without
        // one, it is asked again.
        if entry.kind == .question {
            if store.openQuestionFromHistory(entry.text) {
                store.askBarOpen = true
            } else {
                focused = true
            }
            return
        }
        let query = entry.text
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
        let plainOrder = store.searchPlainOrder
        let revision = store.search.revision
        let sameRanking = term == store.search.fetchedQuery && sort == store.search.fetchedSort
            && related == store.search.fetchedRelated && plainOrder == store.search.fetchedPlainOrder
        let selectedID = sameRanking ? store.search.hits[safe: store.search.index]?.id : nil
        // A bare operator (`from:` with the menu opening under it) is not a
        // search yet: the daemon would drop the valueless token and 400 the
        // empty query, and that refusal is not something to show a reader who
        // is halfway through typing a sender.
        // EVENTS ONLY (`in:calendar`) is no mail search at all: nothing goes
        // to `/client/search` (which would read the operator as a word), and
        // with no hits judged, no deeper-search lane starts.
        let calendarOnly = CalendarSearch.eventsOnly(term)
        guard !term.isEmpty, !FromOperator.awaitingValue(in: term), !calendarOnly else {
            if calendarOnly { store.resetSearchLane() }
            store.search.hits = []
            store.search.error = nil
            store.search.fetchedQuery = nil
            store.search.fetchedSort = nil
            store.search.fetchedPlainOrder = nil
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
        guard !store.search.answers(term, sort: sort, related: related, plainOrder: plainOrder) else {
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
            let page = try await SearchFetch.page(
                term, sort: sort, related: related, plainOrder: plainOrder)
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
            store.search.fetchedPlainOrder = plainOrder
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
            store.search.fetchedPlainOrder = nil
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
        // And the ORDER, for the same reason: page 2 of a grouped ranking asked
        // in plain order skips hits. A flip mid-scroll re-fetches page 1
        // through `runSearch` (`answers` keys on it).
        let plainOrder = store.search.fetchedPlainOrder ?? store.searchPlainOrder
        let revision = store.search.revision
        loadingMore = true
        defer { loadingMore = false }
        do {
            let page = try await SearchFetch.page(
                term, cursor: cursor, sort: sort, related: related, plainOrder: plainOrder)
            guard term == store.search.fetchedQuery, store.search.nextCursor == cursor,
                  plainOrder == store.search.fetchedPlainOrder,
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
    /// Draws the rule under the row (`HitRule`).
    let ruled: Bool
    /// This is the row Tab would take, so it is the one that says so.
    let tabTakes: Bool
    let onShow: () -> Void

    private var name: String? {
        sender.display_name.flatMap { $0.isEmpty ? nil : $0 }
    }

    var body: some View {
        // The shared row envelope, as every other list draws its selection.
        // hoverFill off: this list never washed on hover, and the keys, not
        // the pointer, walk it.
        ListRow(
            selected: selected, hPadding: PanelRow.hitPadding, vPadding: PanelRow.vPadding,
            hoverFill: false, action: onShow
        ) { _, _ in
            // One line, so centred alignment IS the name's line.
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
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .overlay(alignment: .bottom) { if ruled { HitRule() } }
        .accessibilityElement(children: .combine)
        .accessibilityHint("Shows all mail from this sender, newest first")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// One row of the Events block: the calendar's mark in its colour (a ghost
/// drawn in the agenda's own language for how real it is, so a likely plan is
/// striped here as it is on the page), the title, and when and where in the
/// user's zone. A past event reads dimmer; a ghost says it came from mail.
private struct EventSearchRow: View {
    let item: AgendaItem
    let selected: Bool
    let ruled: Bool
    let onOpen: () -> Void

    var body: some View {
        let now = Date()
        let past = CalendarSearch.isPast(item, now: now)
        ListRow(
            selected: selected, hPadding: PanelRow.hitPadding, vPadding: PanelRow.vPadding,
            hoverFill: false, action: onOpen
        ) { _, _ in
            HStack(spacing: Space.s10) {
                // A swatch the avatars' width, not an edge bar: the row's
                // colour is a mark beside the words, as the agenda draws it.
                AgendaMark(
                    fill: item.fill, estimated: item.endEstimated, thin: item.cancelled,
                    hue: ChipColors.inks(item).edge, cornerRadius: 4
                )
                .frame(width: 14, height: 14)
                .frame(width: 26)
                .opacity(past ? 0.55 : 1)
                .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: Space.s2) {
                    // Event text is the user's (or a sender's): plain Text only.
                    Text(item.title)
                        .font(Typo.ui(12, weight: .medium))
                        .foregroundStyle(past ? Palette.inkDim : Palette.ink)
                        .strikethrough(item.cancelled)
                        .lineLimit(1)
                    Text(CalendarSearch.label(item, now: now, calendar: .current))
                        .font(Typo.ui(11))
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                }
                Spacer(minLength: Space.s8)
                if item.isGhost {
                    Text("from mail")
                        .font(Typo.ui(11))
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize()
                } else if past {
                    Text("past")
                        .font(Typo.ui(11))
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize()
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .overlay(alignment: .bottom) { if ruled { HitRule() } }
        .accessibilityElement(children: .combine)
        .accessibilityHint("Opens this event in the calendar")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// "N more events": opens the rest of the block in place.
private struct MoreEventsRow: View {
    let count: Int
    let selected: Bool
    let onExpand: () -> Void

    var body: some View {
        ListRow(
            selected: selected, hPadding: PanelRow.hitPadding, vPadding: PanelRow.vPadding,
            hoverFill: false, action: onExpand
        ) { _, _ in
            HStack(spacing: Space.s10) {
                Image(systemName: "plus")
                    .font(Typo.ui(10, weight: .semibold))
                    .foregroundStyle(Palette.inkDim)
                    .frame(width: 26)
                    .accessibilityHidden(true)
                Text(CalendarSearch.moreLabel(count))
                    .font(Typo.ui(11))
                    .foregroundStyle(Palette.inkDim)
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// THE RULE BETWEEN RESULTS, under the avatar-and-words column: it starts on
/// the avatars' edge and stops on the dates', so it never runs into the
/// relevance bar or past the selection fill's rounded corners.
private struct HitRule: View {
    var body: some View {
        Hairline().padding(.horizontal, PanelRow.hitPadding)
    }
}

private struct HitRow: View {
    let hit: SearchHit
    let terms: [String]
    let selected: Bool
    let expanded: Bool
    /// Draws the rule under the row (`HitRule`).
    var ruled = true
    let onOpen: () -> Void

    var body: some View {
        // Read here: the guide's closure is not main-actor isolated.
        let nameLine = Typo.size(12) * 0.35
        // The shared row envelope (see SenderRow), on the same grid.
        ListRow(
            selected: selected, hPadding: PanelRow.hitPadding, vPadding: PanelRow.vPadding,
            hoverFill: false, action: onOpen
        ) { _, _ in
            // The avatar sits on the SENDER'S LINE, its centre on the name,
            // not centred on the two-line row nor hung from its top edge.
            HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
                Avatar(sender: hit.from_name.map { "\($0) <\(hit.from_addr)>" } ?? hit.from_addr,
                       size: 26)
                    // THE NOT-DONE BAR rides on the avatar, so it is the
                    // avatar's height on the avatar's centre wherever the
                    // avatar sits (the name line). Drawn from the row it was
                    // centred on the two-line row and hung a few points low.
                    // It reaches back out to s4 from the row's leading edge.
                    .overlay(alignment: .leading) {
                        if hit.is_done == false {
                            RoundedRectangle(cornerRadius: 1)
                                .fill(Palette.accentInk)
                                .frame(width: 2)
                                .offset(x: Space.s4 - PanelRow.hitPadding)
                        }
                    }
                    .alignmentGuide(.firstTextBaseline) { d in
                        d[VerticalAlignment.center] + nameLine
                    }
                    .accessibilityHidden(true)
                columns
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .overlay(alignment: .bottom) { if ruled { HitRule() } }
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
            sender.frame(width: Typo.measure(expanded ? 160 : 104), alignment: .leading)
            VStack(alignment: .leading, spacing: Space.scaled(3)) {
                subject
                preview.lineLimit(expanded ? 1 : 2)
            }
            .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
            date.frame(width: Typo.measure(expanded ? 80 : 72), alignment: .trailing)
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
            .padding(.horizontal, Space.panel)
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
                        .padding(.horizontal, PanelRow.listInset)
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
            selected: selected, cornerRadius: Radius.row, tint: Palette.tierColor(update.tier),
            hPadding: PanelRow.hPadding, vPadding: Space.scaled(5), hoverFill: false, action: onSelect
        ) { _, _ in
            HStack(spacing: Space.s8) {
                Circle()
                    .fill(Palette.tierColor(update.tier))
                    .frame(width: 6, height: 6)
                Text("\(update.importance)")
                    .font(Typo.num(11, weight: .semibold))
                    .foregroundStyle(Palette.importanceColor(update.importance))
                    .frame(width: Typo.measure(24), alignment: .trailing)
                Text(SenderCache.resolved(update.senderString).displayName)
                    .font(Typo.ui(11, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .frame(width: Typo.measure(116), alignment: .leading)
                Text(update.one_line)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text(Fmt.relAge(update.surfaced_at))
                    .font(Typo.num(10))
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(width: Typo.measure(28), alignment: .trailing)
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
