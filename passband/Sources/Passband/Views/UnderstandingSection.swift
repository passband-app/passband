// UNDERSTANDING, at the bottom of the Usage page: who the owner's mail says
// they are (docs/UNDERSTANDING.md). Three cards under one heading:
//
//   * People: every correspondent on one plot, mail per month across a log
//     axis and the ladder up it (answered on top, read in the middle, ignored
//     at the floor, too new to judge in a strip on the left). Hover says who
//     and one sentence from their numbers; the table twin says all of it.
//   * Your people: the eight answered most strongly, or the eight fading
//     hardest, each a click from the contact card the reader opens.
//   * What Passband knows: the memory entries, behind the `understanding`
//     hidden feature, with confirm and delete.
//
// THE LEDGER IS NOT THE WINDOW. The page's picker scopes the charts above;
// this section is the ledger's whole span, read once per visit, and its notes
// say how many days that is. A daemon older than the ledger answers 404 and
// the whole section stays away; with the feature off, memory 404s and only
// its card does. The arithmetic and every sentence are UnderstandingSeries.

import Charts
import SwiftUI

struct UnderstandingSection: View {
    @Environment(AppStore.self) private var store

    /// The relationships page, merged from the three sorts the chart needs
    /// both ends of.
    @State private var relationships: RelationshipsResponse?
    @State private var series: UnderstandingSeries?
    @State private var memory: MemoryResponse?
    /// The daemon has no ledger: draw nothing at all.
    @State private var absent = false
    @State private var error: String?
    /// Previews hand their data in and skip the fetch.
    private let staged: Bool

    /// Where the page scrolls to put this section on screen.
    static let anchor = "understanding"

    init() { staged = false }

    /// A section already loaded, for previews.
    init(relationships: RelationshipsResponse, memory: MemoryResponse?, now: Date = Date()) {
        _relationships = State(initialValue: relationships)
        _series = State(initialValue: UnderstandingSeries(relationships, now: now))
        _memory = State(initialValue: memory)
        staged = true
    }

    var body: some View {
        Group {
            if absent {
                EmptyView()
            } else {
                VStack(alignment: .leading, spacing: Space.s16) {
                    heading
                    if let series {
                        PeopleChartCard(series: series)
                        YourPeopleCard(series: series).id("your-people")
                    } else if let error {
                        SectionCard(label: "Understanding") {
                            Text(error).font(Typo.micro).foregroundStyle(Palette.danger)
                        }
                    } else {
                        SectionCard(label: "Understanding") { EmptyNote("reading the ledger…") }
                    }
                    // The flag and a 200 are the whole gate. `enabled` is the
                    // daemon saying its ledger is under two weeks old, and the
                    // card shows that as a note over whatever the owner has
                    // already stated, not by hiding the card.
                    if let memory, showsMemory {
                        KnowsCard(memory: memory, onChange: { self.memory = $0 }).id("knows")
                    }
                }
                .padding(.top, Space.s16)
            }
        }
        .task { await loadLedger() }
        // Again whenever the flag moves: the first stats can land after the
        // page does, and an operator can turn the feature on mid-visit.
        .task(id: showsMemory) { await loadMemory() }
    }

    /// The flag is the gate, and a 404 under it is the same answer. A staged
    /// preview has no stats to ask.
    private var showsMemory: Bool { staged || store.hasFeature(.understanding) }

    private var heading: some View {
        VStack(alignment: .leading, spacing: Space.scaled(3)) {
            Text("Understanding")
                .font(Typo.ui(20, weight: .semibold))
                .foregroundStyle(Palette.ink)
            Text("Who your mail says you are, from what you answer, read and skip.")
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkFaint)
        }
        .padding(.bottom, Space.s2)
    }

    private func loadLedger() async {
        guard !staged else { return }
        do {
            // Three sorts, merged by address: the chart needs both ends of the
            // ledger, and one sort's top 50 is only one end of it.
            async let replies = APIClient.shared.getRelationships(limit: 50, sort: "replies")
            async let fatigue = APIClient.shared.getRelationships(limit: 50, sort: "fatigue")
            async let recent = APIClient.shared.getRelationships(limit: 50, sort: "recent")
            let pages = try await [replies, fatigue, recent]
            var seen = Set<String>()
            var rows: [RelationshipRow] = []
            for row in pages.flatMap(\.rows) where seen.insert(row.addr.lowercased()).inserted {
                rows.append(row)
            }
            let merged = RelationshipsResponse(
                coverage_days: pages[0].coverage_days, computed_at: pages[0].computed_at, rows: rows)
            relationships = merged
            series = UnderstandingSeries(merged)
            error = nil
        } catch let e as APIError where e.status == 404 {
            absent = true
            return
        } catch is CancellationError {
            return
        } catch {
            self.error = "Couldn't read the relationship ledger."
        }
    }

    private func loadMemory() async {
        guard !staged else { return }
        guard store.hasFeature(.understanding) else {
            memory = nil
            return
        }
        // Off on the daemon (or older than the routes) is a 404, and the
        // card simply is not drawn.
        memory = try? await APIClient.shared.getMemory()
    }
}

// MARK: - colours

extension PeopleRegion {
    var color: Color {
        switch self {
        case .answer: Palette.chartSignal
        case .read: Palette.chartIn
        case .ignore: Palette.chartNoise
        case .arrived: Palette.chartOut
        }
    }
}

// MARK: - People

private struct PeopleChartCard: View {
    let series: UnderstandingSeries
    @State private var hovered: PeoplePoint?

    private static let height: CGFloat = 300
    private static let ticks: [Double] = [1, 3, 10, 30, 100, 300, 1000]
    /// The ladder with a margin above and below it.
    private static let yDomain: ClosedRange<Double> = -0.06...1.06

    var body: some View {
        ChartCard(
            title: "People", note: UnderstandingText.coverageNote(days: series.coverageDays),
            legend: [
                LegendKey(label: "answered", color: PeopleRegion.answer.color, symbol: .circle),
                LegendKey(label: "read", color: PeopleRegion.read.color, symbol: .diamond),
                LegendKey(label: "ignored", color: PeopleRegion.ignore.color, symbol: .diamond),
                LegendKey(label: "new", color: PeopleRegion.arrived.color, symbol: .circle),
            ]
        ) {
            VStack(alignment: .leading, spacing: Space.s8) {
                if series.points.isEmpty {
                    EmptyNote("Nobody in the ledger yet.")
                        .frame(height: Self.height, alignment: .center)
                } else {
                    chart.frame(height: Self.height)
                }
                Text("Higher means closer: replies sit above the middle line, reads below it. Further right is more mail a month, and dot size is how much has arrived.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .fixedSize(horizontal: false, vertical: true)
            }
        } table: {
            DayTable(
                columns: ["who", "where", "a month", "replied", "opened", "replies come"],
                rows: series.points.sorted { $0.y > $1.y }.map { p in
                    let r = p.row
                    return [
                        p.name, p.region.title.lowercased(),
                        p.region == .arrived ? "new" : UnderstandingText.perMonth(p.x),
                        r.inbound_threads > 0 ? "\(UsageText.count(r.replied_threads)) of \(UsageText.count(r.inbound_threads))" : "none",
                        r.attention_rows > 0 ? "\(UsageText.count(r.opened_count)) of \(UsageText.count(r.attention_rows))" : "not measured",
                        UnderstandingText.replyTime(r.median_reply_ms) ?? "",
                    ]
                },
                height: Self.height,
                monoFirst: false)
        }
    }

    private var maxCount: Double { Double(series.points.map(\.row.received_count).max() ?? 1) }

    private func size(_ p: PeoplePoint) -> CGFloat {
        CGFloat(36 + 380 * (Double(p.row.received_count) / max(maxCount, 1)).squareRoot())
    }

    private var chart: some View {
        let lo = series.xDomain.lowerBound, hi = series.xDomain.upperBound
        let floor = UnderstandingSeries.axisFloor
        return Chart {
            // THE REGIONS, as quiet fills the points sit on. Each carries its
            // own label in its top corner, so the plot needs no key for them.
            // The outer bands run into the margin, so a dot at the very top
            // or the floor is drawn whole and still sits in its band.
            region(.arrived, x: lo...floor, y: Self.yDomain)
            region(.answer, x: floor...hi, y: UnderstandingSeries.answerLine...Self.yDomain.upperBound)
            region(.read, x: floor...hi, y: UnderstandingSeries.ignoreLine...UnderstandingSeries.answerLine)
            region(.ignore, x: floor...hi, y: Self.yDomain.lowerBound...UnderstandingSeries.ignoreLine)

            ForEach(series.points) { p in
                PointMark(x: .value("Mail a month", p.x), y: .value("Closeness", p.y))
                    .symbol(symbol(p))
                    .symbolSize(size(p))
                    .foregroundStyle(p.region.color.opacity(dimmed(p) ? 0.35 : 0.9))
            }
            if let h = hovered {
                PointMark(x: .value("Mail a month", h.x), y: .value("Closeness", h.y))
                    .symbol(symbol(h))
                    .symbolSize(size(h) + 160)
                    .foregroundStyle(h.region.color.opacity(0.22))
                    .annotation(
                        position: h.x > sqrt(lo * hi) ? .leading : .trailing, alignment: .center,
                        spacing: Space.s10,
                        overflowResolution: .init(x: .fit(to: .plot), y: .fit(to: .plot))
                    ) {
                        PersonReadout(point: h)
                    }
            }
        }
        .chartXScale(domain: lo...hi, type: .log)
        .chartYScale(domain: Self.yDomain)
        .chartLegend(.hidden)
        .chartYAxis(.hidden)
        .chartXAxisLabel(alignment: .trailing) {
            Text("mail a month").font(Typo.micro).foregroundStyle(Palette.inkFaint)
        }
        .chartXAxis {
            AxisMarks(values: Self.ticks.filter { $0 >= floor && $0 <= hi }) { value in
                AxisGridLine().foregroundStyle(Palette.hairline)
                AxisValueLabel {
                    if let v = value.as(Double.self) {
                        Text(UnderstandingText.perMonth(v))
                    }
                }
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
            }
        }
        .chartOverlay { proxy in
            GeometryReader { geo in
                Rectangle().fill(.clear).contentShape(Rectangle())
                    .onContinuousHover { phase in
                        switch phase {
                        case .active(let at):
                            // A staged readout holds still for its screenshot.
                            guard FixtureMode.stageValue("hover") == nil else { return }
                            hovered = nearest(at, proxy: proxy, geo: geo)
                        case .ended:
                            guard FixtureMode.stageValue("hover") == nil else { return }
                            hovered = nil
                        }
                    }
            }
        }
        .animation(.smooth(duration: 0.18), value: hovered?.id)
        .onAppear {
            if let addr = FixtureMode.stageValue("hover") {
                hovered = series.points.first { $0.id == addr }
            }
        }
    }

    private func dimmed(_ p: PeoplePoint) -> Bool {
        guard let hovered else { return false }
        return hovered.id != p.id
    }

    private func symbol(_ p: PeoplePoint) -> BasicChartSymbolShape {
        switch p.measure {
        case .replies: .circle
        case .reads, .none: p.region == .arrived ? .circle : .diamond
        }
    }

    @ChartContentBuilder
    private func region(_ r: PeopleRegion, x: ClosedRange<Double>, y: ClosedRange<Double>) -> some ChartContent {
        RectangleMark(
            xStart: .value("from", x.lowerBound), xEnd: .value("to", x.upperBound),
            yStart: .value("low", y.lowerBound), yEnd: .value("high", y.upperBound)
        )
        .foregroundStyle(r.color.opacity(r == .ignore ? 0.10 : r == .arrived ? 0.05 : 0.07))
        .annotation(position: .overlay, alignment: .topLeading, spacing: 0) {
            Text(r.title)
                .font(Typo.micro)
                // The ignore band's own colour is the de-emphasis gray, which
                // is the point of the dots and too quiet for words: its label
                // takes the ink the other two labels read at.
                .foregroundStyle(r == .ignore ? Palette.inkDim : r.color.opacity(0.95))
                .padding(.horizontal, Space.s8)
                .padding(.vertical, Space.s6)
                .lineLimit(r == .arrived ? 2 : 1)
                .frame(maxWidth: r == .arrived ? Typo.measure(64) : nil, alignment: .leading)
                .fixedSize(horizontal: r != .arrived, vertical: true)
        }
    }

    /// The point under the pointer, by screen distance: on a log axis with
    /// sized dots, "nearest in data units" is not what the eye means.
    private func nearest(_ at: CGPoint, proxy: ChartProxy, geo: GeometryProxy) -> PeoplePoint? {
        guard let anchor = proxy.plotFrame else { return nil }
        let origin = geo[anchor].origin
        return UnderstandingSeries.nearest(series.points, to: (at.x, at.y), radius: 26) { p in
            proxy.position(for: (x: p.x, y: p.y)).map { (origin.x + $0.x, origin.y + $0.y) }
        }
    }
}

/// The hover card: who, which region, and the sentence. The page's readout
/// chrome, the whole border lit evenly.
private struct PersonReadout: View {
    let point: PeoplePoint

    var body: some View {
        VStack(alignment: .leading, spacing: Space.scaled(3)) {
            Text(point.name)
                .font(Typo.ui(12, weight: .semibold))
                .foregroundStyle(Palette.ink)
                .lineLimit(1)
            HStack(spacing: Space.s6) {
                Circle().fill(point.region.color).frame(width: 6, height: 6)
                Text(point.region.title)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
            }
            Text(Fmt.capitalizingFirst(point.sentence) + ".")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkDim)
                .frame(maxWidth: Typo.measure(240), alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, Space.scaled(10))
        .padding(.vertical, Space.s8)
        .background(
            RoundedRectangle(cornerRadius: Radius.chip, style: .continuous).fill(Palette.readerBackground)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Radius.chip, style: .continuous)
                .strokeBorder(Palette.hairlineStrong, lineWidth: 0.5)
        )
        .shadow(color: .black.opacity(0.14), radius: 8, y: 3)
        .fixedSize()
    }
}

// MARK: - Your people

private struct YourPeopleCard: View {
    enum Tab: Hashable { case answered, fading }

    let series: UnderstandingSeries
    @State private var tab: Tab = FixtureMode.stageValue("tab") == "fading" ? .fading : .answered

    private static let tabs: [(value: Tab, label: String)] = [
        (.answered, "Answered"), (.fading, "Fading"),
    ]

    private var bars: [PersonBar] { tab == .answered ? series.answered : series.fading }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s12) {
            HStack(spacing: Space.s12) {
                Text("Your people").font(Typo.zoneTitle).foregroundStyle(Palette.ink)
                Text(tab == .answered ? "by how surely you answer" : "by how little gets read")
                    .font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                Spacer(minLength: Space.s8)
                GlassSegmented(options: Self.tabs, selection: $tab)
            }
            if bars.isEmpty {
                EmptyNote(
                    tab == .answered
                        ? "No replies in the ledger yet."
                        : "Nothing is fading. Everything that arrives gets read.")
            } else {
                VStack(spacing: 0) {
                    ForEach(Array(bars.enumerated()), id: \.element.id) { i, bar in
                        if i > 0 {
                            Rectangle().fill(Palette.hairline).frame(height: 0.5)
                        }
                        PersonBarRow(
                            bar: bar, tint: tab == .answered ? PeopleRegion.answer.color : Palette.warn,
                            showsShare: tab == .answered)
                    }
                }
            }
            Text("Click a name for the contact card.")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
        }
        .zonePadding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.glassTint)
        .animation(.smooth(duration: 0.22), value: tab)
    }
}

private struct PersonBarRow: View {
    let bar: PersonBar
    let tint: Color
    /// The reply share as a number. Off on the fading tab, where the bar is
    /// a fatigue score and a percent beside "opened 1 of 23" would read as a
    /// second, contradicting count.
    var showsShare = true
    @State private var hovering = false

    var body: some View {
        HStack(spacing: Space.s12) {
            Avatar(sender: UnderstandingText.senderString(bar.row), size: Typo.size(24))
            VStack(alignment: .leading, spacing: Space.scaled(2)) {
                Text(bar.name)
                    .font(Typo.row)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                Text(Fmt.capitalizingFirst(bar.sentence))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Meter(value: bar.value, tint: tint)
                .frame(width: Typo.measure(180), height: 6)
            Text(showsShare ? UsageText.percent(bar.value) : "")
                .font(Typo.num(11, weight: .semibold))
                .foregroundStyle(Palette.inkDim)
                .frame(width: Typo.measure(36), alignment: .trailing)
        }
        .padding(.vertical, Space.s10)
        .padding(.horizontal, Space.s8)
        // The whole row lights on hover, evenly: a filled pane, never an edge.
        .background(
            RoundedRectangle(cornerRadius: Radius.chip, style: .continuous)
                .fill(Palette.ink.opacity(hovering ? 0.05 : 0))
        )
        .onHover { hovering = $0 }
        .contactCard(for: UnderstandingText.senderString(bar.row))
    }
}

/// A track and its fill, both capsules, the fill in the tab's colour.
private struct Meter: View {
    let value: Double
    let tint: Color

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Palette.ink.opacity(0.07))
                Capsule().fill(tint.opacity(0.85))
                    .frame(width: max(geo.size.height, geo.size.width * min(max(value, 0), 1)))
            }
        }
    }
}

// MARK: - What Passband knows

private struct KnowsCard: View {
    let memory: MemoryResponse
    let onChange: (MemoryResponse) -> Void
    @Environment(AppStore.self) private var store
    @State private var busy: Set<String> = []
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s14) {
            HStack(spacing: Space.s12) {
                Text("What Passband knows").font(Typo.zoneTitle).foregroundStyle(Palette.ink)
                Text(UnderstandingText.knowsNote(holding: MemoryGrouping.holding(memory.entries), days: memory.coverage_days))
                    .font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                Spacer(minLength: Space.s8)
            }
            if MemoryGrouping.stillLearning(memory) {
                EmptyNote(UnderstandingText.stillLearning(days: memory.coverage_days))
            } else if memory.entries.isEmpty {
                EmptyNote("Nothing written down yet. The first entries appear after the nightly pass.")
            }
            if !memory.entries.isEmpty {
                ForEach(MemoryGrouping.groups(memory.entries)) { group in
                    VStack(alignment: .leading, spacing: Space.s4) {
                        Text(group.title)
                            .font(Typo.sectionLabel)
                            .foregroundStyle(Palette.accent)
                            .textCase(.uppercase)
                            .padding(.bottom, Space.s2)
                        ForEach(Array(group.entries.enumerated()), id: \.element.id) { i, entry in
                            if i > 0 { Rectangle().fill(Palette.hairline).frame(height: 0.5) }
                            MemoryRow(
                                entry: entry, busy: busy.contains(entry.id),
                                confirm: { Task { await confirm(entry) } },
                                remove: { Task { await remove(entry) } })
                        }
                    }
                }
            }
            if let error {
                Text(error).font(Typo.micro).foregroundStyle(Palette.danger)
            }
            Text("Inferred lines are Passband's own guesses, and it weighs them as guesses. Confirm one to make it yours, or delete it and it is gone.")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
                .fixedSize(horizontal: false, vertical: true)
        }
        .zonePadding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.glassTint)
    }

    private func confirm(_ entry: MemoryEntry) async {
        busy.insert(entry.id)
        defer { busy.remove(entry.id) }
        do {
            let updated = try await APIClient.shared.putMemory(entry.id, MemoryPatch(confirm: true))
            var next = memory
            if let at = next.entries.firstIndex(where: { $0.id == entry.id }) { next.entries[at] = updated }
            error = nil
            onChange(next)
        } catch {
            self.error = "Couldn't confirm that one. Try again in a moment."
        }
    }

    private func remove(_ entry: MemoryEntry) async {
        busy.insert(entry.id)
        defer { busy.remove(entry.id) }
        do {
            try await APIClient.shared.deleteMemory(entry.id)
            var next = memory
            next.entries.removeAll { $0.id == entry.id }
            error = nil
            withAnimation(.smooth(duration: 0.22)) { onChange(next) }
        } catch {
            self.error = "Couldn't delete that one. Try again in a moment."
        }
    }
}

private struct MemoryRow: View {
    let entry: MemoryEntry
    let busy: Bool
    let confirm: () -> Void
    let remove: () -> Void
    @Environment(AppStore.self) private var store
    @State private var hovering = false
    @State private var showReceipts = false

    var body: some View {
        let withdrawn = entry.withdrawn(now: Date())
        HStack(alignment: .firstTextBaseline, spacing: Space.s12) {
            VStack(alignment: .leading, spacing: Space.s4) {
                if let withdrawn {
                    let lead = Text(UnderstandingText.withdrawnLead(withdrawn)).foregroundStyle(Palette.inkFaint)
                    let line = Text(entry.line).strikethrough(true, color: Palette.inkFaintest)
                        .foregroundStyle(Palette.inkFaintest)
                    Text("\(lead) \(line)")
                        .font(Typo.row)
                        .fixedSize(horizontal: false, vertical: true)
                } else {
                    Text(entry.line)
                        .font(Typo.row)
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                }
                HStack(spacing: Space.s8) {
                    OriginBadge(origin: entry.origin)
                    if let age = UnderstandingText.entryAge(entry, now: Date()) {
                        Text(withdrawn.map { UnderstandingText.withdrawnAge($0, age: age) } ?? age)
                            .font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                    }
                    if let because = UnderstandingText.because(entry) {
                        Button(because) { showReceipts = true }
                            .buttonStyle(.textAction)
                            .font(Typo.micro)
                            .popover(isPresented: $showReceipts, arrowEdge: .bottom) {
                                Receipts(entry: entry, open: { id in
                                    showReceipts = false
                                    store.openMessage(id, from: .reader)
                                })
                            }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if withdrawn == nil {
                HStack(spacing: Space.s10) {
                    if entry.origin == .inferred {
                        Button("Confirm", action: confirm).buttonStyle(.textAction).font(Typo.micro)
                    }
                    Button("Delete", action: remove).buttonStyle(.textAction).font(Typo.micro)
                }
                .disabled(busy)
                .opacity(hovering || busy ? 1 : 0.55)
            }
        }
        .padding(.vertical, Space.s10)
        .onHover { hovering = $0 }
        .opacity(busy ? 0.6 : 1)
    }
}

/// "you said" in the accent, "inferred" in the quiet gray: a capsule with
/// its whole border drawn, never one edge.
private struct OriginBadge: View {
    let origin: MemoryOrigin

    var body: some View {
        let tint = origin == .stated ? Palette.accent : Palette.inkFaint
        Text(UnderstandingText.origin(origin))
            .font(Typo.micro)
            .foregroundStyle(tint)
            .padding(.horizontal, Space.s6)
            .padding(.vertical, Space.scaled(1.5))
            .background(Capsule().fill(tint.opacity(0.10)))
            .overlay(Capsule().strokeBorder(tint.opacity(0.28), lineWidth: 0.5))
    }
}

/// The receipts behind an entry: the senders it rests on, how many messages,
/// and one of them a click from the reader.
private struct Receipts: View {
    let entry: MemoryEntry
    let open: (Int) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            Text("Why Passband thinks this")
                .font(Typo.ui(12, weight: .semibold))
                .foregroundStyle(Palette.ink)
            if !entry.provenance.addrs.isEmpty {
                VStack(alignment: .leading, spacing: Space.s2) {
                    ForEach(entry.provenance.addrs, id: \.self) { addr in
                        Text(addr).font(Typo.mono(10)).foregroundStyle(Palette.inkDim)
                    }
                }
            }
            if let first = entry.provenance.message_ids.first {
                let n = entry.provenance.message_ids.count
                Text(n == 1 ? "Resting on one message." : "Resting on \(UsageText.count(n)) messages.")
                    .font(Typo.micro).foregroundStyle(Palette.inkFaint)
                // ANY ONE OF THEM, and labelled so. Message ids follow ingest,
                // and a backfill ingests newest first, so the highest id is
                // as likely the oldest mail as the newest. And not on the
                // fixture stage, whose receipts point at mail it does not have.
                if !FixtureMode.isEnabled {
                    Button(n == 1 ? "Open it" : "Open a message") { open(first) }
                        .buttonStyle(.textAction)
                        .font(Typo.micro)
                }
            }
        }
        .padding(Space.s14)
        .frame(minWidth: Typo.measure(220), alignment: .leading)
    }
}

// MARK: - previews

#Preview("Understanding") {
    ScrollView {
        UnderstandingSection(
            relationships: UnderstandingFixture.relationships(), memory: UnderstandingFixture.memory())
            .padding(Space.page)
    }
    .frame(width: 980, height: 1400)
    .environment(AppStore.shared)
}

#Preview("Still learning") {
    UnderstandingSection(
        relationships: RelationshipsResponse(
            coverage_days: 6, computed_at: nil, rows: Array(UnderstandingFixture.relationships().rows.prefix(5))),
        memory: MemoryResponse(
            enabled: false, coverage_days: 6,
            entries: UnderstandingFixture.memory().entries.filter { $0.origin == .stated }))
        .padding(Space.page)
        .frame(width: 980)
        .environment(AppStore.shared)
}
