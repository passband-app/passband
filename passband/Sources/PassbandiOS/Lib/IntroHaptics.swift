// The cold open, felt. The first beat of the intro is a radio with the squelch
// open, all noise; the swipe closes the squelch and only the passband comes
// through. The screen draws that (Views/Squelch), and the hand gets the same
// two beats: static on the first page, then one clean sine wave that settles
// out over about as long as the picture takes to.
//
// THE NOISE IS A BURST, NOT A LOOP. It plays as the scene fades in (and again
// on a swipe back to it), then stops: a phone that buzzed for as long as
// somebody took to read a sentence would be a phone they put down.
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
    private var player: CHHapticPatternPlayer?

    private init() {}

    /// The open squelch: ~2.5s of static, fading in and out.
    func noise() { play(Self.noisePattern()) }

    /// The squelch closing: a sine wave that starts full and settles.
    func sine() { play(Self.sinePattern()) }

    func stop() {
        try? player?.stop(atTime: CHHapticTimeImmediate)
        player = nil
    }

    private func play(_ pattern: CHHapticPattern?) {
        guard let pattern, let engine = readyEngine() else { return }
        stop()
        do {
            let next = try engine.makePlayer(with: pattern)
            try next.start(atTime: CHHapticTimeImmediate)
            player = next
        } catch {
            // A haptic that fails to play is a haptic nobody misses.
        }
    }

    private func readyEngine() -> CHHapticEngine? {
        guard CHHapticEngine.capabilitiesForHardware().supportsHaptics else { return nil }
        if engine == nil {
            guard let made = try? CHHapticEngine() else { return nil }
            // The system stops the engine when the app backgrounds or audio is
            // interrupted; the next beat restarts it, so nothing is held open.
            made.playsHapticsOnly = true
            made.isAutoShutdownEnabled = true
            made.resetHandler = { [weak made] in try? made?.start() }
            engine = made
        }
        try? engine?.start()
        return engine
    }

    // MARK: - patterns

    static let noiseDuration: TimeInterval = 2.5
    static let sineDuration: TimeInterval = 1.8

    /// Static: jittered transients over a faint rumble, under an envelope so it
    /// swells in and dies away rather than switching on and off.
    private static func noisePattern() -> CHHapticPattern? {
        var rng = SystemRandomNumberGenerator()
        var events: [CHHapticEvent] = []
        let d = noiseDuration
        func envelope(_ t: TimeInterval) -> Float {
            let rise = min(1, t / 0.5), fall = min(1, (d - t) / 0.8)
            return Float(max(0, min(rise, fall)))
        }
        var t = 0.0
        while t < d {
            let e = envelope(t)
            events.append(
                CHHapticEvent(
                    eventType: .hapticTransient,
                    parameters: [
                        .init(parameterID: .hapticIntensity, value: e * .random(in: 0.12...0.55, using: &rng)),
                        .init(parameterID: .hapticSharpness, value: .random(in: 0.35...1.0, using: &rng)),
                    ],
                    relativeTime: t))
            t += .random(in: 0.022...0.075, using: &rng)
        }
        events.append(
            CHHapticEvent(
                eventType: .hapticContinuous,
                parameters: [
                    .init(parameterID: .hapticIntensity, value: 0.14),
                    .init(parameterID: .hapticSharpness, value: 0.6),
                ],
                relativeTime: 0, duration: d))
        // The rumble follows the same envelope.
        let rumble = CHHapticParameterCurve(
            parameterID: .hapticIntensityControl,
            controlPoints: stride(from: 0.0, through: d, by: d / 8).map {
                .init(relativeTime: $0, value: envelope($0))
            },
            relativeTime: 0)
        return try? CHHapticPattern(events: events, parameterCurves: [rumble])
    }

    /// One continuous, soft event whose intensity traces a sine that decays
    /// toward stillness: the passband, and then quiet.
    private static func sinePattern() -> CHHapticPattern? {
        let d = sineDuration
        let hz = 2.6
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
        let samples = stride(from: 0.0, through: d, by: step).map { t -> (TimeInterval, Float) in
            let decay = exp(-t * 1.4)
            let wave = 0.5 + 0.5 * sin(2 * .pi * hz * t - .pi / 2)
            return (t, Float(0.08 + 0.92 * wave * decay))
        }
        var curves: [CHHapticParameterCurve] = []
        var i = 0
        while i < samples.count {
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
