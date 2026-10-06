// DRAG TO CREATE, THE ARITHMETIC: a press and a drag down (or up) the day
// column's hours become a span of time, snapped to the quarter hour.
//
// PURE, and Foundation only (Tests/DaySweepTests.swift). The column's own
// geometry is an argument: the night folded into a thin cap at the top
// (midnight to 7am in `nightHeight` points), then the hours at `pph` points
// each. A point in the cap is read at the cap's own scale, so the top of the
// well is midnight and its foot is midnight again, and a sweep can never
// leave the day.
//
// The view (Views/CalendarMonth.swift, `HoursGrid`) holds the two points and
// draws what this says; the release hands the two times to the event pane.

import Foundation

/// The column's vertical scale, in points.
struct DaySweepScale: Hashable, Sendable {
    /// The folded night's height: midnight to `fromHour`.
    let nightHeight: Double
    /// Points per hour from `fromHour` to midnight.
    let pointsPerHour: Double
    /// Where the hours begin (the agenda's 7am).
    let fromHour: Int

    var nightMinutes: Double { Double(fromHour) * 60 }
    var height: Double { nightHeight + Double(24 - fromHour) * pointsPerHour }
}

/// A swept span: minutes after midnight, `start < end`, both on the quarter
/// hour and inside the day (`end` can be 1440, midnight at the day's foot).
struct DaySweepSpan: Hashable, Sendable {
    let start: Int
    let end: Int
}

enum DaySweep {
    /// The snap, and the shortest thing a sweep makes.
    static let step = 15
    /// Under this many points the press was a click, and a click on the empty
    /// well does nothing new.
    static let clickSlop = 4.0

    /// The wall time at a height in the well, unsnapped, clamped to the day.
    static func minutes(at y: Double, _ scale: DaySweepScale) -> Double {
        if y <= 0 { return 0 }
        if y < scale.nightHeight {
            guard scale.nightHeight > 0 else { return scale.nightMinutes }
            return y / scale.nightHeight * scale.nightMinutes
        }
        guard scale.pointsPerHour > 0 else { return scale.nightMinutes }
        return min(1440, scale.nightMinutes + (y - scale.nightHeight) / scale.pointsPerHour * 60)
    }

    /// The height of a wall time: the inverse, for drawing the swept block.
    static func y(of minutes: Int, _ scale: DaySweepScale) -> Double {
        let m = Double(max(0, min(1440, minutes)))
        if m <= scale.nightMinutes {
            guard scale.nightMinutes > 0 else { return 0 }
            return m / scale.nightMinutes * scale.nightHeight
        }
        return scale.nightHeight + (m - scale.nightMinutes) / 60 * scale.pointsPerHour
    }

    static func snap(_ minutes: Double) -> Int {
        let s = Double(step)
        return max(0, min(1440, Int((minutes / s).rounded()) * step))
    }

    /// Whether the pointer has travelled far enough to be a sweep at all.
    static func isDrag(from y0: Double, to y1: Double) -> Bool {
        abs(y1 - y0) >= clickSlop
    }

    /// The span between where the press landed and where the pointer is. The
    /// press end is the anchor: dragging up from 3pm to 2pm is 2 to 3pm.
    /// Never shorter than a quarter hour: a sweep that snaps to nothing grows
    /// away from its anchor in the direction dragged, and back the other way
    /// only where the day's end leaves it no room.
    static func span(from y0: Double, to y1: Double, _ scale: DaySweepScale) -> DaySweepSpan {
        let anchor = snap(minutes(at: y0, scale))
        let head = snap(minutes(at: y1, scale))
        var lo = min(anchor, head)
        var hi = max(anchor, head)
        if hi - lo < step {
            if y1 >= y0 { hi = lo + step } else { lo = hi - step }
            if hi > 1440 { (lo, hi) = (1440 - step, 1440) }
            if lo < 0 { (lo, hi) = (0, step) }
        }
        return DaySweepSpan(start: lo, end: hi)
    }

    /// The label on the block while it is swept: "2:15 to 3:30pm", "11:45am
    /// to 1pm", "11pm to 12am". The first meridiem goes when both share it,
    /// as on the column's own blocks.
    static func label(_ span: DaySweepSpan) -> String {
        let end = clock(span.end)
        var start = clock(span.start)
        if (span.start < 720) == (span.end % 1440 < 720), span.end < 1440 { start = String(start.dropLast(2)) }
        return "\(start) to \(end)"
    }

    /// "9am", "7:45pm"; 1440 is "12am".
    static func clock(_ minutes: Int) -> String {
        let h24 = (minutes / 60) % 24
        let m = minutes % 60
        let hour = h24 % 12 == 0 ? 12 : h24 % 12
        let suffix = h24 < 12 ? "am" : "pm"
        return m == 0 ? "\(hour)\(suffix)" : "\(hour):\(String(format: "%02d", m))\(suffix)"
    }
}
