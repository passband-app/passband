import Foundation

/// First viewing is durable and account-scoped. Repeated views never extend it.
@MainActor
final class CalendarVisibility {
    static let shared = CalendarVisibility()
    static let lifetime: TimeInterval = 24 * 3600
    private let defaults: UserDefaults

    /// The scope the fixture inbox stamps under. Its item ids are fixed
    /// fixtures, so without `forget` a stamp from one session would hide the
    /// same events from every session that starts more than a day later.
    static let fixtureAccount = "fixtures"

    init(defaults: UserDefaults = .standard) { self.defaults = defaults }

    private func key(account: String, item: Int) -> String {
        "\(Self.prefix)\(account).\(item)"
    }
    private static let prefix = "passband.calendar.firstSeen."

    func markSeen(account: String, item: Int, now: Date = Date()) {
        let key = key(account: account, item: item)
        guard defaults.object(forKey: key) == nil else { return }
        defaults.set(now.timeIntervalSince1970, forKey: key)
    }

    /// Drop every first-viewing stamp under one account, so its items start
    /// their timer again the next time they are shown. The fixture inbox
    /// calls this on entry: every launch is a fresh mailbox.
    func forget(account: String) {
        let scope = "\(Self.prefix)\(account)."
        for key in defaults.dictionaryRepresentation().keys where key.hasPrefix(scope) {
            defaults.removeObject(forKey: key)
        }
    }

    /// SupportedTime may carry a local wall time and a separate IANA zone.
    /// Explicit offsets win; missing zones use the user's current calendar.
    static func startDate(_ value: String?, timezone: String?, calendar: Calendar = .current) -> Date? {
        guard let value else { return nil }
        // MEMOIZED, and the formatters are built once: this is asked per
        // calendar row from a view body (and again every thirty seconds by
        // the zone's clock), and it used to build two formatters and try four
        // formats with a round trip each, every time.
        let zone = timezone.flatMap(TimeZone.init(identifier:)) ?? calendar.timeZone
        let key = "\(value)|\(zone.identifier)"
        parseLock.lock()
        defer { parseLock.unlock() }
        if let hit = parsed[key] { return hit }
        if parsed.count >= 500 { parsed.removeAll(keepingCapacity: true) }
        let date = parseStart(value, zone: zone)
        parsed[key] = date
        return date
    }

    nonisolated(unsafe) private static var parsed: [String: Date?] = [:]
    private static let parseLock = NSLock()
    private static let isoFractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()
    private static let isoPlain: ISO8601DateFormatter = {
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

    /// Under `parseLock`: none of the formatters above is safe to share
    /// without one.
    private static func parseStart(_ value: String, zone: TimeZone) -> Date? {
        if let date = isoFractional.date(from: value) { return date }
        if let date = isoPlain.date(from: value) { return date }
        let formatter = wall
        formatter.timeZone = zone
        for format in ["yyyy-MM-dd", "yyyy-MM-dd'T'HH:mm:ss", "yyyy-MM-dd'T'HH:mm", "yyyy-MM-dd'T'HH:mm:ss.SSS"] {
            formatter.dateFormat = format
            if let date = formatter.date(from: value), formatter.string(from: date) == value {
                return date
            }
        }
        return nil
    }

    func admits(account: String, item: Int, startValue: String?, timezone: String?,
                now: Date, calendar: Calendar = .current) -> Bool {
        var eventCalendar = calendar
        if let timezone, let zone = TimeZone(identifier: timezone) { eventCalendar.timeZone = zone }
        return admits(account: account, item: item,
                      start: Self.startDate(startValue, timezone: timezone, calendar: eventCalendar),
                      allDay: startValue?.count == 10, now: now, calendar: eventCalendar)
    }

    func admits(account: String, item: Int, start: Date?, allDay: Bool,
                now: Date, calendar: Calendar = .current) -> Bool {
        if let start {
            // A date-only event remains current through its entire local day.
            let cutoff = allDay
                ? calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: start)) ?? start
                : start
            guard now < cutoff else { return false }
        }
        let key = key(account: account, item: item)
        guard defaults.object(forKey: key) != nil else { return true }
        let seen = Date(timeIntervalSince1970: defaults.double(forKey: key))
        return now < seen.addingTimeInterval(Self.lifetime)
    }
}
