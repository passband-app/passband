// Thread prefetch — the "instant open" machinery. `prefetch(id)` fetches and
// LRU-caches a ClientThreadView; `cached(id)` hands the viewer a fresh one to
// render synchronously on mount, with no loading flash or refetch. A cached
// thread also warms its PREPARED bodies (strip / dedupe / link scan) off the
// main actor, so the open is scan-free as well as network-free. A warmed copy
// lasts minutes (`warmFresh`); the reader asks the daemon about one older than
// a minute after painting it (`isAged`), and `retire` drops one the moment a
// row names a message it lacks. Also holds the measured-height memory, so a
// reopened message paints at its final size.

import Foundation

@MainActor
final class ThreadPrefetch {
    static let shared = ThreadPrefetch()

    /// Sized to hold the whole For-your-eyes list (the sitrep preloads every
    /// standing item, up to its band limit of 200) plus the inbox's head rows,
    /// without LRU churn: at 60 the trickle of a long list evicted the ten
    /// rows on screen, which are the ones that get clicked. A cached view is
    /// a thread's bodies, a few hundred kilobytes at most.
    private static let cacheMax = 260
    /// A cached view older than this refetches on real open — mail can change.
    /// Also how long a thread that would not be kept is left alone.
    private static let freshDefault: TimeInterval = 60
    /// How long a WARMED thread stays good: five minutes, every warm path's
    /// lifetime now and the board's for longer. The warms run on the poll, so
    /// at the 60 s default the Emails page refetched its forty head threads
    /// every minute to find them exactly as they were.
    ///
    /// What makes the five minutes safe is the reader, not this cache: a copy
    /// past the default minute paints the open at once and the reader then
    /// asks the daemon for the current one, taking it in place only if it
    /// differs (`isAged`, ThreadViewer.revalidate). `retire` keeps a copy a row
    /// shows to be behind from painting at all.
    static let warmFresh: TimeInterval = 5 * 60

    private struct Entry {
        var view: ClientThreadView
        /// The newest message id the view holds. A row naming a newer one is
        /// mail this copy predates.
        var newest: Int
        /// Filing order, to tell an answer whose fetch began before this.
        var seq: Int
        var ts: Date
        var fresh: TimeInterval
    }

    /// A thread with no copy to keep, left alone by warms and hovers for the
    /// default minute (whatever lifetime the caller asked for) so they do not
    /// ask for it on every tick. Never served; `fetch` goes past it.
    private struct Withheld {
        /// The daemon said not to keep it (`cache_allowed`), as opposed to the
        /// fetch failing (a 404 for a sealed thread, the daemon away). A newer
        /// message clears a decline; a failure waits out its minute.
        var declined: Bool
        var newest: Int
        var seq: Int
        var until: Date
    }

    private var cache = LRUMap<String, Entry>(limit: cacheMax)
    private var withheld = LRUMap<String, Withheld>(limit: cacheMax)
    /// One shared task for preloads, heroes and visible previews. Results live
    /// in the TTL cache above; this memo only coalesces concurrent requests.
    private let inflight = AsyncMemo<String, Result<ClientThreadView, Error>>(
        limit: 1, keep: { _ in false })
    private let loader: @MainActor (String) async throws -> ClientThreadView
    /// Bumped by `wipe()`. Every fetch this class starts captures it and files
    /// nothing once it no longer matches: a thread fetched for the old account
    /// must not land in the new account's cache, and the trickle of a batch
    /// warm must not keep asking the NEW daemon for the OLD one's thread ids.
    private var generation = 0
    /// The newest message a row named for a thread while its fetch was out. An
    /// answer older than that is handed to its caller and not filed.
    private var heardInFlight: [String: Int] = [:]
    private var filings = 0
    private let now: @MainActor () -> Date

    init(
        loader: @escaping @MainActor (String) async throws -> ClientThreadView = {
            try await APIClient.shared.getThread($0)
        },
        now: @escaping @MainActor () -> Date = { Date() }
    ) {
        self.loader = loader
        self.now = now
    }

    private static func newest(_ view: ClientThreadView) -> Int {
        view.messages.lazy.map(\.id).max() ?? 0
    }

    /// The filing order of whatever is on file for this thread, 0 for nothing.
    private func filed(_ threadId: String) -> Int {
        max(cache.peek(threadId)?.seq ?? 0, withheld.peek(threadId)?.seq ?? 0)
    }

    /// A decline takes the copy with it: the daemon's word is that nothing of
    /// this thread is to be kept. A failure leaves a copy where it is, for the
    /// callers whose lifetimes it is still inside.
    private func withhold(_ threadId: String, declined: Bool, newest: Int) {
        filings &+= 1
        if declined { cache.removeValue(forKey: threadId) }
        withheld.set(threadId, Withheld(
            declined: declined, newest: newest, seq: filings,
            until: now().addingTimeInterval(Self.freshDefault)))
    }

    /// `since`: the filing count when this answer's fetch began. What was filed
    /// after that and is at least as new stays: a copy holding the same mail or
    /// later (the reader's refetch after a send, with that send's status), or
    /// the daemon's later word that the thread is not to be kept.
    private func put(
        _ threadId: String, _ view: ClientThreadView, newest: Int, fresh: TimeInterval,
        gen: Int, since: Int? = nil
    ) {
        guard gen == generation else { return }
        if let since {
            if let later = cache.peek(threadId), later.seq > since, later.newest >= newest { return }
            // A decline from the reader, or its 404: either way the later word.
            if let later = withheld.peek(threadId), later.seq > since { return }
        }
        guard view.cache_allowed == true else {
            withhold(threadId, declined: true, newest: newest)
            return
        }
        filings &+= 1
        withheld.removeValue(forKey: threadId)
        cache.set(threadId, Entry(view: view, newest: newest, seq: filings, ts: now(), fresh: fresh))
        warmBodies(view)
    }

    /// Drop everything, cache and in-flight bookkeeping alike. An account
    /// switch: thread ids, the message ids inside the views, and the
    /// repeated-image maps keyed off both all belong to one daemon.
    func wipe() {
        generation &+= 1
        cache.removeAll()
        withheld.removeAll()
        inflight.clear()
        heardInFlight.removeAll()
    }

    /// Forget one thread: the daemon no longer serves it (the reader's check
    /// of an aged copy got a 404). Left alone for the minute, as a failure is,
    /// and filed, so a warm's answer from before the 404 is not filed after it.
    func drop(_ threadId: String) {
        cache.removeValue(forKey: threadId)
        withhold(threadId, declined: false, newest: 0)
    }

    /// Whether this copy is good for a caller asking `ttl`: it takes the longer
    /// of the two lifetimes, so a repeat may EXTEND one, never shorten it.
    private func live(_ hit: Entry, asking ttl: TimeInterval) -> Bool {
        now().timeIntervalSince(hit.ts) < max(hit.fresh, ttl)
    }

    /// The extension: a copy a caller asking `ttl` was given keeps it.
    private func extend(_ threadId: String, _ hit: Entry, to ttl: TimeInterval) {
        guard ttl > hit.fresh else { return }
        var hit = hit
        hit.fresh = ttl
        cache.set(threadId, hit)
    }

    /// Whether the thread is being left alone: no copy to keep, for a minute.
    func isWithheld(_ threadId: String) -> Bool {
        guard let mark = withheld.peek(threadId) else { return false }
        return now() < mark.until
    }

    /// Whether a warm asking for `ttl` has nothing to do for this thread. A
    /// copy that is live takes the longer lifetime.
    private func settled(_ threadId: String, ttl: TimeInterval) -> Bool {
        if isWithheld(threadId) { return true }
        guard let hit = cache.get(threadId), live(hit, asking: ttl) else { return false }
        extend(threadId, hit, to: ttl)
        return true
    }

    /// Fire-and-forget fetch + cache. Deduped while in flight (a prefetch that
    /// joins one may extend the lifetime it files with); a fresh hit is a
    /// no-op, and so is a thread that is withheld. `fresh` is a per-entry TTL
    /// (warmed and right-rail threads outlive the 60s default), and a repeat
    /// prefetch may EXTEND a TTL, never shorten it.
    func prefetch(_ threadId: String, fresh: TimeInterval? = nil) {
        guard !threadId.isEmpty else { return }
        let ttl = fresh ?? Self.freshDefault
        if !settled(threadId, ttl: ttl) { start(threadId, ttl: ttl) }
    }

    private func start(_ threadId: String, ttl: TimeInterval) {
        let gen = generation
        Task { [weak self] in
            guard let self, gen == generation else { return }
            _ = try? await fetch(threadId, fresh: ttl)
        }
    }

    /// A fresh cached view for instant render, or nil. `holding`: message ids
    /// the open names (the reply it is for, the message it lands on); a copy
    /// lacking one is behind on exactly what was asked for, and is not handed
    /// out.
    func cached(_ threadId: String, holding ids: [Int] = []) -> ClientThreadView? {
        guard let hit = cache.get(threadId), live(hit, asking: 0),
            ids.allSatisfy({ id in hit.view.messages.contains { $0.id == id } })
        else { return nil }
        return hit.view
    }

    /// Whether the copy on file is older than the default minute: one a warm
    /// kept, which the reader paints and then asks the daemon about.
    func isAged(_ threadId: String) -> Bool {
        guard let hit = cache.peek(threadId) else { return false }
        return now().timeIntervalSince(hit.ts) >= Self.freshDefault
    }

    /// Drop every copy a row shows to be BEHIND: a row naming a message newer
    /// than anything in the cached view is mail that landed after the view was
    /// fetched, and an open from that copy would paint without it. A decline
    /// goes the same way, since the new mail is worth asking about. A fetch
    /// out for the thread is told too, and its answer, if it is older, goes to
    /// its caller without being filed. A row naming an OLDER message the view
    /// does not hold changes nothing: that is not mail this copy missed.
    ///
    /// Called with the rows that name what arrives: every band pull, the mail
    /// and sent pages, each live event. The reader's check of an aged copy is
    /// what covers everything they do not name.
    func retire(_ rows: some Sequence<(String, Int)>) {
        for (threadId, messageId) in rows {
            if let hit = cache.peek(threadId), messageId > hit.newest {
                cache.removeValue(forKey: threadId)
            }
            if let mark = withheld.peek(threadId), mark.declined, messageId > mark.newest {
                withheld.removeValue(forKey: threadId)
            }
            if inflight.isPending(threadId) {
                heardInFlight[threadId] = max(heardInFlight[threadId] ?? messageId, messageId)
            }
        }
    }

    /// Fetch THROUGH the cache: a fresh hit resolves immediately.
    func fetch(_ threadId: String, fresh: TimeInterval? = nil) async throws -> ClientThreadView {
        let ttl = fresh ?? Self.freshDefault
        try Task.checkCancellation()
        if let hit = cache.get(threadId), live(hit, asking: ttl) {
            extend(threadId, hit, to: ttl)
            return hit.view
        }
        let gen = generation
        let since = filings
        let result = await inflight.resolve(threadId) { [self] in
            // Only what is named while THIS fetch is out counts against it.
            if gen == generation { heardInFlight.removeValue(forKey: threadId) }
            let loaded: Result<ClientThreadView, Error>
            do {
                let view = try await loader(threadId)
                loaded = .success(view)
            } catch {
                loaded = .failure(error)
            }
            guard gen == generation else { return loaded }
            let heard = heardInFlight.removeValue(forKey: threadId)
            switch loaded {
            case .success(let view):
                let newest = Self.newest(view)
                // Read before a message a row has since named: the caller
                // gets it, the cache does not, and the next warm asks again.
                if let heard, heard > newest { break }
                put(threadId, view, newest: newest, fresh: ttl, gen: gen, since: since)
            case .failure(let error) where !(error is CancellationError):
                // Not over anything filed while this fetch was out, nor over a
                // live decline, which newer mail can clear and a failure would
                // not. A copy stays where it is.
                let declined = withheld.peek(threadId).map { $0.declined && now() < $0.until } ?? false
                if filed(threadId) <= since, !declined {
                    withhold(threadId, declined: false, newest: 0)
                }
            case .failure:
                break
            }
            return loaded
        }
        // A clear retires the shared task without cancelling another consumer.
        // Old-account results must not escape to a hero or a newly mounted peek.
        guard gen == generation else { throw CancellationError() }
        try Task.checkCancellation()
        let view = try result.get()
        // Joiners can request a longer lifetime than the caller that started it.
        if let hit = cache.get(threadId) { extend(threadId, hit, to: ttl) }
        return view
    }

    /// Let the viewer's own (authoritative) fetch feed the cache, so the next
    /// reopen is instant too. Synchronous, so the live generation is by
    /// definition the one this view was fetched under.
    func note(_ threadId: String, _ view: ClientThreadView) {
        put(threadId, view, newest: Self.newest(view), fresh: Self.freshDefault, gen: generation)
    }

    // MARK: - prepared bodies

    /// Preprocess every body in a freshly cached thread OFF the main actor —
    /// otherwise opening a thread pays for a regex walk of every body plus a
    /// runloop beat before the first frame has html to load.
    ///
    /// Fire-and-forget and idempotent: a key already in PreparedBodies is
    /// skipped. Everything crossing out is a value type and Trackers /
    /// ImageRepeats are pure, so only the hand-back needs the main actor.
    private func warmBodies(_ view: ClientThreadView) {
        Task.detached(priority: .utility) {
            for message in view.messages {
                guard let html = message.html, !html.isEmpty else { continue }
                // The tracker policy is part of the prepared identity, so the
                // warmer must key on the SAME one the card will render under or
                // every known-sender body misses and re-scans on the main path.
                let allow = message.allowsTrackers
                // The PARTS are part of that identity too — the cid rewrite
                // resolves against them — so they are passed for the same
                // reason: warm under a different key and every body with an
                // attachment misses and re-scans on the main path.
                let parts = message.attachmentList
                let key = EmailWebView.Prepared.cacheKey(html, allow, parts)
                guard PreparedBodies.shared.get(key) == nil else { continue }
                PreparedBodies.shared.set(
                    key,
                    EmailWebView.Prepared.make(
                        from: html, allowTrackers: allow, attachments: parts))
            }
        }
    }

    // MARK: - batch warming

    /// Warm a staggered batch so a fresh list never stampedes the daemon.
    /// `immediate` rows go now; the rest trickle at `spacing`. Each is kept for
    /// `warmFresh`.
    ///
    /// Only the threads that NEED a fetch, once each: a list whose threads are
    /// all cached costs nothing (the warms run on the poll, and a trickle of
    /// no-op wakes every tick is not nothing), and a thread on several rows is
    /// asked for once.
    func warm(_ threadIds: [String], immediate: Int = 5, spacing: Duration = .milliseconds(120)) {
        let ttl = Self.warmFresh
        var seen = Set<String>()
        var rest: [String] = []
        for (row, id) in threadIds.enumerated() where !id.isEmpty && seen.insert(id).inserted {
            guard !settled(id, ttl: ttl) else { continue }
            if row < immediate { start(id, ttl: ttl) } else { rest.append(id) }
        }
        guard !rest.isEmpty else { return }
        let gen = generation
        Task { [weak self] in
            for id in rest {
                try? await Task.sleep(for: spacing)
                if Task.isCancelled { return }
                // The list belongs to ONE account. A switch mid-trickle must
                // abandon the rest of it rather than ask the new daemon for
                // thread ids it has never heard of.
                guard let self, gen == self.generation else { return }
                self.prefetch(id, fresh: ttl)
            }
        }
    }
}

/// Preprocessed email bodies, keyed by `EmailWebView.Prepared.cacheKey` — the
/// html and tracker policy.
///
/// LOCK-BASED RATHER THAN @MainActor, deliberately: the warmer fills it from a
/// detached task while `EmailWebView.init` — which SwiftUI runs nonisolated,
/// before any `.task` or `onAppear` — reads it to seed state, and neither can
/// await.
final class PreparedBodies: @unchecked Sendable {
    static let shared = PreparedBodies()

    /// Several threads' worth of messages — enough that walking a queue back
    /// and forth never re-scans, bounded because each entry holds a body copy.
    private static let cap = 300

    private let lock = NSLock()
    /// The LRU is not thread-safe on its own; the lock that publishes these
    /// entries across isolation domains is what makes touching it safe.
    private var entries = LRUMap<Int, EmailWebView.Prepared>(limit: cap)

    private init() {}

    func get(_ key: Int) -> EmailWebView.Prepared? {
        lock.lock()
        defer { lock.unlock() }
        return entries.get(key)
    }

    func set(_ key: Int, _ prepared: EmailWebView.Prepared) {
        lock.lock()
        defer { lock.unlock() }
        entries.set(key, prepared)
    }
}

/// Remembered rendered heights, keyed by message id: a frame with one renders
/// at its final size instantly, with no resize on reopen (the reader perceives
/// that resize as flicker).
///
/// It also keeps the GUESSED height of a body nothing has measured yet — the
/// size a message is given while it is scrolling into view for the first time.
/// The two are deliberately separate maps: a measurement is what the document
/// turned out to be, a guess is what its text says it will be, and a guess must
/// never be remembered as though a frame had reported it.
/// OBSERVABLE, because a height arriving is news. The measuring pass fills this
/// while the reader is already looking at the thread, and a frame that mounted
/// before its message was measured has to pick the measurement up when it
/// lands — otherwise the one message that is always on screen before the pass
/// starts, the newest, is also the one that never gets a proper size.
@MainActor
@Observable
final class FrameHeights {
    @ObservationIgnored static let shared = FrameHeights()
    /// A height and WHERE IT CAME FROM. The two sources are not equally good
    /// and the difference decides things: the offscreen pass measures at a
    /// fixed viewport, so nothing it reports can have been influenced by what
    /// it reported last, while a live frame measures inside a box sized from
    /// its own previous answer. For most mail they agree. For a body written in
    /// `vh` units there is no viewport-independent answer at all — the height
    /// is whatever viewport you ask at — and then only the fixed-viewport
    /// number means anything.
    struct Known {
        var height: CGFloat
        var authoritative: Bool
    }

    /// ONE OBSERVABLE CELL PER KEY, and that is the whole point of the shape.
    /// This used to be two dictionaries on the observable object, and a
    /// dictionary is ONE tracked property: a height landing for message A
    /// invalidated every mounted frame in the thread, each of which then ran
    /// its update and hashed its body again. Twenty messages measured newest
    /// first was twenty re-renders of twenty frames. A cell is a tracked
    /// property of its own, so a view reading message A's height is invalidated
    /// by message A's height and by nothing else.
    ///
    /// Created ON READ, not only on write: a frame that asked before its
    /// message was measured has to be invalidated when the answer lands, which
    /// means the cell it observed must be the cell the pass writes to.
    @Observable
    final class Cell {
        var known: Known?
        /// How wide a HUGGING bubble document came to (EmailWebView `bubble`),
        /// so a message scrolled back into view opens at its width rather than
        /// the whole measure and then narrowing. Only ever set on bubble keys.
        var hugWidth: CGFloat?
    }

    @ObservationIgnored private var cells: [String: Cell] = [:]
    /// A MEMO, NOT NEWS: written from inside a view body (see `guess`), and
    /// nothing needs re-rendering because a guess was made. Untracked, so the
    /// first render of card N no longer invalidates cards 0..N-1.
    @ObservationIgnored private var guesses: [String: CGFloat] = [:]
    private init() {}

    private func cell(_ key: String) -> Cell {
        if let cell = cells[key] { return cell }
        let cell = Cell()
        cells[key] = cell
        return cell
    }

    /// THE WIDTH EVERY HEIGHT IN HERE WAS MEASURED AT. A height is only an
    /// answer to a question that included the width — mail reflows, and a body
    /// measured in a wide window is the wrong size in a narrow one.
    ///
    /// The STYLE's contribution to the width is already in the key
    /// (ThreadStyle.frameKey: a bubble is a narrower measure than a card, so
    /// the same message has one height per style). What is left for this is the
    /// column itself, which is the same for both styles and changes only when
    /// the window does — so it is one number rather than part of every key.
    ///
    /// Untracked: the measurer and the pre-renderer read it to size a frame,
    /// and neither is a view.
    @ObservationIgnored private(set) var width: CGFloat = 0

    /// The reader declaring the width it is laying out at. A different one
    /// discards the measurements, which is not a loss: the frames on screen
    /// re-measure themselves as they reflow, and the ones that are not can be
    /// measured again (see FrameMeasurer). The GUESSES survive — they are made
    /// from the text and know nothing about the column.
    func using(width: CGFloat) {
        guard width > 0, abs(width - self.width) > 0.5 else { return }
        self.width = width
        for cell in cells.values {
            if cell.known != nil { cell.known = nil }
            if cell.hugWidth != nil { cell.hugWidth = nil }
        }
    }

    /// The width a hugging bubble document last came to, if this message has
    /// been open as one at this column width.
    func hugWidth(_ key: String) -> CGFloat? { cell(key).hugWidth }
    func setHugWidth(_ key: String, _ width: CGFloat) {
        let cell = cell(key)
        if cell.hugWidth != width { cell.hugWidth = width }
    }

    /// The best height on file, whoever took it — what a frame is drawn at.
    func get(_ key: String) -> CGFloat? { cell(key).known?.height }

    /// Only a height taken at a FIXED viewport. This is the one a live frame
    /// must defer to rather than re-deriving, and the one that says a message
    /// no longer needs measuring.
    func authoritative(_ key: String) -> CGFloat? {
        cell(key).known.flatMap { $0.authoritative ? $0.height : nil }
    }

    /// A provisional height never displaces a measured one: a live frame's
    /// answer is a stopgap until the pass reaches that message, and letting it
    /// overwrite would make the stopgap permanent.
    func set(_ key: String, _ height: CGFloat, authoritative: Bool = false) {
        let cell = cell(key)
        if let known = cell.known, known.authoritative, !authoritative { return }
        // Assignment is notification: an identical height written again is a
        // frame re-rendered for nothing.
        if cell.known?.height == height, cell.known?.authoritative == authoritative { return }
        cell.known = Known(height: height, authoritative: authoritative)
    }

    /// Forget a message, in EVERY SPELLING OF ITS KEY, and its guess with it.
    /// The same message has one height per thread style
    /// (ThreadStyle.frameKey), so a caller that cleared the one spelling it
    /// happened to know left the other to paint a reopened message at a size
    /// nothing on screen is using. The loop lives here so no caller has to know
    /// there are two.
    func clear(messageId: Int) {
        for style in ThreadStyle.allCases {
            let key = style.frameKey(messageId)
            if let cell = cells[key] {
                if cell.known != nil { cell.known = nil }
                if cell.hugWidth != nil { cell.hugWidth = nil }
            }
            guesses.removeValue(forKey: key)
        }
    }

    /// The guessed height for this message, computed once and kept. Memoized
    /// because the caller is a view body — it is asked on every render of a
    /// message card, and the answer is a walk of the body's text (the quoted
    /// chain has to be split off it first, or a one-line reply quoting a long
    /// thread is guessed at the length of the whole thread).
    ///
    /// Keyed the same way the heights are, style and all: the guess is a line
    /// count and a bubble fits fewer characters on a line.
    func guess(_ key: String, _ make: () -> CGFloat) -> CGFloat {
        if let known = guesses[key] { return known }
        let made = make()
        guesses[key] = made
        return made
    }

    /// Forget every height. An account switch: the keys are message ids, one
    /// daemon's, so a surviving entry paints the new account's mail at the old
    /// account's size and then snaps — which reads as a rendering glitch.
    func wipeAll() {
        for cell in cells.values {
            if cell.known != nil { cell.known = nil }
            if cell.hugWidth != nil { cell.hugWidth = nil }
        }
        cells.removeAll()
        guesses.removeAll()
    }
}
