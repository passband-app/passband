// The calendar page's arithmetic, against fixtures (Lib/Agenda.swift).
//
// Every one of these is wrong only on somebody's screen at the wrong hour: a
// 11:30pm dinner filed under tomorrow because the day was cut in UTC, a paid
// concert drawn as a maybe because the model also thought it unlikely, a
// declined meeting still taking a row. The fixture week is the mock's
// (docs/calendar-ux-preview.html): Monday October 5, 2026, 3:40pm in San
// Francisco.

import Foundation

@main
struct AgendaTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static let zone = TimeZone(identifier: "America/Los_Angeles")!
    static var cal: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = zone
        return c
    }
    /// Monday Oct 5 2026, 3:40pm PDT.
    static let now = at("2026-10-05T15:40:00-07:00")

    static func main() {
        theDaemonsShapesDecode()
        wireTimesParse()
        daysAreCutInTheUsersZone()
        aRowCrossingMidnightStaysOnItsStartDay()
        orderInsideADay()
        fillIsTheLikelihoodScale()
        cancelledGoingAndDismissed()
        unlikelyFoldsIntoACount()
        endLabels()
        matchedGhostsAndDeclinedEventsAreNotDrawn()
        earlierTodayFoldsAndTodayAlwaysExists()
        theListCanStartOnAnotherDay()
        theNextCommitment()
        dayTitles()
        todaysStrip()
        theGate()
        theNumberKeysCountTheCalendarOnlyWhenOffered()
        theGateIsTheActiveAccounts()
        syncNotices()
        ghostStanding()
        aBadCalendarObjectCannotFailTheStatsPoll()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: fixtures

    static func at(_ s: String) -> Date {
        AgendaTime.parse(s, zone: nil, calendar: cal)!.date
    }

    static func day(_ s: String) -> Date { cal.startOfDay(for: at(s + "T12:00:00-07:00")) }

    static func ghost(
        _ id: Int, _ title: String, start: String, end: String? = nil, allDay: Bool = false,
        estimated: Bool = false, commitment: GhostCommitment = .invited,
        state: GhostState = .ghost, mail: GhostMailState = .live,
        band: GhostBand? = nil, verified: Bool = true, matched: Bool = false
    ) -> TimelineGhost {
        TimelineGhost(
            id: id, kind: .invite, title: title, start: start,
            start_timezone: "America/Los_Angeles", all_day: allDay, end: end,
            end_estimated: estimated, end_source: estimated ? .model : (end == nil ? nil : .stated),
            estimate: estimated ? GhostEstimate(minutes: 180, basis: "doors 7, show 8") : nil,
            location: nil, commitment: commitment, state: state, mail_state: mail,
            verified: verified,
            likelihood: band.map { GhostLikelihood(band: $0, reason: nil) },
            sources: [GhostSource(message_id: id, thread_id: "t\(id)", role: .created)],
            matched_event: matched ? GhostMatchedEvent(calendar_id: "primary", id: "ev\(id)") : nil)
    }

    static func event(
        _ id: String, _ title: String?, start: String, end: String? = nil, allDay: Bool = false,
        response: TimelineSelfResponse? = nil
    ) -> TimelineEvent {
        TimelineEvent(
            id: id, calendar_id: "primary", calendar_name: "Work", title: title, start: start,
            end: end, all_day: allDay, start_timezone: nil, location: nil, status: .confirmed,
            self_response: response, busy: true, organizer: nil, html_link: nil)
    }

    static func build(
        _ ghosts: [TimelineGhost], _ events: [TimelineEvent] = [], at moment: Date = now,
        showDismissed: Bool = false
    ) -> [AgendaDay] {
        let feed = TimelineFeed(ghosts: ghosts, events: events)
        return Agenda.days(
            Agenda.items(feed, calendar: cal, showDismissed: showDismissed), now: moment,
            calendar: cal)
    }

    static func dayOf(_ days: [AgendaDay], _ d: String) -> AgendaDay? {
        days.first { $0.day == day(d) }
    }

    // MARK: wire

    /// The served ghost, verbatim in shape from squelch-api's
    /// client_timeline.rs, decodes; a daemon without Slice B's keys decodes;
    /// one malformed row costs only itself.
    static func theDaemonsShapesDecode() {
        let today = """
            {"ghosts":[
              {"id":7,"kind":"ticket","title":"Fred again..","start":"2026-10-08T20:00:00-07:00",
               "start_timezone":"America/Los_Angeles","all_day":false,
               "end":"2026-10-08T23:00:00-07:00","end_estimated":true,"end_source":"model",
               "estimate":{"minutes":180,"basis":"doors 7, show 8"},"location":"Bill Graham Civic",
               "commitment":"committed","state":"ghost","mail_state":"live","verified":true,
               "likelihood":null,
               "sources":[{"message_id":41,"thread_id":"18c2","role":"created"}]},
              {"id":8,"kind":"brand_new_kind","title":"All day","start":"2026-10-09",
               "start_timezone":null,"all_day":true,"end":null,"end_estimated":false,
               "end_source":null,"estimate":null,"location":null,"commitment":"proposed",
               "state":"from_the_future","mail_state":"live","verified":false,
               "likelihood":{"band":"likely","reason":null},"sources":[]},
              {"id":"not a number","title":5}
            ]}
            """
        guard let feed = try? JSONDecoder().decode(TimelineFeed.self, from: Data(today.utf8)) else {
            return expect(false, "today's timeline shape decodes")
        }
        expect(feed.ghosts.count == 2, "a malformed ghost is dropped, its neighbours kept")
        expect(feed.events == nil && feed.calendar == nil, "no events key before Slice B")
        let fred = feed.ghosts[0]
        expect(fred.commitment == .committed && fred.end_estimated == true, "fields land")
        expect(fred.estimate?.basis == "doors 7, show 8", "the basis is carried as text")
        expect(fred.sources.first?.thread_id == "18c2", "sources carry their thread")
        expect(feed.ghosts[1].kind == .other, "an unknown kind falls back")
        expect(feed.ghosts[1].state == .ghost, "an unknown state falls back to a plain ghost")
        expect(feed.ghosts[1].verified == false, "verified false is kept")

        let sliceB = """
            {"ghosts":[],"events":[
              {"id":"abc","calendar_id":"primary","calendar_name":"Work","title":"Standup",
               "start":"2026-10-06T09:30:00-07:00","end":"2026-10-06T09:45:00-07:00",
               "all_day":false,"start_timezone":"America/Los_Angeles","location":null,
               "status":"confirmed","self_response":"accepted","busy":true,
               "organizer":"a@b.example","html_link":"https://calendar.google.com/x"}],
             "calendar":{"granted":true,"synced_at":"2026-10-05T22:30:00Z","error":null}}
            """
        guard let b = try? JSONDecoder().decode(TimelineFeed.self, from: Data(sliceB.utf8)) else {
            return expect(false, "Slice B's shape decodes")
        }
        expect(b.events?.first?.self_response == .accepted, "an event decodes")
        expect(b.calendar?.granted == true, "the calendar status decodes")
        expect(b.calendar?.truncated == nil, "no truncated key reads as absent")
        let cut = """
            {"ghosts":[],"events":[],
             "calendar":{"granted":true,"synced_at":"2026-10-02T02:40:09.688012+00:00","error":"quota","truncated":true}}
            """
        let c = try? JSONDecoder().decode(TimelineFeed.self, from: Data(cut.utf8))
        expect(c?.calendar?.truncated == true && c?.calendar?.error == .quota,
               "a truncated status with an error decodes")

        let oldStats = """
            {"tier_counts":{},"total":0,"sealed":0,"bands":{"standing":0,"new":0,"open":0}}
            """
        let stats = try? JSONDecoder().decode(StoreStats.self, from: Data(oldStats.utf8))
        expect(stats != nil && stats?.timeline == nil && stats?.calendar == nil,
               "an older daemon's stats decode with no timeline and no calendar")
        let newStats = """
            {"tier_counts":{},"total":0,"sealed":0,"bands":{"standing":0,"new":0,"open":0},
             "timeline":true,"calendar":{"granted":false,
             "connect_url":"https://console.passband.app/reconnect?calendar=1&hint=a%40b.example",
             "connect_command":null}}
            """
        let s2 = try? JSONDecoder().decode(StoreStats.self, from: Data(newStats.utf8))
        expect(s2?.timeline == true && s2?.calendar?.granted == false, "Slice A's stats decode")
        expect(s2?.calendar?.self_connect == nil, "an older daemon sends no self_connect")

        func access(_ calendar: String) -> CalendarAccess? {
            let body = #"{"tier_counts":{},"total":0,"sealed":0,"bands":{"standing":0,"new":0,"open":0},"#
                + #""timeline":true,"calendar":"# + calendar + "}"
            return (try? JSONDecoder().decode(StoreStats.self, from: Data(body.utf8)))?.calendar
        }
        let selfHost = access(#"{"granted":false,"connect_url":null,"connect_command":"squelchd auth --calendar","self_connect":true}"#)
        expect(selfHost?.self_connect == true && selfHost?.granted == false, "self_connect decodes")
        expect(CalendarGate.connectRoute(selfHost) == .daemon, "and is the daemon route")
        let odd = access(#"{"granted":false,"self_connect":"yes"}"#)
        expect(odd?.granted == false && odd?.self_connect == nil,
               "a malformed self_connect is absent, and costs nothing else")
        let start = try? JSONDecoder().decode(
            CalendarConnectStart.self,
            from: Data(#"{"url":"https://accounts.google.com/o/oauth2/v2/auth?client_id=x","expires_in":600}"#.utf8))
        expect(start?.url?.hasPrefix("https://accounts.google.com/") == true && start?.expires_in == 600,
               "the self-host connect answer decodes")
        let empty = try? JSONDecoder().decode(CalendarConnectStart.self, from: Data("{}".utf8))
        expect(empty != nil && CalendarGate.consentURL(empty?.url) == nil, "an answer with no url is no link")
    }

    static func wireTimesParse() {
        let a = AgendaTime.parse("2026-10-05T15:40:00-07:00", zone: nil, calendar: cal)
        let b = AgendaTime.parse("2026-10-05T22:40:00.000Z", zone: nil, calendar: cal)
        expect(a != nil && a?.date == b?.date, "offset and fractional UTC name one instant")
        expect(a?.dateOnly == false, "an instant is not date-only")
        let d = AgendaTime.parse("2026-10-08", zone: "Asia/Tokyo", calendar: cal)
        expect(d?.dateOnly == true, "a date is date-only")
        expect(d?.date == cal.startOfDay(for: at("2026-10-08T12:00:00-07:00")),
               "a date is that date in the USER's zone, not in UTC or the row's zone")
        expect(AgendaTime.parse("2026-02-30", zone: nil, calendar: cal) == nil, "no Feb 30")
        expect(AgendaTime.parse("tomorrow evening", zone: nil, calendar: cal) == nil, "garbage")
        let wall = AgendaTime.parse("2026-10-05T15:40:00", zone: "America/Los_Angeles", calendar: cal)
        expect(wall?.date == now, "a wall time with no offset is read in the row's zone")
    }

    // MARK: days

    static func daysAreCutInTheUsersZone() {
        let days = build([
            // 11:30pm Monday in SF is 06:30 Tuesday in UTC.
            ghost(1, "Late dinner", start: "2026-10-06T06:30:00Z", commitment: .committed),
            ghost(2, "Hiring loop", start: "2026-10-06T10:00:00-07:00", commitment: .committed),
            ghost(3, "Rent day", start: "2026-10-07", allDay: true, commitment: .committed),
        ])
        expect(dayOf(days, "2026-10-05")?.visible.map(\.id) == ["g:1"],
               "an 11:30pm dinner is Monday's, though it is Tuesday in UTC")
        expect(dayOf(days, "2026-10-06")?.visible.map(\.id) == ["g:2"], "tomorrow's row")
        let rent = dayOf(days, "2026-10-07")?.visible.first
        expect(rent?.allDay == true, "an all-day ghost is all-day")
        expect(rent.map { Agenda.timeLabel($0, on: day("2026-10-07"), calendar: cal) } == "all day",
               "and its time reads all day")
        expect(days.map(\.day) == days.map(\.day).sorted(), "days run forward")
    }

    static func aRowCrossingMidnightStaysOnItsStartDay() {
        let party = ghost(
            4, "Warehouse party", start: "2026-10-10T22:00:00-07:00",
            end: "2026-10-11T06:00:00-07:00", commitment: .committed)
        let days = build([party])
        let sat = dayOf(days, "2026-10-10")
        expect(sat?.visible.map(\.id) == ["g:4"], "a 10pm to 6am party is Saturday's")
        expect(dayOf(days, "2026-10-11") == nil, "and is not listed again on Sunday")
        let row = sat!.visible[0]
        expect(Agenda.endLabel(row, on: sat!.day, calendar: cal)
               == Agenda.EndLabel(text: "ends 6am Sun", estimated: false),
               "a stated end on another day is printed, with its day")

        // At 1am Sunday the party is still on: it is listed under TODAY.
        let oneAM = at("2026-10-11T01:00:00-07:00")
        let later = build([party], at: oneAM)
        let today = later.first
        expect(today?.day == day("2026-10-11") && today?.visible.map(\.id) == ["g:4"],
               "a row still running when today began is listed under today")
        expect(Agenda.timeLabel(row, on: day("2026-10-11"), calendar: cal) == "until 6am",
               "and its time column says until when")
        expect(Agenda.endLabel(row, on: day("2026-10-11"), calendar: cal) == nil,
               "with no second end after the title")
        // At 9am Sunday it has finished: earlier today, not a live row.
        let nine = build([party], at: at("2026-10-11T09:00:00-07:00"))
        expect(nine.first?.earlier.map(\.id) == ["g:4"] && nine.first?.visible.isEmpty == true,
               "once over, it folds into earlier today")
    }

    static func orderInsideADay() {
        let days = build(
            [
                ghost(10, "Maybe coffee", start: "2026-10-07T16:00:00-07:00", band: .maybe),
                ghost(11, "Pottery", start: "2026-10-07T19:00:00-07:00", commitment: .committed),
                ghost(12, "Likely drinks", start: "2026-10-07T16:00:00-07:00", band: .likely),
                ghost(13, "Same as event", start: "2026-10-07T11:00:00-07:00", commitment: .committed),
                ghost(14, "Rent", start: "2026-10-07", allDay: true, commitment: .committed),
            ],
            [event("r1", "Roadmap review", start: "2026-10-07T11:00:00-07:00",
                   end: "2026-10-07T12:00:00-07:00")])
        expect(dayOf(days, "2026-10-07")?.visible.map(\.id)
               == ["g:14", "e:primary|r1", "g:13", "g:12", "g:10", "g:11"],
               "all-day first, then by start; at one instant real before ghost, solid before striped before outline")
    }

    static func fillIsTheLikelihoodScale() {
        func one(_ g: TimelineGhost) -> AgendaItem { Agenda.item(g, calendar: cal)! }
        let committed = one(ghost(20, "Concert", start: "2026-10-08T20:00:00-07:00",
                                  commitment: .committed, band: .unlikely))
        expect(committed.fill == .solid && !committed.folded && committed.band == nil,
               "committed is solid whatever the likelihood says, and never folds")
        expect(one(ghost(21, "x", start: "2026-10-08T20:00:00-07:00", band: .likely)).fill == .striped,
               "likely is striped")
        let maybe = one(ghost(22, "x", start: "2026-10-08T20:00:00-07:00", band: .maybe))
        expect(maybe.fill == .outline && maybe.band == .maybe, "maybe is an outline, and says maybe")
        let none = one(ghost(23, "x", start: "2026-10-08T20:00:00-07:00"))
        expect(none.fill == .outline && none.band == nil,
               "no band is an outline that invents no word")
        let going = one(ghost(24, "x", start: "2026-10-08T20:00:00-07:00",
                              state: .going, band: .unlikely))
        expect(going.fill == .solid && !going.folded, "the user's going makes it solid")
        let real = Agenda.item(event("e", "Standup", start: "2026-10-06T09:30:00-07:00"), calendar: cal)
        expect(real?.fill == .solid && real?.isGhost == false, "a real event is solid")
        expect(one(ghost(25, "x", start: "2026-10-08T20:00:00-07:00", verified: false)).unverified,
               "unauthenticated mail is marked")
        expect(!one(ghost(26, "x", start: "2026-10-08T20:00:00-07:00")).unverified,
               "authenticated mail is not")
    }

    static func cancelledGoingAndDismissed() {
        func one(_ g: TimelineGhost) -> AgendaItem? { Agenda.item(g, calendar: cal) }
        let off = one(ghost(30, "Dinner", start: "2026-10-08T19:00:00-07:00",
                            commitment: .committed, state: .cancelled, mail: .cancelled))
        expect(off?.cancelled == true, "a cancelled ghost is drawn, marked cancelled")
        let goingButOff = one(ghost(31, "Dinner", start: "2026-10-08T19:00:00-07:00",
                                    state: .going, mail: .cancelled))
        expect(goingButOff?.cancelled == true && goingButOff?.fill == .solid,
               "going over a real cancellation still says cancelled")
        let dismissed = ghost(32, "Webinar", start: "2026-10-08T12:00:00-07:00", state: .dismissed)
        expect(one(dismissed) == nil, "dismissed is hidden by default")
        expect(Agenda.item(dismissed, calendar: cal, showDismissed: true) != nil,
               "and shown when asked for")
    }

    static func unlikelyFoldsIntoACount() {
        let days = build([
            ghost(40, "Webinar", start: "2026-10-08T12:00:00-07:00", band: .unlikely),
            ghost(41, "Meetup", start: "2026-10-08T18:30:00-07:00", band: .unlikely),
            ghost(42, "Founders dinner", start: "2026-10-08T19:00:00-07:00", band: .unlikely),
            ghost(43, "Drinks", start: "2026-10-08T18:00:00-07:00", band: .likely),
            ghost(44, "Only unlikely", start: "2026-10-09T12:00:00-07:00", band: .unlikely),
        ])
        let thu = dayOf(days, "2026-10-08")
        expect(thu?.unlikely.count == 3, "three unlikely fold into one count")
        expect(thu?.visible.map(\.id) == ["g:43"], "and take no row of their own")
        expect(thu?.unlikely.map(\.id) == ["g:40", "g:41", "g:42"], "the fold keeps time order")
        let fri = dayOf(days, "2026-10-09")
        expect(fri != nil && fri!.visible.isEmpty && fri!.unlikely.count == 1,
               "a day of only unlikely things still exists, as its count")
    }

    static func endLabels() {
        func label(_ g: TimelineGhost) -> Agenda.EndLabel? {
            let item = Agenda.item(g, calendar: cal)!
            return Agenda.endLabel(item, on: cal.startOfDay(for: item.start), calendar: cal)
        }
        expect(label(ghost(50, "Fred again..", start: "2026-10-08T20:00:00-07:00",
                           end: "2026-10-08T23:00:00-07:00", estimated: true, commitment: .committed))
               == Agenda.EndLabel(text: "ends ~11pm", estimated: true),
               "a guessed end reads ends ~11pm, marked estimated")
        expect(label(ghost(51, "x", start: "2026-10-08T20:00:00-07:00",
                           end: "2026-10-08T22:30:00-07:00", estimated: true))?.text == "ends ~10:30pm",
               "minutes are kept")
        expect(label(ghost(52, "x", start: "2026-10-08T20:00:00-07:00",
                           end: "2026-10-08T23:00:00-07:00")) == nil,
               "a stated end on the same day is left to the detail")
        expect(label(ghost(53, "x", start: "2026-10-08T22:00:00-07:00",
                           end: "2026-10-09T01:00:00-07:00", estimated: true))?.text == "ends ~1am Fri",
               "a guessed end past midnight names its day")
        expect(label(ghost(54, "Lisbon", start: "2026-10-14", end: "2026-10-19", allDay: true,
                           commitment: .committed))?.text == "until Oct 19",
               "a ghost's date-only end is its last day")
        let trip = Agenda.item(event("t", "Lisbon", start: "2026-10-14", end: "2026-10-19", allDay: true),
                               calendar: cal)!
        expect(Agenda.endLabel(trip, on: trip.start, calendar: cal)?.text == "until Oct 19",
               "a real event's date-only end is its last day too, as the daemon serves it")
        expect(Agenda.endLabel(trip, on: day("2026-10-16"), calendar: cal)?.text == "until Oct 19",
               "a multi-day all-day row listed under a later day still says through when")
        expect(Agenda.endLabel(trip, on: day("2026-10-19"), calendar: cal) == nil,
               "and on its last day says nothing more")
        // The fixture's own today: a trip that began yesterday, still on.
        let started = build([ghost(57, "Offsite", start: "2026-10-04", end: "2026-10-07",
                                   allDay: true, commitment: .committed)])
        let todayRow = started.first?.visible.first
        expect(todayRow?.id == "g:57", "a trip that began yesterday is listed under today")
        expect(todayRow.map { Agenda.endLabel($0, on: day("2026-10-05"), calendar: cal)?.text }
               == "until Oct 7", "with its end, not a bare all day")
        let oneDay = Agenda.item(event("o", "Holiday", start: "2026-10-14", end: "2026-10-14", allDay: true),
                                 calendar: cal)!
        expect(Agenda.endLabel(oneDay, on: oneDay.start, calendar: cal) == nil,
               "a one-day real event whose end is its own day says nothing more")
        expect(Agenda.endLabel(Agenda.item(ghost(55, "x", start: "2026-10-14", allDay: true),
                                           calendar: cal)!, on: day("2026-10-14"), calendar: cal) == nil,
               "a one-day all-day row says nothing more")
        let fred = Agenda.item(ghost(56, "Fred", start: "2026-10-08T20:00:00-07:00",
                                     end: "2026-10-08T23:00:00-07:00", estimated: true), calendar: cal)!
        let full = Agenda.fullTime(fred, calendar: cal)
        expect(full.lead == "Thu Oct 8, 8pm" && full.end == "11pm", "the detail's full time")
        let span = Agenda.fullTime(trip, calendar: cal)
        expect(span.lead == "Wed Oct 14" && span.end == "Mon Oct 19", "a span of days")
    }

    static func matchedGhostsAndDeclinedEventsAreNotDrawn() {
        let days = build(
            [ghost(60, "Standup", start: "2026-10-06T09:30:00-07:00", band: .likely, matched: true),
             ghost(61, "Lunch", start: "2026-10-06T12:30:00-07:00", band: .likely)],
            [event("a", "Standup", start: "2026-10-06T09:30:00-07:00", response: .accepted),
             event("d", "Offsite", start: "2026-10-06T14:00:00-07:00", response: .declined),
             event("u", "   ", start: "2026-10-06T16:00:00-07:00", response: nil)])
        let tue = dayOf(days, "2026-10-06")
        expect(tue?.visible.map(\.id) == ["e:primary|a", "g:61", "e:primary|u"],
               "the matched ghost and the declined event are gone; the rest stay")
        expect(tue?.visible.last?.title == "Untitled event", "a blank title is named, not blank")
    }

    static func earlierTodayFoldsAndTodayAlwaysExists() {
        let days = build(
            [ghost(70, "Breakfast", start: "2026-10-05T09:00:00-07:00",
                   end: "2026-10-05T10:00:00-07:00", commitment: .committed),
             ghost(71, "Climbing", start: "2026-10-05T18:30:00-07:00", commitment: .committed),
             ghost(72, "Bill due", start: "2026-10-05", allDay: true, commitment: .committed),
             ghost(73, "Last week", start: "2026-09-28T10:00:00-07:00", commitment: .committed)])
        let today = days.first
        expect(today?.day == day("2026-10-05"), "today leads")
        expect(today?.earlier.map(\.id) == ["g:70"], "what has finished folds into earlier")
        expect(today?.visible.map(\.id) == ["g:72", "g:71"], "all-day today stays up all day")
        expect(!days.flatMap(\.visible).contains { $0.id == "g:73" }, "last week is gone")
        let empty = build([])
        expect(empty.count == 1 && empty[0].day == day("2026-10-05") && empty[0].isEmpty,
               "today is there even with nothing on it")
    }

    /// The phone's selected day leads the list instead of today: last week's
    /// day shows what was on it, a day ahead hides the days before it, and a
    /// trip still running when that day began is listed under it.
    static func theListCanStartOnAnotherDay() {
        let ghosts = [
            ghost(74, "Last week", start: "2026-09-28T10:00:00-07:00", commitment: .committed),
            ghost(75, "Breakfast", start: "2026-10-05T09:00:00-07:00",
                  end: "2026-10-05T10:00:00-07:00", commitment: .committed),
            ghost(76, "Trip", start: "2026-10-06", end: "2026-10-10", allDay: true, commitment: .committed),
            ghost(77, "Dinner", start: "2026-10-09T19:00:00-07:00", commitment: .committed),
        ]
        let items = Agenda.items(TimelineFeed(ghosts: ghosts), calendar: cal)
        let past = Agenda.days(items, now: now, calendar: cal, from: day("2026-09-28"))
        expect(past.first?.day == day("2026-09-28") && past.first?.visible.map(\.id) == ["g:74"],
               "a past day leads with what was on it")
        expect(past.first?.earlier.isEmpty == true, "a past day folds nothing into earlier")
        expect(past.contains { $0.day == day("2026-10-05") && $0.earlier.map(\.id) == ["g:75"] },
               "today still folds what has finished")
        let ahead = Agenda.days(items, now: now, calendar: cal, from: day("2026-10-08"))
        expect(ahead.first?.day == day("2026-10-08"), "a later day leads")
        expect(ahead.first?.visible.map(\.id) == ["g:76"], "a trip running into it is listed there")
        expect(!ahead.contains { $0.day == day("2026-10-05") }, "today is not dragged in ahead of it")
        expect(ahead.map(\.day) == [day("2026-10-08"), day("2026-10-09")], "then the days with something on")
        let quiet = Agenda.days([], now: now, calendar: cal, from: day("2026-10-20"))
        expect(quiet.map(\.day) == [day("2026-10-20")] && quiet[0].isEmpty,
               "the chosen day is there even with nothing on it")
        expect(Agenda.days(items, now: now, calendar: cal, from: nil) == build(ghosts),
               "no day chosen is today, exactly as before")
    }

    static func theNextCommitment() {
        let days = build(
            [ghost(80, "Maybe coffee", start: "2026-10-05T16:00:00-07:00", band: .maybe),
             ghost(81, "Cancelled", start: "2026-10-05T16:10:00-07:00", commitment: .committed,
                   state: .cancelled, mail: .cancelled),
             ghost(82, "Pricing sync", start: "2026-10-05T16:30:00-07:00",
                   end: "2026-10-05T17:00:00-07:00", commitment: .committed)])
        let next = Agenda.next(days, now: now)
        expect(next?.item.id == "g:82" && next?.ongoing == false,
               "the next commitment skips maybes and cancellations")
        expect(Agenda.nextLead(next!.item, ongoing: false, now: now, calendar: cal) == "Next, in 50 min",
               "fifty minutes out")
        let during = at("2026-10-05T16:45:00-07:00")
        let running = Agenda.next(build(days.flatMap(\.visible).compactMap(\.ghost), at: during), now: during)
        expect(running?.item.id == "g:82" && running?.ongoing == true, "a running one wins")
        expect(Agenda.nextLead(running!.item, ongoing: true, now: during, calendar: cal) == "Now, until 5pm",
               "and says until when")
        func lead(_ start: String) -> String {
            let item = Agenda.item(ghost(90, "x", start: start, commitment: .committed), calendar: cal)!
            return Agenda.nextLead(item, ongoing: false, now: now, calendar: cal)
        }
        expect(lead("2026-10-05T19:00:00-07:00") == "Next, at 7pm", "later today")
        expect(lead("2026-10-06T10:00:00-07:00") == "Next, tomorrow at 10am", "tomorrow")
        expect(lead("2026-10-07T11:00:00-07:00") == "Next, Wednesday at 11am", "this week")
        expect(lead("2026-10-21T11:00:00-07:00") == "Next, Oct 21 at 11am", "further out")
        expect(Agenda.next(build([ghost(91, "Maybe", start: "2026-10-06T10:00:00-07:00",
                                        band: .likely)]), now: now) == nil,
               "a likely ghost is not a commitment")
    }

    static func dayTitles() {
        func t(_ d: String) -> (String, String) {
            let r = Agenda.dayTitle(day(d), now: now, calendar: cal)
            return (r.title, r.detail)
        }
        expect(t("2026-10-05") == ("Today", "Mon Oct 5"), "today")
        expect(t("2026-10-06") == ("Tomorrow", "Tue Oct 6"), "tomorrow")
        expect(t("2026-10-08") == ("Thursday", "Oct 8"), "this week by name")
        expect(t("2026-10-21") == ("Oct 21", "Wednesday"), "further out by date")
    }

    static func todaysStrip() {
        let today = day("2026-10-05")
        let evening = Agenda.item(ghost(100, "Climbing", start: "2026-10-05T18:30:00-07:00",
                                        end: "2026-10-05T20:00:00-07:00", commitment: .committed),
                                  calendar: cal)!
        let span = Agenda.stripSpan(evening, day: today, calendar: cal)
        let from = 11.5 / 17, to = 13.0 / 17
        expect(span != nil && abs(span!.lowerBound - from) < 1e-9 && abs(span!.upperBound - to) < 1e-9,
               "6:30 to 8pm sits at its hours on a 7am to midnight strip")
        let dawn = Agenda.item(ghost(101, "Flight", start: "2026-10-05T05:00:00-07:00",
                                     end: "2026-10-05T06:00:00-07:00", commitment: .committed),
                               calendar: cal)!
        expect(Agenda.stripSpan(dawn, day: today, calendar: cal) == nil,
               "over by 7am is not on the 7am to midnight strip")
        let small = Agenda.item(ghost(103, "Party", start: "2026-10-04T22:00:00-07:00",
                                      end: "2026-10-05T01:30:00-07:00", commitment: .committed),
                                calendar: cal)!
        expect(Agenda.stripSpan(small, day: today, calendar: cal) == nil,
               "a 1:30am finish draws no sliver at the 7am edge")
        let early = Agenda.item(ghost(104, "Shift", start: "2026-10-05T06:00:00-07:00",
                                      end: "2026-10-05T08:00:00-07:00", commitment: .committed),
                                calendar: cal)!
        let earlySpan = Agenda.stripSpan(early, day: today, calendar: cal)
        expect(earlySpan?.lowerBound == 0 && earlySpan.map { abs($0.upperBound - 1.0 / 17) < 1e-9 } == true,
               "something that began before 7am and runs past it clamps onto the strip's start")
        let allDay = Agenda.item(ghost(102, "Bill", start: "2026-10-05", allDay: true), calendar: cal)!
        expect(Agenda.stripSpan(allDay, day: today, calendar: cal) == nil, "all-day is not a block")
        let n = Agenda.stripNow(now, day: today, calendar: cal)
        expect(n != nil && abs(n! - (8 + 40.0 / 60) / 17) < 1e-9, "now's tick")
    }

    // MARK: gate

    static func theGate() {
        // The truth table: the daemon's timeline against the `calendar` flag.
        expect(CalendarGate.pageAvailable(timeline: true, feature: true), "both: shown")
        expect(!CalendarGate.pageAvailable(timeline: true, feature: false), "no flag: hidden")
        expect(!CalendarGate.pageAvailable(timeline: false, feature: true), "said no: hidden")
        expect(!CalendarGate.pageAvailable(timeline: false, feature: false), "neither: hidden")
        expect(!CalendarGate.pageAvailable(timeline: nil, feature: true), "older daemon: hidden")
        expect(!CalendarGate.pageAvailable(timeline: nil, feature: false), "older daemon, no flag: hidden")

        // The gate closing takes the user off the calendar, and only off it.
        expect(CalendarGate.leavesPage(available: false, active: "calendar", calendar: "calendar"),
               "the flag goes off on the page: leave it")
        expect(!CalendarGate.leavesPage(available: false, active: "emails", calendar: "calendar"),
               "the flag goes off elsewhere: stay")
        expect(!CalendarGate.leavesPage(available: true, active: "calendar", calendar: "calendar"),
               "still on: stay")

        let link = "https://console.passband.app/reconnect?calendar=1&hint=a%40b.example"
        expect(CalendarGate.connectRoute(nil) == nil, "an older daemon offers nothing")
        expect(CalendarGate.connectRoute(CalendarAccess(granted: true, connect_url: link)) == nil,
               "granted offers nothing")
        expect(CalendarGate.connectRoute(CalendarAccess(granted: false, connect_url: link)) == .link(link),
               "hosted is a link")
        expect(CalendarGate.connectRoute(CalendarAccess(granted: false, connect_url: link, self_connect: true))
               == .link(link), "the hosted link wins over self_connect")
        expect(CalendarGate.connectRoute(CalendarAccess(granted: false, self_connect: true)) == .daemon,
               "self-host that can run the consent asks its daemon")
        expect(CalendarGate.connectRoute(CalendarAccess(
            granted: false, connect_url: "javascript:alert(1)", self_connect: true)) == .daemon,
               "a link that is not http is never opened")
        expect(CalendarGate.connectRoute(CalendarAccess(
            granted: false, connect_url: "javascript:alert(1)")) == nil,
               "and is no route on its own")
        expect(CalendarGate.connectRoute(CalendarAccess(granted: false, self_connect: false)) == nil,
               "neither a link nor self_connect: nothing")
        expect(CalendarGate.connectRoute(CalendarAccess(
            granted: false, connect_command: "squelchd auth --calendar")) == nil,
               "an older self-host daemon (command only, no self_connect) offers nothing")
        expect(CalendarGate.connectRoute(CalendarAccess(granted: true, self_connect: true)) == nil,
               "granted self-host offers nothing")
        expect(CalendarGate.offer(route: CalendarGate.connectRoute(CalendarAccess(
            granted: false, connect_command: "squelchd auth --calendar")), cardSeen: false, practice: false)
               == .none, "so an older daemon gets no card and no banner")
        expect(CalendarGate.offer(route: .daemon, cardSeen: false, practice: false) == .card,
               "self-host gets the same card")
        expect(CalendarGate.consentURL(" https://accounts.google.com/o/oauth2/auth?x=1 ")
               == "https://accounts.google.com/o/oauth2/auth?x=1", "the daemon's link is opened, trimmed")
        expect(CalendarGate.consentURL("file:///etc/passwd") == nil
               && CalendarGate.consentURL("") == nil && CalendarGate.consentURL(nil) == nil,
               "a daemon answer with no http link is no link")

        expect(CalendarGate.offer(route: .link(link), cardSeen: false, practice: false) == .card,
               "first visit: the card")
        expect(CalendarGate.offer(route: .link(link), cardSeen: true, practice: false) == .banner,
               "after that: the banner, never the card again")
        expect(CalendarGate.offer(route: .link(link), cardSeen: false, practice: true) == .none,
               "practice: nothing")
        expect(CalendarGate.offer(route: nil, cardSeen: false, practice: false) == .none,
               "nothing to offer: nothing")

        expect(CalendarGate.isWaiting(since: now, now: now.addingTimeInterval(9 * 60)), "waiting")
        expect(!CalendarGate.isWaiting(since: now, now: now.addingTimeInterval(10 * 60)),
               "not past the consent session's life")
        expect(!CalendarGate.isWaiting(since: nil, now: now), "not without a click")
    }

    /// FINDING 1 OF THE REVIEW: stats outlive an account switch, so until the
    /// active account's own poll lands, nothing about connecting is offered.
    /// Wave 4: "make calendar 2, all mail 3, etc". With the page offered it
    /// is second in the count; without it the numbers are the old ones.
    static func theNumberKeysCountTheCalendarOnlyWhenOffered() {
        let pages = ["sitrep", "emails", "auth", "rules", "groups"]
        let offered = CalendarGate.numbered(pages, calendar: "calendar", offered: true)
        expect(offered == ["sitrep", "calendar", "emails", "auth", "rules", "groups"],
               "offered: sitrep 1, calendar 2, emails 3, and every later page one higher")
        expect(offered.firstIndex(of: "calendar") == 1 && offered.firstIndex(of: "emails") == 2,
               "calendar is 2 and all mail is 3")
        expect(offered.count == pages.count + 1, "one more digit, and no page lost")
        let hidden = CalendarGate.numbered(pages, calendar: "calendar", offered: false)
        expect(hidden == pages, "not offered: the numbers are exactly as they were")
        expect(!hidden.contains("calendar"), "and no digit opens a page that is not there")
        expect(CalendarGate.numbered([String](), calendar: "calendar", offered: true).isEmpty,
               "an empty rail stays empty")
        // The flag drives the count through the one gate.
        let flagOff = CalendarGate.pageAvailable(timeline: true, feature: false)
        expect(CalendarGate.numbered(pages, calendar: "calendar", offered: flagOff) == pages,
               "the flag off: the old numbers")
        let flagOn = CalendarGate.pageAvailable(timeline: true, feature: true)
        expect(CalendarGate.numbered(pages, calendar: "calendar", offered: flagOn) == offered,
               "the flag on: calendar is 2")
    }

    static func theGateIsTheActiveAccounts() {
        let link = "https://console.passband.app/reconnect?calendar=1&hint=a%40b.example"
        let previous = CalendarAccess(granted: false, connect_url: link)
        // Account A's stats were written under epoch 3; the switch made it 4.
        let stale = CalendarGate.access(previous, statsEpoch: 3, epoch: 4)
        expect(stale == nil, "stats from before the switch are no answer")
        expect(CalendarGate.connectRoute(stale) == nil, "so there is no connect route")
        expect(CalendarGate.offer(route: CalendarGate.connectRoute(stale), cardSeen: false, practice: false)
               == .none, "and no card is offered to the new account from them")
        expect(CalendarGate.renewRoute(stale) == nil, "nor a renewal")
        expect(CalendarGate.access(previous, statsEpoch: nil, epoch: 0) == nil,
               "no poll landed yet is no answer")
        let own = CalendarGate.access(previous, statsEpoch: 4, epoch: 4)
        expect(own == previous, "the active account's own stats are the answer")
        expect(CalendarGate.offer(route: CalendarGate.connectRoute(own), cardSeen: false, practice: false)
               == .card, "and offer its card")

        let granted = CalendarAccess(granted: true, connect_url: link)
        expect(CalendarGate.connectRoute(granted) == nil && CalendarGate.renewRoute(granted) == .link(link),
               "a granted calendar offers no connect, but can be renewed through the same door")
        expect(CalendarGate.renewRoute(CalendarAccess(granted: true, self_connect: true)) == .daemon,
               "self-host renews through its daemon")
        expect(CalendarGate.renewRoute(CalendarAccess(granted: true, connect_command: "squelchd auth --calendar"))
               == nil, "an older self-host daemon has no renewal door")
        let selfHost = CalendarAccess(granted: false, self_connect: true)
        expect(CalendarGate.connectRoute(CalendarGate.access(selfHost, statsEpoch: 3, epoch: 4)) == nil,
               "self-host stats from before the switch are no route either")
        expect(CalendarGate.connectRoute(CalendarGate.access(selfHost, statsEpoch: 4, epoch: 4)) == .daemon,
               "the active account's own self-host stats are")
    }

    static func syncNotices() {
        func status(_ error: TimelineSyncError?, synced: String? = "2026-10-05T23:12:00Z",
                    granted: Bool = true, truncated: Bool? = nil) -> TimelineCalendarStatus {
            TimelineCalendarStatus(granted: granted, synced_at: synced, error: error, truncated: truncated)
        }
        func notice(_ s: TimelineCalendarStatus?) -> CalendarGate.SyncNotice? {
            CalendarGate.syncNotice(s, now: now, calendar: cal)
        }
        expect(notice(status(nil)) == nil, "a healthy sync says nothing")
        expect(notice(nil) == nil, "an older daemon says nothing")
        expect(notice(status(.auth, granted: false)) == nil, "not granted is the connect banner's to say")
        expect(notice(status(.auth)) == CalendarGate.SyncNotice(
            kind: .renew,
            text: "Your Google Calendar connection needs renewing. The events here may be out of date."),
               "auth asks for a renewal")
        // 23:12Z on Oct 5 is 4:12pm in San Francisco: today, after `now`'s
        // date but the same day.
        expect(notice(status(.quota)) == CalendarGate.SyncNotice(
            kind: .couldNotRefresh, text: "Could not refresh your calendar. Last synced at 4:12pm."),
               "quota says it could not refresh, and when it last did")
        expect(notice(status(.unavailable, synced: "2026-10-04T23:12:00.5+00:00"))?.text
               == "Could not refresh your calendar. Last synced yesterday at 4:12pm.",
               "unavailable says the same, a day back, fractional seconds and all")
        expect(notice(status(.unavailable, synced: "2026-10-01T23:12:00Z"))?.text
               == "Could not refresh your calendar. Last synced Oct 1 at 4:12pm.", "further back")
        expect(notice(status(.quota, synced: nil))?.text == "Could not refresh your calendar.",
               "never synced: no time is invented")
        expect(notice(status(.quota))?.kind == .couldNotRefresh
               && notice(status(.unavailable))?.kind == .couldNotRefresh,
               "neither offers an action")

        expect(CalendarGate.truncatedNote(status(nil, truncated: true)) == "Only the nearest events are shown.",
               "a cut list says so")
        expect(CalendarGate.truncatedNote(status(nil, truncated: false)) == nil
               && CalendarGate.truncatedNote(status(nil)) == nil, "an uncut one does not")

        expect(!CalendarGate.renewalLanded(status(.auth)), "still failing on auth: not renewed")
        expect(!CalendarGate.renewalLanded(status(.quota)), "a quota failure proves nothing")
        expect(CalendarGate.renewalLanded(status(nil)), "a clean sync: renewed")
    }

    static func ghostStanding() {
        func standing(_ g: TimelineGhost) -> Agenda.Standing? { Agenda.standing(g) }
        let at = "2026-10-08T19:00:00-07:00"
        expect(standing(ghost(110, "x", start: at, commitment: .unknown)) == nil,
               "unknown commitment and no likelihood: nothing is claimed")
        expect(standing(ghost(111, "x", start: at, commitment: .invited)) == .status("Invited, not answered"),
               "an invitation")
        expect(standing(ghost(112, "x", start: at, commitment: .proposed)) == .status("Proposed, not agreed"),
               "a proposal")
        expect(standing(ghost(113, "x", start: at, commitment: .unknown, band: .maybe))
               == .likelihood(GhostLikelihood(band: .maybe, reason: nil)), "a band is the likelihood")
        expect(standing(ghost(114, "x", start: at, state: .going, mail: .cancelled))
               == .status("You said you are going. The mail says it was cancelled."),
               "going over a cancellation says both")
        expect(standing(ghost(115, "x", start: at, commitment: .committed, state: .cancelled, mail: .cancelled))
               == .status("Cancelled, according to the mail"), "cancelled")
        expect(standing(ghost(116, "x", start: at, state: .going)) == .status("You said you are going"),
               "going")
        expect(standing(ghost(117, "x", start: at, commitment: .committed, band: .unlikely))
               == .status("Booked, bought or accepted"), "committed outranks a band")
    }

    /// The stats poll is everyone's, every ten seconds: a calendar object it
    /// cannot read must cost the calendar, not the poll.
    static func aBadCalendarObjectCannotFailTheStatsPoll() {
        let base = #""tier_counts":{},"total":3,"sealed":0,"bands":{"standing":0,"new":0,"open":0},"timeline":true"#
        for (bad, what) in [
            (#""calendar":{"connect_url":null}"#, "no granted"),
            (#""calendar":"yes""#, "not an object"),
            (#""calendar":{"granted":"maybe"}"#, "granted not a bool"),
            (#""calendar":null"#, "null"),
        ] {
            let stats = try? JSONDecoder().decode(StoreStats.self, from: Data("{\(base),\(bad)}".utf8))
            expect(stats?.total == 3 && stats?.timeline == true && stats?.calendar == nil,
                   "a calendar with \(what) reads as absent and the rest decodes")
        }
        let good = try? JSONDecoder().decode(
            StoreStats.self,
            from: Data("{\(base),\"calendar\":{\"granted\":true,\"connect_command\":\"squelchd auth --calendar\"}}".utf8))
        expect(good?.calendar == CalendarAccess(granted: true, connect_command: "squelchd auth --calendar"),
               "a good one still decodes")
        // And it encodes back as a plain optional would (practice mode's
        // fixture daemon encodes its stats).
        if let good, let data = try? JSONEncoder().encode(good),
            let again = try? JSONDecoder().decode(StoreStats.self, from: data)
        {
            expect(again.calendar == good.calendar, "it round-trips")
        } else {
            expect(false, "it round-trips")
        }
        var none = good
        none?.calendar = nil
        let json = none.flatMap { try? JSONEncoder().encode($0) }.map { String(decoding: $0, as: UTF8.self) }
        expect(json.map { !$0.contains("calendar") } == true, "nil is left out, not written as null")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
