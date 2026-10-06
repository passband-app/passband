// THE DASHBOARD'S SWIPE RAIL. The mail tab gets SwiftUI's own `swipeActions`,
// because it is a real `List` and native full-swipe is better than anything
// hand-rolled. The sitrep cannot: its rows live inside `ZoneCard`s inside the
// page's ScrollView, and a `List` nested in a ScrollView is two scrollers
// fighting. So the same rail is drawn by hand here, to the same measurements and
// the same tints, and both surfaces read as one gesture.
//
// WHAT MAKES IT COEXIST WITH THE SCROLL: UIKIT DECIDES, NOT SWIFTUI. The pan is
// a `UIPanGestureRecognizer` hung on the row through
// `UIGestureRecognizerRepresentable`, and its delegate answers one question
// before the gesture is allowed to start — is the finger moving sideways faster
// than it is moving up? If it is not, `gestureRecognizerShouldBegin` says no,
// the recognizer never leaves `.possible`, and the enclosing ScrollView's own
// pan carries the touch as though this row had no gesture on it at all.
//
// Two SwiftUI shapes of this failed before it. A plain `.gesture` claimed every
// drag past its minimum, so the finger dragged a dead card while the sitrep
// refused to scroll. A `.simultaneousGesture` judging direction on its first
// delta still had to be HANDED the drag before it could refuse it, and a
// refusal the ScrollView never hears about is not a refusal: the page stayed
// stuck. UIKit arbitration happens before anyone owns the touch, which is why
// native list swipes and scrolling have never had this fight.
//
// Once the pan does begin, the row owns it outright. Nothing here allows
// simultaneous recognition, so the page under a swiping row stays exactly where
// it was, the way a UITableView row action behaves.
//
// The rails are painted UNDER the row and revealed by moving the row, not built
// as a stack that grows: one clip, one offset, and the row's own glass never
// changes identity.
//
// THE READER'S MESSAGE CARDS WEAR IT TOO, and three things about a card are not
// true of a sitrep row, each answered by a parameter that is off by default.
// A card can be three screens tall, so its rail's labels cannot sit at the
// card's own middle: they sit where the thumb came down (`followsTouch`). A
// card lives in a pushed screen, where a rightward drag is the way back, so an
// edge with no verbs must not start a pan at all (`yieldsEmptyEdges`). And a
// card sits on the reader's ground rather than on a zone card's (`ground`).
// The fourth is about looks. A sitrep row is a line tall, and a rail of solid
// colour behind it is a band; behind three screens of mail the same rail is a
// wall. So a card's verbs are discs on the ground the card slid off
// (`floating`), which is the same vocabulary at a size that fits.

import SwiftUI
import UIKit

struct SwipeRow<Content: View>: View {
    var leading: [SwipeVerb] = []
    var trailing: [SwipeVerb] = []
    var cornerRadius: CGFloat = 10
    /// Width of one rail button. Wide enough for a 15pt glyph over a caption.
    var buttonWidth: CGFloat = 74
    /// What the row slides OVER its rail on; nil is the sitrep's zone-card
    /// ground. See the opaque fill under `content`.
    var ground: Color? = nil
    /// Hang the rail's labels at the height the pan began at rather than at
    /// the row's middle. For a row that can be taller than the screen.
    var followsTouch = false
    /// Draw the verbs as tinted glass discs with a caption under each, on the
    /// bare ground, instead of as full-height slabs of the verb's colour.
    var floating = false
    /// Refuse a pan toward an edge that has no verbs (and no open rail to
    /// close), instead of rubber-banding it. The refusal happens before the
    /// touch is owned, so whatever else wants that direction (the navigation
    /// stack's swipe back) gets it untouched.
    var yieldsEmptyEdges = false
    #if DEBUG
        /// Mount with the trailing rail already out. There is no thumb in a
        /// simulator; see FixtureStage.
        var stagedOpen = false
    #endif
    @ViewBuilder var content: Content

    /// How far past the rail a drag must go before letting go COMMITS the
    /// edge's primary verb instead of just resting the rail open.
    private static var commitOverrun: CGFloat { 56 }

    /// Where the row is resting between gestures (0, or a rail's width).
    @State private var resting: CGFloat = 0
    /// Live drag on top of `resting`. The recognizer's translation is measured
    /// from where the pan started, not from where the row sits, so the two stay
    /// separate and `offset` adds them.
    @State private var drag: CGFloat = 0
    /// Edge-detects the commit threshold so the "you are past it" tap fires once.
    @State private var wasArmed = false
    /// Where the pan that pulled this rail out began, in the row's own space,
    /// and how tall the row is. Only read when `followsTouch`.
    @State private var touchY: CGFloat?
    @State private var height: CGFloat = 0

    private var offset: CGFloat { resting + drag }
    private var open: Bool { resting != 0 }

    private func railWidth(_ verbs: [SwipeVerb]) -> CGFloat {
        CGFloat(verbs.count) * buttonWidth
    }
    private var leadingWidth: CGFloat { railWidth(leading) }
    private var trailingWidth: CGFloat { railWidth(trailing) }

    /// The verb a full swipe commits on whichever edge is showing: the
    /// destructive one if the edge has it, else the first.
    private func primary(_ verbs: [SwipeVerb]) -> SwipeVerb? {
        verbs.first { $0.destructive } ?? verbs.first
    }

    private var armed: Bool {
        if offset < 0, !trailing.isEmpty { return -offset > trailingWidth + Self.commitOverrun }
        if offset > 0, !leading.isEmpty { return offset > leadingWidth + Self.commitOverrun }
        return false
    }

    var body: some View {
        ZStack {
            // Only the rail being pulled paints, so a row at rest is exactly the
            // row and nothing is drawn behind every one of them.
            if offset > 0 {
                rail(leading, alignment: .leading, revealed: offset)
            } else if offset < 0 {
                rail(trailing, alignment: .trailing, revealed: -offset)
            }

            content
                // An OPAQUE ground under the row while it is off-centre, or the
                // rail shows straight through the row that is supposed to be
                // covering it. Canvas plus the zone wash rather than canvas
                // alone: these rows sit on a ZoneCard, so the page colour by
                // itself reads as a lighter panel sliding over a darker one.
                .background {
                    if open || drag != 0 {
                        if let ground {
                            ground
                        } else {
                            ZStack {
                                Palette.canvas
                                Palette.glassTint
                            }
                        }
                    }
                }
                .offset(x: offset)
                // While a rail is out, the row itself is a CLOSE target, not a
                // door: a thumb reaching back for the mail it just swiped must
                // not open it.
                .overlay {
                    if open {
                        Rectangle()
                            .fill(.clear)
                            .contentShape(Rectangle())
                            .onTapGesture { close() }
                            .offset(x: offset)
                    }
                }
        }
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
        .gesture(RailPan(handle: pan, mayBegin: mayBegin))
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
        #if DEBUG
            .onAppear { if stagedOpen, !trailing.isEmpty { resting = -trailingWidth } }
        #endif
        .onChange(of: armed) { _, nowArmed in
            // Felt, not watched: the rail is past its commit point.
            if nowArmed, !wasArmed { Haptics.armed() }
            wasArmed = nowArmed
        }
    }

    // MARK: - rail

    private func rail(_ verbs: [SwipeVerb], alignment: Alignment, revealed: CGFloat) -> some View {
        HStack(spacing: 0) {
            ForEach(verbs) { verb in
                Button {
                    close()
                    Haptics.commit()
                    verb.run()
                } label: {
                    if floating {
                        // THE DISC AND ITS CAPTION ARE THE BUTTON, and nothing
                        // else in the column is: under a card three screens
                        // tall, a column-high target would fire a reply from a
                        // tap on bare ground a screen away from the verb.
                        label(verb, primary: verb.id == primary(verbs)?.id)
                            .frame(width: buttonWidth, height: labelHeight)
                            .contentShape(Rectangle())
                            .frame(maxHeight: .infinity, alignment: followsTouch ? .top : .center)
                            .offset(y: followsTouch ? labelTop : 0)
                    } else {
                        label(verb, primary: false)
                            .frame(width: buttonWidth, height: labelHeight)
                            .frame(maxHeight: .infinity, alignment: followsTouch ? .top : .center)
                            .offset(y: followsTouch ? labelTop : 0)
                            .background(verb.tint)
                            .contentShape(Rectangle())
                    }
                }
                .buttonStyle(.plain)
            }
        }
        // Past the rail's own width the primary verb's colour stretches to fill
        // what the drag opened, which is what says "let go and this happens".
        // A floating rail has no slab to stretch; its primary disc swells
        // instead (see `label`).
        .frame(maxWidth: .infinity, alignment: alignment)
        .background(
            (primary(verbs)?.tint ?? Palette.hairline)
                .opacity(!floating && revealed > railWidth(verbs) ? 1 : 0)
        )
        .frame(maxWidth: .infinity, alignment: alignment)
    }

    /// One verb's face: a glyph over a caption in white on the slab, or a
    /// tinted disc with its caption in the ground's own ink.
    @ViewBuilder
    private func label(_ verb: SwipeVerb, primary: Bool) -> some View {
        if floating {
            VStack(spacing: Space.s4) {
                Image(systemName: verb.symbol)
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: Self.discSize, height: Self.discSize)
                    .glassEffect(.regular.tint(verb.tint).interactive(), in: .circle)
                    // Armed: this is the one a let-go commits.
                    .scaleEffect(primary && armed ? 1.14 : 1)
                    .animation(Motion.disclose, value: armed)
                Text(verb.title)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .fixedSize()
            }
        } else {
            VStack(spacing: 3) {
                Image(systemName: verb.symbol)
                    .font(.system(size: 15, weight: .semibold))
                Text(verb.title)
                    .font(Typo.micro)
            }
            .foregroundStyle(.white)
        }
    }

    private static var discSize: CGFloat { 44 }

    /// The glyph-over-caption block's own height: what `labelTop` centres on
    /// the thumb, and what keeps it inside a short row.
    private var labelHeight: CGFloat { floating ? Self.discSize + 22 : 44 }

    /// Where the labels hang when they follow the touch: centred on it, and
    /// never off either end of the row. A row shorter than the block centres
    /// it, which is what the fixed rail does anyway.
    private var labelTop: CGFloat {
        let room = height - labelHeight
        guard room > 0 else { return room / 2 }
        let inset = min(Space.s12, room / 2)
        let wanted = (touchY ?? height / 2) - labelHeight / 2
        return min(max(wanted, inset), room - inset)
    }

    // MARK: - gesture

    /// Asked by the recognizer's delegate, before the pan exists: may a drag
    /// in this direction start at all? Always yes unless `yieldsEmptyEdges`,
    /// and then only toward an edge that has verbs or away from a rail that is
    /// out and needs a way to be pushed shut.
    private func mayBegin(pullingLeft: Bool) -> Bool {
        guard yieldsEmptyEdges else { return true }
        return pullingLeft
            ? !trailing.isEmpty || resting > 0
            : !leading.isEmpty || resting < 0
    }

    /// The recognizer's whole state machine, in one place. There is no direction
    /// test here and no latch holding one: by the time this sees `.began` the
    /// delegate has already ruled the pan horizontal, and a vertical one is
    /// never reported at all.
    private func pan(_ recognizer: UIPanGestureRecognizer, at point: CGPoint) {
        let raw = recognizer.translation(in: recognizer.view).x
        switch recognizer.state {
        case .began:
            SwipeRowActivity.lastBegan = Date()
            // Only a pan that OPENS a rail says where its labels go: one that
            // starts on a rail already out is adjusting it, and labels that
            // jumped to the new finger would be buttons that dodge.
            if !open { touchY = point.y }
            drag = banded(raw)
        case .changed:
            drag = banded(raw)
        case .ended:
            release(raw)
        case .cancelled, .failed:
            // Something else took the touch, or the pan never resolved. A cancel
            // is not a decision, so the row goes back to where it was RESTING
            // rather than closing a rail the finger never let go of.
            settle(resting)
        default:
            break
        }
    }

    /// Rubber-band an edge with no verbs so the row never slides off into empty
    /// space, while still moving enough to say the swipe was heard.
    private func banded(_ raw: CGFloat) -> CGFloat {
        if raw > 0, leading.isEmpty { return raw * 0.12 }
        if raw < 0, trailing.isEmpty { return raw * 0.12 }
        return raw
    }

    /// Where the finger left the row decides between committing the edge's
    /// primary verb, resting the rail open, and closing. Measured off the raw
    /// translation, not the banded one, so an empty edge lands on the
    /// no-verbs path instead of a distance the band shrank.
    private func release(_ raw: CGFloat) {
        let landed = resting + raw
        let verbs = landed < 0 ? trailing : leading
        let width = railWidth(verbs)
        guard !verbs.isEmpty else {
            settle(0)
            return
        }
        let distance = abs(landed)
        if distance > width + Self.commitOverrun, let verb = primary(verbs) {
            Haptics.commit()
            settle(0)
            verb.run()
        } else if distance > width / 2 {
            settle(landed < 0 ? -width : width)
        } else {
            settle(0)
        }
    }

    /// Every way a gesture can end runs through here, which is also why the
    /// armed edge is re-cocked here and nowhere else.
    private func settle(_ to: CGFloat) {
        drag = 0
        wasArmed = false
        withAnimation(Motion.railSettle) { resting = to }
    }

    private func close() {
        settle(0)
    }
}

// MARK: - the recognizer

/// The row's pan, as UIKit sees it. Declared outside `SwipeRow` on purpose: a
/// class nested in a generic type is itself generic, and a generic NSObject
/// subclass cannot carry the `@objc` witnesses an ObjC delegate protocol needs.
@MainActor
private struct RailPan: UIGestureRecognizerRepresentable {
    /// Called for `.began`, `.changed`, `.ended`, `.cancelled` and `.failed`,
    /// with the touch in the row's own coordinate space.
    var handle: (UIPanGestureRecognizer, CGPoint) -> Void
    /// Whether a horizontal pan may begin: true = the finger is moving LEFT.
    var mayBegin: (Bool) -> Bool

    func makeCoordinator(converter: CoordinateSpaceConverter) -> Coordinator {
        Coordinator()
    }

    func makeUIGestureRecognizer(context: Context) -> UIPanGestureRecognizer {
        let pan = UIPanGestureRecognizer()
        // One thumb. A second finger on a row belongs to whatever the page is
        // doing, not to the rail.
        pan.minimumNumberOfTouches = 1
        pan.maximumNumberOfTouches = 1
        pan.delegate = context.coordinator
        context.coordinator.mayBegin = mayBegin
        return pan
    }

    func updateUIGestureRecognizer(_ recognizer: UIPanGestureRecognizer, context: Context) {
        // Re-seated on every update, not just at make. The recognizer is
        // SwiftUI's to keep and re-attach, the delegate reference is weak, and
        // the one thing this row cannot survive is losing the seat that holds
        // the direction verdict. A pointer write is cheap insurance.
        recognizer.delegate = context.coordinator
        // And the row's current answer about its edges, which changes as its
        // rail opens and shuts.
        context.coordinator.mayBegin = mayBegin
    }

    func handleUIGestureRecognizerAction(_ recognizer: UIPanGestureRecognizer, context: Context) {
        handle(recognizer, context.converter.localLocation)
    }

    @MainActor
    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        var mayBegin: (Bool) -> Bool = { _ in true }

        /// THE VERDICT, and the only one. Velocity at the moment UIKit is ready
        /// to start the pan says which way the finger is really going; a
        /// vertical lead means the page is scrolling and this row never enters
        /// the conversation, so the ScrollView keeps a touch it never had to
        /// win back.
        ///
        /// Deliberately no `shouldRecognizeSimultaneouslyWith`: the default
        /// refusal is what makes a begun swipe exclusive, so a horizontal pull
        /// cannot also drag the page along under it.
        func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            guard let pan = gestureRecognizer as? UIPanGestureRecognizer else { return true }
            // Velocity first, translation as the tiebreak. The refusal here is
            // final for the whole touch, and velocity is a smoothed instant
            // that can come back zero or dead-equal when the sampler has no
            // usable deltas — the accumulated translation always has a
            // direction by the time UIKit asks.
            let velocity = pan.velocity(in: pan.view)
            if velocity != .zero, abs(velocity.x) != abs(velocity.y) {
                return abs(velocity.x) > abs(velocity.y) && mayBegin(velocity.x < 0)
            }
            let translation = pan.translation(in: pan.view)
            return abs(translation.x) > abs(translation.y) && mayBegin(translation.x < 0)
        }
    }
}

/// When a row's rail last took a pan. A page that has a sideways gesture of
/// its own (the sitrep's swipe to the account page) reads this to know the
/// drag it is also hearing belonged to a row, and leaves it alone.
@MainActor
enum SwipeRowActivity {
    static var lastBegan = Date.distantPast
}
