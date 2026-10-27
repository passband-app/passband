// The spacing vocabulary: one ladder every padding, stack gap and inset is
// taken from, resolved through the current density (Lib/Density.swift).
//
// A step is NAMED FOR ITS COMPACT VALUE: `Space.s8` is 8pt in compact and 11pt
// in comfortable. Compact is the layout the app was drawn in, so the name is
// the number a reader already knows, and the density curve is the only place
// that says what it becomes.
//
// WHAT GOES THROUGH HERE AND WHAT DOES NOT.
//   - Padding, stack spacing, insets, gaps between things: a ladder step.
//   - A value that is TUNED and must not move in compact (an odd 5pt row
//     inset, a control's fixed height, anything aligned to something else):
//     `Space.scaled(5)`, which is exact in compact and follows the same curve.
//   - Corner radii come from `Radius` (below): a ROLE per kind of container,
//     not a number per view. They are NOT density-scaled; a card's corner is
//     the same curve in compact and comfortable. A shape nested inside
//     another takes `Radius.concentric(outer, inset:)` with the padding that
//     actually separates them, so the two curves stay parallel.
//   - Optical constants stay literal: hairlines, border widths, dot
//     diameters, shadow offsets, anything in the brand renderers (Vignette,
//     Phosphor, the Squelch drawings), and any radius derived from a size
//     (height / 2, size * 0.24). They describe the ink, not the room around it.
//
// Every accessor reads `Prefs.shared.density`, so a view that uses one in its
// body is observing the setting and re-lays itself out when it flips. The
// corollary: NEVER cache one in a `static let` or a stored property, where it
// would freeze at whichever density was live when it was first read.

import SwiftUI

@MainActor
enum Space {
    static var s2: CGFloat { scaled(2) }
    static var s4: CGFloat { scaled(4) }
    static var s6: CGFloat { scaled(6) }
    static var s8: CGFloat { scaled(8) }
    static var s10: CGFloat { scaled(10) }
    static var s12: CGFloat { scaled(12) }
    static var s14: CGFloat { scaled(14) }
    static var s16: CGFloat { scaled(16) }
    static var s20: CGFloat { scaled(20) }
    static var s24: CGFloat { scaled(24) }
    static var s32: CGFloat { scaled(32) }

    /// THE PAGE GUTTER: the leading and trailing edge every page's title, its
    /// content and its key-hint bar share, so switching tabs does not move the
    /// text. A list whose selection fill bleeds past its words gets there in
    /// two parts, container inset + row padding = `page`, which puts the WORDS
    /// on this line and lets the fill reach out to the left of it.
    static var page: CGFloat { s24 }

    /// An off-ladder compact value on the same curve. For the tuned numbers
    /// the ladder would shift by a point; not a way around choosing a step.
    static func scaled(_ compact: CGFloat) -> CGFloat {
        Density.space(compact, in: Prefs.shared.density)
    }

    /// The density in force, for the rare layout that changes SHAPE rather
    /// than size between the two (a second line that only comfortable has
    /// room for). Sizes should come from the ladder, not from a branch on this.
    static var density: DensityChoice { Prefs.shared.density }
}

/// THE CORNER VOCABULARY: one radius per ROLE. Pick by what the shape IS, not
/// by what looks right next to its neighbour, so two cards that do the same
/// job round the same way on every page.
///
/// Constants on purpose: radii describe the ink, and do not follow density.
enum Radius {
    /// A glass pane: zone cards, section cards, the reader's chrome cards.
    static let pane: CGFloat = 18
    /// Something floating over the page: modals, palettes, the ask bar,
    /// side panels, What's New and the ad card.
    static let modal: CGFloat = 20
    /// A corner toast (they stack, so they must match) and floating banners.
    static let toast: CGFloat = 14
    /// An inset well INSIDE a card: fields, option buttons, tiles, code
    /// blocks, inline banners, an attachment chip with a thumbnail.
    static let well: CGFloat = 10
    /// A list row's selection and hover fill.
    static let row: CGFloat = 8
    /// Small things: non-capsule pills, keycaps, tooltips, tiny tiles.
    static let chip: CGFloat = 6

    /// The radius for a shape inset `inset` points inside one rounded at
    /// `outer`, so the two curves run parallel. Pass the padding as actually
    /// laid out (density-scaled), so it stays concentric in comfortable too.
    static func concentric(_ outer: CGFloat, inset: CGFloat) -> CGFloat {
        max(outer - inset, 2)
    }
}
