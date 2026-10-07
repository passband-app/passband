// The sitrep's bottom-right corner while an automatic unsubscribe runs: who is
// being unsubscribed from, and, when their page wants a person, the one button
// that finishes it there.
//
// Quiet by design. A running job is a spinner beside the sender's mark; a done
// one says so for a breath and goes; only `needs_you` stays, because it is the
// one state that is waiting on the user. Sender strings are email-derived and
// render as Text only, never as markup.

import SwiftUI

struct UnsubscribeTray: View {
    private var jobs: UnsubscribeJobs { .shared }

    /// More than this and the tray would stack up the sitrep's corner; the rest
    /// are counted, and arrive as the ones above them finish.
    private static let maxShown = 3

    var body: some View {
        let items = jobs.items
        let shown = Array(items.suffix(Self.maxShown))
        let more = items.count - shown.count
        GlassEffectContainer(spacing: 8) {
            VStack(alignment: .trailing, spacing: Space.scaled(7)) {
                if more > 0 {
                    Text("+\(more) more")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .padding(.trailing, Space.s6)
                        .transition(.opacity)
                }
                ForEach(shown) { item in
                    UnsubscribeCard(item: item)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                }
            }
        }
        .animation(.smooth(duration: 0.26), value: shown)
    }
}

private struct UnsubscribeCard: View {
    let item: UnsubscribeJobs.Item

    private var name: String { SenderCache.resolved(item.sender).displayName }

    var body: some View {
        HStack(alignment: .center, spacing: Space.s10) {
            Avatar(sender: item.sender, size: 26)
            VStack(alignment: .leading, spacing: Space.scaled(3)) {
                headline
                if case .needsYou(let reason, _) = item.phase {
                    Text(Self.why(reason))
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            #if os(macOS)
                .frame(maxWidth: Typo.measure(230), alignment: .leading)
            #else
                // The phone's card is as wide as the screen lets it be, and the
                // sentence takes what the trailing control leaves.
                .frame(maxWidth: .infinity, alignment: .leading)
            #endif
            trailing
        }
        .padding(.vertical, Space.scaled(9))
        .padding(.leading, Space.s10)
        .padding(.trailing, Space.s12)
        .passbandGlass(.pane, cornerRadius: 14, tint: tint)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var headline: some View {
        switch item.phase {
        case .running:
            (Text("Unsubscribing from ").foregroundStyle(Palette.inkDim)
                + Text(name).foregroundStyle(Palette.ink).fontWeight(.semibold))
                .font(Typo.rowSub)
                .lineLimit(1)
        case .done:
            (Text("Unsubscribed from ").foregroundStyle(Palette.inkDim)
                + Text(name).foregroundStyle(Palette.ink).fontWeight(.semibold))
                .font(Typo.rowSub)
                .lineLimit(1)
        case .needsYou:
            (Text(name).foregroundStyle(Palette.ink).fontWeight(.semibold)
                + Text(" needs you to finish").foregroundStyle(Palette.inkDim))
                .font(Typo.rowSub)
                .lineLimit(1)
        }
    }

    @ViewBuilder
    private var trailing: some View {
        switch item.phase {
        case .running:
            ProgressView()
                .controlSize(.small)
                .accessibilityLabel("in progress")
        case .done:
            Image(systemName: "checkmark.circle.fill")
                .font(Typo.ui(15))
                .foregroundStyle(Palette.positive)
                .accessibilityLabel("done")
        case .needsYou(_, let url):
            HStack(spacing: Space.s6) {
                if url != nil {
                    Button {
                        UnsubscribeJobs.shared.finish(item)
                    } label: {
                        #if os(macOS)
                            Label("Finish on their site", systemImage: "arrow.up.right")
                                .labelStyle(.titleAndIcon)
                        #else
                            // The sentence beside it already says what needs
                            // finishing; five words here would leave it none.
                            Label("Finish", systemImage: "arrow.up.right")
                                .labelStyle(.titleAndIcon)
                        #endif
                    }
                    .buttonStyle(.glassProminent)
                    .controlSize(.small)
                    .help("Opens their unsubscribe page in your browser")
                }
                Button {
                    UnsubscribeJobs.shared.dismiss(item)
                } label: {
                    Image(systemName: "xmark")
                        .font(Typo.ui(10, weight: .semibold))
                        .foregroundStyle(Palette.inkFaint)
                        .frame(width: Space.scaled(18), height: Space.scaled(18))
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Leave it")
                .accessibilityLabel("Dismiss")
            }
        }
    }

    private var tint: Color? {
        switch item.phase {
        case .running: Palette.glassTint
        case .done: Palette.positiveSoft
        case .needsYou: Palette.warnSoft
        }
    }

    /// One line on why, in the user's terms. Keyed by the daemon's
    /// `status_reason`; an unknown reason reads as the generic one.
    static func why(_ reason: String?) -> String {
        switch reason {
        case "choose_topics": "Their page asks which emails to stop."
        case "needs_browser": "Their page only works in a browser."
        case "link_error": "Their link didn't work from here."
        default: "We couldn't confirm it went through."
        }
    }
}
