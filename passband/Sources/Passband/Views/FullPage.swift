// A FULL PAGE IN THE CONTENT PANE: the scaffold, paper and buttons the smart
// clear list is built from.

#if os(macOS)
    import SwiftUI

    // MARK: - the page scaffold

    /// A page in the CONTENT PANE: the app's page header, the content, a bar
    /// across the bottom. The same chrome as every routed page, because it
    /// is mounted where they are.
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
            // real rail, exactly as a routed page is.
        }
    }

    /// The paper the page's cards are drawn on, a step up from the canvas.
    let pagePaper = Color(light: 0xF8FAFD, dark: 0x151E2A)

    /// The text that sits on the accent fill of a primary action.
    private let onAccent = Color(light: 0xFFFFFF, dark: 0x06111D)

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
                    if role == .primary { PageKey(key, style: .onAccent) } else { Kbd(key) }
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

    /// A keycap that can sit on the card or on an accent fill. `Kbd` is
    /// drawn for the first of those only.
    struct PageKey: View {
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
#endif
