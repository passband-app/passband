// Automatic unsubscribes the daemon is working on, for the tray in the sitrep's
// bottom-right corner (`UnsubscribeTray`).
//
// THE LEDGER IS THE TRUTH. Every job is a row on `GET /client/unsubscribes`,
// so a job that needs the user survives a relaunch, and a job started from the
// assistant shows up exactly like one started with `u`. This model only adds
// what the ledger cannot know: which jobs THIS session started (only those get
// the brief "unsubscribed" confirmation, so a launch does not replay last
// week's), and what the user has already waved away here.
//
// POLLING IS ONLY WHILE SOMETHING RUNS. At rest the rows arrive with the
// violation scan's existing fetch (`absorb`); a running job adds a 1s poll that
// stops by itself when nothing is running.

import Foundation
import Observation

@MainActor
@Observable
final class UnsubscribeJobs {
    static let shared = UnsubscribeJobs()

    enum Phase: Equatable {
        case running
        case done
        case needsYou(reason: String?, url: String?)
    }

    struct Item: Identifiable, Equatable {
        let sender: String
        let phase: Phase
        var id: String { sender }
    }

    /// The latest ledger, newest request first.
    private(set) var rows: [UnsubscribeRecord] = []
    /// Senders whose job finished DONE this session, until the confirmation
    /// fades. Keyed by sender, valued by when it was seen done.
    private(set) var celebrating: [String: Date] = [:]
    /// Waved away (finished on their site, or dismissed) this session. The
    /// server agrees soon after; this keeps the row gone in between.
    private(set) var hidden: Set<String> = []

    /// Senders this session asked to unsubscribe from.
    @ObservationIgnored private var asked: Set<String> = []
    /// Jobs started here that the ledger may not show yet, with when. A poll
    /// that left before the daemon wrote the row would otherwise erase the
    /// spinner it was meant to update, and stop polling with the job running.
    @ObservationIgnored private var pending: [String: Date] = [:]
    @ObservationIgnored private var poll: Task<Void, Never>?
    /// Bumped by `reset`: an answer that left under an older generation
    /// belongs to another account and is dropped. Every caller that awaits the
    /// daemon before handing something here captures it FIRST.
    @ObservationIgnored private(set) var generation = 0

    /// How long "Unsubscribed from X" stays up. Long enough to read, short
    /// enough that it is never in the way: success should be quiet.
    static let celebrateFor: TimeInterval = 2.6
    /// A `needs_you` older than this is not news any more; the thread header
    /// still shows the request.
    static let staleAfter: TimeInterval = 7 * 86_400

    private init() {}

    /// What the tray shows, oldest first so new arrivals land at the bottom,
    /// nearest the corner.
    var items: [Item] {
        let now = Date()
        var out: [Item] = []
        for row in rows.reversed() where !hidden.contains(row.sender) {
            switch row.status {
            case .running:
                out.append(Item(sender: row.sender, phase: .running))
            case .done where celebrating[row.sender] != nil:
                out.append(Item(sender: row.sender, phase: .done))
            case .needsYou where row.resolution == nil:
                let at = Fmt.date(row.status_at ?? row.requested_at) ?? now
                if now.timeIntervalSince(at) < Self.staleAfter {
                    out.append(Item(
                        sender: row.sender,
                        phase: .needsYou(reason: row.status_reason, url: row.status_url)))
                }
            default:
                break
            }
        }
        return out
    }

    /// The account changed: nothing here is about the new one.
    func reset() {
        generation &+= 1
        poll?.cancel()
        poll = nil
        rows = []
        celebrating = [:]
        hidden = []
        asked = []
        pending = [:]
    }

    /// The daemon took a job. Show it at once, before the first poll lands.
    /// `generation` is the one captured before the request went out.
    func started(sender: String, generation gen: Int) {
        guard gen == generation else { return }
        let sender = sender.lowercased()
        asked.insert(sender)
        hidden.remove(sender)
        celebrating[sender] = nil
        pending[sender] = Date()
        if let i = rows.firstIndex(where: { $0.sender == sender }) {
            rows[i].status = .running
        } else {
            rows.insert(
                UnsubscribeRecord(
                    sender: sender, requested_at: ISO8601DateFormatter().string(from: Date()), method: .auto,
                    violation_count: 0, last_violation_at: nil, resolution: nil,
                    status: .running),
                at: 0)
        }
        ensurePolling()
    }

    /// Take a fresh ledger, from the violation scan or this model's own poll.
    /// `generation` is the one captured before the fetch went out.
    func absorb(_ fresh: [UnsubscribeRecord], generation gen: Int) {
        guard gen == generation else { return }
        var fresh = fresh
        // A job started here that this answer predates stays a spinner: keep
        // the local row until the ledger shows the job, which is a RUNNING row
        // (this press, or one it joined) or a row stamped around the press
        // (generous, for a daemon clock behind the Mac's). Past a minute the
        // daemon has surely written it.
        let now = Date()
        for (sender, at) in pending {
            let row = fresh.first { $0.sender == sender }
            let stamped = row.flatMap { Fmt.date($0.status_at ?? $0.requested_at) }
                .map { $0.addingTimeInterval(30) >= at } ?? false
            let seen = row?.status == .running || stamped
            if seen || now.timeIntervalSince(at) > 60 {
                pending[sender] = nil
            } else if let local = rows.first(where: { $0.sender == sender }) {
                fresh.removeAll { $0.sender == sender }
                fresh.insert(local, at: 0)
            }
        }
        let before = Dictionary(rows.map { ($0.sender, $0.status) }, uniquingKeysWith: { a, _ in a })
        for row in fresh where row.status == .done && asked.contains(row.sender) {
            // Celebrate the transition, once: a row that was already done when
            // this session first saw it is history, not news.
            guard before[row.sender] == .running, celebrating[row.sender] == nil else { continue }
            let at = Date()
            celebrating[row.sender] = at
            let sender = row.sender
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(Self.celebrateFor))
                // Only THIS celebration: a re-press since then owns the slot.
                if self?.celebrating[sender] == at { self?.celebrating[sender] = nil }
            }
        }
        rows = fresh
        if fresh.contains(where: { $0.status == .running }) || !pending.isEmpty { ensurePolling() }
    }

    /// "Finish on their site": open the original link, and tell the daemon the
    /// request is now the user's (which starts the violation clock).
    func finish(_ item: Item) {
        guard case .needsYou(_, let url) = item.phase else { return }
        Opener.open(url)
        hidden.insert(item.sender)
        Task { try? await APIClient.shared.markUnsubOpened(sender: item.sender) }
    }

    /// Wave a `needs_you` away without finishing it. Recorded as dismissed so it
    /// does not come back on the next launch; the request itself never went out,
    /// so there is nothing for the violation detector to hold them to.
    func dismiss(_ item: Item) {
        hidden.insert(item.sender)
        celebrating[item.sender] = nil
        guard case .needsYou = item.phase else { return }
        Task { try? await APIClient.shared.setUnsubResolution(sender: item.sender, resolution: .dismissed) }
    }

    /// These senders' outcomes have already been shown somewhere else (the
    /// purge's end page), so the tray's "Unsubscribed from" for them is old
    /// news: drop it rather than replay it as that page closes.
    func settleCelebrations(for senders: [String]) {
        for sender in senders where celebrating[sender] != nil { celebrating[sender] = nil }
    }

    private func ensurePolling() {
        guard poll == nil else { return }
        let gen = generation
        poll = Task { [weak self] in
            // A job is a few requests to a stranger's site: most settle in a
            // second or two, the slowest in under a minute (the daemon's budget).
            try? await Task.sleep(for: .milliseconds(700))
            while !Task.isCancelled {
                guard let self else { return }
                let fresh = try? await APIClient.shared.getUnsubscribes()
                // Another account's ledger, if the account changed meanwhile.
                guard gen == self.generation, !Task.isCancelled else { return }
                if let fresh { self.absorb(fresh, generation: gen) }
                // Expire pending starts on the clock too, so a daemon that
                // stopped answering does not keep this loop alive forever.
                self.pending = self.pending.filter { Date().timeIntervalSince($0.value) <= 60 }
                if !self.rows.contains(where: { $0.status == .running }) && self.pending.isEmpty { break }
                try? await Task.sleep(for: .seconds(1))
            }
            if let self, gen == self.generation { self.poll = nil }
        }
    }
}
