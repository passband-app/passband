// What the assistant says when it is asked about Passband itself. The three
// help tools answer from tables compiled into the app, and a table is only as
// good as the questions it can be held to, so the suite is written as those
// questions: "how do I reply all?", "where is dark mode?", "what are the keys?".
//
// Three failures matter:
//
//   * A topic the model can name and the guide cannot answer. The tool's enum
//     is built from `Topic.allCases`, so every case has to produce prose.
//   * A dash the house style forbids. The guide is text a model reads and then
//     paraphrases to a person, and a model that reads em dashes writes them.
//   * The chat and the `?` overlay disagreeing about a key. They share one
//     table now; the checks here are what notice if the guide's own prose
//     wanders off from it.

import Foundation

@main
@MainActor
struct ProductGuideTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        everyTopicAnswers()
        theQuestionThisWasBuiltFor()
        houseStyle()
        thePromptIsAPointer()
        shortcutsAreTheOverlaysTable()
        theGuideAgreesWithTheKeymapTable()
        theCalendarIsListedOnlyWhereItIsOffered()
        settingsAreFoundByDescription()
        settingsWithNoQueryOrNoMatchListEverything()
        whatsNewReadsTheReleaseTable()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - the guide

    static func everyTopicAnswers() {
        for topic in ProductGuide.Topic.allCases {
            let text = ProductGuide.text(topic, runningVersion: "0.0.12")
            expect(text.count > 200, "\(topic.rawValue) has something to say")
            expect(!topic.title.isEmpty, "\(topic.rawValue) has a title for its chip")
            expect(
                ProductGuide.topicIndex.contains(topic.rawValue + ":"),
                "\(topic.rawValue) is in the index the model picks from")
            // A wire name the model has to spell: lowercase and underscores.
            expect(
                topic.rawValue.allSatisfy { $0.isLowercase || $0 == "_" },
                "\(topic.rawValue) is spelled like a tool argument")
        }
    }

    static func theQuestionThisWasBuiltFor() {
        let writing = ProductGuide.text(.writingMail, runningVersion: nil).lowercased()
        expect(writing.contains("reply all"), "writing_mail covers reply all")
        expect(writing.contains("enter"), "and names the key that does it")
        expect(writing.contains("forward"), "and forwarding")
        expect(
            ProductGuide.topicIndex.lowercased().contains("reply all"),
            "the index says where reply all lives, so the model picks the topic in one call")
    }

    /// Every word a help tool can return, and the two lines the prompt carries.
    static func houseStyle() {
        var texts = ProductGuide.Topic.allCases.map {
            ($0.rawValue, ProductGuide.text($0, runningVersion: "0.0.12"))
        }
        texts.append(("shortcuts", ProductGuide.shortcuts()))
        texts.append(("settings", ProductGuide.settings(matching: nil)))
        texts.append(("topic index", ProductGuide.topicIndex))
        texts.append(("prompt intro", ProductGuide.promptIntro))
        texts.append(("prompt tools", ProductGuide.promptTools))
        for (name, text) in texts {
            expect(!text.contains("\u{2014}"), "\(name) has no em dash")
            expect(!text.contains("\u{2013}"), "\(name) has no en dash")
        }
    }

    /// The system prompt names the tools and stops. If the guide's content
    /// ever leaks into it, every question pays for help nobody asked for,
    /// which is the thing the tools exist to avoid.
    static func thePromptIsAPointer() {
        let pointer = ProductGuide.promptIntro + ProductGuide.promptTools
        expect(pointer.count < 600, "the prompt's share of the guide stays a pointer")
        for name in ["product_help", "list_shortcuts", "find_setting"] {
            expect(ProductGuide.promptTools.contains(name), "the prompt names \(name)")
        }
    }

    // MARK: - shortcuts

    static func shortcutsAreTheOverlaysTable() {
        let text = ProductGuide.shortcuts()
        for group in ShortcutCatalog.groups {
            expect(text.contains(group.title), "the \(group.title) group is recited")
            for item in group.items {
                expect(text.contains(item.desc), "\"\(item.desc)\" is recited")
            }
        }
        expect(text.contains("Enter: reply all"), "reply all reads as a key and what it does")
    }

    /// The guide's prose restates a handful of keys the `?` table also lists.
    /// Pinned in pairs, so changing a key in one place fails here until the
    /// other follows.
    static func theGuideAgreesWithTheKeymapTable() {
        let reader = ShortcutCatalog.groups.first { $0.title == "Thread viewer" }
        expect(reader != nil, "the reader has a group in the table")
        func key(_ desc: String) -> [String] {
            reader?.items.first { $0.desc == desc }?.keys ?? []
        }
        expect(key("reply") == ["r"], "the table says r replies")
        expect(key("reply all") == ["Enter"], "the table says Enter replies to all")
        expect(key("forward this message") == ["f"], "the table says f forwards")
        let writing = ProductGuide.text(.writingMail, runningVersion: nil)
        expect(writing.contains("press r"), "the guide says r replies")
        expect(writing.contains("press Enter"), "the guide says Enter replies to all")
        expect(writing.contains("press f"), "the guide says f forwards")
    }

    /// The calendar's keys follow the page: absent from the overlay while it
    /// is not offered, and with Calendar at 2 when it is. The assistant, with
    /// no account in hand, recites them with their condition, and the guide's
    /// prose agrees with the table about the event pane's keys.
    static func theCalendarIsListedOnlyWhereItIsOffered() {
        let off = ShortcutCatalog.groups(calendar: false)
        expect(off.map(\.title) == ShortcutCatalog.groups.map(\.title), "no calendar, the table as it was")
        expect(!off.contains { $0.title == "Calendar" }, "no Calendar group while it is not offered")

        let on = ShortcutCatalog.groups(calendar: true)
        expect(on.contains { $0.title == "Calendar" }, "the Calendar group while it is offered")
        let navigate = on.first { $0.title == "Navigate" }
        expect(navigate?.items.first?.keys.count == 6, "six digits with the calendar")
        expect(navigate?.items.first?.desc.hasPrefix("sitrep · calendar") == true, "Calendar is 2")
        expect(navigate?.items.contains { $0.keys == ["⇧C"] } == true, "⇧C opens it")

        let text = ProductGuide.shortcuts()
        for item in ShortcutCatalog.calendarGroup.items {
            expect(text.contains(item.desc), "the calendar's \"\(item.desc)\" is recited")
        }
        func key(_ desc: String) -> [String] {
            ShortcutCatalog.calendarGroup.items.first { $0.desc == desc }?.keys ?? []
        }
        let guide = ProductGuide.text(.calendar, runningVersion: nil)
        expect(key("new event") == ["n"] && guide.contains("n, or \"New event\""), "n makes an event")
        expect(key("edit event") == ["e"] && guide.contains("e edits"), "e edits one")
        expect(key("save the event") == ["⌘↵"] && guide.contains("⌘↵ saves"), "⌘↵ saves")
        expect(key("delete event") == ["⌫"] && guide.contains("⌫ deletes"), "⌫ deletes")
        expect(guide.contains("plain Enter does nothing"), "plain Enter never tells guests")
    }

    // MARK: - settings

    static func settingsAreFoundByDescription() {
        let dark = ProductGuide.settings(matching: "dark mode")
        expect(dark.contains("Theme"), "\"dark mode\" finds the theme setting")
        expect(dark.contains("General > Appearance"), "and says which pane it is on")
        expect(!dark.contains("Signature"), "and does not recite the rest of the index")

        // A model asks once, in a sentence, with a verb the index has never
        // heard of. The noun still has to land, as a match and not by way of
        // the whole list.
        let sig = ProductGuide.settings(matching: "how do I change my signature")
        expect(sig.contains("Mail > Signature"), "a whole question still lands")
        expect(!sig.contains("Nothing matched"), "as a match, not as a miss")
        let receipts = ProductGuide.settings(matching: "disable read receipts")
        expect(receipts.contains("Read tracking"), "\"disable read receipts\" finds read tracking")
        expect(!receipts.contains("Nothing matched"), "also as a match")
        // A card named after its own section is said once, not "Mail > Mail".
        let images = ProductGuide.settings(matching: "pictures")
        expect(images.contains("Remote images"), "\"pictures\" finds remote images")
        expect(!images.contains("Mail > Mail"), "a card named like its section is not doubled")
    }

    static func settingsWithNoQueryOrNoMatchListEverything() {
        let all = ProductGuide.settings(matching: nil)
        for entry in SettingsSearch.entries where entry.card.isAvailable {
            expect(all.contains(entry.title), "the full list names \(entry.title)")
        }
        let miss = ProductGuide.settings(matching: "zzzz quux")
        expect(miss.contains("Nothing matched"), "a miss says it missed")
        expect(miss.contains("Theme"), "and hands over the whole list to pick from")
    }

    // MARK: - what's new

    static func whatsNewReadsTheReleaseTable() {
        guard let newest = ReleaseNotes.all.first else {
            expect(false, "the release table has a newest release")
            return
        }
        let text = ProductGuide.text(.whatsNew, runningVersion: newest.version)
        expect(text.contains(newest.version), "it names the running release")
        if let first = newest.items(on: .app).first {
            expect(text.contains(first.text), "and recites that release's Mac items")
        }
        // A build the table has never heard of answers with the last release
        // at or below it, and one that reports no version says so.
        let none = ProductGuide.text(.whatsNew, runningVersion: nil)
        expect(none.count > 40, "no version still answers")

        // THE IPHONE READS ITS OWN TABLE. Its versions are a different line:
        // asked as the phone, the Mac's notes for the same number must not
        // come back.
        guard let phone = PhoneReleaseNotes.all.first, let feature = phone.features.first else {
            expect(false, "the phone's table has a release with a feature")
            return
        }
        let onPhone = ProductGuide.text(.whatsNew, runningVersion: phone.version, onPhone: true)
        expect(onPhone.contains("This iPhone is running"), "the phone's answer says it is the phone's")
        expect(onPhone.contains(feature.title) && onPhone.contains(feature.detail), "and recites the phone's features")
        expect(onPhone.contains("person icon"), "and says where the phone keeps it")
        expect(!onPhone.contains("In the Mac app"), "and none of the Mac's notes")
        expect(!onPhone.contains("\u{2014}"), "with no em dashes")
        let before = ProductGuide.text(.whatsNew, runningVersion: "0.0.0", onPhone: true)
        expect(!before.contains(feature.title), "a phone build older than every note recites none")
        expect(
            ProductGuide.text(.appAndAccounts, runningVersion: nil).contains("On the iPhone, \"What's new\""),
            "the accounts page says where What's new is on the iPhone")
    }

    // MARK: - helpers

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
