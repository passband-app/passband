// Keeps the sitrep read model fresh: the three bands + stats + sealed metadata,
// every 10s while the app is frontmost and on window focus, written into the
// store for views to read. Slower while nobody is looking (see `PollCadence`
// at the bottom of this file): a window behind another app every 30s, nothing
// on screen every five minutes, with focus, the window coming back and the
// live event feed as the wakes; one due time computed from all of it
// (`nextPullAt`). A failing daemon widens the interval — see `pullFinished`.
// The sealed leg has one extra trigger of its own (`refreshSealed`), because a
// login code cannot wait out a ten-second tick.
//
// Each band carries its own server-side `band` filter so the buckets match the
// server's definitions exactly. Sealed is metadata-only — never bodies.

import Foundation

@MainActor
final class SitrepPoller {
    static let shared = SitrepPoller()

    /// Shared with the notification extension, which fetches the same standing
    /// band to compute the app icon's badge. One constant because the badge and
    /// the dashboard's headline are supposed to be the same number, and they
    /// only are if both read the same rows — see NeedToday.bandLimit.
    private static let pageLimit = NeedToday.bandLimit

    /// The in-flight pull, shared by the interval poller and every manual
    /// caller. Held as the TASK rather than a flag so an overlapping caller can
    /// JOIN it — a caller that merely skips cannot know whether what is on
    /// screen predates what it asked about.
    private var inFlight: InFlight?
    private var pullCounter = 0

    /// The pull in flight: its task, which pull it is (so its own bookkeeping
    /// can tell whether `stop()` has detached it since), and whether the loop
    /// made or joined it. Only the loop's own failures move the backoff: a
    /// "retry now" or a refresh that fails says nothing the loop's next
    /// attempt would not, and must not push it later.
    private struct InFlight {
        let task: Task<PullOutcome, Never>
        let id: Int
        var isLoops: Bool
    }

    /// What one pull came to: the daemon answered, it failed, or nothing was
    /// asked of it (the door was not connected, or the mailbox moved under
    /// the pull before it read). Only an answer resets the backoff and
    /// answers an ask.
    private enum PullOutcome {
        case answered, failed, skipped
    }
    private let resident = ResidentTask()
    private var presenceObservers: [NSObjectProtocol] = []

    /// Every interval the loop keeps. A value so the suite can run the whole
    /// schedule in seconds; the app never changes it. Set it before `start()`,
    /// which builds the backoff from it.
    var timing = PollTiming()

    // THE SCHEDULE'S INPUTS. The loop's next pull is computed from these
    // alone (`nextPullAt`), and every change to one of them re-arms the
    // loop's sleep (`reschedule`), so no wake has its own path to get wrong.

    /// How often the loop pulls, from what the person can see.
    private(set) var cadence = PollCadence.foreground
    /// When the last pull for this mailbox finished, whoever asked for it.
    private var lastPull: ContinuousClock.Instant?
    /// While the daemon is failing, the loop's last failure: when it came, and
    /// the two jittered draws its retry is made of (`nextPullAt`), kept rather
    /// than the retry itself so the retry follows the cadence when a window
    /// comes or goes.
    private var failure: Failure?
    private struct Failure {
        let at: ContinuousClock.Instant
        /// The backoff's delay for this failure.
        let backoff: TimeInterval
        /// How far past the cadence's interval this client waits, so clients
        /// that failed together do not retry together.
        let stretch: Double
    }
    /// When the loop's own last attempt ended: the floor no input but focus
    /// pulls sooner than, kept apart from `lastPull` so a manual pull that
    /// fails cannot push the loop's retry later.
    private var lastLoopAttempt: ContinuousClock.Instant?
    /// Rebuilt from `timing` on every `start()`; only the loop moves it.
    private lazy var backoff = Backoff(base: timing.backoffBase, cap: timing.backoffCap)
    /// The LATEST moment news asked for a pull (a live event for the mailbox
    /// on screen). Answered, and cleared, by a pull that STARTS at or after
    /// it: a pull already reading the daemon when the news came may have read
    /// before it, so a second event during the pull answering the first one
    /// keeps the ask open.
    private var askedAt: ContinuousClock.Instant?

    /// The loop's current sleep, by identity, so a loop cancelled by `stop()`
    /// tells its nap from the next loop's.
    @MainActor
    private final class Nap {
        var sleeper: Task<Void, Never>?
        /// What the sleeper is sleeping to, so a reschedule that moves nothing
        /// leaves it be.
        var until: ContinuousClock.Instant?
    }
    private var currentNap: Nap?

    /// True while the loop sleeps between pulls.
    var isNapping: Bool { currentNap != nil }

    private var store: AppStore { AppStore.shared }

    private init() {}

    /// Whether the loop is up. False between an account switch's `stop()`
    /// and the next `start()`: nothing can land in `stats` meanwhile,
    /// which is what a writer waiting on that verdict needs to know.
    var isPolling: Bool { resident.isRunning }

    /// The cadence the platform says applies right now.
    private var presentCadence: PollCadence {
        PollCadence(active: Platform.isAppActive, onScreen: Platform.isAppOnScreen)
    }

    /// Start interval + presence polling. Idempotent.
    func start() {
        // The observers ride on the loop's own guard: `ResidentTask` refuses a
        // second loop by itself, but nothing would stop a second `start()` from
        // stacking a second set of observers.
        guard !resident.isRunning else { return }
        cadence = presentCadence
        backoff = Backoff(base: timing.backoffBase, cap: timing.backoffCap)
        resident.start { [weak self] in await self?.run() }
        // `queue: nil`, so the block runs as the notification is posted and
        // only the name crosses to the main actor.
        presenceObservers = Platform.presenceNotifications.map { name in
            NotificationCenter.default.addObserver(forName: name, object: nil, queue: nil) { note in
                let name = note.name
                Task { @MainActor in SitrepPoller.shared.presenceChanged(after: name) }
            }
        }
    }

    func stop() {
        // Cancelling the loop ends its nap too (`napUntilDue`'s cancellation
        // handler); the schedule's inputs are this mailbox's and go with it.
        resident.stop()
        currentNap = nil
        lastPull = nil
        lastLoopAttempt = nil
        failure = nil
        askedAt = nil
        // A new mailbox must never join a request for the mailbox just left.
        // Cancellation alone is insufficient: transports may still finish.
        inFlight?.task.cancel()
        inFlight = nil
        for observer in presenceObservers { NotificationCenter.default.removeObserver(observer) }
        presenceObservers = []
    }

    /// The app came on screen, went off it, or changed whether it is
    /// frontmost (`name` says which). FOCUS PULLS AT ONCE, as it always has,
    /// whatever the schedule says. Any change re-reads the cadence, which
    /// moves the loop's next pull either way: back on screen, sooner (now, if
    /// the new cadence's interval has passed since the last pull); off
    /// screen, later. Nothing for a stopped poller: a notification queued
    /// before `stop()` must not pull for a mailbox being switched away from.
    func presenceChanged(after name: Notification.Name) {
        guard resident.isRunning else { return }
        let presence = Platform.presence(after: name)
        cadence = PollCadence(active: presence.active, onScreen: presence.onScreen)
        if name == Platform.didBecomeActiveNotification {
            // Re-checked after the hop: a `stop()` landing in between must
            // stop this pull too.
            Task { if self.resident.isRunning { await self.pull() } }
        }
        reschedule()
    }

    /// The live event feed brought news for the mailbox on screen: ask for a
    /// pull, so the board, the badge and the outbox catch up with the mail
    /// just announced. When that is, `nextPullAt` decides.
    func noteLiveEvent() {
        guard resident.isRunning else { return }
        askedAt = ContinuousClock.now
        reschedule()
    }

    /// The loop's next pull, from the schedule's inputs alone.
    ///
    /// - On the cadence: the cadence's interval after the last pull, whoever
    ///   made it.
    /// - Asked for by news: at once, but not sooner than the ask floor after
    ///   the last pull: one foreground tick while a window is on screen (so a
    ///   board on a second display catches up as fast as a frontmost one) and
    ///   the behind cadence while nothing is, so a burst of events nobody can
    ///   see is a pull every thirty seconds at most.
    /// - Failing: A FAILING DAEMON IS NEVER ASKED MORE OFTEN THAN A HEALTHY
    ///   ONE WOULD BE. Frontmost, the retry is the backoff it always was (a
    ///   20 s base, so its jittered floor is the foreground tick, doubling to
    ///   a minute). Slower, it is the cadence's interval stretched by this
    ///   client's own draw of up to a quarter, or the backoff where that is
    ///   longer: behind another app every 30 to 60 s, with nothing on screen
    ///   every five minutes or so, and a window coming back brings it in to
    ///   the new cadence's. News does not cut it: the event stream can be up
    ///   while the door sheds load, and the backoff is what lets it.
    /// - Never sooner than one foreground tick after the loop's own last
    ///   attempt: only focus pulls sooner, and it does not come here.
    private var nextPullAt: ContinuousClock.Instant {
        guard let lastPull else { return ContinuousClock.now }
        let interval = timing.interval(cadence)
        var due: ContinuousClock.Instant
        if let failure {
            let paced = cadence == .foreground ? 0 : interval.timeInterval * failure.stretch
            due = failure.at + .seconds(max(failure.backoff, paced))
        } else {
            due = lastPull + interval
            if let askedAt {
                let askFloor = cadence == .hidden ? timing.behind : timing.foreground
                due = min(due, max(askedAt, lastPull + askFloor))
            }
        }
        return max(due, (lastLoopAttempt ?? lastPull) + timing.foreground)
    }

    /// Send the loop round to read `nextPullAt` again, when it has moved.
    private func reschedule() {
        guard let nap = currentNap, nap.until != nextPullAt else { return }
        nap.sleeper?.cancel()
    }

    /// A pull finished: the schedule's inputs move, inside the pull's own
    /// task, so every caller joined to it resumes to the new schedule.
    /// Nothing, for a pull `stop()` has detached: a late answer for the
    /// mailbox just left must not touch the next one's.
    ///
    /// Only the loop's own failures (or a pull it joined) start or advance the
    /// backoff; see `nextPullAt` for what the retry is.
    private func pullFinished(
        _ id: Int, _ outcome: PullOutcome, startedAt: ContinuousClock.Instant
    ) {
        guard let current = inFlight, current.id == id else { return }
        inFlight = nil
        let now = ContinuousClock.now
        // Every outcome is a pull that ended: the schedule runs from here.
        lastPull = now
        switch outcome {
        case .answered:
            backoff.reset()
            failure = nil
            if let asked = askedAt, asked <= startedAt { askedAt = nil }
        case .failed:
            if current.isLoops {
                failure = Failure(at: now, backoff: backoff.next(), stretch: .random(in: 1...1.25))
            }
        case .skipped:
            break
        }
        reschedule()
    }

    /// The loop found the door not connected and asked nothing: counted as
    /// the old loop counted it, nothing to back off from, so the schedule
    /// runs from now on the cadence (which is what notices the reconnect) and
    /// no stale retry is left due in the past for the loop to spin on. An ask
    /// stays open: nothing answered it. A pull that started and then found
    /// the mailbox moved under it is `pullFinished`'s `.skipped`, which leaves
    /// the backoff as it was.
    private func attemptSkipped() {
        lastPull = ContinuousClock.now
        backoff.reset()
        failure = nil
    }

    /// Pull, then sleep until the schedule says the next pull is due.
    private func run() async {
        while !Task.isCancelled {
            _ = await attempt(fromLoop: true)
            if Task.isCancelled { return }
            lastLoopAttempt = ContinuousClock.now
            await napUntilDue()
        }
    }

    /// Sleep to `nextPullAt`, reading it again whenever `reschedule` cuts the
    /// sleeper short.
    private func napUntilDue() async {
        let mine = Nap()
        currentNap = mine
        while !Task.isCancelled, currentNap === mine {
            let due = nextPullAt
            if ContinuousClock.now >= due { break }
            let sleeper = Task { _ = try? await Task.sleep(until: due, clock: .continuous) }
            mine.sleeper = sleeper
            mine.until = due
            await withTaskCancellationHandler {
                await sleeper.value
            } onCancel: {
                sleeper.cancel()
            }
        }
        if currentNap === mine { currentNap = nil }
    }

    /// Fetch the read model once and write it into the store. Joins a pull
    /// already in flight rather than stacking a second; no-ops when the door is
    /// not connected.
    ///
    /// Returns whether the daemon ANSWERED. A door that is not connected
    /// reports true: nothing was asked of the daemon, so there is nothing to
    /// back off from, and the loop keeps the cadence that notices the
    /// reconnect.
    @discardableResult
    func pull() async -> Bool {
        await attempt(fromLoop: false) != .failed
    }

    private func attempt(fromLoop: Bool) async -> PullOutcome {
        guard !Task.isCancelled, store.connStatus == .connected else {
            if fromLoop { attemptSkipped() }
            return .skipped
        }
        if let inFlight {
            if fromLoop { self.inFlight?.isLoops = true }
            return await inFlight.task.value
        }
        // Capture before scheduling: a queued task must not adopt a newer
        // mailbox epoch when it eventually starts.
        let e = store.epoch
        pullCounter &+= 1
        let id = pullCounter
        let startedAt = ContinuousClock.now
        let task = Task { () -> PullOutcome in
            let outcome = await self.performPull(epoch: e)
            self.pullFinished(id, outcome, startedAt: startedAt)
            return outcome
        }
        inFlight = InFlight(task: task, id: id, isLoops: fromLoop)
        return await task.value
    }

    /// The SEALED LEG ONLY, now. Driven by a sealed event off the live feed
    /// (docs/NOTIFY.md §11.6): a login code has landed and the poll that would
    /// have found it is at least nine seconds too slow for mail that expires.
    ///
    /// One request rather than `pull()`'s five, because nothing else about the
    /// sitrep has news — and because this can fire per sealed message, which is
    /// a rate the full pull has no business being run at.
    ///
    /// Updates explicit Auth lookup without triggering notifications or modals.
    ///
    /// Silent on every failure, and it does not touch `refreshError`: this is
    /// an opportunistic extra ask, and the poll loop above is what decides
    /// whether the daemon is reachable.
    func refreshSealed() async {
        guard store.connStatus == .connected else { return }
        // The account this ask is ABOUT. A switch bumps the epoch, and the ids
        // in a sealed list are one daemon's SQLite ints — writing an answer
        // from the mailbox just left into the read model of the one arriving
        // would seed the new account's seen-set from the old account's mail.
        let e = store.epoch
        // Numbered BEFORE the request, so a verdict written meanwhile knows
        // this list may predate it (`AuthDecisions.listed`).
        let fetch = AuthDecisions.shared.beginListFetch()
        guard let sealed = try? await APIClient.shared.listSealed() else { return }
        guard store.isCurrent(e) else { return }
        // A list that started before one already applied is older news: it can
        // still carry a verdict as null after the PUT, and would reopen a card.
        guard AuthDecisions.shared.isFresh(fetch) else { return }
        defer { AuthDecisions.shared.listed(sealed, fetch: fetch) }
        // ASSIGN ONLY ON CHANGE, for the same reason `performPull` does:
        // @Observable notifies on assignment, not on difference, and the
        // watcher downstream of this is an `onChange`.
        if sealed != store.sitrep.sealed { store.sitrep.sealed = sealed }
    }

    /// Re-read every surface a triage correction can move mail between, NOW —
    /// bands are otherwise a poll behind and zones a TTL behind, long enough
    /// that the mail looks like it never moved.
    ///
    /// JOIN THEN PULL: a poll already running may have read the daemon before
    /// the correction committed, so its rows cannot be the ones we settle on.
    func refreshAfterCorrection() async {
        if let inFlight { _ = await inFlight.task.value }
        async let bands: Bool = pull()
        async let zones: Void = store.refreshZones(force: true)
        _ = await (bands, zones)
    }

    /// What the pull came to; the caller decides what a failure costs.
    private func performPull(epoch e: Int) async -> PullOutcome {
        // The account this pull is ABOUT. A switch bumps the store's epoch, and
        // everything below writes the read model the whole app renders — bands
        // keyed by per-daemon message ids most of all. A stale answer is
        // `.skipped`: nothing was asked of the NEW daemon, so there is nothing
        // for the backoff loop to back off from (and that loop is stopped by
        // the switch anyway), and nothing it asked for has been answered.
        guard !Task.isCancelled, store.isCurrent(e), store.connStatus == .connected else { return .skipped }
        do {
            let sealedFetch = AuthDecisions.shared.beginListFetch()
            async let feed = APIClient.shared.getFeed(destination: "fye", limit: Self.pageLimit)
            async let stats = APIClient.shared.getStats()
            async let sealed = APIClient.shared.listSealed()
            let (page, st, sl) = try await (feed, stats, sealed)
            guard !Task.isCancelled, store.isCurrent(e) else { return .skipped }
            // One ordered list. The older bands remain empty until the shared
            // reader state no longer needs their compatibility fields.
            // The sealed leg only lands if no newer sealed list has (see
            // `refreshSealed`); otherwise the board keeps the newer one.
            let sealedFresh = AuthDecisions.shared.isFresh(sealedFetch)
            let next = SitrepData(
                standing: page.items.map(\.row), new: [], open: [],
                sealed: sealedFresh ? sl : store.sitrep.sealed, totalCount: page.total_count)
            // A row naming a message newer than a thread's cached copy is mail
            // that copy predates; it goes before anything can open it.
            ThreadPrefetch.shared.retire(next.standing.lazy.map { ($0.thread_id, $0.id) })
            // ASSIGN ONLY ON CHANGE: @Observable notifies on assignment, not on
            // value difference, so writing an identical read model every 10s
            // re-lays out the whole dashboard for nothing. The stats are
            // assigned apart from the bands for the same reason — they carry
            // the sync clock and change on nearly every tick, and while they
            // rode inside `SitrepData` this compare never once said "same".
            if next != store.sitrep { store.sitrep = next }
            if sealedFresh { AuthDecisions.shared.listed(sl, fetch: sealedFetch) }
            if st != store.stats { store.stats = st }
            // Whose stats these are: the epoch this pull was asked under,
            // which the guard above has just confirmed is the live one. Set
            // even when the body is identical, because identical stats can
            // still belong to a mailbox switched to since they were written.
            if store.statsEpoch != e { store.statsEpoch = e }
            noteCatchUp(st.catch_up)
            // THE BADGE IS REFRESHED EVEN WHEN NOTHING CHANGED, and this is the
            // second write point rather than an accident. `AppStore.sitrep`'s
            // didSet catches every CHANGE; it cannot catch a confirmation, and
            // the line above deliberately skips the assignment when the read
            // model is identical.
            //
            // Which breaks precisely the case that matters most. The badge has a
            // writer that runs while the app does not — the notification
            // extension — so the icon can already say 3 when this process
            // starts. A launch begins with `standing: []`; if the daemon now
            // also says empty (the items were dealt with on the Mac), `next`
            // equals the read model, nothing is assigned, nothing observes, and
            // a stale 3 sits on the icon for the whole session with the app open
            // in front of it. Going to zero is the common case and was the
            // broken one.
            //
            // Counted off `next` rather than the store: identical in either
            // branch, and this is the value we just got from the daemon.
            Badge.set(next.totalCount ?? next.standing.count)
            // FIRST SIGHT OF THE MAILBOX = the first chance to know the human's
            // name well enough to guess. Here rather than at connect time
            // because this is where the address arrives, and unconditional
            // because the pref itself is what fires once — see seedUserName.
            //
            // The Google given name FIRST, when the daemon has one: it is a
            // better guess than the mailbox's local part, and it also replaces
            // that guess on an install which already made it.
            Prefs.shared.seedUserName(fromGivenName: st.account_given_name, email: st.account_email)
            Prefs.shared.seedUserName(fromEmail: st.account_email)
            // The same answer names this account's profile photo.
            AccountPhotos.shared.noteActive(tag: st.account_photo)
            // A REPLY THAT LANDED IN THE THREAD ON SCREEN. The live event feed
            // usually gets there first, and this is the backstop that has to
            // hold anyway: the feed only carries mail triage judged worth a
            // notification, and it is not carrying anything at all while the
            // connection is down. Without one of the two, an answer to the email
            // somebody is sitting on shows up when they leave and come back.
            //
            // Deliberately NOT gated on `next` having changed: a refetch that
            // failed leaves the summary where it was, so the next poll asks
            // again, and the ask stops the moment the viewer adopts the newer
            // id. The store is what makes the ask idempotent and the TELLING
            // once — see AppStore.noteThreadArrival.
            if let open = store.currentThreadSummary, let row = newestArrival(in: next, for: open) {
                store.noteThreadArrival(
                    thread: row.thread_id, message: row.id, sender: row.senderString)
            }
            if store.refreshError != nil {
                store.refreshError = nil
                // Transition only — a healthy poll is not an event.
                Analytics.capture("connection_restored")
            }
            store.lastRefresh = Date()
            // Daily triage-volume counters, all ints: the denominator for the
            // correction rate, and the surfaced-vs-squelched split. Tier counts
            // are flattened onto fixed keys so no server string ever rides.
            Analytics.daily(
                "triage_digest",
                [
                    "total": st.total, "sealed": st.sealed,
                    "band_standing": st.bands.standing, "band_new": st.bands.new,
                    "band_open": st.bands.open,
                    "tier_past_due": st.tier_counts["past_due"] ?? 0,
                    "tier_deadline": st.tier_counts["deadline"] ?? 0,
                    "tier_signal": st.tier_counts["signal"] ?? 0,
                    "tier_noise": st.tier_counts["noise"] ?? 0,
                ])
            // The bands half of the launch image warm's input; refreshZones
            // below supplies the rest.
            ImageWarmer.shared.noteSitrepLanded()

            // Keep a valid selection: if nothing selected, land on the first row.
            if store.selectedId == nil, let first = store.orderedIds.first {
                store.selectedId = first
            }

            // Zones warm from here, not just from SitrepView's mount, or the
            // first visit of a session still pays five round-trips. Its own TTL
            // makes this a date compare on most polls.
            await store.refreshZones()
            // Bands and zones have both been asked of this daemon: a board
            // repainted from a switch's snapshot is the mailbox's again.
            if store.isCurrent(e) { store.noteBoardConfirmed() }
            // The outbox, so a scheduled mail that FAILS toasts wherever the
            // person is, not only on the Sent page. Its own TTL and an
            // empty-outbox skip make this free on most ticks.
            await store.pollScheduled()
            return .answered
        } catch {
            guard !Task.isCancelled, store.isCurrent(e) else { return .skipped }
            // Transition only, before the error lands — count outages, not
            // every failing poll of one.
            if store.refreshError == nil { Analytics.capture("connection_lost") }
            // Keep the kind so the UI can say "daemon unreachable" vs "token
            // rejected" rather than one undifferentiated failure.
            if let api = error as? APIError {
                store.refreshError = RefreshError(message: api.message, kind: api.kind)
            } else {
                store.refreshError = RefreshError(message: "refresh failed", kind: .unknown)
            }
            return .failed
        }
    }

    /// The newest message in a freshly pulled read model that belongs to the
    /// open thread and that the reader has not got. Ids are the store's own row
    /// ids, so newer really is greater — and the compare is what keeps this
    /// quiet: the same rows sit in the bands for as long as they are unresolved,
    /// and only one that outranks the summary's newest is news.
    ///
    /// The NEWEST of them and not each one, because the row is only carried for
    /// the sender's name on a toast: three replies landing between two polls is
    /// one refetch that brings all three, and one line naming whoever spoke
    /// last beats three lines stacked on top of each other.
    ///
    /// KNOWN LIMITATION, and it is why the event feed is the reporter that
    /// matters: the bands collapse to ONE ROW PER THREAD, and the survivor is
    /// picked by importance before recency (see the ROW_NUMBER in
    /// store::sqlite::attention). So a new message that scores BELOW a sibling
    /// already in the band never appears here at all, however recent it is —
    /// this sees an arrival only when it is the most important thing its thread
    /// has to offer, which for a live back-and-forth it usually is.
    private func newestArrival(
        in data: SitrepData, for open: OpenThreadSummary
    ) -> AttentionUpdate? {
        [data.standing, data.new, data.open]
            .joined()
            .filter { $0.thread_id == open.threadId && $0.id > open.newestMessageId }
            .max { $0.id < $1.id }
    }

    /// Manual refresh: poke the daemon to poll Gmail now, then re-pull so fresh
    /// mail shows without waiting out the ~5s server or the client poll. The
    /// poke is fire-and-forget, hence two pulls — one when rows have likely
    /// landed, one a beat later for a slower Gmail round trip.
    func triggerMailRefresh() async {
        guard store.connStatus == .connected else { return }
        // Poke failure is non-fatal — pullSitrep surfaces its own error.
        _ = try? await APIClient.shared.refreshMail()
        try? await Task.sleep(for: .milliseconds(400))
        await pull()
        try? await Task.sleep(for: .milliseconds(1600))
        await pull()
    }
}

// MARK: - cadence

/// How often the sitrep poll runs, given what the person can see.
///
/// The poll is ten seconds because a board somebody is looking at should not
/// be ten seconds stale. A board nobody can see has no such claim, and it used
/// to be polled at the same rate anyway: an idle Mac with the window behind
/// other apps, minimised or closed (the app stays up as a notifier) asked the
/// daemon for the feed, the stats and the sealed list every ten seconds, plus
/// the zones every 45, for as long as it ran.
///
/// Frontmost with a window on screen is the ten seconds it always was. A
/// window on screen behind another app (a Mac on a second display) is thirty,
/// since it can still be glanced at. Nothing on screen (every window closed,
/// minimised, hidden, covered or on another Space; the display asleep or
/// locked, frontmost or not; the phone in the background) is five minutes, a
/// backstop for the Dock badge and a failed scheduled send: mail worth a
/// notification wakes the poll through the event feed (no more often than the
/// behind cadence), coming back on screen wakes it, and anything else (a
/// quiet reply in a live thread, mail resolved on the phone) waits for the
/// backstop.
enum PollCadence: Equatable, Sendable {
    case foreground
    case behind
    case hidden

    init(active: Bool, onScreen: Bool) {
        if !onScreen {
            self = .hidden
        } else if active {
            self = .foreground
        } else {
            self = .behind
        }
    }
}

/// Every interval the poll keeps, in one value: the three cadences, and from
/// the foreground one the failure backoff.
struct PollTiming: Sendable {
    var foreground: Duration = .seconds(10)
    var behind: Duration = .seconds(30)
    var hidden: Duration = .seconds(300)

    func interval(_ cadence: PollCadence) -> Duration {
        switch cadence {
        case .foreground: foreground
        case .behind: behind
        case .hidden: hidden
        }
    }

    /// The failure backoff's base, twice the foreground cadence, so its
    /// jittered floor equals it.
    var backoffBase: TimeInterval { 2 * foreground.timeInterval }
    /// The backoff's cap, six foreground ticks: how stale a board somebody is
    /// looking at may be once the daemon is back.
    var backoffCap: TimeInterval { 6 * foreground.timeInterval }
}

// MARK: - catch-up analytics

extension SitrepPoller {
    /// THE FIRST BACKFILL, END TO END. `catch_up` is on every stats poll while
    /// the daemon is walking a known amount of mail; this remembers when it
    /// first appeared and how big it got, and when it goes away (or reports
    /// itself finished) sends one `catchup_completed` with the message count
    /// and the minutes it took. Two ints, nothing about the mail. Persisted in
    /// UserDefaults so a relaunch mid-backfill keeps the start, since a first
    /// run is exactly when the app is most likely to be quit and reopened.
    ///
    /// Only the active mailbox's poller sees its stats, so a background
    /// account's backfill is not measured; and a catch-up shorter than one
    /// poll interval is never observed at all, which is fine: the number that
    /// matters is how long a NEW install stares at "catching up". Both ends are
    /// seen at the poll's cadence, so the measure is to the ten seconds while
    /// the app is frontmost, the thirty behind another app, and the five
    /// minutes while nothing is on screen.
    func noteCatchUp(_ progress: CatchUpProgress?) {
        let defaults = UserDefaults.standard
        let startKey = "app.passband.analytics.catchup.startedAt"
        let totalKey = "app.passband.analytics.catchup.total"
        if let progress, progress.total > 0, progress.done < progress.total {
            if defaults.double(forKey: startKey) == 0 {
                defaults.set(Date().timeIntervalSince1970, forKey: startKey)
            }
            defaults.set(max(progress.total, defaults.integer(forKey: totalKey)), forKey: totalKey)
            return
        }
        let started = defaults.double(forKey: startKey)
        guard started > 0 else { return }
        let minutes = Int((Date().timeIntervalSince1970 - started) / 60)
        Analytics.capture(
            "catchup_completed",
            ["messages": defaults.integer(forKey: totalKey), "minutes": max(0, minutes)])
        defaults.removeObject(forKey: startKey)
        defaults.removeObject(forKey: totalKey)
    }
}
