// THE KEYBOARD SHORTCUTS, AS THEY ARE TOLD. Curated and grouped by hand rather
// than dumped from the keymap registry, which carries per-context duplicates
// and only knows the surfaces that happen to be mounted.
//
// ONE TABLE, TWO READERS: the `?` overlay draws it (Views/ActionLayer.swift)
// and the assistant's list_shortcuts tool recites it (Lib/ProductGuide.swift).
// It used to live inside the overlay, and a second copy written for the
// assistant would have been the copy nobody updates: the help on screen and
// the help in the chat disagreeing about a key is worse than either being
// silent. Foundation only, so the suite that pins the assistant's answers
// compiles it with no app around it.

import Foundation

enum ShortcutCatalog {
    struct Item: Sendable {
        /// Key caps as the overlay draws them: "j", "Enter", "⌘K".
        var keys: [String]
        var desc: String

        init(_ keys: [String], _ desc: String) {
            self.keys = keys
            self.desc = desc
        }
    }

    struct Group: Identifiable, Sendable {
        var title: String
        var items: [Item]
        var id: String { title }
    }

    static let groups: [Group] = [
        Group(
            title: "Sitrep & inbox",
            items: [
                Item(["j", "k"], "move selection"),
                Item(["Enter"], "open thread"),
                Item(["r"], "reply"),
                Item(["c"], "new message"),
                Item(["e", "d"], "mark done"),
                Item(["v"], "fix a wrong triage verdict"),
                Item(["h"], "remind me about this later"),
                Item(["t"], "tune sender rule"),
                Item(["f"], "search this sender"),
                Item(["u"], "undo last action"),
            ]),
        Group(
            title: "Thread viewer",
            items: [
                // In key order: the thread reads oldest to newest down the
                // page, so j (down) is the NEWER message.
                Item(["j", "k"], "newer / older message"),
                Item(["←", "→"], "previous / next queued email"),
                Item(["r"], "reply"),
                Item(["Enter"], "reply all"),
                Item(["c"], "new message"),
                Item(["e", "d"], "done, and close the reader"),
                Item(["E", "D"], "done, and open the next email"),
                Item(["S"], "skip to the next email, leave this one"),
                Item(["h"], "remind + next"),
                Item(["u"], "unsubscribe from this sender"),
                Item(["t"], "new rule for this sender"),
                // `f` is FORWARD in the reader and search-this-sender
                // everywhere else; the group above still shows `f` as search,
                // and this row is the one place a reader can see that the key
                // changes hands. The reader has no search-this-sender key of
                // its own: `/` then `from:` is the spelled-out way.
                Item(["f"], "forward this message"),
                // ⌘F is search everywhere else (the menu); with an email
                // open it finds inside it, the way it does on a web page.
                Item(["⌘F"], "find in this email"),
                Item(["Esc"], "back"),
            ]),
        Group(
            title: "Navigate",
            items: [
                Item(["1", "2", "3", "4", "5"], "sitrep · emails · auth · rules · groups"),
                Item(["⌘["], "back"),
                Item(["⌘]"], "forward"),
                Item(["a"], "browse all mail"),
                Item(["/"], "search"),
                Item(["⌘K"], "ask your inbox"),
                Item(["⌘⇧K"], "new ask chat"),
                Item(["⌘,"], "settings"),
                Item(["g"], "auth messages"),
                Item(["T"], "rules"),
                Item(["A"], "audit log (in settings)"),
            ]),
        Group(
            title: "App",
            items: [
                Item(["\\"], "toggle light / dark theme"),
                Item(["?"], "this help"),
                Item(["Esc"], "close panel / overlay"),
            ]),
    ]

    // MARK: - the calendar, while it is offered

    /// The Calendar page's own keys. Listed only while the page is offered
    /// (`AppStore.calendarPageAvailable`), so nothing is advertised where it
    /// does nothing.
    static let calendarGroup = Group(
        title: "Calendar",
        items: [
            Item(["m", "w", "a"], "month / week / agenda"),
            Item(["j", "k"], "move selection"),
            Item(["Enter"], "open the mail behind it"),
            Item(["y"], "going (again: not going)"),
            Item(["x"], "dismiss"),
            Item(["n"], "new event"),
            Item(["e"], "edit event"),
            Item(["⌘↵"], "save the event"),
            Item(["Tab"], "new event: to a suggested guest"),
            Item(["⌫"], "delete event"),
            Item(["←", "→", "↑", "↓"], "month: move the day"),
            Item(["←", "→"], "week: move the day"),
            Item(["[", "]"], "month, week: previous / next"),
            Item(["t"], "month, week: today"),
            Item(["Enter"], "month: open the day"),
        ])

    /// The Navigate digits while the calendar is offered: Calendar is 2 and
    /// every later page one higher (`CalendarGate.numbered`).
    static let calendarDigits = Item(
        ["1", "2", "3", "4", "5", "6"], "sitrep · calendar · emails · auth · rules · groups")

    /// The table as the `?` overlay draws it for one account: exactly
    /// `groups` while the calendar is not offered, and with the calendar in
    /// the digits (and ⇧C) under Navigate plus the page's own group while it
    /// is.
    static func groups(calendar: Bool) -> [Group] {
        guard calendar else { return groups }
        var out = groups.map { group in
            guard group.title == "Navigate" else { return group }
            var g = group
            g.items[0] = calendarDigits
            g.items.insert(Item(["⇧C"], "calendar"), at: 1)
            return g
        }
        out.insert(calendarGroup, at: min(2, out.count))
        return out
    }
}
