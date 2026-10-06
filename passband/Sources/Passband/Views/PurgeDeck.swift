// THE PURGE, ON SCREEN: a full-window walk through the Reading senders the
// owner has all but stopped reading (Lib/Purge.swift is the deck, AppStore
// the half that acts, this is what is seen and pressed).
//
// A PAGE, NOT A CARD. Each sender gets the whole window in two columns. On
// the left is who they are and what they send: their icon, their name, and
// one card per email of theirs in Reading, so the mail is recognised before
// anything is decided about it. On the right is the argument and the
// decision: what the person actually did with that mail ("Your reading"),
// the one way to end it, the rules that would keep part of it, and a way to
// say the deck got this one wrong.
//
// Nothing on the page is a count of what is left. The only numbers are
// evidence, and a tally of what has been decided, which only grows.
//
// EACH CHOICE LEAVES ITS OWN WAY, because at speed the exit is the only
// confirmation anybody reads. Unsubscribe lets the mail go: a bright cut
// under the sender and every email card drops away in turn. Block greys the
// whole page and sinks it as one slab. A rule sifts: every other card falls
// out and the rest rise away. Keep turns the page aside, untouched.
//
// SOLID SURFACES, NOT GLASS, on purpose: the page stands on the app's own
// canvas with nothing behind it to refract, and the exits desaturate, rotate
// and fade their pieces, which a glass effect does not do gracefully.
//
// NOTHING IS SENT FROM THE DECK. Decisions are staged in the store's session;
// the confirm page names every sender under what will happen to it, and its
// one press applies them. Then the end page stays with the work: who is
// unsubscribed, who is still running, and who needs a person.

#if os(macOS)
    import SwiftUI

    // MARK: - the page scaffold

    /// A full-window page on the app's canvas: a bar across the top, the
    /// content, a bar across the bottom. Opaque, so nothing of the board
    /// under it shows through. Shared with the smart done list, so the two
    /// read as one family.
    struct FullPage<Top: View, Content: View, Bottom: View>: View {
        let symbol: String
        let title: String
        @ViewBuilder var top: Top
        @ViewBuilder var content: Content
        @ViewBuilder var bottom: Bottom

        var body: some View {
            VStack(spacing: 0) {
                HStack(spacing: Space.s12) {
                    HStack(spacing: Space.scaled(5)) {
                        Image(systemName: symbol)
                            .font(Typo.ui(10, weight: .semibold))
                        Text(title)
                            .font(Typo.ui(9.5, weight: .bold).width(.expanded))
                            .tracking(0.7)
                            .textCase(.uppercase)
                    }
                    .foregroundStyle(Palette.accent)
                    .fixedSize()
                    top
                    Spacer(minLength: 0)
                }
                // Clear of the window's traffic lights.
                .padding(.leading, 86)
                .padding(.trailing, Space.s24)
                .frame(height: 46)
                Rectangle().fill(Palette.hairline).frame(height: 1)
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                Rectangle().fill(Palette.hairline).frame(height: 1)
                bottom
                    .padding(.horizontal, Space.s24)
                    .frame(height: Space.scaled(54))
            }
            .background(Palette.canvas.ignoresSafeArea())
            .ignoresSafeArea(edges: .top)
        }
    }

    /// The paper the page's cards are drawn on, a step up from the canvas.
    let pagePaper = Color(light: 0xF8FAFD, dark: 0x151E2A)

    /// The text that sits on the accent fill of a primary action.
    private let onAccent = Color(light: 0xFFFFFF, dark: 0x06111D)

    /// A key hint in a page's bars.
    struct PageHint: View {
        let key: String
        let label: String
        var body: some View {
            HStack(spacing: Space.s6) {
                Kbd(key)
                Text(label)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
            }
        }
    }

    /// A page's buttons: one filled primary, the rest quiet, all the same
    /// height. Borders are lit evenly all the way round.
    struct PageButton: View {
        enum Role { case primary, plain, danger }
        let key: String
        let label: String
        var role: Role = .plain
        var wide = false
        let action: () -> Void

        @State private var hovering = false

        var body: some View {
            Button(action: action) {
                HStack(spacing: Space.s8) {
                    if role == .primary { PurgeKey(key, style: .onAccent) } else { Kbd(key) }
                    Text(label)
                        .font(Typo.ui(12.5, weight: .semibold))
                        .foregroundStyle(
                            role == .primary ? onAccent : (role == .danger ? Palette.danger : Palette.ink))
                        .lineLimit(1)
                }
                .padding(.horizontal, Space.s16)
                .frame(maxWidth: wide ? .infinity : nil)
                .frame(height: Space.scaled(34))
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(
                            role == .primary
                                ? Palette.accent.opacity(hovering ? 0.88 : 1)
                                : (role == .danger
                                    ? Palette.dangerSoft : Palette.hairline.opacity(hovering ? 1 : 0.6)))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(role == .primary ? Color.clear : Palette.hairline, lineWidth: 1)
                )
                .contentShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .accessibilityLabel(label)
        }
    }

    // MARK: - the overlay

    /// Everything a purge session draws: the deck while deciding, then the
    /// confirm page, which stays up through the apply.
    struct PurgeOverlay: View {
        let session: AppStore.PurgeSession

        var body: some View {
            ZStack {
                switch session.phase {
                case .deciding:
                    PurgeDeckView(session: session)
                        .transition(.opacity)
                case .confirming, .applying:
                    PurgeConfirmPage(session: session)
                        .transition(.opacity)
                }
            }
            .animation(.smooth(duration: 0.26), value: session.phase == .deciding)
            .keyContext(.modal)
            .transition(.opacity)
        }
    }

    // MARK: - the deck

    /// How a sender's page leaves.
    private enum PurgeExit: Equatable {
        case decided(PurgeDecision.Kind)
        /// Back under the deck: the page on screen when `z` brings the
        /// previous one back over it.
        case under
    }

    /// How a sender's page arrives.
    private enum PurgeArrive: Equatable {
        /// The deck opening.
        case first
        /// The next sender, which was underneath all along.
        case under
        /// After a keep: in from the right as the kept page turns aside.
        case right
        /// `z`: back along the way the decision being undone left.
        case reverse(PurgeDecision.Kind)
    }

    /// A page in motion: which exit, how far through it (0 to 1), and how
    /// far through the mark that plays first.
    private struct PurgeMotion: Equatable {
        var exit: PurgeExit
        var t: CGFloat
        var mark: CGFloat
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
            FullPage(symbol: "scissors", title: "Purge") {
                PurgeTally(summary: session.summary)
            } content: {
                ZStack {
                    if let card = deck.current {
                        PurgeSenderPage(
                            card: card, ledgerDays: session.ledgerDays,
                            emails: store.purgeEmails(for: card),
                            suggestions: store.purgeSuggestions[card.id], motion: nil,
                            decide: decide
                        )
                        .modifier(PurgeArrival(style: arrive, reduceMotion: reduceMotion))
                        .id(card.id)
                        .transition(.identity)
                    }
                    ForEach(ghosts) { ghost in
                        PurgeGhost(
                            card: ghost.card, ledgerDays: session.ledgerDays,
                            emails: store.purgeEmails(for: ghost.card),
                            suggestions: store.purgeSuggestions[ghost.card.id],
                            exit: ghost.exit, reduceMotion: reduceMotion
                        )
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                    }
                }
                .clipped()
            } bottom: {
                HStack(spacing: Space.s16) {
                    PageHint(key: "z", label: "back")
                        .opacity(deck.progress.position > 1 ? 1 : 0.35)
                    // Where the deck is, as a line with no numbers on it.
                    GeometryReader { geo in
                        let p = deck.progress
                        let done = CGFloat(max(0, p.position - 1)) / CGFloat(max(1, p.total))
                        ZStack(alignment: .leading) {
                            Capsule().fill(Palette.hairlineStrong)
                            Capsule().fill(Palette.accent.opacity(0.8))
                                .frame(width: max(2, geo.size.width * done))
                        }
                        .frame(height: 2)
                        .frame(maxHeight: .infinity)
                    }
                    .animation(.smooth(duration: 0.3), value: deck.progress)
                    .accessibilityHidden(true)
                    PageHint(key: "esc", label: anythingStaged ? "review and confirm" : "leave")
                }
            }
            .keyBindings(.modal, bindings)
            // THE SUGGESTIONS FOR THIS SENDER AND THE NEXT, and no further:
            // each is a model call against a shared daily cap.
            .task(id: deck.current?.id) {
                guard let card = deck.current else { return }
                async let here: Void = store.loadPurgeSuggestions(for: card)
                if let next = deck.next { await store.loadPurgeSuggestions(for: next) }
                await here
            }
        }

        // MARK: keys

        /// HOLDS, NOT TAPS, for the decisions: a hold swallows its own
        /// autorepeat, so a key leaned on decides one sender and not a blur
        /// of them. Each also answers when its choice is not on offer (and
        /// does nothing), so `u` here can never fall through to the global
        /// undo, nor a digit to the rail's page keys.
        private var bindings: [KeyBinding] {
            [
                KeyBinding("u", "unsubscribe or block", onRelease: { _ in }) {
                    if let card = deck.current { decide(card.ending) }
                },
                KeyBinding("1", "first rule", onRelease: { _ in }) { pick(0) },
                KeyBinding("2", "second rule", onRelease: { _ in }) { pick(1) },
                KeyBinding("3", "third rule", onRelease: { _ in }) { pick(2) },
                KeyBinding("4", "nothing") {},
                KeyBinding("5", "nothing") {},
                KeyBinding("s", "keep", onRelease: { _ in }) { decide(.keep) },
                KeyBinding("z", "back one sender", onRelease: { _ in }) { back() },
                KeyBinding("Backspace", "back one sender", onRelease: { _ in }) { back() },
                KeyBinding("Delete", "back one sender", onRelease: { _ in }) { back() },
                KeyBinding("Escape", "leave or review") { leave() },
                // Return decides nothing here: the confirm page owns it.
                KeyBinding("Enter", "nothing") {},
            ]
        }

        /// A rule by its number, only once its sentence is on screen.
        private func pick(_ index: Int) {
            guard let card = deck.current,
                case .loaded(let found)? = store.purgeSuggestions[card.id],
                found.indices.contains(index)
            else { return }
            decide(.rule(found[index]))
        }

        private func decide(_ decision: PurgeDecision) {
            guard let card = deck.current, card.allows(decision) else { return }
            arrive = decision == .keep ? .right : .under
            push(Ghost(card: card, exit: .decided(decision.kind)))
            guard store.purgeDecide(decision) else { return }
            if decision != .keep {
                NSHapticFeedbackManager.defaultPerformer.perform(.alignment, performanceTime: .now)
            }
            AccessibilityNotification.Announcement(
                PurgeCopy.staged(decision, name: PurgeSenderPage.name(card))
            ).post()
        }

        private func back() {
            guard deck.current != nil, let previous = deck.previous else { return }
            arrive = .reverse(previous.decision.kind)
            if let current = deck.current { push(Ghost(card: current, exit: .under)) }
            store.purgeBack()
            AccessibilityNotification.Announcement(
                "Back to \(PurgeSenderPage.name(previous.card))."
            ).post()
        }

        /// Esc never throws decisions away: with something staged it goes to
        /// the confirm page, which is where discarding is asked about.
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
                try? await Task.sleep(for: .milliseconds(950))
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
                if summary.ending > 0 { TallyChip(count: summary.ending, label: "to unsubscribe") }
                if summary.blocked > 0 { TallyChip(count: summary.blocked, label: "to block") }
                if summary.rules > 0 {
                    TallyChip(count: summary.rules, label: summary.rules == 1 ? "new rule" : "new rules")
                }
                if summary.kept > 0 { TallyChip(count: summary.kept, label: "kept") }
                Text(summary.isEmpty ? "Nothing is sent until you confirm at the end." : "nothing sent yet")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .padding(.leading, summary.isEmpty ? 0 : Space.s4)
            }
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
            .background(Capsule(style: .continuous).fill(pagePaper))
            .background(Capsule(style: .continuous).fill(Palette.accent.opacity(glow ? 0.5 : 0)))
            // The whole border, evenly, brightening for a moment when the
            // number goes up.
            .overlay(
                Capsule(style: .continuous)
                    .strokeBorder(glow ? Palette.accent : Palette.hairlineStrong, lineWidth: 1)
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

    // MARK: - one sender's page

    /// One sender, drawn across the window. Used for the page being decided
    /// on (with `decide`) and for the ghost of one leaving (with `motion`).
    private struct PurgeSenderPage: View {
        let card: PurgeCard
        let ledgerDays: Int
        let emails: [PurgeEmail]
        let suggestions: AppStore.PurgeSuggestions?
        let motion: PurgeMotion?
        var decide: ((PurgeDecision) -> Void)?

        static func name(_ card: PurgeCard) -> String {
            let given = card.sender.display_name.trimmingCharacters(in: .whitespaces)
            return given.isEmpty ? SenderCache.resolved(card.sender.sender).displayName : given
        }

        static func avatar(_ card: PurgeCard) -> String { "\(name(card)) <\(card.sender.sender)>" }

        /// At most four: the daemon sends them strongest first, and a fifth
        /// line is a column somebody has to read rather than see.
        private var reasons: [PurgeReason] { Array(card.sender.reasons.prefix(4)) }

        var body: some View {
            HStack(alignment: .top, spacing: Space.s32) {
                left
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                ScrollView {
                    right
                        .padding(.bottom, Space.s20)
                }
                .scrollIndicators(.never)
                .frame(width: Typo.measure(380))
                .opacity(fade)
            }
            .padding(.horizontal, Space.scaled(36))
            .padding(.top, Space.s24)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(Self.name(card))
        }

        /// The right column and the header fade through an unsubscribe and a
        /// rule while the email cards do the moving.
        private var fade: Double {
            guard let motion else { return 1 }
            switch motion.exit {
            case .decided(.unsubscribe), .decided(.rule): return Double(1 - min(1, motion.t * 1.4))
            default: return 1
            }
        }

        // MARK: left: who, and what they send

        private var left: some View {
            VStack(alignment: .leading, spacing: Space.s16) {
                HStack(alignment: .center, spacing: Space.s14) {
                    Avatar(sender: Self.avatar(card), size: Space.scaled(56))
                        .overlay { stamp }
                    VStack(alignment: .leading, spacing: Space.s2) {
                        Text(Self.name(card))
                            .font(Typo.serif(28, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(2)
                            .truncationMode(.tail)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(card.sender.sender)
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkFaint)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                .help(Self.name(card))
                .overlay(alignment: .bottom) { cut }
                .opacity(fade)
                .accessibilityElement(children: .combine)

                Text("In Reading now")
                    .font(Typo.ui(9, weight: .bold).width(.expanded))
                    .tracking(0.5)
                    .textCase(.uppercase)
                    .foregroundStyle(Palette.inkFaint)
                    .opacity(fade)

                ScrollView {
                    VStack(alignment: .leading, spacing: Space.s8) {
                        ForEach(Array(emails.enumerated()), id: \.element.id) { i, email in
                            PurgeEmailCard(email: email)
                                .modifier(Falling(index: i, motion: motion))
                        }
                        // The daemon's count is the authority on how many
                        // there are; the board may hold fewer.
                        let more = card.sender.open_count - emails.count
                        if emails.isEmpty {
                            Text(
                                card.sender.open_count == 1
                                    ? "1 email of theirs is in Reading."
                                    : "\(card.sender.open_count) emails of theirs are in Reading."
                            )
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkFaint)
                        } else if more > 0 {
                            Text(more == 1 ? "and 1 more like these" : "and \(more) more like these")
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaint)
                                .padding(.leading, Space.s4)
                                .opacity(fade)
                        }
                    }
                    .padding(.bottom, Space.s24)
                }
                .scrollIndicators(.automatic)
                // A ghost's cards fall out of the list, not behind its edge.
                .scrollClipDisabled(motion != nil)
            }
        }

        /// The unsubscribe's cut: one bright line drawn under the sender,
        /// the thread their mail hangs from.
        @ViewBuilder private var cut: some View {
            if let motion, motion.exit == .decided(.unsubscribe) {
                GeometryReader { geo in
                    Capsule()
                        .fill(.white)
                        .frame(width: geo.size.width * motion.mark, height: 2)
                        .shadow(color: Palette.accent, radius: 5)
                        .shadow(color: Palette.accent.opacity(0.8), radius: 12)
                        .opacity(Double(1 - motion.t))
                }
                .frame(height: 2)
                .offset(y: Space.s8)
            }
        }

        /// What was decided, stamped on the sender's icon as it goes.
        @ViewBuilder private var stamp: some View {
            if let motion, case .decided(let kind) = motion.exit, kind != .keep {
                let look: (String, Color) =
                    switch kind {
                    case .unsubscribe: ("scissors", Palette.accent)
                    case .block: ("nosign", Palette.inkFaint)
                    case .rule: ("line.3.horizontal.decrease", Palette.accent)
                    case .keep: ("checkmark", Palette.positive)
                    }
                ZStack {
                    Circle().fill(pagePaper.opacity(0.9))
                    Circle().strokeBorder(look.1, lineWidth: 2)
                    Image(systemName: look.0)
                        .font(.system(size: 20, weight: .bold))
                        .foregroundStyle(look.1)
                }
                .scaleEffect(0.6 + 0.4 * motion.mark)
                .opacity(Double(motion.mark))
            }
        }

        // MARK: right: the argument and the decision

        private var right: some View {
            VStack(alignment: .leading, spacing: Space.s16) {
                why
                ending
                rules
                keep
            }
        }

        private var why: some View {
            VStack(alignment: .leading, spacing: Space.s10) {
                HStack(spacing: Space.s6) {
                    Text("Your reading")
                        .font(Typo.ui(9, weight: .bold).width(.expanded))
                        .tracking(0.5)
                        .textCase(.uppercase)
                        .foregroundStyle(Palette.accent)
                    Spacer(minLength: Space.s8)
                    // Once, quietly: what span every number here is about.
                    Text(PurgeCopy.window(ledgerDays: ledgerDays))
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }
                ForEach(Array(reasons.enumerated()), id: \.offset) { i, reason in
                    PurgeReasonRow(reason: reason, lead: i == 0, keep: false)
                }
            }
            .padding(Space.s14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(Palette.accentSoft.opacity(0.6))
            )
            .accessibilityElement(children: .combine)
            .accessibilityLabel(
                (reasons.map(PurgeCopy.sentence) + [PurgeCopy.window(ledgerDays: ledgerDays)])
                    .joined(separator: ". "))
        }

        /// The one way to end it, on one key, in one place: unsubscribe when
        /// there is a link, block when there is not, and the line beside it
        /// says which and why.
        private var ending: some View {
            VStack(alignment: .leading, spacing: Space.s8) {
                PageButton(
                    key: "u", label: PurgeCopy.title(card.ending), role: .primary, wide: true
                ) { decide?(card.ending) }
                Text(PurgeCopy.explains(card))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, Space.s2)
            }
        }

        /// The narrowing rules. A quiet placeholder while they load, and
        /// NOTHING AT ALL when none come back: an empty box headed
        /// "suggestions" would be a question with no answers under it.
        @ViewBuilder private var rules: some View {
            switch suggestions {
            case .loading?:
                VStack(alignment: .leading, spacing: Space.s8) {
                    Text(PurgeCopy.rulesLoading)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                    ForEach(0..<2, id: \.self) { _ in
                        RoundedRectangle(cornerRadius: 11, style: .continuous)
                            .fill(Palette.hairline.opacity(0.45))
                            .frame(height: Space.scaled(34))
                    }
                }
                .transition(.opacity)
            case .loaded(let found)? where !found.isEmpty && card.offersRules:
                VStack(alignment: .leading, spacing: Space.s8) {
                    Text(PurgeCopy.rulesLead)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkDim)
                    ForEach(Array(found.prefix(3).enumerated()), id: \.offset) { i, want in
                        PurgeRuleChoice(key: "\(i + 1)", want: want) { decide?(.rule(want)) }
                    }
                    Text(PurgeCopy.rulesNote)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .transition(.opacity)
            default:
                EmptyView()
            }
        }

        private var keep: some View {
            VStack(alignment: .leading, spacing: Space.s8) {
                Rectangle().fill(Palette.hairline).frame(height: 1)
                HStack(alignment: .center, spacing: Space.s12) {
                    PageButton(key: "s", label: PurgeCopy.title(.keep)) { decide?(.keep) }
                    Text(PurgeCopy.keepCaption)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    /// One email, as a card to recognise: its line and when it came. Not a
    /// button: nothing here opens.
    private struct PurgeEmailCard: View {
        let email: PurgeEmail

        var body: some View {
            HStack(alignment: .firstTextBaseline, spacing: Space.s12) {
                Text(email.line.isEmpty ? "(no summary)" : email.line)
                    .font(Typo.row)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(2)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if email.arrivedAt != nil {
                    Text(Fmt.relAge(email.arrivedAt))
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .fixedSize()
                }
            }
            .padding(.horizontal, Space.s14)
            .padding(.vertical, Space.s10)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous).fill(pagePaper)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(Palette.hairline, lineWidth: 1)
            )
            .accessibilityElement(children: .combine)
        }
    }

    /// A suggested rule: its number and the sentence, VERBATIM. It is
    /// model-written from sender-written subjects, so it is shown whole (it
    /// wraps rather than truncates) before the key that saves it is pressed.
    private struct PurgeRuleChoice: View {
        let key: String
        let want: String
        let action: () -> Void
        @State private var hovering = false

        var body: some View {
            Button(action: action) {
                HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
                    Kbd(key)
                    Text(want)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(.horizontal, Space.s12)
                .padding(.vertical, Space.scaled(9))
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(hovering ? Palette.accentSoft : pagePaper)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(
                            hovering ? Palette.accent.opacity(0.5) : Palette.hairlineStrong,
                            lineWidth: 1)
                )
                .contentShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .accessibilityLabel("Rule \(key): \(want)")
        }
    }

    /// A keycap that can sit on the card or on an accent fill. `Kbd` is
    /// drawn for the first of those only.
    struct PurgeKey: View {
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

    /// One email card's part in an exit. ANIMATABLE, so it is asked for
    /// every frame: the cards leave one after another, and a stagger worked
    /// out from the page's progress only exists if each frame works it out.
    private struct Falling: ViewModifier, @preconcurrency Animatable {
        let index: Int
        let exit: PurgeExit?
        var t: CGFloat

        init(index: Int, motion: PurgeMotion?) {
            self.index = index
            exit = motion?.exit
            t = motion?.t ?? 0
        }

        var animatableData: CGFloat {
            get { t }
            set { t = newValue }
        }

        private func clamp(_ x: CGFloat) -> CGFloat { min(1, max(0, x)) }

        func body(content: Content) -> some View {
            switch exit {
            case .decided(.unsubscribe):
                // LET GO, top first: each card tips and drops under its own
                // weight a beat after the one above it.
                let p = clamp(t * 1.9 - CGFloat(min(index, 9)) * 0.085)
                content
                    .rotationEffect(.degrees(Double(index % 2 == 0 ? -4 : 5) * p), anchor: .topLeading)
                    .offset(y: 300 * p * p)
                    .opacity(Double(1 - p * p))
            case .decided(.rule):
                // SIFTED: every other card falls out of the list, then what
                // is left rises away.
                if index % 2 == 1 {
                    let p = clamp(t * 2.2)
                    content.scaleEffect(1 - 0.12 * p).offset(x: 36 * p).opacity(Double(1 - p))
                } else {
                    let p = clamp(t * 2.2 - 1.1)
                    content.offset(y: -70 * p).opacity(Double(1 - p))
                }
            default:
                content
            }
        }
    }

    /// How far a page travels when it goes.
    private enum Travel {
        static let aside: CGFloat = 560
        static let sink: CGFloat = 90
        static let rise: CGFloat = 120
    }

    /// The page arriving. Reduce Motion: a fade and nothing else.
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
                        content.offset(y: 16 * away).opacity(a)
                    case .under:
                        content.scaleEffect(0.985 + 0.015 * a).offset(y: 14 * away).opacity(a)
                    case .right:
                        content.offset(x: Travel.aside * 0.6 * away).opacity(a)
                    case .reverse(.keep):
                        content.offset(x: -Travel.aside * 0.6 * away).opacity(a)
                    case .reverse(.rule):
                        content.offset(y: -Travel.rise * away).opacity(a)
                    case .reverse(.block):
                        content.offset(y: Travel.sink * away).saturation(a).opacity(a)
                    case .reverse(.unsubscribe):
                        content.offset(y: Travel.rise * away).opacity(a)
                    }
                }
            }
            .onAppear {
                let animation: Animation
                if reduceMotion {
                    animation = .easeOut(duration: 0.2)
                } else if case .under = style {
                    // Behind the page leaving, so the two are read as one
                    // going and one coming and not as a swap.
                    animation = Motion.deckCard.delay(0.22)
                } else {
                    animation = Motion.deckCard
                }
                withAnimation(animation) { a = 1 }
            }
        }
    }

    /// A page on its way out. Not interactive, not read aloud.
    private struct PurgeGhost: View {
        let card: PurgeCard
        let ledgerDays: Int
        let emails: [PurgeEmail]
        let suggestions: AppStore.PurgeSuggestions?
        let exit: PurgeExit
        let reduceMotion: Bool

        @State private var t: CGFloat = 0
        @State private var mark: CGFloat = 0

        private func page(moving: Bool) -> some View {
            PurgeSenderPage(
                card: card, ledgerDays: ledgerDays, emails: emails, suggestions: suggestions,
                motion: moving ? PurgeMotion(exit: exit, t: t, mark: mark) : nil)
        }

        var body: some View {
            Group {
                if reduceMotion {
                    page(moving: false).opacity(1 - t)
                } else {
                    switch exit {
                    case .under:
                        page(moving: false).scaleEffect(1 - 0.02 * t).opacity(1 - t)
                    case .decided(.keep):
                        // THE PAGE TURN, as the reader's S does it: aside to
                        // the left, untouched, dimming as it goes.
                        page(moving: false)
                            .offset(x: -Travel.aside * t)
                            .opacity(1 - t)
                    case .decided(.block):
                        // SHUT OUT: the colour drains from the whole page,
                        // then it sinks as one slab.
                        page(moving: true)
                            .saturation(1 - mark)
                            .brightness(-0.08 * mark)
                            .scaleEffect(1 - 0.05 * t)
                            .offset(y: Travel.sink * t)
                            .opacity(1 - t)
                    case .decided(.unsubscribe), .decided(.rule):
                        // The cards do the leaving (see `Falling`).
                        page(moving: true)
                    }
                }
            }
            .onAppear(perform: run)
        }

        private func run() {
            if reduceMotion {
                withAnimation(.easeOut(duration: 0.2)) { t = 1 }
                return
            }
            switch exit {
            case .under:
                withAnimation(.easeIn(duration: 0.2)) { t = 1 }
            case .decided(.keep):
                withAnimation(.easeInOut(duration: 0.36)) { t = 1 }
            case .decided(.block):
                withAnimation(.easeOut(duration: 0.2)) { mark = 1 }
                withAnimation(.easeIn(duration: 0.42).delay(0.14)) { t = 1 }
            case .decided(.unsubscribe):
                withAnimation(.easeOut(duration: 0.16)) { mark = 1 }
                // Linear, because each card makes its own curve out of it.
                withAnimation(.linear(duration: 0.72).delay(0.1)) { t = 1 }
            case .decided(.rule):
                withAnimation(.spring(response: 0.22, dampingFraction: 0.6)) { mark = 1 }
                withAnimation(.linear(duration: 0.62).delay(0.06)) { t = 1 }
            }
        }
    }

    // MARK: - confirm and apply

    /// THE ONE PRESS. Every staged decision, grouped by what will happen and
    /// with the senders NAMED under each, so Return is pressed looking at
    /// exactly who it is about. The same page stays up through the apply.
    private struct PurgeConfirmPage: View {
        @Environment(AppStore.self) private var store
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        let session: AppStore.PurgeSession

        /// Esc was pressed once: the page is asking before it discards.
        @State private var asking = false

        private var applying: Bool { session.phase == .applying }
        private var summary: PurgeSummary { session.summary }
        private var groups: [PurgePlan.Group] { PurgePlan.groups(session.deck) }

        var body: some View {
            FullPage(symbol: "scissors", title: "Purge") {
                Text(applying ? "Sending your choices" : "Nothing has been sent yet.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
            } content: {
                ScrollView {
                    VStack(alignment: .leading, spacing: Space.s24) {
                        Text("Here is what will happen")
                            .font(Typo.serif(30, weight: .medium))
                            .foregroundStyle(Palette.ink)
                        ForEach(groups) { group in
                            section(group)
                        }
                    }
                    .frame(maxWidth: Typo.measure(620), alignment: .leading)
                    .padding(.horizontal, Space.s24)
                    .padding(.vertical, Space.s32)
                    .frame(maxWidth: .infinity)
                }
                .opacity(applying ? 0.55 : 1)
            } bottom: {
                actions
            }
            .animation(.easeOut(duration: 0.2), value: asking)
            .animation(.easeOut(duration: 0.25), value: applying)
            .keyBindings(.modal, bindings)
            .onAppear { AccessibilityNotification.Announcement(spoken).post() }
        }

        private var spoken: String {
            var parts: [String] = []
            for group in groups {
                let names = group.entries.map { PurgeSenderPage.name($0.card) }.joined(separator: ", ")
                parts.append("\(PurgeCopy.heading(group.kind)): \(names)")
            }
            parts.append("Nothing has been sent. Press Return to confirm, Z to go back.")
            return parts.joined(separator: ". ")
        }

        private func section(_ group: PurgePlan.Group) -> some View {
            let quiet = group.kind == .keep
            return VStack(alignment: .leading, spacing: Space.s8) {
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(PurgeCopy.heading(group.kind))
                        .font(Typo.ui(13, weight: .semibold))
                        .foregroundStyle(quiet ? Palette.inkDim : Palette.ink)
                    if let note = PurgeCopy.note(group.kind) {
                        Text(note)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                    }
                }
                VStack(spacing: 0) {
                    ForEach(Array(group.entries.enumerated()), id: \.element.id) { i, entry in
                        if i > 0 { Rectangle().fill(Palette.hairline).frame(height: 1) }
                        row(entry, kind: group.kind)
                    }
                }
                .background(
                    RoundedRectangle(cornerRadius: 13, style: .continuous)
                        .fill(quiet ? Color.clear : pagePaper)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 13, style: .continuous)
                        .strokeBorder(Palette.hairline, lineWidth: 1)
                )
            }
        }

        private func row(_ entry: PurgePlan.Entry, kind: PurgeDecision.Kind) -> some View {
            HStack(alignment: .center, spacing: Space.s10) {
                Avatar(sender: PurgeSenderPage.avatar(entry.card), size: Space.scaled(26))
                VStack(alignment: .leading, spacing: Space.s2) {
                    HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                        Text(PurgeSenderPage.name(entry.card))
                            .font(Typo.ui(13, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Text(entry.card.sender.sender)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .layoutPriority(-1)
                    }
                    if let detail = entry.detail {
                        // A rule's sentence is shown whole; the site note is
                        // set apart in the warning colour.
                        Text(detail)
                            .font(Typo.rowSub)
                            .foregroundStyle(kind == .rule ? Palette.inkDim : Palette.warn)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.horizontal, Space.s12)
            .padding(.vertical, Space.s8)
            .accessibilityElement(children: .combine)
        }

        // MARK: the actions

        @ViewBuilder private var actions: some View {
            if applying {
                HStack {
                    Spacer()
                    Text("Sending your choices")
                        .font(Typo.ui(12, weight: .semibold))
                        .foregroundStyle(Palette.inkDim)
                        .padding(.horizontal, Space.s20)
                        .frame(height: Space.scaled(34))
                        // Alive while the requests are out, and no spinner:
                        // the flow hint's glint, all the way round.
                        .overlay { FlowBorder(radius: 11, moving: !reduceMotion) }
                    Spacer()
                }
            } else if asking {
                HStack(spacing: Space.s12) {
                    Text("Throw these choices away? Nothing has been sent.")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.ink)
                    Spacer(minLength: Space.s8)
                    PageButton(key: "z", label: "Keep them") { asking = false }
                    PageButton(key: "esc", label: "Discard", role: .danger) { store.cancelPurge() }
                }
            } else {
                HStack(spacing: Space.s12) {
                    if let fewer = PurgeCopy.fewer(summary.fewerPerMonth) {
                        Text(fewer)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                    }
                    Spacer(minLength: Space.s8)
                    PageButton(key: "z", label: "Back") { store.purgeBack() }
                    PageButton(key: "return", label: "Confirm", role: .primary) { confirm() }
                }
            }
        }

        private func confirm() {
            guard !applying, !asking else { return }
            NSHapticFeedbackManager.defaultPerformer.perform(.levelChange, performanceTime: .now)
            Task { await store.commitPurge() }
        }

        /// WHILE THE QUESTION IS UP, RETURN APPLIES NOTHING. It answers the
        /// question (keep them) and that is all: a Return aimed at a prompt
        /// about discarding must not send nine unsubscribes instead.
        private var bindings: [KeyBinding] {
            // Held so the rail's page keys and the global undo stay out.
            let inert = ["1", "2", "3", "4", "5", "u", "s"].map { KeyBinding($0, "nothing") {} }
            if applying {
                // No way back and no cancel: what has been asked of a sender
                // has been asked.
                return inert + [
                    KeyBinding("Escape", "nothing") {}, KeyBinding("Enter", "nothing") {},
                    KeyBinding("z", "nothing") {},
                ]
            }
            return inert + [
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
            ]
        }
    }

    // MARK: - the end: a status board

    /// AFTER THE PRESS. Not a celebration that walks away: a board that
    /// stays with the work. Each unsubscribe is running, then done, or needs
    /// a person; blocks and rules are simply done.
    ///
    /// When everything automatic has settled and nothing needs hands, THEN
    /// the walk's own reward plays (the P7 squelch, the lock beam, the ring
    /// and check, the chime) and the page leaves by itself. If anything needs
    /// hands it says so first and largest, lists each sender with the button
    /// that opens their page, and waits: the reward comes when the last one
    /// has been opened, and the page is the person's to close.
    struct PurgeBoard: View {
        @Environment(AppStore.self) private var store
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        let finish: AppStore.PurgeFinish
        let dismiss: () -> Void

        @State private var beam: CGFloat = -0.3
        @State private var check: CGFloat = 0
        @State private var pulse = false
        @State private var celebrated = false
        @State private var alerted = false
        @State private var alertGlow = false
        /// Something needed a person at some point, so the page never
        /// leaves by itself.
        @State private var stays = false
        @State private var leaving = false

        private static let phosphor = Color(hex: 0x82BAF5)

        private var statuses: [(card: PurgeCard, status: PurgeStatus)] { store.purgeStatuses(finish) }

        var body: some View {
            let statuses = statuses
            let summary = PurgeSummary(statuses: statuses)
            let waiting = statuses.filter { if case .needsYou = $0.status { true } else { false } }
            FullPage(symbol: "scissors", title: "Purge") {
                EmptyView()
            } content: {
                ScrollView {
                    VStack(spacing: Space.s20) {
                        hero(summary)
                        if !waiting.isEmpty { needsYou(waiting) }
                        ledger(statuses)
                    }
                    .frame(maxWidth: Typo.measure(560))
                    .padding(.horizontal, Space.s24)
                    .padding(.vertical, Space.s24)
                    .frame(maxWidth: .infinity)
                }
            } bottom: {
                HStack(spacing: Space.s12) {
                    if !waiting.isEmpty {
                        Text(PurgeCopy.laterNote)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                            .fixedSize(horizontal: false, vertical: true)
                    } else if let failed = PurgeCopy.failedNote(summary.failed) {
                        Text(failed)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                    }
                    Spacer(minLength: Space.s8)
                    PageButton(
                        key: "return", label: waiting.isEmpty ? "Done" : "Later",
                        role: waiting.isEmpty && summary.isSettled ? .primary : .plain
                    ) { leave() }
                }
            }
            .opacity(leaving ? 0 : 1)
            .keyContext(.modal)
            .keyBindings(.modal, [
                KeyBinding("Escape", "close") { leave() },
                KeyBinding("Enter", "close") { leave() },
            ] + ["1", "2", "3", "4", "5", "u", "s", "z"].map { KeyBinding($0, "nothing") {} })
            .task(id: Phase(summary)) { await react(summary) }
            .animation(.smooth(duration: 0.3), value: summary)
        }

        /// What the page should be doing, as one value to react to.
        private enum Phase: Hashable {
            case running, needsYou, clear
            init(_ s: PurgeSummary) {
                self = !s.isSettled ? .running : (s.needsYou > 0 ? .needsYou : .clear)
            }
        }

        private func react(_ summary: PurgeSummary) async {
            switch Phase(summary) {
            case .running:
                break
            case .needsYou:
                stays = true
                guard !alerted else { return }
                alerted = true
                // SAID, not only shown: a list at the bottom of a page of
                // good news is exactly what gets missed.
                NSHapticFeedbackManager.defaultPerformer.perform(.generic, performanceTime: .now)
                AccessibilityNotification.Announcement(
                    "\(PurgeCopy.headline(summary)) \(PurgeCopy.needsYouLead)"
                ).post()
                alertGlow = true
                withAnimation(.easeOut(duration: 1.4)) { alertGlow = false }
            case .clear:
                guard !celebrated else { return }
                celebrated = true
                ClearChime.play()
                AccessibilityNotification.Announcement(spoken(summary)).post()
                if reduceMotion {
                    check = 1
                } else {
                    withAnimation(.easeInOut(duration: 0.55)) { beam = 1.1 }
                    try? await Task.sleep(for: .milliseconds(380))
                    pulse = true
                    withAnimation(.spring(response: 0.3, dampingFraction: 0.55)) { check = 1 }
                }
                // The person's to close once anything needed them, or
                // anything failed: a line about a failure is not read in
                // the time three good numbers are.
                guard !stays, summary.failed == 0 else { return }
                try? await Task.sleep(for: .milliseconds(4200))
                guard !Task.isCancelled else { return }
                leave()
            }
        }

        private func spoken(_ s: PurgeSummary) -> String {
            var parts = [PurgeCopy.headline(s)]
            if s.unsubscribed > 0 { parts.append("\(s.unsubscribed) unsubscribed") }
            if s.blocked > 0 { parts.append("\(s.blocked) blocked") }
            if s.rules > 0 { parts.append("\(s.rules) new rules") }
            if let failed = PurgeCopy.failedNote(s.failed) { parts.append(failed) }
            return parts.joined(separator: ". ")
        }

        // MARK: the hero

        private func hero(_ summary: PurgeSummary) -> some View {
            let jobs = summary.unsubscribed + summary.running + summary.needsYou + summary.opened
            let settled = jobs == 0 ? 1 : CGFloat(jobs - summary.running) / CGFloat(jobs)
            return VStack(spacing: 0) {
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
                                    .opacity(pulse ? 0 : (celebrated ? 0.9 : 0))
                                    .animation(
                                        .easeOut(duration: 1.2).delay(Double(i) * 0.2), value: pulse)
                            }
                        }
                        Circle()
                            .fill(.black.opacity(0.35))
                            .frame(width: 58, height: 58)
                        // THE RING CLOSES AS THE WORK DOES: its arc is the
                        // share of unsubscribes that have settled, so the
                        // wait has a shape and no number.
                        Circle()
                            .trim(from: 0, to: max(0.04, settled))
                            .stroke(Self.phosphor, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                            .rotationEffect(.degrees(-90))
                            .frame(width: 58, height: 58)
                            .shadow(color: Self.phosphor.opacity(0.9), radius: 6)
                            .animation(.easeInOut(duration: 0.5), value: settled)
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
                .frame(height: 124)
                .frame(maxWidth: .infinity)
                .clipShape(UnevenRoundedRectangle(topLeadingRadius: 18, topTrailingRadius: 18))

                VStack(spacing: Space.s14) {
                    Text(PurgeCopy.headline(summary))
                        .font(Typo.hero(28))
                        .foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                        .contentTransition(.opacity)
                    HStack(spacing: 0) {
                        stat(summary.unsubscribed, "unsubscribed")
                        divider
                        stat(summary.blocked, "blocked")
                        divider
                        stat(summary.rules, summary.rules == 1 ? "new rule" : "new rules")
                    }
                }
                .padding(.horizontal, Space.s20)
                .padding(.vertical, Space.s16)
            }
            .background(RoundedRectangle(cornerRadius: 18, style: .continuous).fill(pagePaper))
            .overlay(
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .strokeBorder(Palette.hairlineStrong, lineWidth: 1)
            )
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

        // MARK: these need you

        private func needsYou(_ waiting: [(card: PurgeCard, status: PurgeStatus)]) -> some View {
            VStack(alignment: .leading, spacing: Space.s10) {
                HStack(spacing: Space.s8) {
                    Image(systemName: "hand.raised.fill")
                        .font(Typo.ui(12, weight: .semibold))
                        .foregroundStyle(Palette.warn)
                    Text(PurgeCopy.needsYouTitle)
                        .font(Typo.ui(14, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                }
                Text(PurgeCopy.needsYouLead)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(waiting, id: \.card.id) { card, status in
                    HStack(spacing: Space.s10) {
                        Avatar(sender: PurgeSenderPage.avatar(card), size: Space.scaled(26))
                        VStack(alignment: .leading, spacing: Space.s2) {
                            Text(PurgeSenderPage.name(card))
                                .font(Typo.ui(13, weight: .medium))
                                .foregroundStyle(Palette.ink)
                                .lineLimit(1)
                                .truncationMode(.tail)
                            Text(PurgeCopy.status(status))
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkDim)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        let opening = finish.opening.contains(card.id)
                        Button {
                            Task { await store.openPurgeRow(card.id) }
                        } label: {
                            HStack(spacing: Space.s4) {
                                Text(opening ? "Opening" : "Open their page")
                                    .font(Typo.ui(12, weight: .semibold))
                                Image(systemName: "arrow.up.right")
                                    .font(Typo.ui(9, weight: .bold))
                            }
                            .foregroundStyle(onAccent)
                            .padding(.horizontal, Space.s12)
                            .frame(height: Space.scaled(28))
                            .background(Capsule(style: .continuous).fill(Palette.accent))
                            .contentShape(Capsule())
                        }
                        .buttonStyle(.plain)
                        .disabled(opening)
                        .opacity(opening ? 0.6 : 1)
                        .help("Opens their unsubscribe page in your browser")
                        .accessibilityLabel("Open \(PurgeSenderPage.name(card))'s unsubscribe page")
                    }
                    .padding(.vertical, Space.s2)
                    .transition(.opacity)
                }
            }
            .padding(Space.s16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(Palette.warnSoft))
            // The whole border, evenly, flaring once when the list appears.
            .overlay(
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .strokeBorder(Palette.warn.opacity(alertGlow ? 1 : 0.55), lineWidth: alertGlow ? 2 : 1)
            )
            .shadow(color: Palette.warn.opacity(alertGlow ? 0.6 : 0), radius: 16)
            .transition(.opacity.combined(with: .scale(scale: 0.98)))
        }

        // MARK: every sender

        private func ledger(_ statuses: [(card: PurgeCard, status: PurgeStatus)]) -> some View {
            VStack(spacing: 0) {
                ForEach(Array(statuses.enumerated()), id: \.element.card.id) { i, entry in
                    if i > 0 { Rectangle().fill(Palette.hairline).frame(height: 1) }
                    HStack(spacing: Space.s10) {
                        Avatar(sender: PurgeSenderPage.avatar(entry.card), size: Space.scaled(22))
                        Text(PurgeSenderPage.name(entry.card))
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                            .truncationMode(.tail)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text(statusLine(entry.status))
                            .font(Typo.micro)
                            .foregroundStyle(statusInk(entry.status))
                            .lineLimit(1)
                            .truncationMode(.tail)
                            .frame(maxWidth: Typo.measure(230), alignment: .trailing)
                        StatusGlyph(status: entry.status, still: reduceMotion)
                    }
                    .padding(.horizontal, Space.s12)
                    .padding(.vertical, Space.s8)
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(
                        "\(PurgeSenderPage.name(entry.card)): \(PurgeCopy.status(entry.status))")
                }
            }
            .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(pagePaper))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Palette.hairline, lineWidth: 1)
            )
        }

        /// The board's short form: the reason a sender is waiting is said in
        /// the list above, not twice.
        private func statusLine(_ status: PurgeStatus) -> String {
            if case .needsYou = status { return "Needs you" }
            return PurgeCopy.status(status)
        }

        private func statusInk(_ status: PurgeStatus) -> Color {
            switch status {
            case .needsYou: Palette.warn
            case .failed: Palette.danger
            case .running: Palette.accent
            default: Palette.inkDim
            }
        }

        private func leave() {
            guard !leaving else { return }
            withAnimation(.easeIn(duration: 0.22)) { leaving = true }
            Task {
                try? await Task.sleep(for: .milliseconds(240))
                dismiss()
            }
        }
    }

    /// The mark at the end of a board row. A running job breathes; it does
    /// not spin.
    private struct StatusGlyph: View {
        let status: PurgeStatus
        let still: Bool
        @State private var breathe = false

        var body: some View {
            Group {
                switch status {
                case .running:
                    Circle()
                        .fill(Palette.accent)
                        .frame(width: 7, height: 7)
                        .scaleEffect(breathe ? 1.25 : 0.75)
                        .opacity(breathe ? 1 : 0.45)
                        .onAppear {
                            guard !still else { return }
                            withAnimation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true)) {
                                breathe = true
                            }
                        }
                case .unsubscribed, .blocked, .ruled:
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(Palette.positive)
                case .kept:
                    Image(systemName: "checkmark.circle").foregroundStyle(Palette.inkFaint)
                case .opened:
                    Image(systemName: "arrow.up.right.circle.fill").foregroundStyle(Palette.inkFaint)
                case .needsYou:
                    Image(systemName: "hand.raised.fill").foregroundStyle(Palette.warn)
                case .failed:
                    Image(systemName: "exclamationmark.circle.fill").foregroundStyle(Palette.danger)
                }
            }
            .font(Typo.ui(13))
            .frame(width: Space.scaled(18))
            .accessibilityHidden(true)
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
