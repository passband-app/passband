// `t`, teach Passband: what the undo chip says a sentence became, what a 404
// from an old daemon falls back to, and the wire shapes the card reads.

import Foundation

@main
@MainActor
struct TeachTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        savedLabelNamesWhoItAppliesTo()
        anOldDaemonFallsBackOnlyWithASender()
        saveIsShutOnEmptyOrTooLong()
        shortLabelClipsTheFirstLine()
        theWireDecodes()
        copyHasNoDashes()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func result(_ kind: String, _ pattern: String?) -> TeachResult {
        TeachResult(kind: kind, id: 7, text: "x", match_pattern: pattern, disposition: nil)
    }

    static func savedLabelNamesWhoItAppliesTo() {
        expect(
            Teach.savedLabel(result("general", nil)) == "got it. applies to all your mail",
            "general reads as all mail")
        var full = result("general", nil)
        full.merged = [MergedRule(id: 9, text: "m", replaced_ids: [1, 2, 3])]
        expect(
            Teach.savedLabel(full) == "got it. merged 3 similar rules to make room",
            "a merge says so")
        expect(
            Teach.savedLabel(result("general", "*@x.com")) == "got it. applies to all your mail",
            "general ignores a stray pattern")
        expect(
            Teach.savedLabel(result("sender", "news@stripe.com"))
                == "got it. applies to mail from news@stripe.com",
            "an address is named as is")
        expect(
            Teach.savedLabel(result("sender", "*@stripe.com"))
                == "got it. applies to all mail from stripe.com",
            "a domain drops its *@")
        expect(
            Teach.savedLabel(result("sender", nil)) == "got it. saved as a sender rule",
            "a sender rule with no pattern still says something true")
        expect(
            Teach.savedLabel(result("something_new", nil)) == "got it. applies to all your mail",
            "an unknown kind is treated as general")
    }

    static func anOldDaemonFallsBackOnlyWithASender() {
        expect(Teach.fallback(sender: nil) == .needsUpdate, "no sender: needs an update")
        expect(
            Teach.fallback(sender: "Bob <Bob@Gmail.com>") == .senderRule(pattern: "bob@gmail.com"),
            "a shared mail host narrows to the address")
        expect(Teach.fallback(sender: "  ") == .needsUpdate, "a blank sender is no sender")
        expect(
            Teach.fallback(sender: "Stripe <news@stripe.com>") == .senderRule(pattern: "*@stripe.com"),
            "a sender falls back to the old domain rule")
        expect(
            Teach.fallback(sender: "ana@example.org") == .senderRule(pattern: "*@example.org"),
            "a bare address too")
    }

    static func saveIsShutOnEmptyOrTooLong() {
        expect(!Teach.canSave(""), "empty")
        expect(!Teach.canSave(" \n "), "whitespace")
        expect(Teach.canSave("never show me grant funding mail"), "a sentence")
        expect(Teach.canSave(String(repeating: "a", count: 1000)), "exactly at the cap")
        expect(!Teach.canSave(String(repeating: "a", count: 1001)), "over the cap")
        expect(Teach.canSave("  " + String(repeating: "a", count: 1000) + "  "), "trimmed before counting")
    }

    static func shortLabelClipsTheFirstLine() {
        expect(Teach.shortLabel("short") == "short", "short stays")
        expect(Teach.shortLabel("line one\nline two") == "line one", "first line only")
        let long = Teach.shortLabel(String(repeating: "word ", count: 20))
        expect(long.count <= 40 && long.hasSuffix("…"), "long is clipped with an ellipsis")
    }

    static func theWireDecodes() {
        let sender = #"{"kind":"sender","id":3,"text":"mute them","match_pattern":"*@a.com","disposition":"squelch"}"#
        let general = #"{"kind":"general","id":9,"text":"never grant mail"}"#
        let list = #"[{"id":1,"text":"t","created_at":"2026-11-05T00:00:00Z","updated_at":"2026-11-05T00:00:00Z"}]"#
        let d = JSONDecoder()
        let s = try? d.decode(TeachResult.self, from: Data(sender.utf8))
        expect(s?.isSender == true && s?.disposition == .squelch && s?.match_pattern == "*@a.com", "sender result")
        expect(s?.replaced == nil, "replaced absent reads as nothing replaced")
        let over = #"{"kind":"sender","id":3,"text":"x","match_pattern":"a@gmail.com","disposition":"surface","replaced":{"want_text":"old","disposition":"filtered"}}"#
        let o = try? d.decode(TeachResult.self, from: Data(over.utf8))
        expect(o?.replaced == ReplacedRule(want_text: "old", disposition: .filtered), "replaced decodes")
        expect(
            o.map(Teach.savedLabel) == "got it. applies to mail from a@gmail.com",
            "a narrowed address is named as is")
        let nulled = #"{"kind":"sender","id":3,"text":"x","match_pattern":"*@a.com","replaced":null}"#
        expect((try? d.decode(TeachResult.self, from: Data(nulled.utf8)))?.replaced == nil, "null replaced")
        let g = try? d.decode(TeachResult.self, from: Data(general.utf8))
        expect(g?.isSender == false && g?.match_pattern == nil && g?.id == 9, "general result, no pattern")
        let rules = try? d.decode([GeneralRule].self, from: Data(list.utf8))
        expect(rules?.first?.text == "t", "general rule list")
        let body = try? JSONEncoder().encode(TeachBody(text: "hi"))
        expect(body.map { String(decoding: $0, as: UTF8.self) } == #"{"text":"hi"}"#, "no source id: absent")
    }

    static func copyHasNoDashes() {
        let copy = [
            Teach.example, Teach.needsUpdateMessage, Teach.undoneText,
            Teach.savedLabel(result("sender", "*@a.com")),
        ]
        for line in copy {
            expect(!line.contains("\u{2014}") && !line.contains("\u{2013}"), "no dashes: \(line)")
        }
    }

    static func expect(_ condition: Bool, _ what: String) {
        checks += 1
        if !condition {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
