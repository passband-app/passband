// The composer's group browser: the popover behind the "groups" button beside
// the To line, for addressing an audience you have not remembered the name of.
//
// Autocomplete already handles the case where you HAVE — type "pres" and the
// group is in the menu. This is the other case, and it is why the list shows
// every group with its mode and size rather than being a second search box: you
// are here precisely because you are looking rather than recalling.
//
// Each row states the MODE, because picking is the moment the choice takes
// effect and finding out afterwards is finding out too late. A bcc group and a
// to group produce mail that looks identical in the composer and nothing alike
// in the recipients' inboxes.

import SwiftUI

struct GroupPicker: View {
    let onPick: (SendGroup) -> Void

    @State private var groups: Loadable<[SendGroup]> = .loading

    private var rows: [SendGroup] { groups.value ?? [] }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s2) {
            Text("address a group")
                .font(Typo.sectionLabel)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.uppercase)
                .padding(.horizontal, Space.scaled(9))
                .padding(.top, Space.scaled(7))
                .padding(.bottom, Space.scaled(3))

            if groups.isLoading && rows.isEmpty {
                note("loading…")
            } else if let error = groups.error, rows.isEmpty {
                note(error)
            } else if rows.isEmpty {
                // Where the page IS differs: a rail slot on the Mac, a row
                // on the phone's Account page.
                #if os(macOS)
                    note("no groups yet. make one on the Groups page.")
                #else
                    note("no groups yet. make one under Account, in Groups.")
                #endif
            } else {
                ScrollView {
                    VStack(spacing: 1) {
                        ForEach(rows) { group in
                            row(group)
                        }
                    }
                    .padding(.horizontal, Space.s4)
                    .padding(.bottom, Space.s4)
                }
                .frame(maxHeight: Space.scaled(260))
            }
        }
        .frame(width: Typo.measure(320))
        .task { await $groups.load("groups failed") { try await APIClient.shared.listGroups() } }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaintest)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, Space.scaled(9))
            .padding(.bottom, Space.scaled(9))
    }

    private func row(_ group: SendGroup) -> some View {
        Button { onPick(group) } label: {
            HStack(spacing: Space.s8) {
                Image(systemName: group.mode.symbol)
                    .font(Typo.ui(10, weight: .semibold))
                    .foregroundStyle(group.mode.tone)
                    .frame(width: Space.scaled(14))
                VStack(alignment: .leading, spacing: 1) {
                    Text(group.name)
                        .font(Typo.row)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                    Text(group.mode.blurb)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                }
                Spacer(minLength: Space.s8)
                Text("\(group.member_count)")
                    .font(Typo.num(11))
                    .foregroundStyle(Palette.inkFaintest)
            }
            .padding(.horizontal, Space.s8)
            .padding(.vertical, Space.s6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(
            RoundedRectangle(cornerRadius: Radius.row, style: .continuous).fill(Palette.accentSoft.opacity(0))
        )
        .hoverHighlight()
    }
}

extension View {
    /// A hover wash for a plain button in a menu-shaped list. The popover's rows
    /// have no selection of their own — the pointer IS the selection — so this is
    /// the only feedback they get.
    fileprivate func hoverHighlight() -> some View {
        modifier(HoverHighlight())
    }
}

private struct HoverHighlight: ViewModifier {
    @State private var hovering = false

    func body(content: Content) -> some View {
        content
            .background(
                RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                    .fill(hovering ? Palette.accentSoft : .clear)
            )
            .onHover { hovering = $0 }
    }
}
