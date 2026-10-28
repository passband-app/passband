// THE CALENDAR PAGE'S ARITHMETIC: what a timeline answer becomes on the agenda.
// Which rows are drawn at all, which day each lands on in the user's own zone,
// the order inside a day, how solid each one is, which fold away, and every
// time string the rows and the detail print.
//
// PURE, and Foundation only, so test.sh can hold the whole of it up against
// fixtures (Tests/AgendaTests.swift). Everything that depends on a clock or a
// zone takes them as arguments; nothing here reads `Date()` or
// `Calendar.current` on its own. The page re-derives this on the stats poll's
// cadence, so the one costly step, parsing the wire's time strings, is
// memoized (the same trade `CalendarVisibility.startDate` makes).
//
// The visual language this encodes is docs/calendar-ux-preview.html: the more
// solid a block, the more real it is. Solid is a real event or a commitment,
// striped is likely, outlined is maybe, unlikely folds into a count. A ghost
// is never drawn fainter for being a ghost; only its container thins.

import Foundation

// MARK: - the gate

/// Whether the Calendar destination exists, and how to connect a calendar.
enum CalendarGate {
    /// THE ONE PLACE THE CALENDAR PAGE IS SWITCHED ON. The daemon must serve
    /// the timeline (`timeline == true` on /client/stats), and the `calendar`
    /// hidden feature must be on for this mailbox (`feature`, which is
    /// `store.hasFeature(.calendar)`; docs/FEATURES.md, docs/CALENDAR.md
    /// section 11). A daemon with the flag off already says `timeline: false`;
    /// the app asks both anyway, so a daemon older than the flag cannot show
    /// the page to somebody who was not let in. The rail, the Go menu, the
    /// number keys, ⇧C, the shortcuts sheet and the route all read it through
    /// `AppStore.calendarPageAvailable`, so it hides everywhere at once.
    static func pageAvailable(timeline: Bool?, feature: Bool) -> Bool {
        timeline == true && feature
    }

    /// Whether the gate closing (the flag turned off, an older daemon) must
    /// take the user off the page they are on: only when that page is the
    /// calendar. Generic for the reason `numbered` is.
    static func leavesPage<Page: Equatable>(available: Bool, active: Page, calendar: Page) -> Bool {
        !available && active == calendar
    }

    /// THE NUMBER KEYS, WITH THE CALENDAR IN THEM (wave 4, the founder: "make
    /// calendar 2, all mail 3, etc"). The calendar is second on the rail, so
    /// while it is offered it is second in the count and every later page is
    /// one higher. While it is NOT offered the numbers are exactly the ones
    /// from before it existed: nobody's fingers are renumbered by a page
    /// they cannot see. Generic so the rule is held by a test without the
    /// app's own page list.
    static func numbered<Page>(_ pages: [Page], calendar: Page, offered: Bool) -> [Page] {
        guard offered, !pages.isEmpty else { return pages }
        var out = pages
        out.insert(calendar, at: 1)
        return out
    }

    /// Where the one "Connect calendar" button gets Google's consent link.
    /// The daemon decides which by what it sent; the app never guesses. Both
    /// end the same way: a browser on Google's consent, and the grant showing
    /// up on the stats poll.
    enum ConnectRoute: Hashable, Sendable {
        /// Hosted: the control plane's consent page, account pinned.
        case link(String)
        /// Self-host whose daemon can run the consent itself: ask it for a
        /// fresh link (`POST /client/calendar/connect`) on each press, since
        /// a link it gave earlier may have expired.
        case daemon
    }

    /// THE CALENDAR HALF OF /client/stats, BUT ONLY THE ACTIVE ACCOUNT'S.
    /// `stats` outlives an account switch (only the poller writes it), so
    /// between the switch and the new mailbox's first poll it still describes
    /// the mailbox just left: its grant, its consent link, its address.
    /// Anything calendar-related derived from that would ask account B to
    /// connect with account A's link. `statsEpoch` is the store epoch of the
    /// poll that wrote `stats`; until it is the live epoch there is no answer,
    /// and no answer means no card, no banner, no route and no grant.
    static func access(_ access: CalendarAccess?, statsEpoch: Int?, epoch: Int) -> CalendarAccess? {
        statsEpoch == epoch ? access : nil
    }

    /// How this mailbox can connect a calendar, or nil when there is nothing
    /// to offer: already granted, a daemon too old to say, or a daemon that
    /// said nothing usable. Nil means no card and no banner.
    static func connectRoute(_ access: CalendarAccess?) -> ConnectRoute? {
        guard let access, !access.granted else { return nil }
        return renewRoute(access)
    }

    /// The same door for a calendar that IS granted but whose token Google
    /// has stopped accepting (the timeline's sync status says `auth`): the
    /// daemon sends its consent link or `self_connect` whether or not it is
    /// granted, and renewing is the same consent over again.
    ///
    /// A hosted link wins; then a daemon that says it can run the consent;
    /// otherwise nothing. `connect_command` is NOT a route: an older
    /// self-host daemon that sends only the command gets no card and no
    /// banner, and the terminal path lives in the self-host docs.
    static func renewRoute(_ access: CalendarAccess?) -> ConnectRoute? {
        guard let access else { return nil }
        if let url = consentURL(access.connect_url) { return .link(url) }
        if access.self_connect == true { return .daemon }
        return nil
    }

    /// A consent link worth opening: http(s) only, as every link the app
    /// hands to the browser. Shared by the stats' `connect_url` and the
    /// self-host daemon's answer.
    static func consentURL(_ raw: String?) -> String? {
        guard let url = raw?.trimmingCharacters(in: .whitespacesAndNewlines),
            let scheme = URL(string: url)?.scheme?.lowercased(),
            scheme == "https" || scheme == "http"
        else { return nil }
        return url
    }

    /// What the page offers about connecting.
    enum Offer: Equatable, Sendable {
        case none
        /// The modal card, raised on its own: the first visit, per account.
        case card
        /// The slim line on the page, after the card has been seen once.
        case banner
    }

    /// The card is raised UNPROMPTED once per account and never again: any
    /// way out of it (Not now, Escape, the scrim, or Connect itself) counts
    /// as seen. After that the banner is the door. Practice mode offers
    /// nothing: its daemon is a fixture.
    static func offer(route: ConnectRoute?, cardSeen: Bool, practice: Bool) -> Offer {
        guard route != nil, !practice else { return .none }
        return cardSeen ? .banner : .card
    }

    /// How long the page keeps saying it is waiting for Google after Connect.
    /// The control plane's consent session lives ten minutes, so past that
    /// the browser tab is dead and "waiting" would be a lie.
    static let connectWaitWindow: TimeInterval = 10 * 60

    static func isWaiting(since: Date?, now: Date) -> Bool {
        guard let since else { return false }
        return now >= since && now.timeIntervalSince(since) < connectWaitWindow
    }

    // MARK: sync status

    /// What the page says about the calendar sync, when it has to say
    /// anything. Quiet by design: the events on screen stay, and only a
    /// broken connection gets a button.
    struct SyncNotice: Equatable, Sendable {
        enum Kind: Equatable, Sendable {
            /// Google stopped accepting the token: renew it (the Connect door).
            case renew
            /// Google was busy or unreachable: nothing to do but wait.
            case couldNotRefresh
        }
        let kind: Kind
        let text: String
    }

    /// The timeline's `calendar` status, as a line on the page. Nil while the
    /// sync is healthy, while calendar is not granted (the connect banner is
    /// that page's line), and on a daemon too old to send a status.
    static func syncNotice(
        _ status: TimelineCalendarStatus?, now: Date, calendar: Calendar
    ) -> SyncNotice? {
        guard let status, status.granted, let error = status.error else { return nil }
        switch error {
        case .auth:
            return SyncNotice(
                kind: .renew,
                text: "Your Google Calendar connection needs renewing. The events here may be out of date.")
        case .quota, .unavailable:
            guard let raw = status.synced_at,
                let synced = AgendaTime.parse(raw, zone: nil, calendar: calendar), !synced.dateOnly
            else {
                return SyncNotice(kind: .couldNotRefresh, text: "Could not refresh your calendar.")
            }
            return SyncNotice(
                kind: .couldNotRefresh,
                text: "Could not refresh your calendar. Last synced \(Agenda.lastSynced(synced.date, now: now, calendar: calendar)).")
        }
    }

    /// The faint note under a list the daemon cut short (`truncated`).
    static func truncatedNote(_ status: TimelineCalendarStatus?) -> String? {
        status?.granted == true && status?.truncated == true
            ? "Only the nearest events are shown." : nil
    }

    /// Whether a renewal the user started has visibly worked. The grant flag
    /// cannot say so (it never went false: the scopes were still recorded),
    /// so the sign is the sync itself: it was failing with `auth` when the
    /// renewal began, and a clean one since means the new token works. A
    /// quota or network failure is not a success, so it keeps waiting.
    static func renewalLanded(_ status: TimelineCalendarStatus?) -> Bool {
        guard let status else { return false }
        return status.granted && status.error == nil
    }
}

// MARK: - the row model

/// How solid a block is drawn. The likelihood scale, plus the real.
enum AgendaFill: Int, Sendable, Hashable, Comparable {
    /// A real event, a committed ghost, or one the user said they are going to.
    case solid = 0
    /// A likely ghost.
    case striped = 1
    /// A maybe ghost, or one with no band at all.
    case outline = 2

    static func < (a: AgendaFill, b: AgendaFill) -> Bool { a.rawValue < b.rawValue }
}

/// One row of the agenda: a real event or a ghost, flattened to what the page
/// draws. `start` / `end` are instants; for an all-day row they are local
/// midnights and `end` is EXCLUSIVE (the midnight after the last day).
struct AgendaItem: Identifiable, Sendable, Hashable {
    enum Origin: Sendable, Hashable {
        case ghost(TimelineGhost)
        case event(TimelineEvent)
    }

    let id: String
    let origin: Origin
    let title: String
    let location: String?
    let start: Date
    let end: Date?
    let allDay: Bool
    /// The end is a guess (history, the model, or the table by kind).
    let endEstimated: Bool
    let fill: AgendaFill
    /// The band word the row prints ("likely", "maybe"). Nil for anything
    /// certain, and for a ghost the daemon gave no band: nothing is invented.
    let band: GhostBand?
    /// The mail says it is off, whatever the user's word was.
    let cancelled: Bool
    /// Made from mail that did not pass sender authentication.
    let unverified: Bool
    /// Unlikely: folds into the day's count instead of taking a row.
    let folded: Bool

    var ghost: TimelineGhost? {
        if case .ghost(let g) = origin { return g }
        return nil
    }

    var event: TimelineEvent? {
        if case .event(let e) = origin { return e }
        return nil
    }

    var isGhost: Bool { ghost != nil }

    /// When the row stops being "now". A timed ghost with no end gets an hour,
    /// which only decides whether it has passed, never what is printed.
    var effectiveEnd: Date {
        if let end, end > start { return end }
        return start.addingTimeInterval(allDay ? 86_400 : 3_600)
    }
}

/// One day of the agenda, in the user's zone.
struct AgendaDay: Identifiable, Sendable, Hashable {
    /// Local midnight.
    let day: Date
    /// Rows drawn in full, in order.
    let visible: [AgendaItem]
    /// Unlikely ghosts, folded into "N unlikely".
    let unlikely: [AgendaItem]
    /// Today only: what has already finished, folded into "N earlier today".
    let earlier: [AgendaItem]

    var id: Date { day }
    var isEmpty: Bool { visible.isEmpty && unlikely.isEmpty && earlier.isEmpty }
}

// MARK: - building

enum Agenda {
    /// Every drawable row in a feed. Hidden here, by rule:
    ///   - a ghost the daemon matched to a real event (the event is drawn);
    ///   - a dismissed ghost, unless asked for;
    ///   - a real event the user declined;
    ///   - anything whose start does not parse.
    static func items(
        _ feed: TimelineFeed, calendar: Calendar, showDismissed: Bool = false
    ) -> [AgendaItem] {
        var out: [AgendaItem] = []
        out.reserveCapacity(feed.ghosts.count + (feed.events?.count ?? 0))
        for event in feed.events ?? [] {
            if let item = item(event, calendar: calendar) { out.append(item) }
        }
        for ghost in feed.ghosts {
            if let item = item(ghost, calendar: calendar, showDismissed: showDismissed) {
                out.append(item)
            }
        }
        return out
    }

    static func item(
        _ ghost: TimelineGhost, calendar: Calendar, showDismissed: Bool = false
    ) -> AgendaItem? {
        guard ghost.matched_event == nil else { return nil }
        guard showDismissed || ghost.state != .dismissed else { return nil }
        guard let start = AgendaTime.parse(ghost.start, zone: ghost.start_timezone, calendar: calendar)
        else { return nil }
        let allDay = ghost.all_day || start.dateOnly
        var end: Date?
        if let raw = ghost.end,
            let parsed = AgendaTime.parse(raw, zone: ghost.start_timezone, calendar: calendar)
        {
            // A ghost's date-only end names its LAST day (it is the user's
            // "through the 19th"), so the exclusive bound is the day after.
            end = parsed.dateOnly ? dayAfter(parsed.date, calendar) : parsed.date
        }
        if allDay, end == nil { end = dayAfter(start.date, calendar) }
        // The flag is the daemon's word; the tier is the fallback for a shape
        // that omitted it. Only stated and user ends are facts.
        let estimated =
            ghost.end_estimated
            ?? (ghost.end_source.map { $0 != .stated && $0 != .user } ?? false)
        let certain = ghost.commitment == .committed || ghost.state == .going
        let band = certain ? nil : ghost.likelihood?.band
        let fill: AgendaFill =
            certain ? .solid : (band == .likely ? .striped : .outline)
        return AgendaItem(
            id: "g:\(ghost.id)",
            origin: .ghost(ghost),
            title: tidy(ghost.title) ?? "Untitled plan",
            location: tidy(ghost.location),
            start: start.date,
            end: end,
            allDay: allDay,
            endEstimated: estimated && end != nil && !allDay,
            fill: fill,
            band: band,
            cancelled: ghost.state == .cancelled || ghost.mail_state == .cancelled,
            unverified: ghost.verified == false,
            folded: !certain && band == .unlikely)
    }

    static func item(_ event: TimelineEvent, calendar: Calendar) -> AgendaItem? {
        guard event.self_response != .declined else { return nil }
        guard let start = AgendaTime.parse(event.start, zone: event.start_timezone, calendar: calendar)
        else { return nil }
        let allDay = event.all_day || start.dateOnly
        var end: Date?
        if let raw = event.end,
            let parsed = AgendaTime.parse(raw, zone: event.start_timezone, calendar: calendar)
        {
            // The daemon serves an all-day event's end the way it serves a
            // ghost's: the LAST day, inclusive. (Google's own exclusive end is
            // converted there, so both kinds of row read one convention.)
            end = parsed.dateOnly ? dayAfter(parsed.date, calendar) : parsed.date
        }
        if allDay, end == nil || end! <= start.date { end = dayAfter(start.date, calendar) }
        return AgendaItem(
            id: itemId(event),
            origin: .event(event),
            title: tidy(event.title) ?? "Untitled event",
            location: tidy(event.location),
            start: start.date,
            end: end,
            allDay: allDay,
            endEstimated: false,
            fill: .solid,
            band: nil,
            cancelled: false,
            unverified: false,
            folded: false)
    }

    /// A real event's row id: its calendar and its id, since Google's ids are
    /// unique only within a calendar. The editor selects a saved event by it.
    static func itemId(_ event: TimelineEvent) -> String {
        "e:\(event.calendar_id)|\(event.id)"
    }

    /// The order inside a day: all-day first, then by start; at the same
    /// instant the real event before the ghost, then the more solid first;
    /// then title, then id, so a re-render never reshuffles equals.
    static func precedes(_ a: AgendaItem, _ b: AgendaItem) -> Bool {
        if a.allDay != b.allDay { return a.allDay }
        if a.start != b.start { return a.start < b.start }
        if a.isGhost != b.isGhost { return !a.isGhost }
        if a.fill != b.fill { return a.fill < b.fill }
        if a.title != b.title { return a.title.localizedStandardCompare(b.title) == .orderedAscending }
        return a.id < b.id
    }

    /// Today and the days after it, in the user's zone. `from` starts the
    /// list on another day instead (the phone's selected day, which can be
    /// last week or next month); nil is today.
    ///
    /// A row lands on the day it STARTS. The exception is one still running
    /// when the first day began (last night's party, a trip that started
    /// Sunday): it is listed under that day, because that is when it is in
    /// the way. Anything that ended before the first day began is gone. The
    /// first day is always returned, empty or not; any other day only when it
    /// has something on it. Only TODAY folds what has finished into "earlier".
    static func days(
        _ items: [AgendaItem], now: Date, calendar: Calendar, from anchor: Date? = nil
    ) -> [AgendaDay] {
        let today = calendar.startOfDay(for: now)
        let first = anchor.map { calendar.startOfDay(for: $0) } ?? today
        var byDay: [Date: [AgendaItem]] = [first: []]
        for item in items {
            let day = calendar.startOfDay(for: item.start)
            if day < first {
                // Ran into the first day at all: listed there, and the
                // earlier fold below takes it if it has finished since.
                guard item.effectiveEnd > first else { continue }
                byDay[first, default: []].append(item)
            } else {
                byDay[day, default: []].append(item)
            }
        }
        return byDay.keys.sorted().compactMap { day in
            let rows = (byDay[day] ?? []).sorted(by: precedes)
            var visible: [AgendaItem] = []
            var unlikely: [AgendaItem] = []
            var earlier: [AgendaItem] = []
            for row in rows {
                if day == today, !row.allDay, row.effectiveEnd <= now {
                    earlier.append(row)
                } else if row.folded {
                    unlikely.append(row)
                } else {
                    visible.append(row)
                }
            }
            let built = AgendaDay(day: day, visible: visible, unlikely: unlikely, earlier: earlier)
            return day == first || day == today || !(visible.isEmpty && unlikely.isEmpty) ? built : nil
        }
    }

    /// `count` days from `first`, EVERY one of them, the empty ones too: the
    /// phone's week, which lists a quiet Tuesday rather than skipping it.
    /// Built by `days(from:)`, so a row lands, folds and orders exactly as it
    /// does in the agenda; past the last day nothing is returned.
    static func run(
        _ items: [AgendaItem], now: Date, calendar: Calendar, from first: Date, count: Int
    ) -> [AgendaDay] {
        let start = calendar.startOfDay(for: first)
        var byDay: [Date: AgendaDay] = [:]
        for day in days(items, now: now, calendar: calendar, from: start) { byDay[day.day] = day }
        return (0..<max(0, count)).compactMap { offset in
            guard let day = calendar.date(byAdding: .day, value: offset, to: start) else { return nil }
            return byDay[day] ?? AgendaDay(day: day, visible: [], unlikely: [], earlier: [])
        }
    }

    // MARK: next

    /// The page's callout: what the user has to be at. Only COMMITMENTS
    /// (solid rows), never a maybe, never something cancelled or all-day. A
    /// commitment already running wins over the next one to start.
    static func next(_ days: [AgendaDay], now: Date) -> (item: AgendaItem, ongoing: Bool)? {
        let candidates = days.flatMap(\.visible).filter {
            $0.fill == .solid && !$0.cancelled && !$0.allDay
        }
        if let running = candidates.first(where: { $0.start <= now && now < $0.effectiveEnd }) {
            return (running, true)
        }
        if let upcoming = candidates.first(where: { $0.start > now }) {
            return (upcoming, false)
        }
        return nil
    }

    /// "Now, until 5pm", "Next, in 50 min", "Next, at 4:30pm", "Next,
    /// tomorrow at 10am", "Next, Wednesday at 11am", "Next, Oct 21 at 11am".
    static func nextLead(
        _ item: AgendaItem, ongoing: Bool, now: Date, calendar: Calendar
    ) -> String {
        if ongoing {
            guard let end = item.end else { return "Now" }
            return "Now, until \(item.endEstimated ? "~" : "")\(clock(end, calendar))"
        }
        let minutes = Int((item.start.timeIntervalSince(now) / 60).rounded(.up))
        if minutes <= 60 { return "Next, in \(max(1, minutes)) min" }
        let at = clock(item.start, calendar)
        let days = dayDistance(from: now, to: item.start, calendar)
        switch days {
        case 0: return "Next, at \(at)"
        case 1: return "Next, tomorrow at \(at)"
        case 2...6: return "Next, \(format(item.start, "EEEE", calendar)) at \(at)"
        default: return "Next, \(format(item.start, "MMM d", calendar)) at \(at)"
        }
    }

    // MARK: strings

    /// Compact wall clock: "9am", "6:30pm". The same shape as `Fmt`'s chip
    /// clock, rebuilt here so this file stays standalone.
    static func clock(_ date: Date, _ calendar: Calendar) -> String {
        let parts = calendar.dateComponents([.hour, .minute], from: date)
        let hour24 = parts.hour ?? 0
        let minute = parts.minute ?? 0
        let suffix = hour24 < 12 ? "am" : "pm"
        let hour = hour24 % 12 == 0 ? 12 : hour24 % 12
        return minute == 0 ? "\(hour)\(suffix)" : "\(hour):\(String(format: "%02d", minute))\(suffix)"
    }

    /// The time column of a row listed under `day`: "all day", "8pm", or
    /// "until 6am" for one that began before the day did.
    static func timeLabel(_ item: AgendaItem, on day: Date, calendar: Calendar) -> String {
        if item.allDay { return "all day" }
        if item.start < day {
            guard let end = item.end else { return "ongoing" }
            return "until \(item.endEstimated ? "~" : "")\(clock(end, calendar))"
        }
        return clock(item.start, calendar)
    }

    /// The end, as a row prints it after the title.
    struct EndLabel: Equatable, Sendable {
        let text: String
        /// Drawn softer than a stated end, tilde and all.
        let estimated: Bool
    }

    /// "ends ~11pm" for a guessed end, always: a guess is the one end worth
    /// a row's space, because it is what blocks the evening. A STATED end is
    /// printed only when it lands on another day ("ends 2am Sat"); the rest
    /// are in the detail, or the agenda turns into a timetable. An all-day
    /// row spanning several days says through when ("until Oct 19"). Nil for
    /// a timed row listed under a later day than its start: its time column
    /// already says "until".
    static func endLabel(_ item: AgendaItem, on day: Date, calendar: Calendar) -> EndLabel? {
        guard let end = item.end else { return nil }
        if item.allDay {
            // An all-day row's time column only ever says "all day", so the
            // span is said here even under a later day than its start (a
            // trip that began Sunday, listed under today): through when,
            // whenever that is after the day it is listed under.
            let listed = max(calendar.startOfDay(for: item.start), calendar.startOfDay(for: day))
            guard let last = calendar.date(byAdding: .day, value: -1, to: end),
                calendar.startOfDay(for: last) > listed
            else { return nil }
            return EndLabel(text: "until \(format(last, "MMM d", calendar))", estimated: false)
        }
        guard item.start >= day else { return nil }
        let crosses = !calendar.isDate(end, inSameDayAs: item.start)
            && end.timeIntervalSince(calendar.startOfDay(for: end)) > 0
        let when = clock(end, calendar) + (crosses ? " \(format(end, "EEE", calendar))" : "")
        if item.endEstimated { return EndLabel(text: "ends ~\(when)", estimated: true) }
        return crosses ? EndLabel(text: "ends \(when)", estimated: false) : nil
    }

    /// A day's header: the word and the date beside it. "Today" / "Thu Oct 1",
    /// "Tomorrow" / "Fri Oct 2", "Wednesday" / "Oct 7" inside the week, and
    /// past that the date leads ("Oct 21" / "Wednesday"), because a bare
    /// weekday two weeks out names no particular one.
    static func dayTitle(_ day: Date, now: Date, calendar: Calendar) -> (title: String, detail: String) {
        switch dayDistance(from: now, to: day, calendar) {
        case 0: return ("Today", format(day, "EEE MMM d", calendar))
        case 1: return ("Tomorrow", format(day, "EEE MMM d", calendar))
        case 2...6: return (format(day, "EEEE", calendar), format(day, "MMM d", calendar))
        default: return (format(day, "MMM d", calendar), format(day, "EEEE", calendar))
        }
    }

    /// The detail's full time, split so the view can soften the guessed end:
    /// "Thu Oct 8, 8pm" and "11pm" (estimated), or "Wed Oct 14" and "Mon Oct
    /// 19" for a span of days, or "Thu Oct 8" with "all day".
    static func fullTime(_ item: AgendaItem, calendar: Calendar) -> (lead: String, end: String?) {
        let date = format(item.start, "EEE MMM d", calendar)
        if item.allDay {
            if let end = item.end, let last = calendar.date(byAdding: .day, value: -1, to: end),
                calendar.startOfDay(for: last) > calendar.startOfDay(for: item.start)
            {
                return (date, format(last, "EEE MMM d", calendar))
            }
            return ("\(date), all day", nil)
        }
        let lead = "\(date), \(clock(item.start, calendar))"
        guard let end = item.end, end > item.start else { return (lead, nil) }
        let sameDay = calendar.isDate(end, inSameDayAs: item.start)
        let tail = sameDay ? clock(end, calendar) : "\(format(end, "EEE", calendar)) \(clock(end, calendar))"
        return (lead, tail)
    }

    /// When the calendar last synced, for the "could not refresh" line: "at
    /// 4:12pm", "yesterday at 4:12pm", "Oct 3 at 4:12pm".
    static func lastSynced(_ date: Date, now: Date, calendar: Calendar) -> String {
        let at = "at \(clock(date, calendar))"
        switch dayDistance(from: now, to: date, calendar) {
        case 0: return at
        case -1: return "yesterday \(at)"
        default: return "\(format(date, "MMM d", calendar)) \(at)"
        }
    }

    /// How real a ghost is, as the detail's "Status" or "Likelihood" row.
    enum Standing: Equatable, Sendable {
        /// A plain sentence derived from the wire's own fields.
        case status(String)
        /// The band, drawn beside its swatch, with the model's reason quoted.
        case likelihood(GhostLikelihood)
    }

    /// Nil when there is nothing true to say: a ghost the daemon could not
    /// place (`commitment` unknown) and gave no likelihood.
    static func standing(_ ghost: TimelineGhost) -> Standing? {
        let mailCancelled = ghost.state == .cancelled || ghost.mail_state == .cancelled
        if mailCancelled {
            // The user's word and the mail's are two facts, and both are
            // said: a going over a cancellation is a plan to check, not one
            // to quietly drop.
            return .status(
                ghost.state == .going
                    ? "You said you are going. The mail says it was cancelled."
                    : "Cancelled, according to the mail")
        }
        if ghost.state == .going { return .status("You said you are going") }
        if ghost.commitment == .committed { return .status("Booked, bought or accepted") }
        if let likelihood = ghost.likelihood { return .likelihood(likelihood) }
        switch ghost.commitment {
        case .invited: return .status("Invited, not answered")
        case .proposed: return .status("Proposed, not agreed")
        case .unknown, .committed: return nil
        }
    }

    // MARK: today's strip

    /// The waking hours today's strip spans: 7am to midnight. Something that
    /// began earlier and runs past 7am is clamped onto its left edge rather
    /// than widening the axis, so the strip's shape means the same thing
    /// every day; something over by 7am is not on the strip at all.
    static let stripFromHour = 7

    /// Where a row sits on today's strip, as fractions of its width, or nil
    /// when it is all-day or misses the strip entirely.
    static func stripSpan(_ item: AgendaItem, day: Date, calendar: Calendar) -> ClosedRange<Double>? {
        guard !item.allDay,
            let from = calendar.date(byAdding: .hour, value: stripFromHour, to: day),
            let to = calendar.date(byAdding: .day, value: 1, to: day),
            // Against the strip's own start, not the day's: a 1:30am finish
            // would otherwise be drawn as a quarter-hour sliver at 7am.
            item.start < to, item.effectiveEnd > from
        else { return nil }
        let width = to.timeIntervalSince(from)
        let lo = max(0, min(1, item.start.timeIntervalSince(from) / width))
        let hi = max(0, min(1, item.effectiveEnd.timeIntervalSince(from) / width))
        // A sliver still has to be seen: at least a quarter hour wide.
        let minimum = 15 * 60 / width
        let low = min(lo, 1 - minimum)
        return low...min(1, max(hi, low + minimum))
    }

    /// Now, as a fraction of today's strip.
    static func stripNow(_ now: Date, day: Date, calendar: Calendar) -> Double? {
        guard let from = calendar.date(byAdding: .hour, value: stripFromHour, to: day),
            let to = calendar.date(byAdding: .day, value: 1, to: day),
            now >= from, now < to
        else { return nil }
        return now.timeIntervalSince(from) / to.timeIntervalSince(from)
    }

    // MARK: helpers

    private static func tidy(_ s: String?) -> String? {
        guard let s else { return nil }
        let collapsed = s.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        return collapsed.isEmpty ? nil : collapsed
    }

    private static func dayAfter(_ date: Date, _ calendar: Calendar) -> Date {
        calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: date)) ?? date
    }

    private static func dayDistance(from now: Date, to date: Date, _ calendar: Calendar) -> Int {
        calendar.dateComponents(
            [.day], from: calendar.startOfDay(for: now), to: calendar.startOfDay(for: date)
        ).day ?? 0
    }

    /// English, in the calendar's zone. The app's copy is English throughout,
    /// and a fixed locale is what keeps "Thu Oct 8" from reading "jeu. 8 oct."
    /// beside sentences that do not translate.
    static func format(_ date: Date, _ pattern: String, _ calendar: Calendar) -> String {
        let key = "\(pattern)|\(calendar.timeZone.identifier)"
        formatterLock.lock()
        defer { formatterLock.unlock() }
        let f: DateFormatter
        if let hit = formatters[key] {
            f = hit
        } else {
            f = DateFormatter()
            f.locale = Locale(identifier: "en_US_POSIX")
            f.calendar = Calendar(identifier: .gregorian)
            f.timeZone = calendar.timeZone
            f.dateFormat = pattern
            formatters[key] = f
        }
        return f.string(from: date)
    }

    nonisolated(unsafe) private static var formatters: [String: DateFormatter] = [:]
    private static let formatterLock = NSLock()
}

// MARK: - wire times

/// The timeline's two time shapes: `YYYY-MM-DD` (a date, floating: it is that
/// date wherever the user is) and RFC3339 with an offset (an instant). A wall
/// time with no offset is read in the row's own zone, for a shape the daemon
/// does not send today but `CalendarVisibility` already tolerates.
enum AgendaTime {
    struct Parsed: Hashable, Sendable {
        let date: Date
        let dateOnly: Bool
    }

    /// MEMOIZED: the page re-derives every row on each poll tick, and a parse
    /// builds nothing but still runs ICU. Keyed by the string and the zones it
    /// could be read in, so a zone change is a miss rather than a stale hit.
    static func parse(_ raw: String, zone: String?, calendar: Calendar) -> Parsed? {
        let value = raw.trimmingCharacters(in: .whitespaces)
        let key = "\(value)|\(zone ?? "")|\(calendar.timeZone.identifier)"
        lock.lock()
        defer { lock.unlock() }
        if let hit = memo[key] { return hit }
        if memo.count >= 2_000 { memo.removeAll(keepingCapacity: true) }
        let parsed = uncachedParse(value, zone: zone, calendar: calendar)
        memo[key] = parsed
        return parsed
    }

    private static func uncachedParse(_ value: String, zone: String?, calendar: Calendar) -> Parsed? {
        if value.count == 10 {
            let parts = value.split(separator: "-").compactMap { Int($0) }
            guard parts.count == 3 else { return nil }
            var c = DateComponents()
            (c.year, c.month, c.day) = (parts[0], parts[1], parts[2])
            // A date-only value is that DATE in the user's zone, not midnight
            // UTC: an all-day Thursday is Thursday in San Francisco and Tokyo.
            guard let date = calendar.date(from: c),
                calendar.component(.day, from: date) == parts[2],
                calendar.component(.month, from: date) == parts[1]
            else { return nil }
            return Parsed(date: date, dateOnly: true)
        }
        if let date = isoFractional.date(from: value) ?? isoPlain.date(from: value) {
            return Parsed(date: date, dateOnly: false)
        }
        let wallZone = zone.flatMap(TimeZone.init(identifier:)) ?? calendar.timeZone
        wall.timeZone = wallZone
        for format in ["yyyy-MM-dd'T'HH:mm:ss", "yyyy-MM-dd'T'HH:mm"] {
            wall.dateFormat = format
            if let date = wall.date(from: value) { return Parsed(date: date, dateOnly: false) }
        }
        return nil
    }

    nonisolated(unsafe) private static var memo: [String: Parsed?] = [:]
    private static let lock = NSLock()
    // Only ever touched under `lock`.
    nonisolated(unsafe) private static let isoFractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()
    nonisolated(unsafe) private static let isoPlain: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()
    private static let wall: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.calendar = Calendar(identifier: .gregorian)
        f.isLenient = false
        return f
    }()
}
