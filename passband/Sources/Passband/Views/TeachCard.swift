// `t`, teach Passband: one field, one sentence. The daemon reads it and
// decides whether it is about a sender (an ordinary sender rule) or about all
// mail (a general rule triage reads on every email). Opened from an email,
// the sender sits on the card as context and the email's id goes with the
// sentence; the model still decides, so not every teach from an email ends up
// tied to its sender.
//
// The same card, with `request.general` set, edits a general rule's text in
// place (Rules page, e / Enter / tap). That save is verbatim, no model.
//
// Keys match RuleEditor: Esc cancels and keeps a draft, Enter and ⌘Enter
// save, ⌥Enter is a new line, Tab takes the suggestion while it is on offer.

import SwiftUI

struct TeachCard: View {
    let request: RuleEditorRequest
    let onClose: () -> Void

    @Environment(AppStore.self) private var store

    @State private var text: String
    @State private var saving = false
    @State private var error: String?
    /// The sender suggestion, shown as the placeholder and taken with tab.
    @State private var suggestion: String?
    @State private var suggestionShown = false
    @FocusState private var focused: Bool
    @State private var openedAt = Date()
    /// What the card opened on; leaving it there is not a draft.
    private let pristine: String

    init(request: RuleEditorRequest, draft: RuleDraft? = nil, onClose: @escaping () -> Void) {
        self.request = request
        self.onClose = onClose
        let start = request.general?.text ?? ""
        pristine = start
        let shown = draft.flatMap { $0.origin == start ? $0.want : nil } ?? start
        _text = State(initialValue: shown)
    }

    private var editing: Bool { request.general != nil }

    var body: some View {
        OverlayScrim(onDismiss: dismissFromScrim) {
            ModalCard(width: 470) {
                header
                field
                if let error {
                    Text(error)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.danger)
                        .fixedSize(horizontal: false, vertical: true)
                }
                footer
            }
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Escape", "cancel", allowInInput: true) { dismiss() },
            KeyBinding(declining: "Tab", "use suggestion", allowInInput: true) {
                acceptSuggestion()
            },
            KeyBinding("Enter", "save", allowInInput: true) { submit() },
            KeyBinding("Enter", "save", meta: true, allowInInput: true) { submit() },
        ])
        .onAppear { focused = true }
        .task { await loadSuggestion() }
    }

    // MARK: - parts

    @ViewBuilder
    private var header: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            Text(editing ? "edit rule" : "teach Passband")
                .font(Typo.modalTitle)
                .foregroundStyle(Palette.ink)
            // Quiet context: the email this was opened from. Email-derived,
            // so plain Text only.
            if !editing, let sender = request.sender {
                HStack(spacing: Space.s6) {
                    Avatar(sender: sender, size: 16)
                    Text(SenderCache.resolved(sender).displayName)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                    Text("· about mail like this one")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                }
            }
        }
    }

    private var field: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            Text(editing ? "what should Passband do?" : "what should Passband know?")
                .font(Typo.row)
                .foregroundStyle(Palette.inkDim)
            HStack(alignment: .top, spacing: Space.s8) {
                TextField(
                    "", text: $text,
                    prompt: Text(offered ?? Teach.example),
                    axis: .vertical
                )
                .textFieldStyle(.plain)
                .lineLimit(3...8)
                .focused($focused)
                .disabled(saving)
                if offered != nil {
                    Button { _ = acceptSuggestion() } label: {
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
            Text(note)
                .font(Typo.micro)
                .foregroundStyle(tooLong ? Palette.warn : Palette.inkFaintest)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// One line under the field: what happens to the sentence, or how much
    /// room is left once it gets long.
    private var note: String {
        let count = text.trimmed.count
        if count > Teach.maxLength { return "\(count - Teach.maxLength) characters over the limit" }
        if count > Teach.lengthWarning { return "\(Teach.maxLength - count) characters left" }
        if editing { return "Passband reads this on every email." }
        if saving { return "Passband is working out what this applies to." }
        return request.sender == nil
            ? "Passband works out whether this is about one sender or all your mail."
            : "Passband works out whether this is about this sender or all your mail."
    }

    private var tooLong: Bool { text.trimmed.count > Teach.maxLength }

    private var footer: some View {
        ModalButtons {
            #if os(macOS)
                HStack(spacing: Space.s10) {
                    KeyHint("↵", offered == nil ? "save" : "use suggestion")
                    KeyHint("⌥↵", "new line")
                }
            #endif
        } buttons: {
            Button("cancel", action: dismiss).buttonStyle(.glass)
            Button(saveLabel) { Task { await save() } }
                .buttonStyle(.glassProminent)
                .tint(Palette.accent)
                .disabled(saving || !Teach.canSave(text))
        }
        .padding(.top, Space.s4)
    }

    private var saveLabel: String {
        if saving { return editing ? "saving…" : "learning…" }
        return editing ? "update rule" : "teach"
    }

    // MARK: - suggestion

    private var offered: String? { text.isEmpty ? suggestion : nil }

    /// Best-effort, like RuleEditor's: an old daemon, no model or a slow
    /// answer leave the example in place.
    private func loadSuggestion() async {
        guard !editing, let sender = request.sender, text.isEmpty else { return }
        guard let answer = try? await APIClient.shared.suggestRule(
            sender: SenderID.address(sender))
        else { return }
        withAnimation(Motion.disclose) { suggestion = answer }
        if text.isEmpty { suggestionShown = true }
    }

    @discardableResult
    private func acceptSuggestion() -> Bool {
        guard let offered else { return false }
        text = offered
        focused = true
        return true
    }

    // MARK: - closing

    /// Enter on an empty field with a suggestion showing takes it; the next
    /// Enter saves, after it has been read.
    private func submit() {
        if acceptSuggestion() { return }
        Task { await save() }
    }

    private func dismiss() {
        stashDraft()
        onClose()
    }

    /// Ignored for a double-click's length after opening, as RuleEditor's is:
    /// the Rules page opens a row on one click.
    private func dismissFromScrim() {
        #if os(macOS)
            guard Date().timeIntervalSince(openedAt) > NSEvent.doubleClickInterval else { return }
        #endif
        dismiss()
    }

    private func stashDraft() {
        guard let key = request.draftKey else { return }
        store.ruleDrafts[key] =
            text == pristine
            ? nil
            : RuleDraft(pattern: "", want: text, disposition: nil, advanced: false, origin: pristine)
    }

    /// Close only if this card is still the one up: the save can outlive an
    /// Esc, and by then another card may be open.
    private func closeIfStillOpen() {
        guard store.ruleEditor?.id == request.id else { return }
        onClose()
    }

    // MARK: - save

    private func save() async {
        let sentence = text.trimmed
        guard !saving, Teach.canSave(sentence) else { return }
        saving = true
        error = nil
        do {
            if let general = request.general {
                try await APIClient.shared.updateGeneralRule(general.id, text: sentence)
                Analytics.capture(
                    "rule_created", ["has_want": true, "edit": true, "general": true])
                store.pushToast("rule updated", .success)
            } else {
                try await teach(sentence)
            }
            request.onSaved?()
            if let key = request.draftKey { store.ruleDrafts[key] = nil }
            closeIfStillOpen()
        } catch let apiError as APIError where apiError.kind == .forbidden {
            error = "no write credential · run squelchd auth"
            saving = false
        } catch {
            self.error = errText(error, "save failed")
            saving = false
        }
    }

    private func teach(_ sentence: String) async throws {
        let result = try await Actions.sendTeach(
            text: sentence, sender: request.sender, sourceMessageId: request.sourceMessageId)
        var properties: [String: Any] = [
            "has_want": true,
            "edit": false,
            "general": !result.isSender,
            "suggested": suggestionShown,
            "from_suggestion": suggestionShown && sentence == suggestion,
        ]
        if let disposition = result.disposition {
            properties["disposition"] = disposition.rawValue
        }
        Analytics.capture("rule_created", properties)
        Discovery.note(.ruleSaved)
        store.pushUndo(
            kind: .teach, messageId: result.id, label: Teach.savedLabel(result),
            undoneText: Teach.undoneText
        ) {
            try await Actions.undoTeach(result)
        }
    }
}
