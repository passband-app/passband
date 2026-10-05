// The what's-new card: one modal, once per version, listing what the release
// brought and which half of Passband brought it.
//
// A MODAL rather than the update card's bottom-centre float, and the difference
// is what each one wants. UpdateAlert waits beside your work for an answer you
// may not want to give yet; this is a thing to READ, arriving exactly once,
// and a card competing with a live inbox for attention gets neither read nor
// dismissed. Same scrim as the shortcuts sheet, so it reads as part of the app
// rather than as an announcement bolted to the front of it.
//
// A WALK, THEN THE RECORD, because nobody reads twenty bullets and nobody
// learns a key from one. Each HEADLINER (an item the table gives a symbol) gets
// a page of its own: a vignette that plays the gesture, the feature's name, and
// one line on how to use it. Next moves through them. The last page is the
// record: the phosphor hero, the headline, and the per-surface lists for
// whoever came looking for one specific fix. The filler every release carries
// ("Bug fixes") is said once, in a footnote, rather than weighing as much as
// the headline. ReleaseDigest does the arranging.
//
// SKIPPED RELEASES ARE ONE CARD, NOT SEVERAL. Somebody two updates behind
// opens on a greeter that says so and names the releases they are catching up
// on, then walks every headliner they missed, newest release first, then gets
// ONE list per surface with every release's bullets in it. Each page still
// names the release its feature came from, which is where the boundaries
// between releases stay visible.

import SwiftUI

struct WhatsNewCard: View {
    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    #if os(macOS)
        @Environment(\.controlActiveState) private var activeState
        private var paused: Bool { activeState != .key }
    #else
        private let paused = false
    #endif

    /// Where the walk is: an index into `pages`.
    @State private var page = WhatsNew.openingPage
    /// Which way the last move went, so the words turn like a page: Next
    /// brings them in from the right, Back from the left.
    @State private var forward = true

    private enum Page {
        case greeter(ReleaseCatchUp)
        case feature(ReleaseDigest.Headliner)
        case record
    }

    private var notes: [ReleaseNote] { store.whatsNew.notes }
    private var catchUp: ReleaseCatchUp? { store.whatsNew.catchUp }
    /// Every release on the card as one read.
    private var digest: ReleaseDigest { ReleaseDigest(notes) }
    /// The greeter when there is one, a page per headliner, then the record.
    private var pages: [Page] {
        (catchUp.map { [.greeter($0)] } ?? []) + digest.headliners.map { .feature($0) } + [.record]
    }
    private var pageCount: Int { pages.count }
    private var onLast: Bool { page >= pageCount - 1 }

    private static let width: CGFloat = 580
    private static let radius: CGFloat = 20
    /// A feature page's picture, the stage's 2:1 across the card.
    private static let artHeight: CGFloat = (width * VignetteClock.canvas.height / VignetteClock.canvas.width).rounded()
    /// Every page of a walk is this tall, so Next changes what the card says
    /// and never where its buttons are.
    private static var pageHeight: CGFloat { artHeight + Space.scaled(128) }

    var body: some View {
        // A CLICK NEVER CLOSES IT. The scrim's backdrop click is the shortcuts
        // sheet's way out, and here it was a way to lose the whole walk to one
        // stray click, on the dim or on a quiet part of the card (which falls
        // through to it). The card closes from its last page and nowhere else.
        OverlayScrim(onDismiss: {}) {
            VStack(spacing: 0) {
                if let newest = notes.first {
                    let pages = pages
                    if pages.count == 1 {
                        record(newest, walked: false)
                    } else {
                        ZStack(alignment: .top) {
                            switch pages[min(page, pages.count - 1)] {
                            case .greeter(let greeting):
                                greeter(greeting, newest: newest)
                                    .transition(pageTurn)
                            case .feature(let headliner):
                                feature(headliner)
                                    .transition(.opacity)
                            case .record:
                                record(newest, walked: true)
                                    .transition(pageTurn)
                            }
                        }
                        .frame(height: Self.pageHeight, alignment: .top)
                        .clipped()
                    }
                }

                footer
            }
            #if os(macOS)
                .frame(width: Self.width)
            #else
                .frame(maxWidth: Self.width)
                .padding(.horizontal, Space.s12)
            #endif
            .passbandGlass(.pane, cornerRadius: Self.radius, tint: Palette.glassTint)
            .shadow(color: .black.opacity(0.28), radius: 40, y: 18)
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            // Esc is "let me out", so it goes to the page that can: the last
            // one, where Got it is. From there it closes.
            KeyBinding("Escape", "skip to the end") { escape() },
            KeyBinding("Enter", "next") { advance() },
            KeyBinding("ArrowRight", "next") { go(to: page + 1) },
            KeyBinding("ArrowLeft", "back") { go(to: page - 1) },
        ])
    }

    private func dismiss() { store.whatsNew.dismiss() }

    /// Next, and on the last page the same button closes the card.
    private func advance() {
        if onLast { dismiss() } else { go(to: page + 1) }
    }

    private func escape() {
        if onLast { dismiss() } else { go(to: pageCount - 1) }
    }

    private func go(to target: Int) {
        let clamped = min(max(target, 0), pageCount - 1)
        guard clamped != page else { return }
        // The direction lands a beat BEFORE the page does: the page leaving
        // takes the transition it was last drawn with, so a Back after a
        // Next would otherwise slide out the wrong way.
        forward = clamped > page
        DispatchQueue.main.async {
            withAnimation(reduceMotion ? .easeInOut(duration: 0.2) : .smooth(duration: 0.42)) { page = clamped }
        }
    }

    /// The words' page turn: in from the side the walk is heading, out the
    /// other, with a fade, since a slide alone across glass reads as a
    /// carousel. Reduce Motion keeps only the fade.
    private var pageTurn: AnyTransition {
        guard !reduceMotion else { return .opacity }
        let shift: CGFloat = 36
        return .asymmetric(
            insertion: .offset(x: forward ? shift : -shift).combined(with: .opacity),
            removal: .offset(x: forward ? -shift : shift).combined(with: .opacity))
    }

    // MARK: - a feature's page

    /// One headliner: the gesture playing, what it is called, how to use it.
    /// Two layers that turn differently: the picture CROSSFADES, one night
    /// screen changing channel to the next, while the words beneath it slide
    /// like a page. Each layer is its own ZStack so the outgoing and incoming
    /// copies overlap in place instead of stacking.
    private func feature(_ headliner: ReleaseDigest.Headliner) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            ZStack {
                Group {
                    if let vignette = headliner.vignette {
                        VignetteStage(scene: vignette.scene, live: !paused, cornerRadius: 0)
                    } else {
                        symbolArt(headliner.symbol)
                    }
                }
                .id(page)
                .transition(.opacity)
            }
            .frame(width: Self.width, height: Self.artHeight)
            .clipShape(UnevenRoundedRectangle(topLeadingRadius: Self.radius, topTrailingRadius: Self.radius))

            ZStack(alignment: .topLeading) {
                words(headliner)
                    .id(page)
                    .transition(pageTurn)
            }
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .clipped()

            Spacer(minLength: 0)
        }
    }

    private func words(_ headliner: ReleaseDigest.Headliner) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                // The release's own name rides every page, so the walk
                // reads as one update rather than as four announcements,
                // and a walk across several says where each one ends.
                Text(eyebrow(headliner))
                    .font(Typo.ui(9, weight: .bold).width(.expanded))
                    .tracking(0.8)
                    .textCase(.uppercase)
                    .foregroundStyle(Palette.accent)
                    .lineLimit(1)
                Spacer(minLength: 8)
                Text(headliner.surfaces.map(\.label).joined(separator: " · "))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
            }
            Text(headliner.text)
                .font(Typo.hero(30))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, Space.scaled(9))
            if let detail = headliner.detail {
                KeyedLine(line: detail)
                    .padding(.top, Space.s8)
            }
        }
        .padding(.horizontal, Space.scaled(28))
        .padding(.top, Space.s20)
        .frame(width: Self.width, alignment: .leading)
        .accessibilityElement(children: .combine)
    }

    /// "Philipp's Update · 0.0.12": the release this page's feature shipped in.
    private func eyebrow(_ headliner: ReleaseDigest.Headliner) -> String {
        guard let note = notes.first(where: { $0.version == headliner.version }) else { return headliner.version }
        return "\(note.headline) · \(note.version)"
    }

    /// A headliner with no vignette drawn for it: its symbol on the same night
    /// screen, so a walk can mix pages that move with pages that do not.
    private func symbolArt(_ symbol: String) -> some View {
        ZStack {
            LinearGradient(
                colors: [VignettePalette.groundTop, VignettePalette.groundBottom], startPoint: .top,
                endPoint: .bottom)
            Image(systemName: symbol)
                .font(.system(size: 72, weight: .light))
                .foregroundStyle(VignettePalette.phosphorHot)
                .shadow(color: VignettePalette.phosphor.opacity(0.7), radius: 18)
        }
        .accessibilityHidden(true)
    }

    // MARK: - the greeter

    /// How many releases the greeter names before it starts counting instead.
    private static let greeterRows = 5

    /// The first page for somebody who took several releases at once: thanks,
    /// which version they came from, and the releases about to be walked.
    private func greeter(_ greeting: ReleaseCatchUp, newest: ReleaseNote) -> some View {
        VStack(spacing: 0) {
            hero(newest)
            VStack(alignment: .leading, spacing: 0) {
                Text(greeting.title)
                    .font(Typo.hero(30))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                Text(greeting.line)
                    .font(Typo.ui(15))
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, Space.s8)

                VStack(alignment: .leading, spacing: Space.s8) {
                    ForEach(notes.prefix(Self.greeterRows)) { note in
                        HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
                            Text(note.version)
                                .font(Typo.num(12, weight: .semibold))
                                .foregroundStyle(Palette.accent)
                            Text(note.headline)
                                .font(Typo.ui(13, weight: .semibold))
                                .foregroundStyle(Palette.ink)
                                .lineLimit(1)
                            Spacer(minLength: 8)
                            Text(Self.longDate(note.date))
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaintest)
                        }
                    }
                    if notes.count > Self.greeterRows {
                        let more = notes.count - Self.greeterRows
                        Text(more == 1 ? "And one release before those." : "And \(more) releases before those.")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                    }
                }
                .padding(.top, Space.s20)

                Spacer(minLength: 0)
            }
            .padding(.horizontal, Space.scaled(28))
            .padding(.top, Space.s20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: - the record

    /// The last page, and the whole card for a release with no headliners: the
    /// hero, the headline, and everything the walk did not say, every release
    /// on the card in one list per surface.
    @ViewBuilder
    private func record(_ newest: ReleaseNote, walked: Bool) -> some View {
        let lists = ScrollView {
            VStack(alignment: .leading, spacing: Space.scaled(22)) {
                title(newest)
                // The headliners were just walked through; here they would be
                // the same thing said twice.
                ReleaseDigestView(digest: digest)
            }
            .padding(.horizontal, Space.scaled(28))
            .padding(.top, Space.s20)
            .padding(.bottom, Space.scaled(28))
        }
        // The list's foot dissolves into the glass rather than being sliced
        // by the footer's rule, which says "there is more below" without a
        // scroller's chrome.
        .mask(
            LinearGradient(
                stops: [.init(color: .black, location: 0), .init(color: .black, location: 0.86),
                        .init(color: .clear, location: 1)],
                startPoint: .top, endPoint: .bottom))
        VStack(spacing: 0) {
            hero(newest)
            if walked {
                lists
            } else {
                // A ceiling, not a height: a small release is a short card,
                // and a big one is a scroll rather than a sheet taller than
                // the window.
                lists
                    .frame(maxHeight: 440)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    // MARK: - chrome

    /// The brand's own light, the quiet board's scene in a strip. It carries
    /// only a pill: text set ON the phosphor would have to survive both its
    /// night and its paper, and the title reads better on the glass below.
    private func hero(_ note: ReleaseNote) -> some View {
        ZStack(alignment: .bottomLeading) {
            Group {
                if PhosphorScene.whatsNew.isUnavailable {
                    IntroSceneBackdrop(engaged: true, wide: true, paused: paused)
                } else {
                    PhosphorBackdrop(paused: paused, scene: .whatsNew)
                }
            }
            .allowsHitTesting(false)
            .accessibilityHidden(true)

            // The phosphor's floor fades into the glass rather than ending at
            // a seam.
            LinearGradient(colors: [.clear, .black.opacity(0.28)], startPoint: .center, endPoint: .bottom)
                .allowsHitTesting(false)

            Text("NEW IN PASSBAND \(note.version)")
                .font(Typo.ui(9, weight: .bold).width(.expanded))
                .tracking(0.6)
                .foregroundStyle(.white.opacity(0.92))
                .padding(.horizontal, Space.scaled(9))
                .padding(.vertical, Space.scaled(5))
                .background(.black.opacity(0.32), in: Capsule())
                .overlay(Capsule().strokeBorder(Color(hex: 0x82BAF5).opacity(0.45), lineWidth: 0.5))
                .padding(Space.s14)
        }
        .frame(height: 112)
        .frame(maxWidth: .infinity)
        .clipShape(UnevenRoundedRectangle(topLeadingRadius: Self.radius, topTrailingRadius: Self.radius))
    }

    /// One release is titled by its own headline. Several have no one headline
    /// to borrow, so the page says what it is and whose bullets are in it.
    private func title(_ note: ReleaseNote) -> some View {
        VStack(alignment: .leading, spacing: Space.scaled(5)) {
            if notes.count > 1 {
                if let catchUp {
                    Text("Since \(catchUp.from)")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                }
                Text("The full list")
                    .font(Typo.hero(30))
                    .foregroundStyle(Palette.ink)
                Text(Self.span(notes))
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaint)
            } else {
                Text(Self.longDate(note.date))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                Text(note.headline)
                    .font(Typo.hero(30))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// "Everything from 0.0.12 and 0.0.13, together." Oldest first, the order
    /// they shipped in; past three it is a count, not a recital.
    static func span(_ notes: [ReleaseNote]) -> String {
        let versions = notes.reversed().map(\.version)
        guard versions.count <= 3 else { return "Everything from \(versions.count) releases, together." }
        let joined = versions.dropLast().joined(separator: ", ") + " and " + (versions.last ?? "")
        return "Everything from \(joined), together."
    }

    private var footer: some View {
        HStack(spacing: Space.s12) {
            if pageCount > 1 { dots }
            // WHERE THE REST LIVES. The card carries only what is unread; the
            // full history is a document, and Settings is where somebody goes
            // looking for it a second time.
            if onLast {
                Text("Settings has this again under General.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
            }
            Spacer(minLength: 10)
            if page > 0 {
                Button("Back") { go(to: page - 1) }
                    .buttonStyle(.plain)
                    .font(Typo.ui(12, weight: .medium))
                    .foregroundStyle(Palette.inkFaint)
            }
            // Enter is the modal context's binding above, not a default-action
            // shortcut here: both would answer one press, and Next twice is a
            // page nobody saw.
            Button(onLast ? "Got it" : "Next") { advance() }
                .buttonStyle(.glassProminent)
                .font(Typo.ui(12, weight: .semibold))
        }
        .padding(.horizontal, Space.scaled(28))
        .padding(.vertical, Space.s14)
        .overlay(alignment: .top) {
            Rectangle().fill(Palette.hairline).frame(height: 1)
        }
    }

    /// Where the walk is, and a way to jump: one dot per page, the current one
    /// drawn out. No "2 of 5": a count is a chore, a row of dots is a place.
    private var dots: some View {
        HStack(spacing: Space.scaled(5)) {
            ForEach(0..<pageCount, id: \.self) { index in
                Button {
                    go(to: index)
                } label: {
                    Capsule()
                        .fill(index == page ? Palette.accent : Palette.ink.opacity(0.16))
                        .frame(width: index == page ? 18 : 6, height: 6)
                        .padding(.vertical, Space.s6)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .animation(.smooth(duration: 0.32), value: page)
                .accessibilityLabel(label(pages[index]))
            }
        }
    }

    private func label(_ page: Page) -> String {
        switch page {
        case .greeter: "Welcome back"
        case .feature(let headliner): headliner.text
        case .record: "Everything else"
        }
    }

    /// "September 29, 2026" from the table's ISO date; the ISO date itself if
    /// it does not parse, since the table test already insists that it does.
    static func longDate(_ iso: String) -> String {
        guard let date = isoDay.date(from: iso) else { return iso }
        return date.formatted(date: .long, time: .omitted)
    }

    private static let isoDay: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()
}

// MARK: - one release

/// The per-surface record: what the walk did not give a page.
private struct ReleaseDigestView: View {
    let digest: ReleaseDigest

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s20) {
            ForEach(digest.sections) { section in
                VStack(alignment: .leading, spacing: Space.s8) {
                    SectionRule(symbol: section.surface.symbol, label: section.surface.label)
                    ForEach(section.items.indices, id: \.self) { index in
                        bullet(section.items[index])
                    }
                    if let footnote = ReleaseDigest.footnote(section.extras) {
                        Text(footnote)
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkFaint)
                            .padding(.top, section.items.isEmpty ? 0 : Space.s2)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func bullet(_ text: String) -> some View {
        HStack(alignment: .top, spacing: Space.scaled(9)) {
            // Aligned to the first LINE of the text, not its centre, so a
            // two-line item still reads as one bullet.
            Circle()
                .fill(Palette.accent.opacity(0.7))
                .frame(width: 4, height: 4)
                .padding(.top, Typo.size(13) / 2)
            Text(text)
                .font(Typo.row)
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A section's label with the rule running out to the edge, so the eye finds
/// the boundaries between surfaces without reading the words.
private struct SectionRule: View {
    let symbol: String
    let label: String

    var body: some View {
        HStack(spacing: Space.scaled(7)) {
            Image(systemName: symbol)
                .font(Typo.ui(10, weight: .semibold))
            Text(label)
                .font(Typo.sectionLabel)
                .textCase(.uppercase)
                .fixedSize()
            Rectangle()
                .fill(Palette.hairline)
                .frame(height: 1)
        }
        .foregroundStyle(Palette.inkFaint)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

extension ReleaseSurface {
    /// The card's glyph for each part of Passband.
    fileprivate var symbol: String {
        switch self {
        case .app: "laptopcomputer"
        case .ios: "iphone"
        case .daemon: "server.rack"
        }
    }
}

// MARK: - a line with keys in it

/// A page's line with its keys drawn as KEYS: the table writes a key in
/// backticks, the way the changelog's markdown wants it, and here each one is
/// a small cap of the same make as the vignette's, so the key in the sentence
/// and the key in the picture above are visibly the same thing. Laid out as
/// words that wrap, since a cap cannot sit inside a `Text`.
private struct KeyedLine: View {
    let line: String

    private enum Piece: Hashable {
        case word(String)
        case key(String)
    }

    private var pieces: [Piece] {
        var out: [Piece] = []
        for (index, part) in line.split(separator: "`", omittingEmptySubsequences: false).enumerated() {
            if index % 2 == 1 {
                out.append(.key(String(part)))
            } else {
                out += part.split(separator: " ").map { .word(String($0)) }
            }
        }
        return out
    }

    var body: some View {
        WordFlow(spacing: Space.scaled(4.5), lineSpacing: Space.scaled(5)) {
            ForEach(Array(pieces.enumerated()), id: \.offset) { _, piece in
                switch piece {
                case .word(let word):
                    Text(word)
                        .font(Typo.ui(15))
                        .foregroundStyle(Palette.inkDim)
                case .key(let key):
                    Text(key)
                        .font(Typo.mono(12.5, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .padding(.horizontal, Space.s6)
                        .frame(minWidth: 21, minHeight: 21)
                        .background(
                            RoundedRectangle(cornerRadius: 5, style: .continuous)
                                .fill(Palette.ink.opacity(0.06)))
                        // EVEN all round, and a soft shadow beneath rather
                        // than a lit top edge.
                        .overlay(
                            RoundedRectangle(cornerRadius: 5, style: .continuous)
                                .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75))
                        .shadow(color: .black.opacity(0.12), radius: 0.5, y: 1)
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(line.replacingOccurrences(of: "`", with: ""))
    }
}

/// Words left to right, wrapping at the width offered, each line's pieces
/// centred on one another so a cap sits level with the words beside it.
private struct WordFlow: Layout {
    var spacing: CGFloat
    var lineSpacing: CGFloat

    private func lines(_ subviews: Subviews, width: CGFloat) -> [[(Int, CGSize)]] {
        var lines: [[(Int, CGSize)]] = [[]]
        var x: CGFloat = 0
        for (index, view) in subviews.enumerated() {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                lines.append([])
                x = 0
            }
            lines[lines.count - 1].append((index, size))
            x += size.width + spacing
        }
        return lines
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        let rows = lines(subviews, width: width)
        var height = lineSpacing * CGFloat(max(0, rows.count - 1))
        var used: CGFloat = 0
        for row in rows {
            var rowWidth = spacing * CGFloat(max(0, row.count - 1))
            var rowHeight: CGFloat = 0
            for (_, size) in row {
                rowWidth += size.width
                rowHeight = max(rowHeight, size.height)
            }
            height += rowHeight
            used = max(used, rowWidth)
        }
        return CGSize(width: proposal.width ?? used, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in lines(subviews, width: bounds.width) {
            let height = row.map(\.1.height).max() ?? 0
            var x = bounds.minX
            for (index, size) in row {
                subviews[index].place(
                    at: CGPoint(x: x, y: y + height / 2), anchor: .leading, proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += height + lineSpacing
        }
    }
}
