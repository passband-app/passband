// READING HERO CACHE — each sender's thumbnail resolved ONCE per thread id,
// negative results included (a sender with no art must not re-walk its thread on
// every LazyVGrid recycle), downsampled at decode time so the bytes kept are the
// bytes drawn, and decoded/sampled in a detached task with only Sendable values
// (the encoded thumbnail and three colour components) crossing back. Preloading
// is just "resolve everything the zone is about to show".
//
// The thread-id table is one session's. Behind it sits `HeroDisk`, which keeps
// the RENDERED thumbnails across done, relaunch and account switch, and
// remembers each sender's last one, so a card paints on its first frame instead
// of waiting out a thread fetch, a download and a decode it already paid for.

import CryptoKit
import SwiftUI

@MainActor
final class HeroCache {
    static let shared = HeroCache()

    /// A resolved hero: art already downsampled to its square, plus the colour
    /// to paint behind it.
    struct Hero {
        let image: PlatformImage
        let fill: Color?
        /// The art is a logo CUT OUT of a larger flat banner (`ImageTrim`), not
        /// the image the email sent. The card draws it whole and inset, on
        /// `fill`, which for a mark is the banner's own background.
        let isMark: Bool
    }

    /// The rendered size in PIXELS: the 54pt square at 2x, retina being the only
    /// display class this app ships on. `nonisolated` for the detached render task.
    private nonisolated static let maxPixel = 108

    /// The size a hero is decoded at to LOOK for a mark in it. Far above
    /// `maxPixel` on purpose: at 108px a logo that is a twentieth of a masthead
    /// is five pixels, and there is nothing left to cut out. 1024 holds a 2x
    /// 600pt email banner nearly whole, so the crop still has a tile's worth
    /// of pixels in it.
    private nonisolated static let analysisPixel = 1024

    /// How many heroes resolve at once during a preload. Each item is network-bound
    /// but ends in a decode, so a wide fan-out queues CPU behind the scroll it is
    /// meant to smooth.
    private static let preloadWidth = 4

    /// How many resolved heroes to keep. Each is a 108px thumbnail, so the
    /// ceiling is tens of megabytes at worst — generous enough that a session's
    /// cards never re-fetch, bounded so a long list cannot grow forever.
    private static let cacheMax = 512

    /// threadId -> resolved hero, or nil for "checked, has none". The outer
    /// Optional is cache membership; the inner one is the verdict.
    private let memo = AsyncMemo<String, Hero?>(limit: cacheMax)

    /// threadId -> what its resolve concluded, for the sender index. Kept beside
    /// the memo rather than in it because the memo's value is what the card
    /// DRAWS, and a failed fetch draws the sender's old art: the memo alone
    /// cannot tell "no art" from "could not look".
    private var verdicts = LRUMap<String, Verdict>(limit: cacheMax)

    /// `art` carries the content hash so a card can file it under its sender.
    fileprivate enum Verdict {
        case art(hash: String)
        case none
        case failed
    }

    private init() {}

    /// A cached hero for instant render, without starting any work — a recycled card
    /// reads this in `body` instead of flashing empty while its `.task` re-confirms.
    ///
    /// Before this thread has a verdict, the SENDER's last art stands in: the
    /// same senders keep sending the same logo, and a card that paints it on
    /// frame one beats a correct one a second later. A verdict always wins,
    /// including "none", so a sender who stopped sending art is not haunted by it.
    func cached(_ threadId: String, address: String? = nil) -> Hero? {
        if let entry = memo.cached(threadId) { return entry }
        guard let address, Prefs.shared.loadRemoteImages, !RehearsalMode.isEnabled else { return nil }
        return HeroDisk.shared.hero(sender: address)
    }

    /// Forget every resolved hero. An account switch: thread ids are one
    /// daemon's, and the art behind them was fetched through that daemon's
    /// mail — so the whole table is about a mailbox that is no longer on
    /// screen. The negative entries go with it, or a sender with no art in the
    /// old account would render as one in the new.
    ///
    /// `HeroDisk` is NOT wiped: it is keyed by a hash of the image url and of the
    /// sender address, neither of which a daemon minted, so a hit in the next
    /// account is the same art.
    func wipe() {
        memo.clear()
        verdicts.removeAll()
    }

    /// Whether this thread has a VERDICT — art, or a cached "has none". `cached`
    /// flattens both to nil and cannot answer it; a card checks this before
    /// starting a `.task` that would only re-hand it what it already drew.
    func isResolved(_ threadId: String) -> Bool { memo.cached(threadId) != nil }

    /// Resolve one hero, deduped and memoized. The remote-images gate is checked
    /// AFTER the cache and any running fetch: a pref flipped off mid-flight must
    /// not throw away an answer already paid for, nor cache a "no" it never asked.
    ///
    /// `address` is the card's sender: it files the answer in the sender index,
    /// and stands in when the fetch FAILS. The warmer has none and passes nil.
    @discardableResult
    func resolve(_ threadId: String, address: String? = nil) async -> Hero? {
        if let entry = memo.cached(threadId) {
            learn(threadId, address: address)
            return entry
        }
        if let joined = await memo.joinPending(threadId) {
            learn(threadId, address: address)
            return joined
        }
        guard Prefs.shared.loadRemoteImages, !threadId.isEmpty else { return nil }
        let hero = await memo.resolve(threadId) { [weak self] in
            let (hero, verdict) = await Self.fetch(threadId)
            self?.verdicts.set(threadId, verdict)
            guard case .failed = verdict, let address else { return hero }
            return HeroDisk.shared.hero(sender: address)
        }
        learn(threadId, address: address)
        return hero
    }

    /// File this thread's verdict under its sender: art becomes the sender's
    /// art, a confirmed "none" forgets theirs, and a failure changes nothing.
    /// Also called by a card whose thread the warmer already resolved, since
    /// the warmer had no sender to file it under.
    func learn(_ threadId: String, address: String?) {
        guard let address, let verdict = verdicts.peek(threadId) else { return }
        switch verdict {
        case .art(let hash): HeroDisk.shared.link(sender: address, to: hash)
        case .none: HeroDisk.shared.unlink(sender: address)
        case .failed: break
        }
    }

    /// Warm the cache for a whole zone's worth of senders, `preloadWidth` at a
    /// time. Fire-and-forget: already-cached and in-flight ids fall straight
    /// through `resolve`, so calling this on every refresh is near-free.
    func preload(_ threadIds: [String]) {
        let pending = threadIds.filter { !$0.isEmpty && memo.cached($0) == nil }
        guard !pending.isEmpty, Prefs.shared.loadRemoteImages else { return }
        Task {
            await withBoundedTaskGroup(width: Self.preloadWidth, over: pending) { [weak self] id in
                await self?.resolve(id)
            }
        }
    }

    // MARK: - resolution

    /// Thread -> hero src -> bytes -> thumbnail + fill. Every failure is a
    /// negative cache entry, not a retry loop.
    ///
    /// A src already on disk skips the download AND the render: the thumbnail
    /// there is exactly what this would have produced from the same url.
    private static func fetch(_ threadId: String) async -> (Hero?, Verdict) {
        guard let view = try? await ThreadPrefetch.shared.fetch(threadId, fresh: 600) else {
            return (nil, .failed)
        }
        guard let newest = view.messages.last, let html = newest.html,
            let src = Trackers.extractHeroSrc(html), let url = URL(string: src)
        else { return (nil, .none) }

        let hash = HeroDisk.hash(src)
        if let hit = HeroDisk.shared.hero(hash: hash) { return (hit, .art(hash: hash)) }

        // Offline, a timeout, a 5xx: no verdict about the ART, so the sender's
        // last one may still stand in.
        guard let bytes = await HeroBytes.shared.load(url) else { return (nil, .failed) }

        // Off the main actor; only the thumbnail and colour components come back.
        let rendered = await Task.detached(priority: .utility) {
            render(bytes, maxPixel: maxPixel)
        }.value

        guard let rendered, let image = PlatformImage(data: rendered.thumbnail) else {
            return (nil, .none)
        }
        let hero = Hero(image: image, fill: rendered.rgb.map(\.color), isMark: rendered.isMark)
        HeroDisk.shared.store(
            hash: hash, png: rendered.thumbnail, rgb: rendered.rgb, isMark: rendered.isMark,
            hero: hero)
        return (hero, .art(hash: hash))
    }

    /// What crosses back from the detached task — Sendable by construction.
    private struct Rendered: Sendable {
        let thumbnail: Data
        let rgb: ImageFill.RGB?
        let isMark: Bool
    }

    /// Sampled BEFORE the encode, off the thumbnail we are about to keep: the
    /// fill has to come from the same pixels the card draws, and the PNG is what
    /// preserves the alpha that lets that fill show through behind the mark.
    private nonisolated static func render(_ bytes: Data, maxPixel: Int) -> Rendered? {
        guard let thumb = Raster.thumbnail(bytes, maxPixel: maxPixel) else { return nil }
        if let mark = mark(bytes, thumb: thumb, maxPixel: maxPixel) { return mark }
        let rgb = ImageFill.dominantRGB(thumb)
        guard let png = Raster.png(thumb) else { return nil }
        return Rendered(thumbnail: png, rgb: rgb, isMark: false)
    }

    /// The logo cut out of a banner that is mostly its own background, or nil
    /// for anything else — which then draws whole, exactly as before.
    ///
    /// The mark is found at `analysisPixel` and the CROP is what gets
    /// downsampled, so the logo spends the square's pixels rather than the
    /// banner around it. That second, larger decode is only paid when the small
    /// thumbnail already looks like a flat field: a photo says no there.
    ///
    /// Its fill is the banner's background, not a sampled dominant: a mark's
    /// dominant colour is the mark, and a blue logo on a blue square is no
    /// logo. On transparency there is no background to continue, so it gets
    /// the neutral stage it contrasts with.
    private nonisolated static func mark(_ bytes: Data, thumb: CGImage, maxPixel: Int) -> Rendered? {
        guard ImageTrim.hasFlatField(thumb),
            let wide = Raster.thumbnail(bytes, maxPixel: analysisPixel),
            let mark = ImageTrim.mark(in: wide),
            let cut = wide.cropping(to: mark.rect),
            let fitted = Raster.fitted(cut, maxPixel: maxPixel),
            let png = Raster.png(fitted)
        else { return nil }
        return Rendered(
            thumbnail: png, rgb: mark.background ?? ImageFill.stageRGB(fitted), isMark: true)
    }
}

/// Fetches hero bytes: ephemeral, cookie-free and referrer-suppressed, like every
/// other remote-image fetch here. Hands back BYTES rather than an image — decoding
/// belongs off the main actor, and HeroCache already keeps the decoded result.
@MainActor
final class HeroBytes {
    static let shared = HeroBytes()

    /// Every knob that could leak the reader is off — this fetches a URL an email
    /// chose. No `urlCache` because HeroCache holds the only copy worth keeping;
    /// a cache underneath would keep the full-size art on disk too, keyed by a
    /// mail-supplied URL.
    private let session = Sessions.ephemeral(
        timeout: 10, cookies: .off, urlCache: false,
        cachePolicy: .reloadIgnoringLocalCacheData)

    /// 8MB per image.
    private static let maxBytes = 8 * 1024 * 1024

    private init() {}

    func load(_ url: URL) async -> Data? {
        guard url.scheme == "http" || url.scheme == "https" else { return nil }
        var req = URLRequest(url: url)
        req.setValue("", forHTTPHeaderField: "Referer")
        guard let (data, response) = try? await session.data(for: req),
            let http = response as? HTTPURLResponse, http.statusCode == 200,
            data.count <= Self.maxBytes,
            let mime = http.value(forHTTPHeaderField: "Content-Type"),
            mime.lowercased().hasPrefix("image/")
        else { return nil }
        return data
    }
}

/// RENDERED HEROES ON DISK — the 108px thumbnail `HeroCache` drew, plus its fill,
/// filed by sha256 of the hero's src, and an index from sha256 of each sender's
/// address to the last hero they sent. The thread-id memo above dies with the
/// session and with every account switch; this is what lets a sender's card
/// paint on its first frame the next time their mail shows up.
///
/// Holds NO urls and NO addresses, only their hashes (docs/SECURITY.md §3, the
/// same rule as `ImageStore`). Small enough to stay on the main actor: an index
/// of a few hundred rows read once, and thumbnails of tens of KB read on a
/// card's first frame. Writes go to a detached task.
@MainActor
final class HeroDisk {
    static let shared = HeroDisk(
        dir: (FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? URL(fileURLWithPath: NSTemporaryDirectory()))
            .appendingPathComponent("Passband/HeroCache", isDirectory: true))

    private struct Meta: Codable {
        /// The fill, as sRGB components. nil = sampling failed, draw the well.
        var rgb: [Double]?
        var isMark: Bool
        var lastAccess: Double
    }

    private struct Index: Codable {
        var heroes: [String: Meta] = [:]
        /// sha256(lowercased address) -> hero hash.
        var senders: [String: String] = [:]
    }

    /// How many thumbnails to keep. A few tens of KB each, so this is a few tens
    /// of MB at worst, and far more senders than one Reading board ever holds.
    static let maxEntries = 600
    /// How many DECODED heroes stay in memory, so a card redrawn mid-scroll does
    /// not re-read its file.
    private static let decodedMax = 256
    private static let saveDelay: Duration = .seconds(2)

    private let dir: URL
    private let indexURL: URL
    private var index = Index()
    private var loaded = false
    private var decoded: LRUMap<String, HeroCache.Hero>
    private var saveTask: Task<Void, Never>?

    init(dir: URL) {
        self.dir = dir
        indexURL = dir.appendingPathComponent("index.json")
        decoded = LRUMap(limit: Self.decodedMax)
    }

    static func hash(_ s: String) -> String { SHA256.hash(data: Data(s.utf8)).hex }

    private static func senderKey(_ address: String) -> String {
        hash(address.trimmingCharacters(in: .whitespaces).lowercased())
    }

    // MARK: - reads

    /// The hero stored under this content hash, decoded once and then kept.
    func hero(hash: String) -> HeroCache.Hero? {
        ensureLoaded()
        guard let meta = index.heroes[hash] else { return nil }
        index.heroes[hash]?.lastAccess = Date().timeIntervalSince1970
        scheduleSave()
        if let hit = decoded.get(hash) { return hit }
        guard let png = try? Data(contentsOf: fileURL(hash)), let image = PlatformImage(data: png)
        else {
            // Index says present, disk disagrees: forget it so the next resolve
            // re-renders instead of missing here forever.
            drop(hash)
            return nil
        }
        let fill = meta.rgb.flatMap { c in
            c.count == 3 ? ImageFill.RGB(r: c[0], g: c[1], b: c[2]).color : nil
        }
        let hero = HeroCache.Hero(image: image, fill: fill, isMark: meta.isMark)
        decoded.set(hash, hero)
        return hero
    }

    /// The last hero this sender sent, if we kept it.
    func hero(sender address: String) -> HeroCache.Hero? {
        ensureLoaded()
        guard let hash = index.senders[Self.senderKey(address)] else { return nil }
        return hero(hash: hash)
    }

    // MARK: - writes

    func store(
        hash: String, png: Data, rgb: ImageFill.RGB?, isMark: Bool, hero: HeroCache.Hero
    ) {
        ensureLoaded()
        let url = fileURL(hash)
        Task.detached(priority: .utility) { try? png.write(to: url, options: .atomic) }
        index.heroes[hash] = Meta(
            rgb: rgb.map { [$0.r, $0.g, $0.b] }, isMark: isMark,
            lastAccess: Date().timeIntervalSince1970)
        decoded.set(hash, hero)
        evictIfNeeded()
        scheduleSave()
    }

    func link(sender address: String, to hash: String) {
        ensureLoaded()
        guard index.heroes[hash] != nil else { return }
        let key = Self.senderKey(address)
        guard index.senders[key] != hash else { return }
        index.senders[key] = hash
        scheduleSave()
    }

    func unlink(sender address: String) {
        ensureLoaded()
        guard index.senders.removeValue(forKey: Self.senderKey(address)) != nil else { return }
        scheduleSave()
    }

    /// Write the index now. For tests, and nothing else needs it: a lost index
    /// costs one re-render per sender.
    func flush() {
        saveTask?.cancel()
        saveTask = nil
        saveNow()
    }

    // MARK: - disk

    private func fileURL(_ hash: String) -> URL { dir.appendingPathComponent(hash + ".png") }

    private func drop(_ hash: String) {
        index.heroes.removeValue(forKey: hash)
        decoded.removeValue(forKey: hash)
        index.senders = index.senders.filter { $0.value != hash }
        let url = fileURL(hash)
        Task.detached(priority: .utility) { try? FileManager.default.removeItem(at: url) }
        scheduleSave()
    }

    /// Oldest-touched out first. A sender pointing at an evicted hero loses it,
    /// which costs that sender one re-render the next time their mail shows up.
    private func evictIfNeeded() {
        let over = index.heroes.count - Self.maxEntries
        guard over > 0 else { return }
        let oldest = index.heroes.sorted { $0.value.lastAccess < $1.value.lastAccess }.prefix(over)
        for (hash, _) in oldest { drop(hash) }
    }

    private func scheduleSave() {
        guard saveTask == nil else { return }
        saveTask = Task { [weak self] in
            try? await Task.sleep(for: Self.saveDelay)
            guard !Task.isCancelled else { return }
            self?.saveTask = nil
            self?.saveNow()
        }
    }

    private func saveNow() {
        guard let data = try? JSONEncoder().encode(index) else { return }
        let url = indexURL
        Task.detached(priority: .utility) { try? data.write(to: url, options: .atomic) }
    }

    /// First touch: make the directory, read the index, and drop rows whose
    /// file is gone plus files no row knows about.
    private func ensureLoaded() {
        guard !loaded else { return }
        loaded = true
        let fm = FileManager.default
        try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutable = dir
        try? mutable.setResourceValues(values)

        if let data = try? Data(contentsOf: indexURL),
            let decoded = try? JSONDecoder().decode(Index.self, from: data)
        {
            index = decoded
        }
        let names = Set((try? fm.contentsOfDirectory(atPath: dir.path)) ?? [])
        for hash in index.heroes.keys where !names.contains(hash + ".png") {
            index.heroes.removeValue(forKey: hash)
        }
        index.senders = index.senders.filter { index.heroes[$0.value] != nil }
        for name in names where name.hasSuffix(".png") {
            if index.heroes[String(name.dropLast(4))] == nil {
                try? fm.removeItem(at: dir.appendingPathComponent(name))
            }
        }
    }
}
