// THE QUIET BOARD: what the sitrep shows once there is nothing left on it. An
// empty board is the best state this app has, and it used to look like a
// broken one: a headline over a void.
//
// Two pieces. `ClearNote` sits under the hero and says it in words: a line of
// encouragement, today's tally, a run of clear days. `ClearScope` fills the
// page behind everything: the landing page's phosphor analyzer (passband-site/
// scope.ts), ported, with the squelch closed and nothing on the band.
//
// THE MOMENT: when the reader clears the last thing (on this page, or in the
// reader and then comes back), the scope starts wide open and full of static
// and the squelch closes on it, with the Squelch chime. Once per clearing,
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

// MARK: - the scope

/// The landing page's analyzer as a page background. x is frequency, y is
/// energy: a noise floor of wandering humps and transient spikes (the
/// digests, the offers, the notifications) under an 8th-order filter curve.
/// At rest the squelch is closed, so only a quiet floor survives inside the
/// band and the spikes outside it barely lift the line. No carriers: nothing
/// on this board needs you, and the picture says so.
///
/// Ticks only while this window is key, at 30 Hz: a mail app sits open all
/// day, and a page that redraws in the background forever is a battery bug
/// with a nice animation on top.
struct ClearScope: View {
    /// When the board was cleared, or nil for a board that opened quiet.
    let clearedAt: Date?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    #if os(macOS)
        @Environment(\.controlActiveState) private var activeState
        private var paused: Bool { reduceMotion || activeState != .key }
    #else
        private var paused: Bool { reduceMotion }
    #endif

    @State private var engine = ScopeEngine()

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30, paused: paused)) { tl in
            Canvas { ctx, size in
                engine.frame(
                    &ctx, size, now: tl.date, clearedAt: reduceMotion ? nil : clearedAt,
                    still: reduceMotion)
            }
        }
        // Faded out at every edge, hardest at the top: it is a backdrop for
        // the hero, never something the headline has to compete with.
        .mask {
            LinearGradient(
                stops: [
                    .init(color: .clear, location: 0),
                    .init(color: .black.opacity(0.35), location: 0.3),
                    .init(color: .black, location: 0.62),
                    .init(color: .black, location: 0.9),
                    .init(color: .clear, location: 1),
                ],
                startPoint: .top, endPoint: .bottom)
        }
        .mask {
            LinearGradient(
                stops: [
                    .init(color: .clear, location: 0),
                    .init(color: .black, location: 0.12),
                    .init(color: .black, location: 0.88),
                    .init(color: .clear, location: 1),
                ],
                startPoint: .leading, endPoint: .trailing)
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
        .onChange(of: clearedAt, initial: true) { _, at in
            if at != nil, !reduceMotion { Self.chime() }
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

/// The analyzer's state between frames: the averaged trace, the spikes, and a
/// few frames of history for the phosphor afterglow. A reference type the
/// Canvas mutates while it draws, deliberately outside observation: nothing in
/// SwiftUI should re-render because a spike decayed.
@MainActor
final class ScopeEngine {
    private static let points = 300

    /// Where the band is tuned and how wide it closes, as canvas fractions.
    private static let center = 0.5
    private static let closedWidth = 0.075
    /// How far the squelch sits closed at rest. Not all the way: fully closed
    /// the line outside the band is dead flat, and the page reads as frozen.
    private static let restEngage = 0.9

    private var trace = [Double](repeating: 0.05, count: points)
    private var history: [[Double]] = []
    private var time = 0.0
    private var last: Date?
    private var engage = restEngage

    private struct Spike { var x, h, w, life, decay: Double }
    private var spikes: [Spike] = (0..<22).map { _ in ScopeEngine.spawn(life: .random(in: 0...1)) }

    private static let humps: [(f: Double, p: Double, a: Double)] = [
        (0.07, 0.3, 0.16), (0.05, 2.1, 0.12), (0.11, 4.0, 0.10),
    ]

    private static func spawn(life: Double = 1) -> Spike {
        Spike(
            x: .random(in: 0...1), h: .random(in: 0.18...0.68), w: .random(in: 0.003...0.011),
            life: life, decay: .random(in: 0.6...2.4))
    }

    /// The same passband the landing page, the meter and the app's gate use:
    /// flat top, steep skirts.
    private static func response(_ x: Double, _ c: Double, _ w: Double) -> Double {
        1 / (1 + pow((x - c) / w, 16)).squareRoot()
    }

    private static func ease(_ t: Double) -> Double {
        let k = min(1, max(0, t))
        return k * k * (3 - 2 * k)
    }

    func frame(
        _ ctx: inout GraphicsContext, _ size: CGSize, now: Date, clearedAt: Date?, still: Bool
    ) {
        let dt = min(0.05, last.map { now.timeIntervalSince($0) } ?? 1.0 / 30)
        last = now
        if !still { step(dt) }

        // After a clearing the squelch starts wide open and closes; otherwise
        // it simply sits at rest.
        if let clearedAt, !still {
            let since = now.timeIntervalSince(clearedAt)
            engage = Self.restEngage * Self.ease((since - 0.5) / 2.4)
        } else {
            engage = Self.restEngage
        }

        let w = size.width, h = size.height
        let baseY = h * 0.84
        let amp = h * 0.5
        let e = engage
        // Wide open is a curve flat across the whole page; closed is one band.
        let bw = 1.6 * pow(Self.closedWidth / 1.6, Self.ease(e))

        gate(&ctx, w: w, baseY: baseY, amp: amp, bw: bw, e: e)

        let k = still ? 1 : 1 - exp(-dt * 22)
        for i in 0..<Self.points {
            let x = Double(i) / Double(Self.points - 1)
            trace[i] += (sample(x, e: e, bw: bw, still: still) - trace[i]) * k
        }
        history.append(trace)
        if history.count > 4 { history.removeFirst() }

        // Phosphor: older frames first, dimmer, so the trace leaves a short
        // afterglow the way the site's persistence does.
        for (age, t) in history.enumerated() {
            let fade = Double(age + 1) / Double(history.count)
            stroke(&ctx, path(t, w: w, baseY: baseY, amp: amp), fade: fade * fade)
        }
    }

    private func step(_ dt: Double) {
        time += dt
        for i in spikes.indices {
            spikes[i].life -= dt * spikes[i].decay
            if spikes[i].life <= 0 { spikes[i] = Self.spawn() }
        }
    }

    /// One sample of the spectrum at x, before averaging.
    private func sample(_ x: Double, e: Double, bw: Double, still: Bool) -> Double {
        var noise = 0.035 + (still ? 0.02 : .random(in: 0...0.045))
        for hp in Self.humps {
            let hx = 0.5 + 0.42 * sin(time * hp.f + hp.p)
            noise += hp.a * exp(-pow((x - hx) / 0.09, 2))
        }
        for s in spikes {
            let d = (x - s.x) / s.w
            if d > -4, d < 4 { noise += s.h * s.life * exp(-d * d) }
        }
        // Inside the band the floor survives, quieter; outside, barely.
        let g = Self.response(x, Self.center, bw)
        return noise * (1 - e * (1 - g * 0.35))
    }

    private func path(_ t: [Double], w: CGFloat, baseY: CGFloat, amp: CGFloat) -> Path {
        var p = Path()
        for i in 0..<t.count {
            let pt = CGPoint(
                x: CGFloat(i) / CGFloat(t.count - 1) * w, y: baseY - CGFloat(min(1, t[i])) * amp)
            if i == 0 { p.move(to: pt) } else { p.addLine(to: pt) }
        }
        return p
    }

    /// A wide dim halo, a glow, a near-white core, as on the site.
    private func stroke(_ ctx: inout GraphicsContext, _ p: Path, fade: Double) {
        ctx.stroke(p, with: .color(Palette.accent.opacity(0.06 * fade)), lineWidth: 9)
        ctx.stroke(p, with: .color(Palette.accent.opacity(0.3 * fade)), lineWidth: 3.5)
        ctx.stroke(
            p, with: .color(Self.core.opacity(0.85 * fade)),
            style: StrokeStyle(lineWidth: 1.2, lineJoin: .round))
    }

    private static let core = Color(light: 0x2B7FD4, dark: 0xD6E9FF)

    /// The filter's response, drawn as the gate: a shaded mask under a thin
    /// dashed line, once it has started to close.
    private func gate(
        _ ctx: inout GraphicsContext, w: CGFloat, baseY: CGFloat, amp: CGFloat, bw: Double, e: Double
    ) {
        let g = min(1, e * 3)
        guard g > 0.01 else { return }
        func top(_ u: Double) -> CGFloat {
            baseY - CGFloat(0.02 + 0.84 * Self.response(u, Self.center, bw)) * amp
        }
        var edge = Path()
        for i in 0...200 {
            let u = Double(i) / 200
            let pt = CGPoint(x: CGFloat(u) * w, y: top(u))
            if i == 0 { edge.move(to: pt) } else { edge.addLine(to: pt) }
        }
        var fill = edge
        fill.addLine(to: CGPoint(x: w, y: baseY))
        fill.addLine(to: CGPoint(x: 0, y: baseY))
        fill.closeSubpath()
        ctx.fill(
            fill,
            with: .linearGradient(
                Gradient(colors: [Palette.accent.opacity(0.08 * g), Palette.accent.opacity(0)]),
                startPoint: CGPoint(x: 0, y: baseY - amp), endPoint: CGPoint(x: 0, y: baseY)))
        ctx.stroke(
            edge, with: .color(Palette.accent.opacity(0.45 * g)),
            style: StrokeStyle(lineWidth: 1, dash: [4, 5]))
    }
}
