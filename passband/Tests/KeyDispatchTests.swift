// THE KEYMAP'S ORDERING RULE, which is the whole reason `E` can be a different
// verb from `e` rather than the same one shouted.
//
// Dispatch runs TWO passes: an exact, case-sensitive one across every context,
// and only then a case-folded one. That is what lets the reader bind `e` (done)
// and `E` (done + next) side by side, while `T` still falls back to `t` on a
// surface that never bound the shifted spelling.
//
// It is also the trap, and the trap is asserted here rather than remembered: a
// DECLINING guard spelled only "e" does not hold `E`. The exact pass finds the
// reader's own "E" first, in a different set entirely, and fires it — the guard
// is never asked. That guard is the inline reply's review hold, the thing
// standing between a shifted keystroke and a draft that is one Enter from
// going out, and its cost is an email nobody can get back.

import SwiftUI

@main
@MainActor
struct KeyDispatchTests {
    static var failures = 0
    static var checks = 0

    /// Every handler that RAN, in order — declining ones included, which is how
    /// a guard proves it was asked at all.
    static var ran: [String] = []

    static func main() {
        exactCaseBeatsTheFold()
        anUnboundShiftIsStillTheLetter()
        aGuardSpelledOnlyLowercaseLosesTheShiftedKey()
        aGuardSpelledBothWaysHoldsBoth()
        decliningPassesTheKeyOn()
        typingIsNeverAVerb()
        shiftIsSpelledOutForNamedKeysOnly()
        aChordAndItsBareTwinCoexist()
        aChordNeverFallsBackToItsBareTwin()
        anExactCaseBindingSitsTheFoldOut()
        aHoldSwallowsItsOwnRepeatsAndHearsTheRelease()
        aHoldIsReleasedWhateverShiftDid()
        aCancelledHoldSaysSo()
        aHoldOutlivesTheSurfaceThatStartedIt()
        theCalendarKeyTakesShiftCOnlyWhenOffered()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - the two passes

    /// The reader's pair. Both spellings are bound in the SAME set, and each one
    /// gets its own verb: this is the case the exact pass exists for.
    static func exactCaseBeatsTheFold() {
        stage([[verb("e", "done"), verb("E", "done+next")]]) {
            equal(press("E"), ["done+next"], "shift+e is its own verb")
            equal(press("e"), ["done"], "and plain e is still the plain one")
        }
    }

    /// The other half of the same rule: a surface that never bound the shifted
    /// spelling still answers it, so shift never silently swallows a key.
    static func anUnboundShiftIsStillTheLetter() {
        stage([[verb("t", "tune")]]) {
            equal(press("T"), ["tune"], "shift falls back to the letter")
        }
    }

    // MARK: - the trap

    /// THE REGRESSION. A guard registered ABOVE the reader — later, so it is
    /// asked first — still loses `E` when it is spelled only "e": the exact pass
    /// sweeps every set before the fold gets a turn, and the reader's own "E" is
    /// an exact hit.
    static func aGuardSpelledOnlyLowercaseLosesTheShiftedKey() {
        stage([
            [verb("e", "done"), verb("E", "done+next")],
            [hold("e", "guard-e")],
        ]) {
            equal(press("e"), ["guard-e"], "the guard holds the key it spells")
            equal(press("E"), ["done+next"], "and never sees the one it does not")
        }
    }

    /// Spelled both ways, it holds both — which is how the review guard is
    /// written and must stay written.
    static func aGuardSpelledBothWaysHoldsBoth() {
        stage([
            [verb("e", "done"), verb("E", "done+next")],
            [hold("e", "guard-e"), hold("E", "guard-E")],
        ]) {
            equal(press("e"), ["guard-e"], "plain held")
            equal(press("E"), ["guard-E"], "shifted held too, and the verb never ran")
        }
    }

    /// A guard that declines is not a guard: the key keeps falling to the verb
    /// underneath. This is what lets the same binding hold only during review.
    static func decliningPassesTheKeyOn() {
        stage([
            [verb("E", "done+next")],
            [hold("E", "guard-E", holds: false)],
        ]) {
            equal(press("E"), ["guard-E", "done+next"], "asked, declined, passed on")
        }
    }

    /// With a text field focused a single letter is a character, both cases. The
    /// shifted twin gets no exemption — `E` in a draft body is a capital E.
    static func typingIsNeverAVerb() {
        stage([[verb("e", "done"), verb("E", "done+next")]]) {
            equal(press("e", editing: true), [], "e types")
            equal(press("E", editing: true), [], "E types")
        }
    }

    // MARK: - normalization

    /// Shift is spelled into the key string only for NAMED keys; for letters the
    /// case IS the signal, which is the premise the whole pair rests on.
    static func shiftIsSpelledOutForNamedKeysOnly() {
        equal(
            KeyRegistry.normalize(KeyEventLike(key: "E", shift: true)), "E",
            "a shifted letter is just the capital")
        equal(
            KeyRegistry.normalize(KeyEventLike(key: "ArrowDown", shift: true)), "shift+ArrowDown",
            "a shifted named key says so")
    }

    // MARK: - the meta rule

    /// The rules card binds Enter TWICE in one set, bare and ⌘-chorded, and both
    /// save. That is only legal because `meta` is matched in BOTH directions.
    static func aChordAndItsBareTwinCoexist() {
        stage([[verb("Enter", "save", allowInInput: true), chord("Enter", "save-chord")]]) {
            equal(press("Enter"), ["save"], "bare Enter reaches the bare binding")
            equal(press("Enter", meta: true), ["save-chord"], "and ⌘Enter reaches the chord")
        }
    }

    /// The other half, and the one that sent someone looking: a chord does NOT
    /// fall back to its bare twin the way a shifted letter falls back to its
    /// lowercase one. Bind only the bare spelling and ⌘Enter is simply unheard,
    /// which is exactly what the rules card did before both were bound.
    static func aChordNeverFallsBackToItsBareTwin() {
        stage([[verb("Enter", "save", allowInInput: true)]]) {
            equal(press("Enter", meta: true), [], "no chord binding, no chord")
        }
        stage([[chord("Enter", "save")]]) {
            equal(press("Enter"), [], "and a bare press never reaches a chord binding")
        }
    }

    /// The Sent page's `s` / `x` and the clock's ⌘⇧L: a binding marked
    /// `exactCase` answers its own spelling and nothing the fold would hand it.
    static func anExactCaseBindingSitsTheFoldOut() {
        let exact = { (key: String, tag: String, meta: Bool) in
            KeyBinding(declining: key, tag, meta: meta, allowInInput: meta, exactCase: true) {
                ran.append(tag)
                return true
            }
        }
        stage([[exact("s", "send-now", false)]]) {
            equal(press("s"), ["send-now"], "s is the verb")
            equal(press("S"), [], "shift-S is not")
        }
        stage([[exact("L", "send-later", true)]]) {
            equal(press("L", meta: true), ["send-later"], "⌘⇧L opens the palette")
            equal(press("l", meta: true), [], "a bare ⌘L does not")
        }
    }

    // MARK: - harness

    /// A plain verb that records itself.
    static func verb(_ key: String, _ tag: String, allowInInput: Bool = false) -> KeyBinding {
        KeyBinding(key, tag, allowInInput: allowInInput) { ran.append(tag) }
    }

    /// The ⌘-chorded twin. Chords are allowed in input by convention — that is
    /// most of why they exist — so the helper bakes it in.
    static func chord(_ key: String, _ tag: String) -> KeyBinding {
        KeyBinding(key, tag, meta: true, allowInInput: true) { ran.append(tag) }
    }

    // MARK: - the calendar's key

    /// ⇧C, as RootView binds it: EXACT CASE, DECLINING, in the GLOBAL set. The
    /// exact pass reaches global before the fold reaches the surface, so while
    /// the page is offered ⇧C opens it over a surface's `c` (compose, which
    /// ⇧C used to reach through the fold). With the gate closed it is asked,
    /// declines, and the fold hands ⇧C to `c` exactly as before.
    static func theCalendarKeyTakesShiftCOnlyWhenOffered() {
        let registry = KeyRegistry.shared
        var offered = true
        let global = registry.register(
            .global,
            box: KeyRegistry.BindingsBox([
                KeyBinding(declining: "C", "calendar", exactCase: true) {
                    ran.append("calendar")
                    return offered
                }
            ]))
        stage([[verb("c", "compose")]]) {
            equal(press("C"), ["calendar"], "shift+c opens the calendar over a surface's c")
            equal(press("c"), ["compose"], "plain c still composes")
            offered = false
            equal(press("C"), ["calendar", "compose"],
                  "gate closed: asked, declines, and shift+c folds to c as it did before")
            equal(press("C", editing: true), [], "a capital C typed into a field is just typed")
        }
        registry.unregister(global)
    }

    /// A declining binding: records that it was ASKED, then holds or passes.
    static func hold(_ key: String, _ tag: String, holds: Bool = true) -> KeyBinding {
        KeyBinding(declining: key, tag) {
            ran.append(tag)
            return holds
        }
    }

    // MARK: - holds

    /// The Reading stack's `e`: pressed, held through twenty repeats, let go.
    /// The binding fires ONCE, the repeats go nowhere (not even to the `e`
    /// underneath it), and the release arrives exactly once.
    static func aHoldSwallowsItsOwnRepeatsAndHearsTheRelease() {
        var releases: [Bool] = []
        stage([
            [KeyBinding("e", "under") { ran.append("under") }],
            [KeyBinding("e", "hold", onRelease: { releases.append($0) }) { ran.append("hold") }],
        ]) {
            equal(press("e"), ["hold"], "the key down starts the hold")
            ran = []
            _ = KeyRegistry.shared.dispatch(KeyEventLike(key: "e", isRepeat: true), editing: false)
            equal(ran, [], "an autorepeat of the held key fires nothing")
            equal(KeyRegistry.shared.release(KeyEventLike(key: "e")), true, "the key-up is taken")
            equal(releases, [false], "released once, not cancelled")
            equal(KeyRegistry.shared.release(KeyEventLike(key: "e")), false, "a second key-up is nobody's")
            ran = []
            _ = KeyRegistry.shared.dispatch(KeyEventLike(key: "e", isRepeat: true), editing: false)
            equal(ran, ["hold"], "with no hold, a repeat is an ordinary press again")
            KeyRegistry.shared.cancelHold()
        }
    }

    /// Down as `e`, up as `E` because shift went down in between: still the
    /// same physical key, still the same hold.
    static func aHoldIsReleasedWhateverShiftDid() {
        var releases: [Bool] = []
        stage([[KeyBinding("e", "hold", onRelease: { releases.append($0) }) {}]]) {
            _ = press("e")
            equal(KeyRegistry.shared.release(KeyEventLike(key: "E", shift: true)), true, "folded")
            equal(releases, [false], "released")
        }
    }

    /// The app lost the keyboard mid-hold: the binding is told, as a cancel.
    static func aCancelledHoldSaysSo() {
        var releases: [Bool] = []
        stage([[KeyBinding("e", "hold", onRelease: { releases.append($0) }) {}]]) {
            _ = press("e")
            KeyRegistry.shared.cancelHold()
            equal(releases, [true], "cancelled")
            equal(KeyRegistry.shared.release(KeyEventLike(key: "e")), false, "and it is over")
        }
    }

    /// The reader's plain `e`: it closes the reader on key-down, so its set is
    /// gone before the first autorepeat. The repeats must STILL go nowhere, not
    /// to the `e` on the list underneath, until the key comes up.
    static func aHoldOutlivesTheSurfaceThatStartedIt() {
        let registry = KeyRegistry.shared
        let under = registry.register(.thread, box: KeyRegistry.BindingsBox([
            KeyBinding("e", "list done") { ran.append("list") }
        ]))
        stage([[KeyBinding("e", "reader done", onRelease: { _ in }) { ran.append("reader") }]]) {
            equal(press("e"), ["reader"], "the reader takes the press")
        }
        let ctx = registry.pushContext(.thread)
        ran = []
        _ = registry.dispatch(KeyEventLike(key: "e", isRepeat: true), editing: false)
        equal(ran, [], "a repeat after the reader closed reaches nothing")
        equal(registry.release(KeyEventLike(key: "e")), true, "the key-up ends it")
        equal(press("e"), ["list"], "the next real press is the list's")
        registry.popContext(ctx)
        registry.unregister(under)
    }

    /// Register `sets` into the thread context IN ORDER — later sets are asked
    /// first, the way an overlay mounted above the reader is — run the body, and
    /// leave the shared registry exactly as it was found.
    static func stage(_ sets: [[KeyBinding]], _ body: () -> Void) {
        let registry = KeyRegistry.shared
        let ctx = registry.pushContext(.thread)
        let tokens = sets.map { registry.register(.thread, box: KeyRegistry.BindingsBox($0)) }
        body()
        for token in tokens { registry.unregister(token) }
        registry.popContext(ctx)
    }

    /// Press a key and hand back every handler that ran. Shift is inferred from
    /// the case, exactly as the AppKit bridge reports it.
    static func press(_ key: String, meta: Bool = false, editing: Bool = false) -> [String] {
        ran = []
        let shifted = key.count == 1 && key.lowercased() != key
        _ = KeyRegistry.shared.dispatch(
            KeyEventLike(key: key, command: meta, shift: shifted), editing: editing)
        return ran
    }

    static func equal<T: Equatable>(_ got: T, _ want: T, _ label: String, line: Int = #line) {
        checks += 1
        if got != want {
            failures += 1
            print("FAIL (line \(line)): \(label)\n  want: \(want)\n   got: \(got)")
        }
    }
}
