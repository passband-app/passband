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
// READ IN THREE PASSES, because nobody reads twenty bullets. The phosphor hero
// and the headline say a release happened; the HEADLINER tiles (the items the
// table gives a symbol) say what it was for; the per-surface lists underneath
// are the record, for whoever came looking for one specific fix. The filler
// every release carries ("Bug fixes") is said once, in a footnote, rather than
// weighing as much as the headline. ReleaseDigest does the arranging.
//
// The NEWEST release gets the card. Somebody who skipped two updates is still
// owed the boundaries between them, since that is the only way to tell which
// release broke or fixed the thing they came looking for, so the older ones
// sit below as rows that open in place rather than as a second and third wall.

import SwiftUI

struct WhatsNewCard: View {
    @Environment(AppStore.self) private var store
    #if os(macOS)
        @Environment(\.controlActiveState) private var activeState
        private var paused: Bool { activeState != .key }
    #else
        private let paused = false
    #endif

    /// The older releases opened in place, by version.
    @State private var opened: Set<String> = []

    private var notes: [ReleaseNote] { store.whatsNew.notes }

    private static let width: CGFloat = 580
    private static let radius: CGFloat = 20

    var body: some View {
        OverlayScrim(onDismiss: dismiss) {
            VStack(spacing: 0) {
                if let newest = notes.first {
                    hero(newest)

                    // A ceiling, not a height: a small release is a short card,
                    // and a big one is a scroll rather than a sheet taller than
                    // the window.
                    ScrollView {
                        VStack(alignment: .leading, spacing: 22) {
                            title(newest)
                            ReleaseDigestView(digest: newest.digest)
                            if notes.count > 1 {
                                earlier(Array(notes.dropFirst()))
                            }
                        }
                        .padding(.horizontal, 24)
                        .padding(.top, 18)
                        .padding(.bottom, 20)
                    }
                    .frame(maxHeight: 440)
                    .fixedSize(horizontal: false, vertical: true)
                }

                footer
            }
            #if os(macOS)
                .frame(width: Self.width)
            #else
                .frame(maxWidth: Self.width)
                .padding(.horizontal, 12)
            #endif
            .passbandGlass(.pane, cornerRadius: Self.radius, tint: Palette.glassTint)
            .shadow(color: .black.opacity(0.28), radius: 40, y: 18)
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Escape", "close what's new") { dismiss() },
            KeyBinding("Enter", "close what's new") { dismiss() },
        ])
    }

    private func dismiss() { store.whatsNew.dismiss() }

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
                .font(.system(size: 9, weight: .bold).width(.expanded))
                .tracking(0.6)
                .foregroundStyle(.white.opacity(0.92))
                .padding(.horizontal, 9)
                .padding(.vertical, 5)
                .background(.black.opacity(0.32), in: Capsule())
                .overlay(Capsule().strokeBorder(Color(hex: 0x82BAF5).opacity(0.45), lineWidth: 0.5))
                .padding(14)
        }
        .frame(height: 112)
        .frame(maxWidth: .infinity)
        .clipShape(UnevenRoundedRectangle(topLeadingRadius: Self.radius, topTrailingRadius: Self.radius))
    }

    private func title(_ note: ReleaseNote) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(Self.longDate(note.date))
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
            Text(note.headline)
                .font(Typo.hero(30))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            // Plural only when it is: somebody who updates every week should
            // not be told they missed a stack of releases.
            if notes.count > 1 {
                Text(
                    notes.count == 2
                        ? "Plus one earlier release you skipped, at the bottom."
                        : "Plus \(notes.count - 1) earlier releases you skipped, at the bottom."
                )
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkFaint)
            }
        }
    }

    private func earlier(_ older: [ReleaseNote]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionRule(symbol: "clock.arrow.circlepath", label: "Earlier")
            ForEach(older) { note in
                VStack(alignment: .leading, spacing: 14) {
                    Button {
                        withAnimation(Motion.disclose) {
                            if opened.contains(note.version) {
                                opened.remove(note.version)
                            } else {
                                opened.insert(note.version)
                            }
                        }
                    } label: {
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Text(note.version)
                                .font(Typo.num(12, weight: .semibold))
                                .foregroundStyle(Palette.accent)
                            Text(note.headline)
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundStyle(Palette.ink)
                                .lineLimit(1)
                            Spacer(minLength: 8)
                            Text(Self.longDate(note.date))
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaintest)
                            Image(systemName: "chevron.right")
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle(Palette.inkFaint)
                                .rotationEffect(.degrees(opened.contains(note.version) ? 90 : 0))
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)

                    if opened.contains(note.version) {
                        ReleaseDigestView(digest: note.digest)
                            .transition(.opacity)
                    }
                }
                .padding(12)
                .background(Palette.ink.opacity(0.035), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(Palette.hairline, lineWidth: 1))
            }
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            // WHERE THE REST LIVES. The card carries only what is unread; the
            // full history is a document, and Settings is where somebody goes
            // looking for it a second time.
            Text("Settings has this again under General.")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
            Spacer(minLength: 10)
            Button("Got it") { dismiss() }
                .buttonStyle(.glassProminent)
                .font(.system(size: 12, weight: .semibold))
                .keyboardShortcut(.defaultAction)
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 14)
        .overlay(alignment: .top) {
            Rectangle().fill(Palette.hairline).frame(height: 1)
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

/// Tiles, then the per-surface record. Shared by the newest release and any
/// older one opened in place, so the two can never be laid out differently.
private struct ReleaseDigestView: View {
    let digest: ReleaseDigest

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            if !digest.headliners.isEmpty {
                tiles
            }
            ForEach(digest.sections) { section in
                VStack(alignment: .leading, spacing: 8) {
                    SectionRule(symbol: section.surface.symbol, label: section.surface.label)
                    ForEach(section.items.indices, id: \.self) { index in
                        bullet(section.items[index])
                    }
                    if let footnote = ReleaseDigest.footnote(section.extras) {
                        Text(footnote)
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkFaint)
                            .padding(.top, section.items.isEmpty ? 0 : 2)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Two up, in rows of equal height so a two-line tile does not leave its
    /// neighbour floating. An odd one out takes the whole last row.
    private var tiles: some View {
        let pairs = stride(from: 0, to: digest.headliners.count, by: 2).map {
            Array(digest.headliners[$0..<min($0 + 2, digest.headliners.count)])
        }
        return Grid(horizontalSpacing: 10, verticalSpacing: 10) {
            ForEach(pairs.indices, id: \.self) { row in
                GridRow {
                    ForEach(pairs[row]) { headliner in
                        tile(headliner)
                            .gridCellColumns(pairs[row].count == 1 ? 2 : 1)
                    }
                }
            }
        }
    }

    private func tile(_ headliner: ReleaseDigest.Headliner) -> some View {
        HStack(alignment: .top, spacing: 11) {
            Image(systemName: headliner.symbol)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Palette.accent)
                .frame(width: 30, height: 30)
                .background(Palette.accentSoft, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
            VStack(alignment: .leading, spacing: 3) {
                Text(headliner.text)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                // Which surface it landed on, said once for all of them:
                // the whole reason the tile exists instead of two bullets.
                Text(headliner.surfaces.map(\.label).joined(separator: " · "))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
            }
            Spacer(minLength: 0)
        }
        .padding(11)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Palette.ink.opacity(0.035), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1))
        .accessibilityElement(children: .combine)
    }

    private func bullet(_ text: String) -> some View {
        HStack(alignment: .top, spacing: 9) {
            // Aligned to the first LINE of the text, not its centre, so a
            // two-line item still reads as one bullet.
            Circle()
                .fill(Palette.accent.opacity(0.7))
                .frame(width: 4, height: 4)
                .padding(.top, 6.5)
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
        HStack(spacing: 7) {
            Image(systemName: symbol)
                .font(.system(size: 10, weight: .semibold))
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
