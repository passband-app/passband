// QUICK LOOK: the desktop sitrep's pinned right rail, on a phone. Calendar /
// Shipments / Banking / Receipts are the SAME components the Mac's rail is
// built from (Views/SitrepZones.swift), and their own header says why they are
// a place rather than a feed: these are RECORDS, not actions. They are
// auto-resolved out of the attention bands at ingest, so this is their only
// surface, and each one renders empty state and all: a column that comes and
// goes as mail arrives is one you stop trusting.
//
// Everything here is something the app pulled out of your mail and is holding
// for you to look up, never something to answer. That is why the name is Quick
// Look rather than Records: you come here to READ ONE FACT (the flight time,
// the tracking number) and leave.
//
// WHERE IT LIVES. Under the sitrep's board, on its own tray (MobileSitrepView's
// `RecordsTray`). It had a tab of its own once; the tab bar's middle slot is
// the calendar's now, for every mailbox, and a calendar is a place you live in
// where records are a place you look one fact up.
//
// THE LOGIN CODES' KEY (`CodesKey`) is the sitrep's, in its leading corner. What
// it opens is a lookup (the sign-in alert from Tuesday, the reset you half
// remember asking for); the case that is a RACE comes in by banner, and tapping
// it opens the same page from any tab (`AppStore.requestAuthPage`).
//
// Tapping a row opens the underlying email through `store.openThread`, exactly
// as it does on the Mac; the push itself is the shell's, bound to the same
// `store.threadId`.

import SwiftUI

/// The four record cards, in the Mac rail's order, under the sitrep's board
/// (MobileSitrepView).
///
/// TWO COLUMNS WHEN THERE IS ROOM (a phone on its side): one card at 700
/// points wide is a row of words with a desert after them. The columns are
/// balanced by hand, the tall one (shipments, each a little card of its own)
/// beside the two short lists, so neither column ends far above the other.
struct RecordZones: View {
    @State private var width: CGFloat = 0

    /// Two cards side by side keep each about as wide as a phone held upright.
    static let twoColumnWidth: CGFloat = 640

    var body: some View {
        Group {
            if width >= Self.twoColumnWidth {
                HStack(alignment: .top, spacing: 16) {
                    VStack(alignment: .leading, spacing: 16) {
                        CalendarZone()
                        BankingZone()
                    }
                    VStack(alignment: .leading, spacing: 16) {
                        ShipmentsZone()
                        ReceiptsZone()
                    }
                }
            } else {
                VStack(alignment: .leading, spacing: 16) {
                    CalendarZone()
                    ShipmentsZone()
                    BankingZone()
                    ReceiptsZone()
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
    }

    /// Both halves, because a record can arrive either way: the zones come
    /// from the zones fetch, and the resolution that MOVED a mail into one of
    /// them rides the sitrep pull. Pulling only the zones would leave the
    /// bands claiming an item this screen has already taken over.
    static func refresh(_ store: AppStore) async {
        _ = await SitrepPoller.shared.pull()
        await store.refreshZones(force: true)
    }
}

/// THE LOGIN CODES' DOOR: a key, with a dot when something just landed. The
/// sitrep's bar carries it.
struct CodesKey: View {
    @Environment(AppStore.self) private var store
    let action: () -> Void

    /// Is there a sealed message young enough to still be worth racing to? The
    /// window and the test both belong to MobileAuthView, which is the surface
    /// that has to keep its word about them; this is only the badge asking.
    ///
    /// EVERY sealed kind, not just the code kinds the sitrep's card is gated on:
    /// the dot's job is "there is something new behind this key", and a sign-in
    /// alert is something new behind this key even though nobody is racing it.
    private var liveCode: Bool {
        store.sitrep.sealed.contains { MobileAuthView.isLive($0) }
    }

    /// Spoken instead of drawn, for the same dot. Typed as `String` rather than
    /// inlined: a ternary of two literals leaves the compiler choosing between
    /// the StringProtocol and LocalizedStringKey overloads of
    /// `accessibilityValue`.
    private var liveCodeValue: String {
        liveCode ? "a code just arrived" : ""
    }

    var body: some View {
        Button(action: action) {
            // A DOT WHEN SOMETHING JUST LANDED. Behind a glyph the codes are
            // invisible, and something arriving while you were elsewhere is
            // the one moment this door has to speak up. Same live window the
            // page itself uses, so the dot and the row's "live" tag can never
            // disagree.
            //
            // The padding RESERVES the dot's corner rather than hanging it
            // outside the glyph: the item's own bounds are still what a
            // toolbar lays out and clips against, and a dot pushed past them
            // is at that clip's mercy.
            Image(systemName: "key.fill")
                .foregroundStyle(Palette.accent)
                .padding(.top, 3)
                .padding(.trailing, 3)
                .overlay(alignment: .topTrailing) {
                    if liveCode {
                        Circle()
                            .fill(Palette.positive)
                            .frame(width: 6, height: 6)
                    }
                }
        }
        .accessibilityLabel("Login codes")
        .accessibilityValue(liveCodeValue)
    }
}
