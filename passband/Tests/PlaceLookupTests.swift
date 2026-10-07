// The location field's lookup, with a fake where MapKit goes
// (Lib/PlaceLookup.swift). What is held here is what would otherwise only be
// wrong under somebody's fingers: the typed place replaced by a suggestion
// nobody picked, a list for "Tar" landing under "Tartine Bakery", Escape
// closing the pane instead of the list, a pick that asks about its own words
// and reopens the list it just closed.

import Foundation

/// A provider that answers when told to, and remembers what it was asked.
@MainActor
final class FakePlaces: PlaceProvider {
    var asked: [String] = []
    var resolved: [String] = []
    var cancels = 0
    /// What each query answers.
    var book: [String: [(String, String)]] = [:]
    /// What a resolve answers, by title. Missing is "MapKit had nothing".
    var places: [String: ResolvedPlace] = [:]
    /// How long an answer takes.
    var delay = Duration.milliseconds(1)
    var resolveDelay = Duration.milliseconds(1)

    func suggestions(for query: String) async -> [PlaceSuggestion] {
        asked.append(query)
        try? await Task.sleep(for: delay)
        return (book[query] ?? []).enumerated().map {
            PlaceSuggestion(id: $0.offset, title: $0.element.0, subtitle: $0.element.1)
        }
    }

    func resolve(_ suggestion: PlaceSuggestion) async -> ResolvedPlace? {
        resolved.append(suggestion.title)
        try? await Task.sleep(for: resolveDelay)
        return places[suggestion.title]
    }

    func cancel() { cancels += 1 }
}

@main
@MainActor
struct PlaceLookupTests {
    static var failures = 0
    static var checks = 0

    static let tartine = PlaceSuggestion(id: 0, title: "Tartine Bakery", subtitle: "600 Guerrero St, San Francisco")
    static let tartineManufactory = PlaceSuggestion(
        id: 1, title: "Tartine Manufactory", subtitle: "595 Alabama St, San Francisco")

    static func main() async {
        theLine()
        typingAsksOnlyWhenThereIsEnough()
        staleAnswersAreDropped()
        arrowsAndTheTypedText()
        escapeClosesTheListOnly()
        aPickIsTheOnlyThingThatReplaces()
        thePickersOwnWriteIsNotTyping()
        await debouncesAndAsksOnce()
        await aStaleAnswerNeverLands()
        await aPickWritesTwice()
        await typingAfterAPickKeepsWhatWasTyped()
        await aFailedResolveKeepsTheSuggestion()
        await escapeStopsWhatIsInFlight()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: the words

    static func theLine() {
        expect(
            PlaceText.line(name: "Tartine Bakery", address: "600 Guerrero St, San Francisco, CA 94110, United States")
                == "Tartine Bakery, 600 Guerrero St, San Francisco, CA 94110, United States",
            "name, then the full address")
        expect(
            PlaceText.line(name: "600 Guerrero St", address: "600 Guerrero St, San Francisco, CA 94110")
                == "600 Guerrero St, San Francisco, CA 94110",
            "an address picked as itself is not said twice")
        expect(PlaceText.line(name: "Tartine Bakery", address: nil) == "Tartine Bakery", "no address: the name")
        expect(PlaceText.line(name: nil, address: "600 Guerrero St") == "600 Guerrero St", "no name: the address")
        expect(PlaceText.line(name: "  ", address: "") == nil, "nothing at all is nothing")
        expect(
            PlaceText.line(name: "Tartine Bakery", address: "600 Guerrero St\nSan Francisco, CA 94110\nUnited States")
                == "Tartine Bakery, 600 Guerrero St, San Francisco, CA 94110, United States",
            "a multi-line address is one line of plain text")
        expect(PlaceText.line(tartine) == "Tartine Bakery, 600 Guerrero St, San Francisco",
               "a suggestion's own two lines")
        expect(PlaceText.line(PlaceSuggestion(id: 0, title: "Paris", subtitle: "")) == "Paris",
               "a suggestion with no second line")
    }

    // MARK: the picker, alone

    static func typingAsksOnlyWhenThereIsEnough() {
        var p = PlacePicker()
        expect(p.typed("T") == nil, "one character asks nothing")
        expect(p.typed("  T  ") == nil, "padding is not characters")
        let ask = p.typed(" Ta")
        expect(ask?.query == "Ta", "two characters ask, trimmed")
        expect(p.landed([tartine], generation: ask!.generation) && p.isOpen, "and its answer opens the list")
        expect(p.typed("T") == nil && !p.isOpen, "deleting back under two closes it")
        expect(p.highlight == nil, "with nothing highlighted")
    }

    static func staleAnswersAreDropped() {
        var p = PlacePicker()
        let first = p.typed("Tar")!
        let second = p.typed("Tart")!
        expect(first.generation != second.generation, "each keystroke is a new question")
        expect(!p.landed([tartineManufactory], generation: first.generation) && !p.isOpen,
               "the answer to the older question is dropped")
        expect(p.landed([tartine], generation: second.generation) && p.suggestions == [tartine],
               "the newest one lands")
        let many = (0..<9).map { PlaceSuggestion(id: $0, title: "Place \($0)", subtitle: "") }
        let third = p.typed("Pla")!
        p.landed(many, generation: third.generation)
        expect(p.suggestions.count == PlacePicker.maxShown, "a long answer is cut to a handful")
    }

    static func arrowsAndTheTypedText() {
        var p = PlacePicker()
        expect(!p.move(1), "no list: the arrow is not the picker's")
        let ask = p.typed("Tar")!
        p.landed([tartine, tartineManufactory], generation: ask.generation)
        expect(p.highlight == nil, "an answer highlights nothing: Enter is still the typed text's")
        expect(p.pick() == nil, "so Enter with no highlight picks nothing")
        expect(p.move(1) && p.highlight == 0, "down is the first row")
        expect(p.move(1) && p.highlight == 1, "down again the second")
        expect(p.move(1) && p.highlight == 1, "and it stops at the last")
        expect(p.move(-1) && p.highlight == 0, "up")
        expect(p.move(-1) && p.highlight == nil, "up from the first row is the typed text again")
        expect(p.move(-1) && p.highlight == nil, "and stays there")
        p.move(1)
        let again = p.typed("Tart")!
        p.landed([tartine], generation: again.generation)
        expect(p.highlight == nil, "new rows under the arrow put it back on the typed text")
    }

    static func escapeClosesTheListOnly() {
        var p = PlacePicker()
        expect(!p.dismiss(), "nothing open: Escape is not the picker's (it is the pane's)")
        let ask = p.typed("Tar")!
        p.landed([tartine], generation: ask.generation)
        expect(p.dismiss() && !p.isOpen, "Escape closes the list and says it did")
        expect(!p.landed([tartine], generation: ask.generation) && !p.isOpen,
               "and an answer still on its way cannot reopen it")
        expect(!p.dismiss(), "a second Escape is the pane's")
    }

    static func aPickIsTheOnlyThingThatReplaces() {
        var p = PlacePicker()
        let ask = p.typed("Tar")!
        p.landed([tartine, tartineManufactory], generation: ask.generation)
        p.move(1)
        p.move(1)
        let picked = p.pick()
        expect(picked?.suggestion == tartineManufactory, "Enter picks the highlighted row")
        expect(!p.isOpen && p.highlight == nil, "and the list closes")
        expect(p.accepts(picked!.generation), "its lookup is the newest thing asked")
        expect(!p.accepts(ask.generation), "and the list's own generation is gone")
        var q = PlacePicker()
        let a2 = q.typed("Tar")!
        q.landed([tartine, tartineManufactory], generation: a2.generation)
        expect(q.pick(1)?.suggestion == tartineManufactory, "a click picks its own row, highlighted or not")
        expect(q.pick(7) == nil, "a row that is not there picks nothing")
    }

    static func thePickersOwnWriteIsNotTyping() {
        var p = PlacePicker()
        let ask = p.typed("Tar")!
        p.landed([tartine], generation: ask.generation)
        let picked = p.pick(0)!
        let line = PlaceText.line(picked.suggestion)
        p.wrote(line)
        expect(p.typed(line) == nil && p.accepts(picked.generation),
               "the field changing to what was just written asks nothing and retires nothing")
        expect(!p.isOpen, "so the list does not reopen about its own words")
        let after = p.typed(line + " patio")
        expect(after?.query == line + " patio" && !p.accepts(picked.generation),
               "the next real keystroke is typing again, and retires the lookup")
    }

    // MARK: the lookup, live

    static func lookup(_ places: FakePlaces, into field: Box) -> PlaceLookup {
        let l = PlaceLookup(provider: places, debounce: .milliseconds(30))
        l.write = { text in
            field.text = text
            field.writes.append(text)
            // The field's own change handler, as the pane wires it.
            l.textChanged(text)
        }
        return l
    }

    final class Box {
        var text = ""
        var writes: [String] = []
    }

    static func pause(_ ms: Int) async { try? await Task.sleep(for: .milliseconds(ms)) }

    /// Wait for something that will happen, however loaded the machine is:
    /// these suites run beside builds, and a fixed pause that is long enough
    /// on a quiet desk is a red run on a busy one.
    static func until(_ what: () -> Bool) async {
        for _ in 0..<2500 where !what() { await pause(2) }
    }

    static func debouncesAndAsksOnce() async {
        let places = FakePlaces()
        places.book["Tartine"] = [("Tartine Bakery", "600 Guerrero St, San Francisco")]
        let l = lookup(places, into: Box())
        // One burst, as a word typed at speed arrives.
        for text in ["Ta", "Tar", "Tart", "Tarti", "Tartin", "Tartine"] { l.textChanged(text) }
        expect(places.asked.isEmpty, "nothing is asked while the typing is still going")
        await until { l.picker.isOpen }
        await pause(60)
        expect(places.asked == ["Tartine"], "a word typed at speed is one question, the last")
        expect(l.picker.suggestions.map(\.title) == ["Tartine Bakery"], "and its answer is the list")
    }

    static func aStaleAnswerNeverLands() async {
        let places = FakePlaces()
        places.delay = .milliseconds(300)
        places.book["Tar"] = [("Tarragon Cafe", "1 Herb St")]
        places.book["Tartine"] = [("Tartine Bakery", "600 Guerrero St, San Francisco")]
        let l = lookup(places, into: Box())
        l.textChanged("Tar")
        await until { places.asked.count == 1 }
        // "Tar" is out (debounced, asked, not yet answered) when more is typed.
        expect(places.asked == ["Tar"] && !l.picker.isOpen, "the first question went out")
        l.textChanged("Tartine")
        // The second answer lands after the first has had its 300ms.
        await until { l.picker.isOpen }
        await pause(60)
        expect(places.asked == ["Tar", "Tartine"], "then the second")
        expect(l.picker.suggestions.map(\.title) == ["Tartine Bakery"],
               "and only the second's answer is ever shown")
        expect(places.cancels >= 1, "the first was cancelled at the provider too")
    }

    static func aPickWritesTwice() async {
        let places = FakePlaces()
        places.resolveDelay = .milliseconds(40)
        places.book["Tartine"] = [
            ("Tartine Bakery", "600 Guerrero St, San Francisco"), ("Tartine Manufactory", "595 Alabama St"),
        ]
        places.places["Tartine Bakery"] = ResolvedPlace(
            name: "Tartine Bakery", address: "600 Guerrero St, San Francisco, CA 94110, United States")
        let field = Box()
        let l = lookup(places, into: field)
        field.text = "Tartine"
        l.textChanged("Tartine")
        await until { l.picker.isOpen }
        expect(field.text == "Tartine" && field.writes.isEmpty, "a list showing has replaced nothing")
        expect(!l.pick(), "Enter with nothing highlighted is not a pick")
        l.move(1)
        expect(l.pick(), "down, Enter: picked")
        expect(field.text == "Tartine Bakery, 600 Guerrero St, San Francisco",
               "at once, the suggestion's own two lines")
        expect(!l.picker.isOpen, "and the list is closed")
        await until { field.writes.count == 2 }
        await pause(60)
        expect(field.text == "Tartine Bakery, 600 Guerrero St, San Francisco, CA 94110, United States",
               "then name and full address, once the place is looked up")
        expect(places.resolved == ["Tartine Bakery"], "one lookup")
        expect(places.asked == ["Tartine"] && !l.picker.isOpen,
               "and neither write asked for suggestions about itself")
    }

    static func typingAfterAPickKeepsWhatWasTyped() async {
        let places = FakePlaces()
        places.resolveDelay = .milliseconds(80)
        places.book["Tartine"] = [("Tartine Bakery", "600 Guerrero St, San Francisco")]
        places.places["Tartine Bakery"] = ResolvedPlace(name: "Tartine Bakery", address: "600 Guerrero St, 94110")
        let field = Box()
        let l = lookup(places, into: field)
        l.textChanged("Tartine")
        await until { l.picker.isOpen }
        expect(l.pick(0), "a click picks")
        // The user types on while the lookup is out.
        field.text = "Tartine Bakery, back room"
        l.textChanged(field.text)
        await pause(250)
        expect(field.text == "Tartine Bakery, back room", "what was typed after the pick is never replaced")
    }

    static func aFailedResolveKeepsTheSuggestion() async {
        let places = FakePlaces()
        places.book["Tartine"] = [("Tartine Bakery", "600 Guerrero St, San Francisco")]
        let field = Box()
        let l = lookup(places, into: field)
        l.textChanged("Tartine")
        await until { l.picker.isOpen }
        l.pick(0)
        await until { places.resolved.count == 1 }
        await pause(60)
        expect(field.text == "Tartine Bakery, 600 Guerrero St, San Francisco",
               "no address back: the suggestion's lines stay, not the bare name")
        expect(field.writes.count == 1, "written once")
    }

    static func escapeStopsWhatIsInFlight() async {
        let places = FakePlaces()
        places.delay = .milliseconds(300)
        places.book["Tartine"] = [("Tartine Bakery", "600 Guerrero St, San Francisco")]
        let l = lookup(places, into: Box())
        l.textChanged("Tartine")
        await until { places.asked.count == 1 }
        expect(!l.dismiss(), "Escape with no list yet is the pane's to take")
        await pause(450)
        expect(!l.picker.isOpen, "and the answer that was on its way does not open one afterwards")
        l.textChanged("Tartine ")
        await until { l.picker.isOpen }
        expect(l.picker.isOpen, "typing again asks again")
        expect(l.dismiss() && !l.picker.isOpen, "and Escape with the list up closes the list")
        l.textChanged("T")
        expect(!l.picker.isOpen, "too short to ask")
        l.stop()
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
