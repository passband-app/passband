// DENSITY: how much room the interface gives each thing, as one setting.
//
// WHY THIS IS NOT ZOOM. Zoom (App/Zoom.swift) scales the whole window like a
// browser page: type, padding and hairlines all grow by the same factor, so a
// crowded layout at 125% is the same crowded layout, larger. Density changes
// the RATIO instead. Comfortable opens the gaps by roughly a third while the
// type grows by about a point, and it lifts the smallest type the most, because
// 8pt metadata packed 4pt apart is what "too dense" actually means. The two
// compose: density picks the proportions, zoom scales the result.
//
// EVERY NUMBER IS WRITTEN IN COMPACT. Compact is the layout the app was drawn
// in, so a call site names the compact value and the curve below answers what
// that becomes in comfortable. In compact both curves are the identity, which
// is what makes migrating a literal onto a token safe: nothing moves until the
// setting does.
//
// Pure (CoreGraphics only) so test.sh can hold the curves up without the app.
// The tokens views actually read are Design/Space.swift and Design/Typography.

import CoreGraphics

/// The setting itself. Compact is the default (see `Prefs.init`), and comes
/// first wherever the two are listed.
enum DensityChoice: String, CaseIterable, Sendable {
    case compact, comfortable

    var label: String {
        switch self {
        case .comfortable: "Comfortable"
        case .compact: "Compact"
        }
    }
}

enum Density {
    /// A point on a curve: the compact value and what it becomes in comfortable.
    typealias Anchor = (compact: CGFloat, comfortable: CGFloat)

    /// SPACING. Small gaps open proportionally more than large ones: the 4pt
    /// between a row's two lines is where crowding is felt, while a 32pt page
    /// margin was never the problem. The even values are `Space`'s ladder.
    static let spaceAnchors: [Anchor] = [
        (1, 1), (2, 3), (4, 6), (6, 9), (8, 11), (10, 14), (12, 16), (14, 18),
        (16, 20), (20, 25), (24, 30), (32, 38),
    ]

    /// TYPE. About a point across the reading sizes, a point and a half at the
    /// bottom where legibility was being spent, two at the headings so the
    /// hierarchy keeps its steps.
    static let typeAnchors: [Anchor] = [
        (6, 7.5), (8, 9.5), (10, 11), (16, 17), (18, 20), (24, 26), (40, 42),
    ]

    /// A compact spacing value in the given density.
    static func space(_ compact: CGFloat, in density: DensityChoice) -> CGFloat {
        resolve(compact, on: spaceAnchors, in: density)
    }

    /// A compact font size in the given density.
    static func typeSize(_ compact: CGFloat, in density: DensityChoice) -> CGFloat {
        resolve(compact, on: typeAnchors, in: density)
    }

    /// The size the interface's reading text is set at, in compact. `measure`
    /// grows by whatever this one grows by.
    static let readingSize: CGFloat = 13

    /// A compact WIDTH THAT HOLDS TEXT (a name column, a sheet, a field) in
    /// the given density. It grows by the ratio the reading type grows by, and
    /// no faster: the same words have to fit, and the spacing curve's third
    /// would hand a column room its text never asked for, taken from the
    /// flexible column beside it.
    static func measure(_ compact: CGFloat, in density: DensityChoice) -> CGFloat {
        guard density == .comfortable else { return compact }
        return snap(compact * typeSize(readingSize, in: density) / readingSize)
    }

    /// Piecewise-linear through the anchors, snapped to the half point. Past
    /// either end, that end's ratio carries on (which leaves a 0.5pt hairline
    /// alone: the spacing curve starts at 1 to 1).
    /// Negative values (an offset pulling something back) mirror the curve.
    private static func resolve(
        _ compact: CGFloat, on anchors: [Anchor], in density: DensityChoice
    ) -> CGFloat {
        guard density == .comfortable else { return compact }
        if compact < 0 { return -resolve(-compact, on: anchors, in: density) }
        guard let first = anchors.first, let last = anchors.last else { return compact }
        // Under the first anchor its ratio carries on downward, the way the
        // last one's carries on upward. For spacing that ratio is 1 (the first
        // anchor is a 1pt rule), so a hairline is returned exactly as it came
        // and never snapped; for type it keeps the curve from falling off a
        // cliff just below its smallest anchor.
        if compact < first.compact {
            let ratio = first.comfortable / first.compact
            return ratio == 1 ? compact : snap(compact * ratio)
        }
        if compact >= last.compact {
            return snap(compact * last.comfortable / last.compact)
        }
        for (lo, hi) in zip(anchors, anchors.dropFirst()) where compact <= hi.compact {
            let t = (compact - lo.compact) / (hi.compact - lo.compact)
            return snap(lo.comfortable + t * (hi.comfortable - lo.comfortable))
        }
        return compact
    }

    private static func snap(_ value: CGFloat) -> CGFloat {
        (value * 2).rounded() / 2
    }
}
