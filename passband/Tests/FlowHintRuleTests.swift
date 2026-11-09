// Flow mode's one-time explanation (Lib/FlowHintRule.swift): a first walk on
// a Mac that has never walked gets it; anyone who has walked, or has seen it,
// never does.

import Foundation

@main
struct FlowHintRuleTests {
    static func main() {
        typealias R = FlowHintRule
        expect(!R.everWalked(walkDeep: 0, walkFinished: 0, flow: .unseen), "never walked")
        expect(R.everWalked(walkDeep: 1, walkFinished: 0, flow: .unseen), "a deep walk counts")
        expect(R.everWalked(walkDeep: 0, walkFinished: 1, flow: .unseen), "a finished walk counts")
        expect(R.everWalked(walkDeep: 0, walkFinished: 0, flow: .found), "flow found counts")
        expect(R.everWalked(walkDeep: 0, walkFinished: 0, flow: .kept), "flow kept counts")
        expect(R.owed(seen: false, everWalked: false), "a first walk ever is owed it")
        expect(!R.owed(seen: true, everWalked: false), "once per Mac")
        expect(!R.owed(seen: false, everWalked: true), "an existing walker never sees it")
        print("Flow hint rule tests passed")
    }

    static func expect(_ ok: Bool, _ what: String) {
        guard ok else { fatalError(what) }
    }
}
