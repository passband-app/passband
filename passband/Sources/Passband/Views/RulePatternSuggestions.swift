// The rule editor's sender menu: who the rule could be about, offered under the
// match-pattern field while it is being typed. A person is offered as their
// address, a host as `*@host`; typing `*@` asks for hosts alone. What to ask
// and what the rows are made of is `RulePatternSuggest`; this is the list, its
// keys and the two fetches.
//
// The keymap is SenderSuggestions' and RecipientField's: the bindings mount
// with the list, so they register after the card's own Enter, Tab and Esc and
// win exactly while there is a list to drive. Esc takes the list down and only
// the list; the next Esc is the card's.

import SwiftUI

struct RulePatternSuggestions: View {
    /// The field's whole text. The owner mounts this only while the field has
    /// focus and has been typed in, so a rule opened for editing does not
    /// arrive under a menu about its own pattern.
    let typed: String
    /// Called with the pattern to write in the field.
    let onAccept: (String) -> Void

    @State private var rows: [RulePatternSuggest.Suggestion] = []
    @State private var index = 0
    /// Set by Esc, cleared by the next keystroke: "not this menu", not "never".
    @State private var dismissed = false

    /// Same as the `from:` menu's: one request per typing run.
    private static let debounce = Duration.milliseconds(120)
    /// People, then hosts under them; hosts get the whole menu when the owner
    /// asked for hosts.
    private static let senderRows = 5
    private static let domainRowsBeside = 2
    private static let domainRowsAlone = 6

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            ForEach(Array(rows.enumerated()), id: \.element.id) { i, row in
                Button {
                    onAccept(row.pattern)
                } label: {
                    HStack(spacing: Space.s8) {
                        // Sender strings are email-derived: rendered as Text
                        // only, never as markup.
                        label(row, selected: i == index)
                        Spacer(minLength: Space.s8)
                        Text(tally(row))
                            .font(Typo.num(10))
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                            .layoutPriority(1)
                    }
                    .padding(.horizontal, Space.scaled(9))
                    .padding(.vertical, Space.scaled(5))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(
                    RoundedRectangle(cornerRadius: Radius.concentric(Radius.well, inset: Space.s4), style: .continuous)
                        .fill(i == index ? Palette.accentSoft : .clear)
                )
            }
            #if os(macOS)
                if !rows.isEmpty {
                    HStack(spacing: Space.s4) {
                        Kbd("↑↓")
                        Text("pick ·").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                        Kbd("enter")
                        Text("use ·").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                        Kbd("esc")
                        Text("dismiss").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                        Spacer(minLength: Space.s8)
                        if case .senders = RulePatternSuggest.ask(for: typed) {
                            Kbd("*@")
                            Text("whole domains").font(Typo.micro)
                                .foregroundStyle(Palette.inkFaintest)
                        }
                    }
                    .padding(.horizontal, Space.scaled(9))
                    .padding(.top, Space.s4)
                }
            #endif
        }
        .padding(rows.isEmpty ? 0 : Space.s4)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(rows.isEmpty ? .clear : Palette.canvas.opacity(0.85))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .strokeBorder(
                    rows.isEmpty ? .clear : Palette.hairlineStrong, lineWidth: 0.75))
        .keyBindings(.modal, rows.isEmpty ? [] : bindings)
        .task(id: typed) { await refresh() }
    }

    @ViewBuilder
    private func label(_ row: RulePatternSuggest.Suggestion, selected: Bool) -> some View {
        switch row.kind {
        case .sender(let name):
            if let name {
                Text(name)
                    .font(Typo.ui(11, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
            }
            Text(row.pattern)
                .font(Typo.mono(11))
                .foregroundStyle(selected ? Palette.ink : Palette.inkDim)
                .lineLimit(1)
                .truncationMode(.middle)
        case .domain(let host, _):
            Text("everyone at")
                .font(Typo.ui(11))
                .foregroundStyle(Palette.inkFaint)
                .lineLimit(1)
                .fixedSize()
            Text(host)
                .font(Typo.mono(11))
                .foregroundStyle(selected ? Palette.ink : Palette.inkDim)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    /// How much mail the rule would be about, and for a host how many
    /// addresses, which is the number that says how wide `*@` reaches.
    private func tally(_ row: RulePatternSuggest.Suggestion) -> String {
        if case .domain(_, let senders?) = row.kind, senders > 1 {
            return "\(senders) senders · \(row.messages)×"
        }
        return "\(row.messages)×"
    }

    // MARK: - keymap

    private var bindings: [KeyBinding] {
        [
            KeyBinding("ArrowDown", "next sender", allowInInput: true) {
                index = min(rows.count - 1, index + 1)
            },
            KeyBinding("ArrowUp", "prev sender", allowInInput: true) {
                index = max(0, index - 1)
            },
            KeyBinding("Enter", "use sender", allowInInput: true) { acceptSelected() },
            KeyBinding("Tab", "use sender", allowInInput: true) { acceptSelected() },
            KeyBinding("Escape", "dismiss senders", allowInInput: true) {
                rows = []
                dismissed = true
            },
        ]
    }

    private func acceptSelected() {
        guard let row = rows[safe: index] else { return }
        onAccept(row.pattern)
    }

    // MARK: - fetch

    private func refresh() async {
        // A new keystroke is a new question; an Esc answered the old one.
        dismissed = false
        let ask = RulePatternSuggest.ask(for: typed)
        // A hand-written glob takes the menu down at once, not a debounce later.
        if ask != .nothing {
            // Debounce: a fresh keystroke cancels this task before the request.
            try? await Task.sleep(for: Self.debounce)
            guard !Task.isCancelled else { return }
        }

        var senders: [SenderHit] = []
        var domains: [SenderDomainHit] = []
        switch ask {
        case .nothing:
            break
        case .domains(let fragment):
            domains = await Self.hosts(matching: fragment, limit: Self.domainRowsAlone)
        case .senders(let text, let host):
            async let people = (try? APIClient.shared.senders(text, limit: Self.senderRows)) ?? []
            if let host {
                domains = await Self.hosts(matching: host, limit: Self.domainRowsBeside)
            }
            senders = await people
        }
        guard !Task.isCancelled, !dismissed else { return }
        rows = RulePatternSuggest.suggestions(typed: typed, senders: senders, domains: domains)
        index = 0
    }

    /// Hosts for a fragment. A daemon older than `/client/sender-domains`
    /// answers 404, and the hosts are then rolled up from the senders it does
    /// have; any other failure is just no hosts.
    private static func hosts(matching fragment: String, limit: Int) async -> [SenderDomainHit] {
        do {
            return try await APIClient.shared.senderDomains(fragment, limit: limit)
        } catch let error as APIError where error.kind == .notFound {
            let page =
                (try? await APIClient.shared.senders(
                    RulePatternSuggest.fallbackQuery(fragment), limit: 25)) ?? []
            return RulePatternSuggest.domains(from: page, matching: fragment, limit: limit)
        } catch {
            return []
        }
    }
}
