// THE SITREP RIGHT RAIL + the reading zone. Calendar / Shipments / Banking
// / Receipts are RECORDS, not actions: they always render, empty state and all,
// because a column that comes and goes as mail arrives is one you stop trusting.
// These records are auto-resolved out of the attention bands at ingest, so this
// rail is their ONLY surface — clicking a row opens the underlying email.

import SwiftUI

// MARK: - calendar

/// Upcoming calendar records, retained for 24 hours after their first viewing.
struct CalendarZone: View {
    @Environment(AppStore.self) private var store

    /// The rows this card shows at `now`. Static so the sitrep can ask whether
    /// the whole rail is empty by the SAME rule each card draws with.
    @MainActor
    private static var accountScope: String {
        RehearsalMode.isEnabled
            ? CalendarVisibility.practiceAccount
            : AccountManager.shared.activeId?.uuidString ?? "disconnected"
    }
    private var accountScope: String { Self.accountScope }

    @MainActor
    static func rows(_ store: AppStore, now: Date) -> [CalendarUpdate] {
        let account = accountScope
        return store.zones.calendar.filter {
            CalendarVisibility.shared.admits(
                account: account, item: $0.id, startValue: $0.starts_at,
                timezone: $0.start_timezone, now: now)
        }
    }

    var body: some View {
        TimelineView(.periodic(from: .now, by: 30)) { context in
            let rows = Self.rows(store, now: context.date)
            ZoneCard(
                symbol: "calendar", title: "Calendar", count: rows.count, tint: Palette.accent
            ) {
                if rows.isEmpty {
                    EmptyNote("No calendar updates.")
                } else {
                    VStack(spacing: 1) {
                        ForEach(rows) { c in
                            Button {
                                store.openRecord(thread: c.thread_id, message: c.message_id)
                            } label: {
                                HStack(spacing: Space.scaled(7)) {
                                    Text(c.event_title ?? c.organizer ?? "calendar event")
                                        .font(Typo.rowSub)
                                        .foregroundStyle(Palette.inkDim)
                                        .strikethrough(c.kind == .cancellation)
                                        .lineLimit(1)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                    if let tag = c.kind.tag {
                                        Chip(text: tag, tone: tagTone(c.kind))
                                    }
                                    Text(when(c))
                                        .font(Typo.num(10))
                                        .foregroundStyle(Palette.inkFaintest)
                                }
                                .padding(.horizontal, RecordRowStyle.hPadding)
                                .padding(.vertical, Space.scaled(5))
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(RecordRowStyle())
                            .recordRowBleed()
                            .modifier(CalendarSeenMarker(account: accountScope, item: c.id))
                            .warmsThreadOnHover(c.thread_id)
                        }
                    }
                }
            }
            .id(accountScope)
        }
        .task { await store.refreshZones() }
    }

    private func tagTone(_ kind: CalendarKind) -> Color {
        kind == .cancellation ? Palette.danger : Palette.inkFaint
    }

    private func when(_ c: CalendarUpdate) -> String {
        guard c.starts_at != nil else { return "" }
        return Fmt.isToday(c.starts_at) ? Fmt.timeOfDay(c.starts_at) : Fmt.shortDate(c.starts_at)
    }
}

/// Only an on-screen row in the active app starts its retention clock.
private struct CalendarSeenMarker: ViewModifier {
    let account: String
    let item: Int
    @Environment(\.scenePhase) private var scenePhase
    @State private var visible = false

    func body(content: Content) -> some View {
        content
            .onScrollVisibilityChange(threshold: 0.5) { showing in
                visible = showing
                markSeen()
            }
            .onChange(of: scenePhase) { _, _ in markSeen() }
    }

    private func markSeen() {
        guard visible, scenePhase == .active else { return }
        CalendarVisibility.shared.markSeen(account: account, item: item)
    }
}

// MARK: - shipments

/// Still-active shipments plus anything delivered TODAY; older deliveries drop
/// out. No j/k, but each card opens its email, and a context menu — or `e`
/// while hovering, on the Mac — clears a package the user is done looking at.
struct ShipmentsZone: View {
    @Environment(AppStore.self) private var store
    /// The Mac board's hover cursor, for `e`. `nil` on the phone, which has no
    /// hover and no keymap; the context menu is its only way to clear.
    var cursor: SitrepCursor? = nil
    private var shipments: [Shipment] { store.zones.shipments }

    /// A delivered package survives the DAY IT LANDED, and that day is
    /// `delivered_at`. `last_update` only stood in for it: it is the row's last
    /// user-visible change, so a parcel dropped at 11pm and seen by the small-hours
    /// poll wears tomorrow's stamp and would linger a whole extra day. Rows from a
    /// daemon older than the field keep the clock they have always been judged by.
    ///
    /// A GROUPED card stays while ANY of its packages is still coming, even if
    /// the one standing for it has landed: one order's delivered first box must
    /// not hide its second, still in transit.
    private var rows: [Shipment] { Self.rows(store) }

    @MainActor
    static func rows(_ store: AppStore) -> [Shipment] {
        store.zones.shipments.filter { $0.staysOnRail(isToday: Fmt.isToday) }
    }

    var body: some View {
        ZoneCard(
            symbol: "shippingbox", title: "Shipments", count: rows.count, tint: Palette.warn
        ) {
            if rows.isEmpty {
                EmptyNote("Nothing en route.")
            } else {
                VStack(spacing: Space.s8) {
                    ForEach(rows) { ShipmentCard(shipment: $0, cursor: cursor) }
                }
            }
        }
        .task { await store.refreshZones() }
    }
}

private struct ShipmentCard: View {
    @Environment(AppStore.self) private var store
    let shipment: Shipment
    let cursor: SitrepCursor?

    @State private var hovering = false

    private var title: String { shipment.displayTitle }

    /// Status → tone: out_for_delivery is the loud one, delivered fades back.
    private var tone: Color {
        switch shipment.status {
        case .outForDelivery: Palette.warn
        case .shipped: Palette.accent
        case .exception: Palette.danger
        case .delivered: Palette.positive
        case .ordered: Palette.inkFaintest
        }
    }

    /// Where the card jumps: the feeding email's thread. `nil` on a row from an
    /// older daemon that sent no thread_id — then the card is inert and never
    /// paints the hover invitation it can't honor.
    private var target: String? {
        guard let tid = shipment.thread_id, !tid.isEmpty else { return nil }
        return tid
    }

    /// How far ahead a weekday name still names one day. Past it "Tue" is a
    /// guess about which Tuesday, so the date is the shorter sentence.
    private static let weekdayWindow = 6
    /// Two, not one: a single rejection is the ordinary gap between a retailer
    /// printing a label and a carrier scanning the parcel.
    private static let noRecordAfter = 2
    /// Layout guard on somebody else's text, not a copy budget.
    private static let carrierWordsCap = 64

    /// Nearby days as words, distant ones as dates.
    private func dayWord(_ iso: String?) -> String {
        guard let then = Fmt.date(iso) else { return "" }
        if Fmt.isToday(iso) { return "today" }
        let cal = Calendar.current
        let days =
            cal.dateComponents(
                [.day], from: cal.startOfDay(for: Date()), to: cal.startOfDay(for: then)
            ).day ?? 0
        return abs(days) <= Self.weekdayWindow ? Fmt.weekday(iso) : Fmt.shortDate(iso)
    }

    /// The status chip's words. Delivered earns a WHEN, and since the zone keeps
    /// only the day's own deliveries that when is an hour; `last_update` stands in
    /// for a row written before `delivered_at` existed.
    private var statusText: String {
        let label = shipment.status.label
        guard shipment.status == .delivered else { return label }
        let landed = shipment.delivered_at ?? shipment.last_update
        guard Fmt.date(landed) != nil else { return label }
        return "\(label) \(Fmt.isToday(landed) ? Fmt.timeOfDay(landed) : dayWord(landed))"
    }

    /// The arrival estimate, which only a carrier poll ever supplies. Silent on a
    /// delivered package — an estimate for something already on the porch is
    /// noise — and silent under "out for delivery" when it estimates today, which
    /// is the one thing that chip already says.
    private var etaText: String? {
        guard shipment.status != .delivered, let eta = shipment.eta, let day = Fmt.date(eta)
        else { return nil }
        if shipment.status == .outForDelivery, Fmt.isToday(eta) { return nil }
        // A date that has passed is still what the carrier says, but "arrives" is
        // the wrong tense for it — and it takes the DATE rather than the weekday
        // `dayWord` would give, because backwards "Tue" names no particular one.
        if day < Calendar.current.startOfDay(for: Date()) {
            return "expected \(Fmt.shortDate(eta))"
        }
        return "arrives \(dayWord(eta))"
    }

    /// Today's estimate is the one worth an eye; the rest sit back.
    private var etaTone: Color {
        Fmt.isToday(shipment.eta) ? Palette.accent : Palette.inkFaint
    }

    /// ONE muted line under the chips, at most. The pre-manifest hint outranks the
    /// carrier's words because `poll_failures` counts CONSECUTIVE rejections:
    /// anything the carrier once said is older than "we have no record of this".
    private var note: String? {
        if shipment.status != .delivered,
            (shipment.poll_failures ?? 0) >= Self.noRecordAfter
        {
            return "carrier has no record yet"
        }
        return carrierWords
    }

    /// The carrier's own words, kept only where our five-rung ladder could not say
    /// the same thing — "Held at customs" has no rung, "DL Delivered" is a second
    /// copy of the chip. Containment BOTH ways, because the ladder's label is
    /// sometimes the longer string and sometimes the shorter one.
    private var carrierWords: String? {
        guard
            let raw = shipment.carrier_status_raw?.trimmingCharacters(
                in: .whitespacesAndNewlines), !raw.isEmpty
        else { return nil }
        let label = shipment.status.label
        let lowered = raw.lowercased()
        guard !lowered.contains(label), !label.contains(lowered) else { return nil }
        return raw.flattenedLine(cap: Self.carrierWordsCap)
    }

    /// The poll clock rides in the tooltip: real information, worth no card space.
    /// Nothing is appended when no carrier was ever asked, which is the resting
    /// state of a daemon holding no carrier keys.
    private var titleHelp: String {
        guard shipment.last_polled_at != nil else { return title }
        return "\(title) · checked \(Fmt.lastChecked(shipment.last_polled_at))"
    }

    var body: some View {
        // The whole card opens the email, as a tap GESTURE rather than an outer
        // Button: nothing inside competes for the click any more, and a gesture
        // leaves the context menu's own press unclaimed.
        //
        // The status rides on the TITLE LINE, hard right. It is the one thing
        // every card has, so on its own row it left a lone pill under a lone
        // title; up here most packages are a one-line card and the rest add a
        // line only when they have something else to say.
        VStack(alignment: .leading, spacing: Space.scaled(7)) {
            HStack(spacing: Space.scaled(7)) {
                if RehearsalMode.isEnabled, shipment.thread_id == "practice-5" {
                    RehearsalNewsletterLogo(brand: .exfed, size: 18, tile: false)
                } else if RehearsalMode.isEnabled,
                   shipment.thread_id == "practice-13" || shipment.thread_id == "practice-14" {
                    RehearsalNewsletterLogo(brand: .rainforest, size: 18)
                } else {
                    CarrierBadge(carrier: shipment.carrier)
                }
                // The order line sits UNDER the title in the same column, so
                // the badge and chip stay beside the name rather than floating
                // over a separate row.
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(title)
                        .font(Typo.ui(12, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(2)
                        .help(titleHelp)
                    if let orderLine = shipment.orderLine {
                        Text(orderLine)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Chip(
                    text: statusText, tone: tone,
                    symbol: shipment.status == .delivered ? "checkmark.circle.fill" : nil,
                    filled: shipment.status == .outForDelivery)
            }
            // Conditional on the chip, not a row holding an empty `if`: an
            // always-present HStack still spends the VStack's spacing on both
            // sides of nothing.
            if let etaText {
                HStack(spacing: Space.scaled(7)) {
                    Chip(text: etaText, tone: etaTone)
                    Spacer(minLength: 0)
                }
            }
            if let note {
                Text(note)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .lineLimit(1)
            }
        }
        .padding(Space.scaled(9))
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(Palette.hairline.opacity(hovering && target != nil ? 0.85 : 0.5))
        )
        .contentShape(Rectangle())
        .onTapGesture { if let target { store.openThread(target, from: .sitrepCard) } }
        .warmsThreadOnHover(target)
        .onHover { over in
            hovering = over
            guard let cursor else { return }
            // Only clear the slot if it is still OURS: sliding card to card can
            // deliver the new card's enter before the old one's exit.
            if over {
                cursor.shipment = shipment.id
            } else if cursor.shipment == shipment.id {
                cursor.shipment = nil
            }
        }
        // A CONTEXT MENU rather than a chip or a swipe: the card's whole face is
        // already spoken for (the tap opens the email), and this rail is a VStack
        // of cards on both platforms, not a List — `swipeActions` would do
        // nothing here. Right click on the Mac, long press on the phone, one
        // gesture neither of the other two uses.
        .contextMenu {
            Button {
                Task { await store.clearShipment(shipment.id) }
            } label: {
                Label("Clear this package", systemImage: "xmark.circle")
            }
        }
        .opacity(shipment.status == .delivered ? 0.65 : 1)
    }
}

/// Carrier favicon, falling back to a package glyph for amazon/unknown (no clean
/// single domain) or a failed fetch.
private struct CarrierBadge: View {
    let carrier: Carrier
    @State private var image: PlatformImage?
    @State private var failed = false

    var body: some View {
        Group {
            if let image, !failed {
                Image(platformImage: image)
                    .resizable().interpolation(.high).aspectRatio(contentMode: .fit)
                    .frame(width: 20, height: 20)
            } else {
                Image(systemName: "shippingbox.fill")
                    .font(.system(size: 13))
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(width: 20, height: 20)
            }
        }
        .help(carrier.label)
        .task {
            guard let domain = carrier.faviconDomain, let url = SenderID.faviconURL(domain) else {
                failed = true
                return
            }
            image = await Self.knockedOut(FaviconLoader.shared.load(url: url, domain: domain), domain)
            failed = image == nil
        }
    }

    /// Per domain, so a rail of UPS cards pays for the flood fill once.
    @MainActor private static var knockouts: [String: PlatformImage] = [:]

    @MainActor private static func knockedOut(_ image: PlatformImage?, _ domain: String) -> PlatformImage? {
        guard let image else { return nil }
        if let done = knockouts[domain] { return done }
        let clear = FaviconMatte.clearingBackdrop(image) ?? image
        knockouts[domain] = clear
        return clear
    }
}

/// Carrier favicons come as a mark on a baked-in WHITE square (UPS, USPS), which
/// reads as a sticker on the card's tint and a lit tile in dark mode. This clears
/// the near-white pixels reachable from the border and nothing else, so white
/// INSIDE the mark (the USPS eagle) survives. A coloured square (DHL's yellow) is
/// the brand itself and is left alone, as is an icon that is already transparent.
enum FaviconMatte {
    /// How close to white a pixel must be to count as backdrop. Low enough to eat
    /// JPEG-ish noise in the square, high enough to spare a pale brand colour.
    private static let floor: UInt8 = 235

    static func clearingBackdrop(_ image: PlatformImage) -> PlatformImage? {
        #if os(macOS)
            guard let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
        #else
            guard let cg = image.cgImage else { return nil }
        #endif
        let w = cg.width, h = cg.height
        guard w > 0, h > 0,
            let ctx = CGContext(
                data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
            let base = ctx.data?.assumingMemoryBound(to: UInt8.self)
        else { return nil }
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
        let px = UnsafeMutableBufferPointer(start: base, count: w * h * 4)

        func isBackdrop(_ i: Int) -> Bool {
            let o = i * 4
            return px[o + 3] == 255 && px[o] >= floor && px[o + 1] >= floor && px[o + 2] >= floor
        }
        var seen = [Bool](repeating: false, count: w * h)
        var stack: [Int] = []
        for x in 0..<w { stack.append(x); stack.append((h - 1) * w + x) }
        for y in 0..<h { stack.append(y * w); stack.append(y * w + w - 1) }
        var cleared = 0
        while let i = stack.popLast() {
            guard !seen[i] else { continue }
            seen[i] = true
            guard isBackdrop(i) else { continue }
            for c in 0..<4 { px[i * 4 + c] = 0 }
            cleared += 1
            let x = i % w, y = i / w
            if x > 0 { stack.append(i - 1) }
            if x < w - 1 { stack.append(i + 1) }
            if y > 0 { stack.append(i - w) }
            if y < h - 1 { stack.append(i + w) }
        }
        // Nothing white on the rim: the icon was already transparent or its
        // square is a brand colour. Hand back the original untouched.
        guard cleared > 0, let out = ctx.makeImage() else { return nil }
        #if os(macOS)
            return NSImage(cgImage: out, size: image.size)
        #else
            return UIImage(cgImage: out, scale: image.scale, orientation: image.imageOrientation)
        #endif
    }
}

// MARK: - banking

/// Statements & transaction alerts, latest first: institution + kind tag +
/// masked account hint, amount right-aligned (a statement's amount is the TOTAL
/// balance the extractor pulled).
///
/// WINDOWED, not capped: the card shows the last 24 hours, or everything since
/// this zone was last SEEN, whichever reaches further back (SitrepWindow). The
/// old fixed "latest 8" held week-old rows forever — issue #82.
struct BankingZone: View {
    @Environment(AppStore.self) private var store
    private var records: [BankingRecord] { store.zones.banking }

    /// How many rows stand unfolded. The window decides what BELONGS; this only
    /// keeps a busy week from swallowing the rail — the rest sits behind the
    /// fold row.
    private static let collapsed = 8

    @State private var expanded = false

    @MainActor
    static func rows(_ store: AppStore) -> [BankingRecord] {
        let window = SitrepWindow.shared
        return store.zones.banking.filter { window.admits(Fmt.date($0.received_at)) }
    }

    var body: some View {
        let inWindow = Self.rows(store)
        let rows = expanded ? inWindow : Array(inWindow.prefix(Self.collapsed))
        ZoneCard(
            symbol: "building.columns", title: "Billing", count: inWindow.count,
            tint: Palette.accentInk
        ) {
            if inWindow.isEmpty {
                EmptyNote("No new bills, statements, or alerts.")
            } else {
                VStack(spacing: 1) {
                    ForEach(rows) { r in
                        Button {
                            store.openRecord(thread: r.thread_id, message: r.message_id)
                        } label: {
                            HStack(spacing: Space.scaled(7)) {
                                Text(institutionLabel(r))
                                    .font(Typo.rowSub)
                                    .foregroundStyle(Palette.inkDim)
                                    .lineLimit(1)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                // Autopay implies bill; one chip says both.
                                Chip(text: r.autopay == true ? "autopay" : r.kind.tag, tone: Palette.inkFaintest)
                                if let due = Fmt.deadlineChip(r.due) {
                                    Chip(text: due.text, tone: due.overdue ? Palette.danger : Palette.inkFaintest)
                                }
                                if r.amount != nil {
                                    Text(Fmt.usd(r.amount, currency: r.currency))
                                        .font(Typo.num(11, weight: .medium))
                                        .foregroundStyle(Palette.accentInk)
                                        .lineLimit(1)
                                        .fixedSize()
                                }
                            }
                            .padding(.horizontal, RecordRowStyle.hPadding)
                            .padding(.vertical, Space.scaled(5))
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(RecordRowStyle())
                        .recordRowBleed()
                        .warmsThreadOnHover(r.thread_id)
                    }
                    if inWindow.count > Self.collapsed {
                        ZoneExpander(overflow: inWindow.count - Self.collapsed, expanded: expanded) {
                            withAnimation(ZoneExpander.animation) { expanded.toggle() }
                        }
                    }
                }
            }
        }
        .task { await store.refreshZones() }
        // THE SEEN-SIGNAL. The stamp belongs to this zone, not to the app: the
        // Mac mounts it on the sitrep page, the phone on the Quick Look tab, and
        // a session spent entirely in Mail must not clear a card nobody looked
        // at. Appearance and disappearance are both reported — the window's
        // clearing rides on them.
        .onAppear { SitrepWindow.shared.surfaceAppeared() }
        .onDisappear { SitrepWindow.shared.surfaceDisappeared() }
    }

    private func institutionLabel(_ r: BankingRecord) -> String {
        let base = r.institution ?? r.from_addr.map(SenderID.displayName) ?? "bank"
        return r.account_hint.map { "\(base) \($0)" } ?? base
    }

}

// MARK: - receipts

/// Money already paid: merchant (left), total (right). Only TODAY's receipts
/// show — a fresh daily digest rather than a growing ledger.
struct ReceiptsZone: View {
    @Environment(AppStore.self) private var store
    private var receipts: [Receipt] { store.zones.receipts }

    private var rows: [Receipt] { Self.rows(store) }

    @MainActor
    static func rows(_ store: AppStore) -> [Receipt] {
        store.zones.receipts.filter { Fmt.isToday($0.received_at) }
    }

    var body: some View {
        ZoneCard(
            symbol: "receipt", title: "Receipts", count: rows.count, tint: Palette.positive
        ) {
            if rows.isEmpty {
                EmptyNote("No receipts today.")
            } else {
                VStack(spacing: 1) {
                    ForEach(rows) { r in
                        Button {
                            store.openRecord(thread: r.thread_id, message: r.message_id)
                        } label: {
                            HStack(spacing: Space.scaled(7)) {
                                Text(SenderCache.resolved(r.senderString).displayName)
                                    .font(Typo.rowSub)
                                    .foregroundStyle(Palette.inkDim)
                                    .lineLimit(1)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .help(r.from_addr)
                                if r.amount != nil {
                                    Text(Fmt.usd(r.amount, currency: r.currency))
                                        .font(Typo.num(11, weight: .medium))
                                        .foregroundStyle(Palette.positive)
                                }
                            }
                            .padding(.horizontal, RecordRowStyle.hPadding)
                            .padding(.vertical, Space.scaled(5))
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(RecordRowStyle())
                        .recordRowBleed()
                        .warmsThreadOnHover(r.thread_id)
                    }
                }
            }
        }
        .task { await store.refreshZones() }
    }

}

// MARK: - reading

/// THE RULE-ONBOARDING SURFACE: recurring marketing senders; a ruled sender is
/// marked only by the card's accent border (rules are edited in the Rules view).
/// Clicking opens that sender's latest email with their whole window as the
/// viewer's h/l queue; `e` while hovering marks it done.
///
/// THE FETCH LIVES IN SitrepView, NOT HERE: SwiftUI gives `EmptyView` no
/// lifetime, so `.task`/`.onAppear` on a view that resolves to nothing never
/// fire. Any zone that can hide itself must be fed by a parent that cannot.
/// NOTHING HERE IS A CLOSURE OR A BINDING, deliberately: both compare unequal on
/// every render, so a zone that took them could never be skipped when the
/// dashboard redrew — and skipping it is what keeps a scroll from re-measuring
/// every card. `[ReadingSender]` is Equatable and the cursor is one stable
/// reference, so SwiftUI can prove this subtree unchanged and leave it alone.
struct ReadingZone: View {
    @Environment(AppStore.self) private var store
    let senders: [ReadingSender]
    let cursor: SitrepCursor

    /// Narrowest a card may be drawn; the grid fits as many equal columns of at
    /// least this width as the zone allows.
    #if os(iOS)
        // The phone's zone is only ~330pt across, where 190 buys exactly one
        // column and a page of full-width cards; 140 pins it to a two-up.
        private static var cardMinimum: CGFloat { Space.scaled(140) }
    #else
        private static var cardMinimum: CGFloat { Space.scaled(190) }
    #endif
    /// Gutter, both axes.
    private static var gap: CGFloat { Space.s10 }
    /// How many cards stand unfolded. The feed decides what BELONGS; this only
    /// keeps a busy week from burying the records under a wall of cards. The
    /// rest sits behind the same quiet expander For-your-eyes uses.
    /// See `BoardFold.reading`, which the launch warms read too.
    private static let collapsed = BoardFold.reading

    @State private var expanded = false

    var body: some View {
        // Always on the board, empty or not: a zone that vanishes reads as a
        // missing feature, not as "nothing this week".
        ZoneCard(
            symbol: "envelope.open", title: "Reading", count: senders.count,
            subtitle: "newsletters, announcements, and offers",
            trailing: actions
        ) {
            if senders.isEmpty {
                EmptyNote("Nothing to read yet.")
            } else {
                grid
            }
        }
    }

    /// The zone's door, smart clear, on both platforms. Gone with the mail:
    /// on an empty zone there is nothing for it to do. Not in the practice
    /// inbox, whose senders are fixtures.
    private var actions: AnyView? {
        guard !senders.isEmpty, !RehearsalMode.isEnabled else { return nil }
        return AnyView(ReadingActions())
    }

    /// NON-LAZY on purpose: a LazyVGrid rebuilds cards as they cross the
    /// viewport, and each rebuild costs a hero lookup and a fresh `.task`. A
    /// week of recurring senders is bounded (the feed fetches 200 messages, and
    /// the zone folds at `collapsed`), so holding all of it is cheaper.
    private var grid: some View {
        let overflow = senders.count - Self.collapsed
        return VStack(alignment: .leading, spacing: 0) {
            AdaptiveGrid(minimum: Self.cardMinimum, spacing: Self.gap) { cards }
            if overflow > 0 { expander(overflow) }
        }
    }

    @ViewBuilder private var cards: some View {
        ForEach(expanded ? senders : Array(senders.prefix(Self.collapsed))) { rs in
            ReadingCard(sender: rs, cursor: cursor)
        }
    }

    /// The board's one expander, so every zone folds the same way.
    private func expander(_ overflow: Int) -> some View {
        ZoneExpander(overflow: overflow, expanded: expanded) {
            withAnimation(ZoneExpander.animation) { expanded.toggle() }
        }
    }
}

/// SMART CLEAR, in Reading's corner. A leaf of its own so the zone above
/// stays free of closures and of the store's working flags:
/// a smart clear starting must not re-measure every card in the grid.
private struct ReadingActions: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        HStack(spacing: Space.s6) {
            ZonePill(
                symbol: "checkmark.rectangle.stack", label: "smart clear",
                help: "Clear what has expired and what you never open. You can undo it.",
                accessibilityName: "Smart clear",
                working: store.smartClearWorking, workingLabel: "tidying up"
            ) {
                #if os(iOS)
                    Haptics.tick()
                #endif
                Task { await store.runSmartClear() }
            }
        }
    }
}

/// Fetch + derive for the reading zone. Free-standing so the always-mounted
/// SitrepView can own it.
enum ReadingFeed {
    /// Membership comes directly from the canonical Reading destination.
    private static let fetchLimit = 200

    static func load() async -> [ReadingSender]? {
        do {
            async let feed = APIClient.shared.getFeed(destination: "reading", limit: fetchLimit)
            async let rules = APIClient.shared.listRules()
            let (page, preferences) = try await (feed, rules)
            let content = Set(page.items.filter(\.isReadingContent).map(\.message_id))
            return Reading.derive(
                updates: page.items.map(\.readingRow), rules: preferences, content: content)
        } catch {
            // Preserve the last successful reading feed on a transient error.
            return nil
        }
    }
}

private struct ReadingCard: View {
    @Environment(AppStore.self) private var store
    let sender: ReadingSender
    let cursor: SitrepCursor

    @State private var hovering = false

    private var summaryText: String {
        Fmt.truncate(Reading.cleanSummary(sender.summary), 90)
    }

    /// How often this sender wrote, SPELLED OUT on the Mac and a bare multiplier
    /// on the phone. The label is `fixedSize` — it never gives width back — and a
    /// phone card is half a Mac column, so "3 this week" would eat the row and
    /// truncate the sender name to a couple of characters. The window is already
    /// named by the zone itself.
    private var countLabel: String {
        #if os(iOS)
            return "\(sender.count)×"
        #else
            return "\(sender.count) emails"
        #endif
    }

    var body: some View {
        Button(action: open) {
            // Hero left as a FIXED square, text right: every card in the grid
            // keeps the same height whether or not its sender ships art.
            HStack(alignment: .top, spacing: Space.scaled(9)) {
                ReadingHero(
                    threadId: sender.latestThreadId, sender: sender.sender,
                    address: sender.address)
                VStack(alignment: .leading, spacing: Space.s6) {
                    HStack(spacing: Space.s6) {
                        Text(SenderCache.resolved(sender.sender).displayName)
                            .font(Typo.ui(12, weight: .semibold))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                        Spacer(minLength: Space.s4)
                        Text(countLabel)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .fixedSize()
                    }
                    if !sender.summary.isEmpty {
                        Text(summaryText)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                            .lineLimit(2)
                            .multilineTextAlignment(.leading)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(Space.scaled(9))
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .fill(
                        sender.isContent
                            ? Palette.readingContent.opacity(hovering ? 1 : 0.8)
                            : Palette.hairline.opacity(hovering ? 0.85 : 0.5))
            )
            .overlay(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .strokeBorder(
                        sender.rule != nil ? Palette.accent.opacity(0.35) : .clear,
                        lineWidth: 1)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // Only seen when a removal is ANIMATED, which is the smart clear
        // sweep: the tile slides a little way off and fades instead of
        // blinking out. An ordinary `e` is not animated and is unchanged.
        .transition(
            .asymmetric(
                insertion: .opacity,
                removal: .opacity.combined(with: .offset(x: 26)).combined(
                    with: .scale(scale: 0.94))))
        .onHover { over in
            hovering = over
            // Nothing READS this during a render — only the `e` handler, at
            // key-press time — so this write is free even at scroll frequency.
            cursor.reading = over ? sender.address : nil
        }
    }

    private func open() { store.openReading(sender) }

}

/// Hero thumbnail mined from the latest email's sanitized html via the shared
/// thread cache. Gated on the remote-images pref: with images "on demand" NO
/// network fetch happens for unopened mail (see docs/SECURITY.md §3).
private struct ReadingHero: View {
    let threadId: String
    let sender: String
    /// The grouping address: keys the sender's art on disk, which is what
    /// paints this card on its first frame after a relaunch or a cleared stack.
    let address: String
    @State private var resolved: HeroCache.Hero?

    /// Side of the square thumb. SMALLER ON THE PHONE, because the card is: a
    /// two-up phone card is roughly 160pt wide, and a 54pt square plus its
    /// gutter takes a third of that away from the sender's name, which is the
    /// one thing on the card you actually pick a sender by. The art is a
    /// recognition cue, and it still works at 40.
    #if os(iOS)
        private static let side: CGFloat = 40
    #else
        private static let side: CGFloat = 54
    #endif

    /// TUNABLE width:height cap on how wide a hero may be DRAWN. Wider art is
    /// cropped to exactly this ratio rather than letterboxed whole: a 728x90
    /// masthead fitted into the square becomes an unreadable few-point strip.
    private static let maxAspect: CGFloat = 7

    /// Breathing room around a mark cut out of its banner (`Hero.isMark`). The
    /// crop is tight to the ink, and a logo touching the square's edges reads
    /// as a cropped photo of a logo. A fraction of `side` so it holds on the
    /// phone's smaller square.
    private static let markInset: CGFloat = side * 0.08

    /// Falling back to the cache INSIDE the read keeps a recycled card from
    /// flashing empty: `@State` is gone the moment the card is dropped, the
    /// cached hero is not, so it repaints on the very first frame. Before the
    /// thread has a verdict that is the SENDER's last art, off disk.
    private var hero: HeroCache.Hero? {
        resolved ?? HeroCache.shared.cached(threadId, address: address)
    }

    var body: some View {
        content
            .task(id: threadId) {
                guard !RehearsalMode.isEnabled else { return }
                // Already answered — `hero` is drawing that answer, and resolving
                // again costs a second body pass. The guard is INSIDE the task,
                // not around it: a conditional modifier would change this view's
                // identity as the verdict lands.
                // `learn` still files the warmer's verdict under this sender.
                guard !HeroCache.shared.isResolved(threadId) else {
                    HeroCache.shared.learn(threadId, address: address)
                    return
                }
                resolved = await HeroCache.shared.resolve(threadId, address: address)
            }
    }

    /// The no-hero branch is a REAL zero-size leaf, not an implicit EmptyView:
    /// `.task` on a view that resolves to nothing never fires, and this view
    /// starts with no image, so the fetch that produces one would never run.
    @ViewBuilder
    private var content: some View {
        if let brand = RehearsalNewsletterBrand.matching(sender) {
            RehearsalNewsletterLogo(brand: brand, size: Self.side)
        } else if let hero {
            art(hero)
                .frame(width: Self.side, height: Self.side)
                // The rest of the square, in the art's own dominant colour, or
                // the neutral well when sampling failed — which must stay
                // distinguishable from a white sample.
                .background(hero.fill ?? Palette.canvas.opacity(0.7))
                .clipShape(RoundedRectangle(cornerRadius: Radius.concentric(Radius.well, inset: Space.scaled(9)), style: .continuous))
        } else {
            // Zero in BOTH axes: a zero-height-only placeholder would still
            // collect the HStack's spacing and indent the text.
            Color.clear.frame(width: 0, height: 0)
        }
    }

    /// The art itself, unclipped and unbacked — the square frame and the fill
    /// behind it belong to the caller.
    @ViewBuilder
    private func art(_ hero: HeroCache.Hero) -> some View {
        let inset = hero.isMark ? Self.markInset : 0
        if ultraWide(hero.image) {
            // Past `maxAspect`, draw at the capped ratio and let the ENDS crop:
            // `.fill` overflows on the long axis and `.clipped` keeps the middle,
            // which is where the wordmark lives.
            let width = Self.side - inset * 2
            Image(platformImage: hero.image)
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: .fill)
                .frame(width: width, height: width / Self.maxAspect)
                .clipped()
        } else {
            // A mark is always drawn WHOLE: it was cut to the ink, so there is
            // no margin left for a fill-crop to spend.
            Image(platformImage: hero.image)
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: hero.isMark ? .fit : fit(hero.image))
                .padding(inset)
        }
    }

    /// Wider than the cap, and guarded against a zero-height decode.
    private func ultraWide(_ image: PlatformImage) -> Bool {
        image.size.height > 0 && image.size.width > image.size.height * Self.maxAspect
    }

    /// Fit by shape: tall/square heroes crop to fill (photos survive a crop),
    /// WIDE art letterboxes — filling a square with a wordmark would leave a
    /// meaningless slice of two letters. Only reached for art within `maxAspect`.
    private func fit(_ image: PlatformImage) -> ContentMode {
        image.size.width > image.size.height * 1.2 ? .fit : .fill
    }
}

// MARK: - adaptive grid

/// `LazyVGrid(columns: [.adaptive(minimum:spacing:)])` geometry WITHOUT the
/// laziness: as many equal-width columns as fit at `minimum`, each row as tall as
/// its tallest card with shorter cards centred (matching `GridItem`'s default
/// `.center`). A `Layout` sees every subview on every pass, so nothing is created
/// or destroyed by scrolling — that is the whole point.
struct AdaptiveGrid: Layout {
    /// Narrowest a column may be. Columns then stretch to divide the width
    /// evenly, matching `.adaptive`'s default unbounded maximum.
    var minimum: CGFloat
    var spacing: CGFloat

    /// Column geometry, kept BETWEEN the sizing pass and the placement pass.
    ///
    /// Without this every layout measured every card TWICE — `sizeThatFits` and
    /// `placeSubviews` each called `metrics`, which maps `sizeThatFits` over all
    /// of them — and a card's height is a two-line text layout, not a constant.
    /// Keyed on the width it was measured at, so a window resize invalidates it
    /// and nothing else needs to.
    struct Cache {
        var measuredAt: CGFloat?
        var measuredCount = 0
        var metrics: Metrics?
    }

    func makeCache(subviews: Subviews) -> Cache { Cache() }

    /// The subviews changed, so the heights we hold describe cards that are gone.
    /// `metrics` ALSO re-checks the count on every read: this is the documented
    /// invalidation hook, but a stale row height is a visibly broken grid, and
    /// the count check costs nothing to carry.
    func updateCache(_ cache: inout Cache, subviews: Subviews) {
        cache.measuredAt = nil
        cache.metrics = nil
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) -> CGSize
    {
        let metrics = metrics(width: proposal.width, subviews: subviews, cache: &cache)
        return CGSize(width: metrics.width, height: metrics.height)
    }

    func placeSubviews(
        in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache
    ) {
        let metrics = metrics(width: bounds.width, subviews: subviews, cache: &cache)
        var y = bounds.minY
        var index = 0
        for rowHeight in metrics.rowHeights {
            for column in 0..<metrics.columns where index < subviews.count {
                let height = metrics.heights[index]
                subviews[index].place(
                    at: CGPoint(
                        x: bounds.minX + CGFloat(column) * (metrics.columnWidth + spacing),
                        y: y + (rowHeight - height) / 2),
                    proposal: ProposedViewSize(width: metrics.columnWidth, height: height))
                index += 1
            }
            y += rowHeight + spacing
        }
    }

    struct Metrics {
        var columns: Int
        var columnWidth: CGFloat
        var heights: [CGFloat]
        var rowHeights: [CGFloat]
        var width: CGFloat
        var height: CGFloat
    }

    /// Cached geometry for this width, measuring only on a miss.
    private func metrics(width proposed: CGFloat?, subviews: Subviews, cache: inout Cache)
        -> Metrics
    {
        let width = (proposed?.isFinite == true) ? max(0, proposed!) : minimum
        if let hit = cache.metrics, cache.measuredAt == width,
            cache.measuredCount == subviews.count
        {
            return hit
        }
        let measured = measure(width: width, subviews: subviews)
        cache.measuredAt = width
        cache.measuredCount = subviews.count
        cache.metrics = measured
        return measured
    }

    /// Column count is the most that fit at `minimum` with a gutter between each;
    /// leftover is split evenly. A nil or infinite proposal is a SwiftUI sizing
    /// probe, not a width — answer it with one column at `minimum`, because
    /// feeding infinity into the column arithmetic traps on the `Int` conversion.
    private func measure(width: CGFloat, subviews: Subviews) -> Metrics {
        let columns = max(1, Int((width + spacing) / (minimum + spacing)))
        let columnWidth = max(0, (width - spacing * CGFloat(columns - 1)) / CGFloat(columns))
        let heights = subviews.map {
            $0.sizeThatFits(ProposedViewSize(width: columnWidth, height: nil)).height
        }
        var rowHeights: [CGFloat] = []
        var index = 0
        while index < heights.count {
            let row = heights[index..<min(index + columns, heights.count)]
            rowHeights.append(row.max() ?? 0)
            index += columns
        }
        let total = rowHeights.reduce(0, +) + spacing * CGFloat(max(0, rowHeights.count - 1))
        return Metrics(
            columns: columns, columnWidth: columnWidth, heights: heights, rowHeights: rowHeights,
            width: width, height: total)
    }
}

// MARK: - shared bits

/// The rail's empty state — present rather than hidden.
struct EmptyNote: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaintest)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, Space.s2)
    }
}

/// THE BOARD'S ONE "N MORE" FOLD: For your eyes, Reading and Billing all
/// open and close their overflow with this, so the three read as the same
/// gesture. A quiet glass pill on the leading edge, lowercase like the rest
/// of the board's small print. The caller owns the state and the animation.
struct ZoneExpander: View {
    let overflow: Int
    let expanded: Bool
    let action: () -> Void

    /// Shared so a button press and arrowing off the end open the same way.
    static let animation: Animation = .smooth(duration: 0.28)

    var body: some View {
        Button(action: action) {
            Text(expanded ? "show less" : "\(overflow) more")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .padding(.horizontal, Space.scaled(11))
                .padding(.vertical, Space.s4)
        }
        .buttonStyle(.glass)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, Space.s8)
    }
}

/// A record row: no chrome at rest, a soft wash on hover. The wash is the
/// board's one row hover (the For-your-eyes rows paint the same tone).
struct RecordRowStyle: ButtonStyle {
    @State private var hovering = false

    /// Room for the hover wash, which only a pointer raises. A phone has no
    /// hover, so there it was pure inset, drawing every record a step inside
    /// its zone's title: on iOS the words sit on the title's line.
    static var hPadding: CGFloat {
        #if os(macOS)
            Space.scaled(7)
        #else
            0
        #endif
    }

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(
                RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                    .fill(hovering ? Palette.hairline.opacity(SelectionTone.hover) : .clear)
            )
            .opacity(configuration.isPressed ? 0.7 : 1)
            .onHover { hovering = $0 }
    }
}

extension View {
    /// A sitrep record row pulled out by its own inset (DeskRow's idea): the
    /// words land on the zone title's line and the hover wash spends the
    /// card's padding instead of indenting the text.
    func recordRowBleed() -> some View {
        padding(.horizontal, -RecordRowStyle.hPadding)
    }

    /// The pointer resting on a record is the likeliest next click: cache its
    /// thread and, a beat later, render its newest message, as a board row
    /// does (SitrepView's row hover). Without it a record opened cold.
    func warmsThreadOnHover(_ threadId: String?) -> some View {
        onHover { over in
            guard over, let threadId, !threadId.isEmpty else { return }
            ThreadPrefetch.shared.prefetch(threadId, fresh: ThreadPrefetch.warmFresh)
            FramePrewarmer.shared.nudge(threadId)
        }
    }
}
