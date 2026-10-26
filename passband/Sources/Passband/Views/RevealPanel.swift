// Sealed reveal: fetches one body on mount (audited and no-store server-side),
// holds it in this view's state only, drops it on dismiss — never to disk, never
// logged, never lifted into the store. HTML renders through the same sandboxed
// EmailWebView as normal mail, with no cacheKey: nothing about sealed mail is
// remembered, not even its measured height. No attachments either, and that is
// the answer rather than an omission — a reveal fetches one body and nothing
// else, so its `cid:` images resolve to nothing and drop. See docs/SECURITY.md.

import SwiftUI

struct RevealPanel: View {
    let meta: SealedMeta
    let onClose: () -> Void

    @State private var revealState: Loadable<RevealedSealed> = .loading

    var body: some View {
        OverlayScrim(onDismiss: onClose) {
            VStack(alignment: .leading, spacing: 0) {
                banner

                VStack(alignment: .leading, spacing: Space.s4) {
                    Text(subject)
                        .font(Typo.ui(15, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                    HStack(spacing: Space.scaled(5)) {
                        Text(fromLine)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                        if kind != nil {
                            Chip(
                                text: AuthCopy.label(kind), tone: Palette.lock,
                                symbol: AuthCopy.symbol(kind))
                        }
                    }
                }
                .padding(.horizontal, Space.s16)
                .padding(.vertical, Space.s12)

                if AuthPolicy.offersAnswer(question) {
                    RevealQuestion(meta: question)
                        .padding(.horizontal, Space.s16)
                        .padding(.bottom, Space.s12)
                }

                Divider().overlay(Palette.hairline)

                ScrollView {
                    Group {
                        if revealState.isLoading {
                            Text("revealing…")
                                .font(Typo.rowSub).foregroundStyle(Palette.inkFaintest)
                        } else if let error = revealState.error {
                            Text(error).font(Typo.rowSub).foregroundStyle(Palette.danger)
                        } else if let revealed = revealState.value {
                            if let html = revealed.html, !html.isEmpty {
                                EmailWebView(html: html)
                            } else {
                                Text(revealed.body)
                                    .font(Typo.row)
                                    .foregroundStyle(Palette.ink)
                                    .textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    }
                    .padding(Space.s16)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 480)

                HStack {
                    Text("cleared from memory when you close this.")
                    Spacer()
                    Text("audited server-side")
                }
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
                .padding(.horizontal, Space.s16)
                .padding(.vertical, Space.s10)
            }
            // A Mac sizes this against the window it floats in; a phone has one
            // measure and the panel takes it.
            #if os(macOS)
                .frame(width: 720)
            #else
                .frame(maxWidth: .infinity)
                .padding(.horizontal, Space.s12)
            #endif
            .passbandGlass(.pane, cornerRadius: 20, tint: Palette.lockSoft.opacity(0.85))
            .shadow(color: .black.opacity(0.32), radius: 46, y: 20)
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Escape", "close reveal", allowInInput: true) { onClose() }
        ])
        .task { await load() }
        // Clear the sensitive body when this view goes away — `.idle` drops the
        // decoded value, which is the only copy of it.
        .onDisappear { revealState = .idle }
    }

    private var banner: some View {
        HStack(spacing: Space.scaled(7)) {
            Image(systemName: "lock.fill").font(Typo.ui(11))
            Text("sensitive · one-time reveal · not stored")
                .font(Typo.micro)
            Spacer()
            // The keycap is the Mac's exit. A phone taps the scrim, and a hint
            // naming a key it has no way to press is worse than no hint.
            #if os(macOS)
                HStack(spacing: Space.s4) {
                    Kbd("esc")
                    Text("close").font(Typo.micro)
                }
            #else
                Button("close", action: onClose)
                    .font(Typo.micro)
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.lock)
            #endif
        }
        .foregroundStyle(Palette.lock)
        .padding(.horizontal, Space.s16)
        .padding(.vertical, Space.s8)
        .frame(maxWidth: .infinity)
        .background(Palette.lockSoft)
    }

    /// What the header names. A notification tap opens this with only an id
    /// and an address, so once the reveal lands its own fields fill the gaps.
    private var subject: String {
        meta.subject.isEmpty ? (revealState.value?.subject ?? "") : meta.subject
    }
    private var kind: SealedKind? { meta.kind ?? revealState.value?.kind }

    /// The meta the question is judged on: the listed sealed row when there is
    /// one (it carries the verdict and `critical`), else what was handed in,
    /// with the kind the reveal reported.
    private var question: SealedMeta {
        if let listed = AppStore.shared.sealedMeta(message: meta.id) { return listed }
        var m = meta
        m.kind = kind
        return m
    }

    private var fromLine: String {
        let revealed = revealState.value
        let name = revealed?.from_name.map { "\($0) · " } ?? ""
        let sender = meta.sender.isEmpty ? (revealed?.sender ?? "") : meta.sender
        let at = meta.received_at.isEmpty ? (revealed?.received_at ?? "") : meta.received_at
        return "\(name)\(sender) · \(Fmt.dateTime(at))"
    }

    private func load() async {
        await $revealState.load("reveal failed") {
            try await APIClient.shared.revealSealed(meta.id)
        }
        // How often humans actually need behind the seal — the count and
        // nothing else; the body itself never leaves the view above.
        if revealState.value != nil { Analytics.capture("sealed_revealed") }
    }
}

/// The question this auth mail asks, answered right where it was read: the
/// same buttons as the Auth page's card, and once answered, the answer with an
/// Undo. Writes through `AuthDecisions`, like the card. Only metadata here,
/// never the body.
struct RevealQuestion: View {
    let meta: SealedMeta
    private var decisions = AuthDecisions.shared

    init(meta: SealedMeta) { self.meta = meta }

    private var tone: Color { meta.isCritical ? Palette.danger : Palette.lock }

    var body: some View {
        let verdict = decisions.decision(meta)
        let answers = AuthPolicy.answers(meta)
        HStack(spacing: Space.s8) {
            Image(systemName: verdict == nil ? "questionmark.circle" : "checkmark.circle")
                .font(Typo.ui(12))
                .foregroundStyle(verdict == nil ? tone : Palette.inkFaint)
            Text(verdict.map { AuthPolicy.answeredLine(meta, $0) } ?? AuthPolicy.questionPrompt(meta))
                .font(Typo.ui(12, weight: .medium))
                .foregroundStyle(verdict == nil ? Palette.ink : Palette.inkDim)
            Spacer(minLength: Space.s8)
            if verdict == nil {
                Button(answers.mine) { decisions.set(meta, .mine) }
                    .buttonStyle(.glassProminent)
                    .tint(Palette.accent)
                Button(answers.notMine) { decisions.set(meta, .notMine) }
                    .buttonStyle(.glass)
                    .foregroundStyle(Palette.danger)
            } else {
                Button("Undo") { decisions.set(meta, nil) }
                    .buttonStyle(.glass)
                    .foregroundStyle(Palette.inkDim)
            }
        }
        .font(Typo.micro)
        .controlSize(.small)
        .padding(.horizontal, Space.s12)
        .padding(.vertical, Space.s8)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(verdict == nil ? tone.opacity(0.08) : Palette.hairline.opacity(0.5))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(verdict == nil ? tone.opacity(0.25) : .clear, lineWidth: 1)
        )
    }
}
