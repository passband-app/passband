// Client-side 2FA code extraction, auth-mail copy, and the "needs a decision"
// ledger (server-backed, with a device-local fallback for older daemons).
//
// "Present, don't read": a login code's body is auto-revealed (audited
// server-side) and the code pulled out here, so the human never opens the mail.

import Foundation
import SwiftUI

// MARK: - code extraction

enum AuthCode {
    /// Kinds that warrant the code modal; the rest (resets/alerts/links) get the
    /// ring only. An unknown wire kind is never one of these.
    static let codeKinds: Set<SealedKind> = [.otp, .verification]

    static func isCodeKind(_ kind: SealedKind?) -> Bool {
        guard let kind else { return false }
        return codeKinds.contains(kind)
    }

    /// Words that tend to sit right next to the actual code.
    nonisolated(unsafe) private static let codeWordPattern =
        /(?i)\b(?:one[-\s]?time|verification|verify|passcode|pass[-\s]?code|security\s+code|access\s+code|auth(?:entication)?\s+code|login\s+code|sign[-\s]?in\s+code|confirmation\s+code|OTP|PIN|code)\b/

    /// A standalone 4-8 digit run, allowing one space/hyphen split ("123 456").
    /// Swift Regex has no lookbehind, so the left boundary is an explicit
    /// alternation and the run is capture group 1 — hence every read uses `m.1`.
    nonisolated(unsafe) private static let digitRunPattern =
        /(?:^|[^\w-])(\d{4,8}|\d{3}[\s-]\d{3}|\d{2}[\s-]\d{2}[\s-]\d{2})(?![\w-])/

    /// Normalize a matched run to bare digits, or nil if it isn't 4-8 digits.
    private static func cleanRun(_ raw: String) -> String? {
        let digits = raw.filter(\.isNumber)
        return (4...8).contains(digits.count) ? digits : nil
    }

    /// Char distance from `index` to the nearest code-word match, or .infinity.
    private static func nearestCodeWord(_ text: String, _ index: Int) -> Double {
        var best = Double.infinity
        for m in text.matches(of: codeWordPattern) {
            let wi = text.distance(from: text.startIndex, to: m.range.lowerBound)
            let wLen = text.distance(from: m.range.lowerBound, to: m.range.upperBound)
            let d = wi <= index ? index - (wi + wLen) : wi - index
            best = min(best, Double(max(0, d)))
        }
        return best
    }

    /// The most likely login code in a revealed body: a 4-8 digit run within ±80
    /// chars of a code word, closest first (proximity beats length), else the
    /// longest standalone run. nil when nothing plausible is found.
    static func extract(_ body: String?) -> String? {
        guard let body, !body.isEmpty else { return nil }
        // Codes live near the top; bound the work.
        let text = String(body.prefix(4000))

        struct Run {
            var code: String
            var index: Int
        }
        var runs: [Run] = []
        for m in text.matches(of: digitRunPattern) {
            guard let code = cleanRun(String(m.1)) else { continue }
            // Index the DIGITS, not the boundary char the pattern consumed.
            runs.append(
                Run(
                    code: code,
                    index: text.distance(from: text.startIndex, to: m.1.startIndex)))
        }
        guard !runs.isEmpty else { return nil }

        let near =
            runs
            .map { (run: $0, dist: nearestCodeWord(text, $0.index)) }
            .filter { $0.dist <= 80 }
        if !near.isEmpty {
            let best = near.sorted { a, b in
                a.dist != b.dist ? a.dist < b.dist : a.run.code.count > b.run.code.count
            }
            return best[0].run.code
        }

        let byLength = runs.sorted { a, b in
            a.code.count != b.code.count ? a.code.count > b.code.count : a.index < b.index
        }
        return byLength[0].code
    }
}

// MARK: - auth copy

/// The DRAWN half of `AuthCopy`. The words themselves live in
/// Lib/AuthLabels.swift, which is Foundation-only so the iOS notification
/// service extension can compile it; this file imports SwiftUI for the ledger
/// below and so can never be on that extension's source list.
extension AuthCopy {
    static func symbol(_ kind: SealedKind?) -> String {
        switch kind {
        case .otp: "key.fill"
        case .passwordReset: "lock.rotation"
        case .magicLink: "envelope.badge.shield.half.filled"
        case .loginAlert: "exclamationmark.shield.fill"
        case .securityAlert: "exclamationmark.shield.fill"
        case .verification: "checkmark.seal.fill"
        case .accountChange: "lock.shield.fill"
        case .appGrant: "app.connected.to.app.below.fill"
        case .signinReceipt: "person.crop.circle.badge.checkmark"
        case .unknown, nil: "key.fill"
        }
    }
}

// MARK: - decisions ledger

/// The owner's verdicts on auth mail, and THE `isOpen` every surface counts by.
///
/// SERVER-BACKED where the daemon can be (docs/AUTH-MAIL-HANDLING.md,
/// "Verdicts"): a verdict is read off the `/client/sealed` row and written with
/// `PUT /client/sealed/{id}/verdict`, optimistically, and rolled back if the
/// write fails. A daemon without that route (older, or rolled back) keeps the
/// old behaviour: the verdict lives in this device's UserDefaults, per ACCOUNT
/// (the map is keyed by message id, which is one daemon's SQLite int). The
/// first sealed list from a daemon that keeps verdicts uploads what the local
/// ledger holds for still-open listed messages, then empties the ledger. The
/// rules themselves are pure and tested (Lib/AuthPolicy.swift).
@MainActor
@Observable
final class AuthDecisions {
    static let shared = AuthDecisions()

    /// THE definition of an open decision, shared by the Auth page's rail, the
    /// badge on the Auth rail button and the phone, so none can disagree.
    func isOpen(_ m: SealedMeta, now: Date = Date()) -> Bool {
        AuthPolicy.isOpen(m, verdict: decision(m), now: now)
    }

    /// Base name of the stored map; the live key is this scoped to the account.
    /// nil when there is no live account: nothing to read, nowhere to write.
    private static let keyBase = "passband.auth-decisions"
    private static var key: String? {
        guard let id = AccountManager.shared.activeId else { return nil }
        return AccountIndex.scopedKey(keyBase, id)
    }
    /// Cap the stored map so it cannot grow without bound.
    private static let cap = 300

    /// The device-local ledger: the whole store on an old daemon, and on a new
    /// one only what is still waiting to be uploaded.
    private var local: [Int: AuthVerdict] = [:]
    /// Writes no sealed list has confirmed yet, shown in place of the row's
    /// own verdict so a card does not flicker back between the PUT and the
    /// list that carries it (`AuthPolicy.settle`).
    private var pending: [Int: AuthPolicy.PendingWrite] = [:]
    /// The last sealed list, for the "still listed?" test a failed PUT needs.
    @ObservationIgnored private var lastRows: [SealedMeta] = []
    /// Numbers every sealed-list fetch as it STARTS (`beginListFetch`).
    @ObservationIgnored private var fetchSeq = 0
    /// The newest fetch number whose list was applied.
    @ObservationIgnored private var appliedFetch = 0
    /// Ids the migration already tried this session, so a refusal is not
    /// retried on every poll.
    @ObservationIgnored private var migrationTried: Set<Int> = []
    @ObservationIgnored private var migrating = false

    private init() { reload() }

    /// Re-read the ledger for whatever account is live NOW. Called by the
    /// account switch, AFTER the new id is committed: this key is derived from
    /// it, so reloading any earlier would re-read the account that just went
    /// away. In-flight bookkeeping belongs to the old daemon and is dropped.
    func reload() {
        let raw =
            Self.key.flatMap { UserDefaults.standard.dictionary(forKey: $0) as? [String: String] }
            ?? [:]
        var next: [Int: AuthVerdict] = [:]
        for (k, v) in raw {
            if let id = Int(k), let verdict = AuthVerdict(stored: v) { next[id] = verdict }
        }
        local = next
        pending = [:]
        lastRows = []
        daemonKeepsVerdicts = false
        migrationTried = []
        migrating = false
    }

    /// Whether the daemon keeps verdicts, read off the last SEALED LIST only
    /// (a row there carries `critical`). Never off a meta built from a feed
    /// row or a notification, which says nothing about the daemon.
    private(set) var daemonKeepsVerdicts = false

    /// The verdict for a message, or nil while it is still open.
    func decision(_ m: SealedMeta) -> AuthVerdict? {
        if let p = pending[m.id] { return p.verdict }
        if daemonKeepsVerdicts {
            // The listed row is the truth; a meta built elsewhere may lag it.
            let row = lastRows.first { $0.id == m.id } ?? m
            if let v = row.serverVerdict { return v }
        }
        // On a new daemon this is a verdict still waiting to be uploaded; on an
        // old one it is the only copy there is.
        return local[m.id]
    }

    /// Record a verdict (nil clears it: Undo). Asymmetric by design: `mine`
    /// just resolves the card, while `notMine` starts an investigation, and
    /// the caller opens the message.
    func set(_ m: SealedMeta, _ verdict: AuthVerdict?) {
        setAll([m], verdict)
    }

    /// The same for several at once ("All me" on a burst): every PUT, then ONE
    /// sealed refresh, rather than a list fetch per member.
    func setAll(_ ms: [SealedMeta], _ verdict: AuthVerdict?) {
        guard Self.key != nil, !ms.isEmpty else { return }
        for m in ms { pending[m.id] = .init(verdict: verdict, acceptedAfterFetch: nil) }
        enqueue(ms, verdict)
    }

    /// Verdict writes run ONE AT A TIME, in tap order: "That was me" then a
    /// quick Undo must reach the server in that order, or the server could end
    /// on the answer the owner took back. Rare enough that one queue for all
    /// messages costs nothing.
    @ObservationIgnored private var writeChain: Task<Void, Never>?

    private func enqueue(_ ms: [SealedMeta], _ verdict: AuthVerdict?) {
        let epoch = AppStore.shared.epoch
        let previous = writeChain
        writeChain = Task { @MainActor in
            await previous?.value
            var accepted = false
            for m in ms {
                // BEFORE each send as well as after it: an account switch
                // between the tap and this task would hand the old daemon's
                // message id to the new one, a verdict on an unrelated message.
                guard AppStore.shared.isCurrent(epoch) else { return }
                if await write(m, verdict, epoch: epoch) { accepted = true }
            }
            guard accepted, AppStore.shared.isCurrent(epoch) else { return }
            await SitrepPoller.shared.refreshSealed()
        }
    }

    /// One PUT and its outcome. True when the server accepted it.
    private func write(_ m: SealedMeta, _ verdict: AuthVerdict?, epoch: Int) async -> Bool {
        do {
            try await APIClient.shared.putSealedVerdict(m.id, verdict: verdict?.rawValue)
            guard AppStore.shared.isCurrent(epoch) else { return false }
            // Only a list fetched from here on can confirm it.
            // (Keeping `resent`, so a disagreeing list resends only once.)
            if pending[m.id]?.verdict == verdict { pending[m.id]?.acceptedAfterFetch = fetchSeq }
            // Any local copy is superseded by the server's.
            writeLocal { $0.removeValue(forKey: m.id) }
            return true
        } catch let error as APIError
            where AuthPolicy.fallsBackToLedger(
                status: error.status,
                stillListed: !daemonKeepsVerdicts || lastRows.contains { $0.id == m.id })
        {
            // No verdict route (an older daemon, or one rolled back under
            // us): this device keeps it, quietly.
            guard AppStore.shared.isCurrent(epoch) else { return false }
            writeLocal { map in
                if let verdict { map[m.id] = verdict } else { map.removeValue(forKey: m.id) }
            }
            pending.removeValue(forKey: m.id)
            return false
        } catch {
            guard AppStore.shared.isCurrent(epoch) else { return false }
            // Only roll back the intent this write carried: a later tap
            // (an Undo queued behind it) owns `pending` now.
            if pending[m.id]?.verdict == verdict { pending.removeValue(forKey: m.id) }
            AppStore.shared.pushToast(errText(error, "could not save that answer"), .error)
            return false
        }
    }

    /// Number a sealed-list fetch as it starts. The poller calls this before
    /// the request and hands the number back with the rows (`listed`).
    func beginListFetch() -> Int {
        fetchSeq += 1
        return fetchSeq
    }

    /// Whether a list from fetch number `fetch` is newer than the last one
    /// applied. The poller drops a stale one rather than painting it.
    func isFresh(_ fetch: Int) -> Bool { fetch > appliedFetch }

    /// Every sealed list the poller takes in, with the number its fetch got:
    /// settles confirmed writes and, on a daemon that keeps verdicts, moves the
    /// local ledger up to it.
    func listed(_ rows: [SealedMeta], fetch: Int) {
        appliedFetch = max(appliedFetch, fetch)
        lastRows = rows
        // An empty list says nothing either way; keep what the last one said.
        if !rows.isEmpty {
            let keeps = rows.contains(where: \.speaksVerdicts)
            if keeps != daemonKeepsVerdicts { daemonKeepsVerdicts = keeps }
        }
        if !pending.isEmpty {
            let settled = AuthPolicy.settle(pending, rows: rows, fetch: fetch)
            if settled.pending != pending { pending = settled.pending }
            // A later list disagreed with an accepted write: send the latest
            // intent once more, through the same queue.
            for id in settled.resend {
                guard let write = pending[id], let row = rows.first(where: { $0.id == id }) else { continue }
                enqueue([row], write.verdict)
            }
        }
        guard !migrating,
            let plan = AuthPolicy.migration(
                local: local.filter { !migrationTried.contains($0.key) }, rows: rows)
        else { return }
        writeLocal { map in for id in plan.drop { map.removeValue(forKey: id) } }
        guard !plan.upload.isEmpty else { return }
        migrating = true
        let epoch = AppStore.shared.epoch
        Task { @MainActor in
            defer { if AppStore.shared.isCurrent(epoch) { migrating = false } }
            for item in plan.upload {
                guard AppStore.shared.isCurrent(epoch) else { return }
                migrationTried.insert(item.id)
                do {
                    try await APIClient.shared.putSealedVerdict(item.id, verdict: item.verdict.rawValue)
                } catch {
                    // Kept locally and still shown; tried again next session.
                    continue
                }
                guard AppStore.shared.isCurrent(epoch) else { return }
                // Shown from `pending` until a later list carries it.
                pending[item.id] = .init(verdict: item.verdict, acceptedAfterFetch: fetchSeq)
                writeLocal { $0.removeValue(forKey: item.id) }
            }
            await SitrepPoller.shared.refreshSealed()
        }
    }

    /// Change the local ledger and persist it (or remove the key once empty).
    private func writeLocal(_ change: (inout [Int: AuthVerdict]) -> Void) {
        guard let key = Self.key else { return }
        var next = local
        change(&next)
        guard next != local else { return }
        // Keys are message ids, so numeric order is arrival order: dropping the
        // lowest keys evicts the oldest decisions first.
        if next.count > Self.cap {
            for k in next.keys.sorted().prefix(next.count - Self.cap) { next.removeValue(forKey: k) }
        }
        local = next
        if next.isEmpty {
            UserDefaults.standard.removeObject(forKey: key)
        } else {
            UserDefaults.standard.set(
                Dictionary(uniqueKeysWithValues: next.map { (String($0.key), $0.value.stored) }),
                forKey: key)
        }
    }
}
