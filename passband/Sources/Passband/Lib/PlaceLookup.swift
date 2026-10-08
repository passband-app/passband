// THE LOCATION FIELD'S LOOKUP: suggestions under an event's "where" as it is
// typed, and what a picked one writes into the field.
//
// PURE, with no MapKit in it (Tests/PlaceLookupTests.swift drives it with a
// fake). The suggestions themselves come from a `PlaceProvider`; the app's is
// Lib/MapKitPlaces.swift, Apple's MapKit search. Google Calendar's own field
// is the Places Autocomplete API, a billed Google Maps Platform product with
// its own key and nothing to do with the calendar grant, and it is not used.
//
// WHAT LEAVES THE DEVICE: the text typed into the location field goes to
// Apple, to be completed. Nothing else about the event does, no location
// permission is asked for, and nothing is sent until two characters are typed.
//
// THE RULES:
//   - FREE TEXT IS ALWAYS ALLOWED. The field is the user's; a suggestion
//     replaces what they typed only when they pick it. Nothing is highlighted
//     until an arrow says so, so Enter on a typed place saves the typed place.
//   - STALE ANSWERS ARE DROPPED. Every keystroke takes a generation; an
//     answer (a list, or a picked place's full address) lands only while its
//     generation is still the latest.
//   - A PICK WRITES TWICE: the suggestion's own two lines at once, then
//     "Name, full address" when the lookup of that place comes back, unless
//     the user has typed since.

import Foundation
import Observation

/// One suggested place: a name and the line under it (usually its address).
struct PlaceSuggestion: Hashable, Sendable, Identifiable {
    /// Its place in the provider's answer, which is how the provider finds
    /// it again to resolve it.
    let id: Int
    let title: String
    let subtitle: String
}

/// A picked place, looked up: what MapKit knows it as.
struct ResolvedPlace: Hashable, Sendable {
    let name: String?
    let address: String?
}

/// Where suggestions come from. The app's is MapKit; the tests' is a fake.
@MainActor
protocol PlaceProvider: AnyObject {
    /// Places matching what has been typed so far. Empty on any failure.
    func suggestions(for query: String) async -> [PlaceSuggestion]
    /// The full name and address of a suggestion from the LAST answer.
    func resolve(_ suggestion: PlaceSuggestion) async -> ResolvedPlace?
    /// Drop whatever is in flight.
    func cancel()
}

// MARK: - the words

enum PlaceText {
    /// "Tartine Bakery, 600 Guerrero St, San Francisco, CA 94110, United
    /// States": the name, then the full address on one line. Plain text,
    /// which is what Google stores in an event's location too. An address
    /// that already leads with the name (a street address picked as itself)
    /// is not said twice.
    static func line(name: String?, address: String?) -> String? {
        let name = tidy(name)
        let address = tidy(address)
        guard let address else { return name }
        guard let name else { return address }
        if address.lowercased().hasPrefix(name.lowercased()) { return address }
        return "\(name), \(address)"
    }

    /// What a pick writes before its lookup lands (and keeps if it fails):
    /// the suggestion's own two lines.
    static func line(_ suggestion: PlaceSuggestion) -> String {
        line(name: suggestion.title, address: suggestion.subtitle) ?? ""
    }

    /// One line, trimmed; nil when nothing is left.
    private static func tidy(_ s: String?) -> String? {
        guard let s else { return nil }
        let flat = s.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .joined(separator: ", ")
        return flat.isEmpty ? nil : flat
    }
}

// MARK: - the list's state

/// The suggestion list under the field: what it shows, which row the arrows
/// are on, and which answers are still wanted.
struct PlacePicker: Hashable, Sendable {
    /// Fewer characters than this ask nothing.
    static let minQuery = 2
    /// Rows shown. More than a handful under a field in half a window is a
    /// second page.
    static let maxShown = 5

    private(set) var suggestions: [PlaceSuggestion] = []
    /// The row the arrows are on. Nil is the typed text itself.
    private(set) var highlight: Int?
    private(set) var generation = 0
    /// What this picker last wrote into the field itself: the field's change
    /// that follows is not typing, and must not ask about its own words.
    private var written: String?

    var isOpen: Bool { !suggestions.isEmpty }

    /// The field's text changed. The query to ask for and the generation its
    /// answer must carry, or nil when there is nothing to ask (too short, or
    /// the change was this picker's own write). Typing always closes a list
    /// that was about the text before it once it is too short to ask about.
    mutating func typed(_ text: String) -> (query: String, generation: Int)? {
        if let written, written == text { return nil }
        written = nil
        generation += 1
        let query = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard query.count >= Self.minQuery else {
            close()
            return nil
        }
        return (query, generation)
    }

    /// An answer arrived. Applied only when it is still the newest asked for.
    @discardableResult
    mutating func landed(_ answer: [PlaceSuggestion], generation: Int) -> Bool {
        guard generation == self.generation else { return false }
        suggestions = Array(answer.prefix(Self.maxShown))
        // The rows changed under the arrow: back to the typed text.
        highlight = nil
        return true
    }

    /// An arrow. Down from the typed text is the first row; up from the first
    /// row is the typed text again. False when there is no list to move in.
    @discardableResult
    mutating func move(_ delta: Int) -> Bool {
        guard isOpen else { return false }
        let at = (highlight ?? -1) + delta
        highlight = at < 0 ? nil : min(suggestions.count - 1, at)
        return true
    }

    /// Escape, or the field losing focus: the list goes and nothing in
    /// flight may bring it back. False when there was no list.
    @discardableResult
    mutating func dismiss() -> Bool {
        let was = isOpen
        generation += 1
        close()
        return was
    }

    /// Enter on a highlighted row, or a click on any row: the suggestion
    /// picked, and the generation its lookup must carry to be written. Nil
    /// when nothing is highlighted, so Enter stays the field's own.
    mutating func pick(_ index: Int? = nil) -> (suggestion: PlaceSuggestion, generation: Int)? {
        guard let i = index ?? highlight, suggestions.indices.contains(i) else { return nil }
        let chosen = suggestions[i]
        generation += 1
        close()
        return (chosen, generation)
    }

    /// The picker is about to write `text` into the field.
    mutating func wrote(_ text: String) {
        written = text
    }

    /// Whether a lookup that began at `generation` may still write its answer.
    func accepts(_ generation: Int) -> Bool { generation == self.generation }

    private mutating func close() {
        suggestions = []
        highlight = nil
    }
}

// MARK: - the lookup

/// The field's lookup, live: debounces the typing, asks the provider, drops
/// what went stale, and writes a pick into the field through `write`.
@MainActor
@Observable
final class PlaceLookup {
    /// Long enough that a word typed at speed is one question, short enough
    /// that the list feels like it is following.
    static let debounce = Duration.milliseconds(250)

    private(set) var picker = PlacePicker()

    @ObservationIgnored private let provider: PlaceProvider
    @ObservationIgnored private let debounce: Duration
    @ObservationIgnored private var pending: Task<Void, Never>?
    /// Where a pick is written: the draft's location.
    @ObservationIgnored var write: (String) -> Void = { _ in }

    init(provider: PlaceProvider, debounce: Duration = PlaceLookup.debounce) {
        self.provider = provider
        self.debounce = debounce
    }

    /// The field's text changed, by anyone.
    func textChanged(_ text: String) {
        let before = picker.generation
        guard let ask = picker.typed(text) else {
            // Typing that asks nothing still retires what was in flight; the
            // picker's own write retires nothing.
            if picker.generation != before { drop() }
            return
        }
        pending?.cancel()
        provider.cancel()
        pending = Task { [weak self, debounce, provider] in
            try? await Task.sleep(for: debounce)
            guard !Task.isCancelled else { return }
            let answer = await provider.suggestions(for: ask.query)
            guard !Task.isCancelled else { return }
            self?.picker.landed(answer, generation: ask.generation)
        }
    }

    @discardableResult
    func move(_ delta: Int) -> Bool { picker.move(delta) }

    /// Escape. True when it closed a list (and so is not the pane's Escape).
    @discardableResult
    func dismiss() -> Bool {
        let was = picker.dismiss()
        drop()
        return was
    }

    /// Enter on a highlighted row, or a click on row `index`. True when a
    /// place was picked.
    @discardableResult
    func pick(_ index: Int? = nil) -> Bool {
        guard let (chosen, generation) = picker.pick(index) else { return false }
        drop()
        put(PlaceText.line(chosen))
        pending = Task { [weak self, provider] in
            let place = await provider.resolve(chosen)
            guard !Task.isCancelled, let self, self.picker.accepts(generation) else { return }
            // No address back (offline, or MapKit had nothing more): the
            // suggestion's own two lines stay.
            guard let place, place.address != nil,
                let line = PlaceText.line(name: place.name ?? chosen.title, address: place.address)
            else { return }
            self.put(line)
        }
        return true
    }

    /// The pane is closing.
    func stop() {
        picker.dismiss()
        drop()
    }

    private func put(_ text: String) {
        picker.wrote(text)
        write(text)
    }

    private func drop() {
        pending?.cancel()
        pending = nil
        provider.cancel()
    }
}
