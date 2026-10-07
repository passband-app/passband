// The reader's find bar (⌘F with an email open), and the two small things the
// bodies it searches share: where a match is, and the invisible view the
// column scrolls to. The session itself is Lib/FindInThread.swift.

import SwiftUI

/// Where a body says the active match is, in that body's own points.
struct FindSpot: Equatable {
    var y: CGFloat
    var height: CGFloat
}

/// What a match is painted with outside a mail frame. The frames paint their
/// own (FindScript), in the same two colours: every match, and the one the
/// cursor is on.
enum FindPaint {
    static let match = Color(red: 1.0, green: 0.898, blue: 0.541)
    static let active = Color(red: 1.0, green: 0.624, blue: 0.110)
    static let ink = Color(red: 0.1, green: 0.1, blue: 0.1)
}

/// THE SCROLL TARGET FOR THE ACTIVE MATCH. The column scrolls and the bodies
/// do not, so "bring that match into view" has to be said to the column, and
/// a column can only aim at a view. This is that view: nothing drawn, placed
/// by its host (an overlay, top leading) where the match is.
struct FindAnchor: View {
    let spot: FindSpot
    @Environment(ReaderFind.self) private var find: ReaderFind?
    /// Whose position the session is holding. A cursor crossing from one
    /// message to the next mounts the new anchor and unmounts the old one in
    /// no promised order, and the old one leaving must not erase the new
    /// one's answer.
    @State private var token = UUID()

    var body: some View {
        Color.clear
            .frame(width: 1, height: spot.height)
            .id(ReaderFind.anchorID)
            .onGeometryChange(for: CGFloat.self) { $0.frame(in: .scrollView).minY } action: {
                find?.noteAnchor(token, minY: $0)
            }
            .padding(.top, spot.y)
            .onDisappear { find?.dropAnchor(token) }
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}

/// The bar: a term, where the cursor is among its matches, and the two ways to
/// move it. Enter and ⇧Enter are bound in the reader's keymap (they have to
/// beat the field's own), so the buttons here are the pointer's spelling of
/// the same two verbs.
struct ReaderFindBar: View {
    @Bindable var find: ReaderFind
    let onClose: () -> Void
    @FocusState private var focused: Bool

    private var status: String {
        guard !find.query.isEmpty else { return "" }
        let total = find.total
        guard total > 0 else { return "no matches" }
        guard let ordinal = find.ordinal else { return "\(total)" }
        return "\(ordinal) of \(total)"
    }

    var body: some View {
        HStack(spacing: Space.s8) {
            Image(systemName: "magnifyingglass")
                .font(Typo.ui(11, weight: .medium))
                .foregroundStyle(Palette.inkFaint)
            TextField("find in this email", text: $find.query)
                .textFieldStyle(.plain)
                .font(Typo.rowSub)
                .foregroundStyle(Palette.ink)
                .frame(width: Space.scaled(170))
                .focused($focused)
            Text(status)
                .font(Typo.num(10))
                .foregroundStyle(
                    find.total == 0 && !find.query.isEmpty ? Palette.warn : Palette.inkFaint
                )
                .monospacedDigit()
                .lineLimit(1)
                .fixedSize()
            HStack(spacing: Space.s2) {
                step("chevron.up", -1, "previous match (⇧Enter)")
                step("chevron.down", 1, "next match (Enter)")
            }
            Button(action: onClose) {
                Image(systemName: "xmark")
                    .font(Typo.ui(10, weight: .semibold))
                    .frame(width: 20, height: 20)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.textAction)
            .help("close (Esc)")
            .accessibilityLabel("close find")
        }
        .padding(.leading, Space.scaled(12))
        .padding(.trailing, Space.s6)
        .padding(.vertical, Space.s4)
        // OPAQUE, not glass: the bar floats over the top of the mail, where a
        // message's date and sender sit, and clear glass let them read
        // straight through the term being typed.
        .background(Palette.canvas, in: Capsule())
        .overlay { Capsule().strokeBorder(Palette.hairlineStrong, lineWidth: 1) }
        .shadow(color: .black.opacity(0.22), radius: 10, y: 3)
        .onAppear { focused = true }
        .onChange(of: find.focusTick) { _, _ in focused = true }
        .onChange(of: focused, initial: true) { _, now in find.fieldFocused = now }
        .onDisappear { find.fieldFocused = false }
    }

    private func step(_ symbol: String, _ delta: Int, _ help: String) -> some View {
        Button { find.step(delta) } label: {
            Image(systemName: symbol)
                .font(Typo.ui(10, weight: .semibold))
                .frame(width: 20, height: 20)
                .contentShape(Rectangle())
        }
        .buttonStyle(.textAction)
        .disabled(find.total == 0)
        .help(help)
    }
}
