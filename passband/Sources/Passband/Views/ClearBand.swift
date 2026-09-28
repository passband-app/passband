// CLEAR BAND: what the sitrep's work surface shows when there is nothing left
// on it. An empty board is the best state this app has, and it used to look
// like a broken one: a headline over a void. This answers it in the product's
// own terms. The band is quiet, one clean carrier runs across it, and a tally
// says how much noise it took to get there.
//
// THE MOMENT: when the reader clears the last item (on this page, or in the
// reader and then comes back), the trace starts as static and squelches down
// into the carrier, with a pulse and the Squelch chime. That happens once per
// clearing, never on a board that simply opened empty: a reward nobody earned
// is just noise, which is the one thing this surface is against.

import SwiftUI

#if os(macOS)
    import AppKit
#endif

/// Whether the board has held items since it was last seen clear. Process
/// wide rather than view state, because the clearing that earns the moment
/// usually happens in the reader, while the sitrep is not mounted at all.
@MainActor
enum ClearMoment {
    static var armed = false
}

struct ClearBand: View {
    /// When the last item was cleared, or nil for a board that opened clear.
    let clearedAt: Date?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var today: MailActivityDay?
    @State private var streak = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            CarrierTrace(clearedAt: reduceMotion ? nil : clearedAt, still: reduceMotion)
                .frame(height: 100)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 6) {
                Text(ClearCopy.tagline())
                    .font(Typo.serif(19))
                    .foregroundStyle(Palette.ink)
                if let line = tally {
                    Text(line)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if streak > 1 {
                StreakChip(days: streak)
            }
        }
        .padding(.horizontal, 20)
        .padding(.top, 8)
        .padding(.bottom, 18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .task {
            streak = ClearStreak.markToday()
            await loadToday()
        }
    }

    // MARK: - copy

    /// Today's traffic, said as the thing Passband did about it. Nil until
    /// the numbers arrive, or when the daemon cannot say: no line beats a
    /// line of zeros that are really "unknown".
    private var tally: String? {
        guard let d = today else { return nil }
        return ClearCopy.tally(received: d.received, noise: d.noise, attention: d.past_due + d.deadline + d.signal)
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

// MARK: - streak chip

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

// MARK: - the trace

/// One carrier across a quiet band. At rest it drifts and breathes, slowly
/// enough to read as alive rather than busy. After a clearing it starts as
/// static and squelches down into the carrier.
///
/// Ticks only while this window is key: a mail app sits open all day, and a
/// page that redraws in the background forever is a battery bug with a nice
/// animation on top.
private struct CarrierTrace: View {
    let clearedAt: Date?
    let still: Bool

    #if os(macOS)
        @Environment(\.controlActiveState) private var activeState
        private var paused: Bool { still || activeState != .key }
    #else
        private var paused: Bool { still }
    #endif

    /// How long the static takes to squelch into the carrier.
    static let squelchTime: Double = 1.5
    /// How long the pulse takes to reach the edges.
    static let pulseTime: Double = 1.2

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30, paused: paused)) { tl in
            Canvas { ctx, size in
                draw(&ctx, size, now: tl.date)
            }
        }
        .onChange(of: clearedAt, initial: true) { _, at in
            if at != nil { Self.celebrate() }
        }
    }

    // MARK: drawing

    private func draw(_ ctx: inout GraphicsContext, _ size: CGSize, now: Date) {
        let t = still ? 0 : now.timeIntervalSinceReferenceDate
        let since = clearedAt.map { now.timeIntervalSince($0) } ?? .infinity
        // 1 = full static, 0 = clean carrier. Eased out so the collapse lands
        // softly instead of stopping dead.
        let raw = max(0, 1 - since / Self.squelchTime)
        let noise = raw * raw * (3 - 2 * raw)
        let w = size.width, h = size.height, mid = h / 2

        grid(&ctx, size)

        // The pulse, from the band's centre, behind the line.
        if since < Self.pulseTime {
            let p = since / Self.pulseTime
            let r = (0.08 + p) * max(w, h) * 0.55
            let ring = Path(ellipseIn: CGRect(x: w / 2 - r, y: mid - r * 0.34, width: r * 2, height: r * 0.68))
            ctx.stroke(ring, with: .color(Palette.accent.opacity(0.7 * (1 - p))), lineWidth: 2)
            sparks(&ctx, size, progress: p)
        }

        let line = carrier(size, t: t, noise: noise)
        // Glow first, then the line itself on top of it.
        var glow = ctx
        glow.addFilter(.blur(radius: 7))
        glow.stroke(line, with: .color(Palette.accent.opacity(0.55)), lineWidth: 5)
        ctx.stroke(
            line,
            with: .linearGradient(
                Gradient(stops: [
                    .init(color: Palette.accent.opacity(0), location: 0),
                    .init(color: Palette.accent, location: 0.25),
                    .init(color: Palette.accent, location: 0.75),
                    .init(color: Palette.accent.opacity(0), location: 1),
                ]),
                startPoint: .zero, endPoint: CGPoint(x: w, y: 0)),
            style: StrokeStyle(lineWidth: 1.8, lineCap: .round, lineJoin: .round))
    }

    /// The carrier: a sine under a window that tapers it flat at the band
    /// edges, plus static scaled by `noise`.
    private func carrier(_ size: CGSize, t: Double, noise: Double) -> Path {
        let w = size.width, mid = size.height / 2
        let amp = 11 + 2.5 * sin(t * 0.7)
        let phase = t * 1.1
        let wavelength = max(90, w / 7)
        // Static redraws at 24 steps a second rather than every frame: noise
        // that re-rolls at 60 Hz reads as flicker, not as a radio.
        let seed = Int(t * 24)
        var path = Path()
        let step: CGFloat = 2
        var x: CGFloat = 0
        while x <= w {
            let u = Double(x / w)
            let window = pow(sin(.pi * u), 1.6)
            var y = amp * window * sin(2 * .pi * Double(x) / wavelength + phase)
            if noise > 0 {
                y += noise * (mid - 8) * 0.9 * (Self.hash(Int(x / step), seed) * 2 - 1)
            }
            let pt = CGPoint(x: x, y: mid - y)
            if x == 0 { path.move(to: pt) } else { path.addLine(to: pt) }
            x += step
        }
        return path
    }

    /// A centre line and the two band edges, faint enough to be felt rather
    /// than read: it is what makes the line look like it is ON something.
    private func grid(_ ctx: inout GraphicsContext, _ size: CGSize) {
        let w = size.width, h = size.height
        var centre = Path()
        centre.move(to: CGPoint(x: 0, y: h / 2))
        centre.addLine(to: CGPoint(x: w, y: h / 2))
        ctx.stroke(centre, with: .color(Palette.hairline), lineWidth: 0.5)
        for edge in [0.12, 0.88] {
            var p = Path()
            p.move(to: CGPoint(x: w * edge, y: h * 0.18))
            p.addLine(to: CGPoint(x: w * edge, y: h * 0.82))
            ctx.stroke(
                p, with: .color(Palette.hairline),
                style: StrokeStyle(lineWidth: 0.75, dash: [2, 4]))
        }
    }

    /// A handful of sparks thrown off the carrier as it locks. Positions are
    /// seeded, so every frame of one clearing agrees about where each spark is.
    private func sparks(_ ctx: inout GraphicsContext, _ size: CGSize, progress p: Double) {
        let w = size.width, mid = size.height / 2
        let ease = 1 - pow(1 - p, 3)
        for i in 0..<22 {
            let x0 = w * (0.15 + 0.7 * Self.hash(i, 7))
            let angle = Double.pi * (0.15 + 0.7 * Self.hash(i, 11)) * (Self.hash(i, 13) > 0.5 ? 1 : -1)
            let dist = (40 + 70 * Self.hash(i, 17)) * ease
            let x = x0 + cos(angle) * dist * 0.6
            let y = mid - sin(angle) * dist
            let r = 1.8 + 2.2 * Self.hash(i, 19)
            let color = i % 3 == 0 ? Palette.positive : Palette.accent
            ctx.fill(
                Path(ellipseIn: CGRect(x: x - r, y: y - r, width: r * 2, height: r * 2)),
                with: .color(color.opacity(1 - p * p)))
        }
    }

    /// Cheap deterministic noise in [0, 1).
    private static func hash(_ a: Int, _ b: Int) -> Double {
        var h = UInt64(truncatingIfNeeded: a &* 374_761_393 &+ b &* 668_265_263)
        h = (h ^ (h >> 13)) &* 1_274_126_177
        h ^= h >> 16
        return Double(h & 0xFFFF) / 65536
    }

    // MARK: the chime and the tap

    #if os(macOS)
        private static var chime: NSSound?
    #endif

    private static func celebrate() {
        #if os(macOS)
            NSHapticFeedbackManager.defaultPerformer.perform(.levelChange, performanceTime: .now)
            // The Mac bundles the chimes under Sounds/; the phone's are flat.
            let url =
                Bundle.main.url(forResource: "Squelch", withExtension: "caf", subdirectory: "Sounds")
                ?? Bundle.main.url(forResource: "Squelch", withExtension: "caf")
            if let url, let sound = NSSound(contentsOf: url, byReference: true) {
                sound.volume = 0.35
                // Held until it finishes: a local that goes out of scope can
                // be released mid-chime.
                chime = sound
                sound.play()
            }
        #endif
    }
}
