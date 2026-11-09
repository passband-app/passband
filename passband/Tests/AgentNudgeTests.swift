// The agent-door ad, and every way it must NOT fire. The rule is a pure static,
// so this needs no daemon, no probe, no clock and no UserDefaults: each case
// starts from one that asks and flips exactly one input.

import Foundation

@main
@MainActor
struct AgentNudgeTests {
    static let now = Date(timeIntervalSince1970: 1_755_600_000)
    static func ago(days: Double) -> Date { now.addingTimeInterval(-days * 24 * 3600) }

    static func earned(
        firstUse: Date? = ago(days: 10),
        asked: Bool = false,
        paneSeen: Bool = false,
        fixtures: Bool = false,
        othersShowing: Bool = false,
        iPhoneAdAskedAt: Date? = nil,
        door: AgentDoorStatus = .open,
        agentSeen: Bool? = false
    ) -> Bool {
        AgentNudge.earned(
            firstUse: firstUse, asked: asked, paneSeen: paneSeen, fixtures: fixtures,
            othersShowing: othersShowing, iPhoneAdAskedAt: iPhoneAdAskedAt,
            door: door, agentSeen: agentSeen, now: now)
    }

    static func free(
        firstUse: Date? = ago(days: 10),
        asked: Bool = false,
        paneSeen: Bool = false,
        fixtures: Bool = false,
        othersShowing: Bool = false,
        iPhoneAdAskedAt: Date? = nil
    ) -> Bool {
        AgentNudge.prerequisites(
            firstUse: firstUse, asked: asked, paneSeen: paneSeen, fixtures: fixtures,
            othersShowing: othersShowing, iPhoneAdAskedAt: iPhoneAdAskedAt, now: now)
    }

    static func main() {
        // The baseline: ten days in, never asked, never been to the pane, the
        // door open, no agent seen. Ask. Both doors an agent can use count.
        precondition(earned())
        precondition(earned(door: .signInRequired))

        // "More than seven days": 6.9 and exactly 7 are not, 7.1 is.
        precondition(!earned(firstUse: ago(days: 6.9)))
        precondition(!earned(firstUse: ago(days: 7)))
        precondition(earned(firstUse: ago(days: 7.1)))
        // No first-launch stamp reads as "not yet".
        precondition(!earned(firstUse: nil))

        // Once, ever.
        precondition(!earned(asked: true))
        // Somebody who has opened Settings > Agents knows.
        precondition(!earned(paneSeen: true))
        // Never over the fixture inbox.
        precondition(!earned(fixtures: true))
        // Never on top of another one-time card.
        precondition(!earned(othersShowing: true))

        // Three days of quiet after the iPhone ad; an ask with no date (from
        // before the stamp existed) is long ago.
        precondition(!earned(iPhoneAdAskedAt: ago(days: 1)))
        precondition(!earned(iPhoneAdAskedAt: ago(days: 2.9)))
        precondition(earned(iPhoneAdAskedAt: ago(days: 3.1)))
        precondition(earned(iPhoneAdAskedAt: nil))

        // A door that is shut, or not known to be open, is never advertised.
        for door: AgentDoorStatus in [.notServed, .hostRefused, .unreachable, .unknown, .checking] {
            precondition(!earned(door: door), "advertised a \(door) door")
        }

        // An agent that has already been through: no. The audit not
        // answering is "do not know", which waits.
        precondition(!earned(agentSeen: true))
        precondition(!earned(agentSeen: nil))

        // The free half agrees with the whole rule on everything it covers, and
        // knows nothing of the door: it is what decides whether to probe.
        precondition(free())
        precondition(!free(firstUse: ago(days: 3)))
        precondition(!free(firstUse: nil))
        precondition(!free(asked: true))
        precondition(!free(paneSeen: true))
        precondition(!free(fixtures: true))
        precondition(!free(othersShowing: true))
        precondition(!free(iPhoneAdAskedAt: ago(days: 1)))

        // The audit signal: exactly the MCP door's actor.
        precondition(AgentNudge.agentSeen(inAuditActors: ["client-api", "agent"]))
        precondition(!AgentNudge.agentSeen(inAuditActors: ["client-api", "human"]))
        precondition(!AgentNudge.agentSeen(inAuditActors: ["assistant"]))
        precondition(!AgentNudge.agentSeen(inAuditActors: []))

        print("agent-nudge: ok")
    }
}
