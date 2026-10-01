// THE ALL-MAIL LIST'S CURSOR, kept out of the page's body.
//
// The cursor moves on every hover and every j/k. When it lived in the page's
// `@State`, each move re-ran the page's body, and the body is the one place that
// lays out every loaded row: hovering cost a diff of the whole list, and the
// list got slower with every older page the scroll pulled in. Here it is an
// observable object that only three things read: each row (`MailRowSlot`, which
// asks "am I the selected one"), the debounced thread prefetch, and the
// keyboard scroll-follow. A hover now redraws the rows on screen and nothing
// else, however long the list gets.

import SwiftUI

@MainActor
@Observable
final class ListCursor {
    var index = 0
    /// True only while the keyboard is driving the cursor.
    var kbActive = false
    /// True only while the pointer is actually over a row.
    var hovering = false
}

/// One row bound to the cursor. Writes are guarded: an observable setter
/// notifies its readers even when the value does not change, and the pointer
/// reports a hover on every movement inside a row.
struct MailRowSlot: View {
    let cursor: ListCursor
    let position: Int
    let update: AttentionUpdate
    let done: Bool
    let verbHint: String?
    let onOpen: () -> Void
    let onAppear: () -> Void

    var body: some View {
        MailRow(
            update: update,
            selected: cursor.kbActive && cursor.index == position,
            done: done,
            verbHint: verbHint,
            onHover: {
                // A hover must NOT follow-scroll: a row near the viewport edge
                // would jump the list out from under the pointer.
                if !cursor.hovering { cursor.hovering = true }
                if cursor.kbActive { cursor.kbActive = false }
                if cursor.index != position { cursor.index = position }
            },
            onOpen: onOpen
        )
        .onAppear(perform: onAppear)
    }
}

/// Pulls the thread for the row the cursor rests on, DEBOUNCED so sweeping the
/// list fires one request for the row you stop on rather than one per row you
/// pass. Covers everything past the bounded head-warm.
struct CursorPrefetch: View {
    let cursor: ListCursor
    let rowAt: (Int) -> (id: Int, threadId: String)?

    var body: some View {
        let row = rowAt(cursor.index)
        Color.clear
            .frame(width: 0, height: 0)
            .task(id: row?.id) {
                guard let row else { return }
                try? await Task.sleep(for: .milliseconds(120))
                guard !Task.isCancelled else { return }
                ThreadPrefetch.shared.prefetch(row.threadId)
            }
    }
}

/// Scrolls to follow the KEYBOARD cursor, never the pointer's.
struct CursorScrollFollower: View {
    let cursor: ListCursor
    let proxy: ScrollViewProxy
    let idAt: (Int) -> AnyHashable?

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .onChange(of: cursor.index) { _, i in
                guard cursor.kbActive, let id = idAt(i) else { return }
                withAnimation(Motion.scrollFollow) { proxy.scrollTo(id, anchor: .center) }
            }
    }
}
