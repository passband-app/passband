// The auto-translate wire (docs/TRANSLATION.md, "Wire") decoded into the very
// types the reader draws from, and the one rule the reader leans on: which
// version of a message is shown.
//
// The decode is the half that matters most. `ClientMessage` is also what the
// iOS notification extension decodes, so a translation that failed to parse
// would not just lose the English, it would lose the message (and the push).
// Every field is therefore tolerant, and this pins that.

import Foundation

@main
@MainActor
struct TranslationWireTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        anOldDaemonSendsNoTranslation()
        aFullTranslationDecodes()
        anUnknownStatusReadsAsDone()
        missingAndMistypedFieldsDoNotFailTheMessage()
        theTranslationIsShownByDefault()
        thePendingOneShowsTheOriginal()
        aDeferredOneDecodesAndIsNotDone()
        onlyTheReaderAsksForADeferredTranslation()
        aTextOnlyTranslationDropsTheOriginalHTML()
        anEmptyFinishedTranslationIsNoTranslation()
        theTwoVersionsAreTwoFrameKeys()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("  FAIL: \(what)")
        }
    }

    static let base =
        #""id":7,"from_addr":"a@example.fr","from_name":"Amélie","received_at":"2026-10-21T10:00:00Z","content":"Bonjour","html":"<p>Bonjour</p>","subject":"Salut""#

    static func message(_ extra: String = "") -> ClientMessage? {
        try? JSONDecoder().decode(ClientMessage.self, from: Data("{\(base)\(extra)}".utf8))
    }

    static func anOldDaemonSendsNoTranslation() {
        let m = message()
        expect(m != nil, "a message with no translation field decodes")
        expect(m?.translation == nil, "and carries none")
        expect(m?.presented().content == "Bonjour", "and is shown as it is")
    }

    static func aFullTranslationDecodes() {
        let m = message(
            #","translation":{"source_lang":"fr","source_language":"French","status":"incomplete","subject":"Hi","content":"Hello","html":"<p>Hello</p>","missing_facts":["LB-48213"]}"#
        )
        let t = m?.translation
        expect(t?.source_lang == "fr", "the code")
        expect(t?.languageName == "French", "the language's English name")
        expect(t?.status == .incomplete, "the status")
        expect(t?.html == "<p>Hello</p>", "the html")
        expect(t?.missing_facts == ["LB-48213"], "the missing facts")
        expect(t?.ready == true, "an incomplete translation is still shown")
    }

    static func anUnknownStatusReadsAsDone() {
        let m = message(
            #","translation":{"source_lang":"fr","source_language":"French","status":"reviewed","subject":"Hi","content":"Hello"}"#
        )
        expect(m?.translation?.status == .done, "a status from a newer daemon is done")
    }

    static func missingAndMistypedFieldsDoNotFailTheMessage() {
        let m = message(#","translation":{"status":5,"content":"Hello","html":7}"#)
        expect(m != nil, "the message still decodes")
        expect(m?.translation?.status == .done, "a mistyped status is done")
        expect(m?.translation?.html == nil, "a mistyped html is none")
        expect(m?.translation?.subject == "", "a missing subject is empty")
        expect(m?.translation?.languageName == "another language", "no name and no code")
        let coded = message(#","translation":{"source_lang":"fr","content":"Hello"}"#)
        expect(coded?.translation?.languageName == "FR", "the code stands in for a missing name")
        let null = message(#","translation":null"#)
        expect(null != nil && null?.translation == nil, "an explicit null is no translation")
    }

    static func theTranslationIsShownByDefault() {
        guard
            let m = message(
                #","translation":{"source_lang":"fr","source_language":"French","status":"done","subject":"Hi","content":"Hello","html":"<p>Hello</p>"}"#
            )
        else { return expect(false, "decodes") }
        let shown = m.presented()
        expect(shown.subject == "Hi", "the translated subject")
        expect(shown.html == "<p>Hello</p>", "the translated html")
        expect(shown.content == "Hello", "the translated text")
        expect(shown.id == m.id, "the same message")
        let original = m.presented(original: true)
        expect(original.html == "<p>Bonjour</p>" && original.subject == "Salut", "show original")
    }

    static func thePendingOneShowsTheOriginal() {
        let m = message(
            #","translation":{"source_lang":"fr","source_language":"French","status":"pending","subject":"","content":""}"#
        )
        expect(m?.readyTranslation == nil, "pending is not ready")
        expect(m?.presented().html == "<p>Bonjour</p>", "the original shows meanwhile")
    }

    /// ARCHIVE MAIL: the daemon serves its translation as `deferred`, with no
    /// text, until the reader displaying the thread asks. It must decode as
    /// itself: read as an unknown status it would fall back to `done`.
    static func aDeferredOneDecodesAndIsNotDone() {
        let m = message(
            #","translation":{"source_lang":"fr","source_language":"French","status":"deferred","subject":"","content":""}"#
        )
        let t = m?.translation
        expect(t?.status == .deferred, "deferred decodes as deferred, not done")
        expect(t?.awaiting == true, "and is on its way, drawn like pending")
        expect(t?.ready == false && m?.readyTranslation == nil, "deferred is not ready")
        expect(m?.presented().html == "<p>Bonjour</p>", "the original shows meanwhile")
        // Even with text on it (a daemon bug), deferred is never shown as done.
        let odd = message(
            #","translation":{"source_lang":"fr","status":"deferred","subject":"Hi","content":"Hello"}"#
        )
        expect(odd?.readyTranslation == nil, "a deferred one with text is still not ready")
        let pending = message(
            #","translation":{"source_lang":"fr","status":"pending","subject":"","content":""}"#
        )
        expect(pending?.translation?.awaiting == true, "pending is on its way too")
    }

    /// THE ASK SPENDS MONEY, so it is made by the reader that DISPLAYS a
    /// thread and by nothing that merely reads one. The prefetch warms search
    /// results, hovers and the sitrep through the same GET, and a prefetch
    /// that asked would buy translations for archive mail nobody opened.
    static func onlyTheReaderAsksForADeferredTranslation() {
        func read(_ path: String) -> String? {
            try? String(contentsOfFile: path, encoding: .utf8)
        }
        guard let prefetch = read("Sources/Passband/Model/ThreadPrefetch.swift"),
            let viewer = read("Sources/Passband/Views/ThreadViewer.swift")
        else { return expect(false, "read the sources (run from passband/)") }
        expect(!prefetch.contains("translateThread"), "the prefetch never asks")
        expect(!prefetch.contains("/translate"), "not even by path")
        expect(viewer.contains("APIClient.shared.translateThread("), "the reader does")
        expect(
            viewer.contains("await requestDeferredTranslations()"),
            "from its open task")
    }

    static func aTextOnlyTranslationDropsTheOriginalHTML() {
        let m = message(
            #","translation":{"source_lang":"fr","source_language":"French","status":"done","subject":"Hi","content":"Hello"}"#
        )
        let shown = m?.presented()
        expect(shown?.html == nil, "no html: the French document is not drawn around it")
        expect(shown?.content == "Hello", "the English text is")
    }

    static func anEmptyFinishedTranslationIsNoTranslation() {
        let m = message(
            #","translation":{"source_lang":"fr","source_language":"French","status":"done","subject":"Hi","content":""}"#
        )
        expect(m?.readyTranslation == nil, "nothing to show is not shown")
        expect(m?.presented().content == "Bonjour", "the original stands")
    }

    static func theTwoVersionsAreTwoFrameKeys() {
        for style in ThreadStyle.allCases {
            expect(
                style.frameKey(7, translated: true) != style.frameKey(7, translated: false),
                "\(style): the translation is filed apart from the original")
            expect(
                style.frameKey(7, translated: false) == style.frameKey(7),
                "\(style): the original keeps the key it always had")
        }
    }
}
