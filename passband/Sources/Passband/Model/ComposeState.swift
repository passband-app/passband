// THE COMPOSER'S VALUE, and the three pure things built from it: what a draft
// saves (`DraftPayload`), what a send or a schedule puts on the wire
// (`SendBody(compose:)`), and the composer an outbox row reopens as
// (`ComposeState.reopened`). Lifted out of AppStore.swift so all four compile
// alone under test.sh: which fields a draft keeps and which a send states are
// the kind of rule that is only ever wrong LATER, in somebody's outbox.
//
// Foundation only. Nothing here touches the store, the network or SwiftUI.

import Foundation

/// Draft state for the send ceremony. ONE type for both composers: the
/// `ComposePane` (new message, the right-hand pane) and the reader's inline
/// reply.
struct ComposeState: Sendable, Equatable {
    /// WHICH COMPOSER THIS IS. Minted once when the state is built and carried
    /// through every copy of it, because this is a value type living in a slot
    /// (`AppStore.compose` / `AppStore.inlineReply`) that anything can replace:
    /// the only way an async continuation can tell "still mine" from "somebody
    /// else's draft is in the slot now" is an identity that copies along.
    ///
    /// The race it exists for: a send is awaited, the sender presses Escape
    /// (which flushes the draft and clears the slot) and opens a different
    /// composer, then the response lands and the continuation closes and
    /// un-marks THAT one — destroying whatever was typed into it. A 60s forward
    /// holds that window open for a minute. Every send continuation therefore
    /// checks this id against the slot before touching it.
    ///
    /// Being `let` with a default keeps it out of the memberwise init, so no
    /// caller can mint two states with one identity — and note this makes
    /// Equatable DISTINGUISH two composers holding identical text, which is
    /// exactly what the slot logic wants: same words, different draft.
    let id = UUID()

    var replyToMessageId: Int?
    /// The message this composer PASSES ON. Mutually exclusive with
    /// `replyToMessageId` — a send names a parent to answer or an original to
    /// forward, never both — and the daemon rejects a body carrying the pair.
    ///
    /// Nothing of the original travels with it: the daemon reads the message
    /// out of its own store, quotes it, re-attaches its files and starts a NEW
    /// thread. So this id is the whole of the forward on the wire, and the
    /// composer's body is only what the sender adds on top of it (which may be
    /// nothing at all).
    var forwardOfMessageId: Int?
    var to: String = ""
    /// The other two recipient headers, comma-joined in the same wire-string
    /// form as `to`. Edited through [`Recipients`], which is what enforces the
    /// rule that matters: an address is in ONE of the three, so moving somebody
    /// to Bcc takes them out of the header everyone can read rather than listing
    /// them twice.
    var cc: String = ""
    var bcc: String = ""
    /// The send group this composition is addressed to, once one has been
    /// picked.
    ///
    /// For a `to`/`bcc` group this is ATTRIBUTION ONLY — the picker has already
    /// expanded the membership into the fields above, and the daemon records
    /// which group that was. For an `individual` group it is the whole audience:
    /// `to` holds only the `#slug` token that keeps the pill alive across a draft
    /// round-trip, and the daemon reads the membership itself.
    var groupId: Int?
    /// The picked group's mode and name, for what the composer has to SAY: the
    /// pill's label, and the review pane's line about how many emails are about
    /// to leave. Display state — the daemon re-reads both from `groupId`, so
    /// nothing here can talk it into a different shape.
    var groupMode: GroupMode?
    var groupName: String?
    var subject: String = ""
    var body: String = ""
    /// WHETHER THESE THREE FIELDS ARE THE ANSWER, or whether the daemon still
    /// derives the audience from the parent.
    ///
    /// A new message and a forward are addressed by hand, so they state it from
    /// the first keystroke. A REPLY does not: it opens knowing only which
    /// message it answers, and the real set — the parent's Reply-To, the room a
    /// reply-all widens to — is derived server-side. Sending `cc: ""` before
    /// that preview lands would be the client asserting an empty copy list it
    /// never had, and on a reply-all that silently narrows the mail to one
    /// person.
    ///
    /// So it flips exactly once per composer, when the derived set is seeded
    /// into the fields (see `InlineReply.loadRecipients`). Until then the wire
    /// carries no `cc` at all and the daemon derives, exactly as it always has
    /// — which is also the safe answer if the preview never lands.
    var recipientsStated = false
    /// WHAT THIS COMPOSER OPENED WITH — the daemon's derivation on a reply,
    /// nothing at all everywhere else. Compared against by the autosave: a reply
    /// whose three fields still say exactly what was derived has not been
    /// addressed by anybody, and minting a draft for it would make every
    /// abandoned `r` come back as restorable mail. A Bcc typed into it, or a
    /// name moved out of the Cc, makes the two differ and the draft real.
    var seededRecipients = Recipients()
    /// The server-side draft this composer is autosaving into, once one exists.
    /// Rides along to `send` so a successful send deletes the draft in the same
    /// transaction — otherwise the next `c` would restore mail already gone.
    var draftId: Int?
    /// The draft this composer was filled from was written by an agent over
    /// Agent MCP, not by the owner. Set by the restore paths and nothing else,
    /// and shown in the composer until it is sent or closed: these are words
    /// and recipients somebody else chose, about to go out under the owner's
    /// name.
    var fromAgent = false
    /// Redacted guard kinds from a 422, set when a held send comes back
    /// blocked and the composer reopens on it. Empty means the guard passed (or
    /// hasn't been asked yet). Non-empty is the ONLY state that offers an
    /// override.
    var guardKinds: [String] = []
    var sending = false
    var error: String?
    /// Ask the daemon to mint a read-tracking pixel for this one send. Seeded
    /// from the account's stored default when the composer opens, then owned by
    /// the composer — the daemon applies no default of its own.
    var includeTracker = false
    /// Answer everyone on the parent rather than only its sender. Meaningless
    /// without `replyToMessageId`, and fixed when the composer opens: which key
    /// opened it is the whole choice, so there is no switch to flip afterwards.
    var replyAll = false
    /// What the forwarded original is called. DISPLAY ONLY: it NEVER reaches the
    /// wire — the daemon builds the quoted original from `forwardOfMessageId`
    /// alone, and a client that shipped its own copy of the subject would be
    /// inviting the two to disagree. It exists so the composer can name what
    /// rides along, and so `ComposePane.reviewSubject` can mirror the daemon's
    /// own `Fwd: …` titling for a sender who cleared the subject field.
    var forwardedSubject: String?
    /// THE FORWARDED ORIGINAL ITSELF, which the composer shows underneath the
    /// note, indented like quoted mail. DISPLAY ONLY, exactly like
    /// `forwardedSubject` and for exactly the same reason: the wire carries
    /// `forwardOfMessageId` and nothing else, and the daemon rebuilds the quote
    /// from its own RAW GMAIL FETCH of that id.
    ///
    /// So this is THE READER'S SANITIZED COPY of the same message, standing in
    /// for a quote that is assembled somewhere else. Without it the composer is
    /// an empty new message that inexplicably sends a fat email, and review —
    /// whose whole job is promising what goes out — promises the covering note
    /// alone.
    ///
    /// THE PREVIEW AND THE WIRE CAN DIFFER, in exactly the ways ingest
    /// sanitization differs from raw: this html has been through ammonia, its
    /// tracking pixels are stripped unless the sender is `sender_known`, and its
    /// `cid:` sources have been rewritten to point at the stored parts. What
    /// actually goes out is the original as Gmail holds it, pixels and all.
    /// Nothing else drifts — same message, same id, same files.
    var forwardedMessage: ClientMessage?
    /// THE TRAY: every file attached to this composition, in the order it was
    /// attached. Uploaded the moment it is dropped (see `ComposeAttach`), so
    /// an entry may still be waiting on its daemon id. Which of these go
    /// INLINE is not recorded here — the body says, by carrying the file's
    /// `cid:` marker or not (see `ComposeMarkers`).
    var attachments: [ComposeAttachment] = []
    /// WHICH PAGE OF THE PANE IS SHOWING: the composer, or the drafts list
    /// that swaps in for its body (`DraftsList`). The pane's own navigation,
    /// nothing about the mail: it never reaches the wire and is never
    /// autosaved (`DraftSaver`'s payload is a closed list of mail fields).
    /// The inline reply has no drafts page and leaves it at `.compose`.
    var page: ComposePage = .compose
    /// WHEN THIS MAIL SHOULD GO, if not now. nil is the ordinary send, through
    /// the five-second hold; a date turns ⌘↵ into a schedule (`POST
    /// /client/scheduled`), which the daemon sends at that time whether or not
    /// this app is running.
    ///
    /// AN INTENT, NEVER AUTOSAVED: `DraftPayload` does not carry it, and
    /// setting or clearing it does not arm `DraftSaver.noteChange`. A draft is
    /// words a person reopens and reads before anything happens to them; a
    /// "send at" restored from a row would act without that read. It lives and
    /// dies with this composer: an account switch clears the slot and it goes
    /// with it.
    var sendAt: Date?

    /// The daemon ids of every file that has finished staging: what a draft
    /// save and a send name.
    var stagedAttachmentIds: [Int] { attachments.compactMap(\.id) }

    /// Whether any file is still on its way up. A send must wait for it — the
    /// tray is the promise of what the mail carries.
    var uploadsPending: Bool { attachments.contains { $0.uploading } }

    /// The three recipient headers as one editable value. The strings stay the
    /// stored form — they are what goes on the wire, verbatim — and this is the
    /// view of them that knows the list grammar.
    var recipients: Recipients {
        get { Recipients(to: to, cc: cc, bcc: bcc) }
        set {
            to = newValue.to
            cc = newValue.cc
            bcc = newValue.bcc
        }
    }

    /// Write the three recipient fields AND record that they are now the
    /// answer. Every recipient edit a composer offers goes through here: the
    /// moment a person touches one of these fields they have stated who the
    /// mail reaches, and the daemon's derivation stops applying. Setting
    /// `recipients` directly is for SEEDING — filling the fields with what the
    /// daemon already derived — which the seeder marks itself.
    mutating func stateRecipients(_ next: Recipients) {
        recipients = next
        recipientsStated = true
    }

    /// The one word both compose events use for what this draft IS —
    /// `compose_opened` and `compose_send` must not disagree about that.
    /// Analytics only; nothing on the wire reads it.
    var analyticsKind: String {
        if forwardOfMessageId != nil { return "forward" }
        return replyToMessageId == nil ? "new" : "reply"
    }
}

/// The compose pane's two pages; see `ComposeState.page`.
enum ComposePage: Sendable, Equatable {
    case compose
    case drafts
}


// MARK: - what a draft saves

/// EVERYTHING A DRAFT HOLDS, and the only place a save reads a composer's
/// fields from. A CLOSED LIST: the words (to, cc, bcc, subject, body), the
/// files that finished staging, and the reply/forward key that says which
/// row they belong to.
///
/// A draft saves WORDS, NEVER INTENTS. Nothing that says what should HAPPEN
/// to the mail belongs here: not a scheduled "send at", not the tracker
/// switch, not a guard override, not a group's send mode. A draft is
/// reopened and read by a person before anything is done with it, and an
/// intent restored from a row would act without that read.
/// `ComposeState.sendAt` is the field this rule exists for: it is NOT here,
/// and test.sh asserts that a composer with a time set saves exactly what the
/// same composer without one saves. Widening this list is a design change,
/// not a field addition.
///
/// Equatable for that assertion and nothing else.
struct DraftPayload: Equatable {
    let replyToMessageId: Int?
    let forwardOfMessageId: Int?
    let to: String
    let cc: String
    let bcc: String
    let subject: String
    let body: String
    let attachmentIds: [Int]

    init(_ state: ComposeState) {
        replyToMessageId = state.replyToMessageId
        forwardOfMessageId = state.forwardOfMessageId
        to = state.to
        cc = state.cc
        bcc = state.bcc
        subject = state.subject
        // WITHOUT the markers of files that are not staged: a composer
        // closed mid-upload flushes here, its file goes to the sweep, and a
        // restored body pointing at a part that does not exist would be a
        // marker nobody can remove from the tray. Once the upload lands,
        // the save it arms writes the marker back.
        body = ComposeMarkers.bodyWithoutUnstaged(state.body, state.attachments)
        // The files that have finished staging: the draft's claim on them
        // is what keeps them past the sweep. One still uploading is
        // claimed by the save its landing arms.
        attachmentIds = state.stagedAttachmentIds
    }
}

// MARK: - what a send states

extension SendBody {
    /// THE ONE MAPPING from a composer to the send route's body, shared by the
    /// send (`ComposeSubmit.fire`, after the hold) and the schedule
    /// (`ComposeSubmit.schedule`, which adds `send_at` beside it). The daemon
    /// freezes a schedule's body and later sends it through the very function
    /// a send calls, so a scheduled mail and a sent one must not be able to
    /// say different things about the same composer: one builder is how.
    ///
    /// `draftId` is passed rather than read off `c`, because a schedule names
    /// the id `DraftSaver.currentDraftId(of:)` knows (a late create's answer),
    /// not the one the slot happened to hold when the composer closed.
    init(compose c: ComposeState, override: Bool, draftId: Int?) {
        let staged = c.stagedAttachmentIds
        self.init(
            // Only a reply names a parent; a forward names an original. Never
            // both: nothing in the client can produce the pair (a composer is
            // opened as one or the other and never converts), and the daemon
            // refuses it.
            reply_to_message_id: c.replyToMessageId,
            // The whole of a forward on the wire: the daemon quotes the
            // original and re-attaches its files from this id.
            forward_of_message_id: c.forwardOfMessageId,
            // Empty = "derive it": on a reply the daemon reads the recipient
            // off the parent message.
            //
            // A FAN-OUT SENDS NO `to` AT ALL. The field holds only the
            // `#slug` token, which is a client-side draft encoding the daemon
            // refuses on sight: the audience travels as `group_id`, and the
            // daemon reads the membership itself.
            to: (c.groupMode == .individual || c.to.isEmpty) ? nil : c.to,
            // THE ONE FIELD WHERE `""` AND ABSENT DIFFER, and the flag is what
            // tells them apart: a composer whose fields state the audience
            // sends its copy list whatever it holds, emptied included, while
            // one still waiting on the daemon's derivation sends nothing and
            // lets the daemon derive. Asserting an empty Cc we never had would
            // narrow a reply-all to one person.
            cc: c.recipientsStated ? c.cc : nil,
            // Nothing derives a blind copy, so empty and absent are the same.
            bcc: c.bcc.isEmpty ? nil : c.bcc,
            // Attribution for an expanded to/bcc group; the whole audience for
            // a fan-out. Never set beside a reply or a forward: those have an
            // audience already, and the daemon refuses the pair.
            group_id: c.replyToMessageId == nil && c.forwardOfMessageId == nil ? c.groupId : nil,
            // nil, never "": the daemon reads Some("") as an explicit blank
            // subject and would send the reply untitled.
            subject: c.subject.isEmpty ? nil : c.subject,
            body: c.body,
            // Both composers write markdown; the daemon renders the HTML half
            // from this same source after the guard has scanned it.
            body_format: "markdown",
            confirm: true, override_guard: override, draft_id: draftId,
            // The composer's own switch, every time: the daemon's stored
            // default is a client preference it never applies itself. Omitted
            // rather than `false`, like `subject`.
            include_tracker: c.includeTracker ? true : nil,
            // Only a reply can widen: `reply_all` names a parent to widen FROM,
            // so a new message carrying it would be asking the daemon to derive
            // a recipient set out of nothing. A flag, never a recipient list.
            reply_all: c.replyToMessageId != nil && c.replyAll ? true : nil,
            // Tray order. Every id must still resolve on the daemon or the send
            // is refused: the tray is the promise.
            attachment_ids: staged.isEmpty ? nil : staged)
    }
}

// MARK: - what an outbox row reopens as

extension ComposeState {
    /// A SCHEDULED MAIL PULLED BACK INTO A COMPOSER: the frozen request a
    /// `DELETE /client/scheduled/{id}` hands back, its files, and the time it
    /// was set for, as the composer they came from.
    ///
    /// The request is the authority for everything on the wire, because it is
    /// what the daemon was going to send. `base` is the composer the schedule
    /// was made from when this app still has it (the toast's undo): it lends
    /// only what the wire never carried, the DISPLAY state (the forwarded
    /// original, the group's name and mode, the derived recipients a reply
    /// was seeded with). The Sent page's pull-back has no base and passes nil.
    ///
    /// A FRESH IDENTITY, always, and no `draftId`: scheduling consumed the
    /// draft, and a reopened composer is a new composition whose next autosave
    /// makes a new row (and re-claims these files, which the cancel released).
    /// Reusing the old identity would hand `DraftSaver` the consumed row's id
    /// out of its `resolved` map.
    ///
    /// A FAN-OUT'S AUDIENCE IS NOT IN THE REQUEST'S `to`. `SendBody(compose:)`
    /// sends none for an individual group (the `#slug` token is a client
    /// encoding the daemon refuses), so the request names it by `group_id`
    /// alone, and a composer rebuilt from `to` would come back addressed to
    /// nobody: no pill, and an Esc that saves a draft with no audience at all.
    /// The token comes back from `base` when this app still has the composer,
    /// else from `group`, the group `group_id` names as the daemon describes it
    /// now (fetched by the caller; nil when it is gone or unreachable, and
    /// then the id still rides on the wire, so a reschedule still reaches it).
    static func reopened(
        request r: ScheduledRequest, attachments: [OutboundAttachment], sendAt: Date?,
        base: ComposeState? = nil, group: SendGroup? = nil
    ) -> ComposeState {
        var state = ComposeState()
        if let base {
            state.forwardedSubject = base.forwardedSubject
            state.forwardedMessage = base.forwardedMessage
            state.groupMode = base.groupMode
            state.groupName = base.groupName
            state.seededRecipients = base.seededRecipients
        }
        state.replyToMessageId = r.reply_to_message_id
        state.forwardOfMessageId = r.forward_of_message_id
        state.to = r.to ?? ""
        state.cc = r.cc ?? ""
        state.bcc = r.bcc ?? ""
        // A `cc` key on the wire, `""` included, is exactly a composer that had
        // stated its audience (see `SendBody(compose:)`). Absent means the
        // daemon was deriving it, and the reopened reply derives it again.
        state.recipientsStated = r.cc != nil
        state.groupId = r.group_id
        if let gid = r.group_id, state.to.isEmpty, let base, base.groupId == gid,
            base.groupMode == .individual
        {
            state.to = base.to
        }
        if let group { state.adoptGroup(group, request: r) }
        state.subject = r.subject ?? ""
        state.body = r.body ?? ""
        state.includeTracker = r.include_tracker ?? false
        state.replyAll = r.reply_all ?? false
        // TRAY ORDER is the request's, which is the order the sender attached
        // them in; the cancel lists the files in upload order, which can
        // differ. A file the request does not name keeps its place after.
        let order = r.attachment_ids ?? []
        let rank = { (a: OutboundAttachment) in order.firstIndex(of: a.id) ?? Int.max }
        state.attachments = attachments.enumerated()
            .sorted { (rank($0.element), $0.offset) < (rank($1.element), $1.offset) }
            .map { ComposeAttachment($0.element) }
        state.sendAt = sendAt
        return state
    }

    /// The fan-out's pill, from the daemon's own answer about the request's
    /// group: fresher than `base`, which only knows the group as it was. A
    /// fan-out with no stated `to` wears the group's token. Also how a
    /// composer that opened BEFORE the lookup came back is patched
    /// (`AppStore.finishPulledBack`), so it is one rule, not two. A group
    /// whose id is not the request's lends nothing.
    mutating func adoptGroup(_ group: SendGroup, request r: ScheduledRequest) {
        guard let gid = r.group_id, group.id == gid else { return }
        if to.isEmpty, group.mode == .individual { to = GroupToken.encode(group) }
        groupMode = group.mode
        groupName = group.name
    }
}
