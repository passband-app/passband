// "Somebody opened this" on the user's OWN sent mail — the read half of the
// tracking pixel the composer arms.
//
// The mark is deliberately soft. An open is a fetch of a 1x1 image, and Gmail
// fetches images through a proxy that can warm its cache before anybody has
// read anything; the copy says "via proxy" when that is what happened rather
// than claiming a read it cannot see. No opens means no mark at all — "not
// opened yet" and "never tracked" are the same silence, and both are normal.

import SwiftUI

enum ReadReceipts {
    /// How many lookups run at once. A thread is normally a handful of
    /// messages; a mailing-list monster is not a reason to open fifty sockets
    /// for an answer that is almost always empty.
    private static let batch = 8

    /// Every recorded open of each message that has one, keyed by local message
    /// id, oldest first.
    ///
    /// Asks about EVERY message in the thread rather than guessing which copy
    /// was the user's own: the wire carries no "this one is mine" flag, only a
    /// tracked send can have opens, and every other id answers with an empty
    /// list. Callers gate on the daemon having tracking configured, so a reader
    /// who does not use the feature spends nothing here.
    static func opens(for ids: [Int]) async -> [Int: [MessageOpen]] {
        var found: [Int: [MessageOpen]] = [:]
        for start in stride(from: 0, to: ids.count, by: batch) {
            let chunk = ids[start..<min(start + batch, ids.count)]
            await withTaskGroup(of: (Int, [MessageOpen]).self) { group in
                for id in chunk {
                    group.addTask {
                        // A failure is silence, not an error: the mark is an
                        // extra, and a thread must never fail to read for one.
                        (id, (try? await APIClient.shared.messageOpens(id)) ?? [])
                    }
                }
                for await (id, opens) in group where !opens.isEmpty {
                    found[id] = opens
                }
            }
        }
        return found
    }
}

/// Sentences the chip, its tooltip and the detail popover share. The same
/// uncertainty must not be described in three different voices.
private enum ReceiptCopy {
    static let proxyCaveat =
        "Fetched by Gmail's image proxy, which sometimes loads images before "
        + "the recipient reads the message."

    /// "just now" / "3h ago" — the chip's own phrasing, reused per row.
    static func age(_ open: MessageOpen) -> String {
        let age = Fmt.relAge(open.date)
        return age == "now" ? "just now" : "\(age) ago"
    }
}

/// "3 opens", under a sent message's datetime. Click for every open.
///
/// A count rather than the old "opened 3h ago" chip: the chip sat in a row of
/// other chips and never said how many, and "opened 4 times over two days" is
/// a different fact from "opened". The latest stamp and the proxy caveat ride
/// in the tooltip; the full list is in the popover.
struct OpensLink: View {
    /// Every recorded open, oldest first. Empty renders nothing.
    let opens: [MessageOpen]
    /// Who the message went to, so the detail can say whose opens these are
    /// when that is knowable. `nil` when the headers were never read.
    var recipients: Recipients? = nil
    /// The account's own address, left out of "sent to".
    var me: String? = nil
    /// The chat caption's one-point-smaller measure.
    var compact = false

    @State private var detailOpen = false
    @State private var hovering = false

    var body: some View {
        if let latest = opens.last {
            Button { detailOpen.toggle() } label: {
                HStack(spacing: Space.scaled(3)) {
                    Image(systemName: "eye")
                        .font(Typo.ui(compact ? 8 : 9, weight: .semibold))
                    Text(opens.count == 1 ? "1 open" : "\(opens.count) opens")
                        .font(Typo.num(compact ? 10 : 11))
                        .underline(hovering)
                }
                .foregroundStyle(hovering || detailOpen ? Palette.inkDim : Palette.inkFaint)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            #if os(macOS)
                .pointerStyle(.link)
            #endif
            .help(tooltip(latest))
            .accessibilityLabel(
                opens.count == 1 ? "opened once" : "opened \(opens.count) times")
            // Downward: the line sits in a message's header, so the space
            // below it is the message body, the one direction with room.
            .popover(isPresented: $detailOpen, arrowEdge: .bottom) {
                OpensDetail(opens: opens, sentTo: sentTo)
            }
        }
    }

    /// Everyone the message named except the account itself.
    private var sentTo: [String] {
        guard let recipients else { return [] }
        let mine = Recipients.key(me ?? "")
        var seen = Set<String>()
        return recipients.all.filter { token in
            let key = Recipients.key(token)
            return !key.isEmpty && key != mine && seen.insert(key).inserted
        }
    }

    /// The exact stamp of the latest open, plus the caveat when it applies.
    private func tooltip(_ latest: MessageOpen) -> String {
        let head = "latest open \(Fmt.dateTime(latest.date))"
        guard latest.viaProxy else { return head }
        return head + ". " + ReceiptCopy.proxyCaveat
    }
}

/// EVERY recorded open, newest first: when, how long ago, and what fetched it.
///
/// There is no location on this wire and there will not be one invented here —
/// an open is a pixel fetch, and the honest columns are the time and the agent
/// string. That agent string is REMOTE-CONTROLLED (it is whatever the fetching
/// client sent), so it renders as plain `Text` and is never markup, never
/// interpolated into another sentence.
private struct OpensDetail: View {
    /// Oldest first, as the wire carries them.
    let opens: [MessageOpen]
    /// Who the message went to, the account itself left out. Empty when the
    /// headers were never read.
    let sentTo: [String]

    /// Newest first: the interesting end of the list, and the one the chip
    /// summarizes.
    private var newestFirst: [MessageOpen] { opens.reversed() }
    private var anyProxied: Bool { opens.contains(where: \.viaProxy) }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.scaled(9)) {
            Text(opens.count == 1 ? "1 open" : "\(opens.count) opens")
                .font(Typo.sectionLabel)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.uppercase)

            if let who {
                Text(who)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
            }

            // Capped and scrolled, the same way the debug inspector is: a pixel
            // that got fetched thirty times must not grow a popover taller than
            // the window it hangs off.
            ScrollView {
                VStack(alignment: .leading, spacing: Space.s8) {
                    // Indexed: two identical opens (same second, same agent) are
                    // a real possibility, and they are two facts, not one row.
                    ForEach(Array(newestFirst.enumerated()), id: \.offset) { _, open in
                        row(open)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 240)

            if anyProxied {
                Hairline()
                Text(ReceiptCopy.proxyCaveat)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(Space.s14)
        .frame(width: 300, alignment: .leading)
    }

    /// WHOSE OPENS THESE ARE, as far as the pixel can say. One pixel rides in
    /// one copy of the mail, so with one recipient the opens are theirs (or a
    /// forward's), and with several there is no telling which of them it was.
    /// Saying so beats letting the reader assume.
    private var who: String? {
        let names = sentTo.map { SenderCache.resolved($0).displayName }
        switch names.count {
        case 0: return nil
        case 1: return "Sent to \(names[0])."
        case 2...3:
            return "Sent to \(names.joined(separator: ", ")). The pixel can't tell which of them opened it."
        default:
            return "Sent to \(names.count) people. The pixel can't tell which of them opened it."
        }
    }

    private func row(_ open: MessageOpen) -> some View {
        VStack(alignment: .leading, spacing: Space.s2) {
            HStack(spacing: Space.s6) {
                Text(Fmt.dateTime(open.date))
                    .font(Typo.num(11))
                    .foregroundStyle(Palette.ink)
                Text(ReceiptCopy.age(open))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                if open.viaProxy {
                    Chip(text: "via proxy", tone: Palette.inkFaintest)
                }
                Spacer(minLength: 0)
            }
            if let agent = open.user_agent, !agent.isEmpty {
                // PLAIN TEXT, deliberately: this string came off the wire from
                // whoever fetched the pixel. Middle truncation because the
                // distinguishing part of a UA is at both ends.
                Text(agent)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
    }
}
