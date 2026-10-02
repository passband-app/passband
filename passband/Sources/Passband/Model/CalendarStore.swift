// The calendar page's half of the store: the gate, the timeline load, the
// user's word on a ghost, and the connect card's lifecycle. The stored state
// it works on is declared in AppStore itself (an @Observable class keeps its
// stored properties in its own body); everything here is behaviour.
//
// Shared with the phone target, which has no calendar page yet: nothing here
// draws, and nothing here is reachable there until something calls it.

import Foundation
import SwiftUI

/// The connect card, while it is up: how this mailbox connects, and which
/// Google account to pick when Google asks. `email` is nil on a daemon too old
/// to name its own address; the card then says nothing about accounts rather
/// than guessing one.
struct CalendarConnectAsk: Hashable {
    let route: CalendarGate.ConnectRoute
    let email: String?
}

extension AppStore {
    // MARK: - the gate

    /// Whether the Calendar destination exists at all. Rail, Go menu, ⇧C and
    /// the route all read this one property; the rule itself, and why it
    /// includes developer mode, is `CalendarGate.pageAvailable`.
    var calendarPageAvailable: Bool {
        CalendarGate.pageAvailable(
            timeline: stats?.timeline, developerMode: Prefs.shared.developerMode)
    }

    /// How this mailbox could connect a calendar; nil when there is nothing
    /// to offer (granted, an older daemon, or nothing usable sent).
    var calendarConnectRoute: CalendarGate.ConnectRoute? {
        CalendarGate.connectRoute(stats?.calendar)
    }

    var calendarGranted: Bool { stats?.calendar?.granted == true }

    /// What the page offers about connecting, given what this account has
    /// already been shown.
    var calendarOffer: CalendarGate.Offer {
        CalendarGate.offer(
            route: calendarConnectRoute, cardSeen: calendarCardSeen,
            practice: RehearsalMode.isEnabled)
    }

    /// True between Connect and the grant arriving, for as long as Google's
    /// consent page can still be finished.
    func calendarWaiting(now: Date = Date()) -> Bool {
        CalendarGate.isWaiting(since: calendarConnectWaitingSince, now: now)
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

    private func markCalendarCardSeen() {
        UserDefaults.standard.set(true, forKey: calendarCardKey)
    }

    // MARK: - the timeline

    /// Load the agenda. Rows already on screen stay until a newer answer
    /// replaces them, and a failure says nothing: the page keeps what it has
    /// and the next poll tick tries again. `kick` first asks the daemon to
    /// sync the calendar now (on the page's appearance); a daemon without the
    /// sync, or inside its throttle, answers something this ignores.
    func refreshCalendar(kick: Bool = false) async {
        let e = epoch
        if kick { try? await APIClient.shared.refreshTimeline() }
        guard isCurrent(e) else { return }
        // Local midnight YESTERDAY: last night's still-running party has to
        // come back, and the daemon judges `expired` in this offset.
        let cal = Calendar.current
        let from = cal.date(byAdding: .day, value: -1, to: cal.startOfDay(for: Date())) ?? Date()
        do {
            let feed = try await APIClient.shared.getTimeline(from: from)
            guard isCurrent(e) else { return }
            // Assignment is invalidation: an unchanged answer is not written.
            if feed != calendarFeed { calendarFeed = feed }
        } catch {
            guard isCurrent(e) else { return }
        }
        if !calendarLoaded { calendarLoaded = true }
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

    private func setGhostLocally(_ id: Int, _ state: GhostState) {
        guard var feed = calendarFeed, let i = feed.ghosts.firstIndex(where: { $0.id == id })
        else { return }
        feed.ghosts[i].state = state
        calendarFeed = feed
    }

    // MARK: - the connect card

    /// The page's first visit for an account that has not connected: raise
    /// the card, once. Every later visit gets the banner instead.
    func raiseCalendarCardIfOwed() {
        guard calendarOffer == .card, calendarConnectAsk == nil else { return }
        openCalendarConnect()
    }

    /// Raise the card on purpose (the banner's Connect).
    func openCalendarConnect() {
        guard let route = calendarConnectRoute else { return }
        let email = stats?.account_email?.trimmingCharacters(in: .whitespacesAndNewlines)
        calendarConnectAsk = CalendarConnectAsk(
            route: route, email: (email?.isEmpty ?? true) ? nil : email)
    }

    /// Connect: open Google's consent in the browser and wait for the grant
    /// to show up on the stats poll. The card stays up, saying so.
    func confirmCalendarConnect() {
        guard let ask = calendarConnectAsk, case .link(let url) = ask.route else { return }
        markCalendarCardSeen()
        calendarConnectWaitingSince = Date()
        Analytics.capture("calendar_connect_clicked", ["self_host": false])
        Opener.open(url)
    }

    /// Self-host's Connect: the command goes on the clipboard, to be run where
    /// the daemon runs. The card stays up and waits the same way.
    func copyCalendarCommand() {
        guard let ask = calendarConnectAsk, case .command(let command) = ask.route else { return }
        markCalendarCardSeen()
        Clip.copy(command)
        calendarConnectWaitingSince = Date()
        Analytics.capture("calendar_connect_clicked", ["self_host": true])
    }

    /// Any way out of the card: Not now, Escape, the scrim, or Close while
    /// waiting. Never raised unprompted again for this account. Closing a
    /// card that is already waiting is not a refusal, and is not counted as
    /// one.
    func dismissCalendarConnect() {
        guard calendarConnectAsk != nil else { return }
        let waiting = calendarWaiting()
        calendarConnectAsk = nil
        markCalendarCardSeen()
        if !waiting { Analytics.capture("calendar_connect_dismissed") }
    }

    /// The grant arrived (on the stats poll): put the card and the wait away
    /// and load the real events. Called by the shell's watcher, so it lands
    /// wherever the user is.
    func calendarGrantArrived() {
        let wasAsking = calendarConnectAsk != nil || calendarConnectWaitingSince != nil
        calendarConnectAsk = nil
        calendarConnectWaitingSince = nil
        guard wasAsking else { return }
        pushToast("Google Calendar connected", .success)
        Task { await refreshCalendar(kick: true) }
    }
}
