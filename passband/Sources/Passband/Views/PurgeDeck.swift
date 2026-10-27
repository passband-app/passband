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

    /// A page in the CONTENT PANE: the app's page header, the content, a bar
    /// across the bottom. The same chrome as every routed page, because it
    /// is mounted where they are. Shared with the smart clear list, so the
    /// two read as one family.
    struct FullPage<Top: View, Content: View, Bottom: View>: View {
        let title: String
        var subtitle: String?
        @ViewBuilder var top: Top
        @ViewBuilder var content: Content
        @ViewBuilder var bottom: Bottom

        var body: some View {
            VStack(spacing: 0) {
                // The app's own page header, at the app's own bar height:
                // these are pages, and they sit beside the rail like one.
                RoutedHeader(title: title, subtitle: subtitle) { top }
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                Rectangle().fill(Palette.hairline).frame(height: 1)
                bottom
                    .padding(.horizontal, Space.scaled(22))
                    .frame(height: Space.scaled(54))
            }
            // NO BACKGROUND AND NO INSETS OF ITS OWN: this is mounted in the
            // content pane (RootView), on the window's backdrop, beside the
            // real rail, exactly as a routed page is. What a click on the
            // rail does mid-purge is the store's rule
            // (`AppStore.yieldPagesForNavigation`).
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
        /// Held on: the thing this button opened is showing.
        var lit = false
        var onHover: ((Bool) -> Void)?
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
                        .fixedSize()
                }
                // A button sharing a row gives its padding to its word.
                .padding(.horizontal, wide ? Space.s6 : Space.s16)
                .frame(maxWidth: wide ? .infinity : nil)
                .frame(height: Space.scaled(34))
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(
                            role == .primary
                                ? Palette.accent.opacity(hovering ? 0.88 : 1)
                                : (role == .danger
                                    ? Palette.dangerSoft
                                    : (lit ? Palette.accentSoft : Palette.hairline.opacity(hovering ? 1 : 0.6))))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(
                            role == .primary
                                ? Color.clear : (lit ? Palette.accent.opacity(0.5) : Palette.hairline),
                            lineWidth: 1)
                )
                .contentShape(RoundedRectangle(cornerRadius: 11, style: .continuous))
            }
            .buttonStyle(.plain)
            .onHover {
                hovering = $0
                onHover?($0)
            }
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
            var selection = 0
        }
        @State private var ghosts: [Ghost] = []
        @State private var arrive: PurgeArrive = .first
        @State private var ruleUI = PurgeRuleUI.fixture
        /// Which of the sender's emails is rendered. The newest, until moved.
        @State private var selection = PurgeFixture.selected

        private var deck: PurgeDeck { session.deck }
        private var anythingStaged: Bool { !session.summary.isEmpty }

        var body: some View {
            FullPage(title: "Purge", subtitle: "senders you have stopped reading") {
                PurgeTally(summary: session.summary)
            } content: {
                GeometryReader { geo in
                let compact = PurgeStrip.isCompact(width: Double(geo.size.width))
                ZStack {
                    if let card = deck.current {
                        PurgeSenderPage(
                            card: card, ledgerDays: session.ledgerDays,
                            emails: store.purgeEmails(for: card),
                            suggestions: store.purgeSuggestions[card.id], motion: nil,
                            ruleUI: $ruleUI,
                            actions: PurgeActions(
                                decide: decide, toggleRules: toggleRules,
                                openOther: { ruleUI.other = true },
                                closeOther: { ruleUI.other = false }, submitOther: submitOther),
                            selection: selection,
                            bodyOf: { store.purgeBodies[$0.id] },
                            failed: { store.purgeBodyFailures.contains($0.id) },
                            select: { selection = $0 },
                            compact: compact
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
                            selection: ghost.selection,
                            bodyOf: { store.purgeBodies[$0.id] }, compact: compact,
                            exit: ghost.exit, reduceMotion: reduceMotion
                        )
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                    }
                }
                .clipped()
                }
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
            // A new sender starts with the three choices, nothing open, and
            // their newest email.
            .onChange(of: deck.current?.id) { _, _ in
                ruleUI = PurgeRuleUI()
                selection = 0
            }
            // THE EMAIL ON SHOW, fetched when it becomes the one on show; and
            // the NEXT sender's newest, so its page arrives already drawn.
            // Read-only (`AppStore.loadPurgeBody`): this is not reading it.
            .task(id: PreviewKey(sender: deck.current?.id, selection: selection)) {
                guard let card = deck.current else { return }
                let emails = store.purgeEmails(for: card)
                if let i = PurgeStrip.shown(selection, count: emails.count) {
                    await store.loadPurgeBody(emails[i])
                }
                if let next = deck.next, let newest = store.purgeEmails(for: next).first {
                    await store.loadPurgeBody(newest)
                }
            }
            .animation(.easeOut(duration: 0.18), value: ruleUI.open)
            // Coming back to a parked purge: the expansion is folded (this
            // view is new) and a half-typed sentence is still there, one
            // press of Create rule and Other away.
            .onAppear {
                if PurgeFixture.rules == nil { ruleUI.text = session.otherText }
            }
            .onChange(of: ruleUI.text) { _, text in store.setPurgeOtherText(text) }
            // PRELOADED, so Create rule opens at once: this sender's
            // suggestions and the next one's, and no further. Each is a model
            // call against a shared daily cap, asked for once per sender for
            // the session (a step back with `z` finds them already here).
            .task(id: deck.current?.id) {
                guard let card = deck.current else { return }
                async let here: Void = store.loadPurgeSuggestions(for: card)
                if let next = deck.next { await store.loadPurgeSuggestions(for: next) }
                await here
            }
        }

        private struct PreviewKey: Hashable {
            var sender: String?
            var selection: Int
        }

        /// The arrows (and the app's list keys, j and k) walk the strip.
        private func step(_ delta: Int) {
            guard let card = deck.current else { return }
            selection = PurgeStrip.move(
                PurgeStrip.shown(selection, count: store.purgeEmails(for: card).count) ?? 0,
                by: delta, count: store.purgeEmails(for: card).count)
        }

        // MARK: keys

        /// HOLDS, NOT TAPS, for the decisions: a hold swallows its own
        /// autorepeat, so a key leaned on decides one sender and not a blur
        /// of them. `u` also answers when it has nothing to do, so it can
        /// never fall through to the global undo.
        ///
        /// THE DIGITS DECLINE unless a suggestion is on screen under that
        /// number: otherwise they are the rail's page keys, and what those do
        /// mid-purge is the store's rule, not something to swallow here.
        ///
        /// WHILE "OTHER" HAS THE KEYBOARD NONE OF THE LETTERS FIRE: the key
        /// monitor does not hand a bare-key binding to a text field. Escape
        /// always arrives, and backs out one step at a time.
        private var bindings: [KeyBinding] {
            [
                KeyBinding("u", "unsubscribe or block", onRelease: { _ in }) {
                    if let card = deck.current { decide(card.ending) }
                },
                KeyBinding("r", "create rule", onRelease: { _ in }) { toggleRules() },
                KeyBinding(declining: "1", "first rule") { pick(0) },
                KeyBinding(declining: "2", "second rule") { pick(1) },
                KeyBinding(declining: "3", "third rule") { pick(2) },
                KeyBinding("o", "your own rule") { if ruleUI.open { ruleUI.other = true } },
                KeyBinding("s", "keep", onRelease: { _ in }) { decide(.keep) },
                KeyBinding("ArrowDown", "next email") { step(+1) },
                KeyBinding("ArrowUp", "previous email") { step(-1) },
                KeyBinding("j", "next email") { step(+1) },
                KeyBinding("k", "previous email") { step(-1) },
                KeyBinding("z", "back one sender", onRelease: { _ in }) { back() },
                KeyBinding("Backspace", "back one sender", onRelease: { _ in }) { back() },
                KeyBinding("Delete", "back one sender", onRelease: { _ in }) { back() },
                KeyBinding("Escape", "back out one step") { leave() },
                // Return decides nothing here: the confirm page owns it.
                KeyBinding("Enter", "nothing") {},
                // The search panel would open under this page, unseen.
                KeyBinding("/", "nothing") {},
            ]
        }

        /// "Create rule": show the suggestions in place. They were fetched
        /// when this sender came up, so normally they are simply there; the
        /// loading state is for a press that beat the fetch. Pressed again,
        /// it folds them away (and keeps what was typed).
        private func toggleRules() {
            guard let card = deck.current, card.offersRules else { return }
            if ruleUI.open {
                ruleUI.open = false
                ruleUI.other = false
            } else {
                ruleUI.open = true
                // A sentence already typed is why they came back here.
                ruleUI.other = !ruleUI.text.isEmpty
            }
        }

        /// A suggestion by its number, only while its sentence is on screen.
        private func pick(_ index: Int) -> Bool {
            guard ruleUI.open, let card = deck.current,
                case .loaded(let found)? = store.purgeSuggestions[card.id],
                found.prefix(3).indices.contains(index)
            else { return false }
            decide(.rule(found[index]))
            return true
        }

        /// Return in the Other field: the person's own sentence, trimmed and
        /// within the limit, or nothing at all.
        private func submitOther() {
            guard let want = PurgeRuleText.accept(ruleUI.text) else { return }
            decide(.ownRule(want))
        }

        private func decide(_ decision: PurgeDecision) {
            guard let card = deck.current, card.allows(decision) else { return }
            arrive = decision == .keep ? .right : .under
            push(Ghost(card: card, exit: .decided(decision.kind), selection: selection))
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
            if let current = deck.current { push(Ghost(card: current, exit: .under, selection: selection)) }
            store.purgeBack()
            AccessibilityNotification.Announcement(
                "Back to \(PurgeSenderPage.name(previous.card))."
            ).post()
        }

        /// Esc never throws decisions away: with something staged it goes to
        /// the confirm page, which is where discarding is asked about.
        private func leave() {
            // One step at a time: out of the field, then out of the
            // suggestions, and only then out of the deck.
            if ruleUI.other {
                ruleUI.other = false
                return
            }
            if ruleUI.open {
                ruleUI.open = false
                return
            }
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

    /// Where the "Create rule" step is on the sender's page. Held by the
    /// deck, so the keys and the page agree, and reset with each sender.
    private struct PurgeRuleUI: Equatable {
        /// "Create rule" has been pressed: the suggestions are showing.
        var open = false
        /// "Other" has been pressed: the field is up.
        var other = false
        var text = ""

        /// What the launch environment asked the page to come up as.
        static var fixture: PurgeRuleUI {
            switch PurgeFixture.rules {
            case .open?, .loading?, PurgeFixture.Rules.none?: PurgeRuleUI(open: true)
            case .other?: PurgeRuleUI(open: true, other: true, text: PurgeFixture.typedRule)
            case nil: PurgeRuleUI()
            }
        }
    }

    /// What the page can ask of the deck. All nil on a ghost.
    private struct PurgeActions {
        var decide: (PurgeDecision) -> Void
        var toggleRules: () -> Void
        var openOther: () -> Void
        var closeOther: () -> Void
        var submitOther: () -> Void
    }

    /// One sender, drawn across the content area. Used for the page being
    /// decided on (with `actions`) and for the ghost of one leaving (with
    /// `motion`).
    private struct PurgeSenderPage: View {
        let card: PurgeCard
        let ledgerDays: Int
        let emails: [AttentionUpdate]
        let suggestions: AppStore.PurgeSuggestions?
        let motion: PurgeMotion?
        var ruleUI: Binding<PurgeRuleUI> = .constant(PurgeRuleUI())
        var actions: PurgeActions?
        /// Which of the sender's emails is rendered, and what it loaded as.
        var selection = 0
        var bodyOf: (AttentionUpdate) -> AppStore.PurgeBody? = { _ in nil }
        var failed: (AttentionUpdate) -> Bool = { _ in false }
        var select: ((Int) -> Void)?

        /// The choice the pointer is over, for the one line under the row.
        @State private var pointed: PurgeCopy.Choice?
        /// Whether the window is small enough that something has to give.
        /// Known from the FIRST layout (the page is laid out inside a
        /// geometry reader), so the rendered email is never laid out at one
        /// width and then reflowed at another.
        var compact = false

        static func name(_ card: PurgeCard) -> String {
            let given = card.sender.display_name.trimmingCharacters(in: .whitespaces)
            return given.isEmpty ? SenderCache.resolved(card.sender.sender).displayName : given
        }

        static func avatar(_ card: PurgeCard) -> String { "\(name(card)) <\(card.sender.sender)>" }

        var body: some View {
            HStack(alignment: .top, spacing: compact ? Space.s20 : Space.s32) {
                left
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                ScrollView {
                    right
                        .padding(.bottom, Space.s20)
                }
                .scrollIndicators(.never)
                // AT A SMALL WINDOW the strip folds to dates and this column
                // narrows; the rendered email keeps what is left.
                .frame(width: Typo.measure(compact ? 350 : 420))
                .opacity(fade)
            }
            .padding(.horizontal, Space.scaled(compact ? 20 : 28))
            .padding(.top, Space.s20)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(Self.name(card))
        }

        /// The right column and the header fade through an unsubscribe and a
        /// rule while the email rows do the moving.
        private var fade: Double {
            guard let motion else { return 1 }
            switch motion.exit {
            case .decided(.unsubscribe), .decided(.rule): return Double(1 - min(1, motion.t * 1.4))
            default: return 1
            }
        }

        // MARK: left: who, and what they send

        private var left: some View {
            VStack(alignment: .leading, spacing: Space.s14) {
                HStack(alignment: .center, spacing: Space.s14) {
                    Avatar(sender: Self.avatar(card), size: Space.scaled(52))
                        .overlay { stamp }
                    VStack(alignment: .leading, spacing: Space.s2) {
                        Text(Self.name(card))
                            .font(Typo.serif(26, weight: .medium))
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

                // THE MAIL ITSELF: a strip of this sender's emails in
                // Reading, newest first, and the selected one RENDERED beside
                // it as the reference. Arrow keys move down the strip.
                HStack(alignment: .top, spacing: Space.s12) {
                    strip
                        .frame(width: Typo.measure(compact ? 74 : 196))
                    PurgeEmailPane(
                        email: shown, body: shown.flatMap(bodyOf),
                        failed: shown.map(failed) ?? false, sender: card
                    )
                    .opacity(fade)
                }
                .padding(.bottom, Space.s16)
            }
        }

        private var shown: AttentionUpdate? {
            PurgeStrip.shown(selection, count: emails.count).map { emails[$0] }
        }

        /// The strip: one entry an email, in the app's list-row envelope
        /// (`ListRow`, so selection paints as it does in every list) and the
        /// mail list's own type and date stamp, cut to a narrow column. At a
        /// small window it is dates only.
        private var strip: some View {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 1) {
                        ForEach(Array(emails.enumerated()), id: \.element.id) { i, email in
                            PurgeStripEntry(
                                email: email, selected: i == (PurgeStrip.shown(selection, count: emails.count) ?? 0),
                                compact: compact
                            ) { select?(i) }
                            .id(email.id)
                            .modifier(Falling(index: i, motion: motion))
                        }
                        // The daemon's count is the authority on how many
                        // there are; the board may hold fewer.
                        let more = card.sender.open_count - emails.count
                        if more > 0, !emails.isEmpty {
                            Text(compact ? "+\(more)" : (more == 1 ? "and 1 more" : "and \(more) more"))
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaint)
                                .padding(.leading, Space.s8)
                                .padding(.top, Space.s6)
                                .opacity(fade)
                        }
                    }
                    .padding(.bottom, Space.s16)
                }
                .scrollIndicators(.never)
                // A ghost's entries fall out of the strip, not behind its edge.
                .scrollClipDisabled(motion != nil)
                .onChange(of: selection) { _, _ in
                    if let shown { proxy.scrollTo(shown.id) }
                }
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
                PurgeEvidencePanel(evidence: PurgeEvidence(card.sender), ledgerDays: ledgerDays)
                choices
                if ruleUI.wrappedValue.open { rules }
            }
        }

        /// ONE ROW, LEFT TO RIGHT, a word or two each: the way out, a rule,
        /// keep. Under it one short line, which says only what a button
        /// cannot: why this is a block, that this unsubscribe ends on their
        /// site, or what the choice under the pointer does.
        private var choices: some View {
            VStack(alignment: .leading, spacing: Space.s8) {
                HStack(spacing: Space.s8) {
                    PageButton(
                        key: "u", label: PurgeCopy.title(card.ending), role: .primary, wide: true,
                        onHover: { point(.ending, $0) }
                    ) { actions?.decide(card.ending) }
                    if card.offersRules {
                        PageButton(
                            key: "r", label: PurgeCopy.title(.rule("")), wide: true,
                            lit: ruleUI.wrappedValue.open, onHover: { point(.rule, $0) }
                        ) { actions?.toggleRules() }
                    }
                    PageButton(
                        key: "s", label: PurgeCopy.title(.keep), wide: true,
                        onHover: { point(.keep, $0) }
                    ) { actions?.decide(.keep) }
                }
                let note = PurgeCopy.note(pointed, on: card)
                Text(note.isEmpty ? " " : note)
                    .font(Typo.micro)
                    .foregroundStyle(
                        pointed == nil && card.ending == .block ? Palette.warn : Palette.inkDim)
                    .padding(.horizontal, Space.s2)
                    .animation(.easeOut(duration: 0.12), value: pointed)
                    .accessibilityHidden(note.isEmpty)
            }
        }

        private func point(_ choice: PurgeCopy.Choice, _ over: Bool) {
            if over { pointed = choice } else if pointed == choice { pointed = nil }
        }

        /// AFTER "Create rule": the daemon's suggestions and Other, in place.
        /// A calm placeholder for the second the suggestions take, and when
        /// none come back, just Other.
        private var rules: some View {
            let ui = ruleUI.wrappedValue
            return VStack(alignment: .leading, spacing: Space.s8) {
                switch suggestions {
                case .loading?, nil:
                    Text(PurgeCopy.rulesLoading)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                    ForEach(0..<2, id: \.self) { _ in
                        RoundedRectangle(cornerRadius: 11, style: .continuous)
                            .fill(Palette.hairline.opacity(0.45))
                            .frame(height: Space.scaled(34))
                    }
                case .loaded(let found)?:
                    ForEach(Array(found.prefix(3).enumerated()), id: \.offset) { i, want in
                        PurgeRuleChoice(key: "\(i + 1)", want: want) { actions?.decide(.rule(want)) }
                    }
                }
                if ui.other {
                    PurgeOtherField(
                        text: Binding(
                            get: { ruleUI.wrappedValue.text }, set: { ruleUI.wrappedValue.text = $0 }),
                        live: actions != nil,
                        submit: { actions?.submitOther() }, cancel: { actions?.closeOther() })
                } else {
                    PurgeRuleChoice(key: "o", want: PurgeCopy.otherTitle, quiet: true) {
                        actions?.openOther()
                    }
                }
                Text(PurgeCopy.rulesNote)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .transition(.opacity)
        }
    }

    /// One email in the strip. The date leads (it is what differs most at a
    /// glance down one sender's mail), the subject under it.
    private struct PurgeStripEntry: View {
        let email: AttentionUpdate
        let selected: Bool
        let compact: Bool
        let action: () -> Void

        private var line: String {
            let subject = email.subject?.displaySubject.flattenedLine(cap: 200) ?? ""
            return subject.isEmpty ? Reading.cleanSummary(email.one_line) : subject
        }

        var body: some View {
            ListRow(
                selected: selected, cornerRadius: 8, hPadding: Space.s8, vPadding: Space.s6,
                action: action
            ) { selected, _ in
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(MailList.stamp(email.received_at))
                        .font(Typo.ui(11.5, weight: selected ? .semibold : .regular))
                        .monospacedDigit()
                        .foregroundStyle(selected ? Palette.ink : Palette.inkFaint)
                        .lineLimit(1)
                    if !compact {
                        Text(verbatim: line)
                            .font(Typo.ui(12.5))
                            .foregroundStyle(selected ? Palette.ink : Palette.inkDim)
                            .lineLimit(2)
                            .multilineTextAlignment(.leading)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .help(line)
            .accessibilityLabel("\(MailList.stamp(email.received_at)), \(line)")
            .accessibilityAddTraits(selected ? .isSelected : [])
        }
    }

    /// THE SELECTED EMAIL, RENDERED, AS A REFERENCE. The app's own renderer
    /// (`EmailWebView`, the reader's), so images and trackers follow the
    /// person's settings exactly as they do in the reader.
    ///
    /// IT IS NOT A READER, and four things keep it from acting like one:
    /// - Nothing here marks the email opened, times it or counts a click:
    ///   the body comes from the read-only thread fetch and the renderer is
    ///   given no message to report on. Looking at a sender you ignore must
    ///   not make them look read.
    /// - LINKS ARE INERT (`inertLinks`), and the frame takes no pointer at
    ///   all: an unsubscribe link clicked here would go round the flow's own
    ///   staged unsubscribe.
    /// - Taking no pointer also means it can never take the KEYBOARD: a web
    ///   view that is clicked becomes first responder, and the purge's keys
    ///   would stop.
    /// - It scrolls inside its own region (the scroll view around the frame
    ///   does the scrolling); the page does not grow.
    private struct PurgeEmailPane: View {
        let email: AttentionUpdate?
        let body_: AppStore.PurgeBody?
        let failed: Bool
        let sender: PurgeCard

        init(email: AttentionUpdate?, body: AppStore.PurgeBody?, failed: Bool, sender: PurgeCard) {
            self.email = email
            body_ = body
            self.failed = failed
            self.sender = sender
        }

        private var summary: String {
            email.map { Reading.cleanSummary($0.one_line) } ?? ""
        }

        var body: some View {
            ScrollView {
                content
                    .frame(maxWidth: .infinity, alignment: .topLeading)
            }
            .scrollIndicators(.automatic)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(pagePaper))
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(Palette.hairline, lineWidth: 1)
            )
            // A new document for a new email, scrolled back to its top.
            .id(email?.id)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Preview of the selected email. \(summary)")
        }

        @ViewBuilder private var content: some View {
            if email == nil {
                note(
                    sender.sender.open_count == 1
                        ? "1 email of theirs is in Reading."
                        : "\(sender.sender.open_count) emails of theirs are in Reading.", nil)
            } else if failed || body_ == .failed {
                // The plain fallback: what it was about, as text.
                note("This email could not be shown here. What it says:", summary)
            } else {
                switch body_ {
                case .html(let html, let allowTrackers, let attachments)?:
                    // UNKEYED: not pooled and no height filed, so nothing
                    // measured in this pane is ever handed to the reader's
                    // frame of the same message. The body was prepared before
                    // this mounted (`AppStore.warmPreview`).
                    EmailWebView(
                        html: html, cacheKey: nil, allowTrackers: allowTrackers,
                        attachments: attachments, textHeight: 420, inertLinks: true
                    )
                    // No pointer: no link, no hover, no first responder.
                    .allowsHitTesting(false)
                case .text(let text)?:
                    Text(verbatim: text)
                        .font(Typo.row)
                        .foregroundStyle(Palette.ink)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(Space.s20)
                case .loading?, nil, .failed?:
                    // Calm: the shape of a page, holding still.
                    VStack(alignment: .leading, spacing: Space.s12) {
                        ForEach([0.5, 1, 0.92, 0.97, 0.6], id: \.self) { share in
                            GeometryReader { geo in
                                RoundedRectangle(cornerRadius: 5, style: .continuous)
                                    .fill(Palette.hairline.opacity(0.6))
                                    .frame(width: geo.size.width * share)
                            }
                            .frame(height: share == 0.5 ? Space.scaled(20) : Space.scaled(11))
                        }
                    }
                    .padding(Space.s20)
                }
            }
        }

        private func note(_ lead: String, _ text: String?) -> some View {
            VStack(alignment: .leading, spacing: Space.s8) {
                Text(lead)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                if let text, !text.isEmpty {
                    Text(verbatim: text)
                        .font(Typo.serif(18))
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(Space.s20)
        }
    }

    /// A suggested rule: its number and the sentence, VERBATIM. It is
    /// model-written from sender-written subjects, so it is shown whole (it
    /// wraps rather than truncates) before the key that saves it is pressed.
    private struct PurgeRuleChoice: View {
        let key: String
        let want: String
        /// "Other": the same shape, in a quieter ink, because it is a door
        /// and not a sentence.
        var quiet = false
        let action: () -> Void
        @State private var hovering = false

        var body: some View {
            Button(action: action) {
                HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
                    Kbd(key)
                    Text(verbatim: want)
                        .font(Typo.rowSub)
                        .foregroundStyle(quiet ? Palette.inkDim : Palette.ink)
                        .multilineTextAlignment(.leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(.horizontal, Space.s12)
                .padding(.vertical, Space.scaled(9))
                .background(
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .fill(hovering ? Palette.accentSoft : (quiet ? Color.clear : pagePaper))
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
            .accessibilityLabel(quiet ? "\(want), write your own rule" : "Rule \(key): \(want)")
        }
    }

    /// OTHER: one line for the person's own sentence. Return accepts it, Esc
    /// backs out of the field (not out of the purge). While it has the
    /// keyboard the page's single-letter keys stand down by themselves: the
    /// key monitor does not fire a bare-key binding into a text field.
    private struct PurgeOtherField: View {
        @Binding var text: String
        /// False on a ghost, which must not take the keyboard.
        let live: Bool
        let submit: () -> Void
        let cancel: () -> Void
        @FocusState private var focused: Bool

        private var usable: Bool { PurgeRuleText.accept(text) != nil }

        var body: some View {
            HStack(spacing: Space.s8) {
                TextField("", text: $text, prompt: Text(PurgeCopy.otherPrompt))
                    .textFieldStyle(.plain)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.ink)
                    .focused($focused)
                    .onSubmit { if usable { submit() } }
                    .onChange(of: text) { _, new in
                        // Held to the limit as it is typed, so what is on
                        // screen is what will be staged.
                        if new.count > PurgeRuleText.limit {
                            text = String(new.prefix(PurgeRuleText.limit))
                        }
                    }
                    .disabled(!live)
                Kbd("return")
                    .opacity(usable ? 1 : 0.35)
                Button(action: cancel) {
                    Kbd("esc")
                }
                .buttonStyle(.plain)
                .help("Back to the suggestions")
                .accessibilityLabel("Cancel")
            }
            .padding(.horizontal, Space.s12)
            .padding(.vertical, Space.scaled(8))
            .background(RoundedRectangle(cornerRadius: 11, style: .continuous).fill(pagePaper))
            // Lit all the way round while it has the keyboard.
            .overlay(
                RoundedRectangle(cornerRadius: 11, style: .continuous)
                    .strokeBorder(Palette.accent.opacity(0.7), lineWidth: 1)
            )
            .onAppear { if live { focused = true } }
        }
    }

    // MARK: - the evidence, drawn

    /// "YOUR READING" AS A PICTURE. The open rate is the centre: a ring that
    /// is the whole of what this sender sent, split into what was opened,
    /// what was cleared without opening and what is still sitting there, with
    /// the opened count in the middle. Under it, how far into a typical one
    /// the reading got, as a bar against the email's length; and how much
    /// arrives, as a month of days.
    ///
    /// EVERY MARK IS A NUMBER THE DAEMON SENT (`PurgeEvidence`); a figure it
    /// did not send draws nothing. Identity is never colour alone: each arc
    /// has its name and its count beside it, in ink, and the whole panel
    /// reads out as numbers.
    private struct PurgeEvidencePanel: View {
        let evidence: PurgeEvidence
        let ledgerDays: Int

        // One hue for the thing being measured, neutrals for the rest: this
        // is a part of a whole, not three categories competing.
        private static var openedInk: Color { Palette.accent }
        private static var clearedInk: Color { Palette.inkFaint }
        private static var untouchedInk: Color { Palette.hairlineStrong }

        var body: some View {
            VStack(alignment: .leading, spacing: Space.s14) {
                HStack(spacing: Space.s6) {
                    Text("Your reading")
                        .font(Typo.sectionLabel)
                        .textCase(.uppercase)
                        .foregroundStyle(Palette.accent)
                    Spacer(minLength: Space.s8)
                    // Once, quietly: what span every number here is about.
                    Text(PurgeCopy.window(ledgerDays: ledgerDays))
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }
                if evidence.hasOpenRate { openRate }
                if let label = evidence.readLabel, let fraction = evidence.readFraction {
                    readDepth(label, fraction)
                }
                if let label = evidence.volumeLabel, let perMonth = evidence.perMonth {
                    volume(label, perMonth)
                }
                if evidence.neverClicked {
                    Text("Never clicked a link")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkDim)
                        .padding(.horizontal, Space.s8)
                        .padding(.vertical, Space.scaled(3))
                        .background(Capsule(style: .continuous).fill(Palette.hairline))
                }
            }
            .padding(Space.s16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(pagePaper))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Palette.hairline, lineWidth: 1)
            )
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Your reading. \(evidence.spoken(ledgerDays: ledgerDays))")
        }

        // MARK: opened of received

        private var openRate: some View {
            HStack(alignment: .center, spacing: Space.s20) {
                ZStack {
                    OpenRing(shares: evidence.shares)
                    VStack(spacing: -2) {
                        Text("\(evidence.opened)")
                            .font(Typo.serif(30, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .monospacedDigit()
                        Text("of \(evidence.received) opened")
                            .font(Typo.ui(9.5, weight: .medium))
                            .foregroundStyle(Palette.inkFaint)
                    }
                }
                .frame(width: Space.scaled(112), height: Space.scaled(112))
                VStack(alignment: .leading, spacing: Space.s8) {
                    legend(Self.openedInk, "Opened", evidence.opened)
                    legend(Self.clearedInk, "Cleared unopened", evidence.clearedUnopened)
                    legend(Self.untouchedInk, "Left unopened", evidence.untouched)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }

        private func legend(_ ink: Color, _ name: String, _ count: Int) -> some View {
            HStack(spacing: Space.s8) {
                RoundedRectangle(cornerRadius: 2, style: .continuous)
                    .fill(ink)
                    .frame(width: 9, height: 9)
                Text(name)
                    .font(Typo.rowSub)
                    .foregroundStyle(count > 0 ? Palette.inkDim : Palette.inkFaintest)
                Spacer(minLength: Space.s8)
                Text("\(count)")
                    .font(Typo.num(12, weight: .semibold))
                    .foregroundStyle(count > 0 ? Palette.ink : Palette.inkFaintest)
            }
        }

        /// The ring: three arcs of one circle, a sliver of the surface
        /// between them, starting at twelve o'clock with the opened ones.
        private struct OpenRing: View {
            let shares: (opened: Double, cleared: Double, untouched: Double)
            private static let width: CGFloat = 11

            var body: some View {
                let parts: [(Double, Color)] = [
                    (shares.opened, PurgeEvidencePanel.openedInk),
                    (shares.cleared, PurgeEvidencePanel.clearedInk),
                    (shares.untouched, PurgeEvidencePanel.untouchedInk),
                ]
                let live = parts.filter { $0.0 > 0 }.count
                // The gap between arcs, as a share of the circle. None when
                // one arc is the whole ring: there is nothing to part.
                let gap = live > 1 ? 0.012 : 0
                ZStack {
                    ForEach(0..<3, id: \.self) { i in
                        let start = parts.prefix(i).map(\.0).reduce(0, +)
                        let share = parts[i].0
                        if share > 0 {
                            Circle()
                                .trim(
                                    from: start + gap / 2,
                                    to: max(start + gap / 2 + 0.004, start + share - gap / 2)
                                )
                                .stroke(
                                    parts[i].1, style: StrokeStyle(lineWidth: Self.width, lineCap: .butt)
                                )
                                .rotationEffect(.degrees(-90))
                        }
                    }
                }
                .padding(Self.width / 2)
            }
        }

        // MARK: how far in

        /// The whole bar is a typical email's length; the lit part is how
        /// far down it the reading got.
        private func readDepth(_ label: String, _ fraction: Double) -> some View {
            VStack(alignment: .leading, spacing: Space.s6) {
                Text(label)
                    .font(Typo.ui(12, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(Palette.hairlineStrong)
                        Capsule().fill(Self.openedInk)
                            .frame(width: max(geo.size.height, geo.size.width * CGFloat(fraction)))
                    }
                }
                .frame(height: Space.scaled(8))
                HStack {
                    Text("start")
                    Spacer()
                    if let caption = evidence.readCaption { Text(caption) }
                    Spacer()
                    Text("end")
                }
                .font(Typo.ui(9.5, weight: .medium))
                .foregroundStyle(Palette.inkFaintest)
            }
        }

        // MARK: how much

        /// A month of days, with about that many carrying their mail.
        private func volume(_ label: String, _ perMonth: Double) -> some View {
            let days = PurgeMarks.days(perMonth: perMonth)
            return VStack(alignment: .leading, spacing: Space.s6) {
                Text(label)
                    .font(Typo.ui(12, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                HStack(spacing: Space.scaled(3)) {
                    ForEach(0..<days.total, id: \.self) { i in
                        RoundedRectangle(cornerRadius: 2, style: .continuous)
                            .fill(i < days.lit ? Self.openedInk.opacity(0.85) : Palette.hairlineStrong)
                            .frame(maxWidth: .infinity)
                            .frame(height: Space.scaled(12))
                    }
                }
            }
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
        let emails: [AttentionUpdate]
        let suggestions: AppStore.PurgeSuggestions?
        /// The email that was on show when the page left, so it leaves
        /// looking as it did.
        let selection: Int
        let bodyOf: (AttentionUpdate) -> AppStore.PurgeBody?
        let compact: Bool
        let exit: PurgeExit
        let reduceMotion: Bool

        @State private var t: CGFloat = 0
        @State private var mark: CGFloat = 0

        private func page(moving: Bool) -> some View {
            PurgeSenderPage(
                card: card, ledgerDays: ledgerDays, emails: emails, suggestions: suggestions,
                motion: moving ? PurgeMotion(exit: exit, t: t, mark: mark) : nil,
                selection: selection, bodyOf: bodyOf, compact: compact)
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
            FullPage(title: "Purge", subtitle: "senders you have stopped reading") {
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
            // Held so the global undo stays out. The rail's page keys are NOT
            // held: what leaving does is the store's rule.
            let inert = ["u", "s", "/"].map { KeyBinding($0, "nothing") {} }
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
            FullPage(title: "Purge", subtitle: "senders you have stopped reading") {
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
            ] + ["u", "s", "z", "/"].map { KeyBinding($0, "nothing") {} })
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
                case .kept, .alreadyDone:
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

    // MARK: - a parked purge

    /// THE WAY BACK, ON EVERY PAGE. A purge left with choices staged is
    /// parked, and this travels with the person in the corner the undo chips
    /// use, so it is not forgotten on the mail page. It says how many
    /// choices are waiting and that nothing has been sent; Resume goes back
    /// to the sender they left; the ✕ asks once before throwing the choices
    /// away.
    ///
    /// It also keeps the session's clock: a parked purge untouched for half
    /// an hour is dropped (`AppStore.expirePurgeIfStale`), checked against
    /// the WALL clock on a slow tick, so a Mac that slept through the
    /// deadline drops it on waking rather than a sleep-length later.
    struct PurgeParkedChip: View {
        @Environment(AppStore.self) private var store
        let chosen: Int
        @State private var asking = false

        var body: some View {
            HStack(spacing: Space.s10) {
                Image(systemName: "scissors")
                    .font(Typo.ui(11, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                Text(asking ? PurgeCopy.discardAsk(chosen) : PurgeCopy.parked(chosen))
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.ink)
                    .fixedSize()
                if asking {
                    chipButton("Discard", ink: Palette.danger) { store.discardParkedPurge() }
                    chipButton("Keep", ink: Palette.ink) { asking = false }
                } else {
                    chipButton("Resume", ink: Palette.accentInk) { store.resumePurge() }
                    Button {
                        asking = true
                    } label: {
                        Image(systemName: "xmark")
                            .font(Typo.ui(9, weight: .semibold))
                            .foregroundStyle(Palette.inkFaint)
                            .frame(width: Space.scaled(18), height: Space.scaled(18))
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("Discard these choices")
                    .accessibilityLabel("Discard these choices")
                }
            }
            .padding(.leading, Space.s12)
            .padding(.trailing, Space.s8)
            .padding(.vertical, Space.scaled(6))
            .glassEffect(
                .regular.tint(Palette.accent.opacity(0.16)),
                in: .rect(cornerRadius: 11, style: .continuous)
            )
            .animation(.easeOut(duration: 0.15), value: asking)
            .task {
                while !Task.isCancelled {
                    store.expirePurgeIfStale()
                    try? await Task.sleep(for: .seconds(20))
                }
            }
            .accessibilityElement(children: .contain)
        }

        private func chipButton(_ label: String, ink: Color, action: @escaping () -> Void) -> some View {
            Button(action: action) {
                Text(label)
                    .font(Typo.ui(11.5, weight: .semibold))
                    .foregroundStyle(ink)
                    .padding(.horizontal, Space.s10)
                    .padding(.vertical, Space.scaled(3))
                    .background(Capsule(style: .continuous).fill(Palette.hairline))
                    .contentShape(Capsule())
            }
            .buttonStyle(.plain)
        }
    }
#endif
