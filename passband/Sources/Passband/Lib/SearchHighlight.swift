// THE MATCHED WORDS IN A SEARCH HIT, painted. One function for both shells:
// the Mac's side panel and the phone's search tab draw a hit's subject and
// excerpt through this, so a word that is lit on one is lit on the other.
//
// The daemon says which words matched (`subject_matches` / `snippet_matches`,
// stems and prefixes resolved to what is actually in the text). A daemon too
// old to say gets the query's own terms from the diagnostics instead.
//
// PAINT ONLY: the text is the hit's own, as plain Text. Nothing here is parsed
// as markup, because every string it touches came from somebody's email.

import SwiftUI

enum SearchHighlight {
    static func paint(_ text: String, matches: [String]) -> AttributedString {
        var attr = AttributedString(text)
        for term in matches where !term.isEmpty {
            var from = attr.startIndex
            while from < attr.endIndex,
                let range = attr[from...].range(of: term, options: .caseInsensitive)
            {
                attr[range].backgroundColor = Palette.searchMatch
                attr[range].foregroundColor = Palette.searchMatchInk
                from = range.upperBound
            }
        }
        return attr
    }
}
