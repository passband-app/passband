// THE PURGE, ON SCREEN: a deck through the Reading zone one SENDER at a time
// (Lib/Purge.swift is the deck, AppStore the half that acts, this is what is
// seen and pressed).
//
// THE EVIDENCE IS THE HERO. Each card leads with what the person actually did
// with this sender's mail, in sentences and in small drawings of the same
// numbers, and the four choices sit under it. Nothing on the card is a count
// of what is left: the only numbers are evidence, and a tally of what has
// been decided, which only grows.
//
// EACH CHOICE LEAVES ITS OWN WAY, because at speed the exit is the only
// confirmation anybody reads: skip turns aside, clear lifts away under a
// check, mute greys and sinks, and unsubscribe is cut in two and dropped.
// Stepping back plays the same motion in reverse.
//
// A SOLID CARD, NOT GLASS, on purpose. The exits mask, desaturate, rotate and
// fade the card, and a glass effect does none of those gracefully: it cannot
// be cut by a mask and it pops rather than fades. The card that is decided on
// and the ghost that leaves are the same drawing, so nothing changes material
// at the moment of the press.
//
// NOTHING IS SENT FROM HERE. Decisions are staged in the store's session and
// the confirm card (below) is the one press that applies them.

#if os(macOS)
    import SwiftUI

    // MARK: - the overlay

    /// Everything a purge session draws: the deck while deciding, then the
    /// confirm card, which stays up through the apply.
    struct PurgeOverlay: View {
        let session: AppStore.PurgeSession

        var body: some View {
            ZStack {
                // A CLICK ON THE DIM DOES NOTHING. Every other overlay closes
                // on it, and this one holds decisions somebody spent a minute
                // making: a stray click must not be a way to lose them.
                Color.black.opacity(0.2)
                    .ignoresSafeArea()
                    .contentShape(Rectangle())
                    .onTapGesture {}
                switch session.phase {
                case .deciding:
                    PurgeDeckView(session: session)
                        .transition(.opacity.combined(with: .scale(scale: 0.97)))
                case .confirming, .applying:
                    PurgeConfirmCard(session: session)
                        .transition(.opacity.combined(with: .scale(scale: 0.95)))
                }
            }
            .animation(.smooth(duration: 0.28), value: session.phase == .deciding)
            .keyContext(.modal)
            .transition(.opacity)
        }
    }

    /// The surface the deck's cards are drawn on (see the file header for why
    /// it is not glass). Lit evenly all the way round.
    private struct PurgeSurface: ViewModifier {
        var radius: CGFloat = 22
        func body(content: Content) -> some View {
            content
                .background(
                    RoundedRectangle(cornerRadius: radius, style: .continuous)
                        .fill(purgeSurface)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: radius, style: .continuous)
                        .strokeBorder(Palette.hairlineStrong, lineWidth: 1)
                )
        }
    }

    private enum PurgeMetrics {
        @MainActor static var width: CGFloat { Typo.measure(456) }
        /// The room the card sits in. Fixed, so a short card and a tall one
        /// do not move the tally above or the hints below.
        @MainActor static var stage: CGFloat { Space.scaled(452) }
    }

    /// The deck's own paper: what its cards, and the chips above them, are
    /// drawn on. The same in both so the chips read as cut from the card.
    private let purgeSurface = Color(light: 0xF7FAFD, dark: 0x121A25)

    /// The text that sits on the accent fill of a primary action.
    private let onAccent = Color(light: 0xFFFFFF, dark: 0x06111D)

    // MARK: - the deck

    /// How a card leaves.
    private enum PurgeExit: Equatable {
        case decided(PurgeDecision)
        /// Back under the deck: the card on screen when `z` brings the
        /// previous one back over it.
        case under
    }

    /// How a card arrives.
    private enum PurgeArrive: Equatable {
        /// The deck opening.
        case first
        /// The next card, which was underneath all along.
        case under
        /// After a skip: in from the right as the skipped one turns aside.
        case right
        /// `z`: the exit of the decision being taken back, played backwards.
        case reverse(PurgeDecision)
    }

    private struct PurgeDeckView: View {
        @Environment(AppStore.self) private var store
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        let session: AppStore.PurgeSession

        private struct Ghost: Identifiable {
            let id = UUID()
            let card: PurgeCard
            let exit: PurgeExit
        }
        @State private var ghosts: [Ghost] = []
        @State private var arrive: PurgeArrive = .first

        private var deck: PurgeDeck { session.deck }
        private var anythingStaged: Bool { !session.summary.isEmpty }

        var body: some View {
            VStack(spacing: Space.s14) {
                PurgeTally(summary: session.summary)
                ZStack {
                    if let card = deck.current {
                        PurgeCardFace(
                            card: card, ledgerDays: session.ledgerDays,
                            summaries: store.purgeSummaries(for: card.id), decide: decide
                        )
                        .modifier(PurgeArrival(style: arrive, reduceMotion: reduceMotion))
                        .id(card.id)
                        .transition(.identity)
                    }
                    ForEach(ghosts) { ghost in
                        PurgeGhost(
                            card: ghost.card, ledgerDays: session.ledgerDays,
                            summaries: store.purgeSummaries(for: ghost.card.id),
                            exit: ghost.exit, reduceMotion: reduceMotion
                        )
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                    }
                }
                .frame(width: PurgeMetrics.width, height: PurgeMetrics.stage)
                footer
            }
            .keyBindings(.modal, bindings)
        }

        /// Where the deck is, as a line with no numbers on it, and the two
        /// ways out.
        private var footer: some View {
            VStack(spacing: Space.s10) {
                GeometryReader { geo in
                    let p = deck.progress
                    let done = CGFloat(max(0, p.position - 1)) / CGFloat(max(1, p.total))
                    ZStack(alignment: .leading) {
                        Capsule().fill(Palette.hairlineStrong)
                        Capsule().fill(Palette.accent.opacity(0.8))
                            .frame(width: max(2, geo.size.width * done))
                    }
                }
                .frame(height: 2)
                .animation(.smooth(duration: 0.3), value: deck.progress)
                .accessibilityHidden(true)
                HStack(spacing: Space.s6) {
                    hint("z", "back")
                        .opacity(deck.progress.position > 1 ? 1 : 0.35)
                    Spacer(minLength: Space.s8)
                    hint("esc", anythingStaged ? "review and confirm" : "leave")
                }
            }
            .frame(width: PurgeMetrics.width)
        }

        private func hint(_ key: String, _ label: String) -> some View {
            HStack(spacing: Space.s6) {
                Kbd(key)
                Text(label)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
            }
        }

        // MARK: keys

        /// HOLDS, NOT TAPS, for the four decisions: a hold swallows its own
        /// autorepeat, so a key leaned on decides one card and not a blur of
        /// them. Each also answers when its choice is not on offer (and does
        /// nothing), so `u` here can never fall through to the global undo.
        private var bindings: [KeyBinding] {
            [
                KeyBinding("u", "unsubscribe", onRelease: { _ in }) { decide(.unsubscribe) },
                KeyBinding("m", "mute", onRelease: { _ in }) { decide(.mute) },
                KeyBinding("e", "clear", onRelease: { _ in }) { decide(.clear) },
                KeyBinding("s", "skip", onRelease: { _ in }) { decide(.skip) },
                KeyBinding("z", "back one card", onRelease: { _ in }) { back() },
                KeyBinding("Backspace", "back one card", onRelease: { _ in }) { back() },
                KeyBinding("Delete", "back one card", onRelease: { _ in }) { back() },
                KeyBinding("Escape", "leave or review") { leave() },
                // Return decides nothing here: the confirm step owns it, and
                // a habit of pressing it must not pick the suggested choice.
                KeyBinding("Enter", "nothing") {},
            ]
        }

        private func decide(_ decision: PurgeDecision) {
            guard let card = deck.current, card.allowed.contains(decision) else { return }
            arrive = decision == .skip ? .right : .under
            push(Ghost(card: card, exit: .decided(decision)))
            guard store.purgeDecide(decision) else { return }
            if decision != .skip {
                NSHapticFeedbackManager.defaultPerformer.perform(.alignment, performanceTime: .now)
            }
            AccessibilityNotification.Announcement(
                PurgeCopy.staged(decision, name: PurgeCardFace.name(card))
            ).post()
        }

        private func back() {
            let index = deck.progress.position - 1
            guard deck.current != nil, index > 0 else { return }
            let previous = deck.cards[index - 1]
            arrive = .reverse(deck.decision(for: previous.id) ?? .skip)
            if let current = deck.current { push(Ghost(card: current, exit: .under)) }
            store.purgeBack()
            AccessibilityNotification.Announcement("Back to \(PurgeCardFace.name(previous)).").post()
        }

        /// Esc never throws decisions away: with something staged it goes to
        /// the confirm step, which is where discarding is asked about.
        private func leave() {
            if anythingStaged {
                store.purgeFinishEarly()
            } else {
                store.cancelPurge()
            }
        }

        private func push(_ ghost: Ghost) {
            ghosts.append(ghost)
            let id = ghost.id
            Task {
                try? await Task.sleep(for: .milliseconds(760))
                ghosts.removeAll { $0.id == id }
            }
        }
    }

    // MARK: - the tally

    /// What has been decided so far. It only grows (or steps back with `z`),
    /// and it says STAGED in so many words: these are plans, and the line
    /// beside them is the promise that nothing has left yet.
    private struct PurgeTally: View {
        let summary: PurgeSummary

        var body: some View {
            HStack(spacing: Space.s8) {
                let ending = summary.unsubscribed + summary.onTheirSite
                if ending > 0 { TallyChip(count: ending, label: "to unsubscribe") }
                if summary.muted > 0 { TallyChip(count: summary.muted, label: "to mute") }
                if summary.cleared > 0 { TallyChip(count: summary.cleared, label: "to clear") }
                Text(summary.isEmpty ? "Nothing is sent until you confirm at the end." : "nothing sent yet")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                    .padding(.leading, summary.isEmpty ? 0 : Space.s4)
            }
            .frame(height: Space.scaled(26))
            .animation(.spring(response: 0.34, dampingFraction: 0.8), value: summary)
            .accessibilityElement(children: .combine)
        }
    }

    private struct TallyChip: View {
        let count: Int
        let label: String
        @State private var glow = false

        var body: some View {
            HStack(spacing: Space.scaled(5)) {
                Text("\(count)")
                    .font(Typo.serif(15, weight: .semibold))
                    .monospacedDigit()
                    .contentTransition(.numericText(value: Double(count)))
                Text(label)
                    .font(Typo.micro)
                    .opacity(0.8)
            }
            .foregroundStyle(Palette.ink)
            .padding(.horizontal, Space.s10)
            .padding(.vertical, Space.scaled(3))
            .background(Capsule(style: .continuous).fill(purgeSurface))
            .background(Capsule(style: .continuous).fill(Palette.accent.opacity(glow ? 0.5 : 0)))
            // The whole border, evenly, brightening for a moment when the
            // number goes up.
            .overlay(
                Capsule(style: .continuous)
                    .strokeBorder(
                        glow ? Palette.accent : Palette.hairlineStrong, lineWidth: 1)
            )
            .shadow(color: Palette.accent.opacity(glow ? 0.8 : 0), radius: 10)
            .transition(.scale(scale: 0.8).combined(with: .opacity))
            .onAppear { flash() }
            .onChange(of: count) { old, new in if new > old { flash() } }
        }

        private func flash() {
            glow = true
            withAnimation(.easeOut(duration: 0.7)) { glow = false }
        }
    }

    // MARK: - a card

    /// One sender, drawn. Used for the card being decided on (with `decide`)
    /// and for the ghost of one leaving (without).
    private struct PurgeCardFace: View {
        let card: PurgeCard
        let ledgerDays: Int
        let summaries: [String]
        var decide: ((PurgeDecision) -> Void)?

        static func name(_ card: PurgeCard) -> String {
            let given = card.sender.display_name.trimmingCharacters(in: .whitespaces)
            return given.isEmpty ? SenderCache.resolved(card.sender.sender).displayName : given
        }

        private enum Mood { case evidence, keep, unknown }

        private var mood: Mood {
            if case .readClosely = card.sender.reasons.first { return .keep }
            let known = card.sender.reasons.contains { $0 != .notEnoughHistory }
            return known ? .evidence : .unknown
        }

        /// At most four: the daemon sends them strongest first, and a fifth
        /// line is a card somebody has to read rather than see.
        private var reasons: [PurgeReason] { Array(card.sender.reasons.prefix(4)) }

        var body: some View {
            VStack(alignment: .leading, spacing: Space.s14) {
                who
                why
                if !summaries.isEmpty { lately }
                choices
            }
            .padding(Space.scaled(18))
            .frame(width: PurgeMetrics.width, alignment: .leading)
            .modifier(PurgeSurface())
            .shadow(color: .black.opacity(0.3), radius: 34, y: 16)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(Self.name(card))
        }

        // MARK: who

        private var who: some View {
            HStack(alignment: .center, spacing: Space.s12) {
                Avatar(sender: "\(Self.name(card)) <\(card.sender.sender)>", size: Space.scaled(40))
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(Self.name(card))
                        .font(Typo.serif(22, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Text(card.sender.sender)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .help(Self.name(card))
                switch mood {
                case .keep: verdict("worth keeping", Palette.positive, Palette.positiveSoft)
                case .unknown: verdict("too new to say", Palette.inkFaint, Palette.hairline)
                case .evidence: EmptyView()
                }
            }
            .accessibilityElement(children: .combine)
        }

        private func verdict(_ text: String, _ ink: Color, _ fill: Color) -> some View {
            Text(text)
                .font(Typo.micro)
                .foregroundStyle(ink)
                .padding(.horizontal, Space.s8)
                .padding(.vertical, Space.scaled(3))
                .background(Capsule(style: .continuous).fill(fill))
                .fixedSize()
        }

        // MARK: why

        private var why: some View {
            VStack(alignment: .leading, spacing: Space.s10) {
                if mood != .unknown {
                    HStack(spacing: Space.s6) {
                        Text("Your reading")
                            .font(Typo.ui(9, weight: .bold).width(.expanded))
                            .tracking(0.5)
                            .textCase(.uppercase)
                            .foregroundStyle(mood == .keep ? Palette.positive : Palette.accent)
                        Spacer(minLength: Space.s8)
                        // Once, quietly: what span every number here is about.
                        Text(PurgeCopy.window(ledgerDays: ledgerDays))
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                    }
                }
                if mood == .unknown {
                    VStack(alignment: .leading, spacing: Space.s4) {
                        Text(PurgeCopy.sentence(.notEnoughHistory))
                            .font(Typo.serif(18))
                            .foregroundStyle(Palette.inkDim)
                        Text("There is too little to go on, so nothing is suggested.")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                    }
                } else {
                    ForEach(Array(reasons.enumerated()), id: \.offset) { i, reason in
                        PurgeReasonRow(reason: reason, lead: i == 0, keep: mood == .keep)
                    }
                }
            }
            .padding(Space.s12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 13, style: .continuous)
                    .fill(
                        mood == .keep
                            ? Palette.positiveSoft
                            : (mood == .unknown ? Color.clear : Palette.accentSoft.opacity(0.6)))
            )
            .overlay {
                // "We do not know yet" is drawn as an open question: a
                // dashed outline round nothing, where the others are filled.
                if mood == .unknown {
                    RoundedRectangle(cornerRadius: 13, style: .continuous)
                        .strokeBorder(
                            Palette.hairlineStrong,
                            style: StrokeStyle(lineWidth: 1, dash: [4, 4]))
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(spokenWhy)
        }

        private var spokenWhy: String {
            let lines = reasons.map(PurgeCopy.sentence)
            guard mood != .unknown else { return lines.joined(separator: ". ") }
            return (lines + [PurgeCopy.window(ledgerDays: ledgerDays)]).joined(separator: ". ")
        }

        // MARK: what it is

        private var lately: some View {
            VStack(alignment: .leading, spacing: Space.s4) {
                Text("Lately")
                    .font(Typo.ui(9, weight: .bold).width(.expanded))
                    .tracking(0.5)
                    .textCase(.uppercase)
                    .foregroundStyle(Palette.inkFaint)
                ForEach(summaries, id: \.self) { line in
                    Text(line)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding(.horizontal, Space.s2)
            .accessibilityElement(children: .combine)
        }

        // MARK: the choices

        private static let keys: [(PurgeDecision, String)] = [
            (.unsubscribe, "u"), (.mute, "m"), (.clear, "e"), (.skip, "s"),
        ]

        private var choices: some View {
            HStack(alignment: .top, spacing: Space.s8) {
                ForEach(Self.keys, id: \.0) { decision, key in
                    PurgeChoice(
                        decision: decision, key: key,
                        caption: PurgeCopy.caption(decision, on: card),
                        allowed: card.allowed.contains(decision),
                        primary: card.recommended == decision,
                        action: decide.map { run in { run(decision) } })
                }
            }
        }
    }

    /// One of the four choices: a keycap, the verb, and a few words on what
    /// it does (or why it is not on offer).
    private struct PurgeChoice: View {
        let decision: PurgeDecision
        let key: String
        let caption: String
        let allowed: Bool
        let primary: Bool
        let action: (() -> Void)?

        @State private var hovering = false

        var body: some View {
            Button {
                action?()
            } label: {
                VStack(spacing: Space.s4) {
                    HStack(spacing: Space.scaled(5)) {
                        PurgeKey(key, style: primary ? .onAccent : .plain)
                        Text(PurgeCopy.title(decision))
                            .font(Typo.ui(11.5, weight: .semibold))
                            .foregroundStyle(primary ? onAccent : Palette.ink)
                            .lineLimit(1)
                            .minimumScaleFactor(0.85)
                    }
                    Text(caption)
                        .font(Typo.ui(9.5, weight: .medium))
                        .foregroundStyle(primary ? onAccent.opacity(0.82) : Palette.inkFaint)
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                }
                .padding(.horizontal, Space.s4)
                // A set height and an equal share of the row: the four are
                // one control, and the suggested one differs by colour alone.
                .frame(maxWidth: .infinity)
                .frame(height: Space.scaled(50))
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(
                            primary
                                ? Palette.accent.opacity(hovering ? 0.88 : 1)
                                : Palette.hairline.opacity(hovering && allowed ? 1 : 0.55))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(primary ? Color.clear : Palette.hairline, lineWidth: 1)
                )
                .contentShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
            }
            .buttonStyle(.plain)
            .disabled(!allowed)
            .opacity(allowed ? 1 : 0.42)
            .onHover { hovering = $0 }
            .accessibilityLabel(
                "\(PurgeCopy.title(decision)), \(caption)"
                    + (allowed ? ", key \(key.uppercased())" : "")
                    + (primary ? ", suggested" : ""))
        }
    }

    /// A keycap that can sit on the card or on an accent fill. `Kbd` is
    /// drawn for the first of those only.
    private struct PurgeKey: View {
        enum Style { case plain, onAccent }
        let label: String
        let style: Style

        init(_ label: String, style: Style = .plain) {
            self.label = label
            self.style = style
        }

        var body: some View {
            switch style {
            case .plain:
                Kbd(label)
            case .onAccent:
                let ink = onAccent
                Text(label)
                    .font(Typo.mono(10, weight: .medium))
                    .foregroundStyle(ink.opacity(0.92))
                    .padding(.horizontal, 5 * Typo.size(10) / 10)
                    .padding(.vertical, 1.5 * Typo.size(10) / 10)
                    .background(
                        RoundedRectangle(cornerRadius: 4, style: .continuous)
                            .fill(ink.opacity(0.16))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 4, style: .continuous)
                            .strokeBorder(ink.opacity(0.35), lineWidth: 0.5)
                    )
                    .fixedSize()
            }
        }
    }

    // MARK: - the evidence, drawn

    /// One reason: its sentence, and where the numbers have a shape, a small
    /// honest drawing of them. The first reason is the strongest and is set
    /// as the card's lead line.
    private struct PurgeReasonRow: View {
        let reason: PurgeReason
        let lead: Bool
        let keep: Bool

        private var tint: Color { keep ? Palette.positive : Palette.accent }

        var body: some View {
            VStack(alignment: .leading, spacing: Space.scaled(5)) {
                Text(PurgeCopy.sentence(reason))
                    .font(lead ? Typo.serif(19, weight: .medium) : Typo.rowSub)
                    .foregroundStyle(lead ? Palette.ink : Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
                drawing
                    .accessibilityHidden(true)
            }
        }

        @ViewBuilder private var drawing: some View {
            switch reason {
            case .rarelyOpened(let opened, let received):
                // One mark an email, the opened ones lit.
                let marks = PurgeMarks.split(lit: opened, of: received)
                HStack(spacing: Space.scaled(3)) {
                    ForEach(0..<marks.total, id: \.self) { i in
                        RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                            .fill(i < marks.lit ? tint : Palette.hairlineStrong)
                            .frame(width: Space.scaled(5), height: Space.scaled(lead ? 13 : 9))
                    }
                }
            case .skimmed(let fraction, _, _):
                readBar(fraction)
            case .readClosely(let fraction):
                readBar(fraction)
            case .highVolume(let perMonth):
                // A month of days, with about that many carrying their mail.
                let days = PurgeMarks.days(perMonth: perMonth)
                HStack(spacing: Space.scaled(3)) {
                    ForEach(0..<days.total, id: \.self) { i in
                        Circle()
                            .fill(i < days.lit ? tint.opacity(0.85) : Palette.hairlineStrong)
                            .frame(width: Space.scaled(5), height: Space.scaled(5))
                    }
                }
            default:
                EmptyView()
            }
        }

        /// How far down each email the reading got: the whole bar is the
        /// email's length, the lit part is what was read.
        private func readBar(_ fraction: Double) -> some View {
            let f = CGFloat(min(1, max(0, fraction)))
            return HStack(spacing: Space.s8) {
                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(Palette.hairlineStrong)
                        Capsule().fill(tint)
                            .frame(width: max(geo.size.height, geo.size.width * f))
                    }
                }
                .frame(height: Space.scaled(5))
                Text("the whole email")
                    .font(Typo.ui(9, weight: .medium))
                    .foregroundStyle(Palette.inkFaintest)
                    .fixedSize()
            }
        }
    }

    // MARK: - motion

    /// How far a card travels sideways and down when it goes.
    private enum Travel {
        static let aside: CGFloat = 360
        static let lift: CGFloat = 150
        static let sink: CGFloat = 110
        static let drop: CGFloat = 250
    }

    /// The card arriving. Reduce Motion: a fade and nothing else.
    private struct PurgeArrival: ViewModifier {
        let style: PurgeArrive
        let reduceMotion: Bool
        @State private var a: CGFloat = 0

        func body(content: Content) -> some View {
            let away = 1 - a
            Group {
                if reduceMotion {
                    content.opacity(a)
                } else {
                    switch style {
                    case .first:
                        content.scaleEffect(0.92 + 0.08 * a).opacity(a)
                    case .under:
                        content.scaleEffect(0.94 + 0.06 * a).offset(y: 12 * away).opacity(a)
                    case .right:
                        content.offset(x: Travel.aside * 0.8 * away).opacity(a)
                    case .reverse(.skip):
                        content.offset(x: -Travel.aside * away).scaleEffect(0.95 + 0.05 * a)
                            .opacity(a)
                    case .reverse(.clear):
                        content.offset(y: -Travel.lift * away).opacity(a)
                    case .reverse(.mute):
                        content.offset(y: Travel.sink * away).scaleEffect(0.9 + 0.1 * a)
                            .saturation(a).opacity(a)
                    case .reverse(.unsubscribe):
                        content.rotationEffect(.degrees(8 * away)).offset(y: Travel.drop * away)
                            .opacity(a)
                    }
                }
            }
            .onAppear {
                let animation: Animation
                if reduceMotion {
                    animation = .easeOut(duration: 0.2)
                } else if case .under = style {
                    // A beat behind the card leaving, so the two are read
                    // as one going and one coming and not as a swap.
                    animation = Motion.deckCard.delay(0.09)
                } else {
                    animation = Motion.deckCard
                }
                withAnimation(animation) { a = 1 }
            }
        }
    }

    /// A card on its way out. Not interactive, not read aloud.
    private struct PurgeGhost: View {
        let card: PurgeCard
        let ledgerDays: Int
        let summaries: [String]
        let exit: PurgeExit
        let reduceMotion: Bool

        /// The exit itself, 0 to 1.
        @State private var t: CGFloat = 0
        /// What plays first: the cut across an unsubscribe, the check on a
        /// clear, the mark on a mute.
        @State private var mark: CGFloat = 0

        private var face: some View {
            PurgeCardFace(card: card, ledgerDays: ledgerDays, summaries: summaries)
        }

        var body: some View {
            Group {
                if reduceMotion {
                    face.opacity(1 - t)
                } else {
                    switch exit {
                    case .under:
                        face.scaleEffect(1 - 0.06 * t).offset(y: 12 * t).opacity(1 - t)
                    case .decided(.skip):
                        // THE PAGE TURN, as the reader's S does it: aside to
                        // the left, receding and dimming as it goes.
                        face.scaleEffect(1 - 0.05 * t)
                            .offset(x: -Travel.aside * t)
                            .opacity(1 - t)
                    case .decided(.clear):
                        // Stamped with a check, then lifted off the top.
                        face.overlay { stamp("checkmark", Palette.positive) }
                            .offset(y: -Travel.lift * t)
                            .scaleEffect(1 - 0.04 * t)
                            .opacity(1 - t * t)
                    case .decided(.mute):
                        // The colour drains first, then it sinks out of sight.
                        face.overlay { stamp("speaker.slash.fill", Palette.inkFaint) }
                            .saturation(1 - mark)
                            .brightness(-0.12 * mark)
                            .scaleEffect(1 - 0.1 * t)
                            .offset(y: Travel.sink * t)
                            .opacity(1 - t)
                    case .decided(.unsubscribe):
                        severed
                    }
                }
            }
            .onAppear(perform: run)
        }

        /// LETTING GO. A bright line is drawn across the card, and the two
        /// halves it leaves come apart: the top drifts up and fades, the
        /// bottom tips and falls away under its own weight.
        private var severed: some View {
            ZStack {
                face
                    .mask { CutHalf(top: true) }
                    .rotationEffect(.degrees(-2.5 * t), anchor: .topLeading)
                    .offset(x: -8 * t, y: -26 * t)
                    .opacity(1 - t)
                face
                    .mask { CutHalf(top: false) }
                    .rotationEffect(.degrees(9 * t), anchor: .bottomLeading)
                    .offset(x: 14 * t, y: Travel.drop * t * t)
                    .opacity(1 - t * t)
                CutLine()
                    .trim(from: 0, to: mark)
                    .stroke(.white, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                    .shadow(color: Palette.accent, radius: 6)
                    .shadow(color: Palette.accent.opacity(0.8), radius: 14)
                    .opacity(Double(1 - t) * (mark > 0 ? 1 : 0))
            }
        }

        private func stamp(_ symbol: String, _ color: Color) -> some View {
            ZStack {
                Circle().fill(color.opacity(0.16))
                Circle().strokeBorder(color, lineWidth: 2)
                Image(systemName: symbol)
                    .font(.system(size: 24, weight: .bold))
                    .foregroundStyle(color)
            }
            .frame(width: 64, height: 64)
            .scaleEffect(0.6 + 0.4 * mark)
            .opacity(Double(mark))
        }

        private func run() {
            if reduceMotion {
                withAnimation(.easeOut(duration: 0.2)) { t = 1 }
                return
            }
            switch exit {
            case .under:
                withAnimation(.easeIn(duration: 0.2)) { t = 1 }
            case .decided(.skip):
                withAnimation(.easeInOut(duration: 0.34)) { t = 1 }
            case .decided(.clear):
                withAnimation(.spring(response: 0.22, dampingFraction: 0.6)) { mark = 1 }
                withAnimation(.easeIn(duration: 0.34).delay(0.16)) { t = 1 }
            case .decided(.mute):
                withAnimation(.easeOut(duration: 0.22)) { mark = 1 }
                withAnimation(.easeIn(duration: 0.44).delay(0.14)) { t = 1 }
            case .decided(.unsubscribe):
                withAnimation(.easeOut(duration: 0.14)) { mark = 1 }
                withAnimation(.easeIn(duration: 0.5).delay(0.13)) { t = 1 }
            }
        }
    }

    /// Where an unsubscribed card is cut: a little above the middle, on a
    /// slight slope, so the two halves read as torn apart rather than as two
    /// tidy boxes.
    private enum Cut {
        static let left: CGFloat = 0.5
        static let right: CGFloat = 0.4
    }

    private struct CutHalf: Shape {
        let top: Bool
        func path(in rect: CGRect) -> Path {
            let l = CGPoint(x: rect.minX, y: rect.minY + rect.height * Cut.left)
            let r = CGPoint(x: rect.maxX, y: rect.minY + rect.height * Cut.right)
            var p = Path()
            if top {
                p.move(to: CGPoint(x: rect.minX, y: rect.minY - 40))
                p.addLine(to: CGPoint(x: rect.maxX, y: rect.minY - 40))
                p.addLine(to: r)
                p.addLine(to: l)
            } else {
                p.move(to: l)
                p.addLine(to: r)
                p.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY + 40))
                p.addLine(to: CGPoint(x: rect.minX, y: rect.maxY + 40))
            }
            p.closeSubpath()
            return p
        }
    }

    private struct CutLine: Shape {
        func path(in rect: CGRect) -> Path {
            var p = Path()
            p.move(to: CGPoint(x: rect.minX - 10, y: rect.minY + rect.height * Cut.left))
            p.addLine(to: CGPoint(x: rect.maxX + 10, y: rect.minY + rect.height * Cut.right))
            return p
        }
    }

    // MARK: - confirm and apply

    /// THE ONE PRESS. What was staged, in the person's terms, and Return to
    /// apply it. The same card stays up through the apply: the senders
    /// gathered at its top are what fly away when the press lands.
    private struct PurgeConfirmCard: View {
        @Environment(AppStore.self) private var store
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        let session: AppStore.PurgeSession

        /// Esc was pressed once: the card is asking before it discards.
        @State private var asking = false
        /// The pile has been let go.
        @State private var released = false
        @State private var shown = false

        private var applying: Bool { session.phase == .applying }
        private var summary: PurgeSummary { session.summary }
        private var staged: [PurgeCard] {
            session.deck.plan.filter { $0.decision != .skip }.map(\.card)
        }

        var body: some View {
            VStack(spacing: Space.s16) {
                pile
                VStack(spacing: Space.s4) {
                    ForEach(PurgeCopy.plan(summary), id: \.self) { line in
                        Text(line)
                            .font(Typo.serif(24, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .multilineTextAlignment(.center)
                    }
                }
                .opacity(applying ? 0.45 : 1)
                details
                    .opacity(applying ? 0.45 : 1)
                Rectangle().fill(Palette.hairline).frame(height: 1)
                actions
            }
            .padding(Space.scaled(22))
            .frame(width: Typo.measure(400))
            .modifier(PurgeSurface())
            .overlay {
                // Alive while the requests are out, and no spinner: the flow
                // hint's glint, all the way round the card.
                if applying { FlowBorder(radius: 22, moving: !reduceMotion) }
            }
            .shadow(color: .black.opacity(0.3), radius: 34, y: 16)
            .scaleEffect(shown ? 1 : (reduceMotion ? 1 : 0.94))
            .opacity(shown ? 1 : 0)
            .onAppear {
                withAnimation(reduceMotion ? .easeOut(duration: 0.2) : Motion.deckCard) {
                    shown = true
                }
                // Mounted straight into the apply (a relaunch mid-flight, or
                // the fixture): the pile has already gone.
                if applying { release() }
                AccessibilityNotification.Announcement(spoken).post()
            }
            .onChange(of: applying) { _, now in
                if now { release() } else { released = false }
            }
            .keyBindings(.modal, bindings)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(spoken)
        }

        private var spoken: String {
            var parts = PurgeCopy.plan(summary)
            if let note = PurgeCopy.siteNote(summary) { parts.append(note) }
            if let leaving = PurgeCopy.leaving(summary.emailsCleared) { parts.append(leaving) }
            if let fewer = PurgeCopy.fewer(summary.fewerPerMonth) { parts.append(fewer) }
            parts.append("Nothing has been sent. Press Return to confirm, Z to go back.")
            return parts.joined(separator: ". ")
        }

        // MARK: the pile

        private static let pileLimit = 11

        /// The senders being let go, gathered. On the press every one of them
        /// flies out and away at once, each on its own heading.
        private var pile: some View {
            let shownCards = Array(staged.prefix(Self.pileLimit))
            let more = staged.count - shownCards.count
            return HStack(spacing: -Space.scaled(7)) {
                ForEach(Array(shownCards.enumerated()), id: \.element.id) { i, card in
                    Avatar(
                        sender: "\(PurgeCardFace.name(card)) <\(card.sender.sender)>",
                        size: Space.scaled(30)
                    )
                    .background(
                        Circle().fill(purgeSurface).padding(-2.5)
                    )
                    .modifier(Flown(index: i, of: shownCards.count, released: released, still: reduceMotion))
                    .zIndex(Double(shownCards.count - i))
                }
                if more > 0 {
                    Text("+\(more)")
                        .font(Typo.num(11, weight: .semibold))
                        .foregroundStyle(Palette.inkDim)
                        .padding(.leading, Space.s14)
                        .opacity(released ? 0 : 1)
                }
            }
            .frame(height: Space.scaled(34))
            .accessibilityHidden(true)
        }

        /// One avatar leaving: out along its own heading, spinning a little,
        /// shrinking to nothing. The headings fan across the upper half so
        /// the whole pile bursts up and outward from the card.
        private struct Flown: ViewModifier {
            let index: Int
            let of: Int
            let released: Bool
            let still: Bool

            func body(content: Content) -> some View {
                let spread = of > 1 ? Double(index) / Double(of - 1) : 0.5
                // 200 degrees to -20: left, up and over, right.
                let angle = (200 - 220 * spread) * .pi / 180
                let reach: Double = 240 + Double((index * 53) % 120)
                let dx = released && !still ? cos(angle) * reach : 0
                let dy = released && !still ? -sin(angle) * reach : 0
                content
                    .scaleEffect(released ? (still ? 1 : 0.3) : 1)
                    .rotationEffect(.degrees(released && !still ? Double((index % 2 == 0 ? 1 : -1) * 70) : 0))
                    .offset(x: dx, y: dy)
                    .opacity(released ? 0 : 1)
                    .animation(
                        still
                            ? .easeOut(duration: 0.25)
                            : .easeOut(duration: 0.62).delay(Double(index) * 0.022),
                        value: released)
            }
        }

        private func release() {
            guard !released else { return }
            released = true
        }

        // MARK: the small print

        private var details: some View {
            VStack(spacing: Space.s8) {
                if let note = PurgeCopy.siteNote(summary) {
                    Text(note)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                }
                let facts = [
                    PurgeCopy.leaving(summary.emailsCleared), PurgeCopy.fewer(summary.fewerPerMonth),
                ].compactMap { $0 }
                if !facts.isEmpty {
                    Text(facts.joined(separator: "  ·  "))
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                }
            }
        }

        // MARK: the actions

        @ViewBuilder private var actions: some View {
            if applying {
                Text("Sending your choices")
                    .font(Typo.ui(12, weight: .semibold))
                    .foregroundStyle(Palette.inkDim)
                    .frame(height: Space.scaled(34))
                    .transition(.opacity)
            } else if asking {
                VStack(spacing: Space.s8) {
                    Text("Throw these choices away? Nothing has been sent.")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.ink)
                    HStack(spacing: Space.s8) {
                        secondary("esc", "Discard", danger: true) { store.cancelPurge() }
                        secondary("z", "Keep them") { asking = false }
                    }
                }
                .transition(.opacity)
            } else {
                HStack(spacing: Space.s8) {
                    secondary("z", "Back") { store.purgeBack() }
                    Button(action: confirm) {
                        HStack(spacing: Space.s8) {
                            PurgeKey("return", style: .onAccent)
                            Text("Confirm")
                                .font(Typo.ui(13, weight: .semibold))
                                .foregroundStyle(onAccent)
                        }
                        .frame(maxWidth: .infinity)
                        .frame(height: Space.scaled(34))
                        .background(
                            RoundedRectangle(cornerRadius: 11, style: .continuous)
                                .fill(Palette.accent)
                        )
                        .contentShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Confirm")
                }
                .transition(.opacity)
            }
        }

        private func secondary(
            _ key: String, _ label: String, danger: Bool = false, action: @escaping () -> Void
        ) -> some View {
            Button(action: action) {
                HStack(spacing: Space.s6) {
                    Kbd(key)
                    Text(label)
                        .font(Typo.ui(12, weight: .semibold))
                        .foregroundStyle(danger ? Palette.danger : Palette.ink)
                }
                .padding(.horizontal, Space.s14)
                .frame(height: Space.scaled(34))
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(danger ? Palette.dangerSoft : Palette.hairline.opacity(0.55))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(Palette.hairline, lineWidth: 1)
                )
                .contentShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
            }
            .buttonStyle(.plain)
            .accessibilityLabel(label)
        }

        private func confirm() {
            guard !applying, !asking else { return }
            NSHapticFeedbackManager.defaultPerformer.perform(.levelChange, performanceTime: .now)
            release()
            Task { await store.commitPurge() }
        }

        /// WHILE THE QUESTION IS UP, RETURN APPLIES NOTHING. It answers the
        /// question (keep them) and that is all: a Return aimed at a prompt
        /// about discarding must not send nine unsubscribes instead.
        private var bindings: [KeyBinding] {
            if applying {
                // No way back and no cancel: what has been asked of a sender
                // has been asked. The keys are held so nothing underneath
                // takes them.
                return [
                    KeyBinding("Escape", "nothing") {}, KeyBinding("Enter", "nothing") {},
                    KeyBinding("z", "nothing") {}, KeyBinding("u", "nothing") {},
                ]
            }
            return [
                KeyBinding("Enter", "confirm", onRelease: { _ in }) {
                    if asking { asking = false } else { confirm() }
                },
                KeyBinding("z", "back to the deck") {
                    if asking { asking = false } else { store.purgeBack() }
                },
                KeyBinding("Backspace", "back to the deck") {
                    if asking { asking = false } else { store.purgeBack() }
                },
                KeyBinding("Escape", "discard") {
                    if asking { store.cancelPurge() } else { asking = true }
                },
                // The deck's `u` is over; do not let it reach the global undo.
                KeyBinding("u", "nothing") {},
            ]
        }
    }

    // MARK: - the end

    /// A PURGE, DONE. The walk's own celebration (`WalkComplete`): the P7
    /// squelch across the top of a card, a lock beam, a ring and check, and
    /// what it came to counting up, with the chime.
    ///
    /// The headline is the best true thing: the mail that stops arriving.
    /// Under the stats, plainly and small, anything that did not go through,
    /// and the unsubscribes that are the person's to finish on the sender's
    /// own site. With those on it the card waits to be dismissed; without, it
    /// leaves by itself like the walk's.
    struct PurgeComplete: View {
        @Environment(AppStore.self) private var store
        let finish: AppStore.PurgeFinish
        let dismiss: () -> Void

        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @State private var shown = false
        @State private var beam: CGFloat = -0.3
        @State private var ring: CGFloat = 0
        @State private var check: CGFloat = 0
        @State private var pulse = false
        @State private var counted = false
        @State private var leaving = false
        /// There was something to finish when the card went up, so it stays
        /// until dismissed, even once every row has been visited.
        @State private var stays = false

        private static let phosphor = Color(hex: 0x82BAF5)
        private static let width: CGFloat = 372

        private var summary: PurgeSummary { finish.summary }
        private var headline: String { PurgeCopy.finished(summary) }

        private var spoken: String {
            var parts = [headline]
            if summary.unsubscribed > 0 { parts.append("\(summary.unsubscribed) unsubscribed") }
            if summary.muted > 0 { parts.append("\(summary.muted) muted") }
            if summary.cleared > 0 { parts.append("\(summary.cleared) cleared") }
            if let failed = PurgeCopy.failedNote(summary.failed) { parts.append(failed) }
            if !finish.sites.isEmpty {
                parts.append("\(finish.sites.count) to finish on their site")
            }
            return parts.joined(separator: ". ")
        }

        var body: some View {
            ZStack {
                Color.black.opacity(shown && !leaving ? 0.45 : 0)
                    .ignoresSafeArea()
                    .onTapGesture { if !stays { leave() } }
                card
                    .scaleEffect(shown && !leaving ? 1 : (reduceMotion ? 1 : 0.88))
                    .opacity(shown && !leaving ? 1 : 0)
            }
            .task { await run() }
            .keyContext(.modal)
            .keyBindings(.modal, [
                KeyBinding("Escape", "close") { leave() },
                KeyBinding("Enter", "close") { leave() },
            ])
        }

        private var card: some View {
            VStack(spacing: 0) {
                hero
                VStack(spacing: Space.s14) {
                    Text(headline)
                        .font(Typo.hero(27))
                        .foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 0) {
                        stat(counted ? summary.unsubscribed : 0, "unsubscribed")
                        divider
                        stat(counted ? summary.muted : 0, "muted")
                        divider
                        stat(counted ? summary.cleared : 0, "cleared")
                    }
                    if let failed = PurgeCopy.failedNote(summary.failed) {
                        Text(failed)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                            .multilineTextAlignment(.center)
                    }
                    if stays {
                        sites
                    }
                }
                .padding(.horizontal, Space.scaled(22))
                .padding(.top, Space.scaled(18))
                .padding(.bottom, Space.scaled(20))
            }
            .frame(width: Self.width)
            .passbandGlass(.pane, cornerRadius: 22, tint: Palette.glassTintStrong)
            .shadow(color: .black.opacity(0.32), radius: 40, y: 18)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(spoken)
        }

        // MARK: finish on their site

        private var sites: some View {
            VStack(alignment: .leading, spacing: Space.s8) {
                Rectangle().fill(Palette.hairline).frame(height: 1)
                if finish.sites.isEmpty {
                    Text("Nothing left to finish.")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .frame(maxWidth: .infinity, alignment: .center)
                } else {
                    VStack(alignment: .leading, spacing: Space.s2) {
                        Text("Finish on their site")
                            .font(Typo.ui(12, weight: .semibold))
                            .foregroundStyle(Palette.ink)
                        Text(
                            finish.sites.count == 1
                                ? "Their mail is cleared. You are still subscribed until you do."
                                : "Their mail is cleared. You are still subscribed to these until you do."
                        )
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize(horizontal: false, vertical: true)
                    }
                    ForEach(finish.sites) { site in
                        siteRow(site)
                    }
                }
                HStack {
                    Spacer()
                    Button(action: leave) {
                        HStack(spacing: Space.s6) {
                            Kbd("esc")
                            Text(finish.sites.isEmpty ? "Done" : "Later")
                                .font(Typo.ui(12, weight: .semibold))
                                .foregroundStyle(Palette.ink)
                        }
                        .padding(.horizontal, Space.s12)
                        .padding(.vertical, Space.s6)
                        .background(Capsule(style: .continuous).fill(Palette.hairline.opacity(0.7)))
                        .contentShape(Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(finish.sites.isEmpty ? "Done" : "Later")
                }
                .padding(.top, Space.s2)
            }
            .animation(.smooth(duration: 0.26), value: finish.sites)
        }

        private func siteRow(_ site: AppStore.PurgeFinish.Site) -> some View {
            let opening = finish.opening.contains(site.id)
            return HStack(spacing: Space.s8) {
                Avatar(sender: "\(site.displayName) <\(site.sender)>", size: Space.scaled(22))
                Text(site.displayName)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .help(site.displayName)
                Button {
                    Task { await store.openPurgeSite(site) }
                } label: {
                    HStack(spacing: Space.s4) {
                        Text(opening ? "Opening" : "Open")
                            .font(Typo.ui(11, weight: .semibold))
                        Image(systemName: "arrow.up.right")
                            .font(Typo.ui(9, weight: .bold))
                    }
                    .foregroundStyle(Palette.accentInk)
                    .padding(.horizontal, Space.s10)
                    .padding(.vertical, Space.s4)
                    .background(Capsule(style: .continuous).fill(Palette.accentSoft))
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .disabled(opening)
                .opacity(opening ? 0.6 : 1)
                .help("Opens their unsubscribe page in your browser")
                .accessibilityLabel("Open \(site.displayName)'s unsubscribe page")
            }
            .transition(.opacity)
        }

        // MARK: the hero (the walk's, on this card's own renderer)

        private var hero: some View {
            ZStack {
                Group {
                    if PhosphorScene.purgeDone.isUnavailable {
                        IntroSceneBackdrop(engaged: true, wide: true, paused: false)
                    } else {
                        PhosphorBackdrop(scene: .purgeDone)
                    }
                }
                .accessibilityHidden(true)
                LinearGradient(
                    colors: [.clear, .black.opacity(0.3)], startPoint: .center, endPoint: .bottom)
                GeometryReader { geo in
                    LinearGradient(
                        colors: [.clear, Self.phosphor.opacity(0.55), .white.opacity(0.85),
                                 Self.phosphor.opacity(0.55), .clear],
                        startPoint: .leading, endPoint: .trailing
                    )
                    .frame(width: geo.size.width * 0.28)
                    .blur(radius: 6)
                    .offset(x: beam * geo.size.width)
                    .blendMode(.plusLighter)
                }
                .allowsHitTesting(false)
                ZStack {
                    if !reduceMotion {
                        ForEach(0..<3) { i in
                            Circle()
                                .stroke(Self.phosphor.opacity(0.7), lineWidth: 1.5)
                                .frame(width: 58, height: 58)
                                .scaleEffect(pulse ? 2.8 : 1)
                                .opacity(pulse ? 0 : 0.9)
                                .animation(
                                    .easeOut(duration: 1.2).delay(Double(i) * 0.2), value: pulse)
                        }
                    }
                    Circle()
                        .fill(.black.opacity(0.35))
                        .frame(width: 58, height: 58)
                    Circle()
                        .trim(from: 0, to: ring)
                        .stroke(Self.phosphor, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                        .frame(width: 58, height: 58)
                        .shadow(color: Self.phosphor.opacity(0.9), radius: 6)
                    PurgeCheck()
                        .trim(from: 0, to: check)
                        .stroke(
                            .white,
                            style: StrokeStyle(lineWidth: 3, lineCap: .round, lineJoin: .round)
                        )
                        .frame(width: 22, height: 16)
                        .shadow(color: Self.phosphor, radius: 5)
                }
            }
            .frame(height: 140)
            .frame(maxWidth: .infinity)
            .clipShape(UnevenRoundedRectangle(topLeadingRadius: 22, topTrailingRadius: 22))
        }

        private func stat(_ n: Int, _ name: String) -> some View {
            VStack(spacing: Space.s2) {
                Text("\(n)")
                    .font(Typo.serif(22, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .monospacedDigit()
                    .contentTransition(.numericText(value: Double(n)))
                Text(name)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .textCase(.uppercase)
                    .tracking(0.5)
            }
            .frame(maxWidth: .infinity)
        }

        private var divider: some View {
            Rectangle().fill(Palette.hairline).frame(width: 1, height: 28)
        }

        private func run() async {
            stays = !finish.sites.isEmpty
            ClearChime.play()
            AccessibilityNotification.Announcement(spoken).post()
            withAnimation(
                reduceMotion ? .easeOut(duration: 0.2) : .spring(response: 0.42, dampingFraction: 0.66)
            ) { shown = true }
            if reduceMotion {
                ring = 1
                check = 1
                counted = true
            } else {
                try? await Task.sleep(for: .milliseconds(160))
                withAnimation(.easeInOut(duration: 0.55)) { beam = 1.1 }
                try? await Task.sleep(for: .milliseconds(320))
                withAnimation(.easeInOut(duration: 0.4)) { ring = 1 }
                pulse = true
                try? await Task.sleep(for: .milliseconds(300))
                withAnimation(.spring(response: 0.3, dampingFraction: 0.55)) { check = 1 }
                withAnimation(.easeOut(duration: 0.7)) { counted = true }
            }
            // Something to finish: the card is the person's to close. A line
            // about a failure earns a longer look than three good numbers.
            guard !stays else { return }
            try? await Task.sleep(for: .milliseconds(summary.failed > 0 ? 6000 : 3200))
            guard !Task.isCancelled else { return }
            leave()
        }

        private func leave() {
            guard !leaving else { return }
            withAnimation(.easeIn(duration: 0.28)) { leaving = true }
            Task {
                try? await Task.sleep(for: .milliseconds(300))
                dismiss()
            }
        }
    }

    /// A check mark drawn as one stroke, so it can be trimmed on.
    private struct PurgeCheck: Shape {
        func path(in rect: CGRect) -> Path {
            var p = Path()
            p.move(to: CGPoint(x: rect.minX, y: rect.midY))
            p.addLine(to: CGPoint(x: rect.minX + rect.width * 0.36, y: rect.maxY))
            p.addLine(to: CGPoint(x: rect.maxX, y: rect.minY))
            return p
        }
    }
#endif
