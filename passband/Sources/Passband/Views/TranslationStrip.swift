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
    /// The reader has stopped waiting for this one (the poll spent its budget,
    /// or the ask for an archived message failed): the strip stops saying it
    /// is translating, and says the original is what is shown.
    var stalled = false
    let onToggle: () -> Void

    var body: some View {
        // A finished translation with nothing in it is no translation: the
        // card shows the original, and the strip has nothing true to say.
        if let translation, translation.awaiting || translation.ready {
            HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                // Decorative: the words say what the glyph does.
                Image(systemName: "translate")
                    .font(Typo.ui(10, weight: .medium))
                    .accessibilityHidden(true)
                Text(Self.label(translation, showingOriginal: showingOriginal, stalled: stalled))
                    .fixedSize(horizontal: false, vertical: true)
                    .help(Self.help(translation, stalled: stalled))
                if translation.ready {
                    Text("·").foregroundStyle(Palette.inkFaintest)
                        .accessibilityHidden(true)
                    toggle
                }
            }
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaint)
            .padding(.horizontal, Space.s2)
            // ONE ELEMENT, READ AS A SENTENCE: "Translated from French. Show
            // original", with the flip as its action, rather than the glyph's
            // own name, a dot, and a button VoiceOver has to be steered to.
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(
                Self.spoken(translation, showingOriginal: showingOriginal, stalled: stalled))
            // The tooltip's detail (which facts the check could not find) for
            // a reader who cannot hover.
            .accessibilityHint(Self.help(translation, stalled: stalled))
            .modifier(StripAction(enabled: translation.ready, name: Self.toggleTitle(showingOriginal),
                action: onToggle))
        }
    }

    /// The flip. The same micro text it always was; on the phone the place a
    /// thumb can land on it is a 44 point band around those letters, padded
    /// out and then padded back, so the strip is drawn no taller than before.
    private var toggle: some View {
        Button(action: onToggle) {
            Text(Self.toggleTitle(showingOriginal))
                #if os(iOS)
                    .padding(.vertical, Self.touchSlop)
                    .padding(.horizontal, Space.s8)
                    .contentShape(Rectangle())
                #endif
        }
        .buttonStyle(.textAction)
        .fixedSize()
        #if os(iOS)
            .padding(.vertical, -Self.touchSlop)
            .padding(.horizontal, -Space.s8)
        #endif
    }

    #if os(iOS)
        /// Half of what the micro line is short of 44 points, rounded up.
        private static let touchSlop: CGFloat = 16
    #endif

    static func toggleTitle(_ showingOriginal: Bool) -> String {
        showingOriginal ? "show translation" : "show original"
    }

    /// What the strip says. Pure, so the copy can be checked without a view.
    static func label(_ t: ClientTranslation, showingOriginal: Bool, stalled: Bool = false) -> String {
        let language = t.languageName
        if t.awaiting {
            return stalled
                ? "translation not ready yet · showing the original"
                : "translating from \(language)…"
        }
        if showingOriginal { return "the original, in \(language)" }
        switch t.status {
        case .incomplete:
            return "translated from \(language), may be incomplete"
        case .done, .pending, .deferred:
            return "translated from \(language)"
        }
    }

    /// What VoiceOver reads: the line as sentences, and the flip after it.
    static func spoken(_ t: ClientTranslation, showingOriginal: Bool, stalled: Bool = false) -> String {
        var line = label(t, showingOriginal: showingOriginal, stalled: stalled)
            .replacingOccurrences(of: " · ", with: ". ")
        if let first = line.first { line = first.uppercased() + line.dropFirst() }
        guard t.ready else { return line }
        let verb = toggleTitle(showingOriginal)
        return "\(line). \(verb.prefix(1).uppercased())\(verb.dropFirst())"
    }

    static func help(_ t: ClientTranslation, stalled: Bool = false) -> String {
        if t.awaiting {
            if stalled {
                return "Passband has not finished translating this message to English. The original is shown, and opening the thread again checks again."
            }
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

/// The flip as the strip's accessibility action, both the default one (a
/// double tap on the element) and by name, and only when there is something
/// to flip to. Two branches is two identities, which costs nothing here: the
/// strip holds no state, and readiness only changes when its words do.
private struct StripAction: ViewModifier {
    let enabled: Bool
    let name: String
    let action: () -> Void

    func body(content: Content) -> some View {
        if enabled {
            content
                .accessibilityAddTraits(.isButton)
                .accessibilityAction { action() }
                .accessibilityAction(named: Text(name), action)
        } else {
            content
        }
    }
}
