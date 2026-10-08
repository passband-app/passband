// Drag to create, against a fixed column (Lib/DaySweep.swift): a 22pt night
// cap over seventeen hours at 40pt each, which is the day column on a tall
// window. Each rule is wrong only under somebody's pointer: a sweep that
// lands a minute off the quarter, a drag upward that makes an event ending
// before it starts, a click that opens the pane, a drag off the bottom of the
// well that runs past midnight.

import Foundation

@main
struct DaySweepTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    /// Night cap 22pt (midnight to 7am), then 40pt an hour: 7am is y 22, noon
    /// y 222, midnight y 702.
    static let scale = DaySweepScale(nightHeight: 22, pointsPerHour: 40, fromHour: 7)

    static func y(_ hour: Double) -> Double { 22 + (hour - 7) * 40 }

    static func main() {
        theScale()
        snapsToTheQuarterHour()
        aClickIsNotASweep()
        downAndUp()
        neverShorterThanAQuarterHour()
        clampsAtTheDaysEnds()
        theNightCap()
        theLabel()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func theScale() {
        expect(scale.height == 702, "the well is the cap and seventeen hours")
        expect(DaySweep.minutes(at: y(7), scale) == 420, "the cap's foot is 7am")
        expect(DaySweep.minutes(at: y(12), scale) == 720, "noon")
        expect(DaySweep.minutes(at: y(19.75), scale) == 1185, "7:45pm")
        expect(DaySweep.y(of: 720, scale) == y(12) && DaySweep.y(of: 1440, scale) == 702,
               "and back: a time's height")
        for m in stride(from: 0, through: 1440, by: 45) {
            expect(abs(DaySweep.minutes(at: DaySweep.y(of: m, scale), scale) - Double(m)) < 1e-6,
                   "a time's height reads back as that time (\(m))")
        }
    }

    static func snapsToTheQuarterHour() {
        expect(DaySweep.snap(14 * 60 + 7) == 14 * 60, "2:07 is 2:00")
        expect(DaySweep.snap(14 * 60 + 8) == 14 * 60 + 15, "2:08 is 2:15")
        expect(DaySweep.snap(14 * 60 + 52.4) == 14 * 60 + 45, "2:52 is 2:45")
        // 2:10pm to 3:20pm under the pointer is 2:15 to 3:15.
        let s = DaySweep.span(from: y(14) + 40 * 10 / 60, to: y(15) + 40 * 20 / 60, scale)
        expect(s == DaySweepSpan(start: 14 * 60 + 15, end: 15 * 60 + 15), "both ends land on the quarter")
        expect(s.start % 15 == 0 && s.end % 15 == 0, "always")
    }

    static func aClickIsNotASweep() {
        expect(!DaySweep.isDrag(from: 300, to: 300), "a press that does not move is a click")
        expect(!DaySweep.isDrag(from: 300, to: 303.9) && !DaySweep.isDrag(from: 300, to: 296.5),
               "under four points either way is still a click")
        expect(DaySweep.isDrag(from: 300, to: 304) && DaySweep.isDrag(from: 300, to: 295),
               "four points is a sweep, down or up")
    }

    static func downAndUp() {
        let down = DaySweep.span(from: y(14), to: y(15.5), scale)
        expect(down == DaySweepSpan(start: 840, end: 930), "2pm dragged down to 3:30")
        let up = DaySweep.span(from: y(15.5), to: y(14), scale)
        expect(up == down, "the same two points dragged upward are the same span")
        expect(up.start < up.end, "start is always the earlier end")
        let across = DaySweep.span(from: y(11.75), to: y(13), scale)
        expect(across == DaySweepSpan(start: 705, end: 780), "11:45am to 1pm, across noon")
    }

    static func neverShorterThanAQuarterHour() {
        let tiny = DaySweep.span(from: y(14), to: y(14) + 5, scale)
        expect(tiny == DaySweepSpan(start: 840, end: 855), "a short drag down is 2 to 2:15")
        let tinyUp = DaySweep.span(from: y(14), to: y(14) - 5, scale)
        expect(tinyUp == DaySweepSpan(start: 825, end: 840), "a short drag up is 1:45 to 2: it grows the way it went")
        let sameSlot = DaySweep.span(from: y(14) + 2, to: y(14) + 7, scale)
        expect(sameSlot.end - sameSlot.start == 15, "two points that snap to one quarter still make a quarter hour")
    }

    static func clampsAtTheDaysEnds() {
        let off = DaySweep.span(from: y(23), to: 5000, scale)
        expect(off == DaySweepSpan(start: 1380, end: 1440), "dragged off the bottom: 11pm to midnight")
        let top = DaySweep.span(from: y(8), to: -400, scale)
        expect(top == DaySweepSpan(start: 0, end: 480), "dragged off the top: midnight to 8am")
        let foot = DaySweep.span(from: 702, to: 700, scale)
        expect(foot == DaySweepSpan(start: 1425, end: 1440), "a nudge at the very foot is the day's last quarter")
        let footDown = DaySweep.span(from: 701, to: 900, scale)
        expect(footDown == DaySweepSpan(start: 1425, end: 1440), "and one dragged down from there cannot leave the day")
        let head = DaySweep.span(from: 0.2, to: -30, scale)
        expect(head == DaySweepSpan(start: 0, end: 15), "the very top is the first quarter")
        expect(DaySweep.minutes(at: -10, scale) == 0 && DaySweep.minutes(at: 9999, scale) == 1440, "clamped both ways")
    }

    /// The folded night reads at its own scale: 22pt for seven hours.
    static func theNightCap() {
        expect(DaySweep.minutes(at: 11, scale) == 210, "half way down the cap is 3:30am")
        let early = DaySweep.span(from: 11, to: y(8), scale)
        expect(early == DaySweepSpan(start: 210, end: 480), "from the cap into the morning: 3:30 to 8am")
        expect(DaySweep.y(of: 210, scale) == 11, "and its block starts back in the cap")
        let flat = DaySweepScale(nightHeight: 0, pointsPerHour: 40, fromHour: 7)
        expect(DaySweep.minutes(at: 0, flat) == 0 && DaySweep.minutes(at: 40, flat) == 480,
               "a column with no cap still reads")
        let dead = DaySweepScale(nightHeight: 22, pointsPerHour: 0, fromHour: 7)
        expect(DaySweep.minutes(at: 300, dead) == 420, "and one with no height never divides by it")
    }

    static func theLabel() {
        func label(_ a: Int, _ b: Int) -> String { DaySweep.label(DaySweepSpan(start: a, end: b)) }
        expect(label(855, 930) == "2:15 to 3:30pm", "one meridiem, said once")
        expect(label(705, 780) == "11:45am to 1pm", "across noon, both said")
        expect(label(540, 600) == "9 to 10am", "whole hours")
        expect(label(1380, 1440) == "11pm to 12am", "to midnight")
        expect(label(0, 15) == "12 to 12:15am", "from midnight")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
