// The card for a sender who ignored an unsubscribe: what they did, the smart
// block on offer when there is one, and the three answers. Mounted in the
// Mac's toast stack (ActionLayer) and the phone's (MobileToastHost); the state
// and the verbs are Model/UnsubscribeViolations.swift.

import SwiftUI

struct UnsubscribeViolationNotice: View {
    let record: UnsubscribeRecord

    private var model: UnsubscribeViolations { .shared }

    var body: some View {
        // Sender strings are email-derived: rendered as Text only, never as
        // markup.
        let name = SenderCache.resolved(record.sender).displayName
        let smart = record.smart_block
        return VStack(alignment: .leading, spacing: Space.scaled(9)) {
            Text(
                "\(name) is ignoring your unsubscribe. "
                    + "\(record.violation_count) email\(record.violation_count == 1 ? "" : "s") "
                    + "since you asked \(Fmt.relAge(record.requested_at)) ago."
            )
            .font(Typo.rowSub)
            .foregroundStyle(Palette.ink)
            .fixedSize(horizontal: false, vertical: true)

            if let smart { smartBlockProposal(smart) }

            HStack(spacing: Space.s8) {
                // With a smart block on offer it is the recommendation, and one
                // notice gets one prominent button.
                let blockSender = Button("Block sender") { Task { await model.block(record) } }
                if let smart {
                    blockSender.buttonStyle(.glass)
                    Button("Smart block") { Task { await model.smartBlock(record, smart) } }
                        .buttonStyle(.glassProminent)
                        .tint(Palette.accent)
                        .help("Saves this rule for \(record.sender): \(smart.rule)")
                } else {
                    blockSender.buttonStyle(.glassProminent).tint(Palette.danger)
                }
                Button("Keep") { Task { await model.keep(record) } }
                    .buttonStyle(.glass)
            }
            .controlSize(.small)
            .disabled(model.busy)
        }
        // A WIDTH, not a maximum, on the Mac. The toast stack is measured at
        // its ideal size, where a maximum proposes nothing and the sentence
        // measures as one line; it then wraps to two when placed, and the line
        // the stack never counted hung down through the layer's bottom margin.
        // The phone's stack is offered the screen, so there the screen decides.
        #if os(macOS)
            .frame(width: Typo.measure(360), alignment: .leading)
        #else
            .frame(maxWidth: .infinity, alignment: .leading)
        #endif
        .padding(Space.scaled(13))
        .passbandGlass(.pane, cornerRadius: 14, tint: Palette.dangerSoft)
    }

    /// What the smart block would do, and under it the sentence it saves: the
    /// rule is model text about the sender's own subjects, so it is on screen
    /// in full before the button that keeps it can be pressed.
    private func smartBlockProposal(_ smart: SmartBlockProposal) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
            Image(systemName: "sparkles")
                .font(Typo.micro)
                .foregroundStyle(Palette.accent)
            VStack(alignment: .leading, spacing: Space.s2) {
                Text("Keep \(smart.keeps), drop \(smart.drops)")
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.ink)
                Text("Rule: \(smart.rule)")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
            }
            .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// Asks for the unsubscribe ledger on the poll tick, from a zero-size leaf
/// (each shell hangs one behind its toasts), and NOT ON EVERY TICK: a
/// violation is something a sender did days ago, and a request every ten
/// seconds for it was the busiest thing this client asked the daemon for.
/// Once a minute.
struct ViolationScanKeeper: View {
    @Environment(AppStore.self) private var store
    @State private var lastScan: Date?
    private static let interval: TimeInterval = 60

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .task(id: store.lastRefresh) {
                if let lastScan, Date().timeIntervalSince(lastScan) < Self.interval { return }
                lastScan = Date()
                // Detached from this task's cancellation: the next tick
                // replaces the task, and a scan cut off while it waits on a
                // smart block would pay for the answer and never read it.
                await Task { await UnsubscribeViolations.shared.scan() }.value
            }
    }
}
