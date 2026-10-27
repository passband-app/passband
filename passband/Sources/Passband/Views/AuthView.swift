// Auth mail: login codes, password resets, sign-in alerts, account changes, app
// grants, sign-in receipts, verifications. Which of them ask the owner a
// question, for how long, and how they group is Lib/AuthPolicy.swift.
// Metadata only — a body is never fetched to render the list. Reveal is always a
// human action and writes a server-side audit row, so this page never reveals on
// load; digits appear immediately only for codes the arrival flow already
// revealed. Revealed codes live in this view's state and are never persisted.

import SwiftUI

struct AuthView: View {
    @Environment(AppStore.self) private var store

    /// Newer than this counts as live and heads the list.
    private static let liveWindow: TimeInterval = 60 * 60
    /// Cards in the decision rail before it stops growing; past it, the
    /// header offers "Clear all".
    private static let maxDecisionCards = AuthPolicy.clearAllAbove

    enum Filter: String, CaseIterable, Identifiable {
        case all, live, codes, risk
        var id: String { rawValue }
        var label: String {
            switch self {
            case .all: "Everything"
            case .live: "Last hour"
            case .codes: "Codes only"
            case .risk: "Risk"
            }
        }
    }

    @State private var filter: Filter = .all
    @State private var index = 0
    @State private var revealing: SealedMeta?
    @State private var copied = false
    @State private var busy: Int?
    /// Codes revealed this session, by message id; a present key with a nil
    /// value means revealed but unreadable. View state only, never persisted.
    @State private var codes: [Int: String?] = [:]
    /// Ids archived from this page, hidden optimistically (the list is a poll).
    @State private var archived: Set<Int> = []
    /// Member ids of bursts the owner asked to "Review each": keyed by member,
    /// so a burst stays laid out as its members are answered or age out.
    @State private var expandedBursts: Set<Int> = []

    private var decisions = AuthDecisions.shared

    private var visible: [SealedMeta] {
        store.sitrep.sealed.filter { !archived.contains($0.id) }
    }
    /// For the key bindings, which run on a keypress, not per frame. Drawing
    /// reads the `Derived` snapshot instead.
    private var shown: [SealedMeta] { visible.filter { matches($0, filter) } }
    private var focus: SealedMeta? { shown[safe: index] }

    /// Everything the page derives from the sealed list, built ONCE per body.
    /// These used to be computed properties read from every row: each row's
    /// `shown.firstIndex(of:)` re-filtered the whole list, so a redraw was
    /// quadratic in auth mail, and scrolling a long list stuttered.
    private struct Derived {
        var visible: [SealedMeta]
        var shown: [SealedMeta]
        var live: [SealedMeta]
        var archive: [SealedMeta]
        var openDecisions: [SealedMeta]
        /// Counted COLLAPSED, the rule the rail's badge uses too: a burst is
        /// one decision ("All me") whether or not it is laid out for review.
        var decisionCards: [AuthPolicy.Card]
        /// A row's position in `shown`, for selection and j/k.
        var indexById: [Int: Int]
        /// Every listed message by id, for the "explained by" lookup.
        var byId: [Int: SealedMeta]
        var focus: SealedMeta?
    }

    private func derive() -> Derived {
        let all = store.sitrep.sealed
        let visible = all.filter { !archived.contains($0.id) }
        let shown = visible.filter { matches($0, filter) }
        var live: [SealedMeta] = []
        var archive: [SealedMeta] = []
        for m in shown {
            if age(m.received_at) <= Self.liveWindow { live.append(m) } else { archive.append(m) }
        }
        let now = Date()
        let open = visible.filter { decisions.isOpen($0, now: now) }
        var indexById: [Int: Int] = [:]
        for (i, m) in shown.enumerated() { indexById[m.id] = i }
        return Derived(
            visible: visible, shown: shown, live: live, archive: archive,
            openDecisions: open, decisionCards: AuthPolicy.cards(open),
            indexById: indexById,
            byId: Dictionary(all.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a }),
            focus: shown[safe: index])
    }

    /// Seconds since an ISO stamp; huge when missing/invalid (=> treated old).
    private func age(_ iso: String?) -> TimeInterval {
        guard let d = Fmt.date(iso) else { return .greatestFiniteMagnitude }
        return Date().timeIntervalSince(d)
    }

    private func matches(_ m: SealedMeta, _ f: Filter) -> Bool {
        switch f {
        case .live: age(m.received_at) <= Self.liveWindow
        case .codes: AuthCode.isCodeKind(m.kind)
        // The ones that mean someone may be trying to get in.
        case .risk: AuthPolicy.asksQuestion(m)
        case .all: true
        }
    }

    var body: some View {
        let d = derive()
        return VStack(spacing: 0) {
            RoutedHeader(title: "Auth", subtitle: "codes, alerts & resets") {
                HStack(spacing: Space.s12) {
                    StatusDot(
                        color: Palette.lock,
                        label: "\(d.live.count) live · \(d.decisionCards.count) awaiting you",
                        tone: Palette.inkFaint, size: 5)
                    HStack(spacing: Space.scaled(3)) {
                        Kbd("j")
                        Kbd("k")
                        Text("move").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                        Kbd("c")
                        Text("copy").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                        Kbd("r")
                        Text("reveal").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                    }
                }
            }

            HStack(alignment: .top, spacing: Space.s16) {
                mainColumn(d)
                rail(d)
            }
            .padding(.horizontal, Space.page)
            .padding(.vertical, Space.s16)
        }
        .keyBindings(.modal, bindings)
        .overlay {
            if let revealing {
                RevealPanel(meta: revealing) { self.revealing = nil }
            }
        }
        .onChange(of: store.authQueue.count) { _, _ in seedFromArrivals() }
        .onAppear { seedFromArrivals() }
        .onChange(of: d.shown.count) { _, count in
            index = max(0, min(index, max(0, count - 1)))
        }
        .onChange(of: d.focus?.id) { _, _ in copied = false }
        // A page kept across an account switch must not carry the old
        // mailbox's state: a reveal open over the switch would reveal the same
        // int on the NEW daemon.
        .onChange(of: store.epoch) { _, _ in
            revealing = nil
            expandedBursts = []
            archived = []
            codes = [:]
        }
    }

    // MARK: - main column

    private func mainColumn(_ d: Derived) -> some View {
        ScrollView {
            // A plain stack of three children; the rows' stack below is the
            // lazy one, so only the rows on screen are built and a page holding
            // hundreds of auth messages scrolls like one holding ten.
            VStack(alignment: .leading, spacing: Space.s14) {
                if let focus = d.focus {
                    FocusPanel(
                        meta: focus,
                        code: codes[focus.id] ?? nil,
                        revealed: codes.index(forKey: focus.id) != nil,
                        busy: busy == focus.id,
                        copied: copied,
                        onCopy: { copy() },
                        onReveal: { Task { await reveal(focus) } },
                        onArchive: { Task { await archiveFocused(focus) } })
                } else {
                    Text(
                        "nothing here yet. login codes, password resets and sign-in alerts land on this page as they arrive."
                    )
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, Space.s24)
                }

                HStack(spacing: Space.s6) {
                    ForEach(Filter.allCases) { f in
                        Button {
                            filter = f
                            index = 0
                        } label: {
                            Text(f.label)
                                .font(Typo.micro)
                                .padding(.horizontal, Space.scaled(9))
                                .padding(.vertical, Space.scaled(3))
                        }
                        .buttonStyle(.glass)
                        .foregroundStyle(filter == f ? Palette.accent : Palette.inkFaint)
                    }
                    Spacer(minLength: Space.s6)
                    Text("\(d.shown.count) of \(d.visible.count) shown")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }

                ScrollViewReader { proxy in
                    // No GlassEffectContainer: these rows select with a fill, not
                    // a material, so there is nothing here to morph — and a
                    // container re-coordinates its descendants every time a
                    // hovered row redraws, which is a cost with no matching gain.
                    LazyVStack(spacing: 1) {
                        section("Live · last hour", rows: d.live, d)
                        section("Archive", rows: d.archive, d)
                    }
                    .onChange(of: index) { _, i in
                        guard let m = d.shown[safe: i] else { return }
                        withAnimation(Motion.scrollFollow) {
                            proxy.scrollTo(m.id, anchor: .center)
                        }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .top)
    }

    @ViewBuilder
    private func section(_ label: String, rows: [SealedMeta], _ d: Derived) -> some View {
        if !rows.isEmpty {
            Text(label)
                .font(Typo.sectionLabel)
                .foregroundStyle(Palette.inkFaintest)
                .textCase(.uppercase)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, Space.s10)
                .padding(.bottom, Space.s4)
            ForEach(rows) { m in
                let i = d.indexById[m.id] ?? 0
                AuthRow(
                    meta: m, selected: i == index, code: codes[m.id] ?? nil,
                    explained: explainedNote(m, d),
                    answered: answeredLine(m),
                    onUndo: { decisions.set(m, nil) },
                    onSelect: {
                        index = i
                        revealing = m
                    })
                .id(m.id)
            }
        }
    }

    /// "You said: That was me", on a row that asked and was answered, beside
    /// its Undo.
    private func answeredLine(_ m: SealedMeta) -> String? {
        guard AuthPolicy.offersAnswer(m), let v = decisions.decision(m) else { return nil }
        return AuthPolicy.answeredLine(m, v)
    }

    /// The quiet line on a row an earlier code, link or receipt explains.
    private func explainedNote(_ m: SealedMeta, _ d: Derived) -> String? {
        guard AuthPolicy.isExplained(m), let by = m.explained_by else { return nil }
        let explainer = d.byId[by]
        return AuthPolicy.explainedNote(
            explainer: explainer,
            service: explainer.map { SenderCache.resolved($0.sender).displayName })
    }

    // MARK: - rail

    private func rail(_ d: Derived) -> some View {
        VStack(spacing: Space.s12) {
            VStack(alignment: .leading, spacing: Space.scaled(9)) {
                AuthDecisionHeader(
                    open: d.openDecisions, cardCount: d.decisionCards.count,
                    showsClearAll: AuthPolicy.showsClearAll(d.decisionCards))
                if d.openDecisions.isEmpty {
                    EmptyNote(
                        "nothing to rule on. sign-in alerts, account changes and reset requests show up here.")
                } else {
                    AuthDecisionStack(
                        open: d.openDecisions, limit: Self.maxDecisionCards,
                        expanded: $expandedBursts,
                        onNotMine: { m in revealing = m })
                }
            }
            .zonePadding()
            .frame(maxWidth: .infinity, alignment: .leading)
            .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.lockSoft.opacity(0.7))

            ShredderCard()
        }
        .frame(width: Typo.measure(306))
    }

    // MARK: - keymap

    private var bindings: [KeyBinding] {
        [
            // Floored at 0 so an empty list never parks the cursor at -1.
            KeyBinding("j", "next") { index = max(0, min(shown.count - 1, index + 1)) },
            KeyBinding("k", "prev") { index = max(0, index - 1) },
            KeyBinding("ArrowDown", "next") { index = max(0, min(shown.count - 1, index + 1)) },
            KeyBinding("ArrowUp", "prev") { index = max(0, index - 1) },
            KeyBinding("r", "reveal") { if let focus { Task { await reveal(focus) } } },
            KeyBinding("Enter", "reveal") { if let focus { Task { await reveal(focus) } } },
            KeyBinding("c", "copy code") { copy() },
        ]
    }

    // MARK: - actions

    /// Code kinds resolve to digits inline; anything whose point is its prose
    /// opens the full one-time reveal panel instead.
    private func reveal(_ m: SealedMeta) async {
        guard AuthCode.isCodeKind(m.kind) else {
            revealing = m
            return
        }
        guard codes.index(forKey: m.id) == nil else { return }
        busy = m.id
        defer { busy = nil }
        do {
            let revealed = try await APIClient.shared.revealSealed(m.id)
            codes[m.id] = AuthCode.extract(revealed.body)
        } catch {
            codes[m.id] = String?.none
            store.pushToast(errText(error, "reveal failed"), .error)
        }
    }

    private func copy() {
        guard let focus, let code = codes[focus.id] ?? nil else { return }
        Clip.copy(code, flashing: $copied)
    }

    private func archiveFocused(_ m: SealedMeta) async {
        busy = m.id
        defer { busy = nil }
        do {
            try await APIClient.shared.actionArchive(m.id)
            archived.insert(m.id)
            store.pushToast("archived", .success)
        } catch {
            store.pushToast(errText(error, "archive failed"), .error)
        }
    }

    private func seedFromArrivals() {
        for entry in store.authQueue where codes.index(forKey: entry.meta.id) == nil {
            codes[entry.meta.id] = entry.code
        }
    }
}

// MARK: - focus panel

/// The blown-up selected message: identity, kind, and — for a code — digits.
private struct FocusPanel: View {
    let meta: SealedMeta
    let code: String?
    let revealed: Bool
    let busy: Bool
    let copied: Bool
    let onCopy: () -> Void
    let onReveal: () -> Void
    let onArchive: () -> Void

    private var isCode: Bool { AuthCode.isCodeKind(meta.kind) }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s14) {
            HStack {
                Text("In focus")
                    .font(Typo.sectionLabel)
                    .foregroundStyle(Palette.accent)
                    .textCase(.uppercase)
                Spacer()
                Text("arrived \(Fmt.relAge(meta.received_at)) ago")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
            }

            HStack(spacing: Space.s12) {
                Avatar(sender: meta.sender, size: 42)
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(SenderCache.resolved(meta.sender).displayName)
                        .font(Typo.ui(15, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .help(meta.sender)
                    Text(meta.subject)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaint)
                        .lineLimit(2)
                        .help(meta.subject)
                }
                Spacer(minLength: Space.s8)
                Chip(
                    text: AuthCopy.label(meta.kind), tone: Palette.lock,
                    symbol: AuthCopy.symbol(meta.kind), filled: true)
            }

            if isCode {
                digits
                actions
            } else {
                VStack(alignment: .leading, spacing: Space.s8) {
                    Button(action: onReveal) {
                        HStack(spacing: Space.s6) {
                            Image(systemName: "eye")
                            Text("Open it")
                            Kbd("r")
                        }
                        .font(Typo.ui(12, weight: .medium))
                        .padding(.horizontal, Space.s12).padding(.vertical, Space.scaled(5))
                    }
                    .buttonStyle(.glassProminent)
                    .tint(Palette.lock)
                    Text(
                        "This one is a message, not a code. Opening it fetches the body once and records an audit entry."
                    )
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                }
            }
        }
        .zonePadding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.lockSoft.opacity(0.8))
    }

    private var digits: some View {
        HStack(spacing: Space.scaled(7)) {
            if revealed, let code {
                ForEach(Array(code.enumerated()), id: \.offset) { _, ch in
                    Text(String(ch))
                        .font(Typo.mono(26, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .frame(width: Space.scaled(40), height: Space.scaled(52))
                        .background(
                            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                                .fill(Palette.lockSoft)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                                .strokeBorder(Palette.lock.opacity(0.35), lineWidth: 1))
                }
            } else {
                ForEach(0..<6, id: \.self) { _ in
                    Text("·")
                        .font(Typo.mono(26, weight: .semibold))
                        .foregroundStyle(Palette.inkFaintest)
                        .frame(width: Space.scaled(40), height: Space.scaled(52))
                        .background(
                            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                                .fill(Palette.hairline.opacity(0.5)))
                }
            }
        }
        .accessibilityLabel("login code")
    }

    private var actions: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            HStack(spacing: Space.s8) {
                if revealed, let code, !code.isEmpty {
                    Button(action: onCopy) {
                        Label(
                            copied ? "Copied" : "Copy code",
                            systemImage: copied ? "checkmark" : "doc.on.doc"
                        )
                        .font(Typo.ui(12, weight: .medium))
                        .padding(.horizontal, Space.s12).padding(.vertical, Space.scaled(5))
                    }
                    .buttonStyle(.glassProminent)
                    .tint(Palette.lock)

                    Button(action: onArchive) {
                        Label("Used it · archive", systemImage: "archivebox")
                            .font(Typo.ui(12))
                            .padding(.horizontal, Space.s10).padding(.vertical, Space.scaled(5))
                    }
                    .buttonStyle(.glass)
                    .foregroundStyle(Palette.inkDim)
                    .disabled(busy)
                } else {
                    Button(action: onReveal) {
                        HStack(spacing: Space.s6) {
                            Image(systemName: "eye")
                            Text(busy ? "Revealing…" : "Reveal code")
                            Kbd("r")
                        }
                        .font(Typo.ui(12, weight: .medium))
                        .padding(.horizontal, Space.s12).padding(.vertical, Space.scaled(5))
                    }
                    .buttonStyle(.glassProminent)
                    .tint(Palette.lock)
                    .disabled(busy)
                }
            }
            Text(note)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var note: String {
        if revealed, let code, !code.isEmpty {
            return
                "Held in memory for this screen only — Passband never stores it. Revealing was written to the audit log."
        }
        if revealed {
            return "Revealed, but no code could be read from this one. Open it to read it yourself."
        }
        return
            "Select a message to view its code or link. These messages are also available in your email."
    }
}

// MARK: - rows

private struct AuthRow: View {
    let meta: SealedMeta
    let selected: Bool
    let code: String?
    /// "explained by your GitHub sign-in", on an alert that needs no ruling.
    let explained: String?
    /// "You said: That was me", with its Undo, once the row's question is answered.
    let answered: String?
    let onUndo: () -> Void
    /// Focus the row AND open the mail. One click, because the mail is what a
    /// row on this page is FOR — a click that only moved a cursor left the page
    /// looking inert, and the second click that used to open it was undiscoverable.
    ///
    /// Opening reveals, and revealing is audited server-side. That is the
    /// contract this page has always had and it is not weakened here: a click is
    /// a human action, which is the only thing the seal ever asked for. What it
    /// never does is reveal on LOAD.
    let onSelect: () -> Void

    var body: some View {
        ListRow(
            selected: selected, cornerRadius: Radius.row, tint: Palette.lock,
            hPadding: Space.s10, vPadding: Space.s6, action: onSelect
        ) { _, _ in
            HStack(spacing: Space.scaled(9)) {
                Avatar(sender: meta.sender, size: 20)
                Text(SenderCache.resolved(meta.sender).displayName)
                    .font(Typo.ui(12, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .frame(width: Typo.measure(150), alignment: .leading)
                    .help(meta.sender)
                HStack(spacing: Space.s6) {
                    Text(meta.subject)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                        .help(meta.subject)
                    if let explained {
                        Label(explained, systemImage: "checkmark.circle")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                            .layoutPriority(1)
                    }
                    if let answered {
                        Text(answered)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                            .lineLimit(1)
                            .layoutPriority(1)
                        Button("Undo", action: onUndo)
                            .buttonStyle(.glass)
                            .controlSize(.mini)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkDim)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                if let code, !code.isEmpty {
                    // Already revealed this session — no reason to ask twice.
                    Text(code)
                        .font(Typo.mono(12, weight: .semibold))
                        .foregroundStyle(Palette.lock)
                } else if meta.isCritical {
                    Chip(
                        text: AuthCopy.label(meta.kind), tone: Palette.danger,
                        symbol: "exclamationmark.triangle.fill")
                } else {
                    Chip(
                        text: AuthCopy.label(meta.kind), tone: Palette.inkFaint,
                        symbol: AuthCopy.symbol(meta.kind))
                }
                Text(Fmt.relAge(meta.received_at))
                    .font(Typo.num(10))
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(width: Space.scaled(30), alignment: .trailing)
            }
        }
    }
}

// MARK: - shredder

/// Shredder panel. Every number comes from the server's `shred_log` ledger,
/// nothing is estimated. This switch is the only way automatic deletion turns
/// on, and shredding means Gmail Trash (recoverable 30 days), never a purge.
private struct ShredderCard: View {
    @Environment(AppStore.self) private var store
    @State private var stats: ShredStats?
    @State private var busy = false

    var body: some View {
        Group {
            if let stats {
                VStack(alignment: .leading, spacing: Space.scaled(9)) {
                    HStack(spacing: Space.s8) {
                        Image(systemName: "trash")
                            .font(Typo.ui(12))
                            .foregroundStyle(Palette.inkFaint)
                        Text("Auto-shredder")
                            .font(Typo.zoneTitle)
                            .foregroundStyle(Palette.ink)
                        Spacer()
                        Toggle(
                            "",
                            isOn: Binding(
                                get: { stats.enabled },
                                set: { _ in Task { await toggle() } })
                        )
                        .labelsHidden()
                        .toggleStyle(.switch)
                        .controlSize(.mini)
                        .tint(Palette.accent)
                        .disabled(busy || !stats.write_ready)
                        .help(
                            stats.write_ready
                                ? "move auth mail older than the window to Gmail Trash"
                                : "needs the write credential")
                    }

                    if !stats.write_ready {
                        Label {
                            Text(
                                "Needs the write credential — run `squelchd auth --write`. Nothing can be shredded until then."
                            )
                        } icon: {
                            Image(systemName: "exclamationmark.shield")
                        }
                        .font(Typo.micro)
                        .foregroundStyle(Palette.warn)
                        .fixedSize(horizontal: false, vertical: true)
                    } else if stats.enabled {
                        Text(
                            "Auth mail older than \(stats.after_days) days moves to Gmail Trash automatically, where it stays recoverable for 30 days."
                                + (stats.pending > 0 ? " \(stats.pending) eligible right now." : "")
                        )
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize(horizontal: false, vertical: true)
                    } else {
                        Text(
                            "Off. Turn it on and auth mail older than \(stats.after_days) days moves to Gmail Trash — recoverable there for 30 days, never deleted outright."
                                + (stats.pending > 0
                                    ? " \(stats.pending) would go on the next pass." : "")
                        )
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize(horizontal: false, vertical: true)
                    }

                    Text(
                        "\(stats.shredded_recent) shredded in the last 30 days"
                            + (stats.shredded_total > stats.shredded_recent
                                ? " · \(stats.shredded_total) all time" : "")
                    )
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                }
                .zonePadding()
                .frame(maxWidth: .infinity, alignment: .leading)
                .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.glassTint)
            }
        }
        .task { await load() }
    }

    private func load() async {
        // Apply the policy now instead of waiting out the server's hourly timer;
        // it is a server-side no-op when the shredder is off or can't run.
        if let run = try? await APIClient.shared.runShredder() {
            stats = run.stats
        } else {
            // Fall back to a plain read — an older daemon may lack the route.
            stats = try? await APIClient.shared.getShredder()
        }
    }

    private func toggle() async {
        guard let current = stats, !busy else { return }
        busy = true
        defer { busy = false }
        do {
            let next = try await APIClient.shared.setShredder(
                ShredderPatch(enabled: !current.enabled))
            stats = next
            if next.enabled {
                let run = try await APIClient.shared.runShredder()
                stats = run.stats
                if run.shredded > 0 {
                    store.pushToast("shredded \(run.shredded) old auth message(s)", .success)
                }
            }
        } catch {
            store.pushToast(errText(error, "could not change the shredder"), .error)
        }
    }
}
