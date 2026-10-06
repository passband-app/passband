// THE LOCATION FIELD'S SUGGESTIONS, FROM APPLE: `MKLocalSearchCompleter` for
// names and addresses as they are typed, and `MKLocalSearch` for the full
// address of the one picked. The rules about what is asked and what is
// written are Lib/PlaceLookup.swift's; this file only talks to MapKit.
//
// No key and no bill, on the Mac and the phone alike. NO LOCATION PERMISSION
// is requested and no region is set: MapKit biases results as it does by
// default and nothing here tells it where the user is. The text typed into
// the field is what goes to Apple (docs/SECURITY.md, the calendar section).

import Foundation
import MapKit

@MainActor
final class MapKitPlaces: NSObject, PlaceProvider {
    private let completer = MKLocalSearchCompleter()
    /// The last answer, kept so a pick can be resolved from the very
    /// completion MapKit handed back.
    private var results: [MKLocalSearchCompletion] = []
    private var waiting: CheckedContinuation<[PlaceSuggestion], Never>?
    private var search: MKLocalSearch?
    private var resolving: (seq: Int, continuation: CheckedContinuation<ResolvedPlace?, Never>)?
    private var resolveSeq = 0

    override init() {
        super.init()
        completer.delegate = self
        completer.resultTypes = [.address, .pointOfInterest]
    }

    func suggestions(for query: String) async -> [PlaceSuggestion] {
        finish([])
        // The completer says nothing about a fragment it was already given.
        if completer.queryFragment == query, !results.isEmpty { return rows() }
        return await withCheckedContinuation { continuation in
            waiting = continuation
            completer.queryFragment = query
        }
    }

    func resolve(_ suggestion: PlaceSuggestion) async -> ResolvedPlace? {
        guard results.indices.contains(suggestion.id), results[suggestion.id].title == suggestion.title
        else { return nil }
        let request = MKLocalSearch.Request(completion: results[suggestion.id])
        request.resultTypes = [.address, .pointOfInterest]
        cancel()
        let search = MKLocalSearch(request: request)
        self.search = search
        resolveSeq += 1
        let seq = resolveSeq
        return await withCheckedContinuation { (continuation: CheckedContinuation<ResolvedPlace?, Never>) in
            resolving = (seq, continuation)
            search.start { response, _ in
                let place = response?.mapItems.first.map {
                    ResolvedPlace(
                        name: $0.name,
                        address: $0.addressRepresentations?.fullAddress(
                            includingRegion: true, singleLine: true) ?? $0.address?.fullAddress)
                }
                Task { @MainActor [weak self] in self?.finishResolve(seq, place) }
            }
        }
    }

    /// Exactly one answer per lookup, whether MapKit answers or it is
    /// cancelled first (a cancelled search is not promised a callback).
    private func finishResolve(_ seq: Int, _ place: ResolvedPlace?) {
        guard let resolving, resolving.seq == seq else { return }
        self.resolving = nil
        resolving.continuation.resume(returning: place)
    }

    func cancel() {
        finish([])
        if completer.isSearching { completer.cancel() }
        search?.cancel()
        search = nil
        if let resolving {
            self.resolving = nil
            resolving.continuation.resume(returning: nil)
        }
    }

    private func rows() -> [PlaceSuggestion] {
        results.enumerated().map { PlaceSuggestion(id: $0.offset, title: $0.element.title, subtitle: $0.element.subtitle) }
    }

    /// Exactly one answer per question: whoever is waiting is told, once.
    private func finish(_ answer: [PlaceSuggestion]) {
        waiting?.resume(returning: answer)
        waiting = nil
    }
}

extension MapKitPlaces: @preconcurrency MKLocalSearchCompleterDelegate {
    func completerDidUpdateResults(_ completer: MKLocalSearchCompleter) {
        results = completer.results
        finish(rows())
    }

    func completer(_ completer: MKLocalSearchCompleter, didFailWithError error: Error) {
        results = []
        finish([])
    }
}
