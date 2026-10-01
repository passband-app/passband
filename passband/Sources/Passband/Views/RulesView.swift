// Sender rules from GET /client/rules, GROUPED: one section per disposition
// (allow, filter, mute), and inside each one the rules gather by registrable
// domain with subdomains nested under a header. A rule's `*@` never reaches the
// screen — `*@futureme.org` is a rule about futureme.org and reads as one — and
// a whole-domain rule carries that domain's icon.
//
// The disposition chip is GONE from the row: the section it sits in says the
// same thing once instead of eleven times, which is what buys the width the
// want text now has.
//
// `RuleGrouping` owns the shape and is unit-tested (`test.sh rule-grouping`),
// because the nesting is invisible on a mailbox whose rules name a different
// domain each — the shape has to be assertable without a window.
//
// Keys: j/k select · n new · Enter/e edit · x delete, undo-first — the 5s toast
// recreates the deleted rule. j/k walk DRAWN order, not fetch order. A click
// opens the rule: selecting a row has nothing to do on its own that the editor
// does not, so the pointer goes straight there (#308).

import SwiftUI

struct RulesView: View {
    @Environment(AppStore.self) private var store

    @State private var rulesState: Loadable<[SenderRule]> = .loading
    @State private var index = 0

    /// The loaded rules, which a reload deliberately keeps on screen while the
    /// next fetch is in flight.
    private var rules: [SenderRule] { rulesState.value ?? [] }

    /// The display tree: sections, domain groups, rows.
    private var sections: [RuleGrouping.Section] { RuleGrouping.sections(rules) }

    /// Rules in DRAWN order. Selection indexes this, never `rules` — the fetch
    /// order and the page order are different sequences, and j/k has to walk
    /// the one the reader can see.
    private var ordered: [SenderRule] { RuleGrouping.ordered(sections) }

    /// rule id -> its position in `ordered`, so a row can tell whether it is the
    /// selected one without searching the tree on every rebuild.
    private var ordinals: [Int: Int] {
        Dictionary(uniqueKeysWithValues: ordered.enumerated().map { ($0.element.id, $0.offset) })
    }

    /// How many currently-loaded updates each rule matched, counted client-side
    /// from the store rather than through a new endpoint.
    private var matchCounts: [Int: Int] {
        var counts: [Int: Int] = [:]
        for u in store.sitrep.standing + store.sitrep.new + store.sitrep.open {
            if let ruleId = u.matched_rule { counts[ruleId, default: 0] += 1 }
        }
        return counts
    }

    var body: some View {
        // A VStack, NOT a Group. `Group` applies a modifier to EACH of its
        // children rather than to the group as a whole, and the populated
        // branch below has two: the scrolling body and the bar under it. The
        // `maxHeight: .infinity` on the frame was therefore claimed TWICE, so
        // the enclosing VStack in RoutedHost split the window between them and
        // the page rendered at half height with its footer stranded in the
        // middle. One real container means one frame, and the bar keeps its
        // natural height while the body takes what is left.
        VStack(spacing: 0) {
            if rulesState.isLoading && rules.isEmpty {
                BandNote("loading rules…")
            } else if let error = rulesState.error {
                BandNote(error)
            } else if rules.isEmpty {
                VStack(alignment: .leading, spacing: Space.s6) {
                    HStack(spacing: Space.s4) {
                        Text("no rules yet. press").font(Typo.rowSub)
                        Kbd("n")
                        Text("to create one, or").font(Typo.rowSub)
                        Kbd("t")
                        Text("on any message.").font(Typo.rowSub)
                    }
                    .foregroundStyle(Palette.inkFaintest)
                }
                .padding(.horizontal, Space.scaled(22))
                .padding(.vertical, Space.s24)
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        let ordinals = ordinals
                        LazyVStack(spacing: 1) {
                            ForEach(sections) { section in
                                RuleSectionHeader(
                                    disposition: section.disposition, count: section.ruleCount)
                                ForEach(section.groups) { group in
                                    if group.showsHeader {
                                        DomainHeaderRow(group: group)
                                    }
                                    ForEach(group.rows) { row in
                                        let i = ordinals[row.rule.id] ?? 0
                                        RuleRow(
                                            row: row, group: group, selected: i == index,
                                            matchCount: matchCounts[row.rule.id] ?? 0,
                                            onOpen: {
                                                index = i
                                                edit(row.rule)
                                            })
                                        .id(row.rule.id)
                                    }
                                }
                            }
                        }
                        .padding(.horizontal, Space.scaled(18))
                        .padding(.bottom, Space.s10)
                    }
                    .onChange(of: index) { _, i in
                        guard let rule = ordered[safe: i] else { return }
                        withAnimation(Motion.scrollFollow) {
                            proxy.scrollTo(rule.id, anchor: .center)
                        }
                    }
                }
                footer
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .keyBindings(.modal, bindings)
        .task {
            RulesReload.shared.handler = { await load() }
            await load()
        }
        .onDisappear { RulesReload.shared.handler = nil }
        .onChange(of: rules.count) { _, count in
            index = max(0, min(index, max(0, count - 1)))
        }
    }

    private var footer: some View {
        KeyHintBar(hints: [
            KeyHint(["j", "k"], "select"),
            KeyHint("n", "new"),
            KeyHint(["e", "↵"], "edit"),
            KeyHint("x", "delete"),
        ])
    }

    private var bindings: [KeyBinding] {
        [
            KeyBinding("j", "next") { step(+1) },
            KeyBinding("k", "prev") { step(-1) },
            KeyBinding("ArrowDown", "next") { step(+1) },
            KeyBinding("ArrowUp", "prev") { step(-1) },
            KeyBinding("n", "new rule") { create() },
            KeyBinding("e", "edit rule") { if let r = ordered[safe: index] { edit(r) } },
            KeyBinding("Enter", "edit rule") { if let r = ordered[safe: index] { edit(r) } },
            KeyBinding("x", "delete rule") {
                if let r = ordered[safe: index] { Task { await delete(r) } }
            },
        ]
    }

    /// Clamped to the drawn rules, and never below 0: an empty list used to
    /// park the cursor at -1.
    private func step(_ delta: Int) {
        index = max(0, min(ordered.count - 1, index + delta))
    }

    private func create() {
        store.openRuleEditor(
            RuleEditorRequest(rule: nil, onSaved: { Task { await load() } }))
    }

    private func edit(_ rule: SenderRule) {
        store.openRuleEditor(
            RuleEditorRequest(rule: rule, onSaved: { Task { await load() } }))
    }

    private func delete(_ rule: SenderRule) async {
        do {
            try await APIClient.shared.deleteRule(rule.id)
            Analytics.capture("rule_deleted", ["disposition": rule.disposition.rawValue])
            // Optimistic removal; a re-fetch happens on undo or next open.
            rulesState.value?.removeAll { $0.id == rule.id }
            // The undo toast recreates the rule from these cached values,
            // disposition included and explicit: an undo restores what was
            // there, it does not re-infer it from the want text. `sweep` is
            // never sent, so restoring a mute rule brings the rule back without
            // the side effect of resolving that sender's open mail — undo puts
            // things back, it does not go clear the inbox.
            store.pushUndo(
                kind: .ruleDelete, messageId: rule.id,
                label: "deleted rule \(rule.match_pattern)"
            ) {
                try await APIClient.shared.createRule(
                    CreateRuleBody(
                        match_pattern: rule.match_pattern, want: rule.want_text,
                        disposition: rule.disposition))
                await RulesReload.shared.reload()
            }
        } catch {
            store.pushToast(errText(error, "delete failed"), .error)
        }
    }

    private func load() async {
        await $rulesState.load("rules failed") {
            try await APIClient.shared.listRules()
        }
    }
}

/// Lets an undo fired from the global toast stack — which outlives this view's
/// closures — ask the rules list to re-pull if it is still on screen.
@MainActor
final class RulesReload {
    static let shared = RulesReload()
    /// Set only while a RulesView is mounted; nil means the undo has nothing to
    /// refresh, which is correct.
    var handler: (@MainActor () async -> Void)?
    private init() {}
    func reload() async { await handler?() }
}

/// Chip colour per disposition, kept off the wire type so the model layer stays
/// free of SwiftUI.
extension Disposition {
    fileprivate var tone: Color {
        switch self {
        case .surface: Palette.positive
        case .squelch: Palette.inkFaint
        case .filtered: Palette.danger
        }
    }
}

/// The disposition band: one line naming what this run of rules does, so the
/// row beneath it does not have to. Its tone is the chip colour the rows used
/// to carry each.
private struct RuleSectionHeader: View {
    let disposition: Disposition
    let count: Int

    var body: some View {
        HStack(spacing: Space.s8) {
            Text(disposition.label.uppercased())
                .font(Typo.ui(10, weight: .semibold))
                .tracking(0.9)
                .foregroundStyle(disposition.tone)
            Text("\(count)")
                .font(Typo.num(10))
                .foregroundStyle(Palette.inkFaintest)
            Rectangle()
                .fill(Palette.hairline)
                .frame(height: 1)
        }
        .padding(.horizontal, Space.scaled(11))
        .padding(.top, Space.scaled(18))
        .padding(.bottom, Space.s6)
        .help(disposition.hint)
    }
}

/// A domain carrying MORE THAN ONE rule: the icon and the domain sit here once,
/// and the rules underneath say only what distinguishes them. A domain with a
/// single rule never draws this — a header over one child is just an indent
/// with extra steps.
private struct DomainHeaderRow: View {
    let group: RuleGrouping.DomainGroup

    var body: some View {
        HStack(spacing: Space.scaled(9)) {
            RuleIcon(domain: group.domain, eligible: group.iconEligible, size: 15)
            Text(group.domain)
                .font(Typo.mono(11))
                .foregroundStyle(Palette.inkDim)
            Text("\(group.rows.count) rules")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Space.scaled(11))
        .padding(.top, Space.scaled(7))
        .padding(.bottom, Space.s2)
    }
}

/// The icon for a domain. Falls back to the base label's own letters rather
/// than a mailbox's initials — under a `garmin.com` header "GA" is the brand
/// and "GC" (garmin·com) is a parsing artefact.
private struct RuleIcon: View {
    let domain: String
    let eligible: Bool
    var size: CGFloat = 15

    private var baseLabel: String {
        domain.split(separator: ".").first.map(String.init) ?? domain
    }

    var body: some View {
        Avatar(
            sender: baseLabel, size: size,
            // nil unless the group earned it — see RuleGrouping.iconPattern.
            domainOverride: eligible ? domain : nil)
    }
}

private struct RuleRow: View {
    let row: RuleGrouping.Row
    let group: RuleGrouping.DomainGroup
    let selected: Bool
    let matchCount: Int
    /// Selects the row and opens it in the editor, on one click.
    let onOpen: () -> Void

    private var rule: SenderRule { row.rule }

    var body: some View {
        ListRow(
            selected: selected, cornerRadius: 8, hPadding: 11, vPadding: 7, action: onOpen
        ) { selected, _ in
            HStack(spacing: Space.scaled(9)) {
                if row.nested {
                    // Indented under its domain header, which already carries
                    // the icon. The rail is what makes the nesting readable at
                    // a glance instead of just a gap.
                    Rectangle()
                        .fill(Palette.hairline)
                        .frame(width: 1, height: Space.scaled(13))
                        .padding(.leading, 7)
                        .padding(.trailing, 6)
                } else if RuleGrouping.isWildcard(rule.match_pattern) {
                    RuleIcon(domain: group.domain, eligible: group.iconEligible, size: 15)
                } else {
                    // A mailbox rule names a person: initials, no network.
                    Avatar(sender: rule.match_pattern, size: 15)
                }
                Text(row.label)
                    .font(Typo.mono(11))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(width: Space.scaled(row.nested ? 196 : 218), alignment: .leading)
                    .help(rule.match_pattern)
                Text(rule.want_text.isEmpty ? "·" : rule.want_text)
                    .font(Typo.micro)
                    .foregroundStyle(
                        rule.want_text.isEmpty ? Palette.inkFaintest : Palette.inkDim
                    )
                    .lineLimit(selected ? nil : 1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .help(rule.want_text)
                Text("\(matchCount)×")
                    .font(Typo.num(10))
                    .foregroundStyle(matchCount == 0 ? Palette.inkFaintest : Palette.accent)
                    .help(
                        matchCount == 0
                            ? "no currently-loaded updates match this rule"
                            : "\(matchCount) loaded update(s) matched")
                Text(Fmt.relAge(rule.updated_at).isEmpty ? "·" : Fmt.relAge(rule.updated_at))
                    .font(Typo.num(10))
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(width: Space.scaled(34), alignment: .trailing)
            }
        }
    }
}

// MARK: - rule editor

/// The `t` tune-sender modal, and the rules view's create/edit.
///
/// NL-FIRST: the plain-English want text IS the rule, and it is stored VERBATIM
/// — nothing rewrites it on the way out, because the daemon hands it to Stage 2
/// as the owner's own instruction. The glob that decides WHO the rule covers is
/// derived from the sender on screen and demoted behind `advanced`.
///
/// The disposition is NOT asked for. The daemon reads the same sentence and
/// resolves allow/mute/filter from it, so the simple card has exactly one
/// question on it; advanced carries an override for the times the guess is
/// wrong. That inference sees the OWNER'S RULE TEXT ONLY — never a subject,
/// body or header — so no sender can write themselves an allow rule.
struct RuleEditor: View {
    let request: RuleEditorRequest
    let onClose: () -> Void

    @Environment(AppStore.self) private var store

    @State private var pattern: String
    @State private var want: String
    /// The OVERRIDE. nil means auto: the request goes out with no disposition and
    /// the daemon resolves one from the want text. Edit opens on the rule's
    /// current value, which is explicit by definition — the rule already is
    /// something, and reopening it must not silently re-roll that.
    @State private var disposition: Disposition?
    /// Whether the raw match pattern is on screen and editable.
    @State private var advanced: Bool
    @State private var saving = false
    @State private var error: String?
    /// Rules whose match_pattern already covers `request.sender`; nil until the
    /// fetch settles, so "none yet" is never claimed before we know.
    @State private var inEffect: [SenderRule]?
    /// A small model's guess at the sentence, shown as the placeholder and
    /// taken with tab. Tune mode only; nil until (and unless) one arrives.
    /// It is SHOWN, never saved on its own: the owner takes it and saves it.
    @State private var suggestion: String?
    /// Whether the suggestion was ever actually on screen: it arrived while the
    /// field was still empty. One that lands behind typing was never offered.
    @State private var suggestionShown = false
    @FocusState private var focusedField: FocusTarget?
    /// What the card opened on BEFORE any draft was laid over it. Leaving the
    /// card back on these values is not a draft, it is nothing written.
    private let pristine: RuleDraft
    /// When this card appeared; see `dismissFromScrim`.
    @State private var openedAt = Date()

    private enum FocusTarget { case pattern, want }

    enum Mode { case tune, create, edit }

    /// `draft` is what this same card held when it was last dismissed unsaved
    /// (#306); the opener looks it up by `request.draftKey`.
    init(request: RuleEditorRequest, draft: RuleDraft? = nil, onClose: @escaping () -> Void) {
        self.request = request
        self.onClose = onClose
        let start: RuleDraft
        if let rule = request.rule {
            // Editing IS pattern work: hiding the glob would hide the thing
            // being edited.
            start = RuleDraft(
                pattern: rule.match_pattern, want: rule.want_text,
                disposition: rule.disposition, advanced: true)
        } else {
            start = RuleDraft(
                pattern: request.pattern
                    ?? request.sender.map(SenderID.patternFromSender) ?? "",
                want: request.want ?? "",
                // nil = auto, which is what every ordinary create/tune sends. A
                // caller that names one is deliberately overriding on the user's
                // behalf, so its choice travels as an explicit value.
                disposition: request.disposition,
                // With no sender there is nothing to derive a glob from, so the
                // glob field is the only way to say who the rule is about.
                advanced: request.sender == nil)
        }
        pristine = start
        let shown = draft.flatMap { $0.origin == start.fingerprint ? $0 : nil } ?? start
        _pattern = State(initialValue: shown.pattern)
        _want = State(initialValue: shown.want)
        _disposition = State(initialValue: shown.disposition)
        _advanced = State(initialValue: shown.advanced)
    }

    private var mode: Mode {
        if request.rule != nil { return .edit }
        if request.sender != nil { return .tune }
        return .create
    }

    private var title: String {
        switch mode {
        case .edit: "edit rule"
        case .create: "new rule"
        case .tune: "tune sender"
        }
    }

    var body: some View {
        OverlayScrim(onDismiss: dismissFromScrim) {
            ModalCard(width: 470) {
                header
                alreadyInEffect
                wantField
                advancedSection

                if let error {
                    Text(error).font(Typo.micro).foregroundStyle(Palette.danger)
                }

                footer
            }
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Escape", "cancel", allowInInput: true) { dismiss() },
            // Tab takes a suggestion while one is on offer in the empty field.
            // Otherwise it only means something while the override is on
            // screen; with advanced closed it DECLINES, so the key falls
            // through to AppKit and moves focus the way it does in every card.
            KeyBinding(declining: "Tab", "use suggestion / cycle disposition", allowInInput: true) {
                if acceptSuggestion() { return true }
                guard advanced else { return false }
                cycle(1)
                return true
            },
            KeyBinding(declining: "shift+Tab", "cycle disposition (back)", allowInInput: true) {
                guard advanced else { return false }
                cycle(-1)
                return true
            },
            // ⌘-chorded because the want field owns every bare letter. Taught
            // by the advanced toggle's tooltip, not the footer, which keeps
            // only the keys every save goes through.
            KeyBinding("d", "advanced", meta: true, allowInInput: true) { toggleAdvanced() },
            // BOTH SPELLINGS SAVE. Bare Enter is this card's own convention and
            // the footer teaches it. ⌘Enter is the
            // one every OTHER multi-line surface in the app uses — the group
            // editor, the composer, the inline reply — so a hand arriving from
            // any of them tries it first and, until now, got nothing. They
            // coexist because dispatch matches `meta` exactly in both
            // directions, so neither binding can answer for the other.
            KeyBinding("Enter", "save rule", allowInInput: true) { submit() },
            KeyBinding("Enter", "save rule", meta: true, allowInInput: true) { submit() },
        ])
        .onAppear {
            // The want text is the rule, so it takes focus everywhere except
            // create-from-scratch, where there is no sender and the empty glob
            // is the first thing that has to be written.
            focusedField = mode == .create ? .pattern : .want
        }
        .task { await loadInEffect() }
        .task { await loadSuggestion() }
    }

    /// WHO the rule is about. In tune mode the sender IS the title, so there is
    /// no eyebrow over it; edit and create have no sender to lead with and get
    /// a plain title instead. The sender is email-derived, so it is rendered as
    /// Text only, never as markup.
    @ViewBuilder
    private var header: some View {
        if let sender = request.sender {
            let resolved = SenderCache.resolved(sender)
            HStack(spacing: Space.s10) {
                Avatar(sender: sender, size: 30)
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(resolved.displayName)
                        .font(Typo.zoneTitle)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                    if resolved.displayName.lowercased() != SenderID.address(sender) {
                        Text(SenderID.address(sender))
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkFaint)
                            .lineLimit(1)
                    }
                }
            }
        } else {
            VStack(alignment: .leading, spacing: Space.scaled(3)) {
                Text(title)
                    .font(Typo.zoneTitle)
                    .foregroundStyle(Palette.ink)
                subtitle
            }
        }
    }

    /// The rule itself, in the user's own words, and now the ONLY question on
    /// the simple card: the sentence decides both what Stage 2 does with the
    /// mail and which disposition the rule lands on. Multi-line because a real
    /// one is a sentence, not a keyword. The examples stay polarity-diverse on
    /// purpose, since a card that only ever showed "i dont care about X" would
    /// teach people this box is a mute button.
    private var wantField: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            // The one question on the card, so it reads at body size rather than
            // as a faint field caption.
            Text(wantLabel)
                .font(Typo.row)
                .foregroundStyle(Palette.inkDim)
            HStack(alignment: .top, spacing: Space.s8) {
                TextField(
                    "", text: $want,
                    prompt: Text(offered ?? "e.g. only order confirmations, or: always show these"),
                    axis: .vertical
                )
                .textFieldStyle(.plain)
                .lineLimit(2...5)
                .focused($focusedField, equals: .want)
                if offered != nil {
                    // The key on a Mac, a tap target on a phone; both take it.
                    Button { _ = acceptSuggestion(requireFocus: false) } label: {
                        #if os(macOS)
                            Kbd("tab")
                        #else
                            Text("use").font(Typo.micro).foregroundStyle(Palette.accent)
                        #endif
                    }
                    .buttonStyle(.plain)
                    .help("use this suggestion")
                    .transition(.opacity)
                }
            }
            .fieldWell()
        }
    }

    /// The suggestion, while it is still on offer: the field is empty.
    private var offered: String? {
        want.isEmpty ? suggestion : nil
    }

    /// Enter on an empty field with a suggestion showing reads as "yes, that",
    /// so it takes the suggestion into the field instead of silently refusing
    /// to save nothing. It does not save it: the second Enter does, after the
    /// sentence has been in the field where it can be read and edited.
    private func submit() {
        if acceptSuggestion(requireFocus: false) { return }
        Task { await save() }
    }

    /// Put the suggestion in the field. From the keyboard only while the want
    /// field has focus, so tab in the pattern field keeps its own meaning.
    private func acceptSuggestion(requireFocus: Bool = true) -> Bool {
        guard let offered, !requireFocus || focusedField == .want else { return false }
        want = offered
        focusedField = .want
        return true
    }

    /// Best-effort, like the in-effect strip: no model, an old daemon, a slow
    /// answer or no pattern all leave the generic example in place.
    private func loadSuggestion() async {
        guard mode == .tune, let sender = request.sender, want.isEmpty else { return }
        guard let answer = try? await APIClient.shared.suggestRule(
            sender: SenderID.address(sender))
        else { return }
        withAnimation(Motion.disclose) { suggestion = answer }
        if want.isEmpty { suggestionShown = true }
    }

    private var wantLabel: String {
        mode == .tune ? "what do you want from this sender?" : "what do you want?"
    }

    /// The override. Lives in advanced because the honest default is now "let
    /// the daemon read the sentence", and a control on the front of the card
    /// would re-ask a question that has already been answered above it.
    private var dispositionOverride: some View {
        VStack(alignment: .leading, spacing: Space.scaled(5)) {
            HStack(spacing: Space.s4) {
                Text("disposition").font(Typo.micro).foregroundStyle(Palette.inkFaint)
                Kbd("tab")
                Text("to cycle").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                Spacer()
            }
            GlassSegmented(options: dispositionOptions, selection: $disposition)
            Text(disposition?.hint ?? "auto: resolved from the line you wrote above.")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
        }
    }

    /// "auto" IS one of the values, not the absence of one: nil is the default
    /// state, so the travelling pane parks on it and the control never reads as
    /// unanswered.
    private var dispositionOptions: [(value: Disposition?, label: String)] {
        var options: [(value: Disposition?, label: String)] = [(value: nil, label: "auto")]
        options.append(
            contentsOf: Disposition.allCases.map { (value: Optional($0), label: $0.label) })
        return options
    }

    /// WHO the rule covers and the way into overriding it, on one dim line
    /// under the field. The glob is visible because nothing about a rule should
    /// be a surprise, and demoted because it is not the question being asked.
    /// An override the caller already chose shows here too, so a collapsed
    /// card never hides what it is about to save.
    @ViewBuilder
    private var advancedSection: some View {
        HStack(spacing: Space.s6) {
            if !advanced {
                Text("applies to").foregroundStyle(Palette.inkFaintest)
                Text(pattern)
                    .font(Typo.mono(10))
                    .foregroundStyle(Palette.inkFaint)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let disposition {
                    Text("· \(disposition.label)").foregroundStyle(Palette.inkFaint)
                }
            }
            Spacer(minLength: Space.s8)
            Button(action: toggleAdvanced) {
                HStack(spacing: Space.s4) {
                    Text("advanced")
                    Image(systemName: "chevron.down")
                        .font(Typo.ui(8, weight: .semibold))
                        .rotationEffect(.degrees(advanced ? 180 : 0))
                }
                .foregroundStyle(Palette.inkFaint)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            #if os(macOS)
                .help("⌘D")
            #endif
        }
        .font(Typo.micro)
        .padding(.top, -Space.s4)

        if advanced {
            Field(label: "match pattern") {
                TextField("*@example.com", text: $pattern)
                    .textFieldStyle(.plain)
                    .font(Typo.mono(12))
                    .focused($focusedField, equals: .pattern)
                    .autocorrectionDisabled()
            }
            dispositionOverride
        }
    }

    /// One row: the keys on the left (Mac only — a phone has none of them),
    /// the buttons on the right. A reason save is shut takes the hints' place
    /// rather than stacking another line on the card.
    private var footer: some View {
        HStack(spacing: Space.s8) {
            if let blocked {
                Text(blocked)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.warn)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                #if os(macOS)
                    HStack(spacing: Space.s10) {
                        KeyHint("↵", offered == nil ? "save" : "use suggestion")
                        KeyHint("⌥↵", "new line")
                        // Tab takes a suggestion first; teach whichever it does.
                        if offered == nil, advanced { KeyHint("tab", "outcome") }
                    }
                #endif
            }
            Spacer(minLength: Space.s8)
            Button("cancel", action: dismiss).buttonStyle(.glass)
            Button(saving ? "saving…" : (mode == .edit ? "update rule" : "save rule")) {
                Task { await save() }
            }
            .buttonStyle(.glassProminent)
            .tint(Palette.accent)
            .disabled(saving || !canSave)
        }
        .padding(.top, Space.s4)
    }

    /// An empty want on auto is the resting state of a fresh card, not a
    /// mistake, so it shuts save without a warning under your cursor.
    private var canSave: Bool {
        blocked == nil && !(want.trimmed.isEmpty && disposition == nil)
    }

    /// Why save is shut, in the user's terms, or nil when it is open. Checked
    /// BEFORE the round trip so the filtered-needs-want rule the store enforces
    /// lands as a hint rather than as an error over a form you already left.
    ///
    /// The want text is what holds save shut now: on auto it is the only input
    /// the disposition can be resolved from, so an empty one asks the daemon to
    /// guess from nothing. An explicit allow/mute override says the outcome out
    /// loud, so it may save wantless — which is also what keeps a legacy
    /// block-created rule (empty want, explicit mute) editable.
    private var blocked: String? {
        if pattern.trimmed.isEmpty { return "needs a match pattern" }
        if want.trimmed.isEmpty {
            if disposition == .filtered {
                return "filter needs a line saying what you do or do not want"
            }
        }
        return nil
    }

    /// ⌘d and the disclosure row are one gesture. Collapsing never discards the
    /// glob, it only stops showing it, so a hand-written pattern survives a
    /// stray toggle.
    private func toggleAdvanced() {
        withAnimation(Motion.disclose) { advanced.toggle() }
        focusedField = advanced ? .pattern : .want
    }

    /// Every way out that is not a save: Escape, the scrim, cancel, and opening
    /// one of the in-effect rules instead. The card goes, what was typed on it
    /// stays for the next time the same card opens (#306).
    private func dismiss() {
        stashDraft()
        onClose()
    }

    /// Keeps the card's values under its draft key, or drops the key when the
    /// card is back where it started (typed, then erased, is not a draft).
    private func stashDraft() {
        guard let key = request.draftKey else { return }
        let unchanged =
            pattern == pristine.pattern && want == pristine.want
            && disposition == pristine.disposition
        store.ruleDrafts[key] =
            unchanged
            ? nil
            : RuleDraft(
                pattern: pattern, want: want, disposition: disposition, advanced: advanced,
                origin: pristine.fingerprint)
    }

    /// The scrim's click, ignored for a double-click's length after the card
    /// appears. The rules page used to open a rule on a double-click and now
    /// opens it on one, so a hand that still double-clicks puts its second
    /// click wherever the row was, which is usually the scrim, and would shut
    /// the card it just opened.
    private func dismissFromScrim() {
        #if os(macOS)
            guard Date().timeIntervalSince(openedAt) > NSEvent.doubleClickInterval else { return }
        #endif
        dismiss()
    }

    /// A rule in the in-effect strip, clicked: this card gives way to that
    /// rule's own editor (#308). Whatever was typed here is kept as this card's
    /// draft, so `t` on the sender again finds it.
    private func openExisting(_ rule: SenderRule) {
        stashDraft()
        store.openRuleEditor(
            RuleEditorRequest(
                rule: rule,
                onSaved: request.onSaved ?? { Task { await RulesReload.shared.reload() } }))
    }

    /// What already governs this sender, above the field that would add another,
    /// scoped by the same glob the triage pipeline matches with. Tune mode only:
    /// create has no address to scope by, edit already shows the rule it edits.
    /// One line per rule now that the card leads with the sender and the want
    /// text; the full want stays in the tooltip.
    @ViewBuilder
    private var alreadyInEffect: some View {
        if mode == .tune, let inEffect, !inEffect.isEmpty {
            VStack(alignment: .leading, spacing: Space.s6) {
                FieldLabel(inEffect.count == 1 ? "current rule" : "current rules")
                // Sized to its rows up to three; past that it scrolls so a
                // sender with a pile of rules cannot push the field off the card.
                // A ScrollView always takes every point it is offered, which is
                // why it only appears once it has something to scroll.
                if inEffect.count > 3 {
                    ScrollView { inEffectRows(inEffect) }.frame(height: Space.scaled(96))
                } else {
                    inEffectRows(inEffect)
                }
            }
        }
    }

    /// The rule's own sentence leads, because that is what the owner wrote and
    /// will recognise. The glob only shows when it is not the one this card
    /// would write, since otherwise it repeats the sender's address.
    private func inEffectRows(_ rules: [SenderRule]) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(rules.enumerated()), id: \.element.id) { i, rule in
                if i > 0 { Hairline() }
                ListRow(
                    selected: false, cornerRadius: 8, hPadding: 10, vPadding: 7,
                    action: { openExisting(rule) }
                ) { _, _ in
                    HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                        Chip(text: rule.disposition.label, tone: rule.disposition.tone, filled: true)
                        VStack(alignment: .leading, spacing: Space.s2) {
                            Text(rule.want_text.isEmpty ? "no description" : rule.want_text)
                                .font(Typo.rowSub)
                                .foregroundStyle(
                                    rule.want_text.isEmpty ? Palette.inkFaintest : Palette.ink
                                )
                                .lineLimit(2)
                                .help(rule.want_text)
                            if rule.match_pattern != pattern {
                                Text(rule.match_pattern)
                                    .font(Typo.mono(10))
                                    .foregroundStyle(Palette.inkFaint)
                                    .lineLimit(1)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                }
            }
        }
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(Palette.canvas.opacity(0.3))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 0.75)
        )
    }

    /// Best-effort: a failed lookup leaves the strip absent rather than claiming
    /// the sender is unruled, which is the one wrong answer.
    private func loadInEffect() async {
        guard mode == .tune, let sender = request.sender else { return }
        guard let all = try? await APIClient.shared.listRules() else { return }
        let address = SenderID.address(sender)
        inEffect = all.filter {
            Reading.ruleMatches(pattern: $0.match_pattern, address: address)
        }
    }

    @ViewBuilder
    private var subtitle: some View {
        switch mode {
        case .tune:
            // Unreachable: tune mode has a sender, and the header leads with it.
            EmptyView()
        case .edit:
            Text("editing \(request.rule?.match_pattern ?? "")")
                .font(Typo.micro).foregroundStyle(Palette.inkFaintest)
        case .create:
            Text("define a sender rule from scratch")
                .font(Typo.micro).foregroundStyle(Palette.inkFaintest)
        }
    }

    /// Walks the override ring, auto included, so handing the decision back is
    /// as cheap as taking it — shift+tab from auto lands straight on filter.
    private func cycle(_ direction: Int) {
        var ring: [Disposition?] = [nil]
        ring.append(contentsOf: Disposition.allCases.map(Optional.init))
        let i = ring.firstIndex(of: disposition) ?? 0
        disposition = ring[(i + direction + ring.count) % ring.count]
    }

    /// Create POSTs (the store upserts on match_pattern, so re-tuning a sender
    /// edits its rule instead of stacking another); edit PUTs the id, so a
    /// pattern change moves the row it is looking at rather than orphaning it.
    /// The want text goes out exactly as typed, trimmed at the ends and nothing
    /// else.
    ///
    /// `disposition` rides only when the user overrode it; otherwise the field
    /// is absent and the RESPONSE tells us what the rule became. That answer is
    /// what the toast and the analytics report, because the client no longer
    /// knows it up front.
    private func save() async {
        guard !saving, canSave else { return }
        let body = CreateRuleBody(
            match_pattern: pattern.trimmed, want: want.trimmed, disposition: disposition)
        // Practice uses this same save path with the local fixture transport;
        // analytics tags the resulting event as a practice action.
        saving = true
        error = nil
        do {
            let saved: CreatedRule
            if let existing = request.rule {
                saved = try await APIClient.shared.updateRule(existing.id, body)
            } else {
                saved = try await APIClient.shared.createRule(body)
            }
            // A daemon too old to answer with the resolved value leaves us with
            // whatever we sent — nil there means we genuinely do not know, and
            // the toast says less rather than guessing.
            let resolved = saved.disposition ?? disposition
            var properties: [String: Any] = [
                "has_want": !want.trimmed.isEmpty,
                "edit": request.rule != nil,
                "suggested": suggestionShown,
                "from_suggestion": suggestionShown && want.trimmed == suggestion,
            ]
            if let resolved { properties["disposition"] = resolved.rawValue }
            Analytics.capture("rule_created", properties)
            let outcome = resolved.map { " → \($0.label)" } ?? ""
            store.pushToast(
                "\(request.rule != nil ? "rule updated" : "rule saved") · \(pattern.trimmed)\(outcome)",
                .success)
            request.onResult?(saved, body)
            request.onSaved?()
            if let key = request.draftKey { store.ruleDrafts[key] = nil }
            onClose()
        } catch let apiError as APIError where apiError.kind == .forbidden {
            error = "no write credential · run squelchd auth --write"
            saving = false
        } catch {
            self.error = errText(error, "save failed")
            saving = false
        }
    }
}
