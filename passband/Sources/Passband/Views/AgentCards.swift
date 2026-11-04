// THE TWO ROWS EVERY AGENT SURFACE DRAWS: what it is doing (a tool chip) and
// what it found (email cards). Both started life inside AskBar and moved here
// when the search panel grew a lane of its own (docs/SEARCH.md §6.4), because
// the alternative was a second copy that would drift — and the thing that would
// drift is a card the reader clicks to open somebody's mail.
//
// What a card says is the DAEMON'S account of the thread, re-read by
// `show_emails` before the card is built, never the model's description of it.
// That rule lives in AgentTools; this file only draws the result.

import SwiftUI

/// The `show_emails` cards: the agent's answer AS emails, rendered like list
/// rows because they are the result rather than a footnote.
///
/// `onOpen` is the caller's, because opening one means different things in
/// different places: the ⌘K tray opens the thread and closes itself, the search
/// panel opens the thread and stays exactly where it is (the reader is still
/// searching, and the results beside the reader are the point of the strip).
struct EmailCardList: View {
    let cards: [EmailCard]
    let onOpen: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s6) {
            ForEach(cards) { card in
                Button {
                    onOpen(card.threadId)
                } label: {
                    VStack(alignment: .leading, spacing: Space.scaled(3)) {
                        HStack(spacing: Space.scaled(7)) {
                            Avatar(sender: card.sender, size: 18)
                            Text(card.sender)
                                .font(Typo.ui(12, weight: .semibold))
                                .foregroundStyle(Palette.ink)
                                .lineLimit(1)
                            Spacer(minLength: Space.s6)
                            Text(Fmt.dateTime(card.date))
                                .font(Typo.num(10))
                                .foregroundStyle(Palette.inkFaintest)
                        }
                        Text(card.subject)
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkDim)
                            .lineLimit(1)
                        if !card.snippet.isEmpty {
                            Text(card.snippet)
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaintest)
                                .lineLimit(2)
                                .multilineTextAlignment(.leading)
                        }
                    }
                    .padding(Space.s10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(
                    RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                        .fill(Palette.hairline.opacity(0.35))
                )
                .help("Open this thread")
            }
        }
    }
}

/// The `show_page` card: a button to one page of the app. Title, glyph and
/// line all come from `PageLink`, never the model. `onOpen` is the caller's,
/// as on the email cards: the tray navigates and closes itself.
struct PageLinkCard: View {
    let page: PageLink
    let onOpen: (PageLink) -> Void

    var body: some View {
        Button {
            onOpen(page)
        } label: {
            HStack(spacing: Space.s10) {
                Image(systemName: page.symbol)
                    .font(Typo.ui(13, weight: .medium))
                    .foregroundStyle(Palette.inkDim)
                    .frame(width: Typo.size(20))
                VStack(alignment: .leading, spacing: Space.scaled(2)) {
                    Text(page.title)
                        .font(Typo.ui(12, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                    Text(page.blurb)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                Spacer(minLength: Space.s6)
                Image(systemName: "arrow.right")
                    .font(Typo.ui(11, weight: .semibold))
                    .foregroundStyle(Palette.inkFaint)
            }
            .padding(Space.s10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(Palette.hairline.opacity(0.35))
        )
        .help("Go to \(page.title)")
    }
}

/// One tool-activity chip: what the agent is doing, in the smallest type the
/// app has, with its outcome on the trailing edge.
struct ToolChipRow: View {
    let tool: ToolActivity

    var body: some View {
        HStack(spacing: Space.s6) {
            Image(systemName: AgentTools.Tool(rawValue: tool.name)?.symbol ?? "wrench")
                .font(Typo.ui(10, weight: .semibold))
                .foregroundStyle(Palette.inkFaint)
                .frame(width: Typo.size(12))
            Text(tool.summary)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: Space.s4)
            switch tool.state {
            case .running:
                ProgressView().controlSize(.mini)
            case .ok:
                Image(systemName: "checkmark")
                    .font(Typo.ui(9, weight: .bold))
                    .foregroundStyle(Palette.positive.opacity(0.8))
            case .failed:
                Image(systemName: "xmark")
                    .font(Typo.ui(9, weight: .bold))
                    .foregroundStyle(Palette.danger)
            }
        }
    }
}
