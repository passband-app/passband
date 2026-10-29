import Foundation

@main
struct HiddenFeaturesTests {
    static func main() throws {
        let renotify = HiddenFeature(key: "renotify")
        let agentSend = HiddenFeature(key: "agent_send")

        // `renotify` is the one key the registry still lists (the `calendar`
        // key is retired). Off is the answer to every doubt: no stats, an old
        // daemon, an empty list.
        precondition(!HiddenFeatures.none.has(renotify))
        precondition(!HiddenFeatures(daemon: nil).has(renotify), "A daemon too old to say grants nothing")
        precondition(!HiddenFeatures(daemon: []).has(renotify))

        let some = HiddenFeatures(daemon: ["renotify", "something_newer_than_this_build"])
        precondition(some.has(renotify))
        precondition(!some.has(agentSend), "One grant is not every grant")
        precondition(!some.has(HiddenFeature(key: "Renotify")), "Keys are compared exactly")

        // Equality is by content, so a view observing it does not redraw on
        // every stats poll that says the same thing in a different order.
        precondition(HiddenFeatures(daemon: ["a", "b"]) == HiddenFeatures(daemon: ["b", "a", "a"]))
        precondition(HiddenFeatures(daemon: nil) == HiddenFeatures(daemon: []))

        // The wire shape: a bare JSON array of strings, and absence is nil.
        struct Stats: Decodable { var features: [String]? }
        let said = try JSONDecoder().decode(Stats.self, from: Data(#"{"features":["renotify"]}"#.utf8))
        precondition(HiddenFeatures(daemon: said.features).has(renotify))
        let silent = try JSONDecoder().decode(Stats.self, from: Data("{}".utf8))
        precondition(silent.features == nil && !HiddenFeatures(daemon: silent.features).has(renotify))

        print("ok: 10 hidden feature checks passed")
    }
}
