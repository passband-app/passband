// THE QUICK CLEAR SPRINT: the queue a quick clear banner promised, opened by
// its tap (Notifier), held by the store, drawn by Views/QuickClearView.swift.
//
// It owns almost nothing. Every verb is the app's own: done is `Actions.done`
// with its undo chip, later is the remind palette, open is the reader, the
// sweep is `AppStore.runSmartClear` with its receipt and undo. What is left
// here is which item is on screen, which ones were passed over, and the clock.
//
// WHAT IS NEXT IS DERIVED, NOT STORED (`QuickClearQueue.pending`). An item
// counts as handled when the store's `resolvedIds` holds it (a done, a
// reminder, the reader's `e`) or a cached row says so (another device), so an
// item handled anywhere since the frame is skipped, and an undo brings its
// item straight back to the front.

import Foundation

@MainActor
@Observable
final class QuickClearSprint: Identifiable {
    typealias Item = QuickClearSignal.Item

    /// The optional first step: one press of smart clear.
    enum SmartStep: Equatable {
        /// The frame named nothing for it.
        case none
        /// Waiting for the press.
        case offered
        /// The daemon is working (up to about twenty seconds).
        case working
        /// Done. `cleared` is what the daemon actually cleared, which may not
        /// be what the frame guessed; `receipt` is the receipt it put up, so
        /// an undo can tell whether it is still that one.
        case cleared(Int, receipt: UUID?)
        /// Passed over, or the press cleared nothing.
        case passed
    }

    let id = UUID()
    let signal: QuickClearSignal
    /// The mailbox it was opened in. A sprint from another account is never
    /// drawn (`ActionLayer` compares) and its verbs would land on the wrong
    /// daemon, so every verb checks too.
    let epoch: Int
    let startedAt: Date
    var smartStep: SmartStep
    /// Passed over with → or Space, in the order they were passed.
    private(set) var skipped: [Int] = []
    /// Marked done or parked from this sprint: the summary counts the ones
    /// still handled when it is read, so an undone one stops counting.
    private(set) var actedOn: Set<Int> = []
    /// The time ran out and the owner chose to keep going: the end card does
    /// not come back for the clock.
    private(set) var keptGoing = false
    /// The end card is up. Set by the clock or by the queue running out.
    private(set) var ended = false
    /// When the end card went up, so its "in 4 minutes" holds still.
    private(set) var endedAt: Date?
    private var reported = false
    /// The sweep was the last thing done, so `u` means its undo.
    private var lastWasSweep = false

    init(signal: QuickClearSignal, epoch: Int, now: Date = Date()) {
        self.signal = signal
        self.epoch = epoch
        self.startedAt = now
        self.smartStep = signal.smartClears > 0 ? .offered : .none
    }

    private var store: AppStore { AppStore.shared }
    private var live: Bool { store.isCurrent(epoch) }

    // MARK: - reading the queue

    var total: Int { signal.items.count }
    var budget: Int { signal.budget }

    /// Every id handled since the frame, here or anywhere the app has heard of.
    var handled: Set<Int> {
        let resolved = store.resolvedIds
        return Set(
            signal.items.lazy.map(\.message_id).filter {
                resolved.contains($0) || QuickClearQueue.handledElsewhere(self.store.update(id: $0))
            })
    }

    var pending: [Item] {
        QuickClearQueue.pending(signal.items, handled: handled, skipped: Set(skipped))
    }

    /// The smart clear card is the step on screen.
    var onSmartStep: Bool {
        switch smartStep {
        case .offered, .working: true
        default: false
        }
    }

    var smartCleared: Int {
        if case .cleared(let n, _) = smartStep { return n }
        return 0
    }

    /// Done or parked from this sprint and still that way, plus the sweep.
    var cleared: Int { actedOn.intersection(handled).count + smartCleared }

    /// Not handled, counting the passed-over ones: what the end card says is
    /// still in the queue.
    var left: Int { signal.items.filter { !handled.contains($0.message_id) }.count }

    func elapsed(at now: Date = Date()) -> Int { max(0, Int(now.timeIntervalSince(startedAt))) }

    func timeIsUp(at now: Date = Date()) -> Bool { elapsed(at: now) >= budget }

    // MARK: - the smart clear step

    /// The press: the board's own smart clear, its receipt and its undo.
    func runSmartClear() async {
        guard live, smartStep == .offered else { return }
        smartStep = .working
        let before = store.smartClearReceipt?.id
        await store.runSmartClear()
        guard live else { return }
        if let receipt = store.smartClearReceipt, receipt.id != before, receipt.cleared > 0 {
            smartStep = .cleared(receipt.cleared, receipt: receipt.id)
            lastWasSweep = true
        } else {
            // Nothing cleared, or it failed: the store already said which in
            // a toast. The queue goes on either way.
            smartStep = .passed
        }
    }

    func passSmartClear() {
        guard smartStep == .offered else { return }
        smartStep = .passed
    }

    /// The sweep's undo is still on offer: its receipt is the one up and has
    /// a batch to undo.
    var smartUndoOffered: Bool {
        guard case .cleared(_, let id) = smartStep, let receipt = store.smartClearReceipt else { return false }
        return receipt.id == id && receipt.batchId != nil && !receipt.undoing
    }

    // MARK: - verbs on the item on screen

    /// `e`: done, through the one done verb, so the undo chip and `u` work
    /// exactly as they do on the board.
    func done(_ item: Item, via: Actions.Via) {
        guard live else { return }
        actedOn.insert(item.message_id)
        lastWasSweep = false
        let row = store.update(id: item.message_id) ?? Self.row(for: item)
        Task { await Actions.done(row, via: via, on: .quickClear) }
    }

    /// `h`: the remind palette, as everywhere else. Parking it resolves it,
    /// so the queue moves on once the reminder lands.
    func later(_ item: Item) {
        guard live else { return }
        actedOn.insert(item.message_id)
        lastWasSweep = false
        store.openRemind(
            RemindTarget(
                messageId: item.message_id, sender: item.sender, subject: item.one_line,
                remindAt: store.update(id: item.message_id)?.remind_at))
    }

    /// Enter: the reader. The sprint hides while it is up and is waiting
    /// where it was when the reader closes; an `e` in there moves it on.
    func open(_ item: Item) {
        guard live else { return }
        store.openMessage(item.message_id, from: .quickClear)
    }

    /// → or Space: not now. It stays in the mailbox; the sprint moves on.
    func skip(_ item: Item) {
        guard !skipped.contains(item.message_id) else { return }
        skipped.append(item.message_id)
    }

    /// ←: back to the last one passed over.
    func unskip() {
        guard !skipped.isEmpty else { return }
        skipped.removeLast()
    }

    /// `u`: the sweep's undo while its receipt is up and it was the last
    /// thing done, else the newest undo chip.
    func undo() async {
        guard live else { return }
        if lastWasSweep, smartUndoOffered {
            lastWasSweep = false
            await store.undoSmartClear()
            if store.smartClearReceipt == nil { smartStep = .passed }
            return
        }
        await store.fireUndo()
    }

    // MARK: - the end

    /// The clock ran out. The end card goes up once; after "keep going" the
    /// clock never raises it again.
    func noteTimeUp() {
        guard !keptGoing, !ended else { return }
        ended = true
        endedAt = Date()
    }

    /// The queue ran out.
    func noteEmptied() {
        guard !ended else { return }
        ended = true
        endedAt = Date()
    }

    /// Past the end card: the clock stops raising it, and anything passed
    /// over comes back around.
    func keepGoing() {
        keptGoing = true
        skipped = []
        ended = false
        endedAt = nil
    }

    /// Leaving, by any route. Reported once: `quick_clear_finished`, with
    /// counts and seconds only, never a sender, a subject or an id.
    func finish() {
        guard !reported else { return }
        reported = true
        let elapsed = elapsed()
        let outcome = QuickClearQueue.outcome(
            pending: onSmartStep ? max(1, pending.count) : pending.count, elapsed: elapsed, budget: budget)
        Analytics.capture(
            "quick_clear_finished",
            [
                "outcome": outcome.rawValue,
                "cleared": cleared,
                "smart_cleared": smartCleared,
                "skipped": skipped.count,
                "items": total,
                "left": left,
                "elapsed_secs": elapsed,
                "kept_going": keptGoing,
            ])
    }

    /// A row for an item the board has not cached, for the done verb. It
    /// needs the id, the thread and the sender (the undo chip names them);
    /// the rest is never read on that path.
    static func row(for item: Item) -> AttentionUpdate {
        AttentionUpdate(
            id: item.message_id, thread_id: item.thread_id, tier: .signal, importance: 0,
            sender: item.sender, one_line: item.one_line, reason: "", status: .open)
    }
}
