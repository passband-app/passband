// THE PAGE KEYS: the global digits and ⇧C, as RootView registers them. Here
// rather than in RootView so the key-dispatch suite compiles the SAME
// bindings the app does (test.sh: this file rides along with KeyDispatch.swift)
// instead of a hand copy that could keep passing while the real ones change.
//
// Generic over the page, and handed the gate as closures asked AT KEY TIME:
// reading the gate where the bindings are built would make the whole shell a
// reader of the stats poll, and asking when the key is pressed means the gate
// opening renumbers without anything re-registering.

import Foundation

enum PageKeys {
    /// One DECLINING binding per digit, 1 through `count`. Each asks for the
    /// page order when pressed and opens the page at its position; a digit
    /// past the end of the order passes on to whatever else binds it.
    static func digits<Page>(
        count: Int, pages: @escaping () -> [Page], go: @escaping (Page) -> Void
    ) -> [KeyBinding] {
        (1...max(count, 1)).map { number in
            KeyBinding(declining: "\(number)", "go to page \(number)") {
                let order = pages()
                guard order.indices.contains(number - 1) else { return false }
                go(order[number - 1])
                return true
            }
        }
    }

    /// ⇧C OPENS THE CALENDAR. EXACT CASE, so a bare `c` (compose) is
    /// untouched: the shifted letter used to reach compose only through the
    /// dispatcher's case fold, and now means this instead wherever the page
    /// is offered. DECLINING: while `offered` says no, the key passes on and
    /// the fold hands it to `c` exactly as before.
    static func calendar(offered: @escaping () -> Bool, go: @escaping () -> Void) -> KeyBinding {
        KeyBinding(declining: "C", "go to calendar", exactCase: true) {
            guard offered() else { return false }
            go()
            return true
        }
    }
}
