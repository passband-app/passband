// The cold open, felt. The first beat of the intro is a radio with the squelch
// open, all noise; the swipe closes the squelch and only the passband comes
// through. The screen draws that (Views/Squelch), and the hand gets the same
// two beats: static for as long as the first page is up, then a steady sine
// wave for as long as the second is. The static opens with a burst: the radio
// switched on at full gain, falling away to the hiss it settles into.
//
// BOTH LOOP, seamlessly. The noise is random with no envelope, so where it
// repeats cannot be felt; the sine is a whole number of cycles that starts and
// ends in a trough, so the seam lands in the quiet. The system stops the engine
// when the app backgrounds, so the intro replays its beat on return (`resume`).
//
// Core Haptics rather than UIFeedbackGenerator, because neither beat is an
// impact: the static is dozens of jittered transients over a low rumble, and
// the sine is a continuous event whose intensity follows a curve. A device
// without the hardware, or with system haptics off, simply plays nothing.

import CoreHaptics
import Foundation

@MainActor
final class IntroHaptics {
    static let shared = IntroHaptics()

    private var engine: CHHapticEngine?
    private var player: CHHapticAdvancedPatternPlayer?
    /// The opening burst, while it plays; the loop is already scheduled to
    /// take over the instant it ends.
    private var burstPlayer: CHHapticPatternPlayer?
    private var current: Beat?

    private enum Beat { case noise, sine }

    private init() {}

    /// The open squelch: static, until told otherwise.
    func noise() { play(.noise) }

    /// The intro's first moment: a burst of energy that decays into exactly
    /// the static `noise()` loops, which is scheduled to start as it ends.
    func switchOn() {
        guard let burst = Self.burstPattern(), let engine = readyEngine() else { return }
        stop()
        do {
            let opening = try engine.makePlayer(with: burst)
            try opening.start(atTime: CHHapticTimeImmediate)
            burstPlayer = opening
            play(.noise, at: engine.currentTime + Self.burstDuration, keepBurst: true)
        } catch {
            play(.noise)
        }
    }

    /// The squelch closed: a steady sine, until told otherwise.
    func sine() { play(.sine) }

    /// Back from the background, where the system stopped the engine and the
    /// player with it: pick the beat up again.
    func resume() {
        if let current { play(current) }
    }

    func stop() {
        try? burstPlayer?.stop(atTime: CHHapticTimeImmediate)
        burstPlayer = nil
        try? player?.stop(atTime: CHHapticTimeImmediate)
        player = nil
        current = nil
    }

    private func play(_ beat: Beat, at time: TimeInterval = CHHapticTimeImmediate, keepBurst: Bool = false) {
        let pattern = beat == .noise ? Self.noisePattern() : Self.sinePattern()
        guard let pattern, let engine = readyEngine() else { return }
        if keepBurst {
            try? player?.stop(atTime: CHHapticTimeImmediate)
        } else {
            stop()
        }
        current = beat
        do {
            let next = try engine.makeAdvancedPlayer(with: pattern)
            next.loopEnabled = true
            try next.start(atTime: time)
            player = next
        } catch {
            // A haptic that fails to play is a haptic nobody misses.
        }
    }

    private func readyEngine() -> CHHapticEngine? {
        guard CHHapticEngine.capabilitiesForHardware().supportsHaptics else { return nil }
        if engine == nil {
            guard let made = try? CHHapticEngine() else { return nil }
            // The system stops the engine when the app backgrounds; `resume`
            // (and every new beat) starts it again.
            made.playsHapticsOnly = true
            // No auto-shutdown: a looping player keeps the engine busy anyway,
            // and a shutdown between beats would cost the next one a cold start.
            // A reset (the haptic server restarting) invalidates every player,
            // the looping one included, so the beat is started over.
            made.resetHandler = {
                Task { @MainActor in IntroHaptics.shared.resume() }
            }
            engine = made
        }
        try? engine?.start()
        return engine
    }

    // MARK: - patterns

    /// Long enough that the loop's repeat is not a rhythm anybody picks out.
    static let noiseDuration: TimeInterval = 4
    /// The sine's rate, and a loop of exactly `sineCycles` of it.
    static let sineHz = 1.6
    static let sineCycles = 12.0

    /// Static's range: the loop's transient strengths, its spacing, and its
    /// rumble. The burst decays onto exactly these, so the handover is felt as
    /// the energy settling rather than one pattern swapping for another.
    private static let noiseIntensity: ClosedRange<Float> = 0.12...0.5
    private static let noiseGap: ClosedRange<Double> = 0.022...0.075
    private static let noiseRumble: Float = 0.12

    static let burstDuration: TimeInterval = 1.4

    /// Switching on: dense, hard hits over a heavy rumble, both falling away
    /// on an exponential to the static's own levels by the end.
    private static func burstPattern() -> CHHapticPattern? {
        var rng = SystemRandomNumberGenerator()
        var events: [CHHapticEvent] = []
        let d = burstDuration
        /// 1 at the start, ~0 by the end.
        func energy(_ t: TimeInterval) -> Double { exp(-t * 3.2) * (1 - t / d) }
        var t = 0.0
        while t < d {
            let e = energy(t)
            let base = Float.random(in: noiseIntensity, using: &rng)
            events.append(
                CHHapticEvent(
                    eventType: .hapticTransient,
                    parameters: [
                        .init(parameterID: .hapticIntensity, value: min(1, base + Float(e) * 0.8)),
                        .init(parameterID: .hapticSharpness, value: .random(in: 0.5...1.0, using: &rng)),
                    ],
                    relativeTime: t))
            // Nearly a roll at the top, opening out to the static's spacing.
            let gap = Double.random(in: noiseGap, using: &rng)
            t += gap + (0.01 - gap) * e
        }
        events.append(
            CHHapticEvent(
                eventType: .hapticContinuous,
                parameters: [
                    .init(parameterID: .hapticIntensity, value: 1),
                    .init(parameterID: .hapticSharpness, value: 0.5),
                ],
                relativeTime: 0, duration: d))
        let rumble = CHHapticParameterCurve(
            parameterID: .hapticIntensityControl,
            controlPoints: stride(from: 0.0, through: d, by: d / 15).map {
                .init(relativeTime: $0, value: noiseRumble + (1 - noiseRumble) * Float(energy($0)))
            },
            relativeTime: 0)
        return try? CHHapticPattern(events: events, parameterCurves: [rumble])
    }

    /// Static: jittered transients over a faint rumble, flat, so it loops
    /// without a seam.
    private static func noisePattern() -> CHHapticPattern? {
        var rng = SystemRandomNumberGenerator()
        var events: [CHHapticEvent] = []
        let d = noiseDuration
        var t = 0.0
        while t < d {
            events.append(
                CHHapticEvent(
                    eventType: .hapticTransient,
                    parameters: [
                        .init(parameterID: .hapticIntensity, value: .random(in: noiseIntensity, using: &rng)),
                        .init(parameterID: .hapticSharpness, value: .random(in: 0.35...1.0, using: &rng)),
                    ],
                    relativeTime: t))
            t += .random(in: noiseGap, using: &rng)
        }
        events.append(
            CHHapticEvent(
                eventType: .hapticContinuous,
                parameters: [
                    .init(parameterID: .hapticIntensity, value: noiseRumble),
                    .init(parameterID: .hapticSharpness, value: 0.6),
                ],
                relativeTime: 0, duration: d))
        return try? CHHapticPattern(events: events, parameters: [])
    }

    /// One continuous, soft event whose intensity traces a sine, trough to
    /// trough, so the loop joins where the wave is quietest.
    private static func sinePattern() -> CHHapticPattern? {
        let hz = sineHz
        let d = sineCycles / hz
        let event = CHHapticEvent(
            eventType: .hapticContinuous,
            parameters: [
                .init(parameterID: .hapticIntensity, value: 1),
                .init(parameterID: .hapticSharpness, value: 0.18),
            ],
            relativeTime: 0, duration: d)
        // A curve holds at most 16 control points, so the wave is sampled into
        // consecutive curves, each timed from its own start.
        let step = 1.0 / (hz * 8)
        let samples = stride(from: 0.0, through: d + step / 2, by: step).map { t -> (TimeInterval, Float) in
            let wave = 0.5 + 0.5 * sin(2 * .pi * hz * t - .pi / 2)
            return (t, Float(0.1 + 0.8 * wave))
        }
        var curves: [CHHapticParameterCurve] = []
        var i = 0
        while i < samples.count - 1 {
            let chunk = samples[i..<min(i + 16, samples.count)]
            let start = chunk.first!.0
            curves.append(
                CHHapticParameterCurve(
                    parameterID: .hapticIntensityControl,
                    controlPoints: chunk.map { .init(relativeTime: $0.0 - start, value: $0.1) },
                    relativeTime: start))
            i += 15  // overlap one point so the chunks join without a step
        }
        return try? CHHapticPattern(events: [event], parameterCurves: curves)
    }
}
