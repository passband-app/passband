// The send ceremony — the one irreversible action in the app. ⌘Enter closes
// the composer and HOLDS the mail for five seconds behind an undo toast
// (`AppStore.sendWithUndo`); only then does it go out, once *without*
// override_guard. A clean pass is sent; a 422 reopens this composer with the
// redacted guard kinds, and sending past them is a distinct act (⌘⇧Enter or
// the danger button). 403 means no write credential.
//
// There used to be a review phase between the two. The hold replaced it: a
// read-back screen on every send taxed every mail to catch the rare wrong one,
// and an undo catches that one for free.
//
// This is the PANE composer: a right-hand working surface in MainShell's
// layout, half the window wide — the page beside it shrinks and stays live,
// because starting an email should not mean losing sight of the inbox that
// prompted it. No scrim, no blur; Esc closes it like the side panels. Replies
// open the reader's inline composer (InlineReply), which runs the same ceremony
// against the same `ComposeSubmit`; this pane owns the new-message path
// (`replyToMessageId == nil`), plus the reply shape it still supports for any
// caller with no thread to open.
//
// The body is markdown, styled LIVE with the markers kept visible (see
// MarkdownTextView); the daemon renders the HTML half of what actually goes
// out from this same source (`body_format: "markdown"`).
//
// ON A PHONE THE PANE IS A SHEET, and that is the only structural difference:
// MobileRootView presents this same view at `.large` off the same
// `store.compose`, so opening, restoring, autosaving and sending are one code
// path on both platforms. What is fenced below is the DESKTOP FURNITURE only —
// the keys printed beside the footer's verbs, and
// the pane's glass edge and its leftward shadow (a sheet brings its own
// ground). The ceremony itself, both phases of it, is shared and untouched: a
// phone drives it with the same footer buttons, minus their keys.
//
// TWO PAGES, ONE STRIP. The drafts control beside the kind label flips
// `ComposeState.page`: on `.drafts` the header stays and the body (fields,
// editor, footer) swaps for `DraftsList`, full width, with the composer kept
// exactly as it was underneath, one Enter or one click on the control away.
// ⌘N always opens the composer page; the control's count is how older drafts
// make themselves known (see `AppStore.openComposeNew`). The header says which
// page is up: the title reads "drafts" and the control turns into the way back.

import SwiftUI

struct ComposePane: View {
    @Environment(AppStore.self) private var store
    @FocusState private var focusedField: FocusTarget?

    private enum FocusTarget: Hashable { case recipient(RecipientSlot), subject }

    @State private var groupPickerOpen = false
    /// A file is being dragged over the pane: the editor's well says so.
    @State private var dropTargeted = false
    /// The picked group's size, for the fan-out pill's "· 12 ·". Held here rather
    /// than on ComposeState because it is a display detail the daemon re-reads
    /// from `groupId` anyway.
    @State private var groupMemberCount = 0

    private var compose: ComposeState? { store.compose }
    private var guarded: Bool { !(compose?.guardKinds.isEmpty ?? true) }
    /// A send time is set: the verbs say "schedule", and ⌘↵ files the mail in
    /// the daemon's outbox instead of holding it for five seconds. Only while
    /// the daemon keeps an outbox: a gate that went off under a set time (an
    /// older daemon after a reconnect) makes the mail an ordinary send, and
    /// the verb says so rather than promising a schedule ⌘↵ would 404.
    private var scheduled: Bool { compose?.sendAt != nil && store.scheduledSendAvailable }
    /// Opened out to a centred column over the page. The Mac's alone: a phone
    /// sheet is already the whole screen.
    private var expanded: Bool {
        #if os(macOS)
            store.composeExpanded
        #else
            false
        #endif
    }

    var body: some View {
        if let compose {
            VStack(alignment: .leading, spacing: 0) {
                header(compose)

                if compose.page == .drafts {
                    // THE DRAFTS PAGE: the list IS the body, and brings its
                    // own muted key line where the footer was. No send and no
                    // attach here: neither acts on a list.
                    DraftsList()
                } else {
                    composerPage(compose)
                }
            }
            // FULL SCREEN IS A COLUMN, not the window: a line of mail as wide
            // as a monitor is unreadable. The reader's own measure, so a mail
            // is written at the width it will be read at.
            .frame(maxWidth: expanded ? ThreadViewer.columnWidth : .infinity)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // A pane is an EDGE in a window: glass, and a shadow thrown left
            // over the page it half-covers. A sheet is neither — it has its own
            // presented shape and nothing beside it to cast onto — so the phone
            // takes the plain canvas the rest of its surfaces stand on.
            #if os(macOS)
                .passbandGlass(.pane, cornerRadius: 0, tint: Palette.glassTintStrong)
                .shadow(color: .black.opacity(0.24), radius: 40, x: -14)
            #else
                .background(Palette.canvas)
            #endif
            .keyContext(.modal)
            .keyBindings(.modal, bindings)
            .onAppear { focusedField = .recipient(.to) }
            // Back from the drafts page (or a draft opened from it): the
            // fields remounted with nothing focused, so put the caret where
            // the mail still needs something, the way opening does.
            .onChange(of: compose.page) { _, page in
                guard page == .compose else { return }
                refocus()
            }
            .onChange(of: compose.id) { _, _ in
                if store.compose?.page == .compose { refocus() }
            }
            // Back from the time palette: its field had the caret, and a
            // composer left with nothing focused hands the next letter typed
            // to whatever binds it.
            .onChange(of: store.schedulePicker) { old, new in
                if EditorRefocus.returns(from: old, to: new, slot: .compose),
                    store.compose?.page == .compose
                {
                    refocus()
                }
            }
            // ANYWHERE ON THE PANE: a file let go over the subject line or
            // the tray still lands.
            .composeDropTarget(.compose, targeted: $dropTargeted)
        }
    }

    /// The first field still empty, else the subject: the body is a platform
    /// text view outside `FocusState`, so the subject is the nearest stop.
    private func refocus() {
        guard let compose = store.compose else { return }
        focusedField = compose.to.trimmed.isEmpty ? .recipient(.to) : .subject
    }

    /// Page one: the fields, the editor, and the footer that sends them.
    @ViewBuilder
    private func composerPage(_ compose: ComposeState) -> some View {
        VStack(alignment: .leading, spacing: Space.s12) {
            editPane(compose)
            // A held send came back blocked: the verdict, and with it
            // the override in the footer.
            if guarded {
                GuardVerdictBox(kinds: compose.guardKinds)
            }
            if let error = compose.error {
                Text(error).font(Typo.micro).foregroundStyle(Palette.danger)
            }
        }
        .padding(.horizontal, Space.scaled(18))
        .padding(.vertical, Space.s14)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

        // NO FOOTER ON A PHONE, unless the guard blocked a send. Send
        // went to the header and cancel to the drag-down gesture, and a
        // bar left behind with nothing in it would still cost its own
        // height between the editor and the keyboard. A blocked send
        // needs the override, and the override deserves a wide target.
        #if os(macOS)
            footer(compose)
        #else
            if guarded { footer(compose) }
        #endif
    }

    private func header(_ compose: ComposeState) -> some View {
        let onList = compose.page == .drafts
        return HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
            // THE SERIF MOMENT. A sheet has no chrome around it — it IS the
            // screen — so the phone spends Newsreader here, once, on the word
            // that names what you are doing.
            //
            // The Mac names the KIND of mail instead, in plain SF: this was
            // "COMPOSE" in the engraved, width-expanded label face with the kind
            // whispered beside it, which said the same thing twice in two
            // voices, the louder one the less useful.
            //
            // ON THE DRAFTS PAGE the title names the page instead, in the
            // same voice and the same spot, and the control beside it becomes
            // the way back: the header reads as a two-way switch without
            // growing a second row of tabs.
            #if os(macOS)
                Text(onList ? "drafts" : kindLabel(compose))
                    .font(Typo.zoneTitle)
                    .foregroundStyle(Palette.ink)
                draftsToggle(compose)
            #else
                Text(onList ? "drafts" : "compose")
                    .font(Typo.serif(24, weight: .medium))
                    .foregroundStyle(Palette.ink)
                if !onList {
                    Text(kindLabel(compose))
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }
                draftsToggle(compose)
            #endif
            Spacer()
            // NO `Esc close` CHIP, AND NO CANCEL BUTTON IN THE FOOTER EITHER.
            // Esc closes this pane like every other panel in the app; saying
            // so in a chip or a button was the pane promising one key twice.
            #if os(macOS)
                expandButton
            #else
                // THE PHONE'S PRIMARY ACTION, IN THE CORNER IT LIVES IN. On a
                // phone the edit phase is a sheet with the keyboard up, and the
                // bottom of the screen is spoken for three times over — the
                // keyboard, its predictive row, and the markdown bar above both.
                // A footer under all of that is the one strip of chrome the
                // thumb has to reach PAST its own keyboard to use, so the verb
                // moves to the bar, where every other phone puts it.
                //
                // AND CANCEL IS NOT BESIDE IT, because the sheet already is
                // cancel: a drag down runs `closeCompose()` through the same
                // binding the button called, and the drag indicator is showing.
                // Two doors out, one of them costing a corner of the bar, was a
                // button spent on something the gesture already did.
                //
                // A BLOCKED send keeps the corner for the plain send (after
                // editing the match out) and puts the override in a footer of
                // its own: sending through the guard stays a wide, deliberate
                // target rather than a chip in the corner.
                //
                // ATTACH RIDES BESIDE IT, because the footer it has on the Mac
                // is not here. The inline reply already offers it on a phone;
                // a new message that could not take a file was the odd one out.
                //
                // NEITHER ON THE DRAFTS PAGE: a list has nothing to send.
                //
                // THE CLOCK SITS JUST LEFT OF SEND, as on the Mac: it is the
                // verb's modifier, and "send" flips to "schedule" once a time
                // is set.
                if !onList {
                    AttachButton(slot: .compose)
                    ScheduleClock(slot: .compose)
                    Button(ScheduleCopy.sendVerb(scheduled: scheduled)) { send(override: false) }
                        .buttonStyle(.glassProminent)
                        .tint(Palette.accent)
                }
            #endif
        }
        .padding(.horizontal, Space.scaled(18))
        .padding(.vertical, Space.scaled(13))
        .overlay(alignment: .bottom) { Hairline() }
    }

    #if os(macOS)
        /// Open the pane out to a centred column over the page, or back.
        private var expandButton: some View {
            Button {
                store.composeExpanded.toggle()
            } label: {
                Image(
                    systemName: expanded
                        ? "arrow.down.right.and.arrow.up.left"
                        : "arrow.up.left.and.arrow.down.right"
                )
                .font(Typo.ui(11, weight: .semibold))
                .frame(width: Space.scaled(22), height: Space.scaled(22))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(Palette.inkFaint)
            .pointingHand()
            .help(expanded ? "back to the side pane" : "full screen")
            .accessibilityLabel(expanded ? "Exit full screen" : "Full screen")
        }
    #endif

    /// THE DRAFTS CONTROL, beside the title: the kind label says what this
    /// mail is, and this answers which one. It sits in the header rather than
    /// the footer because the footer's left cluster holds the mail's options
    /// (attach, track) and its right the verbs (the send clock, send), and
    /// this is neither.
    ///
    /// A TOGGLE between the pane's two pages, in one spot, so a second click
    /// undoes the first. On the composer it counts the drafts you could open
    /// from here (the one this pane is editing is not "another draft"); at
    /// zero the word stays, muted, so the door does not come and go as you
    /// type. On the drafts page it is the way back, in the accent, naming
    /// what it goes back to. Hidden on a daemon without the drafts list
    /// (`draftsV2Available`).
    @ViewBuilder
    private func draftsToggle(_ compose: ComposeState) -> some View {
        if store.draftsV2Available {
            let onList = compose.page == .drafts
            let count = store.otherDrafts.count
            Button {
                patch { $0.page = onList ? .compose : .drafts }
            } label: {
                HStack(spacing: Space.scaled(3)) {
                    Image(systemName: onList ? "arrow.uturn.backward" : "square.stack")
                        .font(Typo.ui(9, weight: .semibold))
                    Text(onList ? "back to \(kindLabel(compose))" : Self.draftsLabel(count))
                }
                .font(Typo.micro)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(
                onList ? Palette.accent : count == 0 ? Palette.inkFaintest : Palette.inkFaint
            )
            .pointingHand()
            .help(onList ? "back to the message" : "saved drafts")
            .accessibilityLabel(
                onList
                    ? "Back to \(kindLabel(compose))"
                    : count == 0 ? "Drafts" : "\(Self.draftsLabel(count)), show list")
        }
    }

    /// "drafts" / "1 draft" / "3 drafts".
    static func draftsLabel(_ count: Int) -> String {
        switch count {
        case 0: "drafts"
        case 1: "1 draft"
        default: "\(count) drafts"
        }
    }

    /// Which of the three this composer is, in the header's lowercase voice.
    private func kindLabel(_ compose: ComposeState) -> String {
        if compose.forwardOfMessageId != nil { return "forward" }
        return compose.replyToMessageId != nil ? "reply" : "new message"
    }

    // MARK: - the forwarded original

    /// How tall the quote may grow in the EDIT phase before it starts scrolling
    /// inside itself. One number for both platforms on purpose: it is a CEILING,
    /// not a height — the editor above is greedy too, so a short pane (or a
    /// phone sheet with the keyboard up) splits the space between them and the
    /// quote simply gets less than this. Review has no ceiling at all; there is
    /// nothing to type into by then, so the whole thing scrolls with the note.
    private static let quoteEditHeight: CGFloat = 300

    /// WHAT RIDES ALONG on a forward, in both phases: the message itself,
    /// indented behind a rail the way every mail client draws included mail.
    ///
    /// The real quote is assembled by the DAEMON out of its own raw fetch, so
    /// nothing here is on the wire — this is the reader's sanitized copy of the
    /// same message (see `ComposeState.forwardedMessage`), shown so the composer
    /// is not an empty new message that inexplicably sends a fat email, and so
    /// review, whose whole job is promising what goes out, promises the whole of
    /// it rather than the covering note.
    ///
    /// NO `to` OR `cc` LINES, and their absence is a fact about the client
    /// rather than a choice: `ClientMessage` carries the sender and nothing else
    /// about the audience, while the daemon's block writes `To:` and `Cc:` from
    /// the raw headers. Inventing them from what is in reach would be inventing
    /// recipients, and the one question a forwarded header block exists to
    /// answer is who was on it.
    @ViewBuilder
    private func forwardedQuote(_ compose: ComposeState) -> some View {
        if compose.forwardOfMessageId != nil, let message = compose.forwardedMessage {
            VStack(alignment: .leading, spacing: Space.scaled(9)) {
                // The composer's mirror of the wire's
                // "---------- Forwarded message ---------" banner: the same
                // sentence the outgoing mail carries, said in the house's micro
                // voice instead of in a row of hyphens.
                Text("forwarded message")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .textCase(.uppercase)

                // The header lines of that banner, in review's own summary
                // grammar — a header being checked, in mono. The subject comes
                // off `forwardedSubject` rather than off the message, because
                // that is the value the outgoing `Fwd: …` title was built from
                // and the two must not read differently.
                VStack(alignment: .leading, spacing: Space.scaled(3)) {
                    ComposeSummaryRow("from", message.senderString)
                    ComposeSummaryRow("date", Fmt.dateTime(message.received_at))
                    ComposeSummaryRow("subject", compose.forwardedSubject ?? "(no subject)")
                }

                // THE READER'S OWN BODY VIEWS, picked by the reader's own test
                // (MessageCard) — html when there is any, plain text otherwise.
                // Same `cacheKey` the reader passes, so this shares the frame
                // pool and the image cache with the thread behind the pane
                // rather than fetching every picture a second time, and the same
                // tracker policy, so opening a composer cannot load a pixel the
                // reader refused.
                if let html = message.html, !html.isEmpty {
                    EmailWebView(
                        html: html, cacheKey: String(message.id),
                        allowTrackers: message.allowsTrackers)
                } else {
                    PlainBody(content: message.content)
                }

                // Unconditional, exactly as the reader mounts it: the strip
                // draws nothing at all when there are no files.
                AttachmentStrip(attachments: message.attachmentList)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // INDENTED LIKE QUOTED MAIL: the whole block sits behind a static
            // bar in the hairline token. Same idiom as the reader's selection
            // rail, minus the only thing that rail does — this one never moves
            // and never changes color, because it marks a kind of content
            // rather than where you are.
            .padding(.leading, Space.s12)
            .overlay(alignment: .leading) {
                RoundedRectangle(cornerRadius: 1, style: .continuous)
                    .fill(Palette.hairlineStrong)
                    .frame(width: 2)
            }
        }
    }

    private func footer(_ compose: ComposeState) -> some View {
        // THE KEYS LIVE IN THE BUTTONS. This bar used to carry a row of key
        // hints on the left AND the same verbs as buttons on the right, and at
        // half a window wide the hints lost the fight for room and wrapped
        // mid-word. One statement per verb: the button, with its key beside it.
        HStack(spacing: Space.s8) {
            // THE MESSAGE OPTIONS, DESKTOP ONLY. The phone renders this bar
            // only for a blocked send (see the call site), and its attach
            // chip lives in the header beside send instead: the tracker switch
            // is what a phone would have to give the row for, and one switch
            // nobody came here to touch is not worth a strip of chrome between
            // the editor and the keyboard. A phone that wants the pixel changes
            // the account default.
            //
            // At the far LEFT, away from the verbs: a switch beside the send
            // button is a switch nobody meant to touch.
            #if os(macOS)
                AttachButton(slot: .compose)
                TrackerToggle(on: bindFlag(\.includeTracker))
            #endif
            Spacer()
            // WHEN IT GOES sits with the verbs, not the options: it changes
            // what the verb beside it does. An icon until a time is picked,
            // then the chip. Draws nothing on a daemon without an outbox.
            #if os(macOS)
                ScheduleClock(slot: .compose)
            #endif
            // Past a blocked verdict the plain send stays (the match may have
            // been edited out) and the override stands beside it in danger red:
            // two acts, never one button that quietly means both.
            //
            // A SET TIME FLIPS BOTH VERBS, "schedule" and "schedule anyway":
            // same keys, and the button says what it does. One computed string
            // each (`ScheduleCopy`), so the label and the act cannot disagree.
            if guarded {
                Button { send(override: true) } label: {
                    keyed(ScheduleCopy.overrideVerb(scheduled: scheduled), "⌘⇧↵")
                }
                    .buttonStyle(.glassProminent)
                    .tint(Palette.danger)
            }
            #if os(macOS)
                Button { send(override: false) } label: {
                    keyed(ScheduleCopy.sendVerb(scheduled: scheduled), "⌘↵")
                }
                    .buttonStyle(.glassProminent)
                    .tint(Palette.accent)
            #endif
        }
        .padding(.horizontal, Space.scaled(18))
        .padding(.vertical, Space.s12)
        .overlay(alignment: .top) { Hairline() }
    }

    /// A footer verb with its key beside it, dimmer, in the key chip's mono.
    /// The key half is Mac only: a phone has no Esc to promise.
    private func keyed(_ verb: String, _ key: String) -> some View {
        HStack(spacing: Space.s6) {
            Text(verb)
            #if os(macOS)
                Text(key)
                    .font(Typo.mono(10, weight: .medium))
                    .opacity(0.6)
            #endif
        }
    }

    /// ONE CARD, THE WAY A MAIL HEADER IS ONE BLOCK: the recipient lines and
    /// the subject as labelled lines ruled apart, and the body under them on
    /// the same ground. This used to be three captioned wells stacked like a
    /// sign-up form (plus a fourth caption teaching markdown), and a message is
    /// not a form: the boxes were most of what the eye had to read.
    ///
    /// The markdown cheat sheet went with the body's caption. The editor styles
    /// markdown LIVE with the markers kept visible, so the syntax teaches itself
    /// the first time it is typed.
    private func editPane(_ compose: ComposeState) -> some View {
        VStack(alignment: .leading, spacing: Space.s12) {
            VStack(alignment: .leading, spacing: 0) {
                recipientRow(compose)
                Hairline()
                InlineFieldRow(label: "subject") {
                    // Left blank on a reply the daemon titles from the parent;
                    // the placeholder says so, because an empty field otherwise
                    // reads as an unset required value.
                    TextField(subjectPlaceholder, text: bind(\.subject))
                        .textFieldStyle(.plain)
                        .focused($focusedField, equals: .subject)
                }
                Hairline()
                MarkdownTextView(
                    text: bind(\.body),
                    // Dropped ON THE EDITOR: at the drop point. Pasted: at
                    // the caret. Both are the daemon-gated affordance, so a
                    // daemon that cannot stage files gets AppKit's default.
                    onDropFiles: store.composeAttachmentsAvailable
                        ? { urls, at in ComposeAttach.add(urls: urls, to: .compose, at: at) }
                        : nil,
                    onPasteImage: store.composeAttachmentsAvailable
                        ? { png, at in
                            ComposeAttach.add(
                                data: png, filename: "pasted-image.png", mime: "image/png",
                                to: .compose, at: at)
                        } : nil,
                    onDropHover: { dropTargeted = $0 }
                )
                .frame(maxHeight: .infinity)
                // The text view's own inset plus its line-fragment padding make
                // up the rest, so the body's left edge is the labels' edge.
                .padding(.horizontal, InlineFieldMetrics.inset - 7)
                .padding(.vertical, Space.s8)
                // The files, under the editor where a mail client puts them,
                // still on the card. Draws nothing when there are none.
                AttachmentTray(slot: .compose)
                    .padding(.horizontal, InlineFieldMetrics.inset)
                    .padding(.bottom, Space.s10)
            }
            .frame(maxHeight: .infinity)
            // Near-opaque, like every input well: a translucent card over a
            // busy wallpaper leaves typed text unreadable.
            .background(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(Palette.canvas.opacity(0.65))
            )
            // The drop cue is the whole card lighting up: a file let go
            // anywhere on the message lands (see `composeDropTarget`).
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(
                        dropTargeted ? Palette.accent : Palette.hairlineStrong,
                        lineWidth: dropTargeted ? 1.5 : 0.75))
            .animation(.easeOut(duration: 0.12), value: dropTargeted)

            // UNDER the editor, where the quote sits in the mail itself, and in
            // a scroller of its own: the note is what you are writing and keeps
            // the flexible remainder, while the original — which can be a whole
            // newsletter — scrolls inside a bounded pocket instead of pushing
            // the editor off the pane.
            //
            // The EDITOR is deliberately not the thing wrapped: MarkdownTextView
            // is a platform text view that scrolls itself, and nesting it in a
            // ScrollView breaks its own scrolling.
            if compose.forwardOfMessageId != nil {
                ScrollView { forwardedQuote(compose) }
                    .frame(maxHeight: Self.quoteEditHeight)
            }
        }
    }

    /// The `to` line and the two affordances that live beside it: browse groups,
    /// and open the bcc row.
    ///
    /// A REPLY GETS NEITHER. A group is an audience and a reply already has one;
    /// the daemon refuses the combination outright, so offering the button here
    /// would be offering a refusal.
    @ViewBuilder
    private func recipientRow(_ compose: ComposeState) -> some View {
        // `to`, with cc and bcc folded behind their own labels on its line
        // — and unfolded on their own whenever they hold anybody. The group
        // affordances belong to `to` alone: a group is an audience, and an
        // audience is who the mail is TO. See `RecipientFields`.
        RecipientFields(
            recipients: recipientsBinding, focus: $focusedField,
            field: FocusTarget.recipient,
            suggestGroups: canAddressGroup,
            onGroupPicked: { pick($0) },
            resolvedGroup: compose.groupName.map {
                (name: $0, count: groupMemberCount)
            },
            inline: true,
            toAccessory: canAddressGroup ? AnyView(groupsButton) : nil)
    }

    /// Browse groups, on the `to` line beside cc/bcc: the three things that
    /// widen who a message is to, in one place. It used to hang on a line of
    /// its own under the field, a whole row for one small word.
    private var groupsButton: some View {
        Button {
            groupPickerOpen = true
        } label: {
            HStack(spacing: Space.scaled(3)) {
                Image(systemName: "person.2").font(Typo.ui(9, weight: .semibold))
                Text("groups")
            }
            .font(Typo.micro)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(Palette.inkFaintest)
        .pointingHand()
        .help("address a send group")
        .popover(isPresented: $groupPickerOpen, arrowEdge: .bottom) {
            GroupPicker { group in
                groupPickerOpen = false
                pick(group)
            }
        }
    }

    /// Groups address a NEW message. A reply and a forward each already have an
    /// audience, and the daemon refuses a `group_id` on either.
    private var canAddressGroup: Bool {
        compose?.replyToMessageId == nil && compose?.forwardOfMessageId == nil
    }

    /// What picking a group does, and the whole of the mode's meaning in this
    /// composer:
    ///
    /// * `to` / `bcc` — EXPAND into that field now, as ordinary address pills.
    ///   What goes on the wire is real recipients, so what the sender reviews is
    ///   exactly who it reaches. The group id rides along as attribution only.
    /// * `individual` — ONE pill, because there is no single message to address.
    ///   The daemon reads the membership itself; `to` carries only the token that
    ///   keeps the pill alive across a draft round-trip.
    private func pick(_ group: SendGroup) {
        Task {
            // The list read carries counts, not membership, so to/bcc need the
            // full group before they can expand it.
            let full = (try? await APIClient.shared.group(group.id)) ?? group
            let members = (full.members ?? []).map(\.addr)
            patch { state in
                // A new audience relocks send-anyway, same as typing one.
                state.guardKinds = []
                state.groupId = full.id
                state.groupMode = full.mode
                state.groupName = full.name
                switch full.mode {
                case .to:
                    state.to = merge(state.to, members)
                case .bcc:
                    // No reveal flag to set: `RecipientFields` unfolds any row
                    // that holds somebody, which is the same rule that keeps a
                    // restored draft's bcc from hiding itself.
                    state.bcc = merge(state.bcc, members)
                case .individual:
                    // The token REPLACES whatever was in the field. A fan-out
                    // addresses one audience and nobody else: leaving a typed
                    // address beside it would be a second, silent recipient of a
                    // mail whose whole point is that it is one-to-one.
                    state.to = GroupToken.encode(full)
                }
            }
            groupMemberCount = full.member_count
            DraftSaver.shared.noteChange(.compose)
        }
    }

    /// Add addresses to a comma-joined field without duplicating what is there.
    private func merge(_ existing: String, _ addrs: [String]) -> String {
        var out = existing.split(separator: ",").map { String($0).trimmed }
            .filter { !$0.isEmpty }
        let seen = Set(out.map { $0.lowercased() })
        for addr in addrs where !seen.contains(addr.lowercased()) {
            out.append(addr)
        }
        return out.joined(separator: ", ")
    }

    private var isReply: Bool { compose?.replyToMessageId != nil }

    /// Stands in for an empty subject on a reply, in both panes: the daemon titles
    /// it `Re: <parent subject>`, and the real parent subject is not in reach here
    /// (the update carries an LLM summary, not the header).
    private var subjectPlaceholder: String {
        isReply ? ComposeCopy.derivedSubject : "subject"
    }

    // MARK: - keymap

    /// ⌘Enter sends from anywhere in the pane, the body included. Plain Enter
    /// is left alone everywhere: it used to open review from the to/subject
    /// lines, and with review gone an Enter that SENDS from the subject line
    /// would be a mail out the door on the keystroke people use to finish a
    /// field.
    private var bindings: [KeyBinding] {
        [
            // DECLINES ON THE DRAFTS PAGE: there Esc means "back", which the
            // list decides (`DraftsList`), whichever set registered first.
            KeyBinding(declining: "Escape", "cancel", allowInInput: true) {
                guard store.compose?.page != .drafts else { return false }
                store.closeCompose()
                return true
            },
            // THE ASK BAR IS A MODAL ON TOP OF THIS PANE, in the same key
            // context, and binds only Escape: a ⌘Enter typed into its field
            // falls through to here, and must not send the mail underneath.
            KeyBinding(declining: "Enter", "send", meta: true, allowInInput: true) {
                // Not from the drafts page: the mail being sent is not on
                // screen, and a send should never be of something unseen.
                guard !store.askBarOpen, store.compose?.page != .drafts else { return false }
                send(override: false)
                return true
            },
            // Past a blocked verdict only; declines otherwise, so the chord is
            // not a quiet way to skip a guard that has not spoken.
            KeyBinding(declining: "shift+Enter", "send anyway", meta: true, allowInInput: true) {
                // THE ASK BAR IS A MODAL ON TOP OF THIS PANE, and KeyMonitor
                // walks the sets newest-first; a chord typed into its field
                // must not send THROUGH the outbound guard from under it.
                guard !store.askBarOpen, guarded, store.compose?.page != .drafts else {
                    return false
                }
                send(override: true)
                return true
            },
            // THE CLOCK'S CHORD (`ScheduleClock.chord`, ⌘⇧L). Declines where
            // the clock is not drawn: on a daemon without an outbox, and on
            // the drafts page, where no mail is on screen to schedule.
            // EXACT CASE: ⌘⇧L, never a bare ⌘L through the fold.
            KeyBinding(
                declining: ScheduleClock.chordKey, "send later", meta: true, allowInInput: true,
                exactCase: true
            ) {
                guard !store.askBarOpen, store.scheduledSendAvailable,
                    store.compose?.page != .drafts
                else { return false }
                store.schedulePicker = .compose
                return true
            },
        ]
    }

    // MARK: - state helpers

    /// The three recipient fields as one binding. Writes go through
    /// `stateRecipients`, which records that this composer's fields ARE the
    /// audience — see `ComposeState.recipientsStated`. Autosaves like every
    /// other field: a Bcc typed and then abandoned has to come back.
    private var recipientsBinding: Binding<Recipients> {
        Binding(
            get: { store.compose?.recipients ?? Recipients() },
            set: { value in
                guard store.compose?.recipients != value else { return }
                patch {
                    $0.stateRecipients(value)
                    $0.guardKinds = []
                }
                DraftSaver.shared.noteChange(.compose)
            })
    }

    private func bind(_ keyPath: WritableKeyPath<ComposeState, String>) -> Binding<String> {
        Binding(
            get: { store.compose?[keyPath: keyPath] ?? "" },
            set: { value in
                // Every field of this composer is bound through here, which is why
                // the autosave hooks HERE and nowhere else: there is no way to edit
                // the draft without arming a save.
                guard store.compose?[keyPath: keyPath] != value else { return }
                // AN EDIT RELOCKS THE OVERRIDE. "Send anyway" is consent to
                // the mail the guard judged; after a keystroke it is a
                // different mail, and the plain send is what judges it again.
                patch {
                    $0[keyPath: keyPath] = value
                    $0.guardKinds = []
                }
                DraftSaver.shared.noteChange(.compose)
            })
    }

    /// Same shape as `bind`, minus the autosave: a draft records what was
    /// written, not how the next send is addressed.
    private func bindFlag(_ keyPath: WritableKeyPath<ComposeState, Bool>) -> Binding<Bool> {
        Binding(
            get: { store.compose?[keyPath: keyPath] ?? false },
            set: { value in patch { $0[keyPath: keyPath] = value } })
    }

    private func patch(_ mutate: (inout ComposeState) -> Void) {
        guard var next = store.compose else { return }
        mutate(&next)
        store.compose = next
    }

    /// Hand the mail to the hold (`AppStore.sendWithUndo`), or with a time set
    /// to the outbox (`AppStore.scheduleCompose`), after the two checks that
    /// have to happen while the composer is still on screen to show their
    /// answer. The checks are the same for both: a scheduled mail is still a
    /// mail, and its tray is still the promise.
    private func send(override: Bool) {
        guard let compose = store.compose else { return }
        // Untouched covers the seeded signature: a signature under nothing is
        // not a message, and must not be one keystroke from going out.
        //
        // A FORWARD IS EXEMPT, and has to be: its content is the original the
        // daemon quotes underneath, so "here, look at this" with nothing typed
        // above it is the ordinary case rather than an empty message. The wire
        // agrees — `forward_of_message_id` is the one shape that accepts an
        // empty body — and refusing it here would make the bare forward, which
        // is most of them, unsendable.
        guard compose.forwardOfMessageId != nil || !Prefs.shared.isBodyUntouched(compose.body)
        else {
            patch { $0.error = "body is empty" }
            return
        }
        // THE TRAY IS THE PROMISE. A file still uploading has no id for the
        // send to name, and one that failed would go out missing; both stop
        // the send here, in words.
        if let problem = ComposeCopy.trayProblem(compose) {
            patch { $0.error = problem }
            return
        }
        if scheduled {
            store.scheduleCompose(.compose, override: override)
        } else {
            store.sendWithUndo(.compose, override: override)
        }
    }
}

// MARK: - labels

/// A button whose LABEL names a key. On a phone the key half is a promise
/// nothing can keep — there is no Esc — so the verb stands alone rather than
/// teaching a shortcut that does not exist.
enum ComposeLabels {
    #if os(macOS)
        static let dismiss = "esc dismiss"
    #else
        // "dismiss", never "discard": closing a composer FLUSHES its draft, so
        // the reply is kept and restored next time, not thrown away.
        static let dismiss = "dismiss"
    #endif
}

// MARK: - shared chrome

/// THE outbound-guard verdict, rendered identically wherever a reply started.
/// The one screen whose job is talking a reader out of a mistake must not read
/// differently in the pane composer and in the reader's inline one.
struct GuardVerdictBox: View {
    /// The redacted kinds the guard matched. Never rendered as markup — they are
    /// server strings.
    let kinds: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s4) {
            HStack(spacing: Space.s4) {
                Text("outbound guard blocked · matched (redacted):")
                    .font(Typo.micro)
                Text(kinds.joined(separator: ", "))
                    .font(Typo.mono(11, weight: .semibold))
            }
            .foregroundStyle(Palette.danger)
            Text("review the recipients and body, then override to send anyway.")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
        }
        .padding(Space.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Palette.dangerSoft)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(Palette.danger.opacity(0.4), lineWidth: 1))
    }
}

/// One `LABEL  value` row of a review summary. The value is mono because it is a
/// header being checked character by character — a recipient, a subject.
struct ComposeSummaryRow: View {
    let label: String
    let value: String

    init(_ label: String, _ value: String) {
        self.label = label
        self.value = value
    }

    var body: some View {
        HStack(alignment: .top, spacing: Space.s10) {
            Text(label)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
                .textCase(.uppercase)
                .frame(width: Typo.measure(60), alignment: .leading)
            Text(value)
                .font(Typo.mono(12))
                .foregroundStyle(Palette.ink)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
