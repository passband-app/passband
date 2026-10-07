// WHEN TO TELL SOMEBODY THEIR MAILBOX HAS AN AGENT DOOR. Once, after a week of
// the app, and only when the door would actually let an agent in.
//
// Most people never find Settings > Agents, so they never learn that Claude,
// Codex, Cursor and the rest can read what Passband has triaged. This is the
// one time the app says so. Every condition below is a reason NOT to:
//
//   * a week has not passed since the first launch `ShareNudge` stamps (no
//     stamp reads as "not yet", as it does for the other asks);
//   * it has been asked before, ever, on this install;
//   * the person has opened Settings > Agents already, so they know (stamped
//     by the pane itself, see `notePaneSeen`);
//   * the practice inbox is up, where there is no real door to point at;
//   * another one-time card is on screen, or the iPhone ad went up in the last
//     three days: two ads in one week is a campaign, not a tip;
//   * the door is shut. Only the probe can say (`AgentConnect.probe`), and only
//     `.open` and `.signInRequired` are doors an agent can use. Advertising a
//     404 sends somebody to a pane that explains why it cannot work;
//   * an agent has already been through it. The daemon has no field for this
//     and this does not invent one: the one trace an agent leaves on the human
//     door is an audit row it wrote with actor "agent" (the MCP door's
//     `set_sender_rule`). That catches only agents that set a rule, never a
//     read-only one, so it is a further "no" and not a gate on its own. Nil
//     (the audit did not answer) is "do not know", which waits.
//
// The first five are free. The last two cost requests, so they run only when
// the free ones already pass: an install that has been asked, or is a day old,
// never probes anything.
//
// ASKED ONCE, EVER, recorded when the card goes up and before any answer, like
// the share ask and the iPhone ad. The pane stays in Settings for anyone who
// wants it later.

import Foundation
import Observation

@MainActor
@Observable
final class AgentNudge {
    static let shared = AgentNudge()

    /// How long after the first launch the ask becomes possible.
    nonisolated static let askAfter: TimeInterval = 7 * 24 * 3600

    /// How long the iPhone ad keeps this one off the screen after it went up.
    nonisolated static let quietAfterIPhoneAd: TimeInterval = 3 * 24 * 3600

    /// After the door or the audit said no, how long before an activation may
    /// ask them again. In memory only: a relaunch asks afresh, and an install
    /// whose door stays shut costs one probe per quarter hour of use at most.
    nonisolated static let recheckAfter: TimeInterval = 15 * 60

    private nonisolated static let askedKey = "passband.agents.nudgeAsked"
    /// Written by the Agents pane the first time it appears.
    nonisolated static let paneSeenKey = "passband.agents.paneSeen"

    private let defaults = UserDefaults.standard

    /// Whether the card is up right now. Owned here so the one place that
    /// decides "ask" is the one place that records "asked".
    var showingNudge = false

    @ObservationIgnored private var checking = false
    @ObservationIgnored private var lastRefusal: Date?

    /// `PASSBAND_FORCE_AGENT_NUDGE=1` puts the card up at launch without
    /// recording an ask, so it can be looked at without a week-old install and
    /// an open door. Same posture as `PASSBAND_FORCE_TESTFLIGHT_NUDGE`.
    nonisolated static let forced =
        ProcessInfo.processInfo.environment["PASSBAND_FORCE_AGENT_NUDGE"] == "1"

    private init() {
        showingNudge = Self.forced
    }

    // MARK: - the rule

    /// THE FREE HALF, pure: everything that needs no request. Only when this
    /// says yes is the door probed at all.
    nonisolated static func prerequisites(
        firstUse: Date?,
        asked: Bool,
        paneSeen: Bool,
        practice: Bool,
        othersShowing: Bool,
        iPhoneAdAskedAt: Date?,
        now: Date
    ) -> Bool {
        guard !asked, !paneSeen, !practice, !othersShowing, let firstUse else { return false }
        guard now.timeIntervalSince(firstUse) > askAfter else { return false }
        // An iPhone ad from before its stamp existed has no date, and counts
        // as long ago.
        if let iPhoneAdAskedAt, now.timeIntervalSince(iPhoneAdAskedAt) < quietAfterIPhoneAd {
            return false
        }
        return true
    }

    /// Whether a probe's answer is a door an agent can use.
    nonisolated static func usable(_ door: AgentDoorStatus) -> Bool {
        door == .open || door == .signInRequired
    }

    /// THE WHOLE RULE, pure, so a headless suite can pin it. `agentSeen` nil is
    /// "the audit did not answer", which is a no for now: an ask that comes a
    /// launch later beats one telling somebody about the agent they already run.
    nonisolated static func earned(
        firstUse: Date?,
        asked: Bool,
        paneSeen: Bool,
        practice: Bool,
        othersShowing: Bool,
        iPhoneAdAskedAt: Date?,
        door: AgentDoorStatus,
        agentSeen: Bool?,
        now: Date
    ) -> Bool {
        prerequisites(
            firstUse: firstUse, asked: asked, paneSeen: paneSeen, practice: practice,
            othersShowing: othersShowing, iPhoneAdAskedAt: iPhoneAdAskedAt, now: now)
            && usable(door) && agentSeen == false
    }

    /// Whether an audit page shows an agent has been through the door: a row
    /// the MCP door wrote. Exactly "agent", the one actor it writes, and not
    /// the audit view's looser "agent-ish" prefixes, which also take in the
    /// in-app assistant.
    nonisolated static func agentSeen(inAuditActors actors: [String]) -> Bool {
        actors.contains { $0 == "agent" }
    }

    // MARK: - this install

    private func prerequisitesHold(othersShowing: Bool, now: Date) -> Bool {
        Self.prerequisites(
            firstUse: defaults.object(forKey: ShareNudge.firstUseKey) as? Date,
            asked: defaults.bool(forKey: Self.askedKey),
            paneSeen: defaults.bool(forKey: Self.paneSeenKey),
            practice: RehearsalMode.isEnabled,
            othersShowing: othersShowing,
            iPhoneAdAskedAt: TestFlightNudge.askedAt,
            now: now)
    }

    /// Ask if this install has earned it. The two answers that cost a request
    /// are passed in rather than fetched here, so this file stays free of the
    /// API client: `door` probes the agent door, `agentSeen` reads the audit.
    /// `othersShowing` is read again after they land, because a card that came
    /// up while the probe was out still wins.
    func askIfEarned(
        othersShowing: @MainActor () -> Bool,
        door: () async -> AgentDoorStatus?,
        agentSeen: () async -> Bool?
    ) async {
        guard !checking, !showingNudge else { return }
        if let lastRefusal, Date().timeIntervalSince(lastRefusal) < Self.recheckAfter { return }
        guard prerequisitesHold(othersShowing: othersShowing(), now: Date()) else { return }
        checking = true
        defer { checking = false }

        // A cancelled probe (nil) is not an answer about the door.
        guard let found = await door() else { return }
        guard Self.usable(found) else {
            lastRefusal = Date()
            return
        }
        let seen = await agentSeen()
        guard
            Self.earned(
                firstUse: defaults.object(forKey: ShareNudge.firstUseKey) as? Date,
                asked: defaults.bool(forKey: Self.askedKey),
                paneSeen: defaults.bool(forKey: Self.paneSeenKey),
                practice: RehearsalMode.isEnabled,
                othersShowing: othersShowing(),
                iPhoneAdAskedAt: TestFlightNudge.askedAt,
                door: found,
                agentSeen: seen,
                now: Date())
        else {
            if seen != false { lastRefusal = Date() }
            return
        }
        ask()
    }

    /// Put the card up and record that we did, before any answer: a crash
    /// with the card on screen has still asked.
    func ask() {
        defaults.set(true, forKey: Self.askedKey)
        showingNudge = true
    }

    /// The Agents pane appeared. Whoever has been there knows the door exists,
    /// so the ad never goes up after this. A card already up stays until its
    /// own ✕ or button: pulling it out from under somebody mid-read is ruder
    /// than leaving it.
    func notePaneSeen() {
        defaults.set(true, forKey: Self.paneSeenKey)
    }
}
