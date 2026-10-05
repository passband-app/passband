// The calendar page's half of the store: the gate, the timeline load, the
// user's word on a ghost, and the connect card's lifecycle. The stored state
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

extension AppStore {
    // MARK: - the gate

    /// Whether the Calendar destination exists at all. Rail, Go menu, ⇧C and
    /// the route all read this one property; the rule itself, and why it
    /// includes developer mode, is `CalendarGate.pageAvailable`.
    ///
    /// Deliberately off the raw `stats`, not `calendarAccess`: this says only
    /// whether a page exists, never whose calendar is in it. Reading it
    /// through the account guard would hide the page for the second between
    /// an account switch and the first poll, and the shell watcher would
    /// throw the user off it. The page's data is fetched from the new daemon
    /// under the new epoch either way.
    var calendarPageAvailable: Bool {
        CalendarGate.pageAvailable(
            timeline: stats?.timeline, developerMode: Prefs.shared.developerMode)
    }

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
            practice: RehearsalMode.isEnabled)
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
        RehearsalMode.isEnabled
            ? CalendarVisibility.practiceAccount
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
            if feed != calendarFeed { calendarFeed = feed }
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
        guard calendarMode == .month else { return nil }
        return calendarMonthPlace(now: now, calendar: .current).month
    }

    /// `a` / `m` and the header's switch. The levels the month had open are
    /// closed, so coming back to it starts from the grid.
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
    /// cannot both show, opened.
    func selectCalendarDay(_ day: Date, open: Bool, now: Date = Date()) {
        let cal = Calendar.current
        calendarPlace = MonthGrid.select(day, in: calendarMonthPlace(now: now, calendar: cal), calendar: cal)
        calendarMonthItem = nil
        if open { calendarDayOpen = true }
    }

    /// Escape on the month: closes the item's detail, then the opened day.
    /// False when there is nothing to close, so Escape goes on to its usual
    /// "back to the sitrep".
    func calendarMonthBack() -> Bool {
        guard activeView == .calendar, calendarMode == .month else { return false }
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
        let e = epoch
        let before = ghost.state
        // What the user's mark was, so an undo of a dismissal restores it
        // rather than merely clearing it.
        let priorMark: GhostState? = before == .going ? .going : (before == .dismissed ? .dismissed : nil)
        let shown: GhostState = state ?? (ghost.mail_state == .cancelled ? .cancelled : .ghost)
        setGhostLocally(ghost.id, shown)
        do {
            _ = try await APIClient.shared.setGhostState(ghost.id, state)
            guard isCurrent(e) else { return }
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
            guard isCurrent(e) else { return }
            setGhostLocally(ghost.id, before)
            pushToast(errText(error, "Could not update that plan"), .error)
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
