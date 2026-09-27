// The half of Zoom (App/Zoom.swift) that Prefs, Glass and Chrome need on every
// platform: the ladder and the snap onto it. The phone never zooms, so there
// Prefs.zoom just sits at 1; the AppKit verbs that move it stay in App/.

import CoreGraphics

enum Zoom {
    /// Safari's ladder, trimmed. Fixed steps rather than a free multiplier so
    /// ⌘0 and a few presses always land back on the same numbers.
    static let steps: [Double] = [0.75, 0.85, 1.0, 1.1, 1.25, 1.5, 1.75]

    /// The layout's own floor (see PassbandApp's `minWidth`/`minHeight`), in
    /// logical points. The window's floor is this times the zoom.
    static let minLogicalSize = CGSize(width: 980, height: 640)

    /// Snap a stored value onto the ladder, so a hand-edited default or a
    /// retired step can never leave the shell at some off-grid scale.
    static func clamp(_ value: Double) -> Double {
        steps.min(by: { abs($0 - value) < abs($1 - value) }) ?? 1
    }
}
