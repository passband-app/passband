// The two wire shapes behind the Understanding section, decoded the way a
// daemon built beside this client will send them: whole, with fields missing,
// with nulls where counts should be, with fields and kinds this build never
// heard of. The rule under test is that none of that costs more than the one
// thing that was wrong: an unknown kind is `other` with its line kept, a row
// that cannot decode is dropped on its own, and the list around it survives.

import Foundation

@main
@MainActor
struct UnderstandingWireTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        relationshipsDecodeWhole()
        relationshipsTolerateGapsAndNoise()
        memoryDecodesWhole()
        memoryToleratesUnknownKindsAndOrigins()
        aBadEntryCostsOnlyItself()
        thePatchLeavesNilOffTheWire()
        theFixtureRoundTrips()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func decode<T: Decodable>(_ type: T.Type, _ json: String) -> T? {
        do {
            return try JSONDecoder().decode(T.self, from: Data(json.utf8))
        } catch {
            print("  decode error: \(error)")
            return nil
        }
    }

    // MARK: - relationships

    static func relationshipsDecodeWhole() {
        let json = """
            {"coverage_days": 41, "computed_at": "2026-11-04T09:00:00Z", "rows": [{
              "addr": "sam@northwind.example", "display_name": "Sam Rivera",
              "received_count": 212, "received_per_month": 38.0,
              "received_first_at": "2025-10-01T00:00:00Z", "received_last_at": "2026-11-04T08:00:00Z",
              "sent_count": 188, "sent_last_at": "2026-11-04T08:10:00Z", "initiated_count": 40,
              "inbound_threads": 33, "replied_threads": 31, "median_reply_ms": 1320000,
              "attention_rows": 80, "opened_count": 77, "median_read_fraction": 0.93,
              "link_clicks": 12, "corrections": 0, "fatigue_score": null,
              "fatigue_reasons": []
            }]}
            """
        guard let r = decode(RelationshipsResponse.self, json) else {
            expect(false, "a whole relationships page decodes")
            return
        }
        expect(r.coverage_days == 41 && r.computed_at == "2026-11-04T09:00:00Z", "the page's header")
        expect(r.rows.count == 1, "one row")
        let row = r.rows[0]
        expect(row.addr == "sam@northwind.example" && row.display_name == "Sam Rivera", "who")
        expect(row.replied_threads == 31 && row.inbound_threads == 33 && row.median_reply_ms == 1_320_000, "replies")
        expect(row.received_per_month == 38 && row.median_read_fraction == 0.93, "rates")
        expect(row.fatigue_score == nil && row.fatigue_reasons.isEmpty, "no fatigue")
    }

    static func relationshipsTolerateGapsAndNoise() {
        let json = """
            {"rows": [
              {"addr": "a@x.example", "received_count": null, "replied_threads": 2, "inbound_threads": 3,
               "display_name": "", "new_field_from_later": {"nested": true},
               "fatigue_reasons": [{"code": "rarely_opened", "opened": 2, "received": 38, "note": "text"},
                                   {"code": "skimmed", "median_read_fraction": 0.25, "median_words": 900, "median_dwell_ms": 4000},
                                   {"code": "known_contact"},
                                   {"no_code": 1}]},
              {"display_name": "nobody, no addr"},
              "not even an object",
              {"addr": "b@x.example", "median_reply_ms": "soon", "fatigue_score": 0.4}
            ]}
            """
        guard let r = decode(RelationshipsResponse.self, json) else {
            expect(false, "a gappy relationships page still decodes")
            return
        }
        expect(r.coverage_days == 0 && r.computed_at == nil, "a missing header reads as zero and nil")
        expect(r.rows.map(\.addr) == ["a@x.example", "b@x.example"], "a row with no address, and a non-row, are dropped alone")
        let a = r.rows[0]
        expect(a.received_count == 0 && a.sent_count == 0 && a.attention_rows == 0, "null and missing counts are zero")
        expect(a.display_name == nil, "an empty name is no name")
        expect(a.fatigue_reasons.map(\.code) == ["rarely_opened", "skimmed", "known_contact"],
            "the daemon's tagged reasons decode, a reason with no code is dropped")
        expect(a.fatigue_reasons[0].values == ["opened": 2, "received": 38], "numbers kept by name, text dropped")
        expect(a.fatigue_reasons[1].values["median_read_fraction"] == 0.25, "an f32 field is kept as a Double")
        expect(a.fatigue_reasons[2].values.isEmpty, "a variant with no fields has no values")
        expect(UnderstandingText.fatigue(a) == "opened 2 of 38", "and rarely_opened is what the Fading tab says")
        expect(r.rows[1].median_reply_ms == nil && r.rows[1].fatigue_score == 0.4, "a mistyped field is nil, its neighbours kept")
        expect(decode(RelationshipsResponse.self, "{}")?.rows.isEmpty == true, "an empty object is an empty page")
    }

    // MARK: - memory

    static func memoryDecodesWhole() {
        let json = """
            {"enabled": true, "coverage_days": 41, "entries": [{
              "id": "01JABC", "scope": "sender:sam@northwind.example", "origin": "stated", "kind": "person",
              "body": {"addr": "sam@northwind.example", "relation": "cofounder", "note": "answers fast", "weight": 3},
              "line": "Sam Rivera is a cofounder.",
              "provenance": {"message_ids": [11, 12, 13], "addrs": ["sam@northwind.example"]},
              "confidence": 1.0, "created_at": "2026-10-01T00:00:00Z", "updated_at": "2026-10-20T00:00:00Z",
              "affirmed_at": "2026-10-28T00:00:00Z", "expires_at": null, "retired_at": null
            }]}
            """
        guard let m = decode(MemoryResponse.self, json) else {
            expect(false, "a whole memory page decodes")
            return
        }
        expect(m.enabled && m.coverage_days == 41 && m.entries.count == 1, "the header")
        let e = m.entries[0]
        expect(e.id == "01JABC" && e.scope == "sender:sam@northwind.example", "id and scope")
        expect(e.origin == .stated && e.kind == .person, "origin and kind")
        expect(e.body == ["addr": "sam@northwind.example", "relation": "cofounder", "note": "answers fast"],
            "the body keeps its string fields and drops the rest")
        expect(e.provenance.message_ids == [11, 12, 13] && e.provenance.addrs == ["sam@northwind.example"], "receipts")
        expect(e.affirmed_at == "2026-10-28T00:00:00Z" && e.expires_at == nil && !e.isRetired, "dates")
    }

    static func memoryToleratesUnknownKindsAndOrigins() {
        let json = """
            {"enabled": true, "coverage_days": 20, "entries": [
              {"id": "1", "kind": "routine", "origin": "learned_elsewhere", "line": "Clears mail on Sundays."},
              {"id": "2", "kind": "ignores", "origin": "inferred", "line": "Skips vendor updates.",
               "retired_at": "2026-11-01T00:00:00Z", "provenance": {"message_ids": "lots"}}
            ]}
            """
        guard let m = decode(MemoryResponse.self, json) else {
            expect(false, "a memory page with an unknown kind decodes")
            return
        }
        expect(m.entries.count == 2, "both entries survive")
        expect(m.entries[0].kind == .other, "an unknown kind is other")
        expect(m.entries[0].line == "Clears mail on Sundays.", "and its line is still there to draw")
        expect(m.entries[0].origin == .inferred, "an unknown origin is the weaker claim, never stated")
        expect(m.entries[0].scope == "owner" && m.entries[0].provenance.message_ids.isEmpty, "absent scope and receipts")
        expect(m.entries[1].isRetired, "a retired entry is retired")
        expect(m.entries[1].provenance.message_ids.isEmpty, "mistyped receipts read as none")
        expect(MemoryGrouping.groups(m.entries).map(\.kind) == [.ignores, .other], "and it groups under other, last")
    }

    static func aBadEntryCostsOnlyItself() {
        let json = """
            {"enabled": true, "coverage_days": 30, "entries": [
              {"id": "no-line", "kind": "role"},
              {"id": "ok", "kind": "role", "line": "Runs a company."},
              42
            ]}
            """
        let m = decode(MemoryResponse.self, json)
        expect(m?.entries.map(\.id) == ["ok"], "an entry with no line, and a non-entry, are dropped alone")
        expect(decode(MemoryResponse.self, "{}")?.enabled == false, "an empty object is off and empty")
    }

    static func thePatchLeavesNilOffTheWire() {
        let confirm = String(data: try! JSONEncoder().encode(MemoryPatch(confirm: true)), encoding: .utf8)
        expect(confirm == #"{"confirm":true}"#, "confirm alone is the whole body")
    }

    static func theFixtureRoundTrips() {
        let now = Date(timeIntervalSince1970: 1_793_000_000)
        let rel = UnderstandingFixture.relationships(now: now)
        let mem = UnderstandingFixture.memory(now: now)
        let rel2 = (try? JSONEncoder().encode(rel)).flatMap { try? JSONDecoder().decode(RelationshipsResponse.self, from: $0) }
        let mem2 = (try? JSONEncoder().encode(mem)).flatMap { try? JSONDecoder().decode(MemoryResponse.self, from: $0) }
        expect(rel2 == rel, "the relationships fixture survives the wire, as the fixture stage serves it")
        expect(mem2 == mem, "and so does the memory fixture")
        expect(rel.rows.count >= 12, "a dozen people or more")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
