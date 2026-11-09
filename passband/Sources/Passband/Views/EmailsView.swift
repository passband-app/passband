// EMAILS VIEW — the all-mail list: every message, newest day first, each day's
// mail ranked by importance, finished mail kept (dimmed and checked) rather than
// dropped. Rows (`MailRow`) read like a mail client's — sender, subject, the
// triage one-liner, the sitrep zone it was filed under, the arrival time — and
// the list pages back through the whole mailbox as you scroll
// (`AppStore.loadMoreMail`). Owns the "list" KeyContext. No persistent
// selection: the highlight renders only while the keyboard drives, and action
// keys are inert without kbActive or a live hover. The cursor lives in
// `ListCursor`, out of this body; see `cursor` for why.
//
// Above the list, the filter bar: lenses (All / Important / Deadlines /
// Attachments, which narrow the rows) and Reminders (which swaps them for the
// parked list), plus the category picker that hides whole sitrep zones.
//
// A SECOND PAGE, `sent`, shares the chrome and the cursor but not the verbs: it
// holds outbound mail, which nothing triaged and no verb here can resolve. So
// the rows are SentRows, j/k/Enter work exactly as they do everywhere else, and
// r/e/d/v/f are inert rather than acting on a row they have no meaning for.
//
// AND A THIRD, `spam`, which is the mail PROVIDER's folder and not ours. It is
// the only page reached from a chip rather than a segment (the segment appears
// once you are on it — see `MailMode.segments`), it carries a standing notice
// saying who filtered its rows, and it subtracts the three verbs that need a
// triage verdict to mean anything while adding the one verb that only means
// something here: `i`, back to the inbox.
//
// THE REMINDER LENS swaps the rows for mail that is parked (`h`) and waiting to
// come back. A lens rather than a page because it is not a place you navigate
// to — the first Escape after any filter sheds it. Its rows come from their own
// cache, sorted by when each comes back, so they draw with no day headings.

import SwiftUI

struct EmailsView: View {
    @Environment(AppStore.self) private var store

    /// THE CURSOR, in an object of its own rather than three `@State`s. A
    /// `@State` read in this body makes the body depend on it, and the cursor
    /// moves on every hover: each move re-ran this body and re-diffed every
    /// loaded row, which is why scrolling got slower as more mail loaded. Now
    /// only the rows (`MailRowSlot`) and two zero-size leaves read it, so a
    /// hover redraws the rows on screen and nothing else. The forwarding
    /// accessors below keep every key binding reading it as before; they run
    /// when a key fires, not while the body is built.
    @State private var cursor = ListCursor()
    /// THE LIST'S CLOCK, on the minute: what the rows say with the time alone
    /// ("3m", a deadline or reminder chip) used to ride on the redraw every
    /// poll brought, when the header read the stats.
    @State private var minute = Date()
    /// Whether this is the page on screen. The shell keeps Emails mounted
    /// behind other pages (PagePresence), so a visit is this turning true, not
    /// a mount.
    @Environment(\.pageIsActive) private var pageActive
    /// When the page last left the screen, for `trimOnReturn`.
    @State private var leftAt: Date?
    /// Bumped to rebuild the list's scroll at the top (a return after
    /// `trimOnReturn`).
    @State private var scrollEpoch = 0
    private var index: Int {
        get { cursor.index }
        nonmutating set { cursor.index = newValue }
    }
    /// True only while the keyboard is driving the cursor.
    private var kbActive: Bool {
        get { cursor.kbActive }
        nonmutating set { cursor.kbActive = newValue }
    }
    /// True only while the cursor is actually over a row.
    private var hovering: Bool {
        get { cursor.hovering }
        nonmutating set { cursor.hovering = newValue }
    }

    private var mode: MailMode { store.mailMode }
    /// The reminder lens, which only the inbox page has. Read through here so
    /// every row/verb/key below asks one question rather than two.
    private var reminders: Bool { store.reminderFilter && mode == .inbox }
    /// The cached page for whichever INBOX-shaped mode is showing (sent has its
    /// own wire type and its own cache — see `sentPage`), or the parked list
    /// when the lens is on.
    private var page: Loadable<[AttentionUpdate]> {
        reminders ? store.remindersPage : store.mailPage(mode)
    }
    private var sentPage: Loadable<[SentItem]> { store.sentPage }

    /// The cached page MINUS anything resolved since it was fetched: the page
    /// only reloads on the `store.lastRefresh` poll, so without this filter
    /// mail finished from the reader sits here visibly undone until the next tick.
    ///
    /// The parked rows skip it entirely, and must: setting a reminder resolves
    /// the thread, so every id on that list is in `resolvedIds` by construction
    /// and the filter would empty the page it just filled.
    ///
    /// Done mail is in the cache (see `performMailRefresh`) and the row dims.
    /// The filter bar's lens narrows the inbox only: spam has no verdict for
    /// any lens to read.
    ///
    /// Each section is ordered by importance (`MailList.rankedSections`), so the flat
    /// cursor walks rows in the order they are drawn. Read through the memo:
    /// this is asked a dozen times per body, and the body runs on every hover.
    private var listing: MailList.Listing.Output {
        MailList.Listing.shared.of(
            page.value ?? [], showDone: true, filter: filter,
            resolved: reminders ? [] : store.resolvedIds,
            hiddenZones: hiddenZones, grouped: !reminders)
    }
    private var rows: [AttentionUpdate] { listing.rows }
    // FINISHED MAIL ALWAYS SHOWS, dimmed and checked: archived is not gone.
    private var filter: MailFilter { mode == .inbox && !reminders ? store.mailFilter : .all }
    /// The category picker's choice, on the inbox only (the reminder lens is
    /// the user's own parked list and shows whatever they parked).
    private var hiddenZones: Set<String> {
        mode == .inbox && !reminders ? Prefs.shared.mailHiddenZones : []
    }
    /// Finished, as this page draws it. Never on the reminder lens, whose rows
    /// are all resolved by construction and are the point of the lens.
    private func isDone(_ u: AttentionUpdate) -> Bool {
        !reminders && MailList.isDone(u, resolved: store.resolvedIds)
    }
    /// What the reader walks with "done + next" when it opens `u`: the rows
    /// still to do, plus `u` itself even when it is a dimmed one, so the
    /// reader can find where it is and walk on to the next live row.
    private func queue(opening u: AttentionUpdate) -> [AttentionUpdate] {
        rows.filter { !isDone($0) || $0.id == u.id }
    }
    /// Sent rows are NOT filtered against `resolvedIds`: a resolve is an inbox
    /// verdict, and it must never delete a record of something you sent.
    private var sent: [SentItem] { sentPage.value ?? [] }
    /// THE OUTBOX, pinned above the sent mail (docs/SCHEDULED-SEND.md §6),
    /// next to go first. Empty on every other page, so the cursor arithmetic
    /// below is the old arithmetic everywhere but Sent, and on Sent whenever
    /// nothing is scheduled.
    private var scheduled: [ScheduledSendView] { mode == .sent ? store.scheduledRows : [] }

    private var selected: AttentionUpdate? { rows[safe: index] }
    /// ONE CURSOR over both groups on Sent: positions `0..<scheduled.count`
    /// are the outbox, the rest are the sent rows after it.
    private var selectedScheduled: ScheduledSendView? { scheduled[safe: index] }
    private var selectedSent: SentItem? { sent[safe: index - scheduled.count] }
    /// However many rows the page on screen has — the cursor's only bound.
    private var rowCount: Int { mode == .sent ? scheduled.count + sent.count : rows.count }
    /// What the cursor is anchored against: the outbox ids above the sent
    /// rows (none off Sent) and the page's length.
    private struct CursorShape: Equatable {
        var outbox: [Int]
        var count: Int
        /// The mail rows' ids in drawn order (none on Sent), so a poll or an
        /// older page that inserts rows ABOVE the cursor (importance ranks a
        /// new arrival mid-day) moves the cursor with its row instead of
        /// leaving it on a stranger's mail.
        var ids: [Int]
    }
    /// The row under the cursor as the two things every page can answer: the
    /// message id (a `.task` key, and the scroll target) and the thread to warm.
    /// An outbox row answers only when it has a thread (a reply's), which is
    /// the thread the preview's `edit` opens.
    private func cursorRow(at i: Int) -> (id: Int, threadId: String)? {
        if mode == .sent {
            if let row = scheduled[safe: i] {
                return row.thread_id.map { (row.id, $0) }
            }
            return sent[safe: i - scheduled.count].map { ($0.id, $0.thread_id) }
        }
        return rows[safe: i].map { ($0.id, $0.thread_id) }
    }
    /// The outbox row the keys may act on: under the keyboard cursor or the
    /// pointer, the same `actionable` rule every verb here keeps.
    /// A `sending` row is never actionable: it is in Gmail's hands.
    private var actionableScheduled: ScheduledSendView? {
        guard actionable, let row = selectedScheduled, row.state != "sending" else { return nil }
        return row
    }
    /// Action keys are inert unless something is actually highlighted.
    private var actionable: Bool { kbActive || hovering }
    /// The triage verbs on top of that: sent mail has no triage to act on.
    private var triageable: Bool { actionable && mode != .sent }
    /// And the narrower set that needs a VERDICT to act on, which a spam row has
    /// never had. `v` would correct a call nobody made, `h` would park the row
    /// into bands that filter spam out (the daemon refuses the stamp outright),
    /// and `f` would search a corpus spam is excluded from. Three keys that
    /// would appear to work and do nothing.
    ///
    /// `e`/`d` and `r` stay live: finishing with a row and answering one are
    /// things you can mean about any mail, verdict or no verdict.
    private var verdicted: Bool { triageable && mode != .spam }

    var body: some View {
        let _ = minute
        VStack(spacing: 0) {
            header
                .task {
                    await clockTicks(every: 60, while: { Platform.isAppOnScreen }) {
                        // Not on a fresh mount, which has just drawn with now.
                        let now = Date()
                        if abs(now.timeIntervalSince(minute)) >= 1 { minute = now }
                    }
                }
            // ABOVE the list rather than inside it, so it does not scroll away.
            // The sentence answers "who put this here", and that question is
            // asked at row 40 as readily as at row 1.
            if mode == .spam {
                // On the list's inset, the edge the rows' selection fill
                // reaches: its outline lines up with the fill and its words
                // with the rows' (SpamNotice pads by the rows' own padding).
                SpamNotice()
                    #if os(macOS)
                        .padding(.horizontal, DeskRow.listInset)
                    #else
                        .padding(.horizontal, Space.scaled(18))
                    #endif
                    .padding(.top, Space.s10)
            }
            if mode == .inbox {
                // The page's OWN rows for the counts, never the parked list the
                // reminder lens swaps in.
                MailFilterBar(
                    counts: MailList.Counts.shared.of(
                        // Counts are of mail STILL TO DO: "Important 12" is
                        // twelve things to look at, not twelve ever received.
                        store.mailPage(mode).value ?? [], showDone: false,
                        resolved: store.resolvedIds,
                        hiddenZones: Prefs.shared.mailHiddenZones))
            }
            #if os(macOS)
                desktopList
            #else
                phoneList
            #endif
        }
        .keyBindings(.list, bindings)
        // Fires on mount, on each poll AND on a mode switch — only the page
        // being shown is refreshed, so the spam page costs nothing until you go
        // there. The store's short TTL is what makes the mount half free when a
        // tick just landed; the tick half always outruns it and refreshes for real.
        // IN A LEAF, not on this body: the tick is a new Date every poll, and
        // a body that reads it re-evaluates every poll —
        // every loaded row's worth of view values, for a fetch that usually
        // finds nothing new. The leaf's body is a zero-size rectangle.
        // Only on screen: a page kept behind another refreshed nothing before
        // it was kept, and shouldn't now. A visit remounts it, which refreshes.
        .background { if pageActive { MailRefreshKeeper(mode: mode, reminders: reminders) } }
        // ASK FOR THE SPAM FOLDER ON ENTRY, and only on entry: a switch to
        // the spam page, or a return to Emails while on it. Not on the poll
        // tick: the daemon does not track that
        // folder, so this is a real Gmail round trip and it costs one per visit
        // instead of one every ten seconds.
        .task(id: SpamVisit(mode: mode, onScreen: pageActive)) {
            guard mode == .spam, pageActive else { return }
            await store.refreshSpamFolder()
        }
        // Pull the thread for the row the cursor rests on, DEBOUNCED so sweeping
        // a long list fires one request for the row you stop on rather than
        // one per row you pass. Covers everything past the bounded head-warm.
        // In a LEAF so that only it, not this body, depends on the cursor.
        .background { if pageActive { CursorPrefetch(cursor: cursor, rowAt: { cursorRow(at: $0) }) } }
        // ONE HANDLER for the cursor's bound and the outbox's shifts, in that
        // order: re-anchor to the row it was on (a poll that adds or drops an
        // outbox row shifts every position under it, and the next Enter / `s`
        // / `x` would act on somebody else's mail), THEN clamp to the page.
        // Two handlers fired in the same update in no set order, and clamping
        // first moved the cursor one row too high (`ScheduledCursor.follow`).
        // Off Sent the outbox is `[]` on both sides, so this is the plain
        // clamp, and reads nothing of the store's outbox.
        .onChange(
            of: CursorShape(
                outbox: scheduled.map(\.id), count: rowCount,
                ids: mode == .sent ? [] : rows.map(\.id))
        ) { old, new in
            if let id = old.ids[safe: index], let moved = new.ids.firstIndex(of: id) {
                if moved != index { index = moved }
                return
            }
            index = ScheduledCursor.follow(
                index, old: old.outbox, new: new.outbox, count: new.count)
        }
        // Back to the head on a first visit, or a return after a while, so
        // rows deep in the scroll are not served from an afternoon ago
        // (`trimMailToHead`). A quick return keeps the scroll: that is what
        // keeping the page mounted is for.
        .onPageShown {
            if let left = leftAt, Date().timeIntervalSince(left) >= Self.trimOnReturn {
                // Back to the top with the head: a scroll left deep in rows
                // that are no longer loaded would land on the footer and ask
                // for them all again.
                store.trimMailToHead(mode)
                resetCursor()
                scrollEpoch &+= 1
            } else if leftAt == nil {
                store.trimMailToHead(mode)
            }
            leftAt = nil
        }
        .onPageHidden { leftAt = Date() }
        // A mode switch replaces every row, so the cursor cannot keep its index —
        // it would land on an unrelated email, and the verbs act on the highlight.
        // Flipping the lens replaces them just as completely.
        // The lens goes with it: a page switch is a fresh page, and a filter
        // that survived one would silently re-narrow the inbox on the way back.
        .onChange(of: mode) { _, _ in
            store.reminderFilter = false
            store.mailFilter = .all
            resetCursor()
        }
        .onChange(of: reminders) { _, _ in resetCursor() }
        .onChange(of: store.mailFilter) { _, _ in resetCursor() }
        .onChange(of: Prefs.shared.mailHiddenZones) { _, _ in resetCursor() }
        // Jump to (and highlight) a hand-off target from the sitrep rails.
        // ON SCREEN ONLY: the page is kept mounted behind the board, where the
        // selection moves with every `e` and every open, and a highlight
        // following it there would be waiting, live under the verb keys, on a
        // row nobody picked. A hand-off (`viewInEmails`) routes here and sets
        // the selection in one go, so a CHANGED selection arrives with the page
        // on screen. A first visit (nothing mounted yet) or a selection that
        // already held that id fires nothing, as it never did before the page
        // was kept.
        .onChange(of: store.selectedId) { _, id in
            guard pageActive, let id, let i = rows.firstIndex(where: { $0.id == id }) else { return }
            kbActive = true
            index = i
        }
        // And the highlight goes out with the page, as it did when leaving
        // unmounted it: a return finds the row the cursor was on, unarmed.
        .onPageHidden { kbActive = false }
    }

    // MARK: - the lists

    /// The inline notes for the mail and spam pages, shared by both
    /// layouts. Each is gated on having NO rows at all: a reload keeps the last
    /// page on screen, so a revisit — or a failure while offline — updates
    /// underneath what you are already reading instead of replacing it with a
    /// word. The sent page carries its own notes inside `sentList`.
    @ViewBuilder
    private var note: some View {
        if let error = page.error, page.value == nil {
            BandNote(error)
        } else if page.value == nil {
            BandNote(
                reminders
                    ? "Loading reminders…" : mode == .spam ? "Loading spam…" : "Loading mail…")
        } else if rows.isEmpty {
            // A narrowed list that is empty may only be empty SO FAR: the
            // footer under this note keeps paging back, and the copy says so
            // until the mailbox runs out.
            //
            // THE SPAM PAGE HAS THREE EMPTY STATES, not one, and they are the
            // same empty list on the wire. The daemon does not track that
            // folder, so "nothing here" is only true once it has actually
            // looked; saying it while a fetch is in flight, or after one
            // failed, would be inventing an answer.
            BandNote(
                reminders
                    ? "No pending reminders."
                    : filter != .all
                        ? (store.mailPaging(mode).exhausted
                            ? "No mail matches \(filter.label)."
                            : "Nothing matches \(filter.label) yet. Looking further back.")
                        : !hiddenZones.isEmpty
                            ? (store.mailPaging(mode).exhausted
                                ? "No mail in the categories you picked."
                                : "Nothing in the categories you picked yet. Looking further back.")
                            : mode == .spam ? spamEmptyNote : "No mail.")
        }
    }

    /// Which of the spam page's three empty states this is. Only the last one
    /// is a statement about the mailbox; the other two are statements about
    /// this client's knowledge of it.
    private var spamEmptyNote: String {
        if store.spamFetchInFlight {
            return "Checking your email provider's spam folder…"
        }
        if store.spamFetchStalled || !store.spamEverSynced {
            return "Could not reach your email provider's spam folder."
        }
        return "Your email provider filtered nothing in the last 30 days."
    }

    /// The Sent page: the outbox group, then the sent rows.
    @ViewBuilder private var sentList: some View {
        scheduledGroup
        sentRows
    }

    /// THE OUTBOX GROUP, headed "scheduled" in the app's section-label style
    /// and absent when there is nothing in it (then the page is exactly the
    /// sent list it always was). When it shows, a "sent" label heads the rows
    /// under it, or the first sent mail would read as one more scheduled one.
    @ViewBuilder private var scheduledGroup: some View {
        if !scheduled.isEmpty {
            SentSectionLabel("scheduled", first: true)
            ForEach(Array(scheduled.enumerated()), id: \.element.id) { i, row in
                ScheduledRow(
                    row: row,
                    groupName: store.scheduledGroupNames[row.id],
                    selected: kbActive && i == index,
                    onHover: {
                        hovering = true
                        kbActive = false
                        index = i
                    },
                    onOpen: { store.openScheduledPreview(row) },
                    onEdit: { store.pullBackScheduled(row) },
                    onSendNow: { store.sendScheduledNow(row) },
                    onCancel: { store.cancelScheduledRow(row) }
                )
                .id(Self.scheduledRowId(row.id))
                #if !os(macOS)
                    // The thumb's two verbs. Cancel on the trailing edge and
                    // NOT a full swipe: it has an undo, but a flick past a row
                    // is not a decision about somebody's mail. Send now on
                    // the leading edge, never full-swipe either: it is the
                    // one verb here with no undo at all.
                    .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                        if row.state != "sending" {
                            Button(role: .destructive) {
                                Haptics.commit()
                                store.cancelScheduledRow(row)
                            } label: {
                                Label("cancel", systemImage: "xmark")
                            }
                        }
                    }
                    .swipeActions(edge: .leading, allowsFullSwipe: false) {
                        if row.state != "sending" {
                            Button {
                                Haptics.commit()
                                store.sendScheduledNow(row)
                            } label: {
                                Label("send now", systemImage: "paperplane")
                            }
                            .tint(Palette.accent)
                        }
                    }
                #endif
            }
            SentSectionLabel("sent", first: false)
        }
    }

    /// An outbox row's scroll target. A string, so it can never collide with
    /// a sent row's message id in the same scroll view.
    private static func scheduledRowId(_ id: Int) -> String { "scheduled-\(id)" }

    /// The sent rows, same three gated notes. The reader opens with NO queue:
    /// "done + next" walks a triage list, and there is nothing to finish here.
    /// Their cursor positions start after the outbox group's.
    @ViewBuilder private var sentRows: some View {
        let offset = scheduled.count
        if let error = sentPage.error, sentPage.value == nil {
            BandNote(error)
        } else if sentPage.value == nil {
            BandNote("loading sent…")
        } else if sent.isEmpty {
            BandNote("Nothing sent yet.")
        } else {
            ForEach(Array(sent.enumerated()), id: \.element.id) { i, item in
                SentRow(
                    item: item,
                    selected: kbActive && i + offset == index,
                    onHover: {
                        hovering = true
                        kbActive = false
                        index = i + offset
                    },
                    onOpen: { store.openThread(item.thread_id, from: .emails) }
                )
                .id(item.id)
            }
        }
    }

    /// The scroll target for a row position on whichever page is showing.
    private func rowId(at i: Int) -> AnyHashable? {
        if mode == .sent {
            if let row = scheduled[safe: i] { return Self.scheduledRowId(row.id) }
            return sent[safe: i - scheduled.count]?.id
        }
        return rows[safe: i]?.id
    }

    /// The day sections, cached on the rows they were built from: the body
    /// re-evaluates on every cursor move and hover, and rebuilding the
    /// headings each time is a measurable stutter on a long list.
    private var sections: [MailList.Section] { listing.sections }

    private var hasRows: Bool { page.value != nil && page.error == nil && !rows.isEmpty }

    #if os(macOS)
        private var desktopList: some View {
            // Measured as a whole so every mail row draws the same columns
            // (`MailColumns`): narrower ones beside the compose pane.
            MailColumnsReader {
                desktopScroller
            }
        }

        private var desktopScroller: some View {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 1) {
                        if mode == .sent {
                            sentList
                        } else if !hasRows {
                            note
                            // An empty LENS keeps paging back for a match.
                            if page.value != nil, page.error == nil { olderMailFooter }
                        } else if reminders {
                            // Sorted by when each comes BACK, so a heading of
                            // when each arrived would scatter the list.
                            ForEach(Array(rows.enumerated()), id: \.element.id) { i, u in
                                mailRow(u, at: i)
                            }
                        } else {
                            ForEach(sections, id: \.id) { section in
                                DayHeading(section.title, first: section.start == 0)
                                ForEach(Array(section.rows.enumerated()), id: \.element.id) {
                                    j, u in
                                    mailRow(u, at: section.start + j)
                                }
                            }
                            olderMailFooter
                        }
                    }
                    // The rows' fill edge. Their padding carries the words the
                    // rest of the way to the page gutter, and every heading and
                    // note in here pads by the same amount to meet them.
                    .padding(.horizontal, DeskRow.listInset)
                    .padding(.vertical, Space.s10)
                }
                .id(scrollEpoch)
                // Follow the KEYBOARD selection only. Both pages key their rows
                // on the message id, so one scroll target serves both. A leaf,
                // like the prefetch, so the cursor stays out of this body.
                .background {
                    CursorScrollFollower(cursor: cursor, proxy: proxy, idAt: { rowId(at: $0) })
                }
            }
            // Only LEAVING the list ends the hover, as on the sitrep: this was
            // `.onContinuousHover`, which runs for every mouse-moved event
            // over the list, and only ever needed its end.
            .onHover { over in
                if !over, hovering { hovering = false }
            }
        }
    #endif

    #if os(macOS)
        /// One row of the inbox / noise / spam list at flat position `i`.
        private func mailRow(_ u: AttentionUpdate, at i: Int) -> some View {
            let count = rows.count
            let mode = mode
            let reminders = reminders
            return MailRowSlot(
                cursor: cursor,
                position: i,
                update: u,
                done: isDone(u),
                // Only the spam page's hint: its `i` has no other affordance on
                // the Mac. Elsewhere the hint cost the selected row's subject
                // fifty points and reflowed it on every j/k.
                verbHint: mode == .spam ? "[i][e][d]" : nil,
                onOpen: { store.openThread(u.thread_id, queue: queue(opening: u), from: .emails) },
                // INFINITE SCROLL: a row this close to the end of what is
                // loaded asks for the next page, well before the bottom shows.
                onAppear: {
                    guard !reminders, i >= count - MailList.loadAhead else { return }
                    Task { await store.loadMoreMail(mode) }
                }
            )
            .id(u.id)
        }

    #endif

    /// Under the last row: a quiet note while an older page is on its way, a
    /// way to try again when one failed, and nothing once the mailbox has run
    /// out. Both layouts.
    @ViewBuilder private var olderMailFooter: some View {
        let paging = store.mailPaging(mode)
        let mode = mode
        if reminders || mode == .sent || paging.exhausted {
            EmptyView()
        } else if paging.loadingMore {
            BandNote("Loading older mail…")
        } else if paging.failedAt != nil {
            HStack(spacing: Space.s10) {
                Text("Couldn't load older mail.")
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaintest)
                Button("Try again") { store.retryOlderMail(mode) }
                    .buttonStyle(.plain)
                    .font(Typo.chip)
                    .foregroundStyle(Palette.accent)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Space.s12)
            .padding(.vertical, Space.scaled(18))
        } else {
            // A SENTINEL as well as the rows' own trigger: when a lens or the
            // category picker hides most of a page, the rows that would have
            // asked for the next one never appear. Keyed on how much is
            // loaded, so it appears again after every page.
            Color.clear
                .frame(height: 1)
                .id("older-\(store.mailPage(mode).value?.count ?? 0)")
                .onAppear { Task { await store.loadMoreMail(mode) } }
        }
    }

    #if !os(macOS)
        /// A REAL `List` on the phone, and only on the phone. Everything else in
        /// this app draws its rows into a LazyVStack because the Mac's lists are
        /// keyboard surfaces that need a cursor, an anchor and a scroll-to; none
        /// of that survives contact with a thumb, and what a thumb wants instead —
        /// `swipeActions`, full-swipe commit, the rubber-band, the system's own
        /// timing — is something only `List` can hand over. Reimplementing it
        /// would be a worse copy of a control the OS ships.
        ///
        /// The design skin survives intact: plain style, no separators, clear row
        /// backgrounds, and the app's own `UpdateRow` inside. What the List
        /// contributes is the gesture, not the look.
        private var phoneList: some View {
            List {
                if mode == .sent {
                    // Sent rows carry no triage, so no swipe verbs — the row is
                    // a door to the thread and nothing else (`triageable` says
                    // the same thing to the keyboard).
                    sentList.plainRow(bleed: true)
                } else if !hasRows {
                    note.plainRow()
                    if page.value != nil, page.error == nil { olderMailFooter.plainRow() }
                } else if reminders {
                    ForEach(rows) { u in phoneRow(u) }
                } else {
                    ForEach(sections, id: \.id) { section in
                        Section {
                            ForEach(section.rows) { u in phoneRow(u) }
                        } header: {
                            DayHeading(section.title, first: section.start == 0)
                        }
                    }
                    olderMailFooter.plainRow()
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, 0)
        }

        @ViewBuilder private func phoneRow(_ u: AttentionUpdate) -> some View {
            let verbs = UpdateVerbs(update: u, queue: queue(opening: u), finished: isDone(u))
            UpdateRow(
                update: u,
                selected: false,
                onHover: {},
                onOpen: verbs.open
            )
            .opacity(isDone(u) ? 0.6 : 1)
            .plainRow(bleed: true)
            .swipeVerbs(verbs)
            .updateContextMenu(verbs)
        }
    #endif

    // MARK: - header

    /// A Mac's page header is also the app's chrome bar: wordmark, counts, the
    /// freshness stamp, and the door to the process page. The auth / audit /
    /// shortcuts / theme chips that used to line up here are gone, not moved:
    /// every one of them already has a rail button or a global key (g, A, ?, \),
    /// and a second door in the chrome was chrome for its own sake. A phone has
    /// a navigation bar for the name and a tab bar for the doors, so all that is
    /// left up here is the one thing this page owns — which of the two lists you
    /// are looking at, and how much is in the other one.
    @ViewBuilder
    private var header: some View {
        #if os(macOS)
            desktopHeader
        #else
            phoneHeader
        #endif
    }

    #if !os(macOS)
        /// Whether the phone bar offers the spam door: on the inbox, whenever
        /// the folder has something in it. The noise door that used to share
        /// this slot is gone with the noise page.
        private var showSpamDoor: Bool { store.spamDoorVisible && mode == .inbox }
        /// The count beside it, when there is one to show.
        private var spamDoorCount: Int? {
            store.spamCount.flatMap { $0 > 0 ? $0 : nil }
        }

        private var phoneHeader: some View {
            @Bindable var store = store

            return HStack(spacing: Space.s10) {
                GlassSegmented(
                    options: MailMode.segments(showingSpam: mode == .spam).map { ($0, $0.label) },
                    selection: $store.mailMode)
                Spacer(minLength: Space.s8)
                if let err = store.refreshError {
                    Text("offline")
                        .font(Typo.micro).foregroundStyle(Palette.warn)
                        .help(err.message)
                } else if showSpamDoor {
                    // THE PHONE'S SPAM DOOR: this bar has the segmented
                    // control plus room for one count, and this is it.
                    Button { store.mailMode = .spam } label: {
                        HStack(spacing: Space.s6) {
                            if let spam = spamDoorCount {
                                Text("\(spam)")
                                    .font(Typo.num(12, weight: .bold))
                                    .foregroundStyle(Palette.inkFaintest)
                            }
                            Text("spam").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, Space.s16)
            .padding(.bottom, Space.s10)
        }
    #endif

    #if os(macOS)
    private var desktopHeader: some View {
        @Bindable var store = store
        let signal = store.sitrep.standing.count + store.sitrep.new.count + store.sitrep.open.count

        return HStack(spacing: Space.s10) {
            // NO MODE LABEL BESIDE THE WORDMARK. It said what the segmented
            // control an inch to its right already says, and it said it in a
            // string whose width changed with the answer — so every switch
            // between all mail / noise / sent shoved the control and the whole
            // run of chrome after it sideways. The one thing in this bar that
            // moved was the one thing that was repeating itself.
            Text("passband")
                .font(Typo.serif(19, weight: .medium))
                .foregroundStyle(Palette.ink)
            GlassSegmented(
                options: MailMode.segments(showingSpam: mode == .spam).map { ($0, $0.label) },
                selection: $store.mailMode)
            Spacer(minLength: Space.s12)
            HStack(spacing: Space.s8) {
                Text("\(signal)").font(Typo.num(12, weight: .bold)).foregroundStyle(Palette.accent)
                Text("signal").font(Typo.micro).foregroundStyle(Palette.inkFaint)
                // No noise door: all mail shows every message, noise included,
                // each labelled with where the sitrep filed it.
                // THE SPAM DOOR, in the same shape as the old noise count
                // and one step quieter at rest (inkFaintest against its
                // inkFaint), absent entirely when the folder is empty or the
                // daemon is too old to answer. It still takes the accent while
                // you are ON the page, exactly as the noise count does: that
                // colour is not emphasis, it is where-you-are, and dropping it
                // would leave the spam page the one list in the app whose door
                // does not light up under you.
                //
                // It sits here rather than in the segmented control because
                // spam is a place you go looking for something, once, not one
                // of the three lists you live in.
                if store.spamDoorVisible {
                    Button { store.mailMode = .spam } label: {
                        HStack(spacing: Space.s8) {
                            // THE NUMBER ONLY WHEN THERE IS ONE. The daemon does
                            // not track this folder, so before the first visit
                            // there is no count to show and a "0" would be a
                            // claim nobody has checked.
                            if let spam = store.spamCount, spam > 0 {
                                Text("\(spam)")
                                    .font(Typo.num(12, weight: .bold))
                                    .foregroundStyle(
                                        mode == .spam ? Palette.accent : Palette.inkFaintest)
                            }
                            Text("spam")
                                .font(Typo.micro)
                                .foregroundStyle(
                                    mode == .spam ? Palette.accent : Palette.inkFaintest)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("mail your email provider filtered out. Opening this fetches it")
                }
                // Offline still says so. The "last checked" stamp is gone:
                // the list's own times say how fresh it is.
                if let err = store.refreshError {
                    Text("· offline")
                        .font(Typo.micro).foregroundStyle(Palette.warn)
                        .help(err.message)
                }
            }
            // The reminder lens moved into the filter bar, beside the others.
            RetriageButton()
        }
        // These metrics must match the sitrep masthead's: every page's bar ends
        // on the line the rail's top edge is cut to, and its words start on the
        // page gutter.
        .padding(.horizontal, Space.page)
        .frame(height: TopBar.height)
        .overlay(alignment: .bottom) { Hairline() }
    }
    #endif

    // MARK: - keymap

    private var bindings: [KeyBinding] {
        [
            KeyBinding("j", "next") { moveByKey(+1) },
            KeyBinding("k", "prev") { moveByKey(-1) },
            KeyBinding("ArrowDown", "next") { moveByKey(+1) },
            KeyBinding("ArrowUp", "prev") { moveByKey(-1) },
            // A LADDER, the same shape as the reader closing over a still-open
            // side panel: the lens comes off first, then any page that is not
            // the inbox steps back to it, and only then does Escape leave the
            // tab. Each rung undoes the narrowing you did last.
            KeyBinding("Escape", "back") {
                if filter != .all {
                    store.mailFilter = .all
                } else if reminders {
                    store.reminderFilter = false
                } else if mode != .inbox {
                    store.mailMode = .inbox
                } else {
                    store.setView(.sitrep)
                }
            },
            KeyBinding("Enter", "drill in") {
                guard actionable else { return }
                if mode == .sent {
                    // An outbox row opens READ-ONLY in the reader's slot, as
                    // scheduled: NEVER into the composer (only the preview's
                    // `edit` does that). A `sending` row opens too, for its
                    // status; the preview offers it no verbs.
                    if let row = selectedScheduled {
                        store.openScheduledPreview(row)
                        return
                    }
                    guard let item = selectedSent else { return }
                    // No queue: "done + next" walks a triage list, and sent mail
                    // has nothing to finish.
                    store.openThread(item.thread_id, from: .emails)
                } else if let u = selected {
                    // The ordered rows become the viewer's queue, so "done + next"
                    // (e/d) can advance in place.
                    store.openThread(u.thread_id, queue: queue(opening: u), from: .emails)
                }
            },
            KeyBinding("v", "fix triage") {
                guard verdicted, let u = selected else { return }
                store.openTriageFix(
                    TriageFixTarget(
                        messageId: u.id, sender: u.sender, subject: u.one_line))
            },
            // `t` = teach Passband from the HIGHLIGHTED row. It lives here, beside the
            // other row verbs, because this list's cursor is its own: the shared
            // layer's old `t` read the sitrep's selection and opened a rule for
            // whoever was first on that board instead.
            KeyBinding("t", "teach Passband") {
                guard verdicted, let u = selected else { return }
                Actions.teach(sender: u.sender, messageId: u.id)
            },
            // Reply opens the email and composes in it, so it hands over the same
            // queue Enter does — done + next keeps working from inside the reader.
            KeyBinding("r", "reply") {
                guard triageable, let u = selected else { return }
                Actions.reply(u, queue: queue(opening: u))
            },
            // `sender` IS the address on this wire type (see AttentionUpdate's
            // SenderStringConvertible note) — the display name would search the
            // body text of unrelated mail. Inert on the sent page, where the
            // sender is the reader and the seed would find their whole archive.
            KeyBinding("f", "search this sender") {
                guard verdicted, let u = selected else { return }
                store.openSearch(seed: "from:\(u.sender)")
            },
            KeyBinding("e", "done") { resolveSelected() },
            KeyBinding("d", "done") { resolveSelected() },
            // On the lens it reschedules the row it is already showing — the
            // palette is the same either way.
            KeyBinding("h", "remind me later") {
                guard verdicted, let u = selected else { return }
                store.openRemind(
                    RemindTarget(
                        messageId: u.id, sender: u.sender, subject: u.one_line,
                        remindAt: u.remind_at))
            },
            // THE OUTBOX ROW'S TWO VERBS, bound on Sent only and DECLINED
            // anywhere else, so neither letter means anything on the other
            // pages. `s` sends it on the worker's next tick; `x` cancels it
            // behind the undo toast, and so do Backspace / Delete, the keys
            // the drafts page deletes a draft with.
            // EXACT CASE, both: a shifted S or X is not the verb.
            KeyBinding(declining: "s", "send scheduled mail now", exactCase: true) {
                guard let row = actionableScheduled else { return false }
                store.sendScheduledNow(row)
                return true
            },
            KeyBinding(declining: "x", "cancel scheduled mail", exactCase: true) {
                guard let row = actionableScheduled else { return false }
                store.cancelScheduledRow(row)
                return true
            },
            // AHEAD of the reminder lens's Backspace, and declining, so that
            // binding is untouched: the two never share a page.
            KeyBinding(declining: "Backspace", "cancel scheduled mail") {
                guard let row = actionableScheduled else { return false }
                store.cancelScheduledRow(row)
                return true
            },
            KeyBinding(declining: "Delete", "cancel scheduled mail") {
                guard let row = actionableScheduled else { return false }
                store.cancelScheduledRow(row)
                return true
            },
            // Cancel a reminder WITHOUT reopening the mail — it was resolved
            // when the reminder was set, and "I do not need to see this again"
            // is a statement about the reminder, not about the email. Bound
            // only on the lens: everywhere else Backspace has no row to mean.
            KeyBinding("Backspace", "cancel reminder") {
                guard reminders, actionable, let u = selected else { return }
                Task { await Actions.cancelReminder(u) }
            },
            // THE SPAM PAGE'S ONE VERB, and it only exists there: `i` for
            // inbox, which is both where the mail goes and what the key does.
            // Bound on this page alone rather than everywhere-and-inert,
            // because there is no other page where "move this out of spam"
            // names anything — mail elsewhere is already out of it.
            KeyBinding("i", "not spam · move to inbox") {
                guard mode == .spam, actionable, let u = selected else { return }
                Task { await Actions.notSpam(u, via: .key, on: .emails) }
            },
            KeyBinding("a", "browse all") { store.openSide(.browse) },
            KeyBinding("T", "rules") { store.openSettings(.rules) },
            // The audit log is a settings pane now, so `A` opens Settings ON
            // it rather than a routed page of its own.
            KeyBinding("A", "audit log") { store.openSettings(.audit) },
            KeyBinding("g", "auth messages") { store.setView(.auth) },
            // `u` (undo), `\` (theme) and `?` (help) are global bindings, not
            // listed here.
        ]
    }

    /// `.task(id:)` key: a poll tick and a mode switch both have to refetch, and a
    /// tuple of the two cannot be Hashable.
    private func moveByKey(_ delta: Int) {
        kbActive = true
        index = max(0, min(rowCount - 1, index + delta))
    }

    private func resetCursor() {
        index = 0
        kbActive = false
        hovering = false
    }

    private func resolveSelected() {
        guard triageable, let u = selected, !isDone(u) else { return }
        Task { await Actions.done(u, via: .key, on: .emails) }
        // The row stays put (dimmed), so nothing slides under the cursor: step
        // to the next row still to do,
        // or "e e e" stalls on the first one.
        if kbActive,
            let next = rows.indices.first(where: { $0 > index && !isDone(rows[$0]) && rows[$0].id != u.id })
        {
            index = next
        }
        // NOT evicted from the cached page: `Actions.done` records the id in
        // `resolvedIds`, which is all `rows` needs to dim the row on the next
        // frame. A poll that lands before
        // the server agrees still cannot hand it back for the same reason, and
        // an undo, which clears the id, brings it back at once.
    }
}

/// A group heading inside the Sent list, in the drafts page's section style.
/// Padded by the rows' own padding, which puts the words on their text edge.
private struct SentSectionLabel: View {
    let text: String
    let first: Bool
    init(_ text: String, first: Bool) {
        self.text = text
        self.first = first
    }

    var body: some View {
        Text(text)
            .font(Typo.sectionLabel)
            .foregroundStyle(Palette.inkFaint)
            .textCase(.uppercase)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Space.s12)
            // DayHeading's rhythm, so the two pages' labels sit alike.
            .padding(.top, SectionLabelRhythm.top(first: first))
            .padding(.bottom, SectionLabelRhythm.bottom)
    }
}

/// An inline note inside the list (loading / empty / error).
struct BandNote: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(Typo.rowSub)
            .foregroundStyle(Palette.inkFaintest)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Space.s12)
            .padding(.vertical, Space.scaled(18))
    }
}


/// Refreshes the page being shown on mount, on every poll tick, and on a mode
/// switch. Only the page being shown, so the spam page costs nothing until
/// you go there. The store's short TTL is what makes the mount half free when
/// a tick just landed; the tick half always outruns it and refreshes for
/// real. A view of its own so that reading the tick invalidates nothing but
/// itself — see the call site in EmailsView.
private struct MailRefreshKeeper: View {
    @Environment(AppStore.self) private var store
    let mode: MailMode
    let reminders: Bool

    private struct RefreshKey: Hashable {
        var tick: Date?
        var mode: MailMode
        var reminders: Bool
    }

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .task(id: RefreshKey(tick: store.lastRefresh, mode: mode, reminders: reminders)) {
                if reminders {
                    await store.refreshReminders()
                } else if mode == .sent {
                    await store.refreshSent()
                } else {
                    await store.refreshMail(mode)
                }
            }
    }
}

/// The spam folder is asked for on each VISIT to the spam page: a mode switch
/// to it, or a return to Emails while on it.
private struct SpamVisit: Equatable {
    var mode: MailMode
    var onScreen: Bool
}

extension EmailsView {
    /// How long the page can be away before a return goes back to the head
    /// of the list rather than where it was.
    static let trimOnReturn: TimeInterval = 10 * 60
}
