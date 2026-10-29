// The contact card (issue #318): a click on a sender's name or avatar in the
// reader opens everything the daemon knows about that person, and a
// right-click copies their name or address.
//
// The identity half (avatar, name, address) is drawn from the message the
// reader already holds, so the card opens with something on it at once. The
// history half comes from `/client/person` a round-trip later. A daemon too
// old to have that route answers 404, and the card then just has no history:
// the copy and write actions work against any daemon.
//
// PRIVACY: nothing here leaves the device except that one read on the human
// door. The avatar is the reader's own `Avatar`, which never fetches a human
// correspondent's picture.

import SwiftUI

extension View {
    /// Make this view the door to `sender`'s contact card: click for the card,
    /// right-click (long-press on iOS) for copy name / copy email.
    func contactCard(for sender: String) -> some View {
        modifier(ContactCardTrigger(sender: sender))
    }
}

/// What the trigger and the card both need to say about one sender, parsed
/// once from the "Name <addr>" string the reader carries.
@MainActor
private struct ContactIdentity {
    let sender: String
    let address: String
    /// The name the header actually carried. Empty for a bare address, and
    /// then there is no name to copy: `SenderCache`'s display name would be a
    /// guess made from the local part.
    let headerName: String

    init(_ sender: String) {
        self.sender = sender
        let parsed = SenderID.parse(sender)
        address = parsed.addr
        headerName = parsed.name
    }

    var displayName: String { SenderCache.resolved(sender).displayName }
}

private struct ContactCardTrigger: ViewModifier {
    let sender: String
    @State private var open = false

    func body(content: Content) -> some View {
        let who = ContactIdentity(sender)
        content
            .contentShape(Rectangle())
            .onTapGesture { open.toggle() }
            .help("Show \(who.displayName)'s contact card")
            .contextMenu {
                if !who.headerName.isEmpty {
                    Button("Copy Name") { _ = Platform.copyToPasteboard(who.headerName) }
                }
                Button("Copy Email") { _ = Platform.copyToPasteboard(who.address) }
                Divider()
                Button("Show Contact Card") { open = true }
            }
            #if DEBUG && os(iOS)
                .onAppear { if FixtureStage.takeContactCard() { open = true } }
            #endif
            #if os(macOS)
                // Downward, like the read receipt beside it: the space below a
                // message's header is its body, the one direction with room.
                .popover(isPresented: $open, arrowEdge: .bottom) {
                    ContactCard(who: who, dismiss: { open = false })
                }
            #else
                // A SHEET ON THE PHONE, opaque. As a popover it was a glass
                // bubble pinned over the thread's header, with the subject and
                // the recipient lines showing through it.
                .sheet(isPresented: $open) {
                    ScrollView {
                        ContactCard(who: who, dismiss: { open = false })
                    }
                    .scrollBounceBehavior(.basedOnSize)
                    .presentationDetents([.medium, .large])
                    .presentationDragIndicator(.visible)
                    .presentationBackground(Palette.canvas)
                }
            #endif
    }
}

/// The card's sizes. The Mac's are a popover's; the phone's are a sheet's,
/// read at arm's length, with rows a thumb can hit.
@MainActor
private enum CardMetrics {
    #if os(macOS)
        static let avatar: CGFloat = 40
        static var name: Font { Typo.ui(15, weight: .semibold) }
        static var body: Font { Typo.rowSub }
        static var detail: Font { Typo.micro }
        static var detailInk: Color { Palette.inkFaintest }
        static var labelColumn: CGFloat { Typo.measure(66) }
        static var padding: EdgeInsets { EdgeInsets(Space.s14) }
        static let rowHeight: CGFloat? = nil
    #else
        static let avatar: CGFloat = 52
        static var name: Font { Typo.ui(19, weight: .semibold) }
        static var body: Font { Typo.ui(15) }
        static var detail: Font { Typo.ui(13) }
        static var detailInk: Color { Palette.inkFaint }
        static var labelColumn: CGFloat { 92 }
        static var padding: EdgeInsets {
            EdgeInsets(top: Space.s24 + Space.s8, leading: Space.page,
                bottom: Space.s24, trailing: Space.page)
        }
        static let rowHeight: CGFloat? = 44
    #endif
}

private extension EdgeInsets {
    init(_ all: CGFloat) { self.init(top: all, leading: all, bottom: all, trailing: all) }
}

private struct ContactCard: View {
    let who: ContactIdentity
    let dismiss: () -> Void

    @Environment(AppStore.self) private var store

    private enum Phase: Equatable {
        case loading
        case loaded(PersonCard)
        /// No history to show: an older daemon without the route, or a failed
        /// read. The card still names the person and still acts on them.
        case unavailable
    }

    @State private var phase: Phase = .loading
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s12) {
            header
            Hairline()
            history
            Hairline()
            actions
        }
        .padding(CardMetrics.padding)
        #if os(macOS)
            .frame(width: Typo.measure(320), alignment: .leading)
        #else
            .frame(maxWidth: .infinity, alignment: .leading)
        #endif
        .task(id: who.address) { await load() }
    }

    private var person: PersonCard? {
        if case .loaded(let p) = phase { return p }
        return nil
    }

    /// The header's name, else the one the daemon has for them (the name the
    /// user's own Sent mail addressed them by), else the local-part guess.
    private var name: String {
        if !who.headerName.isEmpty { return who.headerName }
        if let n = person?.display_name, !n.isEmpty { return n }
        return who.displayName
    }

    // MARK: - who

    private var header: some View {
        HStack(alignment: .top, spacing: Space.scaled(11)) {
            Avatar(sender: who.sender, size: CardMetrics.avatar)
            VStack(alignment: .leading, spacing: Space.scaled(3)) {
                Text(name)
                    .font(CardMetrics.name)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(2)
                    .textSelection(.enabled)
                HStack(spacing: Space.scaled(5)) {
                    Text(who.address)
                        .font(CardMetrics.body)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Button(action: copyAddress) {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                            .font(Typo.ui(10, weight: .semibold))
                    }
                    .buttonStyle(.textAction)
                    .help(copied ? "Copied" : "Copy email address")
                }
                badges
            }
            Spacer(minLength: 0)
        }
    }

    @ViewBuilder
    private var badges: some View {
        if let p = person, p.is_self || p.known_contact {
            HStack(spacing: Space.scaled(5)) {
                if p.is_self {
                    Chip(text: "you", tone: Palette.accent)
                } else if p.known_contact {
                    // The known-contact floor, said out loud: this is why their
                    // mail never lands below the line.
                    Chip(text: "known contact", tone: Palette.positive, symbol: "checkmark")
                        .help("You've written to them, so their mail is always signal")
                }
            }
            .padding(.top, Space.scaled(3))
        }
    }

    // MARK: - what the daemon knows

    @ViewBuilder
    private var history: some View {
        switch phase {
        case .loading:
            HStack(spacing: Space.s8) {
                ProgressView().controlSize(.small)
                Text("Reading your history with them")
                    .font(CardMetrics.body)
                    .foregroundStyle(Palette.inkFaint)
            }
            .frame(maxWidth: .infinity, minHeight: Space.scaled(36), alignment: .leading)
        case .unavailable:
            Text("No history available for this address.")
                .font(CardMetrics.body)
                .foregroundStyle(Palette.inkFaint)
        case .loaded(let p):
            loaded(p)
        }
    }

    @ViewBuilder
    private func loaded(_ p: PersonCard) -> some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            if p.received == nil && p.sent == nil {
                Text("No mail with them yet.")
                    .font(CardMetrics.body)
                    .foregroundStyle(Palette.inkFaint)
            }
            if let r = p.received {
                fact(
                    "Wrote you",
                    "\(count(r.count, "email")) since \(monthYear(r.first_at))",
                    detail: "last \(ago(r.last_at))")
            }
            if let s = p.sent {
                fact(
                    "You wrote",
                    count(s.count, "time"),
                    detail: s.last_at.map { "last \(ago($0))" })
            }
            if let rule = p.rule {
                ruleFact(rule)
            }
            if !p.groups.isEmpty {
                fact("Groups", p.groups.map(\.name).joined(separator: " · "))
            }
            if !p.recent_threads.isEmpty {
                recent(p.recent_threads)
            }
        }
    }

    /// One labelled line. The label column is fixed so the values line up.
    private func fact(_ label: String, _ value: String, detail: String? = nil) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
            Text(label)
                .font(CardMetrics.body)
                .foregroundStyle(Palette.inkFaint)
                .frame(width: CardMetrics.labelColumn, alignment: .leading)
            VStack(alignment: .leading, spacing: 1) {
                Text(value)
                    .font(CardMetrics.body)
                    .foregroundStyle(Palette.ink)
                if let detail {
                    Text(detail)
                        .font(CardMetrics.detail)
                        .foregroundStyle(CardMetrics.detailInk)
                }
            }
            Spacer(minLength: 0)
        }
    }

    /// The rule Stage-1 applies to this address today. The pattern is shown
    /// when it is wider than the address, because "all of fund.com is muted"
    /// is a different fact from "Ann is muted".
    private func ruleFact(_ rule: PersonCard.Rule) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
            Text("Rule")
                .font(CardMetrics.body)
                .foregroundStyle(Palette.inkFaint)
                .frame(width: CardMetrics.labelColumn, alignment: .leading)
            VStack(alignment: .leading, spacing: Space.s2) {
                HStack(spacing: Space.scaled(5)) {
                    Chip(text: rule.disposition.label, tone: tone(rule.disposition))
                    if rule.match_pattern.lowercased() != who.address.lowercased() {
                        Text(rule.match_pattern)
                            .font(Typo.mono(10))
                            .foregroundStyle(Palette.inkFaint)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                if !rule.want_text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Text(rule.want_text)
                        .font(CardMetrics.body)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
    }

    private func recent(_ threads: [PersonCard.RecentThread]) -> some View {
        VStack(alignment: .leading, spacing: Space.s4) {
            Text("Recent")
                .font(Typo.sectionLabel)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.uppercase)
                .padding(.top, Space.s4)
            ForEach(threads) { t in
                let here = t.thread_id == store.threadId
                Button {
                    dismiss()
                    store.openThread(t.thread_id, from: .reader)
                } label: {
                    HStack(spacing: Space.s8) {
                        Text(t.subject.isEmpty ? "(no subject)" : t.subject)
                            .font(CardMetrics.body)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Spacer(minLength: Space.s8)
                        Text(here ? "open" : shortAge(t.last_at))
                            .font(CardMetrics.detail.monospacedDigit())
                            .foregroundStyle(CardMetrics.detailInk)
                    }
                    .frame(minHeight: CardMetrics.rowHeight)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.textAction)
                // The conversation already on screen is not somewhere to go.
                .disabled(here)
            }
        }
    }

    // MARK: - acting on them

    @ViewBuilder
    private var actions: some View {
        #if os(macOS)
            textActions
        #else
            HStack(spacing: Space.s10) {
                door("email", symbol: "square.and.pencil") {
                    dismiss()
                    store.openCompose(ComposeState(to: who.address))
                }
                .disabled(store.compose != nil)
                door("their mail", symbol: "magnifyingglass") {
                    dismiss()
                    store.openSearch(seed: "from:\(who.address)")
                }
            }
            .padding(.top, Space.s4)
        #endif
    }

    #if !os(macOS)
        /// The reader's own reply doors (ChainFoot), so the two read as one set.
        private func door(_ title: String, symbol: String, action: @escaping () -> Void)
            -> some View
        {
            Button(action: action) {
                HStack(spacing: Space.s8) {
                    Image(systemName: symbol)
                        .font(Typo.ui(13, weight: .semibold))
                        .foregroundStyle(Palette.accent)
                    Text(title)
                        .font(Typo.ui(15, weight: .medium))
                        .foregroundStyle(Palette.ink)
                }
                .frame(maxWidth: .infinity)
                .frame(height: 44)
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .glassCapsule(tint: Palette.glassTint)
        }
    #endif

    private var textActions: some View {
        HStack(spacing: Space.s14) {
            Button {
                dismiss()
                store.openCompose(ComposeState(to: who.address))
            } label: {
                Label("Email", systemImage: "square.and.pencil")
            }
            // `openCompose` replaces the slot outright; a live draft is not
            // something a click on a name gets to blank.
            .disabled(store.compose != nil)
            .help(store.compose != nil ? "Finish or close the open draft first" : "Write to \(name)")

            Button {
                dismiss()
                store.openSearch(seed: "from:\(who.address)")
            } label: {
                Label("Their mail", systemImage: "magnifyingglass")
            }
            .help("Search everything from \(who.address)")

            Spacer(minLength: 0)
        }
        .font(CardMetrics.body)
        .buttonStyle(.textAction)
    }

    // MARK: - plumbing

    private func load() async {
        phase = .loading
        do {
            phase = .loaded(try await APIClient.shared.person(who.address))
        } catch is CancellationError {
            // The card closed; nothing to show it on.
        } catch {
            phase = .unavailable
        }
    }

    private func copyAddress() {
        guard Platform.copyToPasteboard(who.address) else { return }
        copied = true
        Task {
            try? await Task.sleep(for: .seconds(1.2))
            copied = false
        }
    }

    private func count(_ n: Int, _ noun: String) -> String {
        n == 1 ? "1 \(noun)" : "\(n.formatted()) \(noun)s"
    }

    /// "3d", "2w", "5mo", and past a year the month it happened: "31mo" is
    /// arithmetic the reader should not have to do.
    private func shortAge(_ iso: String) -> String {
        guard let d = Fmt.date(iso) else { return "" }
        if Date().timeIntervalSince(d) > 365 * 86_400 { return monthYear(iso) }
        return Fmt.relAge(d)
    }

    private func ago(_ iso: String) -> String {
        let age = shortAge(iso)
        if age.isEmpty { return Fmt.shortDate(iso) }
        if age == "now" { return "just now" }
        return age.first?.isNumber == true ? "\(age) ago" : age
    }

    private func monthYear(_ iso: String) -> String {
        guard let d = Fmt.date(iso) else { return "" }
        return d.formatted(.dateTime.month(.abbreviated).year())
    }

    private func tone(_ d: Disposition) -> Color {
        switch d {
        case .surface: Palette.positive
        case .squelch: Palette.inkFaint
        case .filtered: Palette.danger
        }
    }
}
