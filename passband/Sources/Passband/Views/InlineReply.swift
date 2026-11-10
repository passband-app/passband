// REPLY WHERE YOU READ. A composer under the message stack rather than a
// panel over it: the email you are answering stays on screen, unblurred and
// scrollable, which is the whole point of answering from the reader.
//
// ON THE MAC IT IS THE NEXT ROW OF THE CONVERSATION. It sits in the reader's
// own scroll, under the newest message, where the reply will be once it is
// sent (see `ReplyFoot` in ThreadViewer), and its editor is as tall as what
// has been written in it: no fixed box, no second scroll inside the first.
// A long reply is read back the way the mail above it is, by scrolling the
// page.
//
// The ceremony is the pane composer's, and LOCKED: ⌘Enter closes the reply and
// holds it five seconds behind an undo toast (`AppStore.sendWithUndo`), then it
// goes out once. Both composers hand the same state to the same hold, so there is one request shape and one
// error mapping. The body is markdown, live-styled by the same
// MarkdownTextView the pane uses.
//
// THE HEADER LINE IS A DISCLOSURE. Collapsed it says who this reply reaches, in
// one line, because that is all most replies need. Opened it is three editable
// recipient fields — to, cc, bcc — so the answer to "actually, put Dana on bcc"
// is two clicks in the composer you are already in, rather than closing it and
// starting the mail again somewhere that has the fields.
//
// WHICH MEANS THE AUDIENCE CHANGES HANDS. The composer still opens knowing only
// its parent, and the real set is still derived server-side — the parent's
// Reply-To, the room a reply-all widens to. What is new is that the derivation
// is SEEDED into the fields when it lands, and from that moment the fields are
// the answer and the send carries them explicitly (`recipientsStated`). Before
// it lands, and if it never does, the wire carries no recipients at all and the
// daemon derives exactly as it always has — which is why a failed lookup costs a
// preview and never a reply.
//
// The seeded set is also remembered (`seededRecipients`), so the autosave can
// tell "a reply nobody addressed" from "a reply somebody moved to bcc": the
// first must not mint a draft, the second must.
//
// ON A PHONE IT IS NOT A BAR AT ALL. It is the LAST CARD OF THE CHAIN: the reply
// sits under the mail it answers, in the chain's own scroll, where the two
// buttons that opened it were a moment ago (see ChainFoot). It was a bar pinned
// over the keyboard once, and on a screen that size a pinned bar and a raised
// keyboard left one line of the email showing: the reader could not read what
// they were answering, which is the one thing this composer exists to allow.
// Scrolling with the mail, the mail is a thumb's flick away and so is the reply.
//
// Same state, same ceremony, same parts; what is fenced is the ARRANGEMENT.
// One line says who it reaches and opens the fields. The editor has no box of
// its own and no fixed height: it is as tall as what has been typed, because
// the chain is already the scroller and a second one nested in it is two
// surfaces answering one thumb. And the KEY HINT BAR becomes a row of real
// controls, which is not decoration: the ceremony is driven ENTIRELY by keys on
// the Mac, so without them a phone could open a reply and have no way on earth
// to send it.

import SwiftUI

struct InlineReply: View {
    /// The thread's messages, so the draft's `reply_to_message_id` can be resolved
    /// back to the message it answers. Passed in rather than read from the store
    /// because the composer is mounted unconditionally — see ThreadViewer.
    let messages: [ClientMessage]
    /// The thread's subject, for the derived-subject line. Messages carry no
    /// subject of their own on the wire.
    let threadSubject: String

    @Environment(AppStore.self) private var store
    @FocusState private var focusedField: RecipientSlot?

    /// Whether the recipient fields are open. Per-composer by nature: it resets
    /// when the reply closes, because the next one is a different audience.
    @State private var editingRecipients = false
    /// A file is being dragged over the composer: the editor's well says so.
    @State private var dropTargeted = false

    /// The daemon's derived recipient set, TAGGED with the key it was fetched
    /// for. The tag is what makes a stale read impossible: this view is
    /// mounted unconditionally, so bare state would survive one composer and
    /// render — for a frame, before the keyed task clears it — under the next.
    /// nil = nothing landed yet; `(key, nil)` = the fetch for that key failed.
    @State private var fetchedRecipients: (key: String, set: ReplyRecipients?)?

    private var compose: ComposeState? { store.inlineReply }
    /// A send time is set: the verbs say "schedule" (see `ScheduleCopy`).
    /// Only while the daemon keeps an outbox, as in the pane.
    private var scheduled: Bool { compose?.sendAt != nil && store.scheduledSendAvailable }
    /// Bumped when the time palette closes over this composer, so the editor
    /// takes the caret back (`EditorRefocus`): its autofocus fires on mount
    /// alone, and the palette's field had the caret.
    @State private var refocusEditor = 0
    #if os(macOS)
        /// What the body needs at the width it has, as the editor last
        /// measured it (`MarkdownTextView.onContentHeight`). Changes when a
        /// line is added or lost, not per keystroke.
        @State private var bodyHeight: CGFloat = 0
    #endif
    /// The message being answered. nil = nothing to answer, which renders as no
    /// composer at all.
    private var parent: ClientMessage? {
        guard let id = store.inlineReply?.replyToMessageId else { return nil }
        return messages.first { $0.id == id }
    }
    /// What the daemon will title the reply, mirrored for display.
    private var replySubject: String { ComposeCopy.replySubject(threadSubject) }

    /// The fetch's identity: which parent, in which mode. EVERY reply fetches
    /// now, not just reply-all — the daemon honors the parent's Reply-To on a
    /// plain reply too, so the stored sender the client holds can be the wrong
    /// answer. Mode is part of the key so `r` then Enter on the same message
    /// refetches rather than showing the other mode's set.
    private var recipientsKey: String? {
        guard let compose = store.inlineReply, let parent = compose.replyToMessageId else {
            return nil
        }
        return "\(parent):\(compose.replyAll)"
    }

    /// The fetched set, only if it belongs to the CURRENT key.
    private var recipients: ReplyRecipients? {
        guard let fetchedRecipients, fetchedRecipients.key == recipientsKey else { return nil }
        return fetchedRecipients.set
    }

    /// True once the current key's fetch has come back (with or without a set)
    /// — the difference between "deriving…" and "the daemon will derive it".
    private var recipientsSettled: Bool {
        fetchedRecipients?.key == recipientsKey
    }

    var body: some View {
        if let compose, let parent {
            surface(compose, parent: parent)
            // REGISTRATION ORDER IS LOAD-BEARING. Within a context the LATEST
            // registered set wins, and this set mounts with the composer — after
            // the viewer's — which is the only reason Escape here means "leave the
            // composer" while the viewer's Escape still means "leave the email".
            // Hoisting these onto the always-mounted viewer would register them
            // FIRST and invert that layering: Escape would close the thread out
            // from under an open draft.
            .keyBindings(.thread, bindings)
            // Once per (parent, mode). Keyed, and the fetched value carries its
            // key too, so reopening the composer on another message or in the
            // other mode can never show the last one's addresses — not even for
            // the frame before this task runs.
            .task(id: recipientsKey) { await loadRecipients() }
            // A different message (or the same one in the other mode) is a
            // different audience: the fields close so nobody edits one reply's
            // recipients believing they are another's.
            .onChange(of: recipientsKey) { _, _ in editingRecipients = false }
            // Back from the time palette. Nothing is focused once its field
            // goes, and with nothing focused every letter typed is a reader
            // verb: `e` would finish the thread under a half-written reply.
            .onChange(of: store.schedulePicker) { old, new in
                if EditorRefocus.returns(from: old, to: new, slot: .inlineReply) {
                    refocusEditor &+= 1
                }
            }
            // `r` over a reply that is already open (a quiet reopen, usually):
            // the caret goes in, and it is a composer from here on.
            .onChange(of: store.inlineReplyFocus) { _, _ in
                patch { $0.quietOpen = false }
                refocusEditor &+= 1
            }
        }
    }

    /// THE COMPOSER'S OWN SHAPE, which is the one thing the two platforms do
    /// not share: a row at the end of the Mac's conversation, a card at the
    /// end of the phone's chain. Everything hung on it in `body` is common to
    /// both.
    @ViewBuilder
    private func surface(_ compose: ComposeState, parent: ClientMessage) -> some View {
        #if os(macOS)
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: Space.scaled(9)) {
                    headerLine(compose, parent: parent)
                    // ABOVE the words it is about, beside who they go to:
                    // under the editor it was the last thing read, if at all.
                    if compose.fromAgent { AgentDraftNotice() }
                    if editingRecipients {
                        recipientEditor(compose)
                    }
                    editor(compose)
                    // The files, under the editor. Nothing when empty.
                    AttachmentTray(slot: .inlineReply)
                    if let error = compose.error {
                        Text(error).font(Typo.micro).foregroundStyle(Palette.danger)
                    }
                    keyLine
                }
                // A MESSAGE CARD'S OWN MEASURE: the same inset from the
                // column's edge and the same air above and below, so the
                // reply's first line starts where the mail's does.
                .padding(.leading, Self.gutter)
                .padding(.vertical, Space.s16)
                // Anywhere on the composer, same as the pane: see
                // `composeDropTarget`.
                .composeDropTarget(.inlineReply, targeted: $dropTargeted)
            }
            // Inside the reader's column, and as wide as it.
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .top) { Hairline() }
        #else
            card(compose, parent: parent)
        #endif
    }

    /// Ask the daemon who this reply would reach. Best-effort by contract: the
    /// send derives the set again server-side (and, for a reply-all, hard-fails
    /// there if it cannot), so a failure here is a missing preview, never a
    /// blocked reply — which is why it neither surfaces an error nor touches
    /// `compose.error`.
    private func loadRecipients() async {
        guard let key = recipientsKey, let compose = store.inlineReply,
            let parentId = compose.replyToMessageId
        else { return }
        let slot = compose.id
        let fetched = try? await APIClient.shared.replyRecipients(parentId, all: compose.replyAll)
        // The composer may have closed, or moved on, while this was in flight.
        guard recipientsKey == key else { return }
        fetchedRecipients = (key, fetched)
        seed(fetched, into: slot)
    }

    /// HAND THE DERIVED SET TO THE COMPOSER, once, and only while the composer
    /// has not been addressed by anybody yet.
    ///
    /// This is the moment the audience changes hands: before it, the send
    /// carries no recipients and the daemon derives; after it, the fields are
    /// the answer. Seeding what the daemon itself just derived means the two are
    /// the same mail — nobody's reply is quietly re-addressed by the handover.
    ///
    /// Keyed to the composer's identity, like every other write that lands after
    /// an await: the slot may hold a reply to another message by now, and
    /// stamping one message's recipients onto another's draft is the worst
    /// available outcome. `recipientsStated` already being true means either a
    /// restored draft or the sender got here first, and both outrank a
    /// derivation.
    private func seed(_ derived: ReplyRecipients?, into slot: UUID) {
        guard let derived, var next = store.inlineReply, next.id == slot,
            !next.recipientsStated
        else { return }
        let set = Recipients(to: derived.to, cc: derived.cc ?? "")
        next.recipients = set
        // Remembered so the autosave can tell an untouched reply from an
        // addressed one — see `ComposeState.seededRecipients`.
        next.seededRecipients = set
        next.recipientsStated = true
        store.inlineReply = next
    }

    // MARK: - panes

    #if os(macOS)
        private func headerLine(_ compose: ComposeState, parent: ClientMessage) -> some View {
            HStack(spacing: Space.scaled(5)) {
                // THE WHOLE "replying to <who>" PHRASE IS THE DISCLOSURE, chevron
                // and all: the thing you want to change is the thing you click.
                Button {
                    editingRecipients.toggle()
                    if editingRecipients { focusedField = .to }
                } label: {
                    HStack(spacing: Space.scaled(5)) {
                        Image(
                            systemName: editingRecipients
                                ? "chevron.down" : "chevron.right"
                        )
                        .font(Typo.ui(8, weight: .semibold))
                        .foregroundStyle(Palette.inkFaintest)
                        Text(compose.replyAll ? "replying to all" : "replying to")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                        // Addresses and sender strings alike are email-derived:
                        // rendered as Text only, never as markup, and never
                        // interpolated into a localized literal.
                        Text(headerTarget(compose, parent: parent))
                            .font(Typo.ui(11, weight: .medium))
                            .foregroundStyle(Palette.inkDim)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                // A sentence in the micro voice reads as a caption until the
                // pointer says it is a control.
                .pointingHand()
                .accessibilityLabel(editingRecipients ? "hide recipients" : "edit recipients")
                Text("·").foregroundStyle(Palette.inkFaintest)
                Text(replySubject)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: Space.s8)
                AttachButton(slot: .inlineReply)
                TrackerToggle(on: bindFlag(\.includeTracker))
            }
        }
    #endif

    /// Who the header names. A plain reply names the parent's sender; a
    /// reply-all names the fetched set — and until that lands (or when it never
    /// does) it says the derivation is pending rather than naming the sender,
    /// who is NOT certainly in the set: a mailing list's Reply-To routes the
    /// mail somewhere the sender's own address never appears.
    private func headerTarget(_ compose: ComposeState, parent: ClientMessage) -> String {
        // Once the fields hold the answer they ARE the answer, reply-all or
        // not: somebody who just moved a name to bcc has to see the header
        // agree with what they did.
        if compose.recipientsStated, let summary = recipientSummary(compose) { return summary }
        let sender = SenderCache.resolved(parent.senderString).displayName
        guard compose.replyAll else { return sender }
        return recipientsSettled ? "recipients derived at send" : "deriving recipients…"
    }

    /// "alice@example.com +3 more" — the header is one line above the mail, so a
    /// twelve-person thread has to collapse into a count rather than push the
    /// composer around. The full set is in the fields below, and in review.
    ///
    /// COUNTS BLIND COPIES IN THE TOTAL but never names one first: the summary
    /// leads with the visible audience, because "who is this to" is the question
    /// it answers. The bcc row states itself, in the fields and in review.
    private func recipientSummary(_ compose: ComposeState) -> String? {
        let r = compose.recipients
        let visible = r.tokens(.to) + r.tokens(.cc)
        let total = visible.count + r.count(.bcc)
        guard let first = visible.first ?? r.tokens(.bcc).first else { return nil }
        guard total > 1 else { return first }
        return "\(first) +\(total - 1) more"
    }

    /// THE THREE RECIPIENT FIELDS, opened from the header line.
    ///
    /// Not shown until the derivation has landed, and that is a correctness rule
    /// rather than a loading state: editing empty fields beforehand would make
    /// this composer state an audience it never learned, and on a reply-all the
    /// mail would go to one person instead of the room. The wait is one metadata
    /// fetch, and it started when the composer opened.
    @ViewBuilder
    private func recipientEditor(_ compose: ComposeState) -> some View {
        if compose.recipientsStated {
            RecipientFields(
                recipients: recipientsBinding, focus: $focusedField, field: { $0 },
                // The one field with nothing to seed it says why it is empty,
                // rather than reading as a value that got lost.
                placeholder: { $0 == .bcc ? "nobody is blind-copied" : nil })
                .padding(.bottom, Space.s2)
        } else {
            Text("deriving recipients…")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
        }
    }

    #if os(macOS)
        private func editor(_ compose: ComposeState) -> some View {
            // autofocus is the affordance: `r` must land the cursor in the body, or
            // the composer is a box you have to go click. It lives on the EDITOR,
            // not the bar: the editor also mounts on the way BACK from review (the
            // bar never left), so Esc out of review would otherwise drop the cursor
            // and hand every letter you typed next to the reader's verbs.
            MarkdownTextView(
                text: bind(\.body), autofocus: !compose.quietOpen, refocus: refocusEditor,
                disabled: compose.sending,
                // Same two doors the pane's editor has, gated the same way.
                onDropFiles: store.composeAttachmentsAvailable
                    ? { urls, at in ComposeAttach.add(urls: urls, to: .inlineReply, at: at) }
                    : nil,
                onPasteImage: store.composeAttachmentsAvailable
                    ? { png, at in
                        ComposeAttach.add(
                            data: png, filename: "pasted-image.png", mime: "image/png",
                            to: .inlineReply, at: at)
                    } : nil,
                onDropHover: { dropTargeted = $0 },
                // The editor grows with the reply and the page scrolls, not
                // the box (see `editorHeight`).
                onContentHeight: { bodyHeight = $0 }
            )
            .frame(height: editorHeight)
            .padding(.horizontal, Self.wellPadding)
            .padding(.vertical, Space.s8)
            .background(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .fill(Palette.canvas.opacity(0.65))
            )
            .overlay(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .strokeBorder(
                        dropTargeted ? Palette.accent : Palette.hairlineStrong,
                        lineWidth: dropTargeted ? 1.5 : 0.75))
            .animation(.easeOut(duration: 0.12), value: dropTargeted)
            // THE TYPED TEXT ON THE MAIL'S BODY EDGE. The well is pulled
            // out by its own padding and the text view's inset, so the
            // words land on `gutter` (where every message body above starts)
            // and the fill bleeds out to the left of them, toward the
            // column's edge.
            .padding(.leading, -(Self.wellPadding + Self.editorTextInset))
        }

        private static var wellPadding: CGFloat { Space.s6 }
        /// MarkdownTextView's own text inset: its container inset (2) plus
        /// AppKit's default line-fragment padding (5). The composer pane's
        /// editor subtracts the same 7.
        private static let editorTextInset: CGFloat = 7
    #endif

    /// The composer's own inset. The Mac's is the reader column's 22, so the
    /// bar's text starts where the mail's does. The phone's is the inset of a
    /// card that already sits inside that column.
    #if os(macOS)
        private static var gutter: CGFloat { ThreadViewer.replyInset }
        /// AS TALL AS THE REPLY. The floor is a few lines, so an empty
        /// composer reads as somewhere to write rather than a search field;
        /// past that every line written is a line of height, and the page
        /// scrolls instead of the box.
        private var editorHeight: CGFloat { max(Space.scaled(96), bodyHeight) }
    #else
        private static var gutter: CGFloat { Space.s14 }
        /// THE FLOOR, not the height: four lines or so, which is enough to
        /// read as a place to write and short enough that the whole card,
        /// address line to send button, fits between the reader's header and
        /// a raised keyboard. Past it the editor grows with the text.
        private static var editorFloor: CGFloat { Space.scaled(92) }
        private static var cardRadius: CGFloat { Radius.pane }
    #endif

    #if os(macOS)
        private var hints: [KeyHint] {
            [
                KeyHint("⌘enter", ScheduleCopy.sendVerb(scheduled: scheduled)),
                KeyHint("esc", "dismiss"),
            ]
        }

        /// The send keys, as the last line of the composer: the clock just
        /// left of the send verb it changes (`ScheduleClock`), as in the pane.
        /// `KeyHintBar` without the bar. That one is a strip across the foot
        /// of a page with a rule over it, and this is a row in a column that
        /// has its own.
        private var keyLine: some View {
            HStack(spacing: Space.s4) {
                ScheduleClock(slot: .inlineReply).padding(.trailing, Space.s4)
                ForEach(Array(hints.enumerated()), id: \.offset) { i, hint in
                    if i > 0 { Text("·").foregroundStyle(Palette.inkFaintest) }
                    hint
                }
                Spacer()
            }
            .padding(.top, Space.s2)
        }
    #endif

    #if !os(macOS)
        // MARK: - the phone's card

        /// Address line, editor, files and errors, controls: one glass card,
        /// top to bottom in the order a reply is written.
        private func card(_ compose: ComposeState, parent: ClientMessage) -> some View {
            VStack(alignment: .leading, spacing: 0) {
                addressLine(compose, parent: parent)
                // Its own row, always drawn for an agent's draft. It used to
                // sit with the files and errors below, a block that is only
                // there when one of THOSE is, so most agent drafts opened on
                // the phone carried no mark at all.
                if compose.fromAgent {
                    AgentDraftNotice()
                        .padding(.horizontal, Self.gutter)
                        .padding(.bottom, Space.s10)
                }
                if editingRecipients {
                    VStack(alignment: .leading, spacing: Space.s8) {
                        recipientEditor(compose)
                        // What the daemon will title it. Stated, not editable:
                        // a reply's subject is derived from its parent, and
                        // this is where somebody checking the audience looks
                        // for the rest of the envelope.
                        Text(replySubject)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                    .padding(.horizontal, Self.gutter)
                    .padding(.bottom, Space.s10)
                }
                Hairline()
                growingEditor(compose)
                    .padding(.horizontal, Self.gutter)
                    .padding(.top, Space.s6)
                if !compose.attachments.isEmpty || compose.error != nil {
                    VStack(alignment: .leading, spacing: Space.scaled(9)) {
                        // The files, under the editor. Nothing when empty.
                        AttachmentTray(slot: .inlineReply)
                        if let error = compose.error {
                            Text(error).font(Typo.micro).foregroundStyle(Palette.danger)
                        }
                    }
                    .padding(.horizontal, Self.gutter)
                    .padding(.top, Space.s8)
                }
                controls(compose)
                    .padding(.horizontal, Self.gutter)
                    .padding(.top, Space.s10)
                    .padding(.bottom, Space.s12)
            }
            .passbandGlass(.pane, cornerRadius: Self.cardRadius)
            #if DEBUG
                .onAppear { if FixtureStage.recipientsOpen { editingRecipients = true } }
            #endif
        }

        /// WHO IT REACHES, and the way out. The whole phrase is the door to
        /// the recipient fields, as on the Mac: the thing you want to change
        /// is the thing you press. It is the card's one line of chrome, so it
        /// is set at reading size rather than in the micro voice the Mac's bar
        /// uses, where a pointer is there to say a caption is a control.
        private func addressLine(_ compose: ComposeState, parent: ClientMessage) -> some View {
            HStack(spacing: Space.s4) {
                Button {
                    editingRecipients.toggle()
                    if editingRecipients { focusedField = .to }
                } label: {
                    HStack(spacing: Space.s6) {
                        Image(
                            systemName: compose.replyAll
                                ? "arrowshape.turn.up.left.2.fill" : "arrowshape.turn.up.left.fill"
                        )
                        .font(Typo.ui(11, weight: .semibold))
                        .foregroundStyle(Palette.accent)
                        Text(compose.replyAll ? "replying to all" : "replying to")
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkFaint)
                            .fixedSize()
                        // Addresses and sender strings alike are email-derived:
                        // rendered as Text only, never as markup, and never
                        // interpolated into a localized literal.
                        Text(headerTarget(compose, parent: parent))
                            .font(Typo.ui(13, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Image(systemName: "chevron.down")
                            .font(Typo.ui(9, weight: .semibold))
                            .foregroundStyle(Palette.inkFaintest)
                            .rotationEffect(.degrees(editingRecipients ? 180 : 0))
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .frame(minHeight: Self.lineHeight)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(editingRecipients ? "hide recipients" : "edit recipients")
                // "dismiss", never "discard": closing a composer FLUSHES its
                // draft, so the reply is kept and restored next time. The Mac
                // says it with Esc; a card says it in its corner.
                Button { store.closeInlineReply() } label: {
                    Image(systemName: "xmark")
                        .font(Typo.ui(11, weight: .semibold))
                        .foregroundStyle(Palette.inkFaint)
                        .frame(width: Self.lineHeight, height: Self.lineHeight)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("dismiss reply, the draft is kept")
            }
            .padding(.leading, Self.gutter)
            .animation(Motion.disclose, value: editingRecipients)
        }

        /// A thumb's worth of row for the address line and its close button.
        private static var lineHeight: CGFloat { 44 }

        /// The same editor, wired the same way, minus the two things a card in
        /// a scroll view must not have: a well of its own (the card is the
        /// well) and a height (see `MarkdownTextView.grows`).
        private func growingEditor(_ compose: ComposeState) -> some View {
            MarkdownTextView(
                text: bind(\.body), autofocus: !compose.quietOpen, refocus: refocusEditor,
                disabled: compose.sending, grows: true
            )
            .attaching(store.composeAttachmentsAvailable ? .inlineReply : nil)
            .frame(minHeight: Self.editorFloor, alignment: .top)
        }

        /// THE PHONE'S HALF OF THE CEREMONY. The same `send()`: what changes
        /// is that a thumb presses it instead of ⌘Enter.
        ///
        /// What the mail CARRIES on the left (files, the tracker), when and
        /// whether it GOES on the right, the clock just left of the verb it
        /// changes, as in the pane. One row when it fits. A set send time
        /// turns the clock into a chip, which can ask for more than a phone
        /// is wide, so the verbs drop to a row of their own rather than
        /// squeezing every control on the line into an ellipsis.
        private func controls(_ compose: ComposeState) -> some View {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: Space.s8) {
                    carries
                    Spacer(minLength: Space.s8)
                    verbs
                }
                VStack(alignment: .trailing, spacing: Space.s10) {
                    HStack(spacing: Space.s8) {
                        carries
                        Spacer(minLength: 0)
                    }
                    HStack(spacing: Space.s8) { verbs }
                }
            }
        }

        /// The tracker switch is here on a phone now. It was cut while this
        /// was one crowded line pinned over a keyboard; a card has the room,
        /// and a reply whose pixel can only be decided on the Mac is a
        /// setting, not a switch.
        @ViewBuilder
        private var carries: some View {
            // The paperclip is on the keyboard's bar, as in the composer.
            TrackerToggle(on: bindFlag(\.includeTracker))
        }

        @ViewBuilder
        private var verbs: some View {
            // When it goes, just left of the verbs: "send this tomorrow
            // morning" is what a reply typed at midnight wants.
            ScheduleClock(slot: .inlineReply)
            Button { send() } label: {
                verbLabel(ScheduleCopy.sendVerb(scheduled: scheduled))
            }
            .buttonStyle(.glassProminent)
            .tint(Palette.accent)
        }

        private func verbLabel(_ verb: String) -> some View {
            Text(verb)
                .font(Typo.ui(13, weight: .semibold))
                .padding(.horizontal, Space.s6)
                .fixedSize()
        }
    #endif

    // MARK: - keymap

    private var bindings: [KeyBinding] {
        [
            // Escape closes the composer, and only the NEXT press reaches the
            // viewer's Escape and closes the email. Plain Enter is not bound
            // at all: in the body it is a newline.
            //
            // EXCEPT a reply the reader reopened from its draft that nobody
            // has touched (`quietOpen`): the person came to read, so Escape
            // still leaves the email, and the draft stays saved as it was.
            KeyBinding(declining: "Escape", "dismiss reply", allowInInput: true) {
                guard store.inlineReply?.quietOpen != true else { return false }
                store.closeInlineReply()
                return true
            },
            KeyBinding("Enter", "send", meta: true, allowInInput: true) {
                // A draft an agent put on screen is sent with the button,
                // never this chord: see `ComposeState.keyboardSendLocked`.
                guard store.inlineReply?.keyboardSendLocked != true else {
                    store.noteKeyboardSendLocked()
                    return
                }
                send()
            },
            // The clock's chord, as in the pane (`ScheduleClock.chord`).
            KeyBinding(
                declining: ScheduleClock.chordKey, "send later", meta: true, allowInInput: true,
                exactCase: true
            ) {
                guard !store.askBarOpen, store.scheduledSendAvailable else { return false }
                store.schedulePicker = .inlineReply
                return true
            },
        ]
    }

    // MARK: - state helpers

    private func bind(_ keyPath: WritableKeyPath<ComposeState, String>) -> Binding<String> {
        Binding(
            get: { store.inlineReply?[keyPath: keyPath] ?? "" },
            set: { value in
                // The autosave's one hook for this composer — the body is the only
                // field it has, and it is bound through here.
                guard store.inlineReply?[keyPath: keyPath] != value else { return }
                patch {
                    $0[keyPath: keyPath] = value
                    // Typed into: it is a composer now, not a draft on show.
                    $0.quietOpen = false
                }
                DraftSaver.shared.noteChange(.inlineReply)
            })
    }

    /// The three recipient fields as one binding. Writes go through
    /// `stateRecipients` — touching a recipient field is the sender taking the
    /// audience over from the daemon — and arm the autosave like any other
    /// edit: a bcc added and then abandoned has to come back.
    private var recipientsBinding: Binding<Recipients> {
        Binding(
            get: { store.inlineReply?.recipients ?? Recipients() },
            set: { value in
                guard store.inlineReply?.recipients != value else { return }
                patch {
                    $0.stateRecipients(value)
                    $0.quietOpen = false
                }
                DraftSaver.shared.noteChange(.inlineReply)
            })
    }

    /// Same shape as `bind`, minus the autosave: a draft records what was
    /// written, not how the next send is addressed.
    private func bindFlag(_ keyPath: WritableKeyPath<ComposeState, Bool>) -> Binding<Bool> {
        Binding(
            get: { store.inlineReply?[keyPath: keyPath] ?? false },
            set: { value in patch { $0[keyPath: keyPath] = value } })
    }

    private func patch(_ mutate: (inout ComposeState) -> Void) {
        guard var next = store.inlineReply else { return }
        mutate(&next)
        store.inlineReply = next
    }

    /// Same two checks as the pane composer, then the same hold, or with a
    /// time set the same outbox.
    private func send() {
        guard let compose = store.inlineReply else { return }
        // Same seed rule as the pane: an untouched signature is an empty body.
        guard !Prefs.shared.isBodyUntouched(compose.body) else {
            patch { $0.error = "body is empty" }
            return
        }
        // Same tray rule as the pane: a file still uploading or one that
        // failed stops the send here, in words.
        if let problem = ComposeCopy.trayProblem(compose) {
            patch { $0.error = problem }
            return
        }
        if scheduled {
            store.scheduleCompose(.inlineReply)
        } else {
            store.sendWithUndo(.inlineReply)
        }
    }
}
