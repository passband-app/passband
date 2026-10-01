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
    @ObservationIgnored private var poll: Task<Void, Never>?

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

    /// The daemon took a job. Show it at once, before the first poll lands.
    func started(sender: String) {
        let sender = sender.lowercased()
        asked.insert(sender)
        hidden.remove(sender)
        celebrating[sender] = nil
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
    func absorb(_ fresh: [UnsubscribeRecord]) {
        let before = Dictionary(rows.map { ($0.sender, $0.status) }, uniquingKeysWith: { a, _ in a })
        for row in fresh where row.status == .done && asked.contains(row.sender) {
            // Celebrate the transition, once: a row that was already done when
            // this session first saw it is history, not news.
            guard before[row.sender] == .running, celebrating[row.sender] == nil else { continue }
            celebrating[row.sender] = Date()
            let sender = row.sender
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(Self.celebrateFor))
                self?.celebrating[sender] = nil
            }
        }
        rows = fresh
        if fresh.contains(where: { $0.status == .running }) { ensurePolling() }
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

    private func ensurePolling() {
        guard poll == nil else { return }
        poll = Task { [weak self] in
            // A job is a few requests to a stranger's site: most settle in a
            // second or two, the slowest in under a minute (the daemon's budget).
            try? await Task.sleep(for: .milliseconds(700))
            while !Task.isCancelled {
                guard let self else { return }
                if let fresh = try? await APIClient.shared.getUnsubscribes() {
                    self.absorb(fresh)
                }
                if !self.rows.contains(where: { $0.status == .running }) { break }
                try? await Task.sleep(for: .seconds(1))
            }
            self?.poll = nil
        }
    }
}
