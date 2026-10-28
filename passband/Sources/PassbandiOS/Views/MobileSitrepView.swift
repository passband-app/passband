// THE PHONE'S DASHBOARD — the ACTION half of the Mac's SitrepView, folded to one
// column: the hero states what needs you today, For-your-eyes ranks the standing
// band, and the reading zone offers the rule-onboarding it always has.
//
// The Mac's other half, the pinned records rail, is a TAB here instead
// (MobileRecordsView). That split is the phone's own: a Mac shows work surface
// and reference material side by side, and a phone that stacked them would bury
// the four record cards under a scroll nobody reaches — reference material you
// have to scroll past your obligations to reach is reference material you stop
// consulting.
//
// WHAT IS NOT HERE, and why. The Mac's masthead is a chrome bar (wordmark,
// re-triage, today's stamp, sync age) — a phone has a navigation bar and a tab
// bar for all of that. The status strip's doors (rules, cost) belong to
// destinations this phase does not have. And every keyboard cursor the Mac's
// dashboard carries is a hover/j-k concept with no thumb equivalent: a tap
// opens the mail, and that is the whole interaction.
//
// TWO THINGS THIS SCREEN USED TO CARRY AND NO LONGER DOES. The band-count strip
// (standing / new / open) was a dashboard metric on a screen whose whole
// argument is that it names obligations in words, not numbers. And the process
// deck is gone from the phone entirely — a card-at-a-time queue wants a keyboard
// and a session, and neither is what a phone is for. The body below stays one
// thing: what needs you.
//
// WHAT IT GAINED IS TWO CORNERS. The navigation bar carries the account page
// (leading) and the composer (trailing), because this is the screen the app
// opens on and neither of those was worth a tab. They are in the CHROME and not
// in the column on purpose: the scroll is obligations, and a bar button is
// reachable without disturbing a single one of them.
//
// THE TRAILING CORNER USED TO BE THE KEY, and the swap is a trade of a door you
// open most days for one that is empty most days. Writing an email is the thing
// this app could not do from a phone at all (#166); the login codes are a lookup,
// and a lookup belongs on Quick Look with the other things the app pulled out of
// your mail and is holding (MobileRecordsView's bar carries the key now, dot and
// all).
//
// THE URGENT CASE COMES IN BY NOTIFICATION. A code that just landed is not a
// lookup — you are standing at a login form holding the phone the code is on —
// and its banner is the fast path: a tap opens the login-codes page straight
// from wherever you were (`AppStore.requestAuthPage`), and a reveal there raises
// AuthCodeModal over the whole shell.

import SwiftUI

struct MobileSitrepView: View {
    @Environment(AppStore.self) private var store
    @Environment(Prefs.self) private var prefs
    /// The login-codes push, owned by the shell so a notification tap can open
    /// it from any tab. Pushed from HERE only while this board holds the
    /// records (`holdsRecords`); otherwise Quick Look's bar is its door.
    @Binding var showAuth: Bool
    /// The calendar holds the tab bar's second slot, so Quick Look's records
    /// (and their login-codes key) live under this board instead.
    let holdsRecords: Bool

    /// ReadingZone takes a cursor because the Mac drives it with the
    /// keyboard. Nothing writes to this one — it is the shared component's
    /// price of admission, and holding it here keeps that zone byte-identical
    /// across the two shells rather than forking it for the phone.
    @State private var cursor = SitrepCursor()

    /// How many ranked standing items show before the "{n} more" expander;
    /// see `BoardFold` for why four.
    fileprivate static let eyesVisible = BoardFold.forYourEyes
    @State private var expanded = false
    /// THE BOARD'S CLOCK, on the minute: what the rows say with the time alone
    /// ("3m", a deadline chip) and today's date in the headline's key used to
    /// ride on the redraw every poll brought, when the board read the stats.
    @State private var minute = Date()

    /// Warm the threads behind the rows showing, the first four at once.
    fileprivate static func warm(_ visible: [AttentionUpdate]) {
        ThreadPrefetch.shared.warm(
            visible.map(\.thread_id), immediate: eyesVisible, spacing: .milliseconds(150))
    }

    /// When the drag now in flight started, to tell a page swipe from a row's.
    @State private var dragBegan: Date?

    /// LEFT TO RIGHT ANYWHERE ON THE BOARD OPENS THE ACCOUNT PICKER: a drawer
    /// from the left with the mailboxes in it, the phone's answer to the Mac's
    /// rail of accounts, one gesture from the screen the app opens on.
    ///
    /// EXCEPT ON A MAIL ROW, which has its own use for that direction (reply,
    /// remind, archive). The row's rail is a UIKit pan and this is a SwiftUI
    /// one, so both hear the drag; the row stamps `SwipeRowActivity` when it
    /// takes it, and a drag a row took is not a page swipe.
    ///
    /// Simultaneous, so the scroll never waits on it, and judged only when the
    /// finger lifts: mostly sideways, and far enough to have been meant.
    private var accountSwipe: some Gesture {
        DragGesture(minimumDistance: 24)
            .onChanged { value in
                if dragBegan == nil { dragBegan = value.time }
            }
            .onEnded { value in
                let began = dragBegan ?? value.time
                dragBegan = nil
                let dx = value.translation.width, dy = value.translation.height
                guard dx > 90, abs(dy) < dx * 0.5 else { return }
                // A row that was panned at any point during this drag took
                // it. A little slack before the start: the row's recognizer
                // and this one do not begin on the same event.
                guard SwipeRowActivity.lastActive < began.addingTimeInterval(-0.3) else { return }
                Haptics.tick()
                AccountDoor.shared.pane = .picker
            }
    }

    var body: some View {
        let _ = minute
        // Preserve the server order on every device.
        let ranked = store.sitrep.standing
        let visible = expanded ? ranked : Array(ranked.prefix(Self.eyesVisible))
        let overflow = ranked.count - Self.eyesVisible

        ScrollView {
            VStack(spacing: 0) {
                VStack(alignment: .leading, spacing: 16) {
                    // ABOVE THE HERO, because it outranks it, as on the Mac: "what
                    // needs you today" is worthless while the mailbox behind it
                    // has been frozen since Tuesday.
                    banner
                    hero
                    if !ranked.isEmpty {
                        // Auth mail stays off the reader's queue: it opens in the
                        // one-time reveal, never in the reader.
                        forYourEyes(
                            visible: visible, overflow: overflow,
                            queue: ranked.filter { store.authMeta(for: $0) == nil })
                    } else if isClear {
                        // An empty board is the best state this app has, and it
                        // should not look like a broken one.
                        ClearNote()
                            .padding(.horizontal, 4)
                            .transition(.opacity)
                    }
                    // Empty reading still stands beside a live For-your-eyes, but
                    // on a board with nothing else it is one more absence.
                    if !(reading.isEmpty && ranked.isEmpty) {
                        ReadingZone(senders: reading, cursor: cursor)
                    }
                    BoardFootnote(bannerShowing: bannerShowing)
                }
                .padding(.horizontal, 16)
                .padding(.bottom, holdsRecords ? 24 : 28)
                if holdsRecords {
                    RecordsTray()
                }
            }
        }
        .background(Palette.canvas)
        .task {
            await clockTicks(every: 60, while: { Platform.isAppOnScreen }) {
                // Not on a fresh mount, which has just drawn with now.
                let now = Date()
                if abs(now.timeIntervalSince(minute)) >= 1 { minute = now }
            }
        }
        .navigationTitle("Sitrep")
        .navigationBarTitleDisplayMode(.inline)
        // THE TWO DOORS OFF THE DASHBOARD: who you are, and the one verb this
        // app owes you that no amount of triage can stand in for. The account
        // page used to be a tab and did not earn one — a tab claims you will be
        // there often and it is a twice-a-month visit — so it gets a corner of
        // this bar instead of permanent width in the tab bar.
        //
        // The sides are the platform's habit, not a coin flip: a person glyph
        // leading is where every phone keeps "you", and the trailing corner is
        // the thumb's corner, which is the right one for the thing you actually
        // reach for.
        //
        // BARE GLYPHS, NOT BUTTONS. iOS 26 gives a toolbar item its own glass
        // capsule by default, and two of them over a dashboard that is already
        // a column of glass cards reads as chrome competing with content. The
        // background goes; the accent stays, which is the platform's own signal
        // that a glyph is a door and the reason a bare icon still looks tappable.
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                HStack(spacing: 18) {
                    // The picker, exactly what the swipe from the left edge
                    // opens: the glyph and the gesture are one door. The full
                    // Account page is the drawer's Settings button.
                    Button {
                        Haptics.tick()
                        AccountDoor.shared.pane = .picker
                    } label: {
                        Image(systemName: "person.crop.circle")
                            .foregroundStyle(Palette.accent)
                    }
                    .accessibilityLabel("Account")
                    // THE LOGIN CODES, while the calendar holds Quick Look's
                    // tab: a key beside "you", because the codes are your
                    // accounts' and this is the screen the app opens on. A code
                    // you are racing arrives by banner and opens the page from
                    // anywhere; this is the door for looking one up.
                    if holdsRecords {
                        CodesKey { showAuth = true }
                    }
                }
            }
            .sharedBackgroundVisibility(.hidden)

            ToolbarItem(placement: .topBarTrailing) {
                // THE COMPOSER, ON THE SCREEN THE APP OPENS ON. Until #166 the
                // phone could not write an email at all, and the fix is not a
                // fifth tab: composing is a verb, not a place, and a verb wants
                // the corner your thumb is already resting on.
                //
                // It opens the SAME `store.compose` the mail tab's button and
                // the Mac's `c` open — one composer, one draft, one autosave —
                // and the sheet it raises is mounted on the shell, so the
                // dashboard is what you come back to.
                Button {
                    store.openComposeNew()
                } label: {
                    Image(systemName: "square.and.pencil")
                        .foregroundStyle(Palette.accent)
                }
                .accessibilityLabel("New message")
            }
            .sharedBackgroundVisibility(.hidden)
        }
        #if DEBUG
            // A screenshot's way down to the records (`records-bottom`).
            .defaultScrollAnchor(FixtureStage.scene == "records-bottom" ? .bottom : nil)
        #endif
        .simultaneousGesture(accountSwipe)
        .refreshable { await RecordZones.refresh(store) }
        .navigationDestination(isPresented: holdsRecords ? $showAuth : .constant(false)) {
            MobileAuthView()
        }
        // The zones each ask for this too; the store joins the in-flight pass, so
        // asking here as well is what makes a cold launch paint from one round of
        // requests rather than from whichever zone happened to mount first.
        .task { await store.refreshZones() }
        // THE ROWS ON SCREEN, not the band: this ran on every return to the
        // tab and warmed every standing thread, up to two hundred fetches for
        // four rows anybody could tap. Expanding the list, or a row changing,
        // warms what that brings into view, and every poll keeps them warm (a
        // copy lapses, or a reply drops it); a warm of threads already cached
        // asks nothing.
        .onAppear { Self.warm(visible) }
        .onChange(of: visible.map(\.id)) { _, _ in Self.warm(visible) }
        .background { VisibleWarmKeeper(rows: visible) }
        .onChange(of: visible.count) { _, count in
            if count == 0 { expanded = false }
        }
    }

    // MARK: - what is wrong, when something is

    /// The Mac's banner slot: a dead Google sign in with its way back, or
    /// whatever else is keeping mail from arriving or being sorted. Not
    /// dismissible on either platform, because neither goes away by wishing.
    @ViewBuilder
    private var banner: some View {
        if store.gmailDisconnected {
            GmailDisconnectedBanner()
        } else if store.syncStatus.showsBanner {
            SyncStatusBanner(status: store.syncStatus)
        }
    }

    private var bannerShowing: Bool {
        store.gmailDisconnected || store.syncStatus.showsBanner
    }

    private var reading: [ReadingSender] {
        store.readingOnBoard
    }

    /// Nothing for your eyes, and truly so: the board has loaded, and it is
    /// not empty only because sync or triage is stuck.
    private var isClear: Bool {
        store.hasStats && store.sitrep.standing.isEmpty
            && store.syncStatus.emptyHero(
                queued: store.triageWaiting, lastSync: store.daemonLastSync) == nil
    }

    // MARK: - hero

    /// THE ONE SERIF LINE ON THIS SCREEN. Same sentence the Mac opens with, at a
    /// phone's measure — the brand's voice, said once, then out of the way.
    private var hero: some View {
        VStack(alignment: .leading, spacing: 3) {
            GreetingLine()
            Text(headline)
                .font(Typo.hero(30))
                .foregroundStyle(Palette.ink)
                .lineSpacing(-1)
                .fixedSize(horizontal: false, vertical: true)
                .contentTransition(.opacity)
                .animation(.easeInOut(duration: 0.35), value: headline)
                // Tap for another line, as on the Mac.
                .opacity(store.fyeHeadlineRegenerating ? 0.45 : 1)
                .animation(.easeInOut(duration: 0.2), value: store.fyeHeadlineRegenerating)
                .contentShape(Rectangle())
                .onTapGesture {
                    guard !store.sitrep.standing.isEmpty else { return }
                    Task { await store.regenerateFyeHeadline() }
                }
        }
        .task(id: store.headlineKey) { await store.refreshFyeHeadline() }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, 4)
    }

    private var headline: String {
        // Only "all clear" when triage has actually run; the footnote says why.
        HeroLine.title(
            headline: store.fyeHeadline, count: store.sitrep.standing.count,
            // Asked only over an empty board, the one place it is said: the
            // line is read off the stats, which change every poll.
            empty: store.sitrep.standing.isEmpty
                ? store.syncStatus.emptyHero(
                    queued: store.triageWaiting, lastSync: store.daemonLastSync)?.title
                : nil)
    }

    // MARK: - for your eyes

    /// The standing band, ranked. Tapping a row opens the reader with the WHOLE
    /// ranked list as its queue, which is what the Mac's list rows hand over —
    /// the dashboard's own rows pass no queue there because h/l is a keyboard
    /// walk, and a thumb has no such walk to protect.
    ///
    /// The rows swipe. They cannot use SwiftUI's `swipeActions` the way the mail
    /// tab does — a `List` inside this ScrollView would be two scrollers fighting
    /// — so `SwipeRow` draws the same rails by hand, off the same verb arrays, and
    /// the long press carries the rest of the keymap on both.
    private func forYourEyes(visible: [AttentionUpdate], overflow: Int, queue: [AttentionUpdate])
        -> some View
    {
        ZoneCard(symbol: "eye", title: "For your eyes", count: store.sitrep.standing.count) {
            VStack(spacing: 2) {
                ForEach(Array(visible.enumerated()), id: \.element.id) { _, u in
                    let verbs = UpdateVerbs(update: u, queue: queue, surface: .sitrep)
                    if let meta = store.authMeta(for: u) {
                        // Sealed: summary and a lock, a tap opens the audited
                        // reveal, and the only swipe is Done. Reply, remind,
                        // archive and tune all start from somewhere this row
                        // does not send auth mail.
                        SwipeRow(leading: [], trailing: verbs.doneVerbs) {
                            MobileAuthEyesRow(update: u, meta: meta) { store.requestReveal(meta) }
                        }
                        .padding(.horizontal, -PhoneRow.hPadding)
                    } else {
                        SwipeRow(leading: verbs.leadingVerbs, trailing: verbs.trailingVerbs) {
                            UpdateRow(
                                update: u,
                                selected: false,
                                onHover: {},
                                onOpen: verbs.open)
                        }
                        // The row's words on the zone title's line; its press
                        // wash and swipe rails bleed into the card's padding.
                        .padding(.horizontal, -PhoneRow.hPadding)
                        .updateContextMenu(verbs)
                    }
                }
                if overflow > 0 {
                    // THE WHOLE ROW IS THE BUTTON, and only the pill is drawn.
                    // A thumb aims at a line of a list, not at a 60pt chip at
                    // the end of it, and every row above this one is already
                    // tappable edge to edge — a control that looks like it
                    // belongs to that stack and answers to a third of its width
                    // reads as a miss rather than as a smaller target.
                    //
                    // So the glass is painted by hand (`glassCapsule`) inside a
                    // `.plain` button and `contentShape` claims the full width:
                    // `.buttonStyle(.glass)` would put the hit area exactly where
                    // the pill is, which is the problem.
                    Button {
                        withAnimation(Motion.disclose) { expanded.toggle() }
                    } label: {
                        HStack(spacing: 0) {
                            Text(expanded ? "show less" : "\(overflow) more")
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaint)
                                .padding(.horizontal, 11)
                                .padding(.vertical, 5)
                                .glassCapsule()
                            Spacer(minLength: 0)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 6)
                }
            }
        }
    }
}

// MARK: - footnote

/// The page's last line: how fresh the board is, and what went wrong if
/// anything did. The Mac says this in its masthead; a phone has no masthead
/// to say it in, and "when did this last update" is the one question a
/// glanceable surface owes an answer to.
///
/// ITS OWN VIEW: the stamp is read off the stats and the client's refresh
/// time, and both move every poll. Inline, that made the whole board a reader
/// of them; here only this line redraws.
private struct BoardFootnote: View {
    @Environment(AppStore.self) private var store
    let bannerShowing: Bool

    var body: some View {
        if let refreshError = store.refreshError {
            Label(refreshError.message, systemImage: "exclamationmark.triangle.fill")
                .font(Typo.micro)
                .foregroundStyle(Palette.warn)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else if !bannerShowing, let line = store.syncStatus.oneLine() {
            // The same derived status the Mac's banner shows, in one line.
            Label(line, systemImage: "clock")
                .font(Typo.micro)
                .foregroundStyle(store.syncStatus.isWarning ? Palette.warn : Palette.inkFaint)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            // The same stamp and tint as the Mac's status strip.
            let stamp = SyncStatus.syncedStamp(
                stats: store.stats, clientRefresh: store.lastRefresh, date: Fmt.date)
            Text(stamp.text)
                .font(Typo.micro)
                .foregroundStyle(stamp.warn ? Palette.warn : Palette.inkFaintest)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Auth mail in For your eyes, at phone scale: a critical account change nobody
/// has ruled on, or one the owner said was not them. The model's one-line
/// summary and a lock; the body is sealed and is never fetched to draw this.
struct MobileAuthEyesRow: View {
    let update: AttentionUpdate
    let meta: SealedMeta
    let onOpen: () -> Void

    private var tone: Color { meta.isCritical ? Palette.danger : Palette.lock }

    var body: some View {
        ListRow(
            selected: false, cornerRadius: PhoneRow.radius, tint: tone, hPadding: PhoneRow.hPadding, vPadding: 8,
            hoverFill: false, action: onOpen
        ) { _, _ in
            HStack(alignment: .top, spacing: 10) {
                Avatar(sender: update.senderString, size: 26)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(SenderCache.resolved(update.senderString).displayName)
                            .font(.system(size: 14, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                        Image(systemName: "lock.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(tone)
                        Spacer(minLength: 4)
                        Text(Fmt.relAge(meta.received_at))
                            .font(Typo.num(11))
                            .foregroundStyle(Palette.inkFaintest)
                    }
                    Text(update.one_line.isEmpty ? AuthCopy.label(meta.kind) : update.one_line)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                    Label(
                        meta.isCritical ? "Critical" : AuthCopy.label(meta.kind),
                        systemImage: meta.isCritical
                            ? "exclamationmark.triangle.fill" : AuthCopy.symbol(meta.kind)
                    )
                    .font(Typo.micro)
                    .foregroundStyle(tone)
                }
            }
        }
        .accessibilityLabel(
            "\(AuthCopy.label(meta.kind)) from \(SenderCache.resolved(update.senderString).displayName), sealed")
    }
}

/// Re-warms the rows showing on every poll, as the Mac's board keeper does.
/// A zero-size view of its own so that reading the poll heartbeat invalidates
/// it and not the board.
private struct VisibleWarmKeeper: View {
    @Environment(AppStore.self) private var store
    let rows: [AttentionUpdate]

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            // THE ROWS ON SCREEN ONLY, as the Mac's keeper does: with the
            // list expanded, re-warming all of it every poll would start a
            // long trickle every ten seconds.
            .onChange(of: store.lastRefresh) { _, _ in
                MobileSitrepView.warm(Array(rows.prefix(MobileSitrepView.eyesVisible)))
            }
    }
}

/// QUICK LOOK'S RECORDS, under the board while the calendar holds their tab.
/// No heading: what sets them apart is the surface. The board's cards sit on
/// the page; these sit on a tray a shade off it, rounded at the top and
/// running to the bottom of the scroll, so the eye reads a second layer (the
/// things kept for you) under the first (the things that need you) without a
/// line of copy to explain it. An even fill, no edge stroke along one side.
private struct RecordsTray: View {
    var body: some View {
        RecordZones()
            .padding(.horizontal, 16)
            .padding(.top, 20)
            .padding(.bottom, 40)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                UnevenRoundedRectangle(
                    topLeadingRadius: 28, bottomLeadingRadius: 0, bottomTrailingRadius: 0,
                    topTrailingRadius: 28, style: .continuous)
                    .fill(Palette.hairline.opacity(0.9))
                    // Past the end of the scroll too, so a bounce never shows
                    // the page under the tray's foot.
                    .padding(.bottom, -600)
            )
    }
}
