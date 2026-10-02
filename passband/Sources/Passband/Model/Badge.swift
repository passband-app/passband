// THE APP ICON'S NUMBER, ON THE PHONE ONLY: how many items are in
// For-your-eyes, said to someone who is not in the app. The Mac's dock shows
// none (see `set`).
//
// It is `NeedToday.count` (the number of rows in the band) off the same
// `store.sitrep.standing` the app shows, because a badge is read WITHOUT the sentence
// that would correct it. Two counts would eventually differ and the badge is the
// one nobody can check.
//
// WRITTEN FROM TWO PLACES, and the second one is not redundant.
//
// `AppStore.sitrep`'s `didSet` catches every CHANGE: the poller's pull, a
// resolve, a re-triage, the wipe on disconnect all end in an assignment to that
// property, so hooking it rather than the callers is what stops a sixth path
// from shipping a stale number by forgetting to call anything.
//
// What a didSet cannot catch is a CONFIRMATION. `SitrepPoller` assigns only when
// the read model actually differs — writing an identical one every ten seconds
// would re-lay out the dashboard for nothing — so "the daemon agrees with what
// we already had" produces no assignment and no observer. That is fine for
// anything whose only writer is this process, and wrong for the badge, whose
// other writer runs while this process does not. A launch begins with an empty
// standing band; if the extension left a 3 on the icon and the daemon now agrees
// the band is empty, nothing is assigned and the 3 survives with the app open in
// front of it. So the poller refreshes unconditionally after each successful
// pull, which is the only place that learns the number is still right.
//
// AND A SECOND WRITER THAT IS NOT THIS FILE. The badge has to be right while the
// app is CLOSED, which is exactly when this code is not running, so the
// notification service extension stamps its own count onto each push it
// enriches (Sources/PassbandiOSNotify/NotificationService.swift). That is the
// only other thing that may set it, it uses the same pure count, and it reads
// the band with `peek` so refreshing a number never marks mail as seen.
//
// TWO PLATFORMS, ONE NUMBER. An iOS badge is a notification capability and
// shows nothing unless the user granted `.badge` (see PushRegistration), which
// makes it opt-in; the Mac dock asks nobody, so it gets no number at all.

import Foundation

#if os(macOS)
    import AppKit
#else
    import UserNotifications
#endif

@MainActor
enum Badge {
    /// The whole contract: given the standing band, put its row count on the
    /// app icon (the phone's; the Mac dock gets none). Zero clears the badge rather than drawing a "0" — an icon
    /// wearing a zero is a notification that nothing happened.
    /// What the icon was last told. nil = never told this session, so the
    /// first confirmation always goes through (the notification extension
    /// may have written a stale count while the app was closed).
    private static var lastSet: Int?

    static func refresh(_ standing: [AttentionUpdate]) {
        set(NeedToday.count(standing))
    }

    static func set(_ count: Int) {
        // The dock is asked once per CHANGE. The poller confirms the count on
        // every tick on purpose (see SitrepPoller), but a confirmation of the
        // number already on the icon has nothing to tell the window server.
        if lastSet == count { return }
        lastSet = count
        #if os(macOS)
            // NO DOCK NUMBER. A red count on the dock is the same backlog the
            // sitrep stopped headlining, said to someone who is not even in
            // the app, and it is what made opening Passband feel like a chore.
            // Cleared rather than skipped so nothing an older build left can
            // linger; the phone's badge is opt-in (`.badge`) and stays.
            _ = count
            NSApplication.shared.dockTile.badgeLabel = nil
        #else
            // Fails quietly when the user never granted `.badge`, which is the
            // behavior we want: nothing to report and nothing to report it to.
            UNUserNotificationCenter.current().setBadgeCount(count)
        #endif
    }
}
