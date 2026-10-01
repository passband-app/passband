# Density: comfortable and compact

Passband has two densities, switched in Settings → General → Appearance, where
each option is drawn as a small preview of the rows it produces. **Compact** is
the default: it is the layout the app was originally drawn in, and it is
pixel-for-pixel what shipped before the setting existed wherever a value went
through `Space.scaled` or `Typo`. **Comfortable** gives everything more room.

The phone is pinned to compact and shows no switch until the comfortable curve
has been looked at on a real one.

## Why it is not zoom

Zoom (`App/Zoom.swift`, ⌘+ / ⌘-) scales the whole window by one factor. A
crowded layout at 125% is the same crowded layout, larger. Density changes the
**ratio**: comfortable opens gaps by about a third while type grows by about a
point, and it lifts the smallest type the most. The two compose.

## The three files

| File | What it is |
|---|---|
| `Lib/Density.swift` | The setting (`DensityChoice`) and the two curves. Pure, tested by `Tests/DensityTests.swift`. |
| `Design/Space.swift` | The spacing ladder, `Space.s2` … `Space.s32`, and `Space.scaled(_:)`. |
| `Design/Typography.swift` | `Typo.ui(_:weight:design:)`, the named roles, `Typo.size(_:)`, `Typo.measure(_:)`. |

**Every number is written in compact.** `Space.s8` is 8pt in compact and 11pt in
comfortable; `Typo.ui(13)` is 13pt and 14pt. In compact both curves are the
identity.

| compact | 2 | 4 | 6 | 8 | 10 | 12 | 14 | 16 | 20 | 24 | 32 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| comfortable space | 3 | 6 | 9 | 11 | 14 | 16 | 18 | 20 | 25 | 30 | 38 |

| compact | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 18 | 20 | 24 | 40 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| comfortable type | 9.5 | 10.5 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 20 | 22 | 26 | 42 |

## Writing a view

**Type.** Never `.font(.system(size: 13, weight: .semibold))`. Write
`.font(Typo.ui(13, weight: .semibold))`, or a named role when one fits
(`Typo.row`, `.rowSub`, `.zoneTitle`, `.chip`, `.micro`, `.sectionLabel`,
`.mono`, `.num`, `.serif`, `.hero`). SF Symbols set with `.font` go through
`Typo.ui` too, so a glyph keeps pace with the text beside it. AppKit and CSS
that need the number take `Typo.size(13)`.

**Spacing.** Padding, stack `spacing:`, insets and gaps come from the ladder:
`.padding(.horizontal, Space.s16)`, `VStack(spacing: Space.s8)`.

**Off-ladder values.** The old code is full of 5, 7, 9, 11 and 13. Two choices:

- Snap to the neighbouring step when a point either way is harmless in compact
  (the gap in a free-flowing stack, a card's inset). This is the preferred
  outcome: it is how the drift goes away.
- `Space.scaled(5)` when the value is tuned: a row's or chip's vertical padding,
  a control's height, anything aligned to something else, anything with a
  comment explaining the number. Exact in compact, same curve in comfortable.

**Widths that hold text.** A name column, a sheet, a field's width: the same
words have to fit, so it grows by what the type grows by (about 8%) and no
faster. `.frame(width: Typo.measure(160))`. `Space.scaled(160)` would give it
the gap curve's 19%, taken from the flexible column beside it. `ModalCard`
resolves its own `width:` this way, so callers pass the compact number.

**Component parameters.** A padding or spacing a component takes as an argument
(`ListRow(hPadding:vPadding:)`, `FlowLine(spacing:)`) is passed ALREADY
RESOLVED, as a token, the way `.padding` takes one. The component never scales
what it is given.

**Fixed frames.** A `.frame(height: 26)` around text will clip in comfortable.
Either drop the fixed height and let padding size it, or
`.frame(height: Space.scaled(26))`. Frames that are not about text (a panel's
width, an image box, a window minimum) are a judgement call: scale the ones that
hold type-bearing content, leave the rest.

**What stays literal.** Hairlines and border widths, corner radii, dot and
indicator diameters, shadow radii and offsets, opacity, animation distances,
`TopBar.height` (cut to the traffic lights), and everything inside the brand
renderers (`Views/Phosphor`, `Views/Squelch`) and choreographed scenes. They
describe the ink, not the room around it.

**Type inside a composition.** A wordmark locked up beside a mark of a set
size, or a line drawn inside a picture (the TestFlight card's phone), sits in a
box that does not follow the density, so its type must not either:
`Typo.serif(24, fixed: true)`, `Typo.hero(13, fixed: true)`, or a plain
`.system(size:)`. A `Typo` role in a fixed box is a bug that only shows in
comfortable.

## Traps

- **Never cache a token.** `static let rowHeight = Space.s8 * 3` freezes at
  whichever density was live when it was first read. Use a computed
  `static var`. Same for a token captured in `@State`'s initial value.
- **Tokens are `@MainActor`.** View bodies already are. A nonisolated helper
  that needs one takes it as a parameter or becomes `@MainActor`.
- **AppKit and web content do not observe.** A SwiftUI body that reads a token
  re-runs when the setting flips; an `NSView`, an `NSAttributedString` built
  once, or CSS injected into the email web view does not. Those need the value
  passed in through `updateNSView` (or re-injected) so they follow the switch
  without a relaunch.
- **Measured heights.** Anything that measures or estimates a height from a
  hardcoded line height (`FrameMeasurer`, the minimap, a scroll-to offset) has
  to derive it from `Typo.size` / `Space` as well.
- **Do not branch on density for sizes.** `Space.density` exists for a layout
  that changes shape (a second line only comfortable has room for), not for
  `density == .compact ? 5 : 8`. If the curve gives the wrong answer for a
  role, that is a new named token, not an inline ternary.
