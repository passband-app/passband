import Foundation

@main
struct HiddenFeaturesTests {
    static func main() throws {
        let calendar = HiddenFeature(key: "calendar")
        let agentSend = HiddenFeature(key: "agent_send")

        // Off is the answer to every doubt: no stats, an old daemon, an empty list.
        precondition(!HiddenFeatures.none.has(calendar))
        precondition(!HiddenFeatures(daemon: nil).has(calendar), "A daemon too old to say grants nothing")
        precondition(!HiddenFeatures(daemon: []).has(calendar))

        let some = HiddenFeatures(daemon: ["calendar", "something_newer_than_this_build"])
        precondition(some.has(calendar))
        precondition(!some.has(agentSend), "One grant is not every grant")
        precondition(!some.has(HiddenFeature(key: "Calendar")), "Keys are compared exactly")

        // Equality is by content, so a view observing it does not redraw on
        // every stats poll that says the same thing in a different order.
        precondition(HiddenFeatures(daemon: ["a", "b"]) == HiddenFeatures(daemon: ["b", "a", "a"]))
        precondition(HiddenFeatures(daemon: nil) == HiddenFeatures(daemon: []))

        // The wire shape: a bare JSON array of strings, and absence is nil.
        struct Stats: Decodable { var features: [String]? }
        let said = try JSONDecoder().decode(Stats.self, from: Data(#"{"features":["calendar"]}"#.utf8))
        precondition(HiddenFeatures(daemon: said.features).has(calendar))
        let silent = try JSONDecoder().decode(Stats.self, from: Data("{}".utf8))
        precondition(silent.features == nil && !HiddenFeatures(daemon: silent.features).has(calendar))

        print("ok: 10 hidden feature checks passed")
    }
}
