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
        withTheCalendarOffTheGuideIsMains()
        aPagesDigitIsTheSheets()
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
            let text = ProductGuide.text(topic, runningVersion: "0.0.12", calendar: true)
            expect(text.count > 200, "\(topic.rawValue) has something to say")
            expect(!topic.title.isEmpty, "\(topic.rawValue) has a title for its chip")
            expect(
                ProductGuide.topicIndex(calendar: true).contains(topic.rawValue + ":"),
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
            ProductGuide.topicIndex(calendar: false).lowercased().contains("reply all"),
            "the index says where reply all lives, so the model picks the topic in one call")
    }

    /// Every word a help tool can return, and the two lines the prompt carries.
    static func houseStyle() {
        var texts: [(String, String)] = []
        for calendar in [false, true] {
            texts += ProductGuide.topics(calendar: calendar).map {
                ($0.rawValue, ProductGuide.text($0, runningVersion: "0.0.12", calendar: calendar))
            }
            texts.append(("shortcuts", ProductGuide.shortcuts(calendar: calendar)))
            texts.append(("topic index", ProductGuide.topicIndex(calendar: calendar)))
        }
        texts.append(("settings", ProductGuide.settings(matching: nil)))
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

        let text = ProductGuide.shortcuts(calendar: true)
        for item in ShortcutCatalog.calendarGroup.items {
            expect(text.contains(item.desc), "the calendar's \"\(item.desc)\" is recited")
        }
        expect(text.contains("- ⇧C: calendar"), "and ⇧C, under Navigate")
        let quiet = ProductGuide.shortcuts(calendar: false)
        expect(!quiet.contains("\nCalendar\n"), "no Calendar group recited while the page is off")
        expect(!quiet.contains("⇧C"), "nor ⇧C")
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

    /// THE CALENDAR PAGE OFF IS MAIN, BYTE FOR BYTE (fix round 1, C1). The
    /// guide, the topic list and the product_help schema built from them go
    /// to every tenant's assistant, and before the flag is on for a mailbox
    /// none of it may mention a page that mailbox does not have. The list is
    /// main's own, spelled out; the prose is held to having no calendar page
    /// in it, and was diffed against main's output when this was written.
    static func withTheCalendarOffTheGuideIsMains() {
        let mains = [
            "getting_around", "sitrep", "reading_mail", "writing_mail", "mail_list", "triage",
            "sender_rules", "reminders", "search", "login_codes", "assistant", "app_and_accounts",
            "whats_new",
        ]
        expect(ProductGuide.topics(calendar: false).map(\.rawValue) == mains, "off: main's topics, in main's order")
        expect(
            ProductGuide.topics(calendar: true).map(\.rawValue)
                == ["getting_around", "sitrep", "calendar"] + mains.dropFirst(2),
            "on: the calendar topic third")
        let index = ProductGuide.topicIndex(calendar: false)
        expect(!index.contains("calendar:"), "off: the schema offers no calendar topic")
        expect(!index.contains("Calendar page"), "nor describes the page")
        expect(ProductGuide.topicIndex(calendar: true).contains("calendar: the Calendar page"), "on: it does")
        for topic in ProductGuide.topics(calendar: false) {
            let text = ProductGuide.text(topic, runningVersion: "0.0.12", calendar: false)
            expect(!text.contains("Calendar page"), "off: \(topic.rawValue) says nothing of the page")
            expect(!text.contains("⇧C"), "off: \(topic.rawValue) teaches no ⇧C")
        }
        let off = ProductGuide.text(.gettingAround, runningVersion: nil, calendar: false)
        expect(off.contains("- 1 Sitrep:"), "off: 1 is the sitrep")
        expect(off.contains("- 2 Emails:") && off.contains("- 5 Groups:"), "off: Emails 2 to Groups 5")
        expect(!off.contains("- 6 "), "off: no sixth page")
        expect(off.contains("modifier):\n- 1 Sitrep"), "the list sits right under its heading")
        expect(off.contains("the key opens Settings > Groups.\n- Usage and Settings"), "and runs straight on")
        let on = ProductGuide.text(.gettingAround, runningVersion: nil, calendar: true)
        expect(on.contains("- 2 Calendar:") && on.contains("- 3 Emails:"), "on: Calendar 2, Emails 3")
        expect(on.contains("- 6 Groups:"), "on: Groups 6")
    }

    /// verify8 V8-14: every topic that names the digit of a page takes it
    /// from the Navigate row the `?` sheet draws for this mailbox. With the
    /// Calendar page on, Emails is 3, Auth 4, Rules 5 and Groups 6 in the
    /// prose as on the keys; with it off, the sentences are main's own.
    static func aPagesDigitIsTheSheets() {
        func text(_ topic: ProductGuide.Topic, _ calendar: Bool) -> String {
            ProductGuide.text(topic, runningVersion: "0.0.12", calendar: calendar)
        }
        // Main's sentences, spelled out: the page off changes no byte of them.
        let mains: [(ProductGuide.Topic, String)] = [
            (.writingMail, "- Groups are made in Settings > Groups (key 5): n new"),
            (.writingMail, "- Scheduled mail waits on the Emails page (key 2) under \"sent\""),
            (.mailList, "The Emails page (key 2) is every message in one list."),
            (.senderRules, "- From Settings > Rules (key 4, or T on the Emails page): n."),
            (.reminders, "- Emails page (key 2), then the Reminders pill"),
            (.loginCodes, "on the Auth page (key 3, or g on the Emails page)"),
        ]
        for (topic, sentence) in mains {
            expect(text(topic, false).contains(sentence), "off: \(topic.rawValue) says main's \"\(sentence)\"")
            expect(!text(topic, true).contains(sentence), "on: \(topic.rawValue) no longer gives the page-off digit")
        }
        let shifted: [(ProductGuide.Topic, String)] = [
            (.writingMail, "Settings > Groups (key 6)"),
            (.writingMail, "the Emails page (key 3) under"),
            (.mailList, "The Emails page (key 3) is"),
            (.senderRules, "Settings > Rules (key 5, or T"),
            (.reminders, "- Emails page (key 3), then"),
            (.loginCodes, "the Auth page (key 4, or g"),
            (.calendar, "key 2, or ⇧C from anywhere. Emails is then key 3, Auth 4, Rules 5 and Groups 6."),
        ]
        for (topic, phrase) in shifted {
            expect(text(topic, true).contains(phrase), "on: \(topic.rawValue) says \"\(phrase)\"")
        }
        // The digits ARE the sheet's: each page's key in the prose is the key
        // beside its name in the Navigate row, for both tables.
        for calendar in [false, true] {
            let row = ShortcutCatalog.groups(calendar: calendar).first { $0.title == "Navigate" }!.items[0]
            let names = row.desc.components(separatedBy: " · ")
            expect(names.count == row.keys.count, "one digit a page (calendar \(calendar))")
            for (name, key) in zip(names, row.keys) {
                expect(ProductGuide.keyed("x {key:\(name)} y", calendar: calendar) == "x \(key) y",
                       "\(name) is \(key) (calendar \(calendar))")
            }
            for topic in ProductGuide.topics(calendar: calendar) {
                expect(!text(topic, calendar).contains("{key:"), "\(topic.rawValue): every digit was filled in (calendar \(calendar))")
            }
            // The getting-around list, numbered on its own, agrees with it.
            let around = text(.gettingAround, calendar)
            for (name, key) in zip(names, row.keys) {
                expect(around.contains("- \(key) \(name.prefix(1).uppercased() + name.dropFirst()):"),
                       "the pages list puts \(name) on \(key) too (calendar \(calendar))")
            }
        }
        expect(ProductGuide.keyed("{key:nowhere}", calendar: true) == "{key:nowhere}", "a page the row lacks is left to be caught")
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
        let french = ProductGuide.settings(matching: "how do I stop translating french")
        expect(french.contains("Languages I read"), "\"stop translating french\" finds the languages card")
        expect(french.contains("Mail > Translation"), "and says which pane it is on")
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
        // The newest ANNOUNCED release: the guide reads the table the way the
        // card does, and a quiet release is invisible to both.
        guard let newest = ReleaseNotes.all.first(where: { !$0.quiet }) else {
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
        guard let phone = PhoneReleaseNotes.all.first(where: { !$0.quiet }), let feature = phone.features.first else {
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
