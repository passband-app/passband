// ⌘K "ask your inbox": a top-anchored agent. Type a question, watch it work,
// answer its confirmation cards, keep talking. The loop runs locally on the
// user's own key (BYOK); the squelch server never sees the question or the
// answer.
//
// THE MATERIAL GROWING DOWNWARD IS THE TRAY. Bar and transcript share ONE
// glassEffectID inside a GlassEffectContainer, so asking makes the bar grow a
// chat log out of its own bottom edge instead of a second panel appearing
// beneath a first. That is why everything below lives inside the same VStack.
// Once a conversation exists the input row sits at the BOTTOM, under the log —
// where a chat's composer belongs — and the log grows between it and the
// header.
//
// The rows are TINTS, never glass: a glass row means a container that
// re-coordinates every descendant on any change, and this list changes on every
// streamed token (see Glass.swift's selectionFill).

import SwiftUI

struct AskBar: View {
    /// THE WINDOW RATHER THAN A TRAY: a conversation that came out of search
    /// (see `AppStore.askBarFullscreen`). Same view, same session, same keys;
    /// only the room changes, so the bar can never be one thing in the modal
    /// and another full screen.
    var fullscreen = false
    let onClose: () -> Void

    @Environment(AppStore.self) private var store
    @State private var question = ""
    @Namespace private var askGlass
    @FocusState private var focused: Bool
    /// Rendered markdown, kept across body evaluations — see MarkdownCache.
    @State private var rendered = MarkdownCache()

    /// The conversation outlives this view — see AppStore.assistant. While the
    /// bar is watching a deeper search finish, it is that lane instead.
    private var session: AssistantSession { store.askBarSession }

    /// The email behind the bar, as the agent should hear about it — nil when
    /// the bar was opened from a list. The summary lands a beat after the
    /// thread id does (the viewer writes it when the thread arrives), and it is
    /// read through `currentThreadSummary` so it is only ever this thread's: a
    /// question asked in that gap still pins the thread, which is the part that
    /// matters, and get_thread recovers the rest.
    private var openEmail: OpenEmailContext? {
        guard let threadId = store.threadId else { return nil }
        return OpenEmailContext(threadId: threadId, summary: store.currentThreadSummary)
    }

    /// What the "in:" chip names, which is not the same email at every moment.
    ///
    /// While a run is open it names the email THAT RUN was asked under: the bar
    /// can be reopened over another thread mid-answer, and a chip that followed
    /// the reader would then claim the working question is about an email it has
    /// never heard of. Idle, it names what the NEXT question would carry, which
    /// is the reader's own thread. nil either way means no chip: a run asked
    /// with nothing open has no email in it, whatever is behind the bar now.
    private var chipSubject: String? {
        session.running
            ? session.activeAskEmail?.summary?.subject
            : store.currentThreadSummary?.subject
    }

    /// Scroll target pinned to the bottom of the log.
    private static let bottomAnchor = "askbar.bottom"

    var body: some View {
        surface
        .keyContext(.modal)
        // Enter submits from the field itself, not the keymap, so it can never
        // collide with list keys.
        .keyBindings(.modal, [
            KeyBinding("Escape", "close", allowInInput: true) { onClose() }
        ])
        .onAppear {
            focused = true
            store.finishLaneHandoff()
        }
        // A watched deeper search that finishes becomes the chat, so the
        // follow-up field under it starts working.
        .onChange(of: store.searchLane.running) { store.finishLaneHandoff() }
        // STRUCTURE ONLY. Animating anything that changes per token would smear
        // the streaming text; the count changes once per row.
        .animation(.smooth(duration: 0.25), value: session.transcript.count)
    }

    @ViewBuilder
    private var surface: some View {
        if fullscreen {
            // THE RAIL STAYS, as it does beside the reader, and a click on it
            // leaves (`setView` closes this). Only its title strip takes this
            // surface's ground, so the top bar reads as one bar.
            HStack(spacing: 0) {
                VStack(spacing: 0) {
                    if !WindowState.shared.isFullscreen {
                        ReaderBackdrop()
                            .overlay(Palette.accentSoft.opacity(0.35))
                            .frame(height: TopBar.height)
                    }
                    Color.clear
                }
                .frame(width: SidebarRail.railWidth)
                .allowsHitTesting(false)
                chatPage
            }
            .ignoresSafeArea()
            .transition(.opacity)
        } else {
            // NOT ON `PaletteShell`, though it shares its edge, glass and
            // shadow: this is a chat that grows a transcript out of its own
            // bottom edge, with the field moving under it, not a filtered list
            // with a footer. Higher than the palettes' inset because the tray
            // grows to the window's height.
            OverlayScrim(alignment: .top, topInset: 96, onDismiss: onClose) {
                GlassEffectContainer(spacing: 10) {
                    VStack(alignment: .leading, spacing: 0) {
                        header
                        if !session.transcript.isEmpty {
                            Divider().overlay(Palette.hairline)
                            tray
                            Divider().overlay(Palette.hairline)
                        }
                        inputRow
                    }
                    .frame(width: Typo.measure(620))
                    .passbandGlass(
                        .pane, cornerRadius: Radius.modal, tint: Palette.glassTintStrong,
                        id: "askbar", in: askGlass)
                    .overlayShadow()
                }
                .padding(.bottom, Space.s32)
            }
        }
    }

    /// The full-screen page beside the rail. OPAQUE, the reader's own ground,
    /// and it takes every click: the page underneath is still mounted (Esc
    /// comes back to it) and must not be clickable through a surface that
    /// looks like the window.
    private var chatPage: some View {
        ZStack(alignment: .top) {
            // THE BAND'S OWN BLUE, washed over the opaque page: the band in
            // search and this page are the same conversation, and going from
            // one to the other should read as the box growing, not as a jump
            // to a different surface. Same wash as DeeperSearchBand's fill.
            ReaderBackdrop()
                .overlay(Palette.accentSoft.opacity(0.35))
                .ignoresSafeArea()
                .contentShape(Rectangle())
            // The rules run the page's full width; the words between them
            // keep a reading measure, since a line of chat as wide as a 27"
            // display is unreadable.
            VStack(alignment: .leading, spacing: 0) {
                measured(header)
                Divider().overlay(Palette.hairline)
                if session.transcript.isEmpty {
                    Spacer(minLength: 0)
                } else {
                    measured(tray)
                }
                Divider().overlay(Palette.hairline)
                measured(inputRow)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
    }

    /// One band of the full-screen page at the reading measure, centered.
    private func measured(_ content: some View) -> some View {
        content
            .frame(maxWidth: Typo.measure(760))
            .padding(.horizontal, Space.s24)
            .frame(maxWidth: .infinity)
    }

    // MARK: - chrome

    private var header: some View {
        HStack(spacing: Space.scaled(7)) {
            // The wordmark's face ("passband" in the mail bar). Named for what
            // it holds: a search's conversation is deeper search, as the band
            // it grew out of says; plain ⌘K is talking to the agent.
            Text(store.askBarShowsSearch ? "deeper search" : "ask your inbox")
                .font(Typo.serif(19, weight: .medium))
                .foregroundStyle(Palette.ink)
            // WHAT THE AGENT CAN SEE, said before the question is asked: the
            // bar is opened over the reader often enough that "this email"
            // should look like it means something. Shown only once the thread
            // has landed — a subject is the only thing here worth naming — and
            // it follows the RUN once one is open; see `chipSubject`.
            if let subject = chipSubject {
                Text("in: \(subject)")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: Typo.measure(230), alignment: .leading)
                    .padding(.horizontal, Space.scaled(7)).padding(.vertical, Space.s2)
                    .background(Capsule().fill(Palette.hairline))
                    .help(
                        session.running
                            ? "The assistant is working on a question about this email"
                            : "This email is part of the question")
            }
            Spacer()
            if !session.transcript.isEmpty {
                Button {
                    store.newAskChat()
                    focused = true
                } label: {
                    HStack(spacing: Space.s4) {
                        Text("new chat").font(Typo.micro)
                        Kbd("⌘⇧K")
                    }
                }
                .buttonStyle(.textAction)
                .help("Start over with an empty conversation")
            }
            HStack(spacing: Space.s4) {
                // Full screen came from search, and Esc is the way back to it:
                // say so, since nothing else on screen shows the search exists.
                if fullscreen && store.sideView == .search {
                    Text("back to search")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                }
                Kbd("esc")
            }
        }
        .padding(.horizontal, PaletteMetrics.edge)
        // FULL SCREEN, THE HEADER IS THE TOP BAR: it sits in the title-bar row
        // beside the traffic lights, as every page's own bar does, rather than
        // under a strip of empty window.
        .padding(.top, fullscreen ? 0 : Space.s14)
        .padding(.bottom, fullscreen ? 0 : Space.s8)
        .frame(minHeight: fullscreen ? TopBar.height : nil)
    }

    private var canSubmit: Bool { !question.trimmed.isEmpty && !session.running }

    private var inputRow: some View {
        HStack(spacing: Space.s8) {
            // Never disabled: a long answer is exactly when somebody types their
            // next question. Only SUBMIT waits for the turn to finish.
            TextField(
                session.transcript.isEmpty
                    ? "e.g. what did I say I'd send Dan?" : "ask a follow-up…",
                text: $question
            )
            .textFieldStyle(.plain)
            .font(Typo.ui(16))
            .foregroundStyle(Palette.ink)
            .focused($focused)
            .onSubmit(submit)
            Image(systemName: "return")
                .font(Typo.ui(11, weight: .semibold))
                .foregroundStyle(canSubmit ? Palette.accent : Palette.inkFaintest)
        }
        .padding(.horizontal, PaletteMetrics.edge)
        // With a log above, the divider replaces the header as the top
        // neighbor and the row needs its own breathing room.
        .padding(.top, session.transcript.isEmpty ? 0 : Space.s12)
        .padding(.bottom, Space.s14)
    }

    private func submit() {
        // A watched lane that ended a beat ago may not have been handed over
        // yet; do it now so the question lands in the chat, not the search.
        store.finishLaneHandoff()
        guard canSubmit else { return }
        let text = question.trimmed
        question = ""
        // The email goes WITH the question — read here, at ask time, so it can
        // never name a thread the user has since walked away from.
        session.send(text, openEmail: openEmail)
    }

    // MARK: - the tray

    private var tray: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Space.s10) {
                    ForEach(session.transcript.chatRows) { item in
                        row(item).frame(maxWidth: .infinity, alignment: .leading)
                    }
                    if session.awaitingOutput {
                        workingRow
                    } else if parkedOnConfirmation {
                        parkedRow
                    }
                    Color.clear.frame(height: 1).id(Self.bottomAnchor)
                }
                .padding(.horizontal, PaletteMetrics.edge)
                .padding(.vertical, Space.s12)
            }
            // THE WINDOW'S HEIGHT, in both forms: a conversation is read top to
            // bottom, and a 440pt tray made a long answer a keyhole. The modal
            // is held off the window's bottom edge by the scrim's own margin.
            .frame(maxHeight: .infinity)
            // MOUNT AT THE BOTTOM. Both follows below are onChange, and neither
            // fires on appear — so reopening ⌘K would otherwise restore the
            // conversation scrolled to its beginning, with the card that is
            // holding submit hostage off the bottom of the pane.
            .defaultScrollAnchor(.bottom)
            // The anchor alone is applied BEFORE the lazy rows finish sizing,
            // so a reopened long conversation still lands short of the end.
            // Re-pin after layout settles — same deferred-scroll trick the
            // search panel's onAppear restore uses.
            .onAppear {
                Task { @MainActor in
                    proxy.scrollTo(Self.bottomAnchor, anchor: .bottom)
                }
            }
            // A new row is a structural change worth following visibly.
            .onChange(of: session.transcript.count) { _, _ in
                withAnimation(Motion.scrollFollow) {
                    proxy.scrollTo(Self.bottomAnchor, anchor: .bottom)
                }
            }
            // Tokens are not. Following them UNANIMATED is what keeps the text
            // legible while it lands — an animated follow at token rate is the
            // smear ComposePane's `.animation` comment warns about.
            .onChange(of: session.streamTick) { _, _ in
                proxy.scrollTo(Self.bottomAnchor, anchor: .bottom)
            }
            // EVERY LINK IN HERE GOES THROUGH THE HOUSE DOOR. Assistant answers
            // render as markdown and the model's input is attacker-authored mail
            // bodies, so `[Verify your account](…)` is a link somebody else
            // wrote. Without this the click reaches NSWorkspace with no scheme
            // check at all — file:, and any registered custom scheme, included.
            // Opener is the same http(s)-only guard the reader holds
            // (docs/SECURITY.md §2 layer 4).
            .environment(
                \.openURL,
                OpenURLAction { url in
                    Opener.open(url.absoluteString)
                    return .handled
                })
        }
    }

    /// True while the loop is suspended on a card nobody has answered. `running`
    /// stays true through that (the loop really is open), so submit is disabled
    /// with nothing on screen to say why.
    private var parkedOnConfirmation: Bool {
        guard session.running, let last = session.transcript.last else { return false }
        return last.action?.state == .pending
    }

    @ViewBuilder
    private func row(_ item: ChatItem) -> some View {
        switch item.kind {
        case .user:
            userRow(item.text)
        case .assistant:
            Text(assistantText(item))
                .font(Typo.ui(14))
                .foregroundStyle(Palette.ink)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        case .tool:
            if let tool = item.tool { ToolChipRow(tool: tool) }
        case .action:
            if let action = item.action { actionCard(action) }
        case .error:
            Text(item.text)
                .font(Typo.rowSub)
                .foregroundStyle(Palette.danger)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        case .citations:
            citations(item.citations)
        case .emails:
            emailCards(item.emails)
        case .page:
            if let page = item.page.flatMap(PageLink.init(rawValue:)) {
                PageLinkCard(page: page) { page in
                    store.open(page)
                    onClose()
                }
            }
        }
    }

    private func userRow(_ text: String) -> some View {
        HStack(spacing: 0) {
            Spacer(minLength: Space.scaled(56))
            Text(text)
                .font(Typo.row)
                .foregroundStyle(Palette.ink)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, Space.s10)
                .padding(.vertical, Space.s6)
                .background(
                    RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                        .fill(Palette.accentSoft)
                )
        }
    }

    private var workingRow: some View {
        HStack(spacing: Space.s8) {
            ProgressView().controlSize(.mini)
            Text("working…")
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkFaint)
        }
    }

    private var parkedRow: some View {
        HStack(spacing: Space.s8) {
            Image(systemName: "hand.raised")
                .font(Typo.ui(10, weight: .semibold))
                .foregroundStyle(Palette.inkFaint)
            Text("waiting for your confirmation above")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
        }
    }

    /// The row STILL GROWING renders as plain text; every closed row is parsed
    /// once and memoized. `body` re-runs on every streamed token, so parsing the
    /// live row per render is a full markdown parse of an ever-longer string
    /// dozens of times a second, on the main actor — the formatting arrives the
    /// instant the turn closes, which is soon enough.
    private func assistantText(_ item: ChatItem) -> AttributedString {
        let streaming = session.running && session.transcript.last?.id == item.id
        return streaming
            ? AttributedString(item.text) : rendered.markdown(item.id, source: item.text)
    }

    // MARK: - confirm card

    private func actionCard(_ action: PendingAction) -> some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            HStack(spacing: Space.s6) {
                Image(systemName: action.tool.symbol)
                    .font(Typo.ui(11, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                Text(Self.cardTitle(action))
                    .font(Typo.ui(12, weight: .semibold))
                    .foregroundStyle(Palette.ink)
            }
            // WHO AND WHAT, FROM THE DAEMON. A send names its target inside the
            // preview instead (the recipient row), so these two are for the
            // cards that act on a message somebody else sent.
            if action.tool != .sendEmail {
                if let sender = action.verifiedSender { ComposeSummaryRow("from", sender) }
                if let subject = action.verifiedSubject { ComposeSummaryRow("subject", subject) }
            }
            if let detail = action.detail {
                Text(detail)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if action.tool == .unsubscribeSender {
                Text(Self.unsubscribeNote)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if action.tool == .sendEmail { sendPreview(action) }

            switch action.state {
            case .pending:
                cardButtons(action)
            case .running:
                HStack(spacing: Space.s6) {
                    ProgressView().controlSize(.mini)
                    Text("working…").font(Typo.micro).foregroundStyle(Palette.inkFaint)
                }
            default:
                Text(Self.outcomeLine(action.state))
                    .font(Typo.micro)
                    .foregroundStyle(
                        Self.isFailure(action.state) ? Palette.danger : Palette.inkFaintest)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(Space.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(Palette.accentSoft.opacity(0.6))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75)
        )
    }

    private func cardButtons(_ action: PendingAction) -> some View {
        HStack(spacing: Space.s8) {
            Button("Confirm") { session.resolve(action.id, .approved) }
                .buttonStyle(.glassProminent)
                .tint(Palette.accent)
            Button("Decline") { session.resolve(action.id, .declined) }
                .buttonStyle(.glass)
            if action.tool == .sendEmail {
                Button("Edit in composer") { editInComposer(action) }
                    .buttonStyle(.glass)
                    // Handing off REPLACES whatever the composer holds, so an
                    // open composition closes this door rather than losing what
                    // is in it. Same rule `c` and ⌘N already keep.
                    .disabled(store.compose != nil)
                    .help(
                        store.compose != nil
                            ? "A composition is already open. Finish or close it first."
                            : "Move this draft into the composer to finish it yourself")
            }
            Spacer(minLength: 0)
        }
        .font(Typo.chip)
    }

    private func sendPreview(_ action: PendingAction) -> some View {
        VStack(alignment: .leading, spacing: Space.s6) {
            // ON A REPLY THE RECIPIENT IS THE PARENT'S SENDER, read back from
            // the daemon before this card was parked. The placeholder is the
            // last resort, not the normal case: the whole point of the tap is
            // that the person can see who the mail goes to.
            ComposeSummaryRow("to", action.to ?? action.verifiedSender ?? Self.derivedRecipient)
            // APPROVING A SEND MEANS SEEING EVERYONE IT REACHES. A copy list the
            // card did not mention is a recipient the person authorized without
            // knowing, and for a blind one this card is the only screen that
            // will ever say so — the sent mail shows it to nobody.
            if let cc = action.cc, !cc.trimmed.isEmpty { ComposeSummaryRow("cc", cc) }
            if let bcc = action.bcc, !bcc.trimmed.isEmpty { ComposeSummaryRow("bcc", bcc) }
            ComposeSummaryRow(
                "subject",
                action.subject
                    ?? action.verifiedSubject.map { "Re: \($0)" }
                    ?? (action.replyToMessageId != nil ? ComposeCopy.derivedSubject : "(none)"))
            ScrollView {
                Text(Self.markdown(action.body ?? ""))
                    .font(Typo.row)
                    .foregroundStyle(Palette.ink)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: Space.scaled(140))
            .padding(Space.s8)
            .background(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .fill(Palette.canvas.opacity(0.6))
            )
        }
    }

    /// Hand the model's draft to the real composer and tell the agent it is no
    /// longer holding the pen. Same entry point every other compose flow uses,
    /// so the draft autosave and the send ceremony come along unchanged.
    private func editInComposer(_ action: PendingAction) {
        // `openCompose` overwrites `store.compose` outright AND cancels the
        // armed autosave, so a live composition would be gone from the screen
        // and from the server both. The button is disabled for the same reason;
        // this is the guard that makes that true rather than merely displayed.
        guard store.compose == nil else { return }
        store.openCompose(
            ComposeState(
                replyToMessageId: action.replyToMessageId,
                to: action.to ?? "",
                cc: action.cc ?? "",
                bcc: action.bcc ?? "",
                subject: action.subject ?? "",
                body: action.body ?? ""))
        // A HAND-OFF IS COMPOSED CONTENT: the words are on screen because the
        // reader asked the agent for them and chose to edit them. `openCompose`
        // marks the slot opened (untouched), which would make an Esc before
        // the first keystroke drop the mail and nothing say so. Marking it
        // changed arms the autosave and makes the close flush keep it.
        //
        // BUT ONLY FOR PLAIN NEW MAIL. A reply saves under its parent's key,
        // one row per parent, and the pane composer never restores a reply
        // draft, so arming a reply hand-off would autosave one debounce later
        // and silently OVERWRITE a reply the person already had saved for that
        // parent (the reason `AgentTools.saveDraft` refuses an occupied slot).
        // Unarmed, the reply's first keystroke arms it, and by then the person
        // is editing on purpose.
        if action.replyToMessageId == nil {
            DraftSaver.shared.noteChange(.compose)
        }
        session.resolve(action.id, .editedInComposer)
        onClose()
    }

    // MARK: - shown emails

    /// The show_emails cards, drawn by the shared list (Views/AgentCards.swift
    /// — the search lane draws the same rows). Same click contract as a
    /// citation here: open the thread, close the bar.
    private func emailCards(_ cards: [EmailCard]) -> some View {
        EmailCardList(cards: cards) { threadId in
            store.openThread(threadId, from: .assistant)
            onClose()
        }
    }

    // MARK: - citations

    @ViewBuilder
    private func citations(_ cites: [ToolCitation]) -> some View {
        if !cites.isEmpty {
            VStack(alignment: .leading, spacing: Space.s4) {
                Text("sources")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .textCase(.uppercase)
                ForEach(cites) { citation in
                    Button {
                        store.openThread(citation.threadId, from: .assistant)
                        onClose()
                    } label: {
                        HStack(spacing: Space.scaled(7)) {
                            Avatar(sender: citation.sender, size: 16)
                            Text(citation.sender)
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkDim)
                                .lineLimit(1)
                            Text(citation.subject)
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaintest)
                                .lineLimit(1)
                            Spacer(minLength: Space.s4)
                            Image(systemName: "arrow.up.right")
                                .font(Typo.ui(9, weight: .semibold))
                                .foregroundStyle(Palette.accent)
                        }
                        .padding(.horizontal, Space.scaled(9))
                        .padding(.vertical, Space.scaled(5))
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(RecordRowStyle())
                    .help("Open this thread")
                }
            }
        }
    }

    // MARK: - copy

    /// NATIVE markdown parsing, deliberately NOT the in-house `MarkdownStyle`:
    /// that scanner is the composer's editor highlighter and KEEPS the `**`
    /// visible, which is right over a source buffer and wrong in a chat log.
    /// Inline-only preserving whitespace, so a half-streamed list still reads as
    /// the lines it was typed as. A parse failure falls back to the raw text —
    /// a stray bracket must never blank an answer.
    fileprivate static func markdown(_ text: String) -> AttributedString {
        (try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }

    /// Only reached when a send names neither a recipient nor a parent we could
    /// read back — the daemon still derives one, but this card could not say so.
    private static let derivedRecipient = "(derived from the message being answered)"

    /// The part of an unsubscribe that happens outside Passband, said before the
    /// tap rather than after it.
    private static let unsubscribeNote =
        "Opens this sender's unsubscribe page in your browser."

    private static func cardTitle(_ action: PendingAction) -> String {
        switch action.tool {
        case .archiveMessage: "Archive this email"
        case .labelMessage: "Change this email's labels"
        case .sendEmail: "Send this email"
        case .unsubscribeSender: "Unsubscribe from this sender"
        default: action.verb
        }
    }

    private static func outcomeLine(_ state: PendingAction.State) -> String {
        switch state {
        case .declined: "Declined"
        case .handedOff: "Moved to the composer"
        case .executed(let done): done
        case .failed(let text): text
        case .pending, .running: ""
        }
    }

    private static func isFailure(_ state: PendingAction.State) -> Bool {
        if case .failed = state { return true }
        return false
    }
}

/// Parsed markdown, memoized per transcript row.
///
/// A CLASS, held by `@State`, so `row(_:)` can fill it during body evaluation —
/// a value type there would be a mutation mid-render. Deliberately NOT
/// observable: filling the memo must not invalidate the view that just read it.
/// Keyed on the row id AND its source text, so a row that grows past the
/// streaming window still re-parses exactly once when it settles.
@MainActor
private final class MarkdownCache {
    private var entries: [ChatItem.ID: (source: String, rendered: AttributedString)] = [:]

    func markdown(_ id: ChatItem.ID, source: String) -> AttributedString {
        if let hit = entries[id], hit.source == source { return hit.rendered }
        let parsed = AskBar.markdown(source)
        entries[id] = (source, parsed)
        return parsed
    }
}

// ToolChipRow and the email cards moved to Views/AgentCards.swift when the
// search panel grew a lane that draws the same two rows.
