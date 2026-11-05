// The morning brief on the phone: the push id `"<tag>:brief:<YYYYMMDD>"`
// (tag-less, malformed and look-alike ones refused, and never read as a mail
// event), the banner the extension draws from `GET /client/brief`, and where a
// tap goes, dressed or not.

import Foundation

@main
@MainActor
struct MorningBriefTests {
    static var failures = 0
    static var checks = 0

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    static let account = UUID(uuidString: "3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32")!

    static func main() {
        thePushId()
        theBanner()
        whereATapGoes()
        print("morning-brief: \(checks) checks, \(failures) failures")
        if failures > 0 { exit(1) }
    }

    static func thePushId() {
        let raw = "\(account.uuidString):brief:20261105"
        let push = EventBanner.briefPush(raw)
        expect(push == EventBanner.BriefPush(accountId: account, day: 20_261_105), "the daemon's shape parses")
        expect(EventBanner.unresolvedPush(raw) == nil, "never read as a mail event")
        expect(EventBanner.calendarPush(raw) == nil, "nor as a reminder")
        expect(EventBanner.draftPush(raw) == nil, "nor as a draft")
        for bad in [
            "brief:20261105",  // no tag
            "not-a-uuid:brief:20261105",
            "\(account.uuidString):brief:2026110",  // seven digits
            "\(account.uuidString):brief:2026-11-05",
            "\(account.uuidString):brief:020261105",
            "\(account.uuidString):Brief:20261105",
            "\(account.uuidString):brief:20261105:1",
            "\(account.uuidString):cal:7:1793210000",
        ] {
            expect(EventBanner.briefPush(bad) == nil, "refused: \(bad)")
        }
    }

    static func decode(_ json: String) -> MorningBrief? {
        try? JSONDecoder().decode(MorningBrief.self, from: Data(json.utf8))
    }

    static func theBanner() {
        guard
            let brief = decode(
                #"{"day":"2026-11-05","title":"Thursday, Nov 5","lines":["2 need you · PG&E bill due today","First up: Dentist at 9:30 AM"]}"#
            )
        else { return expect(false, "the route's body decodes") }
        let copy = EventBanner.morningBriefCopy(brief)
        expect(copy?.title == "Thursday, Nov 5", "the daemon's title")
        expect(
            copy?.body == "2 need you · PG&E bill due today\nFirst up: Dentist at 9:30 AM",
            "its lines, one per line")

        // Nothing to say (the day emptied after the push): keep the relay's.
        if let empty = decode(#"{"day":"2026-11-05","title":null,"lines":[]}"#) {
            expect(EventBanner.morningBriefCopy(empty) == nil, "an empty day keeps the fallback")
        } else {
            expect(false, "the empty body decodes")
        }
        expect(
            EventBanner.morningBriefCopy(MorningBrief(day: nil, title: "  ", lines: ["x"])) == nil,
            "no title, no banner")
        // A body that grew: two lines at most, blank ones dropped.
        let long = MorningBrief(day: nil, title: "Friday, Nov 6", lines: ["a", " ", "b", "c"])
        expect(EventBanner.morningBriefCopy(long)?.body == "a\nb", "two lines at most")
        // An older daemon without the route: an unknown body decodes to nothing.
        if let other = decode(#"{"error":"not found"}"#) {
            expect(EventBanner.morningBriefCopy(other) == nil, "a foreign body draws nothing")
        }
    }

    static func whereATapGoes() {
        // The relay's own banner (the extension could not dress it).
        let raw: [AnyHashable: Any] = ["event_id": "\(account.uuidString):brief:20261105"]
        expect(EventBanner.tapRoute(raw) == .morningBrief(accountId: account), "the fallback opens the board")
        // The dressed banner.
        let dressed = EventBanner.morningBriefUserInfo(account: account)
        expect(EventBanner.tapRoute(dressed) == .morningBrief(accountId: account), "the dressed one too")
        // Dressed but its account lost: still the board, in the active account.
        expect(
            EventBanner.tapRoute([EventBanner.routeKey: EventBanner.morningBriefRoute])
                == .morningBrief(accountId: nil), "no account, still the board")
    }
}
