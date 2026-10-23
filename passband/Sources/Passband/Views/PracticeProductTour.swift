import SwiftUI

/// A companion to the real UI, never a modal keyboard context. Reader e/t,
/// board navigation, and the actual undo stack continue to work underneath it.
struct PracticeProductTour: View {
    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.displayScale) private var displayScale
    @State private var frame: CGRect = .zero
    @State private var cardHeight: CGFloat = 270
    /// The Next button, in the tour space: what the arrow points at when a
    /// lesson has nothing on the board to click.
    @State private var nextButton: CGRect?

    private var tour: TourController { store.tour }
    private var step: PracticeTourStep { tour.practiceStep }
    private let cardWidth: CGFloat = 390

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .topLeading) {
                ForEach(Array(ringRects.enumerated()), id: \.offset) { _, ring in
                    RoundedRectangle(cornerRadius: 16)
                        .strokeBorder(Palette.accent, lineWidth: 2)
                        .shadow(color: Palette.accent.opacity(0.25), radius: 14)
                        .frame(width: ring.width, height: ring.height)
                        .offset(x: ring.minX, y: ring.minY)
                        .allowsHitTesting(false)
                }
                if store.ruleEditor == nil, let arrow = clickArrow(in: geometry.size) {
                    ClickArrow(direction: arrow.direction, reduceMotion: reduceMotion)
                        .id(arrow.direction)
                        .frame(width: ClickArrow.size, height: ClickArrow.size)
                        .offset(x: arrow.origin.x, y: arrow.origin.y)
                        .allowsHitTesting(false)
                        .transition(.opacity)
                }
                if store.ruleEditor == nil {
                    guideCard
                        .id(step)
                        .frame(width: min(cardWidth, max(240, geometry.size.width - 32)))
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { cardHeight = $0 }
                        .offset(cardOrigin(in: geometry.size))
                        .transition(reduceMotion ? .opacity : .opacity.combined(with: .offset(y: 8)))
                }
            }
        }
        .onGeometryChange(for: CGRect.self) { $0.frame(in: .named(tourSpace)) } action: { frame = $0 }
        .animation(reduceMotion ? nil : .smooth(duration: 0.2), value: ringRects)
        .animation(reduceMotion ? nil : .smooth(duration: 0.2), value: clickRect)
        .animation(reduceMotion ? nil : .easeOut(duration: 0.24), value: step)
        .onChange(of: store.threadId) { _, _ in reconcile() }
        .onChange(of: store.openThreadSummary) { _, _ in reconcile() }
        .onChange(of: store.undos.map(\.id)) { _, _ in reconcile() }
        .onChange(of: store.resolvedIds) { previous, current in
            // Only a new successful restore advances the lesson. Revisiting
            // Undo with Back should show its completed state, not bounce ahead.
            if tour.active, step == .undo, tour.practiceDoneID != nil,
               previous.contains(1), !current.contains(1) {
                tour.advancePractice()
            }
        }
        .onChange(of: step) { _, next in prepare(next) }
        .onAppear { prepare(step) }
        .keyBindings(.global, [
            KeyBinding(declining: "ArrowRight", "next tour step", meta: true) {
                guard store.ruleEditor == nil else { return false }
                next()
                return true
            },
            KeyBinding(declining: "ArrowLeft", "previous tour step", meta: true) {
                guard store.ruleEditor == nil else { return false }
                tour.back()
                return true
            },
            KeyBinding(declining: "Escape", "leave guided tour", meta: true) {
                guard store.ruleEditor == nil else { return false }
                tour.skip()
                return true
            },
        ])
    }

    private var guideCard: some View {
        VStack(alignment: .leading, spacing: Space.s12) {
            HStack {
                Label("PASSBAND GUIDE", systemImage: "sparkles")
                    .font(Typo.sectionLabel)
                    .tracking(1)
                    .foregroundStyle(Palette.accentInk)
                Spacer()
                Text("\(step.rawValue + 1) / \(PracticeTourStep.allCases.count)")
                    .font(Typo.num(11))
                    .foregroundStyle(Palette.inkFaint)
            }
            Text(guideTitle)
                .font(Typo.serif(28, weight: .medium))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            explanationText(guideExplanation)
                .font(Typo.ui(15))
                .lineSpacing(3)
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
            keyboardHint
            if step == .ruleSaved, tour.practiceRuleSaved {
                Label("Your Brightly rule is saved in this practice inbox.", systemImage: "checkmark.circle.fill")
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.positive)
            }
            primaryAction
            HStack(spacing: Space.scaled(5)) {
                ForEach(PracticeTourStep.allCases, id: \.rawValue) { item in
                    Capsule()
                        .fill(item.rawValue <= step.rawValue ? Palette.accent : Palette.ink.opacity(0.1))
                        .frame(height: 3)
                }
            }
            .accessibilityHidden(true)
            HStack(spacing: Space.s14) {
                Button("Back") { tour.back() }
                    .disabled(step == .welcome)
                    .help("Previous step · ⌘←")
                Spacer()
                Button("Skip tour") { tour.skip() }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.inkFaintest)
                    .help("Leave the guided tour · ⌘Esc")
                if step != .wrap && step != .welcome {
                    if step.isInteraction && !(step == .undo && undoWasRestored) {
                        Button("Skip step") { next() }
                            .foregroundStyle(Palette.inkFaint)
                            .help("Skip this interaction · ⌘→")
                    } else {
                        Button("Next") { next() }
                            .buttonStyle(.borderedProminent)
                            .tint(Palette.accent)
                            .font(Typo.ui(14, weight: .semibold))
                            .controlSize(.large)
                            .help("Next step · ⌘→")
                            .onGeometryChange(for: CGRect.self) { $0.frame(in: .named(tourSpace)) } action: {
                                nextButton = $0
                            }
                    }
                }
            }
            .buttonStyle(.textAction)
            .font(Typo.micro)
        }
        .padding(Space.s20)
        .background(Color(light: 0xF7FAFD, dark: 0x293747), in: RoundedRectangle(cornerRadius: 20))
        .overlay {
            RoundedRectangle(cornerRadius: 20).strokeBorder(Palette.accent.opacity(0.24))
        }
        .shadow(color: .black.opacity(0.22), radius: 24, y: 10)
    }

    @ViewBuilder
    private var primaryAction: some View {
        if step == .welcome {
            actionButton("Show me around", symbol: "arrow.right") { tour.advancePractice() }
        } else if step == .wrap {
            actionButton("Explore my inbox", symbol: "arrow.right") { tour.completePractice() }
        }
    }

    private func actionButton(_ title: String, symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: symbol)
                .font(Typo.ui(13, weight: .semibold))
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, Space.s4)
        }
        .buttonStyle(.borderedProminent)
        .tint(Palette.accent)
    }

    @ViewBuilder
    private var keyboardHint: some View {
        #if os(macOS)
        switch step {
        case .done:
            guideKeyHint("e", "mark done and return to the board")
        case .undo:
            if !undoWasRestored { guideKeyHint("u", "undo the last action") }
        case .rule:
            guideKeyHint("t", "write a sender rule")
        case .calendar, .shipments:
            if readingRecord { guideKeyHint("esc", "return to your board") }
        default:
            EmptyView()
        }
        #endif
    }

    /// The copy with each `{key}` drawn as the same keycap the hint row uses,
    /// and `**Section**` runs set in full ink at semibold, so a section name
    /// reads as a place in the app against the dimmed sentence around it.
    /// Text cannot host a bordered view, but it can interpolate an Image, so
    /// the cap is rasterized and the sentence still wraps as one paragraph.
    private func explanationText(_ copy: String) -> Text {
        var text = Text(verbatim: "")
        var run = ""
        var bold = false
        func flush() {
            guard !run.isEmpty else { return }
            let piece = Text(verbatim: run)
            text = Text("\(text)\(bold ? piece.fontWeight(.semibold).foregroundStyle(Palette.ink) : piece)")
            run = ""
        }
        var rest = Substring(copy)
        while let first = rest.first {
            if rest.hasPrefix("**") {
                flush()
                bold.toggle()
                rest = rest.dropFirst(2)
            } else if first == "{", let close = rest.firstIndex(of: "}") {
                flush()
                let key = String(rest[rest.index(after: rest.startIndex)..<close])
                text = Text("\(text)\(inlineKeycap(key))")
                rest = rest[rest.index(after: close)...]
            } else {
                run.append(first)
                rest = rest.dropFirst()
            }
        }
        flush()
        return text
    }

    private func inlineKeycap(_ key: String) -> Text {
        let renderer = ImageRenderer(content: Kbd(key, size: 11)
            .environment(\.colorScheme, colorScheme))
        renderer.scale = displayScale
        #if os(macOS)
        let image = renderer.nsImage.map { Image(nsImage: $0) }
        #else
        let image = renderer.uiImage.map { Image(uiImage: $0) }
        #endif
        guard let image else { return Text(verbatim: key) }
        // Center the cap on the x-height of the 15pt copy around it.
        return Text(image).baselineOffset(-2.5).accessibilityLabel(key == "esc" ? "Escape" : key)
    }

    private func guideKeyHint(_ key: String, _ label: String) -> some View {
        HStack(spacing: Space.s8) {
            Kbd(key).scaleEffect(1.15)
            Text(label).font(Typo.ui(13)).foregroundStyle(Palette.inkDim)
        }
    }

    private var readingRecord: Bool {
        tour.practiceRecordThreadID != nil && tour.practiceRecordThreadID == store.threadId
    }

    private var guideTitle: String {
        if step == .ruleSaved {
            if tour.practiceRuleMuted {
                return "Now you’ll never see a Brightly terms-of-service update again."
            }
            return tour.practiceRuleSaved ? "Your rule is saved." : "You’re in control."
        }
        if readingRecord { return "Take a look, then head back." }
        if step == .undo && undoWasRestored { return "Right back where it belongs." }
        return step.title
    }

    private var guideExplanation: String {
        if step == .ruleSaved {
            if tour.practiceRuleMuted {
                return "You can change it at any time. Try “Always show me product updates” or “Only show me discounts 25% off or more.”"
            }
            if !tour.practiceRuleSaved {
                return "Smart rules let you choose how a sender’s mail is handled. You can try one whenever you’re ready."
            }
        }
        if readingRecord {
            return "Here’s the original email. Press {esc} when you’re ready to head back."
        }
        if step == .undo && undoWasRestored {
            return "Maya’s message is back on your board. You’ve tried both finishing a message and bringing it back."
        }
        return step.explanation
    }

    private var ringRects: [CGRect] {
        guard store.threadId == nil, store.activeView == .sitrep else { return [] }
        return step.targets.compactMap { target in
            guard let raw = tour.targets[target] else { return nil }
            let local = raw.offsetBy(dx: -frame.minX, dy: -frame.minY)
            let rect = (step == .openBrightly || step == .openMaya) ? local.insetBy(dx: -4, dy: -4) : local
            let visible = rect.intersection(CGRect(origin: .zero, size: frame.size))
            guard !visible.isNull, visible.width > 50, visible.height > ((step == .openBrightly || step == .openMaya) ? 16 : 35) else { return nil }
            return visible
        }
    }

    /// The click target for this lesson in overlay coordinates, if it is on
    /// screen. Board targets only count while the board is showing; the
    /// reader's verbs and the undo toast are measured wherever they are.
    private var clickRect: CGRect? {
        guard let target = step.clickTarget else { return nil }
        // Done once restored: nothing left to click.
        if step == .undo && undoWasRestored { return nil }
        // Reading a calendar record, the way on is esc, not another click.
        if readingRecord { return nil }
        let onBoard: Bool
        switch target {
        case .readerDone, .readerRule, .undoToast: onBoard = false
        default: onBoard = true
        }
        if onBoard, store.threadId != nil || store.activeView != .sitrep { return nil }
        if target == .undoToast, store.undos.isEmpty { return nil }
        if target == .readerDone || target == .readerRule, store.threadId == nil { return nil }
        guard let raw = tour.targets[target] else { return nil }
        let local = raw.offsetBy(dx: -frame.minX, dy: -frame.minY)
        let visible = local.intersection(CGRect(origin: .zero, size: frame.size))
        guard !visible.isNull, visible.width > 4, visible.height > 4 else { return nil }
        return visible
    }

    /// The card never moves: bottom left, clear of the rail and any undo
    /// toast. Chasing the highlight made it the thing people looked at, and
    /// the thing they clicked was its Skip step button.
    private func cardRect(in size: CGSize) -> CGRect {
        let width = min(cardWidth, max(240, size.width - 32))
        let margin: CGFloat = 18
        let maxX = max(margin, size.width - width - margin)
        let maxY = max(margin, size.height - cardHeight - margin)
        // The toast follows density, so it is taller in comfortable; the
        // room left for it has to grow on the same curve.
        let toastClearance: CGFloat = store.undos.isEmpty ? 0 : Space.scaled(64)
        let x = min(maxX, max(margin, SidebarRail.railWidth + margin - frame.minX))
        let y = max(margin, maxY - toastClearance)
        return CGRect(x: x, y: y, width: width, height: cardHeight)
    }

    private func cardOrigin(in size: CGSize) -> CGSize {
        let rect = cardRect(in: size)
        return CGSize(width: rect.minX, height: rect.minY)
    }

    /// Whether the card is showing Next (rather than Skip step) right now.
    private var showsNext: Bool {
        step != .wrap && step != .welcome
            && !(step.isInteraction && !(step == .undo && undoWasRestored))
    }

    /// Where the arrow goes: at the thing to click, or, when a lesson has
    /// nothing to click, at Next. A board row is pointed at from its left,
    /// level with it, the way a finger would; anything else from above when
    /// there is room, then below, then beside it, never over the guide card.
    private func clickArrow(in size: CGSize) -> (origin: CGPoint, direction: ClickArrow.Direction)? {
        let side = ClickArrow.size
        let gap: CGFloat = 6
        let card = cardRect(in: size)
        if let target = clickRect {
            let bounds = CGRect(origin: .zero, size: size).insetBy(dx: 4, dy: 4)
            let leftOf = (CGPoint(x: target.minX - gap - side, y: target.midY - side / 2), ClickArrow.Direction.right)
            let above = (CGPoint(x: target.midX - side / 2, y: target.minY - gap - side), ClickArrow.Direction.down)
            let below = (CGPoint(x: target.midX - side / 2, y: target.maxY + gap), ClickArrow.Direction.up)
            let rightOf = (CGPoint(x: target.maxX + gap, y: target.midY - side / 2), ClickArrow.Direction.left)
            let isRow = step.clickTarget == .maya || step.clickTarget == .brightly
            let candidates = isRow ? [leftOf, above, below, rightOf] : [above, below, rightOf, leftOf]
            let fits = candidates.first { origin, _ in
                let rect = CGRect(origin: origin, size: CGSize(width: side, height: side))
                return bounds.contains(rect) && !rect.intersects(card)
            }
            // Somewhere is better than nowhere: a cramped window still gets
            // an arrow at the target rather than no hint at all.
            return fits ?? candidates[0]
        }
        // Nothing to click: point at Next from just outside the card's right
        // edge, level with the button.
        guard showsNext, !readingRecord, let raw = nextButton else { return nil }
        let button = raw.offsetBy(dx: -frame.minX, dy: -frame.minY)
        let x = card.maxX + gap
        guard x + side <= size.width else { return nil }
        return (CGPoint(x: x, y: button.midY - side / 2), .left)
    }

    private func next() {
        if step.isInteraction { tour.skipPracticeStep() }
        else { tour.advancePractice() }
    }

    private func prepare(_ next: PracticeTourStep) {
        guard tour.active else { return }
        if store.activeView != .sitrep { store.setView(.sitrep) }
        prerender(for: next)
        // The learner just opened these readers. Preserve their loaded content
        // as the instruction changes instead of closing and fetching it again.
        if next == .done {
            if store.threadId != "practice-1" { openExample(1) }
            // The lesson is "e: done, and back to the board". A learner who
            // opened the email with Space is in flow mode, where `e` walks on
            // to the next email instead; the lesson takes them out of it.
            store.walk = nil
        } else if next == .rule {
            if store.threadId != "practice-11" { openExample(11) }
        } else { store.closeThread() }
        reconcile()
    }

    /// Render what this lesson asks the learner to open BEFORE they click
    /// it, so the reader opens on a finished frame instead of a blank one.
    private func prerender(for step: PracticeTourStep) {
        var ids: [String] = []
        switch step.clickTarget {
        case .calendar: ids = store.zones.calendar.compactMap(\.thread_id)
        default: break
        }
        if step == .shipments { ids = store.zones.shipments.compactMap(\.thread_id) }
        if let example = step.exampleID { ids.insert("practice-\(example)", at: 0) }
        guard !ids.isEmpty else { return }
        for id in ids { ThreadPrefetch.shared.prefetch(id, fresh: ThreadPrefetch.warmFresh) }
        FramePrewarmer.shared.prefer(ids)
    }

    private func openExample(_ id: Int) {
        let thread = "practice-\(id)"
        // A real queue is essential: the reader resolves through Actions.done
        // with an undo only when the opened thread has its AttentionUpdate.
        let queue = store.update(id: id).map { [$0] } ?? []
        store.openThread(thread, queue: queue, from: .tour)
    }

    private var undoWasRestored: Bool {
        tour.practiceDoneID != nil && !store.resolvedIds.contains(1)
    }

    private var readerCategory: TourTarget? {
        guard let thread = store.threadId else { return nil }
        if store.zones.calendar.contains(where: { $0.thread_id == thread }) { return .calendar }
        if store.zones.shipments.contains(where: { $0.thread_id == thread }) { return .shipments }
        return nil
    }

    /// Navigation alone is not completion: wait for the matching loaded reader,
    /// successful Done's actual undo entry, and Undo's successful restore.
    private func reconcile() {
        guard tour.active else { return }
        tour.observePracticeRecordReader(
            threadID: store.threadId,
            loadedThreadID: store.currentThreadSummary?.threadId,
            category: readerCategory,
            run: tour.practiceRunID)
        switch step {
        case .openMaya:
            if store.threadId == "practice-1", store.currentThreadSummary?.threadId == "practice-1" {
                tour.advancePractice()
            }
        case .done:
            if let entry = store.undos.last(where: { $0.messageId == 1 && $0.kind == .done }) {
                tour.notePracticeDone(entry.id)
                if store.threadId != "practice-1" { tour.advancePractice() }
            }
        case .openBrightly:
            if store.threadId == "practice-11", store.currentThreadSummary?.threadId == "practice-11" {
                tour.advancePractice()
            }
        default: break
        }
    }
}

/// A bobbing arrow at the thing to click. Large and accent-coloured on
/// purpose: it is the one mark in the lesson that says "your turn".
private struct ClickArrow: View {
    enum Direction { case down, up, left, right }
    static let size: CGFloat = 40

    let direction: Direction
    let reduceMotion: Bool

    var body: some View {
        // A phase animator, not a repeatForever started in onAppear: the
        // first arrow appears inside the step change's own transaction, which
        // swallowed that one-shot repeat and left Maya's arrow standing still.
        Image(systemName: "arrow.down")
            .font(.system(size: 26, weight: .heavy))
            .foregroundStyle(Palette.accent)
            .shadow(color: Palette.accent.opacity(0.45), radius: 8)
            .rotationEffect(rotation)
            .phaseAnimator(reduceMotion ? [false] : [false, true]) { arrow, out in
                arrow.offset(x: out ? travel.width : 0, y: out ? travel.height : 0)
            } animation: { _ in .easeInOut(duration: 0.6) }
            .frame(width: Self.size, height: Self.size)
            .accessibilityHidden(true)
    }

    private var rotation: Angle {
        switch direction {
        case .down: .zero
        case .up: .degrees(180)
        case .left: .degrees(90)
        case .right: .degrees(-90)
        }
    }

    /// Nudges toward the target, so the motion itself points.
    private var travel: CGSize {
        switch direction {
        case .down: CGSize(width: 0, height: 5)
        case .up: CGSize(width: 0, height: -5)
        case .left: CGSize(width: -5, height: 0)
        case .right: CGSize(width: 5, height: 0)
        }
    }
}
