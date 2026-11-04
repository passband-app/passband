// THE ALL-MAIL PAGE'S FILTER BAR and the day headings under it.
//
// Left: the lenses, one at a time — All, Important, Deadlines, Attachments, and
// Reminders. The first four narrow the rows on screen (`AppStore.mailFilter`);
// Reminders swaps the page for the parked list (`AppStore.reminderFilter`).
// They sit in one row as peers because that is what they are to somebody using
// them, and choosing any one turns the others off. Right: the category picker,
// which hides whole sitrep zones and composes with the lens. Inbox only.
//
// Finished mail always shows, dimmed and checked: archived is not gone.

import SwiftUI

struct MailFilterBar: View {
    @Environment(AppStore.self) private var store
    /// Mail still to do per lens (`MailList.Counts`), so a chip says what
    /// clicking it gets you before you click. Over what is loaded so far.
    let counts: [MailFilter: Int]

    private var reminders: Bool { store.reminderFilter }

    #if os(macOS)
        /// The pills' natural width and the room the bar gives them, each
        /// reported only when it CHANGES (see `pillScroller`).
        @State private var pillsWidth: CGFloat = 0
        @State private var room: CGFloat = .infinity
    #endif

    var body: some View {
        HStack(spacing: Space.s6) {
            pillScroller {
                HStack(spacing: Space.s6) {
                    ForEach(MailFilter.allCases, id: \.self) { f in
                        FilterPill(
                            text: f.label,
                            count: f == .all ? nil : counts[f, default: 0],
                            active: !reminders && store.mailFilter == f,
                            help: f.help
                        ) {
                            store.reminderFilter = false
                            store.mailFilter = f
                        }
                    }
                    FilterPill(
                        text: "Reminders", icon: "bell", active: reminders,
                        help: "Mail you parked with a reminder (h sets one)"
                    ) {
                        store.mailFilter = .all
                        store.reminderFilter.toggle()
                    }
                }
            }
            // The reminder lens is your own parked list, which the picker does
            // not narrow; a lit control there would claim it did.
            CategoryPicker()
                .disabled(reminders)
                .opacity(reminders ? 0.4 : 1)
        }
        #if os(macOS)
            .padding(.horizontal, Space.page)
        #else
            // Trailing only: the lenses' own scroller reaches the screen's
            // leading edge and insets its content instead (`pillScroller`).
            .padding(.trailing, Space.s16)
        #endif
        .padding(.top, Space.s10)
        .padding(.bottom, Space.s2)
    }
}

/// The pills scroll sideways when they do not fit: always on a phone, where
/// five of them never do, and on a Mac only when the list is narrow (the
/// compose pane open beside it). A Mac with the room draws them as they are,
/// with no scroller at all; squeezed, they used to WRAP inside their capsules
/// ("Attachment / s") and then run under the picker.
///
/// THE SCROLLER ENDS WHERE THE PICKER BEGINS. It used to draw past its own
/// frame (`scrollClipDisabled`, so a pill could run out to the screen's edge),
/// and the frame's other neighbour is the category picker, whose capsule has
/// no fill: the fourth and fifth lenses were drawn straight through it. Now
/// the scroller keeps to its frame and, on a phone, gets its bleed the honest
/// way, by owning the leading edge of the screen and insetting what it
/// scrolls. On the picker's side the last few points fade out, which is what
/// says there are more lenses under there, and the inset on that side is the
/// fade's width, so the last lens scrolls clear of it and stops.
///
/// NOT A `ViewThatFits`. It was one, and on the Mac it cost nearly half the
/// main thread while the list below scrolled: every layout pass the list's
/// scroll set off, it rebuilt every pill and sized a scroll view from
/// scratch to ask the same question again (profiled 2026-11-04: 4,275 of
/// 9,100 main-thread samples). The answer only changes with the width, so it
/// is asked then: the pills' natural width and the bar's room are each
/// reported on change, and a plain `if` picks the plain row or the scroller.
extension MailFilterBar {
    @MainActor @ViewBuilder fileprivate func pillScroller<C: View>(
        @ViewBuilder _ content: () -> C
    ) -> some View {
        #if os(macOS)
            let pills = content()
            Group {
                if pillsWidth <= room { pills } else { fadingScroller(pills) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { room = $0 }
            // The natural width, measured off a copy nobody sees or reaches.
            .background(alignment: .leading) {
                pills
                    .fixedSize()
                    .hidden()
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
                    .onGeometryChange(for: CGFloat.self) { $0.size.width } action: {
                        pillsWidth = $0
                    }
            }
            // The gap the Spacer that used to sit here kept at its least
            // (8, plus the bar's spacing either side of it). A Spacer beside
            // a frame that already fills would split the room with it, and
            // the pills would scroll at half the width they have.
            .padding(.trailing, Space.s8 + Space.s6)
        #else
            fadingScroller(content())
                .contentMargins(.leading, Space.s16, for: .scrollContent)
        #endif
    }
}

@MainActor private func fadingScroller<C: View>(_ content: C) -> some View {
    ScrollView(.horizontal, showsIndicators: false) { content }
        .contentMargins(.trailing, pillFade, for: .scrollContent)
        .mask {
            HStack(spacing: 0) {
                Rectangle()
                LinearGradient(
                    colors: [.black, .clear], startPoint: .leading, endPoint: .trailing
                )
                .frame(width: pillFade)
            }
        }
}

/// How much of the scroller's trailing end dissolves.
private let pillFade: CGFloat = 22

/// WHICH SITREP ZONES THE LIST SHOWS, a menu of every zone with a tick each.
/// Unticking one (Shipments, say) hides its mail from all mail until it is
/// ticked again; the choice is remembered (`Prefs.mailHiddenZones`). It
/// composes with the lens on the left rather than replacing it, so
/// "Important, but no shipping" is two clicks. Mail with no zone yet always
/// shows: there is nothing to hide it by.
private struct CategoryPicker: View {
    @State private var prefs = Prefs.shared

    private var hidden: Set<String> { prefs.mailHiddenZones }

    /// Says WHICH when it is one, so the narrowing is never a mystery.
    private var label: String {
        switch hidden.count {
        case 0: "Categories"
        case 1: "Hiding \(SitrepZone(hidden.first)?.title ?? "1")"
        default: "Hiding \(hidden.count)"
        }
    }

    var body: some View {
        Menu {
            ForEach(SitrepZone.allCases, id: \.self) { zone in
                Toggle(
                    isOn: Binding(
                        get: { !hidden.contains(zone.rawValue) },
                        set: { on in
                            var next = hidden
                            if on { next.remove(zone.rawValue) } else { next.insert(zone.rawValue) }
                            prefs.mailHiddenZones = next
                        })
                ) {
                    Label(zone.title, systemImage: zone.symbol)
                }
            }
            if !hidden.isEmpty {
                Divider()
                Button("Show all categories") { prefs.mailHiddenZones = [] }
            }
        } label: {
            HStack(spacing: Space.scaled(5)) {
                Image(systemName: "line.3.horizontal.decrease")
                    .font(Typo.ui(10, weight: .semibold))
                Text(label)
                    .font(Typo.chip)
                Image(systemName: "chevron.down")
                    .font(Typo.ui(8, weight: .semibold))
            }
            .foregroundStyle(hidden.isEmpty ? Palette.inkDim : Palette.accent)
            .padding(.horizontal, Space.s10)
            .padding(.vertical, Space.s4)
            .background(Capsule().fill(hidden.isEmpty ? Color.clear : Palette.accentSoft))
            .overlay(
                Capsule().strokeBorder(
                    hidden.isEmpty ? Palette.hairline : Palette.accent.opacity(0.45),
                    lineWidth: 1))
            .contentShape(Capsule())
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Hide categories of mail from this list")
        .accessibilityLabel("Categories")
        .accessibilityValue(hidden.isEmpty ? "all shown" : "\(hidden.count) hidden")
    }
}

/// One lens in the filter bar: a capsule whose WHOLE border lights when it is
/// on, never one edge of it.
private struct FilterPill: View {
    let text: String
    var icon: String? = nil
    var count: Int? = nil
    let active: Bool
    let help: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: Space.scaled(5)) {
                if let icon {
                    Image(systemName: active ? "\(icon).fill" : icon)
                        .font(Typo.ui(10))
                }
                // Never wraps: a lens that runs out of room scrolls with
                // the others (`pillScroller`) rather than folding in two.
                Text(text).font(Typo.chip).lineLimit(1).fixedSize()
                if let count {
                    Text("\(count)")
                        .font(Typo.num(10, weight: .semibold))
                        .foregroundStyle(active ? Palette.accent : Palette.inkFaintest)
                }
            }
            .foregroundStyle(active ? Palette.accent : Palette.inkDim)
            .padding(.horizontal, Space.s10)
            .padding(.vertical, Space.s4)
            .background(Capsule().fill(active ? Palette.accentSoft : Color.clear))
            .overlay(
                Capsule().strokeBorder(
                    active ? Palette.accent.opacity(0.45) : Palette.hairline, lineWidth: 1))
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityAddTraits(active ? .isSelected : [])
    }
}

/// A date heading over a run of rows ("Today", "Yesterday", "Monday", …), on
/// the rows' text edge in the app's section-label style.
struct DayHeading: View {
    let title: String
    let first: Bool
    init(_ title: String, first: Bool) {
        self.title = title
        self.first = first
    }

    var body: some View {
        Text(title)
            .font(Typo.sectionLabel)
            .foregroundStyle(Palette.inkFaint)
            .textCase(.uppercase)
            .frame(maxWidth: .infinity, alignment: .leading)
            #if os(macOS)
                // The rows' padding, so the words sit on their text edge.
                .padding(.horizontal, DeskRow.hPadding)
            #else
                .padding(.horizontal, Space.scaled(11))
            #endif
            .padding(.top, SectionLabelRhythm.top(first: first))
            .padding(.bottom, SectionLabelRhythm.bottom)
            #if !os(macOS)
                // A plain List pins section headers; without a ground the
                // rows would scroll visibly under the words.
                .background(Palette.canvas)
            #endif
            .accessibilityAddTraits(.isHeader)
    }
}
