// THE QUIET BOARD: what the sitrep shows once there is nothing left on it. An
// empty board is the best state this app has, and it used to look like a
// broken one: a headline over a void.
//
// Two pieces. `ClearNote` sits under the hero and says it in words: a line of
// encouragement, today's tally, a run of clear days. `ClearScene` fills the
// page behind everything: the onboarding's squelch scene, gate closed.
//
// THE MOMENT: when the reader clears the last thing (on this page, or in the
// reader and then comes back), the gate drops open, noise pours through, and
// the squelch closes on it again, with the Squelch chime. Once per clearing,
// never on a board that simply opened quiet: a reward nobody earned is just
// noise, which is the one thing this surface is against.

import SwiftUI

#if os(macOS)
    import AppKit
#endif

/// Whether the board has held anything since it was last seen quiet. Process
/// wide rather than view state, because the clearing that earns the moment
/// usually happens in the reader, while the sitrep is not mounted at all.
@MainActor
enum ClearMoment {
    static var armed = false
}

// MARK: - the note

struct ClearNote: View {
    @State private var today: MailActivityDay?
    @State private var streak = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(ClearCopy.tagline())
                .font(Typo.serif(19))
                .foregroundStyle(Palette.inkDim)
            if let line = tally {
                Text(line)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if streak > 1 {
                StreakChip(days: streak).padding(.top, 4)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .task {
            streak = ClearStreak.markToday()
            await loadToday()
        }
    }

    /// Today's traffic, said as the thing Passband did about it. Nil until
    /// the numbers arrive, or when the daemon cannot say: no line beats a
    /// line of zeros that are really "unknown".
    private var tally: String? {
        guard let d = today else { return nil }
        return ClearCopy.tally(
            received: d.received, noise: d.noise, attention: d.past_due + d.deadline + d.signal)
    }

    private func loadToday() async {
        guard let res = try? await APIClient.shared.getMailActivity(days: 1) else { return }
        // Sparse rows: a day with no mail has no row at all, and that is a
        // real zero rather than a missing answer.
        today = res.rows.first { $0.day == res.until }
            ?? MailActivityDay(
                day: res.until, received: 0, sent: 0, sealed: 0, past_due: 0,
                deadline: 0, signal: 0, noise: 0, pending: nil)
    }
}

private struct StreakChip: View {
    let days: Int

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: "antenna.radiowaves.left.and.right")
                .font(.system(size: 10, weight: .semibold))
            Text("\(days) clear days running")
        }
        .font(Typo.chip)
        .foregroundStyle(Palette.accent)
        .padding(.horizontal, 9)
        .padding(.vertical, 4)
        .glassCapsule(tint: Palette.accent.opacity(0.16), interactive: false)
        .help("Days this board has been seen all clear, back to back, on this Mac.")
    }
}

// MARK: - the scene

/// The onboarding's squelch scene as the quiet board's backdrop, scrims and
/// all (`IntroSceneBackdrop`), so the page the reader earns looks like the
/// page they were promised on the first launch. At rest the squelch is closed.
/// After a clearing it drops open, noise pours through the gate, and it closes
/// again on the Squelch chime.
///
/// Held on its settled frame while this window is not key: a mail app sits
/// open all day, and a Metal display link running behind other windows is a
/// battery bug with a nice animation on top.
struct ClearScene: View {
    /// When the board was cleared, or nil for a board that opened quiet.
    let clearedAt: Date?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    #if os(macOS)
        @Environment(\.controlActiveState) private var activeState
        private var paused: Bool { activeState != .key }
    #else
        private let paused = false
    #endif

    @State private var engaged = true

    /// How long the gate stands open before it closes on the chime: long
    /// enough to see the noise pour, short enough to still read as a reward.
    private static let openFor: Duration = .milliseconds(1600)

    var body: some View {
        IntroSceneBackdrop(engaged: engaged, wide: true, paused: paused)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
            .task(id: clearedAt) {
                guard clearedAt != nil else { return }
                if !reduceMotion {
                    engaged = false
                    try? await Task.sleep(for: Self.openFor)
                    guard !Task.isCancelled else { return }
                    engaged = true
                }
                Self.chime()
            }
    }

    #if os(macOS)
        private static var sound: NSSound?
    #endif

    private static func chime() {
        #if os(macOS)
            NSHapticFeedbackManager.defaultPerformer.perform(.levelChange, performanceTime: .now)
            // The Mac bundles the chimes under Sounds/; the phone's are flat.
            let url =
                Bundle.main.url(forResource: "Squelch", withExtension: "caf", subdirectory: "Sounds")
                ?? Bundle.main.url(forResource: "Squelch", withExtension: "caf")
            if let url, let s = NSSound(contentsOf: url, byReference: true) {
                s.volume = 0.35
                // Held until it finishes: a local can be released mid-chime.
                sound = s
                s.play()
            }
        #endif
    }
}
