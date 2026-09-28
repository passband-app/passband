// THE QUIET BOARD: what the sitrep shows once there is nothing left on it. An
// empty board is the best state this app has, and it used to look like a
// broken one: a headline over a void.
//
// Two pieces. `ClearNote` sits under the hero and says it in words: a line of
// encouragement, today's tally, a run of clear days. `ClearScene` fills the
// whole window behind everything: the onboarding's squelch scene, gate closed.
//
// THE MOMENT: when the reader clears the last thing (on this page, or in the
// reader and then comes back), the Squelch chime plays. Once per clearing,
// never on a board that simply opened quiet: a reward nobody earned is just
// noise, which is the one thing this surface is against.

import SwiftUI

#if os(macOS)
    import AppKit
    import AVFoundation
#endif

/// The quiet board's shared state. The sitrep decides it; the window shell
/// draws the scene from it, because the scene runs under the sidebar rail
/// and the traffic lights too, which are not the sitrep's to paint.
@MainActor
@Observable
final class QuietBoard {
    static let shared = QuietBoard()

    /// The sitrep is on screen with nothing on it.
    var isQuiet = false

    /// Whether the board has held anything since it was last seen quiet.
    /// Process wide rather than view state, because the clearing that earns
    /// the chime usually happens in the reader, while the sitrep is not
    /// mounted at all.
    @ObservationIgnored var armed = false
}

// MARK: - the note

struct ClearNote: View {
    @State private var today: MailActivityDay?
    @State private var streak = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("There is no important mail.")
                    .foregroundStyle(Palette.inkDim)
                AllMailLink()
            }
            .font(Typo.serif(19))
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

/// The way out of an empty board: the mail Passband sorted away is all still
/// there, one click off, for whoever wants to check its work.
private struct AllMailLink: View {
    @Environment(AppStore.self) private var store
    @State private var hovering = false

    var body: some View {
        Button {
            store.openMail(.inbox)
        } label: {
            Text("View all mail")
                .foregroundStyle(Palette.accent)
                .underline(hovering, color: Palette.accent)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .pointingHand()
        .accessibilityHint("opens all mail")
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

/// The quiet board's backdrop: the P7 phosphor squelch from brand/phosphor,
/// pre-rendered widescreen with the gate held open (Resources/Brand/
/// P7Squelch.mp4, a seamless 8 s loop, 12 sweeps of persistence so it breathes rather than strobes). A video rather than a live render
/// because the look IS the offline renderer: bloom at four radii, a phosphor
/// tonemap and 9000 beam samples a frame are not something to redo per frame
/// in a mail app.
///
/// Paused while this window is not key, and held on its first frame under
/// Reduce Motion. Falls back to the onboarding's Metal scene when the video
/// is missing (and on the phone, which has not had this pass yet).
struct ClearScene: View {
    #if os(macOS)
        @Environment(\.controlActiveState) private var activeState
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        private var paused: Bool { activeState != .key || reduceMotion }
    #else
        private let paused = false
    #endif

    static let video: URL? =
        Bundle.main.url(forResource: "P7Squelch", withExtension: "mp4", subdirectory: "Brand")
        ?? Bundle.main.url(forResource: "P7Squelch", withExtension: "mp4")

    var body: some View {
        Group {
            #if os(macOS)
                if let url = Self.video {
                    LoopingVideo(url: url, paused: paused)
                } else {
                    IntroSceneBackdrop(engaged: true, wide: true, paused: paused)
                }
            #else
                IntroSceneBackdrop(engaged: true, wide: true, paused: paused)
            #endif
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

#if os(macOS)
    /// A muted, aspect-filled, gaplessly looping video. `AVPlayerLooper` over a
    /// queue player, because a plain player's seek-to-zero on end leaves a
    /// visible hitch at every loop.
    private struct LoopingVideo: NSViewRepresentable {
        let url: URL
        let paused: Bool

        func makeNSView(context: Context) -> LoopingVideoView { LoopingVideoView(url: url) }

        func updateNSView(_ view: LoopingVideoView, context: Context) {
            view.setPaused(paused)
        }

        static func dismantleNSView(_ view: LoopingVideoView, coordinator: ()) {
            view.setPaused(true)
        }
    }

    private final class LoopingVideoView: NSView {
        private let player = AVQueuePlayer()
        private var looper: AVPlayerLooper?
        private let playerLayer = AVPlayerLayer()

        init(url: URL) {
            super.init(frame: .zero)
            player.isMuted = true
            // Never wake the display or hold off sleep for a backdrop.
            player.preventsDisplaySleepDuringVideoPlayback = false
            looper = AVPlayerLooper(player: player, templateItem: AVPlayerItem(url: url))
            playerLayer.player = player
            playerLayer.videoGravity = .resizeAspectFill
            wantsLayer = true
            layer = playerLayer
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        func setPaused(_ paused: Bool) {
            if paused {
                if player.rate != 0 { player.pause() }
            } else if player.rate == 0 {
                player.play()
            }
        }
    }
#endif

/// The Squelch chime and a haptic tick, for the moment the board is cleared.
@MainActor
enum ClearChime {
    #if os(macOS)
        private static var sound: NSSound?
    #endif

    static func play() {
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
