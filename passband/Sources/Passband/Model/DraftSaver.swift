// AUTOSAVE FOR BOTH COMPOSERS. A reply you were halfway through is not something
// an Esc, a queue step, or a closed window gets to destroy — there is no undo for
// a lost draft — so every keystroke arms a debounced PUT /client/drafts and every
// exit path flushes one last save on the way out.
//
// It is SILENT, without exception: no toasts, no error text, no spinner. A save
// that failed is invisible, because an autosave the reader did not ask for must
// never interrupt what they are writing to report on itself. The only thing a
// failure costs is the restore.
//
// Two slots, saved independently — the pane composer and the reader's inline
// reply are two live drafts at once, keyed differently server-side (the parent
// message id for a reply; the forwarded message's id for a forward, on a
// `drafts_v2` daemon only; the row's own id for new mail on a `drafts_v2`
// daemon, or the one nil-key slot on an older one), so they debounce and flush
// on their own timers.

import Foundation

@MainActor
final class DraftSaver {
    static let shared = DraftSaver()

    /// Which composer a save belongs to. Mirrors the two `ComposeState` slots on
    /// the store; nothing else has a draft.
    enum Slot: Hashable {
        case compose
        case inlineReply
    }

    /// Long enough that a normal typing rhythm writes once per pause rather than
    /// once per word, short enough that anything worth keeping is on disk within a
    /// breath of the reader stopping.
    private static let debounce: Duration = .seconds(1)

    private var store: AppStore { AppStore.shared }

    /// Armed debounce per slot. A fresh keystroke cancels the previous one.
    private var pending: [Slot: Task<Void, Never>] = [:]
    /// Slots edited since they opened. THE gate on flushing: a composer that was
    /// opened and closed without a keystroke — including one that opened onto a
    /// restored draft and left it alone — must not write anything, or every glance
    /// at a draft would bump its `updated_at`.
    private var touched: Set<Slot> = []
    /// The save currently on the wire per slot, so the next one waits for it. Two
    /// PUTs racing on one key can land out of order, and the loser would be the
    /// newer text. This closes the overlap that actually happens — a debounced save
    /// still in flight when the composer closes and flushes — rather than being a
    /// general queue.
    private var inflight: [Slot: Task<Void, Never>] = [:]
    /// Flushes that have been STARTED but have not reached `write` yet. A
    /// flush hands its save to a fresh Task, and that task does not enter
    /// `inflight` until it runs — so `inflight` alone cannot answer "is
    /// anything still going out?", which is the only question `settle` asks.
    private var flushes: [Int: Task<Void, Never>] = [:]
    private var nextFlush = 0
    /// THE ID EACH COMPOSER'S LAST SAVE CAME BACK WITH, keyed by
    /// `ComposeState.id`, and read by every save in preference to the id its
    /// captured state carries.
    ///
    /// The hole it closes: a save captures its state BEFORE it waits out the
    /// slot's previous save (`write`), so save B can read a slot whose
    /// `draftId` save A has not stamped yet. On a `drafts_v2` daemon a nil id
    /// means "create", and B would mint a SECOND row of the same mail. A
    /// flush and a `saveDetached` have it worse: their slot is already empty,
    /// so re-reading it cannot help. This map is written the moment A's
    /// answer lands, whatever the slot holds by then, so B sees it after the
    /// wait.
    ///
    /// Double Optional, like `AppStore.detachedDrafts`: `.some(nil)` is a
    /// blank-delete's answer, and it has to WIN over a stale id, or the next
    /// save edits a deleted row, 404s, and the text is gone.
    private var resolved: [UUID: Int?] = [:]

    private init() {}

    // MARK: - lifecycle

    /// A slot just opened. Any timer left by its previous occupant is stale, and
    /// the fresh draft has nothing to save yet — a restore is not an edit.
    func noteOpened(_ slot: Slot) {
        pending[slot]?.cancel()
        pending[slot] = nil
        touched.remove(slot)
        pruneResolved()
    }

    /// One field of one composer changed. Called from the composers' binding
    /// setters — the single funnel every edit passes through, so there is no way
    /// to type into a draft without arming its save.
    func noteChange(_ slot: Slot) {
        // A FORWARD ON A DAEMON KNOWN TO BE OLD IS NEVER AUTOSAVED — see
        // `save`. Refused at the door rather than at the write, so the slot is
        // never even marked touched: a flush is gated on that mark, and an
        // untouched slot cannot be talked into a save by any exit path. While
        // there is no verdict yet the mark IS set, like new mail's: `save`
        // waits (bounded) for the stats to say which daemon this is before it
        // picks a route, so the keystroke is not lost to the launch window.
        guard !(state(of: slot).map(forwardRefused) ?? false) else { return }
        touched.insert(slot)
        pending[slot]?.cancel()
        pending[slot] = Task { [weak self] in
            try? await Task.sleep(for: Self.debounce)
            guard !Task.isCancelled, let self else { return }
            // Before clearing the marker: a keystroke during the sleep already
            // cancelled us and installed its own task, and nil-ing that one would
            // strand it.
            self.pending[slot] = nil
            // Read the slot NOW rather than capturing values at arm time — the
            // reader kept typing for a second after the change that armed this.
            //
            // Never while a send is on the wire: that request carries the draft's
            // id and deletes the row, so a save landing behind it would resurrect
            // a draft of mail that has already gone out. A failed send leaves the
            // composer open and `touched` set, so nothing is lost by waiting.
            guard let state = self.state(of: slot), !state.sending else { return }
            await self.write(slot, state)
        }
    }

    /// A closing composer's LAST save, fire-and-forget: the caller is about to
    /// clear the slot, so the values are passed in rather than read back.
    ///
    /// Returns whether a write was QUEUED, so the close toast ("kept in
    /// drafts") only promises a row something is actually going to write.
    /// Queued is not landed: the write itself stays silent (see the header).
    @discardableResult
    func flush(_ slot: Slot, _ state: ComposeState?) -> Bool {
        pending[slot]?.cancel()
        pending[slot] = nil
        // Nothing typed => nothing to write. `remove` both tests and clears, so a
        // second exit path firing on the same close cannot double-save.
        guard touched.remove(slot) != nil, let state else { return false }
        // Forwards do not autosave on an old daemon — see `save`. Unreachable
        // while `noteChange` refuses to mark them touched there, and kept
        // because a flush takes the state from the CALLER: the day something
        // hands this a forward it did not type into, the answer must still be
        // no. (`save` asks again at the write, for the stats that land between
        // this flush and its turn on the wire.)
        guard !forwardRefused(state) else { return false }
        let ticket = nextFlush
        nextFlush &+= 1
        flushes[ticket] = Task { [weak self] in
            await self?.write(slot, state)
            self?.flushes[ticket] = nil
        }
        return true
    }

    /// Wait until nothing this saver started is still on the wire.
    ///
    /// The account switch's one hard ordering rule: the draft PUTs go to the
    /// daemon that is configured when they are BUILT, so reconfiguring
    /// APIClient while one is parked would post what the human wrote in
    /// account A into account B's drafts, keyed to a `reply_to_message_id`
    /// that means something else there.
    ///
    /// Call it after flushing every slot: `flush` cancels that slot's debounce
    /// timer, and this drains the two tables the timers feed rather than
    /// racing them.
    func settle() async {
        // Flushes first: each one ends inside `write`, which enters (and
        // clears) `inflight` on its way through.
        for (_, task) in flushes { await task.value }
        for (_, task) in inflight { await task.value }
    }

    /// The composer's mail has been handed to the send hold
    /// (`AppStore.sendWithUndo`), whose send will delete this draft
    /// server-side. Drop the timer and the pending flush with it: the slot
    /// empties next, and a flush there would faithfully re-create the draft for
    /// mail that is on its way out.
    func noteSent(_ slot: Slot) {
        pending[slot]?.cancel()
        pending[slot] = nil
        touched.remove(slot)
        pruneResolved()
    }

    /// The id this composer's draft row has as far as this saver knows: a save
    /// that landed after the slot last changed wins over the slot's copy. For
    /// the store's "is this row the one on screen" test, which must not miss a
    /// composer whose first save came back a moment ago.
    func currentDraftId(of state: ComposeState) -> Int? {
        resolved[state.id] ?? state.draftId
    }

    /// Whether a composer holds anything worth keeping, by the SAME test the
    /// autosave uses to decide between saving and discarding (see `save` for
    /// why each field counts). One helper so the "kept in drafts" toast can
    /// never promise a row the saver just declined to write.
    ///
    /// A FORWARD opens already titled (`Fwd: <original>`), so its subject
    /// counts as blank while it still reads exactly as seeded, or is cleared.
    /// Testing "is the subject empty" would make every forward composed, and
    /// every `f` that was opened, typed into and emptied again would save a
    /// row holding nothing but the title it opened with.
    static func hasContent(_ state: ComposeState) -> Bool {
        let subject = state.subject.trimmed
        let blankSubject =
            subject.isEmpty
            || (state.forwardOfMessageId != nil
                && subject == ComposeCopy.forwardSubject(state.forwardedSubject ?? ""))
        return !(state.recipients == state.seededRecipients && blankSubject
            && Prefs.shared.isBodyUntouched(state.body) && state.stagedAttachmentIds.isEmpty)
    }

    /// Whether this composer is a forward the daemon is KNOWN to be unable to
    /// keep: stats in, and no `drafts_v2`. Nil stats is not a refusal (see
    /// `noteChange`); `save` itself waits for the verdict before writing.
    private func forwardRefused(_ state: ComposeState) -> Bool {
        guard state.forwardOfMessageId != nil, let stats = store.sitrep.stats else { return false }
        return stats.drafts_v2 != true
    }

    /// Drop the `resolved` entries nobody can read any more. Only when NOTHING
    /// is on the wire or queued behind it: every save still pending captured a
    /// state from before its id landed, and pruning under it would reopen the
    /// exact race the map exists to close. Idle, the only states left to save
    /// are the ones in the slots (which carry their adopted ids) and held sends
    /// (which carry theirs through `AppStore.detachedDrafts`), so the live slot
    /// identities are all that must survive.
    private func pruneResolved() {
        guard flushes.isEmpty, inflight.isEmpty, !resolved.isEmpty else { return }
        let live = Set([store.compose?.id, store.inlineReply?.id].compactMap { $0 })
        resolved = resolved.filter { live.contains($0.key) }
    }

    /// Save a composer that no longer sits in any slot: a held send being
    /// PARKED as a draft because it cannot go out (an account switch during its
    /// hold, or nowhere left to reopen it). No touched test, because the
    /// sender pressed send on it, which is as composed as mail gets. Tracked
    /// with the flushes, so `settle` waits for it. A forward is saved only on
    /// a `drafts_v2` daemon; see `save`.
    func saveDetached(_ slot: Slot, _ state: ComposeState) {
        let ticket = nextFlush
        nextFlush &+= 1
        flushes[ticket] = Task { [weak self] in
            await self?.write(slot, state)
            self?.flushes[ticket] = nil
        }
    }

    // MARK: - the write

    private func state(of slot: Slot) -> ComposeState? {
        switch slot {
        case .compose: store.compose
        case .inlineReply: store.inlineReply
        }
    }

    private func write(_ slot: Slot, _ state: ComposeState) async {
        // Wait out the slot's previous save — see `inflight`.
        //
        // A LOOP, not one wait: two writers can queue behind the same save A
        // (a debounce and a flush, say). One `if` resumes them both when A
        // ends, and both would then go out together, racing two creates (two
        // rows of one mail) or two updates landing out of order. Looping
        // re-checks after every wake: the first to resume finds the slot free
        // and stores its task below with no suspension in between (this is
        // MainActor, so nothing else runs there), and the second then finds
        // THAT task and waits again.
        while let running = inflight[slot] { await running.value }
        let task = Task { await save(slot, state) }
        inflight[slot] = task
        await task.value
        // Only our OWN marker: a later save may have replaced it while we were
        // suspended, and nil-ing that one would let the next writer overlap it.
        if inflight[slot] == task { inflight[slot] = nil }
    }

    private func save(_ slot: Slot, _ state: ComposeState) async {
        // A FORWARD IS SAVED ONLY ON A `drafts_v2` DAEMON, keyed by the message
        // it forwards, one row per forwarded message like a reply's. THE
        // reason, stated once here because every other guard in this file
        // points at it: only that daemon's draft row has a field for the
        // forwarded message (`forward_of_message_id`). An older one's
        // `PUT /client/drafts` drops the field it does not know and keys the
        // row by `reply_to_message_id`, which a forward leaves nil, exactly
        // like a plain new message. So a forward saved there would come back
        // as the account's one new-message draft on the next `c`, wearing its
        // "Fwd: …" subject with the original it was forwarding silently gone,
        // and the reader would send a quote of nothing. Losing an unsent
        // forward is the cheaper failure: the original is still in the
        // mailbox, one `f` away.
        //
        // With NO VERDICT yet (stats not in) the save first waits for one;
        // see `awaitVerdict`.
        //
        // ONLY a forward or plain new mail asks the question: a reply's
        // route is the same on every daemon, so it never waits.
        if payloadNeedsVerdict(state), store.sitrep.stats == nil {
            guard await awaitVerdict() else { return }
        }
        if state.forwardOfMessageId != nil, store.sitrep.stats?.drafts_v2 != true { return }
        // A body that is only the seeded signature counts as blank: saving it
        // would mint a draft of nothing, and every later `c` would restore it.
        //
        // ADDRESSING IS COMPOSING, so the recipients count too: a Cc typed and
        // abandoned is something the sender said about who this mail reaches,
        // and a Bcc doubly so — it is the one field nothing else on screen
        // would show them missing later. Compared against the SEED rather than
        // against empty, because a reply opens already holding the set the
        // daemon derived: testing "is `to` empty" would make every abandoned
        // `r` mint a draft of a message nobody wrote.
        //
        // AND SO IS A FILE: a draft holding nothing but an attachment is a
        // draft of that attachment, and discarding it would let the sweep take
        // the file with it.
        //
        // The test itself is `hasContent`, shared with the close toast.
        //
        // THE ID, after the wait in `write`: whatever this composer's previous
        // save came back with beats the one captured in `state`. See
        // `resolved`.
        let draftId = resolved[state.id] ?? state.draftId
        if !Self.hasContent(state) {
            // EMPTIED, not composed: a draft cleared back to nothing is discarded
            // rather than saved as a blank row that would restore as one. With no
            // id yet there was never a row, so there is nothing to discard either.
            guard let id = draftId else { return }
            try? await APIClient.shared.deleteDraft(id)
            adopt(nil, slot: slot, key: state.id)
            return
        }
        let payload = DraftPayload(state)
        do {
            let saved: DraftView
            if let parent = payload.replyToMessageId {
                // A REPLY is keyed by its parent on every daemon, old or new:
                // same route, same row, no capability question to ask.
                saved = try await APIClient.shared.putDraft(
                    replyToMessageId: parent, to: payload.to, cc: payload.cc, bcc: payload.bcc,
                    subject: payload.subject, body: payload.body,
                    attachmentIds: payload.attachmentIds)
            } else if let original = payload.forwardOfMessageId {
                // A FORWARD, keyed by its original (the gate at the top already
                // settled that this daemon has the field). Upserted by key like
                // a reply, so no id to adopt first and no create to race: `f`
                // on the same message twice is the same row.
                saved = try await APIClient.shared.putDraft(
                    replyToMessageId: nil, forwardOfMessageId: original, to: payload.to,
                    cc: payload.cc, bcc: payload.bcc, subject: payload.subject,
                    body: payload.body, attachmentIds: payload.attachmentIds)
            } else if let stats = store.sitrep.stats {
                if stats.drafts_v2 == true {
                    // Many new-message drafts, told apart by id. The first save
                    // mints the row; every later one edits it.
                    saved = try await saveNew(id: draftId, payload)
                } else {
                    // An older daemon's ONE new-message slot, keyed nil. Exactly
                    // the write this client always made there.
                    saved = try await APIClient.shared.putDraft(
                        replyToMessageId: nil, to: payload.to, cc: payload.cc, bcc: payload.bcc,
                        subject: payload.subject, body: payload.body,
                        attachmentIds: payload.attachmentIds)
                }
            } else {
                // STILL NO VERDICT after `awaitVerdict`'s bound. Neither write
                // is safe blind (see there), so this save gives up. On the
                // debounce path `touched` is still set and the close flush
                // tries again; a flush or a parked send that gets here is
                // lost, which is the price of never overwriting another draft.
                return
            }
            adopt(saved.id, slot: slot, key: state.id)
        } catch {
            // Silent, always. A 404 here is a parent sealed since the composer
            // opened — the draft is deliberately unsavable, and the reader is told
            // nothing because there is nothing for them to do about it.
        }
    }

    /// Whether this save's ROUTE depends on which daemon this is: plain new
    /// mail (v2 create/update by id vs. the old nil-key slot) and a forward
    /// (saved on v2, refused on older). A reply's route is the same on every
    /// daemon.
    private func payloadNeedsVerdict(_ state: ComposeState) -> Bool {
        state.replyToMessageId == nil
    }

    /// How long a save waits for `/client/stats` before giving up. Long enough
    /// to cover a slow launch or account switch; short enough that `settle`
    /// (which an account switch awaits) is not held hostage by a daemon that
    /// never answers. Internal because the quit hook has to outlast it (see
    /// `applicationShouldTerminate`).
    static let verdictWait: Duration = .seconds(10)

    /// WAIT FOR THE DAEMON'S VERDICT (`/client/stats`), bounded. True once
    /// the stats are in; false on timeout, or when the account changed
    /// underneath (the store's epoch moved), in which case NOTHING may be
    /// written: the PUT would be built against the NEW account's daemon.
    ///
    /// Why wait rather than skip: a flush clears `touched` before it gets
    /// here, and a parked send has no slot at all, so "the next keystroke
    /// retries" is not true for them. A composer closed in the first second
    /// after launch, before the first stats land, would otherwise lose its
    /// mail. And why not write blind: the old nil-key PUT on a `drafts_v2`
    /// daemon hits its compat rule and OVERWRITES the newest existing draft;
    /// the new routes on an old daemon 404. Polled, because the stats have no
    /// change hook to await, and a tenth of a second is nothing next to a
    /// human closing a composer. Internal so a RESTORE can wait the same
    /// bounded way before reading `draftsV2Available`.
    func awaitVerdict() async -> Bool {
        let epoch = store.epoch
        let deadline = ContinuousClock.now + Self.verdictWait
        while store.sitrep.stats == nil {
            guard ContinuousClock.now < deadline else { return false }
            try? await Task.sleep(for: .milliseconds(100))
            guard store.epoch == epoch else { return false }
            // The switch stops the poller BEFORE it flushes and waits on this
            // saver (`performSwitch` steps 4 and 5), and the old account's
            // stats are never coming once it has: waiting out the bound there
            // would hold the switch for ten seconds and lose the mail anyway.
            guard SitrepPoller.shared.isPolling else { return false }
        }
        return true
    }

    /// EVERYTHING A DRAFT HOLDS, and the only place a save reads a composer's
    /// fields from. A CLOSED LIST: the words (to, cc, bcc, subject, body), the
    /// files that finished staging, and the reply/forward key that says which
    /// row they belong to.
    ///
    /// A draft saves WORDS, NEVER INTENTS. Nothing that says what should HAPPEN
    /// to the mail belongs here: not a scheduled "send at", not the tracker
    /// switch, not a guard override, not a group's send mode. A draft is
    /// reopened and read by a person before anything is done with it, and an
    /// intent restored from a row would act without that read (a future
    /// `ComposeState.sendAt` depends on this list staying closed). Widening it
    /// is a design change, not a field addition.
    struct DraftPayload {
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

    /// A `drafts_v2` new-message save: edit the row by id, or mint one.
    ///
    /// A 404 on the edit means the row is gone out from under a live composer
    /// (discarded from another window, or by a send that did not take this
    /// composer with it). The text on screen is the only copy left, so it is
    /// written as a fresh row rather than dropped: a lost draft has no undo,
    /// and a resurrected one costs a delete.
    private func saveNew(id: Int?, _ payload: DraftPayload) async throws -> DraftView {
        if let id {
            do {
                return try await APIClient.shared.updateDraft(
                    id: id, to: payload.to, cc: payload.cc, bcc: payload.bcc,
                    subject: payload.subject, body: payload.body,
                    attachmentIds: payload.attachmentIds)
            } catch let error as APIError where error.kind == .notFound {
                // Fall through to the create below.
            }
        }
        return try await APIClient.shared.createDraft(
            to: payload.to, cc: payload.cc, bcc: payload.bcc, subject: payload.subject,
            body: payload.body, attachmentIds: payload.attachmentIds)
    }

    /// Write the server's id back into the slot — the thing `send` needs so a
    /// successful send takes the draft with it.
    ///
    /// The slot may have moved on while the request was in flight: closed, or
    /// re-opened on a different message. `key` is the IDENTITY of the composer
    /// the save was taken from (`ComposeState.id`), so a mismatch means the id
    /// belongs to a draft nobody is editing any more and is dropped rather than
    /// stamped onto the current one.
    ///
    /// Identity, not the reply key: `replyToMessageId` is nil for a plain new
    /// message AND for a forward, and identical for two successive composers on
    /// the same message — so a stale flush's id could stamp onto a SUCCESSOR
    /// composer, whose send would then carry it and DELETE a draft holding mail
    /// that never went out. The UUID tells every composer from every other.
    ///
    /// ONE OTHER PLACE THE COMPOSER CAN BE: in the send hold, off every slot.
    /// A save that was on the wire when send was pressed lands after the slot
    /// emptied, and its id is exactly the one the held send must carry, or
    /// the daemon keeps the draft of mail that went out. So a miss is handed
    /// to the store, which matches it by the same identity.
    ///
    /// AND FIRST, unconditionally: `resolved`, whatever the slot holds now, so
    /// a save already queued behind this one reads the answer instead of the
    /// id it captured before it existed.
    private func adopt(_ id: Int?, slot: Slot, key: UUID) {
        resolved[key] = .some(id)
        switch slot {
        case .compose:
            guard var next = store.compose, next.id == key else {
                store.noteDetachedDraft(id, composer: key)
                return
            }
            next.draftId = id
            store.compose = next
        case .inlineReply:
            guard var next = store.inlineReply, next.id == key else {
                store.noteDetachedDraft(id, composer: key)
                return
            }
            next.draftId = id
            store.inlineReply = next
        }
    }
}
