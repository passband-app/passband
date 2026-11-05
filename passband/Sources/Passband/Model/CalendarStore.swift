// The calendar page's half of the store: the gate, the timeline load, the
// user's word on a ghost, the connect card's lifecycle, the event pane's
// lifecycle, and the three writes to a real calendar (create, edit, delete). The stored state
// it works on is declared in AppStore itself (an @Observable class keeps its
// stored properties in its own body); everything here is behaviour.
//
// Shared with the phone target, which has no calendar page yet: nothing here
// draws, and nothing here is reachable there until something calls it.
//
// ONE ACCOUNT AT A TIME. Every calendar answer derived from `stats` goes
// through `calendarAccess`, which is nil until the active account's own first
// poll has landed (`CalendarGate.access`); the card and the wait carry the
// epoch they were made under and mean nothing under any other.

import Foundation
import SwiftUI

/// The connect card, while it is up: how this mailbox connects, and which
/// Google account to pick when Google asks. `email` is nil on a daemon too old
/// to name its own address; the card then says nothing about accounts rather
/// than guessing one.
struct CalendarConnectAsk: Hashable {
    let route: CalendarGate.ConnectRoute
    let email: String?
    /// The store epoch the card was raised under: the account it is FOR. A
    /// card whose epoch is not the live one is not drawn and does nothing.
    let epoch: Int
    /// Raised to renew a granted calendar whose token Google stopped
    /// accepting, rather than to connect one for the first time.
    var renewing = false
}

/// Connect was pressed and Google's consent is open in the browser: since
/// when, and for which account. The same wait for both routes.
struct CalendarConnectWait: Hashable {
    let since: Date
    let epoch: Int
    let renewing: Bool
}

/// The event pane, while it is open: new, or an event being changed.
struct CalendarEditorRequest: Hashable, Identifiable {
    enum Mode: Hashable {
        case create
        case edit(TimelineEvent)
    }

    let id = UUID()
    /// The id a create's event is made under, the same on every send from
    /// this pane (`CalendarWrite.createWrite`). After an answer that never
    /// came the pane freezes and can only send the same body again, which
    /// the daemon answers with the first event rather than making another.
    let createId = CalendarWrite.newEventId()
    let mode: Mode
    /// What the pane opened with. An edit sends only what moved from this,
    /// and a draft that still equals it has not been touched.
    let initial: EventDraft
    /// The account it is for, as the connect card's is.
    let epoch: Int

    var event: TimelineEvent? {
        if case .edit(let e) = mode { return e }
        return nil
    }
}

/// "Delete this event from Google Calendar?", asked from the detail pane.
struct CalendarDeleteAsk: Hashable {
    let event: TimelineEvent
    let epoch: Int
}

extension AppStore {
    // MARK: - the gate

    // `calendarPageAvailable`, the gate itself, is stored in AppStore and
    // written from `stats` only when it flips.

    /// The gate closed under the user (an older daemon, one that serves no
    /// timeline): leave the page for the Sitrep if it is up, close the event
    /// pane and the delete question, and drop everything the page held. An
    /// unsaved draft in the pane is DISCARDED: the page it belongs to no
    /// longer exists for this mailbox, and the daemon would refuse its save.
    func calendarGateClosed() {
        if CalendarGate.leavesPage(available: calendarPageAvailable, active: activeView, calendar: .calendar) {
            setView(.sitrep)
        }
        dropCalendarState()
        // The reminders switch in Settings belongs to the calendar too: off
        // with it, rather than a switch that writes to a route now 404ing.
        calendarReminderPref = .notOffered
    }

    // MARK: - warm before the page opens

    /// The first time this session for the live account (launch, a switch
    /// to another account): paint the feed saved on disk at once, and ask the
    /// daemon for a fresh one in the background, without kicking a Google
    /// sync (the page's own open does that). So the page has events on its
    /// first frame, and fresh ones by the time anyone opens it. Only on stats
    /// that are the live account's (`statsEpoch`): stats are not wiped on a
    /// switch. Never while the page is not offered: there is no page to warm,
    /// and the daemon 404s the feed.
    func warmCalendarIfDue() {
        guard calendarPageAvailable, statsEpoch == epoch,
              let id = AccountManager.shared.activeId, calendarWarmedFor != id
        else { return }
        calendarWarmedFor = id
        let e = epoch
        Task {
            if calendarFeed == nil, !FixtureMode.isEnabled,
               let saved = await CalendarDisk.shared.feed(for: id),
               isCurrent(e), AccountManager.shared.activeId == id, calendarFeed == nil {
                calendarFeed = saved
            }
            await refreshCalendar()
        }
    }
    /// Everything the calendar page holds for the ACTIVE mailbox, gone: the
    /// feed, the open item, the connect card and its wait, the event pane and
    /// its draft (a parked one too), the delete question, and any write still
    /// out (its ticket is from the session this ends, so its answer is
    /// dropped whole: `CalendarWriteTicket`). The view mode and the month are
    /// the user's and stay. Shared by an account switch and the gate closing.
    func dropCalendarState() {
        calendarFeed = nil
        calendarLoaded = false
        calendarWarmedFor = nil
        calendarMonthItem = nil
        calendarConnectAsk = nil
        calendarConnectWait = nil
        calendarConnectStarting = false
        calendarEditor = nil
        calendarDraft = nil
        calendarSweepCancel = nil
        calendarDeleteAsk = nil
        calendarWriting = false
        calendarEditorParked = false
        calendarPaneProposed = false
        calendarPaneEngaged = false
        calendarPaneState = .editing
        calendarPaneSent = nil
        calendarLoadGeneration &+= 1
        calendarSession &+= 1
    }

    /// What a write takes with it when it is sent, and must still be when
    /// its answer lands.
    var calendarTicket: CalendarWriteTicket { CalendarWriteTicket(epoch: epoch, session: calendarSession) }

    /// The answer to a write sent under `ticket` may be applied.
    func calendarLands(_ ticket: CalendarWriteTicket) -> Bool { ticket.lands(live: calendarTicket) }

    /// The calendar grant as the ACTIVE account's own stats report it; nil
    /// until they have arrived. Everything below reads this, never
    /// `stats?.calendar` directly.
    var calendarAccess: CalendarAccess? {
        CalendarGate.access(stats?.calendar, statsEpoch: statsEpoch, epoch: epoch)
    }

    /// How this mailbox could connect a calendar; nil when there is nothing
    /// to offer (granted, an older daemon, nothing usable sent, or no stats
    /// for this account yet).
    var calendarConnectRoute: CalendarGate.ConnectRoute? {
        CalendarGate.connectRoute(calendarAccess)
    }

    /// The same door, for renewing a granted calendar whose sync says `auth`.
    var calendarRenewRoute: CalendarGate.ConnectRoute? {
        CalendarGate.renewRoute(calendarAccess)
    }

    var calendarGranted: Bool { calendarAccess?.granted == true }

    /// What the page offers about connecting, given what this account has
    /// already been shown.
    var calendarOffer: CalendarGate.Offer {
        CalendarGate.offer(
            route: calendarConnectRoute, cardSeen: calendarCardSeen,
            fixtures: FixtureMode.isEnabled)
    }

    /// The connect card is up, for the account on screen.
    var calendarCardUp: Bool { calendarConnectAsk?.epoch == epoch }

    /// The wait, if it is this account's.
    var calendarLiveWait: CalendarConnectWait? {
        guard let wait = calendarConnectWait, wait.epoch == epoch else { return nil }
        return wait
    }

    /// True between Connect and the grant arriving, for as long as Google's
    /// consent page can still be finished.
    func calendarWaiting(now: Date = Date()) -> Bool {
        CalendarGate.isWaiting(since: calendarLiveWait?.since, now: now)
    }

    // MARK: - per-account memory

    /// The scope "seen the card" is remembered under: the account's id, as
    /// the calendar zone keys its own first-viewing stamps. A second mailbox
    /// gets its own first visit, because it is its own calendar to connect.
    private var calendarScope: String {
        FixtureMode.isEnabled
            ? CalendarVisibility.fixtureAccount
            : AccountManager.shared.activeId?.uuidString ?? "disconnected"
    }

    private var calendarCardKey: String { "passband.calendar.connectCardSeen.\(calendarScope)" }

    /// Read on each render of the page; the page also reads
    /// `calendarConnectAsk`, whose change is what re-renders it after this
    /// flips, so the plain UserDefaults read never goes stale on screen.
    var calendarCardSeen: Bool { UserDefaults.standard.bool(forKey: calendarCardKey) }

    /// Only for a card raised under the live epoch: the key is the ACTIVE
    /// account's, and a card left over from another must not mark it.
    private func markCalendarCardSeen(_ ask: CalendarConnectAsk) {
        guard ask.epoch == epoch else { return }
        UserDefaults.standard.set(true, forKey: calendarCardKey)
    }

    // MARK: - the timeline

    /// Load the agenda. Rows already on screen stay until a newer answer
    /// replaces them, and a failure says nothing: the page keeps what it has
    /// and the next poll tick tries again. `kick` first asks the daemon to
    /// sync the calendar now (on the page's appearance); a daemon without the
    /// sync, or inside its throttle, answers something this ignores.
    ///
    /// NEWEST REQUEST WINS. A poll tick's GET can be sent before a mark's
    /// PATCH and land after the mark's own reload; applied, it would put a
    /// dismissed row back on screen until the next tick. So each request
    /// takes a generation, local writes take one too, and an answer is
    /// applied only while its generation is still the latest.
    func refreshCalendar(kick: Bool = false) async {
        let e = epoch
        if kick { try? await APIClient.shared.refreshTimeline() }
        guard isCurrent(e) else { return }
        calendarLoadGeneration &+= 1
        let generation = calendarLoadGeneration
        // Local midnight YESTERDAY: last night's still-running party has to
        // come back, and the daemon judges `expired` in this offset. With a
        // month on screen, that month too (`MonthGrid.requestRange`, which
        // keeps the agenda's window inside it whenever it can).
        let cal = Calendar.current
        let now = Date()
        var from = cal.date(byAdding: .day, value: -1, to: cal.startOfDay(for: now)) ?? now
        var to: Date?
        if let month = calendarRequestMonth(now: now) {
            (from, to) = MonthGrid.requestRange(month: month, now: now, calendar: cal)
        }
        do {
            let feed = try await APIClient.shared.getTimeline(from: from, to: to)
            guard isCurrent(e), generation == calendarLoadGeneration else { return }
            // Assignment is invalidation: an unchanged answer is not written.
            if feed != calendarFeed {
                calendarFeed = feed
                // What the page paints first next launch or switch back.
                // Never a fixture feed: it would land under the real account.
                // Nor anything after the credentials were re-validated, until
                // the next account change (SECURITY.md §6, as the board).
                if !FixtureMode.isEnabled, boardWritesRefusedEpoch != epoch,
                   let id = AccountManager.shared.activeId {
                    Task.detached(priority: .utility) { await CalendarDisk.shared.save(feed, for: id) }
                }
            }
            if let wait = calendarLiveWait, wait.renewing, CalendarGate.renewalLanded(feed.calendar) {
                calendarRenewed()
            }
        } catch {
            guard isCurrent(e), generation == calendarLoadGeneration else { return }
        }
        if !calendarLoaded { calendarLoaded = true }
    }

    // MARK: - the month

    /// Where the month is: the remembered place, or today's.
    func calendarMonthPlace(now: Date, calendar: Calendar) -> MonthPlace {
        calendarPlace ?? MonthGrid.place(today: now, calendar: calendar)
    }

    /// The month the timeline request has to cover, while one is on screen.
    func calendarRequestMonth(now: Date) -> Date? {
        // The week on screen is one of its month's grid rows (the place's
        // day is always a cell of the place's month), so the month's range
        // covers it, lookback for trips included.
        guard calendarMode.isGrid else { return nil }
        return calendarMonthPlace(now: now, calendar: .current).month
    }

    /// `m` / `w` / `a` and the header's switch. The levels the month had open
    /// are closed, so coming back to it starts from the grid. The place is
    /// kept: the week opens on the month's selected day, and back.
    func setCalendarMode(_ mode: CalendarViewMode) {
        guard mode != calendarMode else { return }
        calendarMode = mode
        calendarDayOpen = false
        calendarMonthItem = nil
    }

    /// An arrow, a month step, or today. The day moves, so whatever item was
    /// open was the old day's and closes; an opened day stays open, so on a
    /// narrow window the arrows page through the columns.
    func moveCalendarMonth(_ move: MonthMove, now: Date = Date()) {
        let cal = Calendar.current
        calendarPlace = MonthGrid.navigate(
            calendarMonthPlace(now: now, calendar: cal), move, today: now, calendar: cal)
        calendarMonthItem = nil
    }

    /// A cell clicked: that day selected and, where the grid and the column
    /// cannot both show, opened. With the event pane open the click is the
    /// draft's: its date becomes that day, and nothing opens (the column is
    /// away while the pane is up).
    func selectCalendarDay(_ day: Date, open: Bool, now: Date = Date()) {
        let cal = Calendar.current
        calendarPlace = MonthGrid.select(day, in: calendarMonthPlace(now: now, calendar: cal), calendar: cal)
        if eventPaneOpen {
            // A frozen pane's draft is what was sent: a click moves nothing.
            if calendarPaneState.fieldsEditable, let draft = calendarDraft {
                calendarDraft = EventForm.moved(draft, to: day, today: now, calendar: cal)
            }
            return
        }
        calendarMonthItem = nil
        if open { calendarDayOpen = true }
    }

    /// A row in a cell clicked: its day selected and its detail open, in the
    /// pane beside the grid or (on a narrow window) in place of it.
    func selectCalendarItem(_ id: String, on day: Date, now: Date = Date()) {
        let cal = Calendar.current
        calendarPlace = MonthGrid.select(day, in: calendarMonthPlace(now: now, calendar: cal), calendar: cal)
        calendarMonthItem = id
    }

    /// A search hit opened (the search panel's Events block): the Calendar
    /// page, on a grid, turned to the row's day with its detail open. Moving
    /// the place is what makes the page fetch that month, so an event a year
    /// back is in the feed by the time the detail looks for it.
    func openCalendarSearchHit(_ item: AgendaItem, now: Date = Date()) {
        if activeView != .calendar { setView(.calendar) }
        guard activeView == .calendar else { return }
        if !calendarMode.isGrid { setCalendarMode(.month) }
        selectCalendarItem(item.id, on: CalendarSearch.day(item, calendar: .current), now: now)
        #if os(iOS)
            calendarSearchOpen = item.id
        #endif
    }

    /// Escape on the month or the week: closes the item's detail, then (the
    /// month's) opened day.
    /// False when there is nothing to close, so Escape goes on to its usual
    /// "back to the sitrep".
    func calendarMonthBack() -> Bool {
        guard activeView == .calendar, calendarMode.isGrid else { return false }
        // A sweep under way on the day column is the first thing Escape ends.
        if let cancel = calendarSweepCancel {
            calendarSweepCancel = nil
            cancel()
            return true
        }
        if calendarMonthItem != nil {
            calendarMonthItem = nil
            return true
        }
        if calendarDayOpen {
            calendarDayOpen = false
            return true
        }
        return false
    }

    /// The user's word on a ghost: going, dismissed, or (nil) neither. Drawn
    /// at once, sent, and put back with a toast if the daemon refuses. A
    /// dismissal takes the row off the page, so it leaves an undo behind.
    func markGhost(_ ghost: TimelineGhost, _ state: GhostState?, viaKey: Bool) async {
        let ticket = calendarTicket
        let before = ghost.state
        // What the user's mark was, so an undo of a dismissal restores it
        // rather than merely clearing it.
        let priorMark: GhostState? = before == .going ? .going : (before == .dismissed ? .dismissed : nil)
        let shown: GhostState = state ?? (ghost.mail_state == .cancelled ? .cancelled : .ghost)
        setGhostLocally(ghost.id, shown)
        do {
            _ = try await APIClient.shared.setGhostState(ghost.id, state)
            guard calendarLands(ticket) else { return }
            // Spelled out rather than `rawValue`: the vocabulary is closed, and
            // only these three words are in it.
            let word = state == .going ? "going" : (state == .dismissed ? "dismissed" : "cleared")
            Analytics.capture(
                "calendar_ghost_marked", ["state": word, "via": viaKey ? "key" : "click"])
            if state == .dismissed {
                let id = ghost.id
                pushUndo(kind: .ghostDismiss, messageId: id, label: "dismissed \(ghost.title)") {
                    [weak self] in
                    // A write: whatever was asked for before it is stale.
                    await self?.retireCalendarLoads()
                    _ = try await APIClient.shared.setGhostState(id, priorMark)
                    await self?.refreshCalendar()
                }
            }
            await refreshCalendar()
        } catch {
            guard calendarLands(ticket) else { return }
            setGhostLocally(ghost.id, before)
            pushToast(errText(error, "Could not update that plan"), .error)
        }
    }

    /// The user's answer to a change another sender's mail offers on their
    /// card (`LinkedChange`): accept takes it, false keeps the card as it
    /// is. Nothing is drawn ahead of the daemon (an accepted move changes
    /// times the daemon resolves), so the page waits for the refresh; a
    /// refusal, or a link already gone, is a toast.
    func answerLinkedChange(_ change: LinkedChange, accept: Bool, viaKey: Bool) async {
        // A write: whatever was asked for before it is stale.
        retireCalendarLoads()
        let ticket = calendarTicket
        do {
            _ = try await APIClient.shared.answerLinkedChange(
                change.ghost_id, from: change.from, accept: accept)
            guard calendarLands(ticket) else { return }
            // Closed vocabulary: these four words and nothing from the mail.
            Analytics.capture(
                "calendar_linked_change",
                [
                    "answer": accept ? "accepted" : "kept",
                    "kind": change.kind == .cancelled ? "cancelled" : "moved",
                    "via": viaKey ? "key" : "click",
                ])
            await refreshCalendar()
        } catch {
            guard calendarLands(ticket) else { return }
            pushToast(errText(error, "Could not update that plan"), .error)
            await refreshCalendar()
        }
    }

    /// A write is newer than every answer already on its way: none of them
    /// may be applied over it (see `refreshCalendar`).
    func retireCalendarLoads() {
        calendarLoadGeneration &+= 1
    }

    private func setGhostLocally(_ id: Int, _ state: GhostState) {
        retireCalendarLoads()
        guard var feed = calendarFeed, let i = feed.ghosts.firstIndex(where: { $0.id == id })
        else { return }
        feed.ghosts[i].state = state
        calendarFeed = feed
    }

    // MARK: - writing events

    /// The calendars the editor offers (writable, primary first). Empty on a
    /// daemon too old to list them.
    var calendarWritable: [TimelineCalendar] { CalendarWrite.writable(calendarFeed) }

    /// Whether "New event" and `n` exist at all. The fixture inbox.s daemon is a
    /// fixture, so it never offers one.
    var calendarCanCreate: Bool {
        !FixtureMode.isEnabled && CalendarWrite.canCreate(calendarFeed)
    }

    /// The event pane is open, for the account on screen, and not parked
    /// on another page. What the shell lays out beside, as `composeOpen` is
    /// for the composer: it changes on open, close, park and return only,
    /// never while a field is typed in.
    var eventPaneOpen: Bool { calendarEditor?.epoch == epoch && !calendarEditorParked }

    /// The pane's fields differ from what it opened with.
    var eventDraftTouched: Bool {
        guard let request = calendarEditor, let draft = calendarDraft else { return false }
        return draft != request.initial
    }

    /// `n` and "New event": the pane, on `day` (the month's selected day;
    /// today from the agenda).
    func openEventEditor(day: Date?, now: Date = Date()) {
        guard calendarCanCreate, calendarEditor == nil, calendarDeleteAsk == nil else { return }
        let cal = Calendar.current
        let draft = EventForm.newDraft(
            day: day ?? now, now: now, calendarId: calendarWritable.first?.id, calendar: cal)
        openEventPane(CalendarEditorRequest(mode: .create, initial: draft, epoch: epoch))
    }

    /// A span swept on the day column (drag to create): the pane, on that
    /// day, with those times. Minutes after midnight.
    func openEventEditor(day: Date, startMinutes: Int, endMinutes: Int, now: Date = Date()) {
        guard calendarCanCreate, calendarEditor == nil, calendarDeleteAsk == nil else { return }
        let draft = EventForm.newDraft(
            day: day, startMinutes: startMinutes, endMinutes: endMinutes, now: now,
            calendarId: calendarWritable.first?.id, calendar: .current)
        openEventPane(CalendarEditorRequest(mode: .create, initial: draft, epoch: epoch))
    }

    /// `e` and the detail's Edit: the same pane, filled in. Only for an event
    /// the daemon called editable.
    func openEventEditor(editing event: TimelineEvent, now: Date = Date()) {
        guard !FixtureMode.isEnabled, CalendarWrite.canEdit(event), calendarEditor == nil,
            calendarDeleteAsk == nil
        else { return }
        let draft = EventForm.draft(editing: event, today: now, calendar: .current)
        openEventPane(CalendarEditorRequest(mode: .edit(event), initial: draft, epoch: epoch))
    }

    /// THE ASSISTANT'S PROPOSAL (wave 6, Lib/EventProposal.swift): the pane
    /// opened on the Calendar page with `draft` filled in, and NOTHING
    /// written. The user adds it, or not, with the pane's own control.
    /// Returns why not, in words for the assistant, or nil when the pane is
    /// open.
    ///
    /// The pane opens TOUCHED: what it opened with is a blank new event, and
    /// the proposal is what is in it. So it parks rather than closes when
    /// the user goes to another page, a composer does not replace it, and a
    /// second proposal is refused until this one is answered, exactly as if
    /// the user had typed it.
    ///
    /// IT NEVER TAKES THE SCREEN FROM SOMEBODY (`EventProposals.landing`,
    /// verify7 D2). Refused unless the assistant's bar is open at this
    /// moment, and while mail is being written. An email being read is not
    /// closed: the pane is then PARKED, whole, for the Calendar page to show
    /// when the user goes there. And the pane is marked proposed, so it
    /// takes no keyboard and no ↵ or ⌘↵ until the user clicks or types in it.
    func proposeEvent(_ draft: EventDraft, now: Date = Date()) -> Result<EventProposals.Landing, ProposalRefusal> {
        func refused(_ text: String) -> Result<EventProposals.Landing, ProposalRefusal> {
            .failure(ProposalRefusal(text: text))
        }
        guard calendarPageAvailable, calendarCanCreate else { return refused(EventProposals.noCalendarText) }
        let landing: EventProposals.Landing
        switch EventProposals.landing(
            askBarOpen: askBarOpen, writingMail: mailBeingWritten(), threadOpen: threadId != nil)
        {
        case .failure(let refusal): return .failure(refusal)
        case .success(let where_): landing = where_
        }
        if let refusal = EventProposals.paneRefusal(
            paneExists: calendarEditor != nil, touched: eventDraftTouched,
            editable: calendarPaneState.fieldsEditable && !calendarWriting, deleteAsked: calendarDeleteAsk != nil)
        {
            return refused(refusal)
        }
        // An untouched pane (opened and left empty) gives way.
        if calendarEditor != nil { closeEventEditor() }
        let cal = Calendar.current
        if landing == .calendarPage {
            if activeView != .calendar { setView(.calendar) }
            guard activeView == .calendar else { return refused(EventProposals.noCalendarText) }
        }
        // A grid, turned to the event's day, so its dashed chip is in view.
        if !calendarMode.isGrid { setCalendarMode(.month) }
        if case .success(let when) = EventForm.when(draft, today: now, calendar: cal) {
            calendarPlace = MonthGrid.select(
                when.start, in: calendarMonthPlace(now: now, calendar: cal), calendar: cal)
            calendarMonthItem = nil
        }
        var blank = EventForm.newDraft(day: now, now: now, calendarId: draft.calendarId, calendar: cal)
        // The proposal's title is not read for a day or a time (it already
        // says when), and neither is anything typed over it.
        blank.titleRead = draft.titleRead
        openEventPane(
            CalendarEditorRequest(mode: .create, initial: blank, epoch: epoch), draft: draft, proposed: true,
            parked: landing == .parked)
        return .success(landing)
    }

    /// The user clicked or typed inside a pane the assistant filled in: its
    /// keys are theirs from here.
    func engageProposedPane() {
        if calendarPaneProposed, !calendarPaneEngaged { calendarPaneEngaged = true }
    }

    /// ↵ or ⌘↵ on a proposed pane nobody has clicked or typed in.
    func noteProposedPaneLocked() {
        pushToast(EventProposals.keysLockedText)
    }

    /// ⌘↵ on a proposed pane with guests: the button is the only way.
    func noteProposedGuestsNeedAClick() {
        pushToast(EventProposals.guestsNeedAClickText)
    }

    /// An event pane the user is writing in, as an agent's draft asking for
    /// the screen sees it (`EventPaneTravel.heldByUser`).
    var eventPaneHeldByUser: Bool {
        EventPaneTravel.heldByUser(
            exists: calendarEditor != nil, touched: eventDraftTouched, parked: calendarEditorParked,
            editable: calendarPaneState.fieldsEditable && !calendarWriting)
    }

    /// ONE PANE AT A TIME. The event pane takes the half of the window the
    /// composer and the side panel use, so both step aside for it. A composer
    /// is closed the way Escape closes it: its mail is kept in drafts and the
    /// usual toast says so, so nothing is lost by the swap.
    ///
    /// `draft`: what the pane holds when it opens, when that is not what it
    /// opened WITH (a proposal). `proposed`: the assistant filled it in.
    /// `parked`: it is kept and not drawn (a proposal made while an email is
    /// open), so nothing steps aside for it until the user comes to the page
    /// (`returnParkedEventEditor`).
    private func openEventPane(
        _ request: CalendarEditorRequest, draft: EventDraft? = nil, proposed: Bool = false, parked: Bool = false
    ) {
        if !parked {
            // A composer still here holds nothing typed when this is a
            // proposal: `proposeEvent` refused otherwise.
            if compose != nil { closeCompose() }
            if sideView.isOpen { closeSide() }
        }
        calendarDraft = draft ?? request.initial
        calendarEditor = request
        calendarEditorParked = parked
        calendarPaneProposed = proposed
        calendarPaneEngaged = false
        calendarPaneState = .editing
        calendarPaneSent = nil
    }

    /// THE OTHER HALF OF THE RULE: a composer asked for while the event pane
    /// is open. An untouched pane simply gives way. One with something typed
    /// in it does NOT: an event has no drafts folder to be kept in, so the
    /// composer is refused with one quiet line rather than the event thrown
    /// away. True when the composer may open.
    ///
    /// A PARKED pane (left with something typed in it, see
    /// `parkEventEditorForNavigation`) is not in the way: the composer opens
    /// and the pane stays parked for the way back.
    func yieldEventPaneToCompose() -> Bool {
        guard calendarEditor != nil else { return true }
        let rule = EventPaneTravel.composer(
            open: eventPaneOpen, parked: calendarEditorParked, touched: eventDraftTouched)
        guard rule.allow else {
            pushToast(EventPaneRules.composerRefusedText(calendarPaneState))
            return false
        }
        if rule.closePane { closeEventEditor() }
        return true
    }

    /// Closes the pane, a frozen one too: the event, if Google made it,
    /// arrives with the next sync. Nothing of the draft is kept.
    func closeEventEditor() {
        calendarEditor = nil
        calendarDraft = nil
        calendarEditorParked = false
        calendarPaneProposed = false
        calendarPaneEngaged = false
        calendarPaneState = .editing
        calendarPaneSent = nil
    }

    /// Leaving the Calendar page (a digit, the rail, ⌘[, a search): an
    /// untouched pane closes as it always did; one with something typed in
    /// it is PARKED, kept whole and not drawn, because an event has no drafts
    /// folder to be kept in (`EventPaneTravel`).
    func parkEventEditorForNavigation() {
        switch EventPaneTravel.leaving(open: eventPaneOpen, touched: eventDraftTouched) {
        case .park: calendarEditorParked = true
        case .close: closeEventEditor()
        case nil:
            // Not on screen: parked already, or another account's.
            if calendarEditor != nil, calendarEditor?.epoch != epoch { closeEventEditor() }
        }
    }

    /// Back on the Calendar page: a parked pane comes back as it was left.
    /// It takes its half of the window the way it did when it opened
    /// (`openEventPane`): a composer opened meanwhile is closed into drafts
    /// with the usual toast, and a side panel steps aside.
    ///
    /// EXCEPT A PROPOSAL OVER MAIL BEING WRITTEN (verify8 V8-2,
    /// `EventPaneTravel.parkedMayReturn`): a pane the assistant filled in
    /// stays parked while a composer or an inline reply holds anything
    /// typed, whichever door brought the user here (the page reached, the
    /// email over it closed). It is asked again when that mail is sent or
    /// closed (RootView's hook on the composers).
    func returnParkedEventEditor() {
        guard calendarEditorParked else { return }
        guard calendarEditor?.epoch == epoch else {
            closeEventEditor()
            return
        }
        guard EventPaneTravel.parkedMayReturn(proposed: calendarPaneProposed, writingMail: mailBeingWritten())
        else { return }
        if compose != nil { closeCompose() }
        if sideView.isOpen { closeSide() }
        calendarEditorParked = false
    }

    /// The draft's chip on the month: where the pane's fields would put the
    /// event, in the colour of the calendar picked (an edit keeps its
    /// event's own). Nil with no pane, and while the times do not read.
    func calendarProvisional(now: Date, calendar: Calendar) -> AgendaItem? {
        guard eventPaneOpen, let request = calendarEditor, let draft = calendarDraft else { return nil }
        let color = request.event?.color
            ?? calendarFeed?.calendars?.first { $0.id == draft.calendarId }?.color
        return EventForm.provisional(draft, color: color, today: now, calendar: calendar)
    }

    /// The account's own address, for this account's stats only: who Google
    /// sends an invitation from, and the one address that is never a guest.
    var calendarAccountEmail: String? {
        let email = (statsEpoch == epoch ? stats?.account_email : nil)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return (email?.isEmpty ?? true) ? nil : email
    }

    /// What the pane's draft may do about guests, and which controls save it
    /// (`EventGuests.plan`). The pane draws from this and `saveEvent` checks
    /// against it, so the control pressed is judged by the draft as it
    /// stands when the save runs.
    func guestPlan(_ draft: EventDraft, _ request: CalendarEditorRequest) -> GuestPlan {
        EventGuests.plan(draft, editing: request.event, feed: calendarFeed)
    }

    /// Save the pane. True when the daemon took it: the answer is laid into
    /// the page at once and the pane closes. A refusal keeps the pane open
    /// and editable with everything typed in it, after one quiet toast saying
    /// why; a draft that cannot be saved yet is said on the pane itself and
    /// sends nothing.
    ///
    /// `control` is the footer control that asked (a click on it, or its
    /// ⌘↵). It is the ONLY thing that can make Google email the guests:
    /// `EventGuests.notify` answers true for a control that says so and is
    /// on offer for this draft, and a control that is not on offer saves
    /// nothing at all.
    ///
    /// AN ANSWER THAT MAY HAVE HAPPENED (`CalendarWrite.verdict`: anything
    /// but a definite "nothing happened"): Google may have made the change.
    /// The page reloads to find out (`settleUncertain`); seen, the pane
    /// closes as a success. Not seen, the pane FREEZES
    /// (`EventPaneState.uncertain`): read-only, the draft showing, and only
    /// "check again", "send again" (the same body) and "close" on offer. An
    /// edited draft is never sent under the same id. Only a definite
    /// refusal gives the fields back.
    ///
    /// AN ANSWER FOR NOBODY: one landing after the gate closed or the account
    /// changed (`CalendarWriteTicket`) is dropped whole, analytics included.
    @discardableResult
    func saveEvent(_ draft: EventDraft, _ request: CalendarEditorRequest, via control: SaveControl) async -> Bool {
        guard EventPaneRules.maySave(paneFacts(request)) else { return false }
        let now = Date()
        let cal = Calendar.current
        let plan = guestPlan(draft, request)
        guard let notify = EventGuests.notify(control, offered: plan.controls) else { return false }
        let terms = plan.terms(owner: calendarAccountEmail)
        let sent: SentWrite
        switch request.mode {
        case .create:
            guard case .success(let made) = EventForm.wire(draft, today: now, calendar: cal, guests: terms)
            else { return false }
            sent = CalendarWrite.createWrite(
                made, notify: notify, createId: request.createId, before: CalendarWrite.keys(calendarFeed))
        case .edit(let original):
            guard
                case .success(let patch) = EventForm.patch(
                    draft, from: request.initial, today: now, calendar: cal, guests: terms)
            else { return false }
            // Nothing moved: nothing to send, nothing to say, and nobody
            // to tell, whichever control was pressed.
            guard let edit = CalendarWrite.editWrite(original, patch, notify: notify) else {
                closeEventEditor()
                return true
            }
            sent = edit
        }
        calendarPaneSent = sent
        return await sendPaneWrite(sent, request, plan: plan, event: .send)
    }

    /// The frozen pane's "send again": EXACTLY the body already sent, the
    /// same create id with it. Never the draft as it now stands.
    @discardableResult
    func sendEventAgain(_ request: CalendarEditorRequest) async -> Bool {
        guard let sent = EventPaneRules.again(paneFacts(request), sent: calendarPaneSent) else { return false }
        return await sendPaneWrite(sent, request, plan: nil, event: .sendAgain)
    }

    /// The frozen pane's "check again" (and its ⌘↵): the daemon is asked to
    /// sync, the page reloaded, and the write looked for again. Seen, the
    /// pane closes as a success; not seen, it stays frozen.
    @discardableResult
    func checkEventAgain(_ request: CalendarEditorRequest) async -> Bool {
        guard let sent = EventPaneRules.again(paneFacts(request), sent: calendarPaneSent) else { return false }
        let ticket = calendarTicket
        calendarPaneState = calendarPaneState.next(.checkAgain)
        calendarWriting = true
        try? await APIClient.shared.refreshTimeline()
        guard calendarLands(ticket) else { return false }
        let seen = await settleUncertain(ticket, sayUnseen: false) { sent.found(in: $0) != nil }
        return paneSettled(seen, sent, request)
    }

    /// The pane as `EventPaneRules` judges it.
    private func paneFacts(_ request: CalendarEditorRequest) -> EventPaneRules.Pane {
        EventPaneRules.Pane(
            isOpen: EventPaneRules.applies(open: calendarEditor?.id, sentFrom: request.id),
            sameAccount: request.epoch == epoch, writing: calendarWriting, state: calendarPaneState)
    }

    /// One send from the pane, first or again, and where it leaves the pane.
    private func sendPaneWrite(
        _ sent: SentWrite, _ request: CalendarEditorRequest, plan: GuestPlan?, event: EventPaneState.Event
    ) async -> Bool {
        let ticket = calendarTicket
        let again = event == .sendAgain
        calendarPaneState = calendarPaneState.next(event)
        calendarWriting = true
        do {
            let answer: TimelineEvent
            switch sent {
            case .create(let body, _):
                answer = try await APIClient.shared.createCalendarEvent(body)
            case .edit(let original, let patch, let notify):
                answer = try await APIClient.shared.patchCalendarEvent(
                    calendarId: original.calendar_id, id: original.id, patch, notify: notify)
            }
            guard calendarLands(ticket) else { return false }
            calendarWriting = false
            paneMoved(.answered, request)
            landEvent(answer, closing: request)
            switch sent {
            case .create(let body, _):
                let invited = body.guests?.count ?? 0
                Analytics.capture(
                    "calendar_event_created",
                    ["all_day": body.all_day, "guests": invited > 0, "notified": body.notify == true])
                // A daemon that took the event and dropped the guests must
                // not pass for an invitation sent.
                if invited > 0, EventGuests.others(answer).count < invited {
                    pushToast("The event was added, but this Passband server did not add its guests.", .error)
                }
            case .edit(_, let patch, let notify):
                let guests = plan.map { $0.before > 0 || $0.after > 0 } ?? (patch.guests != nil)
                Analytics.capture(
                    "calendar_event_edited", ["all_day": answer.all_day, "guests": guests, "notified": notify])
            }
            return true
        } catch {
            guard calendarLands(ticket) else { return false }
            let verdict = CalendarWrite.verdict(of: error)
            guard let refused = EventPaneRules.failureEvent(verdict) else {
                let seen = await settleUncertain(ticket, sayUnseen: false, saying: CalendarWrite.settlingText(verdict)) {
                    sent.found(in: $0) != nil
                }
                return paneSettled(seen, sent, request)
            }
            calendarWriting = false
            paneMoved(refused, request)
            if let text = EventPaneRules.refusalText(verdict, sent, again: again) { pushToast(text, .error) }
            return false
        }
    }

    /// After a settle: the write on the page closes the pane as a success;
    /// not on it, the pane is frozen (or stays so).
    private func paneSettled(_ seen: Bool, _ sent: SentWrite, _ request: CalendarEditorRequest) -> Bool {
        guard seen, let event = sent.found(in: calendarFeed) else {
            paneMoved(.unsure, request)
            return false
        }
        paneMoved(.answered, request)
        landEvent(event, closing: request)
        return true
    }

    /// The pane's state moved by `event`, only while that pane is still the
    /// open one: a pane closed while its write was out keeps no state.
    private func paneMoved(_ event: EventPaneState.Event, _ request: CalendarEditorRequest) {
        guard EventPaneRules.applies(open: calendarEditor?.id, sentFrom: request.id) else { return }
        calendarPaneState = calendarPaneState.next(event)
    }

    /// After a 504: say so, and reload the page a few times (the daemon
    /// kicked a sync of its own when it answered) until `seen` finds the
    /// change in it. The write stays locked (`calendarWriting`) throughout,
    /// so nothing can be sent twice while it is unknown. True when the
    /// change showed up; false, for a delete (`sayUnseen`), says so in one
    /// more toast (a pane says it on itself, frozen). Either way the lock is
    /// released, unless the answer has stopped being anybody's.
    private func settleUncertain(
        _ ticket: CalendarWriteTicket, sayUnseen: Bool = true, saying: String = CalendarWrite.uncertainText,
        seen: (TimelineFeed?) -> Bool
    ) async -> Bool {
        pushToast(saying)
        for _ in 0..<3 {
            try? await Task.sleep(for: .seconds(2))
            guard calendarLands(ticket) else { return false }
            await refreshCalendar()
            guard calendarLands(ticket) else { return false }
            if seen(calendarFeed) {
                calendarWriting = false
                return true
            }
        }
        calendarWriting = false
        if sayUnseen { pushToast(CalendarWrite.uncertainUnseenText) }
        return false
    }

    /// The daemon's answer, on screen now: laid into the feed, every load
    /// already out retired (an answer asked for before this write would put
    /// the old event back), the card closed, and the event selected where it
    /// now is, so the month turns to it and its detail opens.
    ///
    /// `closing`: the pane that asked, when it may have been closed (or
    /// another opened) while the answer was in doubt; only that one closes.
    private func landEvent(_ event: TimelineEvent, closing request: CalendarEditorRequest? = nil) {
        retireCalendarLoads()
        calendarFeed = CalendarWrite.applying(event, to: calendarFeed)
        if request == nil || calendarEditor?.id == request?.id { closeEventEditor() }
        let cal = Calendar.current
        if let item = Agenda.item(event, calendar: cal) {
            calendarPlace = MonthGrid.select(
                item.start, in: calendarMonthPlace(now: Date(), calendar: cal), calendar: cal)
            calendarMonthItem = item.id
        }
    }

    /// The detail's Delete and ⌫: ask once, on the app's own card.
    func askDeleteEvent(_ event: TimelineEvent) {
        guard !FixtureMode.isEnabled, CalendarWrite.canEdit(event), calendarEditor == nil else { return }
        calendarDeleteAsk = CalendarDeleteAsk(event: event, epoch: epoch)
    }

    func cancelDeleteEvent() {
        calendarDeleteAsk = nil
    }

    /// Delete at Google, then off the page. There is no undo (a recreated
    /// event would be a different event, its invitations and id gone), which
    /// is why this is only ever reached through the question.
    ///
    /// `tellGuests` is true only from the question's "delete and tell
    /// guests" control (or its ⌘↵), and counts only for an event that has
    /// guests to tell on a calendar that may tell them
    /// (`EventGuests.mayTell`): then Google emails them the cancellation.
    /// A 504 is settled as a save's is: gone from the reloaded page is
    /// deleted, still there keeps the question up.
    ///
    /// "Delete and tell guests" on a calendar that may no longer tell them
    /// (the page reloaded under the question) deletes NOTHING and says so,
    /// keeping the question up (`CalendarWrite.deleteNotify`): a silent
    /// delete is not what the control said.
    @discardableResult
    func deleteEvent(_ event: TimelineEvent, tellGuests: Bool = false) async -> Bool {
        guard CalendarWrite.canEdit(event), !calendarWriting else { return false }
        let ticket = calendarTicket
        let hasGuests = EventGuests.count(event) > 0
        guard
            let sent = CalendarWrite.deleteWrite(
                event, tellGuests: tellGuests, hasGuests: hasGuests,
                mayTell: EventGuests.mayTell(event, feed: calendarFeed))
        else {
            pushToast(CalendarWrite.cannotTellText, .error)
            return false
        }
        let notify = sent.notify
        calendarWriting = true
        do {
            try await APIClient.shared.deleteCalendarEvent(
                calendarId: sent.calendarId, id: sent.id, notify: notify)
            guard calendarLands(ticket) else { return false }
            calendarWriting = false
            eventDeleted(event)
            Analytics.capture(
                "calendar_event_deleted", ["all_day": event.all_day, "guests": hasGuests, "notified": notify])
            return true
        } catch {
            guard calendarLands(ticket) else { return false }
            let verdict = CalendarWrite.verdict(of: error)
            switch verdict {
            case .mayHaveHappened:
                guard await settleUncertain(ticket, seen: { CalendarWrite.gone(event, from: $0) }) else {
                    return false
                }
                eventDeleted(event)
                return true
            case .notSent:
                calendarWriting = false
                pushToast(CalendarWrite.failureText(nil, .delete), .error)
            case .refused(let refusal):
                calendarWriting = false
                pushToast(CalendarWrite.failureText(refusal, .delete), .error)
            }
            return false
        }
    }

    /// The event is gone at Google: off the page, its detail, its question
    /// and its pane closed.
    private func eventDeleted(_ event: TimelineEvent) {
        retireCalendarLoads()
        calendarFeed = CalendarWrite.removing(calendarId: event.calendar_id, id: event.id, from: calendarFeed)
        if calendarMonthItem == Agenda.itemId(event) { calendarMonthItem = nil }
        calendarDeleteAsk = nil
        if calendarEditor?.event.map(Agenda.itemId) == Agenda.itemId(event) { closeEventEditor() }
        pushToast("Deleted from Google Calendar", .success)
    }

    // MARK: - the connect card

    /// The page's first visit for an account that has not connected: raise
    /// the card, once. Every later visit gets the banner instead. The offer
    /// is `.none` until this account's own stats have landed, so nothing is
    /// raised from the stats of the account before a switch.
    func raiseCalendarCardIfOwed() {
        guard calendarOffer == .card, !calendarCardUp else { return }
        openCalendarConnect()
    }

    /// Raise the card on purpose (the banner's Connect, or the sync line's
    /// when the connection needs renewing). Built from THIS account's stats
    /// only, and stamped with this epoch.
    func openCalendarConnect(renewing: Bool = false) {
        guard let route = renewing ? calendarRenewRoute : calendarConnectRoute else { return }
        let email = (statsEpoch == epoch ? stats?.account_email : nil)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        calendarConnectAsk = CalendarConnectAsk(
            route: route, email: (email?.isEmpty ?? true) ? nil : email, epoch: epoch,
            renewing: renewing)
    }

    /// Connect (the button or Enter): open Google's consent in the browser
    /// and wait for the grant to show up on the stats poll. The card stays
    /// up, saying so.
    func confirmCalendarConnect() {
        guard let ask = calendarConnectAsk, ask.epoch == epoch, !calendarConnectStarting
        else { return }
        Analytics.capture("calendar_connect_clicked", ["self_host": ask.route == .daemon])
        openCalendarConsent(ask)
    }

    /// "Open Google again" while waiting: the tab was closed or lost. On
    /// self-host this asks the daemon for a FRESH link, since the one it gave
    /// before may have expired. Not counted as another click.
    func reopenCalendarConsent() {
        guard let ask = calendarConnectAsk, ask.epoch == epoch else { return }
        openCalendarConsent(ask)
    }

    /// Both routes end here: a consent link in the browser and the wait
    /// (re)started, since a fresh link is a fresh consent session. The card
    /// counts as seen only once a link has actually opened.
    private func openCalendarConsent(_ ask: CalendarConnectAsk) {
        switch ask.route {
        case .link(let url):
            consentOpened(ask, url)
        case .daemon:
            // One request at a time: a second Enter while the first is out
            // would open two consent tabs.
            guard !calendarConnectStarting else { return }
            calendarConnectStarting = true
            Task { await self.startDaemonConsent(ask) }
        }
    }

    /// Self-host: the daemon starts the consent and hands back Google's
    /// link. A refusal (hosted answers 409), a network failure or an answer
    /// with no usable link is one quiet toast; the card stays as it was, so
    /// Connect can simply be pressed again. A toast and not an alert: an
    /// alert would block the key dispatcher.
    private func startDaemonConsent(_ ask: CalendarConnectAsk) async {
        let url = CalendarGate.consentURL(try? await APIClient.shared.startCalendarConnect().url)
        // The answer is for the card that asked. After a switch of account it
        // is nobody's (the switch already cleared the flag, and the new
        // account may have a request of its own out); with the card closed
        // while the request was out, nobody is waiting for a tab.
        guard isCurrent(ask.epoch) else { return }
        calendarConnectStarting = false
        guard calendarConnectAsk == ask else { return }
        guard let url else {
            pushToast("Could not start the calendar connection", .error)
            return
        }
        consentOpened(ask, url)
    }

    private func consentOpened(_ ask: CalendarConnectAsk, _ url: String) {
        markCalendarCardSeen(ask)
        calendarConnectWait = CalendarConnectWait(
            since: Date(), epoch: ask.epoch, renewing: ask.renewing)
        Opener.open(url)
    }

    /// Any way out of the card: Not now, Escape, the scrim, or Close while
    /// waiting. Never raised unprompted again for this account. Closing a
    /// card that is already waiting is not a refusal, and is not counted as
    /// one.
    func dismissCalendarConnect() {
        guard let ask = calendarConnectAsk else { return }
        let waiting = calendarWaiting()
        calendarConnectAsk = nil
        markCalendarCardSeen(ask)
        if !waiting { Analytics.capture("calendar_connect_dismissed") }
    }

    /// The user went to another page with the card up: the card is the
    /// calendar page's, so it goes. It has been seen, but leaving is not a
    /// refusal, so nothing is counted. A wait in progress stays, and the
    /// page's own waiting line says so on the way back.
    func closeCalendarConnectForNavigation() {
        guard let ask = calendarConnectAsk else { return }
        calendarConnectAsk = nil
        markCalendarCardSeen(ask)
    }

    /// The grant arrived (on the stats poll): put the card and the wait away
    /// and load the real events. Called by the shell's watcher, so it lands
    /// wherever the user is. Only THIS account's card or wait counts as
    /// having asked: `calendarGranted` reads this account's stats alone, and
    /// a card or wait from before a switch is not this account's.
    func calendarGrantArrived() {
        let wasAsking = calendarCardUp || calendarLiveWait != nil
        calendarConnectAsk = nil
        calendarConnectWait = nil
        guard wasAsking else { return }
        pushToast("Google Calendar connected", .success)
        Task { await refreshCalendar(kick: true) }
    }

    /// A renewal worked: the sync that was failing on the old token has come
    /// back clean. The grant flag never moved, so this is heard off the
    /// timeline rather than the stats.
    private func calendarRenewed() {
        if calendarConnectAsk?.renewing == true { calendarConnectAsk = nil }
        calendarConnectWait = nil
        pushToast("Google Calendar reconnected", .success)
    }
}
