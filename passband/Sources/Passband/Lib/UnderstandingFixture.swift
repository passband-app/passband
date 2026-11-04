// A mailbox's worth of understanding, made up: the ledger rows and memory
// entries the Usage page's Understanding section draws, for SwiftUI previews,
// for the fixture stage that demos the page with no daemon (`--fixture-usage`,
// FixtureMode.stagesUsage, served by FixtureAPI), and for the suites. Also the plain mail and spend
// series the top of the Usage page needs on the same stage, so the page opens
// whole rather than as a section under an error.
//
// The people are shaped to exercise every branch the section has: a cofounder
// answered within the hour, a teammate and a lawyer answered the same day, a
// recruiter answered once (a known contact, so never fading), three
// newsletters never opened, an essay list read
// to the end, a vendor whose sorting was corrected, a coffee shop's receipts
// skimmed, and two senders too new to judge. Everything is relative to `now`,
// so the fixture never ages into the "just arrived" strip emptying out.
//
// Every address is on a reserved or invented domain. None of these are real
// people or real companies.

import Foundation

enum UnderstandingFixture {
    static func relationships(now: Date = Date()) -> RelationshipsResponse {
        func ago(_ days: Double) -> String { UnderstandingText.rfc3339(now.addingTimeInterval(-days * 86_400)) }
        let minute = 60_000, hour = 3_600_000
        let rows: [RelationshipRow] = [
            RelationshipRow(
                addr: "sam@northwind.example", display_name: "Sam Rivera", received_count: 212,
                received_per_month: 38, received_first_at: ago(400), received_last_at: ago(0.1),
                sent_count: 188, sent_last_at: ago(0.1), initiated_count: 40, inbound_threads: 33,
                replied_threads: 31, median_reply_ms: 22 * minute, attention_rows: 80, opened_count: 77,
                median_read_fraction: 0.93, link_clicks: 12),
            RelationshipRow(
                addr: "dana@northwind.example", display_name: "Dana Park", received_count: 126,
                received_per_month: 21, received_first_at: ago(300), received_last_at: ago(0.4),
                sent_count: 74, sent_last_at: ago(0.5), initiated_count: 11, inbound_threads: 26,
                replied_threads: 18, median_reply_ms: 2 * hour + 10 * minute, attention_rows: 60,
                opened_count: 55, median_read_fraction: 0.82, link_clicks: 6),
            RelationshipRow(
                addr: "priya@foundry-capital.example", display_name: "Priya Shah", received_count: 41,
                received_per_month: 9, received_first_at: ago(120), received_last_at: ago(1),
                sent_count: 30, sent_last_at: ago(1), initiated_count: 4, inbound_threads: 14,
                replied_threads: 12, median_reply_ms: 3 * hour, attention_rows: 30, opened_count: 30,
                median_read_fraction: 0.97, link_clicks: 4),
            RelationshipRow(
                addr: "jordan@ellis-law.example", display_name: "Jordan Ellis", received_count: 19,
                received_per_month: 4, received_first_at: ago(90), received_last_at: ago(2),
                sent_count: 14, sent_last_at: ago(2), initiated_count: 2, inbound_threads: 9,
                replied_threads: 8, median_reply_ms: 9 * hour, attention_rows: 15, opened_count: 15,
                median_read_fraction: 0.9, link_clicks: 2),
            RelationshipRow(
                addr: "lin.chen@mail.example", display_name: "Lin Chen", received_count: 64,
                received_per_month: 6, received_first_at: ago(700), received_last_at: ago(3),
                sent_count: 40, sent_last_at: ago(4), initiated_count: 9, inbound_threads: 20,
                replied_threads: 15, median_reply_ms: 30 * hour, attention_rows: 24, opened_count: 23,
                median_read_fraction: 0.88),
            RelationshipRow(
                addr: "alex@talentbridge.example", display_name: "Alex Moreno", received_count: 11,
                received_per_month: 2.5, received_first_at: ago(130), received_last_at: ago(9),
                sent_count: 1, sent_last_at: ago(100), inbound_threads: 6, replied_threads: 1,
                attention_rows: 8, opened_count: 3, median_read_fraction: 0.3,
                fatigue_score: 0, fatigue_reasons: [FatigueReason(code: "known_contact")]),
            RelationshipRow(
                addr: "essays@longreadclub.example", display_name: "Long Read Club", received_count: 46,
                received_per_month: 6, received_first_at: ago(240), received_last_at: ago(2),
                inbound_threads: 46, attention_rows: 20, opened_count: 18, median_read_fraction: 0.91,
                link_clicks: 9),
            RelationshipRow(
                addr: "support@cumulus-hosting.example", display_name: "Cumulus Hosting", received_count: 88,
                received_per_month: 15, received_first_at: ago(260), received_last_at: ago(1),
                inbound_threads: 40, attention_rows: 30, opened_count: 14, median_read_fraction: 0.48,
                link_clicks: 3, corrections: 4, fatigue_score: 0.33,
                fatigue_reasons: [
                    FatigueReason(code: "skimmed", values: ["median_read_fraction": 0.48, "median_words": 310, "median_dwell_ms": 21_000])
                ]),
            RelationshipRow(
                addr: "receipts@bluebird-coffee.example", display_name: "Bluebird Coffee", received_count: 58,
                received_per_month: 10, received_first_at: ago(200), received_last_at: ago(0.6),
                inbound_threads: 58, attention_rows: 25, opened_count: 9, median_read_fraction: 0.2,
                fatigue_score: 0.58,
                fatigue_reasons: [
                    FatigueReason(code: "rarely_opened", values: ["opened": 9, "received": 25]),
                    FatigueReason(code: "brief_looks", values: ["median_dwell_ms": 2_400]),
                ]),
            RelationshipRow(
                addr: "digest@productpulse.example", display_name: "Product Pulse", received_count: 190,
                received_per_month: 31, received_first_at: ago(340), received_last_at: ago(0.2),
                inbound_threads: 190, attention_rows: 38, opened_count: 2, median_read_fraction: 0.1,
                fatigue_score: 0.86,
                fatigue_reasons: [
                    FatigueReason(code: "rarely_opened", values: ["opened": 2, "received": 38]),
                    FatigueReason(code: "high_volume", values: ["per_month": 31]),
                ]),
            RelationshipRow(
                addr: "news@saasweekly.example", display_name: "SaaS Weekly", received_count: 52,
                received_per_month: 12, received_first_at: ago(260), received_last_at: ago(1.5),
                inbound_threads: 52, attention_rows: 23, opened_count: 1,
                fatigue_score: 0.92, fatigue_reasons: [FatigueReason(code: "rarely_opened", values: ["opened": 1, "received": 23])]),
            RelationshipRow(
                addr: "hello@growthletter.example", display_name: "The Growth Letter", received_count: 34,
                received_per_month: 8, received_first_at: ago(200), received_last_at: ago(2.5),
                inbound_threads: 34, attention_rows: 17, opened_count: 0,
                fatigue_score: 0.95, fatigue_reasons: [FatigueReason(code: "cleared_unopened", values: ["count": 17])]),
            RelationshipRow(
                addr: "maya@studio-oak.example", display_name: "Maya Okafor", received_count: 2,
                received_first_at: ago(3), received_last_at: ago(1), sent_count: 1, sent_last_at: ago(1),
                inbound_threads: 1, replied_threads: 1, attention_rows: 2, opened_count: 2,
                median_read_fraction: 1.0),
            RelationshipRow(
                addr: "hello@fieldnote.example", display_name: "Fieldnote", received_count: 3,
                received_first_at: ago(6), received_last_at: ago(0.8), inbound_threads: 3,
                attention_rows: 2, opened_count: 0),
        ]
        return RelationshipsResponse(coverage_days: 41, computed_at: ago(0.05), rows: rows)
    }

    static func memory(now: Date = Date()) -> MemoryResponse {
        func ago(_ days: Double) -> String { UnderstandingText.rfc3339(now.addingTimeInterval(-days * 86_400)) }
        func entry(
            _ id: String, _ kind: MemoryKind, _ origin: MemoryOrigin, _ line: String, scope: String = "owner",
            body: [String: String] = [:], messages: Int, addrs: [String] = [], confidence: Double,
            affirmed: Double, retired: Double? = nil
        ) -> MemoryEntry {
            MemoryEntry(
                id: id, scope: scope, origin: origin, kind: kind, body: body, line: line,
                provenance: MemoryProvenance(
                    message_ids: messages > 0 ? Array(4_100..<(4_100 + messages)) : [], addrs: addrs),
                confidence: confidence, created_at: ago(affirmed + 20), updated_at: ago(affirmed),
                affirmed_at: ago(affirmed), expires_at: origin == .inferred ? ago(affirmed - 90) : nil,
                retired_at: retired.map(ago))
        }
        let entries: [MemoryEntry] = [
            entry(
                "01JFIXROLE", .role, .inferred,
                "Runs a small software company; most mail is about product, hiring and investors.",
                body: ["what": "Runs a small software company"], messages: 24,
                addrs: ["sam@northwind.example", "dana@northwind.example"], confidence: 0.86, affirmed: 1),
            entry(
                "01JFIXROUND", .project, .inferred,
                "Closing a seed round through November; investor and lawyer mail is live.",
                body: ["name": "Seed round", "until": "2026-11-30"], messages: 11,
                addrs: ["priya@foundry-capital.example", "jordan@ellis-law.example"], confidence: 0.78, affirmed: 2),
            entry(
                "01JFIXSAM", .person, .stated,
                "Sam Rivera is a cofounder; replied to within the hour on 31 of 33 threads.",
                scope: "sender:sam@northwind.example",
                body: ["addr": "sam@northwind.example", "relation": "cofounder"], messages: 31,
                addrs: ["sam@northwind.example"], confidence: 1, affirmed: 6),
            entry(
                "01JFIXPRIYA", .person, .inferred,
                "Priya Shah is an investor in the round, and gets an answer the same afternoon.",
                scope: "sender:priya@foundry-capital.example",
                body: ["addr": "priya@foundry-capital.example", "relation": "investor"], messages: 12,
                addrs: ["priya@foundry-capital.example"], confidence: 0.74, affirmed: 2),
            entry(
                "01JFIXALEX", .person, .inferred,
                "Alex Moreno is a recruiter you were talking to about a role.",
                scope: "sender:alex@talentbridge.example",
                body: ["addr": "alex@talentbridge.example", "relation": "recruiter"], messages: 2,
                addrs: ["alex@talentbridge.example"], confidence: 0.4, affirmed: 30, retired: 5),
            entry(
                "01JFIXREADS", .prefers, .inferred,
                "Reads long essays to the end; skims company newsletters.",
                body: ["what": "Reads long essays to the end"], messages: 18,
                addrs: ["essays@longreadclub.example"], confidence: 0.69, affirmed: 3),
            entry(
                "01JFIXSAAS", .ignores, .inferred,
                "Opens almost none of the product update mail from software vendors.",
                body: ["what": "Product update mail", "basis": "fatigue"], messages: 0,
                addrs: ["digest@productpulse.example", "news@saasweekly.example", "hello@growthletter.example"],
                confidence: 0.81, affirmed: 1),
            entry(
                "01JFIXCOFFEE", .ignores, .stated,
                "Coffee shop receipts never need a look.",
                scope: "sender:receipts@bluebird-coffee.example",
                body: ["what": "Coffee shop receipts"], messages: 0, addrs: ["receipts@bluebird-coffee.example"],
                confidence: 1, affirmed: 12),
            // A kind this build does not know, as a newer daemon would send
            // it: drawn under "Also noticed" with its line intact.
            entry(
                "01JFIXSUNDAY", .other, .inferred,
                "Clears the inbox on Sunday evenings, usually in one sitting.",
                messages: 9, confidence: 0.55, affirmed: 4),
        ]
        return MemoryResponse(enabled: true, coverage_days: 41, entries: entries)
    }

    // MARK: - the rest of the Usage page

    /// The UTC day keys the usage ledger writes, `days` of them ending today.
    static func dayKeys(days: Int, now: Date) -> [String] {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "UTC")!
        return (0..<max(days, 1)).reversed().compactMap { back in
            cal.date(byAdding: .day, value: -back, to: now).map { d in
                let c = cal.dateComponents([.year, .month, .day], from: d)
                return String(format: "%04d-%02d-%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0)
            }
        }
    }

    /// A smooth, weekly-shaped series: weekdays busier than weekends, and a
    /// slow swell so the windows compare to something.
    private static func wave(_ i: Int, base: Double, swing: Double) -> Int {
        let weekday = [1.0, 1.08, 1.12, 1.05, 0.96, 0.52, 0.44][((i % 7) + 7) % 7]
        let swell = 1 + 0.12 * sin(Double(i) / 9)
        let jitter = Double((i * 37 + 11) % 13) / 13 - 0.5
        return max(0, Int((base * weekday * swell + jitter * swing).rounded()))
    }

    static func mailActivity(days: Int, now: Date = Date()) -> MailActivityResponse {
        let keys = dayKeys(days: days, now: now)
        let rows = keys.enumerated().map { i, key in
            let received = wave(i, base: 64, swing: 10)
            let noise = Int(Double(received) * 0.62)
            let signal = max(0, received - noise - 6)
            return MailActivityDay(
                day: key, received: received, sent: wave(i + 3, base: 9, swing: 4), sealed: i % 5 == 0 ? 1 : 0,
                past_due: i % 11 == 0 ? 1 : 0, deadline: i % 4 == 0 ? 2 : 1, signal: signal, noise: noise,
                pending: 0)
        }
        return MailActivityResponse(days: days, since: keys.first ?? "", until: keys.last ?? "", rows: rows)
    }

    static func usage(days: Int, now: Date = Date()) -> UsageResponse {
        let keys = dayKeys(days: days, now: now)
        func category(_ model: String, perCall: Double, calls base: Double, tokens: Int) -> UsageCategory {
            let rows = keys.enumerated().map { i, key in
                let calls = wave(i, base: base, swing: base / 6)
                return UsageRow(
                    day: key, calls: calls, input_tokens: calls * tokens, output_tokens: calls * tokens / 9,
                    est_cost_usd: Double(calls) * perCall)
            }
            let totals = UsageTotals(
                calls: rows.reduce(0) { $0 + $1.calls }, input_tokens: rows.reduce(0) { $0 + $1.input_tokens },
                output_tokens: rows.reduce(0) { $0 + $1.output_tokens },
                est_cost_usd: rows.reduce(0) { $0 + ($1.est_cost_usd ?? 0) })
            return UsageCategory(rows: rows, totals: totals, model: model, provider: "fixture")
        }
        let categories = [
            "stage1": category("small-model", perCall: 0.0011, calls: 64, tokens: 2_400),
            "stage2": category("capable-model", perCall: 0.012, calls: 7, tokens: 6_800),
            "extract_calendar": category("small-model", perCall: 0.0008, calls: 6, tokens: 1_900),
        ]
        let empty = UsageTotals(calls: 0, input_tokens: 0, output_tokens: 0, est_cost_usd: 0)
        return UsageResponse(rows: [], totals: empty, provider: "fixture", model: "capable-model", categories: categories)
    }
}
