// THE ONE LINE A TRANSLATED MESSAGE OWES THE READER: this is not what the
// sender wrote, it is what they wrote in English, and here is theirs.
//
// A CAPTION, NOT A BANNER. Mail in another language is not a problem to be
// flagged, and the translation is the normal way it is read here, so the strip
// is set the way the image bar's quieter half is: micro type, faint ink, no
// fill and no edge. Every filled shape in this app is filled in a semantic
// colour, and nothing here is wrong. The one verb on it is a text action, the
// same affordance the reader's other chrome-over-content uses.
//
// See docs/TRANSLATION.md, "Reader". The strings carry no dashes of any kind.

import SwiftUI

struct TranslationStrip: View {
    /// nil, or a failed translation the daemon left off the wire: the strip
    /// draws nothing, and the card is the original exactly as before.
    let translation: ClientTranslation?
    let showingOriginal: Bool
    let onToggle: () -> Void

    var body: some View {
        // A finished translation with nothing in it is no translation: the
        // card shows the original, and the strip has nothing true to say.
        if let translation, translation.status == .pending || translation.ready {
            HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                Image(systemName: "translate")
                    .font(Typo.ui(10, weight: .medium))
                Text(Self.label(translation, showingOriginal: showingOriginal))
                    .fixedSize(horizontal: false, vertical: true)
                    .help(Self.help(translation))
                if translation.ready {
                    Text("·").foregroundStyle(Palette.inkFaintest)
                    Button(showingOriginal ? "show translation" : "show original", action: onToggle)
                        .buttonStyle(.textAction)
                        .fixedSize()
                }
            }
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaint)
            .padding(.horizontal, Space.s2)
            .accessibilityElement(children: .combine)
        }
    }

    /// What the strip says. Pure, so the copy can be checked without a view.
    static func label(_ t: ClientTranslation, showingOriginal: Bool) -> String {
        let language = t.languageName
        if t.status == .pending { return "translating from \(language)…" }
        if showingOriginal { return "the original, in \(language)" }
        switch t.status {
        case .incomplete:
            return "translated from \(language), but it may have missed something. Check the original"
        case .done, .pending:
            return "translated from \(language)"
        }
    }

    static func help(_ t: ClientTranslation) -> String {
        if t.status == .pending {
            return "Passband is translating this message to English. The original is shown until it is ready."
        }
        if t.status == .incomplete {
            let facts = (t.missing_facts ?? []).filter { !$0.isEmpty }
            let tail = facts.isEmpty ? "" : " Not found in the translation: \(facts.joined(separator: ", "))."
            return "Passband could not confirm that every detail carried over into English.\(tail)"
        }
        return "Passband translated this message to English. The original is one click away."
    }
}
