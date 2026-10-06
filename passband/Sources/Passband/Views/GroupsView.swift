// Send groups from GET /client/groups: named audiences you address as one.
// Two columns — the groups on the left, the selected one's detail on the right:
// its members, and below them the mail that has already gone to it.
//
// THE HISTORY IS THE POINT OF THE RIGHT COLUMN. A group is made for people you
// have been emailing for a year, so the daemon answers with two sources unioned
// — sends made THROUGH the group, and mail derived by matching stored recipients
// against the membership — and this page does not distinguish them beyond a
// mode chip, because to the reader they are one question: what have I sent these
// people.
//
// Keys: j/k select · n new · e/Enter edit · x delete.
//
// The rail slot is ⌘6, appended below Audit rather than inserted, so no digit
// anybody already has in their fingers moved.
//
// ON THE PHONE there is no beside, so the two columns are two screens: the
// list hangs off the Account page (PassbandiOS/Views/AccountPage.swift) and a
// tap pushes the detail. The detail is the Mac's right column, the same three
// functions drawn into a ScrollView, on a second GroupsView told which group
// it is about. See `phone` and `phoneDetail` below.

import SwiftUI

struct GroupsView: View {
    @Environment(AppStore.self) private var store

    @State private var groupsState: Loadable<[SendGroup]> = .loading
    @State private var index = 0

    /// The selected group's detail — membership plus history — keyed by id so a
    /// stale response for a group the user has already moved off is dropped
    /// rather than painted over the new one.
    @State private var detail: Loadable<SendGroup> = .idle
    @State private var history: Loadable<[GroupHistoryEntry]> = .idle
    @State private var detailFor: Int?
    #if !os(macOS)
        /// PHONE ONLY: the one group this instance is the detail screen of, or
        /// nil for the list. The pushed screen is a second GroupsView rather
        /// than a closure over the first one's state, and that is not style:
        /// a destination built from the list's `detail` and `history` is drawn
        /// from whatever they held when the list's body last ran, and the list
        /// has no reason to run its body again while it is covered. An instance
        /// of its own reads its own state in its own body.
        var focus: SendGroup?
        /// The row whose detail is pushed. The whole row and not an index: a
        /// position would be pointing at somebody else's group the moment a
        /// reload re-sorted the list.
        @State private var opened: SendGroup?
        @Environment(\.dismiss) private var dismiss
    #endif

    private var groups: [SendGroup] { groupsState.value ?? [] }
    private var selected: SendGroup? { groups[safe: index] }

    var body: some View {
        #if os(macOS)
            desk
        #else
            if let focus { phoneDetail(focus) } else { phone }
        #endif
    }

    /// The Mac's page: two columns, a cursor in the left one.
    private var desk: some View {
        // A VStack, NOT a Group. `Group` applies a modifier to EACH of its
        // children rather than to the group as a whole, and the populated
        // branch below has two: the scrolling body and the bar under it. The
        // `maxHeight: .infinity` on the frame was therefore claimed TWICE, so
        // the enclosing VStack in RoutedHost split the window between them and
        // the page rendered at half height with its footer stranded in the
        // middle. One real container means one frame, and the bar keeps its
        // natural height while the body takes what is left.
        VStack(spacing: 0) {
            if groupsState.isLoading && groups.isEmpty {
                BandNote("loading groups…")
            } else if let error = groupsState.error, groups.isEmpty {
                BandNote(error)
            } else if groups.isEmpty {
                empty
            } else {
                HStack(spacing: 0) {
                    groupList
                    Divider().overlay(Palette.hairline)
                    detailColumn
                }
                footer
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .keyBindings(.modal, bindings)
        .task {
            GroupsReload.shared.handler = { await load() }
            await load()
        }
        .onDisappear { GroupsReload.shared.handler = nil }
        .onChange(of: groups.count) { _, count in
            index = max(0, min(index, max(0, count - 1)))
        }
        // Selection drives the right column. Keyed on the ID rather than the
        // index: a delete that shifts everything up must re-fetch, and a reload
        // that leaves the same group selected must not.
        .task(id: selected?.id) { await loadDetail(for: selected) }
    }

    private var empty: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            HStack(spacing: Space.s4) {
                Text("no groups yet. press").font(Typo.rowSub)
                Kbd("n")
                Text("to make one.").font(Typo.rowSub)
            }
            Text(
                "a group is a set of people you write to together, like your investors or your design partners. name it once and address it by name."
            )
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaintest)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: Typo.measure(420), alignment: .leading)
        }
        .foregroundStyle(Palette.inkFaintest)
        .padding(.horizontal, Space.scaled(22))
        .padding(.vertical, Space.s24)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - left column

    private var groupList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 1) {
                    ForEach(Array(groups.enumerated()), id: \.element.id) { i, group in
                        GroupRow(
                            group: group, selected: i == index,
                            onSelect: { index = i },
                            onEdit: { edit(group) }
                        )
                        .id(group.id)
                    }
                }
                .padding(.horizontal, Space.s10)
                .padding(.vertical, Space.s10)

                // A mailbox with one group leaves this column almost entirely
                // empty, and an empty column reads as something failing to load
                // rather than as a list with one thing in it. One quiet line
                // under the list says which it is.
                if groups.count < 3 {
                    HStack(spacing: Space.s4) {
                        Text("press").font(Typo.micro)
                        Kbd("n")
                        Text("for another").font(Typo.micro)
                    }
                    .foregroundStyle(Palette.inkFaintest)
                    .padding(.horizontal, Space.s16)
                    .padding(.top, Space.s2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .onChange(of: index) { _, i in
                guard let group = groups[safe: i] else { return }
                withAnimation(Motion.scrollFollow) { proxy.scrollTo(group.id, anchor: .center) }
            }
        }
        .frame(width: Typo.measure(250))
    }

    // MARK: - right column

    @ViewBuilder
    private var detailColumn: some View {
        if let group = selected {
            ScrollView {
                VStack(alignment: .leading, spacing: Space.scaled(18)) {
                    detailHeader(group)
                    membersSection(group)
                    historySection
                }
                .padding(.horizontal, Space.s20)
                .padding(.vertical, Space.s14)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        } else {
            Color.clear.frame(maxWidth: .infinity)
        }
    }

    private func detailHeader(_ group: SendGroup) -> some View {
        VStack(alignment: .leading, spacing: Space.s6) {
            HStack(spacing: Space.s8) {
                Text(group.name)
                    .font(Typo.sectionLabel)
                    .foregroundStyle(Palette.ink)
                Chip(text: group.mode.label, tone: group.mode.tone, filled: true)
                Spacer()
                Button("edit") {
                    #if os(macOS)
                        edit(group)
                    #else
                        // With its people: see `withPeople`.
                        Task { await phoneEdit(group) }
                    #endif
                }
                    .buttonStyle(.glass)
                    .font(Typo.micro)
            }
            // What this group DOES when you address it, in one line. The mode is
            // the single most consequential thing about a group and the least
            // visible in the mail it produces, so it is spelled out rather than
            // left to a three-letter chip.
            Text(group.mode.blurb)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
            if !group.note.isEmpty {
                Text(group.note)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    @ViewBuilder
    private func membersSection(_ group: SendGroup) -> some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            SectionHead("\(group.member_count) \(group.member_count == 1 ? "person" : "people")")
            if let members = detail.value?.members, !members.isEmpty {
                FlowLine(spacing: Space.scaled(5)) {
                    ForEach(members) { member in
                        MemberChip(member: member)
                    }
                }
            } else if detail.isLoading {
                Text("loading…").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
            } else if let error = detail.error {
                Text(error).font(Typo.micro).foregroundStyle(Palette.danger)
            }
        }
    }

    @ViewBuilder
    private var historySection: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            SectionHead("sent to this group")
            // TWO SOURCES, ONE QUESTION. The list unions sends made THROUGH the
            // group with mail matched to its membership afterwards, and the
            // reader wants both under "what have I sent these people". Saying
            // which is which per row cost a repeated count that was structurally
            // always true — an "individually" group reaches exactly one person
            // per message, so every row read "1 of 2" forever. It is one fact
            // about the list, so the list says it once.
            if (history.value ?? []).contains(where: { !$0.isRecorded }) {
                Text("includes mail you already had with these people")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
            }
            if history.isLoading && history.value == nil {
                Text("loading…").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
            } else if let error = history.error, history.value == nil {
                Text(error).font(Typo.micro).foregroundStyle(Palette.danger)
            } else if (history.value ?? []).isEmpty {
                Text("nothing yet. mail you send this group shows up here, and so does anything you already sent these people.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: Typo.measure(420), alignment: .leading)
            } else {
                VStack(spacing: 1) {
                    ForEach(history.value ?? []) { entry in
                        GroupHistoryRow(entry: entry) { open(entry) }
                    }
                }
            }
        }
    }

    // MARK: - chrome

    private var footer: some View {
        KeyHintBar(hints: [
            KeyHint(["j", "k"], "select"),
            KeyHint("n", "new"),
            KeyHint(["e", "↵"], "edit"),
            KeyHint("x", "delete"),
        ])
    }

    private var bindings: [KeyBinding] {
        [
            KeyBinding("j", "next") { step(+1) },
            KeyBinding("k", "prev") { step(-1) },
            KeyBinding("ArrowDown", "next") { step(+1) },
            KeyBinding("ArrowUp", "prev") { step(-1) },
            KeyBinding("n", "new group") { create() },
            KeyBinding("e", "edit group") { if let g = selected { edit(g) } },
            KeyBinding("Enter", "edit group") { if let g = selected { edit(g) } },
            KeyBinding("x", "delete group") {
                if let g = selected { Task { await delete(g) } }
            },
        ]
    }

    // MARK: - actions

    /// Clamped to the groups, and never below 0: an empty list used to park
    /// the cursor at -1.
    private func step(_ delta: Int) {
        index = max(0, min(groups.count - 1, index + delta))
    }

    private func create() {
        store.openGroupEditor(GroupEditorRequest(group: nil, onSaved: { Task { await load() } }))
    }

    private func edit(_ group: SendGroup) {
        // The LIST's row carries counts and no people, and the editor's save
        // replaces the membership with what its field holds: opened on the row,
        // a rename would have to retype everyone or lose them. The detail
        // column is showing the same group with its people, so hand that over.
        let full = detail.value.flatMap { $0.id == group.id && $0.members != nil ? $0 : nil }
        store.openGroupEditor(
            GroupEditorRequest(group: full ?? group, onSaved: { Task { await load() } }))
    }

    /// Delete, undo-first, like every other destructive verb in this app.
    ///
    /// The undo recreates the group from the membership the DETAIL column is
    /// already holding, which is why it refuses when that has not landed: the
    /// list read carries counts only, so an undo built from it would restore an
    /// empty group wearing the old name. Better to decline for the second it
    /// takes to load than to offer an undo that quietly loses everyone.
    ///
    /// The recreated group is a NEW row, so any recorded sends the old one had
    /// no longer point at it. Its derived history — mail matched against the
    /// membership — comes back untouched, which is most of what the page shows.
    private func delete(_ group: SendGroup) async {
        guard let full = detail.value, full.id == group.id, let members = full.members else {
            store.pushToast("still loading this group; try again in a second", .info)
            return
        }
        await remove(group, full: full, members: members)
    }

    /// The delete itself, once the membership the undo needs is in hand. Split
    /// from the guard above because the two platforms come by that membership
    /// differently: the Mac is already showing it in the right column, and the
    /// phone's list has to go and ask (`withPeople`).
    ///
    /// Answers whether the group went, which only the phone's detail screen
    /// asks: it has to leave when its subject does.
    @discardableResult
    private func remove(_ group: SendGroup, full: SendGroup, members: [GroupMember]) async
        -> Bool
    {
        do {
            try await APIClient.shared.deleteGroup(group.id)
            Analytics.capture("group_deleted", ["mode": group.mode.rawValue])
            groupsState.value?.removeAll { $0.id == group.id }
            let body = GroupBody(
                name: full.name, mode: full.mode, note: full.note, members: members)
            store.pushUndo(
                kind: .groupDelete, messageId: group.id, label: "deleted \(group.name)"
            ) {
                try await APIClient.shared.createGroup(body)
                await GroupsReload.shared.reload()
            }
            return true
        } catch {
            store.pushToast(errText(error, "delete failed"), .error)
            return false
        }
    }

    /// Open the mail this entry names. A recorded fan-out points at the first
    /// recipient's copy; an entry whose echo never landed points at nothing and
    /// is inert rather than dead-ending in an empty reader.
    private func open(_ entry: GroupHistoryEntry) {
        guard let threadId = entry.thread_id else { return }
        store.openThread(threadId, from: .groups)
    }

    private func load() async {
        await $groupsState.load("groups failed") { try await APIClient.shared.listGroups() }
    }

    /// The right column, both halves, for whichever group is selected.
    ///
    /// `detailFor` is the staleness guard: `.task(id:)` cancels on a change, but
    /// the two awaits below can still land after the user has moved on, and
    /// painting a previous group's members under the current group's name is the
    /// one wrong thing this page could do.
    private func loadDetail(for group: SendGroup?) async {
        guard let group else {
            detail = .idle
            history = .idle
            detailFor = nil
            return
        }
        if detailFor != group.id {
            detail = .loading
            history = .loading
            detailFor = group.id
        }
        await $detail.load("members failed") { try await APIClient.shared.group(group.id) }
        guard detailFor == group.id else { return }
        await $history.load("history failed") {
            try await APIClient.shared.groupHistory(group.id).items
        }
    }
}

#if !os(macOS)
    // MARK: - the phone's pages

    extension GroupsView {
        /// THE LEFT COLUMN, AS A SCREEN. Every gesture ends in a function the
        /// Mac's keys already call:
        ///
        ///   tap a row            j/k onto it   pushes the right column
        ///   swipe left: edit     e / Enter     `openGroupEditor`
        ///   swipe left: delete   x             `remove` (undo in the toast)
        ///   + in the bar         n             `create`
        ///   pull down                          `load`
        ///
        /// A `List` for the reason the Rules page is one: the swipe is the
        /// system's, and so is its truce with the back gesture.
        fileprivate var phone: some View {
            List {
                if groupsState.isLoading && groups.isEmpty {
                    BandNote("loading groups…").plainRow()
                } else if let error = groupsState.error, groups.isEmpty {
                    BandNote(error).plainRow()
                } else if groups.isEmpty {
                    phoneEmpty.plainRow()
                } else {
                    ForEach(groups) { group in
                        PhoneGroupRow(group: group) { opened = group }
                            .plainRow()
                            // Delete takes the full swipe: it is the one with
                            // an undo behind it. Edit opens a card, and a card
                            // nobody asked for is the worse surprise.
                            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                Button {
                                    Haptics.commit()
                                    Task { await phoneDelete(group) }
                                } label: {
                                    Label("Delete", systemImage: "trash")
                                }
                                .tint(Palette.danger)
                                Button {
                                    Task { await phoneEdit(group) }
                                } label: {
                                    Label("Edit", systemImage: "pencil")
                                }
                                .tint(Palette.accent)
                            }
                            .contextMenu { phoneMenu(group) }
                    }
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.defaultMinListRowHeight, 0)
            .background(Palette.canvas)
            .navigationTitle("Groups")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(action: create) {
                        Image(systemName: "plus").foregroundStyle(Palette.accent)
                    }
                    .accessibilityLabel("New group")
                }
                .sharedBackgroundVisibility(.hidden)
            }
            .refreshable { await load() }
            // Runs again on the way back from the detail, which is the reload
            // an edit or a delete made there is owed.
            .task {
                GroupsReload.shared.handler = { await load() }
                await load()
            }
            .onDisappear { GroupsReload.shared.handler = nil }
            .navigationDestination(item: $opened) { group in GroupsView(focus: group) }
            #if DEBUG
                // A screenshot's way into a group; see FixtureRoute.
                .onChange(of: FixtureRoute.shared.group, initial: true) { _, staged in
                    if let row = groups.first(where: { $0.id == staged }) { opened = row }
                }
            #endif
        }

        /// THE RIGHT COLUMN, AS A PUSH: the same header, people and history
        /// the Mac draws beside the list, by the same three functions.
        ///
        /// `focus` is the list's row, which is what there is to draw before
        /// the single-group read lands; after it, and after every edit, the
        /// header reads what that came back with, so a rename made from here
        /// shows here.
        fileprivate func phoneDetail(_ focus: SendGroup) -> some View {
            let group = detail.value.flatMap { $0.id == focus.id ? $0 : nil } ?? focus
            return ScrollView {
                VStack(alignment: .leading, spacing: Space.scaled(18)) {
                    detailHeader(group)
                    membersSection(group)
                    historySection
                }
                .padding(.horizontal, 16)
                .padding(.top, Space.s8)
                .padding(.bottom, 28)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .background(Palette.canvas)
            // The name is the first thing under the bar already.
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        phoneMenu(group)
                    } label: {
                        Image(systemName: "ellipsis").foregroundStyle(Palette.accent)
                    }
                    .accessibilityLabel("Group actions")
                }
                .sharedBackgroundVisibility(.hidden)
            }
            .refreshable { await loadDetail(for: focus) }
            .task { await loadDetail(for: focus) }
            // A history row opens mail, and done should come back here.
            .readerHost()
        }

        /// The long press on a row and the bar menu on the detail: one list of
        /// verbs, so the two cannot drift.
        @ViewBuilder private func phoneMenu(_ group: SendGroup) -> some View {
            if focus == nil {
                Button { opened = group } label: { Label("Open", systemImage: "person.2") }
            }
            Button { Task { await phoneEdit(group) } } label: {
                Label("Edit", systemImage: "pencil")
            }
            // Destructive ROLE on an undoable act: the role is what puts it
            // last and in red.
            Button(role: .destructive) { Task { await phoneDelete(group) } } label: {
                Label("Delete", systemImage: "trash")
            }
        }

        /// The group WITH its people. The list read carries counts only, by the
        /// daemon's design, and both verbs below are wrong without the
        /// membership: an editor opened on a list row shows an empty people
        /// field and would save it, and an undo built from one restores an
        /// empty group wearing the old name. So each asks first, unless this
        /// is the detail screen and already holding them.
        private func withPeople(_ group: SendGroup) async -> SendGroup? {
            if let held = detail.value, held.id == group.id, held.members != nil { return held }
            do {
                return try await APIClient.shared.group(group.id)
            } catch {
                store.pushToast(errText(error, "could not load this group"), .error)
                return nil
            }
        }

        /// `e`, on the full group. A save re-pulls whichever screen asked:
        /// the list its rows, the detail its people. (The list under an open
        /// detail re-pulls on the way back; see its `.task`.)
        private func phoneEdit(_ group: SendGroup) async {
            guard let full = await withPeople(group) else { return }
            let onDetail = focus
            store.openGroupEditor(
                GroupEditorRequest(
                    group: full,
                    onSaved: {
                        Task {
                            if let onDetail { await loadDetail(for: onDetail) } else { await load() }
                        }
                    }))
        }

        /// `x`. Same delete, same undo; the membership the Mac reads off its
        /// right column is fetched here instead of refused for.
        private func phoneDelete(_ group: SendGroup) async {
            guard let full = await withPeople(group), let members = full.members else { return }
            let gone = await remove(group, full: full, members: members)
            // A detail whose group is gone has nothing left to show.
            if gone, focus != nil { dismiss() }
        }

        private var phoneEmpty: some View {
            VStack(alignment: .leading, spacing: Space.s10) {
                Text("no groups yet.")
                    .font(Typo.row)
                    .foregroundStyle(Palette.inkDim)
                Text(
                    "a group is a set of people you write to together, like your investors or your design partners. name it once and address it by name."
                )
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkFaintest)
                .fixedSize(horizontal: false, vertical: true)
                Button("new group", action: create)
                    .buttonStyle(.glass)
                    .padding(.top, Space.s2)
            }
            .padding(.horizontal, Space.scaled(11))
            .padding(.vertical, Space.s24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// A group in the phone's list. The Mac's row can be terse because the
    /// column beside it spells the group out; here the row is all there is
    /// before a tap, so it says the one consequential thing itself: HOW mail
    /// to these people goes out.
    private struct PhoneGroupRow: View {
        let group: SendGroup
        let onOpen: () -> Void

        private var people: String {
            "\(group.member_count) \(group.member_count == 1 ? "person" : "people")"
        }

        var body: some View {
            ListRow(
                selected: false, cornerRadius: 10, hPadding: Space.scaled(11), vPadding: Space.s10,
                hoverFill: false, action: onOpen
            ) { _, _ in
                HStack(spacing: Space.s12) {
                    Image(systemName: group.mode.symbol)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(group.mode.tone)
                        .frame(width: 22)
                    VStack(alignment: .leading, spacing: Space.scaled(3)) {
                        Text(group.name)
                            .font(Typo.row)
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                        Text("\(people) · \(group.mode.blurb)")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                    }
                    Spacer(minLength: Space.s8)
                    if let last = group.last_sent_at {
                        // "sent", because a bare age beside a group reads
                        // as how old the group is.
                        Text("sent \(Fmt.relAge(last))")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                    }
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Palette.inkFaintest)
                }
            }
        }
    }
#endif

/// Lets a save fired from the editor — which outlives this view's closures —
/// ask the list to re-pull if it is still on screen. Mirrors `RulesReload`.
@MainActor
final class GroupsReload {
    static let shared = GroupsReload()
    var handler: (@MainActor () async -> Void)?
    private init() {}
    func reload() async { await handler?() }
}

/// Chip colour per mode, kept off the wire type so the model layer stays free of
/// SwiftUI. Bcc is the WARN tone on purpose: it is the mode whose audience is
/// invisible in the mail itself, and the one whose consequences are hardest to
/// see after the fact.
extension GroupMode {
    var tone: Color {
        switch self {
        case .to: Palette.inkFaint
        case .bcc: Palette.warn
        case .individual: Palette.accent
        }
    }
}

// MARK: - rows

private struct GroupRow: View {
    let group: SendGroup
    let selected: Bool
    let onSelect: () -> Void
    let onEdit: () -> Void

    var body: some View {
        ListRow(selected: selected, cornerRadius: 8, hPadding: Space.s10, vPadding: Space.scaled(7), action: onSelect) {
            _, _ in
            VStack(alignment: .leading, spacing: Space.scaled(3)) {
                HStack(spacing: Space.s6) {
                    Image(systemName: group.mode.symbol)
                        .font(Typo.ui(9, weight: .semibold))
                        .foregroundStyle(group.mode.tone)
                        .frame(width: Space.s12)
                    Text(group.name)
                        .font(Typo.row)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                    Spacer(minLength: Space.s6)
                    Text("\(group.member_count)")
                        .font(Typo.num(10))
                        .foregroundStyle(Palette.inkFaintest)
                }
                if let last = group.last_sent_at {
                    Text("last sent \(Fmt.relAge(last))")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                }
            }
        }
        .onTapGesture(count: 2, perform: onEdit)
    }
}

private struct MemberChip: View {
    let member: GroupMember

    var body: some View {
        HStack(spacing: Space.scaled(5)) {
            Avatar(sender: member.addr, size: 16)
            // Contact strings are email-derived: rendered as Text only, never as
            // markup.
            Text(member.label)
                .font(Typo.mono(10))
                .foregroundStyle(Palette.inkDim)
                .lineLimit(1)
        }
        .padding(.horizontal, Space.scaled(7))
        .padding(.vertical, Space.scaled(3))
        .background(
            Capsule(style: .continuous).fill(Palette.accentSoft.opacity(0.5))
        )
        .help(member.addr)
    }
}

private struct GroupHistoryRow: View {
    let entry: GroupHistoryEntry
    let onOpen: () -> Void

    /// "12 of 12" reads as noise on every row that reached everyone, which is
    /// most of them. The count earns its place only when it is a shortfall —
    /// AND ONLY A RECORDED SEND CAN HAVE ONE.
    ///
    /// A derived entry never aimed at the group: it is ordinary mail the daemon
    /// matched to this membership afterwards, so "1 of 2" describes nothing that
    /// went wrong. Showing it here put a shortfall on every row of a year's
    /// correspondence and made a working mailbox look like a page of failed
    /// deliveries. Derived rows say what they are instead, once, in `origin`.
    private var reach: String? {
        guard entry.isRecorded else { return nil }
        if entry.failed > 0 { return "\(entry.reached) of \(entry.group_size)" }
        if entry.reachedEveryone { return nil }
        return "\(entry.reached) of \(entry.group_size)"
    }


    var body: some View {
        ListRow(selected: false, cornerRadius: 8, hPadding: Space.s10, vPadding: Space.scaled(7), action: onOpen) {
            _, _ in
            HStack(spacing: Space.s10) {
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(entry.subject.isEmpty ? "(no subject)" : entry.subject)
                        .font(Typo.row)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                        .help(
                            entry.isRecorded
                                ? "sent through this group"
                                : "mail you already had with "
                                    + "\(entry.reached) of the \(entry.group_size) people in "
                                    + "this group, not sent through it")
                    if !entry.snippet.isEmpty {
                        Text(entry.snippet)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: Space.s8)
                // A PARTLY DELIVERED FAN-OUT gets the loudest thing on the row.
                // Eleven investors got the update and one did not, and the only
                // outcome worse than that is not being told.
                if entry.failed > 0 {
                    Chip(text: "\(entry.failed) failed", tone: Palette.danger, filled: true)
                }
                if let reach {
                    Text(reach)
                        .font(Typo.num(10))
                        .foregroundStyle(entry.failed > 0 ? Palette.danger : Palette.inkFaint)
                        .help(
                            entry.failed > 0
                                ? "the fan-out did not reach everyone"
                                : "sent through this group, before its membership changed")
                }

                if entry.opens > 0 {
                    HStack(spacing: Space.scaled(3)) {
                        Image(systemName: "eye").font(Typo.ui(8))
                        Text("\(entry.opens)").font(Typo.num(10))
                    }
                    .foregroundStyle(Palette.inkFaintest)
                }
                Text(Fmt.relAge(entry.sent_at))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(width: Typo.measure(56), alignment: .trailing)
            }
        }
    }
}

/// A small uppercase section label, the same one the detail column uses twice.
private struct SectionHead: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(Typo.sectionLabel)
            .foregroundStyle(Palette.inkFaint)
            .textCase(.uppercase)
    }
}
