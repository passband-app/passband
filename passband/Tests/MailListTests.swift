import Foundation

@main
struct MailListTests {
    static func main() throws {
        func row(
            _ id: Int, tier: Tier = .signal, status: AttentionStatus = .open,
            received: String? = nil, deadline: String? = nil, attachments: Bool = false
        ) -> AttentionUpdate {
            var u = AttentionUpdate(
                id: id, thread_id: "t\(id)", tier: tier, importance: 50,
                sender: "a@example.com", one_line: "Line \(id)", reason: "r",
                deadline: deadline, has_attachments: attachments, status: status,
                surfaced_at: "2026-09-01T00:00:00Z")
            u.received_at = received
            return u
        }
        let rows = [
            row(1, tier: .signal),
            row(2, tier: .noise, attachments: true),
            row(3, tier: .deadline, status: .done, deadline: "2026-10-01T00:00:00Z"),
            row(4, tier: .noise),
        ]

        // To do drops server-done rows AND rows resolved this session.
        let todo = MailList.visible(rows, showDone: false, filter: .all, resolved: [4])
        precondition(todo.map(\.id) == [1, 2], "to do hides done + locally resolved")
        // Everything keeps them, in fetch order.
        let all = MailList.visible(rows, showDone: true, filter: .all, resolved: [4])
        precondition(all.map(\.id) == [1, 2, 3, 4], "everything keeps finished mail")
        precondition(MailList.isDone(rows[3], resolved: [4]) && !MailList.isDone(rows[0], resolved: [4]))

        // Lenses.
        precondition(
            MailList.visible(rows, showDone: true, filter: .important, resolved: []).map(\.id)
                == [1, 3], "important = anything triage did not file as noise")
        precondition(
            MailList.visible(rows, showDone: true, filter: .deadlines, resolved: []).map(\.id)
                == [3])
        precondition(
            MailList.visible(rows, showDone: false, filter: .deadlines, resolved: []).isEmpty,
            "a lens composes with to do")
        precondition(
            MailList.visible(rows, showDone: true, filter: .attachments, resolved: []).map(\.id)
                == [2])

        // Day headings, against a fixed clock and calendar.
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "UTC")!
        let now = Fmt.date("2026-09-30T15:00:00Z")!
        func title(_ iso: String) -> String {
            MailList.dayTitle(Fmt.date(iso), now: now, calendar: cal)
        }
        precondition(title("2026-09-30T01:00:00Z") == "Today")
        precondition(title("2026-10-01T01:00:00Z") == "Today", "a fast sender clock is still today")
        precondition(title("2026-09-29T23:00:00Z") == "Yesterday")
        precondition(title("2026-09-21T12:00:00Z") == "Last week")
        precondition(title("2026-09-02T12:00:00Z").hasPrefix("Earlier in "))
        precondition(!title("2026-08-20T12:00:00Z").hasPrefix("Earlier"), "a past month is its name")
        precondition(MailList.dayTitle(nil, now: now, calendar: cal) == "Recent")

        // Sections are contiguous runs, with flat start positions.
        let dated = [
            row(10, received: "2026-09-30T10:00:00Z"),
            row(11, received: "2026-09-30T09:00:00Z"),
            row(12, received: "2026-09-29T09:00:00Z"),
            row(13, received: "2026-08-01T09:00:00Z"),
        ]
        let sections = MailList.sections(dated, now: now, calendar: cal)
        precondition(sections.map(\.title).prefix(2) == ["Today", "Yesterday"])
        precondition(sections.map(\.start) == [0, 2, 3], "starts index the flat list")
        precondition(sections.map(\.rows.count) == [2, 1, 1])

        // received_at wins over surfaced_at; an old daemon falls back.
        precondition(dated[0].arrivedAt == "2026-09-30T10:00:00Z")
        precondition(rows[0].arrivedAt == "2026-09-01T00:00:00Z")

        // Stamps: time today, no year this year, a year otherwise.
        precondition(!MailList.stamp(Fmt.date("2026-09-30T10:00:00Z"), now: now, calendar: cal).contains("Sep"))
        precondition(MailList.stamp(Fmt.date("2026-09-02T10:00:00Z"), now: now, calendar: cal).contains("Sep"))
        precondition(MailList.stamp(Fmt.date("2025-09-02T10:00:00Z"), now: now, calendar: cal).contains("2025"))

        // The wire still decodes without received_at (older daemon).
        let json = #"{"id":1,"thread_id":"t","tier":"signal","importance":1,"sender":"a@b.c","one_line":"x","reason":"y","status":"open","surfaced_at":null,"resolved_at":null}"#
        let decoded = try JSONDecoder().decode(AttentionUpdate.self, from: Data(json.utf8))
        precondition(decoded.received_at == nil && decoded.arrivedAt == nil)
        print("ok: mail list checks passed")
    }
}
