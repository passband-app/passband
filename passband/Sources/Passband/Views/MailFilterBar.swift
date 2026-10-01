// THE ALL-MAIL PAGE'S FILTER BAR and the day headings under it.
//
// Left: the lenses, one at a time — All, Important, Deadlines, Attachments, and
// Reminders. The first four narrow the rows on screen (`AppStore.mailFilter`);
// Reminders swaps the page for the parked list (`AppStore.reminderFilter`).
// They sit in one row as peers because that is what they are to somebody using
// them, and choosing any one turns the others off. Inbox only: noise has no
// important mail by definition, and nothing here means anything on spam.
//
// Finished mail always shows, dimmed and checked: archived is not gone.

import SwiftUI

struct MailFilterBar: View {
    @Environment(AppStore.self) private var store
    /// Rows per lens under the current to-do switch (`MailList.Counts`), so a
    /// chip says what clicking it gets you before you click.
    let counts: [MailFilter: Int]

    private var reminders: Bool { store.reminderFilter }

    var body: some View {
        HStack(spacing: 6) {
            pillScroller {
                HStack(spacing: 6) {
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
                        help: "mail you parked with a reminder (h sets one)"
                    ) {
                        store.mailFilter = .all
                        store.reminderFilter.toggle()
                    }
                }
            }
            Spacer(minLength: 0)
        }
        #if os(macOS)
            .padding(.horizontal, 24)
        #else
            .padding(.horizontal, 16)
        #endif
        .padding(.top, 10)
        .padding(.bottom, 2)
    }
}

/// The pills scroll sideways on a phone, where five of them do not fit; on a
/// Mac they sit in the bar as they are.
@ViewBuilder private func pillScroller<C: View>(@ViewBuilder _ content: () -> C) -> some View {
    #if os(macOS)
        content()
    #else
        ScrollView(.horizontal, showsIndicators: false) { content() }
            .scrollClipDisabled()
    #endif
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
            HStack(spacing: 5) {
                if let icon {
                    Image(systemName: active ? "\(icon).fill" : icon)
                        .font(.system(size: 10))
                }
                Text(text).font(Typo.chip)
                if let count {
                    Text("\(count)")
                        .font(Typo.num(10, weight: .semibold))
                        .foregroundStyle(active ? Palette.accent : Palette.inkFaintest)
                }
            }
            .foregroundStyle(active ? Palette.accent : Palette.inkDim)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
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
            .padding(.horizontal, 11)
            .padding(.top, first ? 4 : 16)
            .padding(.bottom, 5)
            .accessibilityAddTraits(.isHeader)
    }
}
