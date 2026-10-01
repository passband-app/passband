// THE DENSITY CURVES, which every padding and font size in the app is read
// through, so a mistake here is a mistake on every screen at once.
//
// Three promises are worth holding up without the app. Compact is the identity:
// that is what lets a literal be moved onto a token with nothing on screen
// changing. Comfortable only ever gives MORE room. And the curves keep their
// order, because a ladder where two steps land on one size has quietly lost a
// step of hierarchy that some view was relying on.

import CoreGraphics
import Foundation

@main
@MainActor
struct DensityTests {
    static var failures = 0
    static var checks = 0

    /// Every step of `Space`'s ladder, by its compact value.
    static let ladder: [CGFloat] = [2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 32]
    /// Every font size the interface sets, hero included.
    static let typeSizes: [CGFloat] = [
        8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 22, 24, 26, 28, 30, 32, 38, 40,
    ]

    static func main() {
        compactIsTheIdentity()
        comfortableOnlyAddsRoom()
        theLadderKeepsItsSteps()
        typeKeepsItsHierarchy()
        theSmallestTypeIsLiftedTheMost()
        gapsOpenMoreThanTypeGrows()
        valuesOffTheLadderFollowTheSameCurve()
        whatIsNotSpacingIsLeftAlone()
        everythingLandsOnTheHalfPoint()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func compactIsTheIdentity() {
        for v in stride(from: CGFloat(-8), through: 60, by: 0.5) {
            expect(Density.space(v, in: .compact) == v, "space \(v) moved in compact")
            expect(Density.typeSize(v, in: .compact) == v, "type \(v) moved in compact")
        }
    }

    static func comfortableOnlyAddsRoom() {
        for v in stride(from: CGFloat(0), through: 60, by: 0.5) {
            expect(Density.space(v, in: .comfortable) >= v, "space \(v) shrank")
            expect(Density.typeSize(v, in: .comfortable) >= v, "type \(v) shrank")
        }
    }

    static func theLadderKeepsItsSteps() {
        let comfortable = ladder.map { Density.space($0, in: .comfortable) }
        for (a, b) in zip(comfortable, comfortable.dropFirst()) {
            expect(b > a, "two ladder steps landed on \(a) and \(b)")
        }
        // The named anchors, so retuning the curve is a decision and not a typo.
        expect(Density.space(4, in: .comfortable) == 6, "s4")
        expect(Density.space(8, in: .comfortable) == 11, "s8")
        expect(Density.space(16, in: .comfortable) == 20, "s16")
    }

    static func typeKeepsItsHierarchy() {
        let comfortable = typeSizes.map { Density.typeSize($0, in: .comfortable) }
        for (a, b) in zip(comfortable, comfortable.dropFirst()) {
            expect(b > a, "two type sizes landed on \(a) and \(b)")
        }
        expect(Density.typeSize(13, in: .comfortable) == 14, "row text")
        expect(Density.typeSize(10, in: .comfortable) == 11, "micro")
    }

    static func theSmallestTypeIsLiftedTheMost() {
        let small = Density.typeSize(8, in: .comfortable) / 8
        let body = Density.typeSize(13, in: .comfortable) / 13
        let hero = Density.typeSize(40, in: .comfortable) / 40
        expect(small > body && body > hero, "the lift should taper: \(small) \(body) \(hero)")
    }

    /// The whole point of density over zoom: the ratio changes.
    static func gapsOpenMoreThanTypeGrows() {
        for v in [4, 6, 8, 10, 12] as [CGFloat] {
            let gap = Density.space(v, in: .comfortable) / v
            let type = Density.typeSize(13, in: .comfortable) / 13
            expect(gap > type, "a \(v)pt gap grew no faster than the type")
        }
    }

    static func valuesOffTheLadderFollowTheSameCurve() {
        expect(Density.space(5, in: .comfortable) == 7.5, "5 sits between 4 and 6")
        expect(Density.space(26, in: .comfortable) == 32, "26 sits between 24 and 32")
        expect(Density.space(48, in: .comfortable) == 57, "past the last anchor, its ratio")
        expect(Density.space(-6, in: .comfortable) == -9, "a negative offset mirrors")
    }

    static func whatIsNotSpacingIsLeftAlone() {
        expect(Density.space(0, in: .comfortable) == 0, "zero")
        expect(Density.space(0.5, in: .comfortable) == 0.5, "a hairline")
        expect(Density.space(1, in: .comfortable) == 1, "a 1pt rule")
    }

    static func everythingLandsOnTheHalfPoint() {
        for v in stride(from: CGFloat(2), through: 60, by: 1) {
            for out in [Density.space(v, in: .comfortable), Density.typeSize(v, in: .comfortable)] {
                expect((out * 2).rounded() == out * 2, "\(v) became \(out)")
            }
        }
    }

    static func expect(_ ok: Bool, _ message: String) {
        checks += 1
        if !ok {
            failures += 1
            print("  FAIL: \(message)")
        }
    }
}
