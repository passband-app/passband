// The chrome vocabulary: the rule, the chip, the key-hint bar, the section card
// and the segmented control every surface is assembled from. A shape that shows
// up on more than one screen belongs here, where it can only drift once.
//
// Everything in this file is a LEAF — a rule, a pill, a label pair. None of it
// wraps a stateful subtree, so swapping a hand-rolled copy for one of these can
// never change the view identity of the row or card around it.

import SwiftUI

// MARK: - the top bar

/// The strip a page's own header occupies, measured from the TRUE top of the
/// window.
///
/// macOS keeps roughly the first 32pt of a window clear for the traffic lights
/// and SwiftUI insets the whole app below it, which used to leave every page
/// wearing a band of empty backdrop above its own title. The header goes up
/// there instead — level with the buttons, the way a browser puts its tab strip
/// — and the RAIL is what yields: it starts at this line rather than at the
/// window's edge, so nothing but the dots is ever in the strip.
///
/// ONE height for every page, because the rail's top edge is cut to it: a page
/// whose header ran taller or shorter would leave the rail's edge hanging in
/// mid-air beside that page's rule instead of continuing it.
enum TopBar {
    /// Deep enough for the wordmark line the sitrep sets in 19pt serif, and
    /// close enough to the buttons' own centre (y 16) that a title beside them
    /// reads as being ON their line rather than under it.
    static let height: CGFloat = 40

    /// How far in from the window's LEADING edge anything sharing the strip has
    /// to start. The traffic lights span x 9-69; this clears them with a
    /// gutter.
    ///
    /// Most surfaces never need it, because the rail is the only thing in the
    /// strip and it yields the whole span. A surface that goes FULL WINDOW WIDTH
    /// covers the rail as well, and then its own header starts at x 0 — under
    /// the buttons, which is where fullscreen search was drawing its title.
    ///
    /// The buttons are AppKit's and do not zoom, so the clearance is 78 WINDOW
    /// points, divided back into the zoomed layout's points.
    @MainActor static var dotsClearance: CGFloat { 78 / Prefs.shared.zoom }
}

// MARK: - waiting

/// A THREE-DOT WAIT, for the places a word would be too loud and a spinner too
/// official — inside a field, beside the thing being waited on.
///
/// It says only "working", which is all a search in flight can honestly claim.
/// The banner it replaced said "searching…" on its own line, which took a row
/// of the results away from the results and read as an ANSWER rather than as a
/// status, in the same slot the empty state uses.
///
/// The three dots rise and fall on one shared toggle, phase-shifted by their
/// index so the motion reads as travel rather than a blink. The loop starts in
/// `onAppear` and dies with the view, so nothing has to remember to stop it.
struct WaitDots: View {
    var tone: Color = Palette.inkFaintest

    private static let diameter: CGFloat = 3.5
    private static let spacing: CGFloat = 3

    /// The space three dots occupy. A caller that shows them CONDITIONALLY
    /// reserves this much so their arrival does not re-lay whatever they sit
    /// beside — see the search field, where it would shove the caret.
    static var width: CGFloat { diameter * 3 + spacing * 2 }

    /// One toggle for all three; the stagger is a per-dot animation delay
    /// rather than three pieces of state to keep in step.
    @State private var raised = false

    /// An infinite animation is the case this setting exists for, so this one
    /// asks even though it is the app's first to. Reduced, the dots simply sit
    /// there at the brightness they average out to: still a "working" mark,
    /// with nothing moving.
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private static let dim: Double = 0.28
    private static let beat: Double = 0.5

    var body: some View {
        HStack(spacing: Self.spacing) {
            ForEach(0..<3, id: \.self) { i in
                Circle()
                    .fill(tone)
                    .frame(width: Self.diameter, height: Self.diameter)
                    .opacity(reduceMotion ? 0.6 : (raised ? 1 : Self.dim))
                    .animation(
                        reduceMotion
                            ? nil
                            : .easeInOut(duration: Self.beat)
                                .repeatForever(autoreverses: true)
                                .delay(Double(i) * 0.16),
                        value: raised
                    )
            }
        }
        .onAppear { raised = true }
        .accessibilityElement()
        .accessibilityLabel("searching")
    }
}

// MARK: - the meter slot

/// THE NARROW LEADING SLOT of a mail row: the importance meter on an arrival,
/// the word "to" on a sent row, the clock on a scheduled one. Different things
/// in the same column, so one formula or the lists stop lining up with each
/// other.
///
/// It holds 10pt type and nothing else, so it grows by exactly what that type
/// grows by: wider than `Typo.measure` would make it, narrower than the
/// spacing curve would.
enum MeterSlot {
    @MainActor static func width(_ compact: CGFloat = 30) -> CGFloat {
        compact * Typo.size(10) / 10
    }
}

/// THE PHONE'S TWO-LINE MAIL ROW, measured once. The inbox's rows
/// (`UpdateRow`), the sent rows and the outbox rows above them are three
/// structs drawing one list shape: a 26pt mark, a name line with its stamp on
/// the right, and the words under it at the full width of the row. They read
/// these so flipping the segmented control between all mail and sent does not
/// change the rhythm of the page, and so the three cannot drift apart.
#if !os(macOS)
    @MainActor
    enum PhoneRow {
        static var hPadding: CGFloat { Space.s12 }
        static var vPadding: CGFloat { Space.scaled(9) }
        static let radius: CGFloat = Radius.well
        /// The screen's side margin, which a zone title, a day heading and a
        /// row's own words all line up on.
        static let gutter: CGFloat = 16
        /// What a row's list inset is when the row carries `hPadding` of its
        /// own: the press wash bleeds out into the margin, the words stay on
        /// the gutter. Rows were drawn a padding's width inside everything
        /// above them, which cost a narrow phone 24pt of every line.
        static var bleed: CGFloat { gutter - hPadding }
        /// The avatar, or whatever stands where one would.
        static let mark: CGFloat = 26
        /// Mark to text.
        static var gap: CGFloat { Space.s10 }
        /// Name line to the words under it.
        static var lineGap: CGFloat { Space.scaled(3) }
        static var name: Font { Typo.ui(14, weight: .medium) }
        static var stamp: Font { Typo.num(11) }
    }
#endif

/// THE MAC'S BLEEDING LIST ROW, the desktop twin of `PhoneRow`. A list whose
/// selection fill reaches past its words (the mail tab's rows, the rules and
/// groups pages, the audit ledger, the calendar agenda) lands those words on
/// the page gutter in two parts that have to add up: the list is inset
/// `listInset`, the row pads `hPadding`, and the fill reaches out into the
/// inset to the left of the text. Headings and notes inside such a list pad
/// by `hPadding` too, which is what puts them on the same line as the rows.
///
/// Not fenced to macOS: several of those rows compile into the phone target
/// (`MailRow` among them, never mounted there), so the numbers exist on both.
@MainActor
enum DeskRow {
    static var listInset: CGFloat { Space.page - hPadding }
    static var hPadding: CGFloat { Space.s12 }
    /// The mail tab's row height, which `MailRow`, `UpdateRow`, `SentRow` and
    /// `ScheduledRow` share so the page keeps its rhythm between modes.
    static var vPadding: CGFloat { Space.s6 }
}

/// THE RHYTHM OF A SECTION LABEL INSIDE A LIST: the mail tab's day headings
/// (`DayHeading`) and the Sent page's "scheduled" / "sent" groups
/// (`SentSectionLabel`) are the same engraved label over a run of rows, so
/// they take the same air: a little over the list's first one, a group's
/// worth over every later one, and the same short drop to the rows.
@MainActor
enum SectionLabelRhythm {
    static var firstTop: CGFloat { Space.s4 }
    static var top: CGFloat { Space.s16 }
    static var bottom: CGFloat { Space.scaled(5) }
    static func top(first: Bool) -> CGFloat { first ? firstTop : top }
}

/// ONE FILE CHIP, measured once: the reader's attachment cards
/// (`AttachmentStrip`), the composer's tray (`AttachmentTray`) and the
/// outbox preview's read-only chips (`ScheduledPreview`) are the same object
/// in three states, so they share the padding, the thumb and its corner,
/// which runs concentric with the chip's `Radius.well` across that padding
/// and never goes squarer than `Radius.chip` (`Radius.image`).
@MainActor
enum AttachmentChip {
    static var padding: CGFloat { Space.s6 }
    /// The tile. Holds an image or a glyph rather than text, but it sits
    /// beside two lines of type, so it follows the spacing curve.
    static var thumb: CGFloat { Space.scaled(38) }
    static var thumbRadius: CGFloat { Radius.image(in: Radius.well, inset: padding) }
    /// Tile to the words.
    static var gap: CGFloat { Space.scaled(9) }
    /// The document glyph standing in for a thumbnail.
    static var glyph: Font { Typo.ui(18, weight: .light) }
}

/// THE SIDE PANEL'S BLEEDING LIST ROWS, in two envelopes.
///
/// THE SEARCH PANEL'S TEXT EDGE is `hitInset + hitPadding` (panel + s16): the
/// search field's words (its capsule pads `hitPadding`), the recent searches,
/// the section headers, the sender block and the hits' avatars all start
/// there, and their selection fills sit on the panel gutter (`hitInset`), the
/// field capsule's own edge. One edge, so typing does not move the column.
///
/// THE BROWSE BOARD keeps `DeskRow`'s tighter arrangement: inset `listInset`,
/// padded `hPadding`, words on `Space.panel` with the fill reaching out past
/// them.
@MainActor
enum PanelRow {
    static var hPadding: CGFloat { Space.s10 }
    static var listInset: CGFloat { Space.panel - hPadding }
    /// One height for the rows on the hit grid (26pt avatar, then words):
    /// the hits and the senders offered above them.
    static var vPadding: CGFloat { Space.s10 }

    /// THE SEARCH HITS' OWN ENVELOPE, roomier than the rest: the selection
    /// fill sits ON the panel gutter (the search field's edge) and the words
    /// a full `hitPadding` inside it. The hits carry a relevance bar ahead of
    /// the avatar (`HitRow`), and on the tighter `listInset`/`hPadding` grid
    /// the bar, avatar and name ran into each other. Bar at gutter + s4,
    /// avatar at gutter + s16, name 36 past that.
    static var hitInset: CGFloat { Space.panel }
    static var hitPadding: CGFloat { Space.s16 }
}

// MARK: - rules

/// The 0.5pt rule dividing a header from what it heads. Ink-side (see
/// `Palette.hairline`) so it reads on a glass pane's own tint.
struct Hairline: View {
    var body: some View {
        Rectangle().fill(Palette.hairline).frame(height: 0.5)
    }
}

// MARK: - chips

/// Chip metrics. The hand-rolled copies these replaced drifted across 6/7/8/9
/// horizontal and 2.5/3/4 vertical; 7/3 was the most common of each.
///
/// Computed, not stored: a `static let` would freeze at whichever density was
/// live the first time a chip drew, and every chip after that would ignore the
/// setting.
@MainActor
private enum ChipMetrics {
    static var horizontal: CGFloat { Space.scaled(7) }
    static var vertical: CGFloat { Space.scaled(3) }
}

/// A glass chip action — the app's one small-button shape. Padding is fixed
/// here on purpose: a chip that sets its own padding is a chip that drifts.
///
/// `tone` is the chip's ink; a label that colours its own parts (the reading
/// rule chip) simply overrides it further down.
struct ChromeChip<Content: View>: View {
    private let tone: Color
    private let help: String?
    private let action: () -> Void
    private let label: Content

    init(
        tone: Color = Palette.inkFaint, help: String? = nil,
        action: @escaping () -> Void, @ViewBuilder label: () -> Content
    ) {
        self.tone = tone
        self.help = help
        self.action = action
        self.label = label()
    }

    var body: some View {
        Button(action: action) {
            label
                .padding(.horizontal, ChipMetrics.horizontal)
                .padding(.vertical, ChipMetrics.vertical)
        }
        .buttonStyle(.glass)
        .foregroundStyle(tone)
        .help(ifPresent: help)
    }
}

/// A chip's standard label: an SF Symbol, a short string, or both.
struct ChipLabel: View {
    var text: String?
    var icon: String?
    var font: Font = Typo.micro

    var body: some View {
        Group {
            if let text, let icon {
                Label(text, systemImage: icon)
            } else if let icon {
                Image(systemName: icon)
            } else if let text {
                Text(text)
            }
        }
        .font(font)
    }
}

extension ChromeChip where Content == ChipLabel {
    /// The common chip — icon, text, or both.
    init(
        text: String? = nil, icon: String? = nil, font: Font = Typo.micro,
        tone: Color = Palette.inkFaint, help: String? = nil,
        action: @escaping () -> Void
    ) {
        self.init(tone: tone, help: help, action: action) {
            ChipLabel(text: text, icon: icon, font: font)
        }
    }
}

// MARK: - key hints

/// One key hint: the key chip(s) and the verb they fire.
struct KeyHint: View {
    let keys: [String]
    let label: String

    init(_ key: String, _ label: String) {
        self.keys = [key]
        self.label = label
    }

    init(_ keys: [String], _ label: String) {
        self.keys = keys
        self.label = label
    }

    var body: some View {
        HStack(spacing: Space.s4) {
            ForEach(keys, id: \.self) { Kbd($0) }
            Text(label).font(Typo.micro).foregroundStyle(Palette.inkFaintest)
        }
    }
}

/// The bottom bar a list surface teaches its keymap with: dot-separated key
/// hints under a top hairline.
struct KeyHintBar: View {
    let hints: [KeyHint]
    /// A control ahead of the first hint (the inline reply's send clock, just
    /// left of the send verb it changes). nil = the bar as it always was.
    var leading: AnyView? = nil

    var body: some View {
        HStack(spacing: Space.s4) {
            if let leading {
                leading.padding(.trailing, Space.s4)
            }
            ForEach(Array(hints.enumerated()), id: \.offset) { i, hint in
                if i > 0 { Text("·").foregroundStyle(Palette.inkFaintest) }
                hint
            }
            Spacer()
        }
        // The page gutter RoutedHeader sets its title on, so the hints start
        // under the title.
        .padding(.horizontal, Space.page)
        .padding(.vertical, Space.s10)
        .overlay(alignment: .top) { Hairline() }
    }
}

// MARK: - status

/// A state dot with its label — "connected · saved", "3 live · 1 awaiting you".
struct StatusDot: View {
    let color: Color
    let label: String
    /// Label ink; defaults to the dot's own colour.
    var tone: Color?
    var size: CGFloat = 6

    var body: some View {
        HStack(spacing: Space.s4) {
            Circle().fill(color).frame(width: size, height: size)
            Text(label)
                .font(Typo.micro)
                .foregroundStyle(tone ?? color)
        }
    }
}

// MARK: - section card

/// One engraved section on a single glass pane. `note` is the right-aligned
/// annotation beside the label (a model name, a provider); with none, the
/// heading is exactly the bare label it was before the slot existed.
struct SectionCard<Content: View>: View {
    let label: String
    var note: String?
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s12) {
            HStack {
                Text(label)
                    .font(Typo.sectionLabel)
                    .foregroundStyle(Palette.accent)
                    .textCase(.uppercase)
                if let note {
                    Spacer()
                    Text(note)
                        .font(Typo.mono(10))
                        .foregroundStyle(Palette.inkFaintest)
                }
            }
            content
        }
        .zonePadding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.glassTint)
    }
}

// MARK: - segmented control

/// The control's coordinate space. FILE-SCOPE for the same reason as the
/// rail's: the buttons read it inside `onGeometryChange`'s Sendable closure.
/// One name serves every instance — a lookup resolves the nearest ancestor.
private let segmentedSpace = "glass-segmented"

/// A sliding segmented control, mechanics borrowed from the sidebar rail: the
/// options are bare text, and ONE long-lived pane behind them slides between
/// slots the buttons report as they lay out — moved by geometry, not identity,
/// which is what lets the travel animate.
struct GlassSegmented<T: Hashable>: View {
    let options: [(value: T, label: String)]
    @Binding var selection: T

    /// Where each option sits in the control's own space.
    @State private var slots: [T: CGRect] = [:]

    /// Same crossing time as the rail, so the two selectors read as one motion
    /// vocabulary.
    private static var travel: Animation { .smooth(duration: 0.32) }

    var body: some View {
        HStack(spacing: Space.s2) {
            ForEach(options, id: \.value) { option in
                Button {
                    selection = option.value
                } label: {
                    Text(option.label)
                        .font(Typo.chip)
                        // A segment is one word on one line. Squeezed, the
                        // control should claim its width from the row, never
                        // wrap a label ("Squelc / h" in the Sound picker).
                        .lineLimit(1)
                        .fixedSize()
                        .foregroundStyle(selection == option.value ? .white : Palette.ink)
                        .padding(.horizontal, Space.s12)
                        .padding(.vertical, Space.scaled(5))
                        .contentShape(Capsule())
                        .onGeometryChange(for: CGRect.self) {
                            $0.frame(in: .named(segmentedSpace))
                        } action: { slots[option.value] = $0 }
                }
                .buttonStyle(.plain)
            }
        }
        .coordinateSpace(.named(segmentedSpace))
        .background(alignment: .topLeading) { selector }
        .animation(Self.travel, value: selection)
        .background(Capsule().fill(Palette.hairline.opacity(0.5)))
    }

    /// The travelling pane: absent only until the first layout pass reports a
    /// slot, then one view for the life of the control.
    @ViewBuilder
    private var selector: some View {
        if let slot = slots[selection] {
            Color.clear
                .frame(width: slot.width, height: slot.height)
                .glassEffect(.regular.tint(Palette.accent.opacity(0.55)), in: Capsule())
                // Sits over the active button — an interactive material would
                // eat that option's clicks.
                .allowsHitTesting(false)
                .offset(x: slot.minX, y: slot.minY)
        }
    }
}

// MARK: - helpers

extension View {
    /// `.help` only when there is something to say — an empty tooltip string
    /// still registers a tooltip region.
    @ViewBuilder fileprivate func help(ifPresent text: String?) -> some View {
        if let text { help(text) } else { self }
    }
}


// MARK: - the pointing hand

#if os(macOS)
    /// Push/pop the pointing-hand cursor while the pointer is inside.
    ///
    /// Both halves are guarded and the disappear case is real, not defensive:
    /// these sit on controls that VANISH under the cursor by design — the
    /// cc/bcc toggles fold a field open and the layout moves, a recipient pill
    /// vanishes when it is dragged to another field — and a view removed while hovered
    /// never gets its exit callback. An unbalanced push leaves the pointing
    /// hand stuck over the whole window.
    private struct PointingHandCursor: ViewModifier {
        @State private var pushed = false

        func body(content: Content) -> some View {
            content
                .onHover { inside in
                    if inside, !pushed {
                        NSCursor.pointingHand.push()
                        pushed = true
                    } else if !inside, pushed {
                        NSCursor.pop()
                        pushed = false
                    }
                }
                .onDisappear {
                    if pushed {
                        NSCursor.pop()
                        pushed = false
                    }
                }
        }
    }
#endif

extension View {
    /// THE POINTER SAYS IT IS A CONTROL. For the things that are clickable but
    /// do not look like buttons — a text toggle, a pill, a header line that
    /// unfolds — where the shape alone reads as a label and nothing else on
    /// screen says otherwise. A control nobody knows is a control is not one.
    ///
    /// Inert off the Mac: a finger has no hover state to say it with.
    @ViewBuilder
    func pointingHand() -> some View {
        #if os(macOS)
            modifier(PointingHandCursor())
        #else
            self
        #endif
    }
}

// MARK: - toasts

/// THE TOAST VOCABULARY: one inset, one corner, one material and one shadow for
/// everything that floats in a corner of the window (the toast and undo stack,
/// the unsubscribe tray, the violation notice, the smart clear receipt, the
/// update card). They stack beside each other, so a card that padded or lifted
/// itself differently read as a different kind of thing in the same pile.
///
/// Computed, not stored: the insets follow the density setting.
@MainActor
enum ToastMetrics {
    static var hPadding: CGFloat { Space.s12 }
    static var vPadding: CGFloat { Space.s10 }
    /// How far a corner toast, the unsubscribe tray and the update card float
    /// in from the window's bottom (and right) edge, so the floating things
    /// pinned to an edge sit off it by the same air.
    static var edge: CGFloat { Space.scaled(18) }
    // The corner is `Radius.toast`, read directly.
}

extension View {
    /// A corner toast's inset and glass. `interactive` for a toast that is
    /// itself the button (the toast and undo pills). Padding is applied here,
    /// so call it on the bare content.
    func toastSurface(tint: Color? = Palette.glassTintStrong, interactive: Bool = false) -> some View {
        self
            .padding(.horizontal, ToastMetrics.hPadding)
            .padding(.vertical, ToastMetrics.vPadding)
            .toastGlass(tint: tint, interactive: interactive)
    }

    /// The glass and shadow alone, for a toast whose padding has to sit inside
    /// a button label or before a frame (the toast stack's pills, the update
    /// card's max width). Pair it with `ToastMetrics`' insets.
    func toastGlass(tint: Color? = Palette.glassTintStrong, interactive: Bool = false) -> some View {
        self
            .passbandGlass(
                .pane, cornerRadius: Radius.toast, tint: tint, interactive: interactive)
            .shadow(color: .black.opacity(0.22), radius: 18, y: 8)
    }
}

// MARK: - hero pill

/// The pill set on a phosphor hero, naming what is new: What's New's strip and
/// the ad cards wear the same one. White on a dark capsule with the brand's
/// blue edge, so it survives both the scene's night and its paper.
///
/// `fixed` for a fixed composition (the ad cards), where nothing follows the
/// density; otherwise the type and padding scale with it. The caller places
/// it (its inset from the hero's corner).
struct HeroPill: View {
    let text: String
    var fixed = false

    var body: some View {
        Text(text)
            .font(Typo.ui(9, weight: .bold, fixed: fixed).width(.expanded))
            .tracking(0.6)
            .foregroundStyle(.white.opacity(0.92))
            .padding(.horizontal, fixed ? 9 : Space.scaled(9))
            .padding(.vertical, fixed ? 5 : Space.scaled(5))
            .background(.black.opacity(0.32), in: Capsule())
            .overlay(Capsule().strokeBorder(Color(hex: 0x82BAF5).opacity(0.45), lineWidth: 0.5))
    }
}
