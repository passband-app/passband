// The composer's "to" field: committed recipients render as PILLS in the send
// line, with a live text fragment after them. A pill is minted by accepting a
// suggestion, typing a comma, or typing a space after a complete address —
// space WITHOUT an "@" stays literal, because "alice j" is a display-name
// search, not an address. Backspace on an empty fragment is two-stage: first
// press highlights the last pill, second deletes it. Clicking a pill selects
// it the same way.
//
// MOVING SOMEBODY BETWEEN FIELDS HAS THREE DOORS, wherever this field is one of
// THREE (see `RecipientFields`). It is the thing people do halfway through
// addressing a message, and a composer that can only add and delete makes them
// retype an address they already got right:
//
// - DRAG THE PILL onto another field. The direct one: the thing on screen is
//   the person, and you put them where they go. The pill itself travels — the
//   one in the row hides while it is in the air — and the WELL is the target,
//   not the pills in it, because an empty Bcc has nothing to aim at and that
//   is exactly the field people drag to; every field unfolds for the duration
//   so it is there to aim at. On the Mac both ends of the drag are AppKit
//   views (`PillDragHandle`, `PillDropWell`), for reasons each one explains.
// - Right-click it (long-press on a phone) for the verbs in a menu.
// - Click it and press Backspace to remove it: a click selects the pill, the
//   way the second-stage backspace does.
//
// There used to be a MOVE BAR under a selected pill offering the same verbs as
// the menu. It went when the drag arrived: three doors to one act is findable,
// a row of verbs under every click is furniture.
//
// Every door ends with the address in exactly one header. The menu goes
// through `Recipients.move`; a drop writes this field's list back through its
// binding, and the owner's `Recipients.set` takes the newcomer out of the
// other two. An address left in To while the sender believes it went out blind
// is the one outcome worth writing a whole type to prevent.
//
// A drop is plain text, and `Recipients.dropped` decides what in it is an
// address; the rest is refused rather than pilled. On the phone that means text
// dragged out of a message lands as a recipient too. On the Mac only a pill
// lands, for now: see `RecipientDropWell` for the two ways text does not.
//
// The surfaces that hold ONE list — the share sheet, a group's membership — pass
// no `moves`: no menu, no drag handle, because there is nowhere to move anybody
// to. They carry a well all the same, for whatever reaches them.
//
// Autocomplete rides the fragment: every keystroke asks the daemon for
// Sent-derived contacts — people the user has actually written to — and the
// suggestion list opens under the field.
//
// STATE CONTRACT: `text` (ComposeState.to, the wire string) stays the single
// source of truth — pills and fragment are a PARSE of it, and every mutation
// writes back through the same binding, so DraftSaver's autosave hook and the
// send ceremony never learn pills exist. An external write (a draft restore)
// re-parses.
//
// The keymaps register into the HOST'S context, read from the environment
// (`\.keyContext`): "modal" in the pane composer, "thread" in the reader's
// inline reply. Naming "modal" here was the bug that made a reply's pills
// undeletable: the reader gates every modal binding, so Backspace, and the
// whole suggestion set with it, registered where no key would ever look. The
// suggestion set (arrows / Enter / Tab / Esc) mounts ONLY while a list is
// showing — within a context the latest-registered set wins, so an open list
// eats Enter before the pane's Enter-to-review sees it. The always-mounted set
// carries only Backspace, declining whenever there is fragment text to edit
// normally.

import SwiftUI

/// What a field can do with one of its pills besides delete it, handed in by
/// whoever owns all three lists.
///
/// A bundle rather than three loose closures because they are one capability:
/// a field either sits among siblings — and can pass somebody to them, and must
/// know who is already addressed anywhere — or it stands alone and can do none
/// of it. The WRITES live with the owner (`RecipientFields`) on purpose, since
/// only it holds the value `Recipients.move` has to operate on.
struct RecipientMoves {
    /// Which header this field is, so the menu offers the other two.
    let slot: RecipientSlot
    let move: (String, RecipientSlot) -> Void
    let remove: (String) -> Void
    /// Is this address already on the message ANYWHERE? Autocomplete asks, so it
    /// never re-offers somebody who is currently blind-copied.
    let addressed: (String) -> Bool
    /// A pill has been picked up (`true`) or let go (`false`). The owner
    /// unfolds every field for the duration, so the drag has somewhere to land,
    /// and folds the empty ones back when it ends.
    var dragging: (Bool) -> Void = { _ in }
}

struct RecipientField<F: Hashable>: View {
    @Binding var text: String
    var focus: FocusState<F?>.Binding
    let field: F
    /// The caption over the well. Defaults to the composer's "to"; the group
    /// editor reuses this whole field for its membership list and says so.
    var label: String = "to"
    /// Placeholder for the empty field, for the same reason.
    var placeholder: String = "recipient@example.com"
    /// Offer SEND GROUPS alongside contacts as you type. Opt-in: the group
    /// editor reuses this field for its own membership list, and a group that
    /// could contain a group is not a thing this feature means.
    var suggestGroups: Bool = false
    /// Called when a group is accepted from the menu. The composer owns what
    /// happens next, because the answer depends on the group's mode — expand
    /// into this field, expand into the bcc field, or mint one pill — and this
    /// field knows about neither the other field nor the mode.
    var onGroupPicked: ((SendGroup) -> Void)?
    /// The group a `#slug` pill in this field stands for, when the composer has
    /// resolved one. Absent for a token it could not resolve, which is what the
    /// pill renders as a problem.
    var resolvedGroup: (name: String, count: Int)?
    /// WHERE A PILL CAN GO from here. Absent on the one-list surfaces, which is
    /// what turns the drag handle and the menu off for them.
    var moves: RecipientMoves?
    /// Something to hang on the right of the LABEL row. The `to` field carries
    /// the cc/bcc toggles there, so they sit on the line that names the field
    /// they unfold rather than floating loose above the stack.
    var accessory: AnyView?
    /// Draw as a header LINE (label in a left column, no well of its own)
    /// rather than a captioned well. The pane composer sets it: its fields sit
    /// together on one card, the way a mail header does, instead of stacking up
    /// as a form.
    var inline = false

    /// The key layer this field was mounted in, which is where its keymaps go.
    @Environment(\.keyContext) private var keyContext

    /// Committed recipients — the pills.
    @State private var pills: [String] = []
    /// The live fragment being typed after the pills.
    @State private var fragment = ""
    /// Backspace stage two's target (and click-to-select).
    @State private var selectedPill: Int?

    @State private var hits: [ContactHit] = []
    /// Group matches, always ABOVE the contacts: there are few of them, picking
    /// one is the more deliberate act, and a group buried under eight addresses
    /// would never be found by the person who made it.
    @State private var groupHits: [SendGroup] = []
    @State private var index = 0
    /// A drag is hovering this field. Lights the surface, so the sender can see
    /// which header is about to take the person on the cursor.
    @State private var dropTargeted = false

    /// The menu as one flat list, which is what the arrow keys walk.
    private var suggestionCount: Int { groupHits.count + hits.count }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s4) {
            if inline {
                // The whole header LINE takes the drop, label included: on a
                // card the line is the field.
                InlineFieldRow(label: label, accessory: accessory) { pillRow }
                    .background(
                        RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
                            .fill(Palette.accentSoft)
                            .opacity(dropTargeted ? 1 : 0)
                    )
                    .animation(.easeOut(duration: 0.12), value: dropTargeted)
                    .modifier(dropWell)
            } else {
                VStack(alignment: .leading, spacing: Space.scaled(5)) {
                    HStack(spacing: Space.s8) {
                        FieldLabel(label)
                        Spacer(minLength: 0)
                        accessory
                    }
                    pillRow
                        .fieldWell()
                        .overlay(
                            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                                .strokeBorder(Palette.accent, lineWidth: dropTargeted ? 1.5 : 0)
                        )
                        .animation(.easeOut(duration: 0.12), value: dropTargeted)
                        .modifier(dropWell)
                }
            }

            if suggestionCount > 0 {
                suggestions
                    .modifier(InlineFieldTuck(active: inline))
            }
        }
        .keyBindings(keyContext, fieldBindings)
        .task(id: searchFragment) { await refresh() }
        .onAppear { parse(text) }
        // External writes only (a draft restore): our own writes round-trip to
        // exactly `composed`, and re-parsing those would fight the caret.
        .onChange(of: text) { _, now in
            if now != composed { parse(now) }
        }
        // A click elsewhere defocuses the field; a stale list or a half-armed
        // backspace must not linger.
        .onChange(of: focus.wrappedValue) { _, now in
            if now != field {
                hits = []
                groupHits = []
                selectedPill = nil
            }
        }
    }

    // MARK: - send line

    private var pillRow: some View {
        FlowLine(spacing: Space.scaled(5)) {
            ForEach(Array(pills.enumerated()), id: \.offset) { i, addr in
                RecipientPill(
                    addr: addr, selected: i == selectedPill,
                    group: GroupToken.isToken(addr) ? resolvedGroup : nil,
                    moves: moves,
                    onTap: {
                        selectedPill = i
                        focus.wrappedValue = field
                    })
            }
            TextField(pills.isEmpty ? placeholder : "", text: $fragment)
                .textFieldStyle(.plain)
                .focused(focus, equals: field)
                // Narrower on a header line: the line shares its width with
                // the groups/cc/bcc accessory, and at 120 the caret dropped to
                // a line of its own beside plenty of room.
                .frame(minWidth: Space.scaled(inline ? 72 : 120))
                // Commit-on-separator runs off onChange, not a custom Binding
                // setter: handing a method reference into Binding(get:set:)
                // trips an IRGen crash in Swift 6.3 (isolation thunk), and this
                // is the idiomatic shape anyway. Re-entrant sets terminate: an
                // unchanged value never re-fires onChange.
                .onChange(of: fragment) { _, raw in fragmentTyped(raw) }
        }
    }

    /// This field as a drop target: lit while a drag hovers, and fed whatever
    /// lands. What takes the drop is platform-specific (see `RecipientDropWell`)
    /// but what happens to it is not: `accept(dropped:)`.
    private var dropWell: RecipientDropWell {
        RecipientDropWell(
            targeted: $dropTargeted,
            accepts: { payload in
                let here = Set(pills.map(Recipients.key))
                return Recipients.dropped(payload).contains { !here.contains(Recipients.key($0)) }
            },
            drop: { accept(dropped: $0) })
    }

    /// Every keystroke and paste lands here. Commas ALWAYS commit the token
    /// before them; a trailing space commits only a token carrying "@".
    private func fragmentTyped(_ raw: String) {
        selectedPill = nil
        var rest = raw
        while let comma = rest.firstIndex(of: ",") {
            commit(String(rest[..<comma]))
            rest = String(rest[rest.index(after: comma)...])
        }
        if rest.hasSuffix(" "), rest.trimmed.contains("@") {
            commit(rest)
            rest = ""
        }
        if rest.drop(while: { $0 == " " }).isEmpty { rest = "" }
        if fragment != rest { fragment = rest }
        sync()
    }

    private func commit(_ token: String) {
        let addr = token.trimmed
        guard !addr.isEmpty, !pills.contains(addr) else { return }
        pills.append(addr)
    }

    /// SOMETHING WAS DROPPED ON THIS FIELD. Usually a pill from one of the other
    /// two; possibly an address dragged out of a message, or out of another app
    /// entirely.
    ///
    /// The newcomers join THIS field's pills and the list is written back the
    /// way typing writes it — `sync`, through the binding — which is what makes
    /// a drop between headers a move: the owner's `Recipients.set` takes anyone
    /// this field now names out of the other two. It also leaves a half-typed
    /// fragment where it was, trailing and still editable; a write that
    /// appended the newcomer AFTER the fragment would have turned "bo" into a
    /// pill.
    ///
    /// A no-op — the drag snaps back — when nothing in the payload is an
    /// address, or when everyone in it is already here: a pill dropped back
    /// where it started keeps its place in the list instead of jumping to the
    /// end.
    private func accept(dropped items: [String]) -> Bool {
        let here = Set(pills.map(Recipients.key))
        let arriving = items.flatMap(Recipients.dropped).filter { !here.contains(Recipients.key($0)) }
        guard !arriving.isEmpty else { return false }
        pills += arriving
        selectedPill = nil
        sync()
        focus.wrappedValue = field
        return true
    }

    /// Write the parse back to the wire string. The fragment rides along raw,
    /// so mid-typing the wire string is what a plain text field would hold.
    private func sync() {
        var parts = pills
        if !fragment.trimmed.isEmpty { parts.append(fragment.trimmed) }
        let next = parts.joined(separator: ", ")
        if text != next { text = next }
    }

    private var composed: String {
        var parts = pills
        if !fragment.trimmed.isEmpty { parts.append(fragment.trimmed) }
        return parts.joined(separator: ", ")
    }

    /// An external value: complete tokens become pills, a trailing partial
    /// (no comma after it) stays editable as the fragment.
    private func parse(_ value: String) {
        var tokens = Recipients.split(value)
        var trailingPartial = ""
        // A COMPLETE ADDRESS IS A PILL HOWEVER IT ARRIVED — moved in from
        // another field, restored from a draft, seeded from the daemon's
        // derivation. Only a trailing token that is not yet an address (no `@`,
        // and a group token is its own kind of complete) stays editable,
        // because that one is somebody mid-keystroke.
        //
        // Testing for the trailing COMMA alone was the bug: nothing but typing
        // puts one there, so every address that arrived any other way landed as
        // raw text in the well and stayed text until the sender pressed space —
        // which made "move to cc" look like it had half-worked.
        if !value.trimmed.hasSuffix(","), let last = tokens.last,
            Recipients.key(last).isEmpty, !GroupToken.isToken(last)
        {
            trailingPartial = last
            tokens.removeLast()
        }
        pills = tokens
        fragment = trailingPartial
        selectedPill = nil
    }

    // MARK: - suggestions

    /// What autocomplete searches: the fragment, while the field has focus.
    private var searchFragment: String? {
        guard focus.wrappedValue == field else { return nil }
        let trimmed = fragment.trimmed
        return trimmed.isEmpty ? nil : trimmed
    }

    private var suggestions: some View {
        VStack(alignment: .leading, spacing: 1) {
            ForEach(Array(groupHits.enumerated()), id: \.element.id) { i, group in
                Button {
                    acceptGroup(group)
                } label: {
                    HStack(spacing: Space.s8) {
                        Image(systemName: group.mode.symbol)
                            .font(Typo.ui(9, weight: .semibold))
                            .foregroundStyle(group.mode.tone)
                            .frame(width: Space.scaled(12))
                        Text(group.name)
                            .font(Typo.ui(11, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                        // The mode is said in the MENU, not only after picking:
                        // it decides whether these addresses are about to become
                        // visible to each other, and finding that out afterwards
                        // is finding out too late.
                        Text(group.mode.blurb)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                        Spacer(minLength: Space.s8)
                        Text("\(group.member_count)")
                            .font(Typo.num(10))
                            .foregroundStyle(Palette.inkFaintest)
                    }
                    .padding(.horizontal, Space.scaled(9))
                    .padding(.vertical, Space.scaled(5))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(
                    RoundedRectangle(cornerRadius: Radius.concentric(Radius.well, inset: Space.s4), style: .continuous)
                        .fill(i == index ? Palette.accentSoft : .clear)
                )
            }
            if !groupHits.isEmpty && !hits.isEmpty {
                Divider().overlay(Palette.hairline).padding(.vertical, Space.s2)
            }
            ForEach(Array(hits.enumerated()), id: \.element.id) { i, hit in
                Button {
                    accept(hit)
                } label: {
                    HStack(spacing: Space.s8) {
                        // Contact strings are email-derived: rendered as Text
                        // only, never as markup.
                        if let name = hit.display_name, !name.isEmpty {
                            Text(name)
                                .font(Typo.ui(11, weight: .medium))
                                .foregroundStyle(Palette.ink)
                                .lineLimit(1)
                        }
                        Text(hit.addr)
                            .font(Typo.mono(11))
                            .foregroundStyle(
                                i + groupHits.count == index ? Palette.ink : Palette.inkDim
                            )
                            .lineLimit(1)
                        Spacer(minLength: Space.s8)
                        Text("\(hit.sent_count)×")
                            .font(Typo.num(10))
                            .foregroundStyle(Palette.inkFaintest)
                    }
                    .padding(.horizontal, Space.scaled(9))
                    .padding(.vertical, Space.scaled(5))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(
                    RoundedRectangle(cornerRadius: Radius.concentric(Radius.well, inset: Space.s4), style: .continuous)
                        .fill(i + groupHits.count == index ? Palette.accentSoft : .clear)
                )
            }
            HStack(spacing: Space.s4) {
                Kbd("↑↓")
                Text("pick ·").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                Kbd("enter")
                Text("accept ·").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                Kbd("esc")
                Text("dismiss").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
            }
            .padding(.horizontal, Space.scaled(9))
            .padding(.top, Space.s4)
        }
        .padding(Space.s4)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(Palette.canvas.opacity(0.85))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75))
        // Mounts WITH the list, so these register after everything else and win
        // exactly while there is a list to drive.
        .keyBindings(keyContext, suggestionBindings)
    }

    // MARK: - keymaps

    /// Always mounted: the two-stage backspace. Declines whenever the fragment
    /// still has text, so ordinary editing never notices it.
    private var fieldBindings: [KeyBinding] {
        [
            KeyBinding(declining: "Backspace", "remove recipient", allowInInput: true) {
                guard focus.wrappedValue == field, fragment.isEmpty, !pills.isEmpty
                else { return false }
                if let selected = selectedPill {
                    let addr = pills[selected]
                    selectedPill = nil
                    // Through the owner when there is one, so a removal and a
                    // move take the same path out of the value.
                    if let moves {
                        moves.remove(addr)
                    } else {
                        pills.remove(at: selected)
                        sync()
                    }
                } else {
                    selectedPill = pills.count - 1
                }
                return true
            }
        ]
    }

    private var suggestionBindings: [KeyBinding] {
        [
            KeyBinding("ArrowDown", "next suggestion", allowInInput: true) {
                index = min(suggestionCount - 1, index + 1)
            },
            KeyBinding("ArrowUp", "prev suggestion", allowInInput: true) {
                index = max(0, index - 1)
            },
            KeyBinding("Enter", "accept suggestion", allowInInput: true) { acceptSelected() },
            KeyBinding("Tab", "accept suggestion", allowInInput: true) { acceptSelected() },
            KeyBinding("Escape", "dismiss suggestions", allowInInput: true) {
                hits = []
                groupHits = []
            },
        ]
    }

    // MARK: - state

    private func refresh() async {
        guard let searchFragment else {
            hits = []
            groupHits = []
            return
        }
        // Debounce: a fresh keystroke cancels this task before the request.
        try? await Task.sleep(for: .milliseconds(120))
        guard !Task.isCancelled else { return }
        // Both lookups in flight together, so adding groups to the menu costs a
        // round trip's latency rather than two.
        async let contacts = APIClient.shared.contacts(searchFragment)
        async let groups: [SendGroup] =
            suggestGroups ? APIClient.shared.searchGroups(searchFragment, limit: 4) : []
        let found = (try? await contacts) ?? []
        let foundGroups = (try? await groups) ?? []
        guard !Task.isCancelled else { return }
        // Anyone already addressed is a done deal, not a suggestion — and where
        // this field has siblings, that means addressed in ANY of the three.
        // Offering somebody who is currently blind-copied would be offering to
        // un-blind them by accident.
        hits = found.filter { hit in
            if let moves { return !moves.addressed(hit.addr) }
            return !pills.contains(hit.addr)
        }
        groupHits = foundGroups
        index = 0
    }

    /// Accept whatever the cursor is on, out of the flat list the arrows walk.
    private func acceptSelected() {
        if let group = groupHits[safe: index] {
            acceptGroup(group)
        } else if let hit = hits[safe: index - groupHits.count] {
            accept(hit)
        }
    }

    private func accept(_ hit: ContactHit) {
        commit(hit.addr)
        fragment = ""
        hits = []
        groupHits = []
        sync()
        // The caret stays in the field: accepting one recipient is usually the
        // middle of addressing, not the end of it.
        focus.wrappedValue = field
    }

    /// Hand the group up and clear the menu. What lands in which field is the
    /// composer's call — it depends on the group's mode, and this field knows
    /// about neither the mode nor the bcc row beside it.
    private func acceptGroup(_ group: SendGroup) {
        fragment = ""
        hits = []
        groupHits = []
        sync()
        onGroupPicked?(group)
        focus.wrappedValue = field
    }
}

// MARK: - pill

private struct RecipientPill: View {
    let addr: String
    let selected: Bool
    /// The group this pill stands for, when it is one. Set only by the composer,
    /// and only for a FAN-OUT group: `to`/`bcc` groups are expanded into ordinary
    /// address pills at pick time, so they never reach here.
    var group: (name: String, count: Int)?
    /// Present only where this pill has somewhere else to go.
    var moves: RecipientMoves?
    let onTap: () -> Void

    /// A group pill whose token no longer resolves. Rendered as a PROBLEM rather
    /// than silently dropped: the daemon refuses to send it, and the sender needs
    /// to know which audience went missing rather than watch a send fail.
    private var unresolved: Bool { group == nil && GroupToken.isToken(addr) }

    @Environment(\.colorScheme) private var colorScheme
    /// This pill is the one on the cursor. The copy in the row hides for the
    /// duration, so what the sender sees is the pill itself moving, not a
    /// ghost of it leaving one behind.
    @State private var inFlight = false

    var body: some View {
        let pill = chrome(
            Button(action: onTap) { face }
                // PRESS, not release: a click selects the pill at once (the
                // second-stage backspace's target), and a press that turns
                // into a drag has already done so.
                .buttonStyle(PressToSelect(onPress: onTap))
        )
        .opacity(inFlight ? 0 : 1)
        .pointingHand()
        .help(unresolved ? "this group could not be found; pick it again" : addr)
        // The verbs, where a right-click (long-press on
        // a phone) goes looking for them. Three doors to one act, because moving
        // a recipient is the kind of thing people reach for in three different
        // ways and it has to be findable in all of them.
        .contextMenu {
            if let moves {
                ForEach(RecipientSlot.allCases.filter { $0 != moves.slot }, id: \.self) { dest in
                    Button("Move to \(dest.label.uppercased())") { moves.move(addr, dest) }
                }
                Divider()
                Button("Remove", role: .destructive) { moves.remove(addr) }
            }
        }

        // THE PILL IS THE PERSON, so you can pick them up and put them in
        // another field. Plain text on the way out, which also makes a pill
        // draggable into anything that takes an address. Only where there is
        // somewhere to take them: the one-list surfaces pass no `moves`.
        // A GROUP pill stays put: `Recipients` has no key for a `#slug`, so no
        // field would take it, and a pill that lifts only to fly home every
        // time is a pill that reads as broken.
        if moves != nil, !GroupToken.isToken(addr) {
            #if os(macOS)
                // Not `.draggable`. The window moves by its background (see
                // `Glass.WindowConfigurator`), and AppKit decides "background"
                // before SwiftUI's drag has seen a single mouseDragged — so
                // `.draggable` on a pill picked up the whole window. An AppKit
                // view that owns the left mouse is the one thing AppKit
                // consults on that question.
                pill.overlay(
                    PillDragHandle(
                        addr: addr, onPress: onTap,
                        onDrag: { active in
                            inFlight = active
                            moves?.dragging(active)
                        }
                    ) {
                        // The pill as it is about to look — the press that
                        // starts a drag has just selected it, so the ring is
                        // on — on an opaque base: its own fill is a tint over
                        // the well, and a tint alone over the page reads as a
                        // ghost.
                        let renderer = ImageRenderer(
                            content: chrome(face, selected: true)
                                .background(Capsule().fill(Palette.canvas))
                                .environment(\.colorScheme, colorScheme))
                        renderer.scale = NSScreen.main?.backingScaleFactor ?? 2
                        return renderer.nsImage
                    })
            #else
                pill.draggable(addr)
            #endif
        } else {
            pill
        }
    }

    /// The pill's content, minus the capsule: what the Button presses and what
    /// the drag carries as its image.
    private var face: some View {
        HStack(spacing: Space.scaled(5)) {
                if group != nil {
                    Image(systemName: "person.2.fill").font(Typo.ui(8, weight: .semibold))
                } else if unresolved {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(Typo.ui(8, weight: .semibold))
                }
                // Email-derived string: Text only, never markup.
                Text(label)
                    .font(group == nil ? Typo.mono(11) : Typo.ui(11, weight: .medium))
                    .lineLimit(1)
                if let group {
                    // The count is the whole point of a fan-out pill: one pill
                    // stands for twelve emails, and the number is the only thing
                    // on screen that says so.
                    Text("· \(group.count) · will send individually")
                        .font(Typo.micro)
                        .opacity(0.75)
                        .lineLimit(1)
                }
            }
            .foregroundStyle(unresolved ? Palette.danger : Palette.ink)
            .padding(.horizontal, Space.s8)
            .padding(.vertical, Space.scaled(3))
            .contentShape(Capsule())
    }

    /// The capsule around a face: the pill's own paint, which `PressToSelect`
    /// deliberately leaves to it.
    private func chrome<Face: View>(_ face: Face, selected: Bool? = nil) -> some View {
        let selected = selected ?? self.selected
        return face
            .background(Capsule().fill(fill(selected: selected)))
            .overlay(
                Capsule().strokeBorder(
                    unresolved
                        ? Palette.danger
                        : (selected ? Palette.accent : Palette.accent.opacity(0.25)),
                    lineWidth: selected || unresolved ? 1.25 : 0.75)
            )
    }

    private var label: String {
        if let group { return group.name }
        if unresolved { return GroupToken.slug(addr) ?? addr }
        return addr
    }

    private func fill(selected: Bool) -> Color {
        if unresolved { return Palette.danger.opacity(0.14) }
        return selected ? Palette.accent.opacity(0.38) : Palette.accentSoft
    }
}

/// A button style that fires on PRESS instead of on release, and draws nothing
/// of its own — the pill paints its own capsule.
///
/// `Button`'s own action waits for mouse-up inside, which is correct for
/// anything that should be abandonable by dragging away. A recipient pill is
/// the opposite case: a press that turns into a drag should already have
/// selected the pill, so the drag image and the row agree on how it looks.
private struct PressToSelect: ButtonStyle {
    let onPress: () -> Void

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            // Only the leading edge: `isPressed` falls back to false on release
            // (or on a drag away), and firing there would toggle the selection
            // straight off again.
            .onChange(of: configuration.isPressed) { _, pressed in
                if pressed { onPress() }
            }
    }
}

#if os(macOS)
    import AppKit

    /// THE PILL'S LEFT MOUSE, IN APPKIT. Laid over the pill, it takes the
    /// mouse-down (selecting the pill, as the Button under it would have), and
    /// once the pointer has moved a few points it hands the address to an
    /// `NSDraggingSession` with a rendering of the pill as the drag image.
    ///
    /// Why an NSView and not `.draggable`: the shell window moves by its
    /// background (`Glass.WindowConfigurator`), and AppKit settles "is this
    /// mouse-down a window drag?" at the moment of the mouse-down by asking the
    /// hit view's `mouseDownCanMoveWindow`. SwiftUI's hosting view says yes over
    /// a shape carrying `.draggable` — a drag source has nothing to claim until
    /// the pointer moves — and by then the window is what is moving. A view of
    /// our own that answers no is the only seat at that table.
    ///
    /// Only the LEFT button is ours. `hitTest` steps aside for everything else
    /// — right-click and control-click, so the SwiftUI `.contextMenu` under us
    /// still gets them; hover, so `.help` and the pointing hand still work —
    /// because the point of this view is one gesture, not to re-implement the
    /// pill.
    ///
    /// It is also the drag's DESTINATION-FINDER. AppKit's own lookup never
    /// reaches a view under SwiftUI's hosting view (traced on a real mouse:
    /// sessions tracked for hundreds of moves over correctly framed wells
    /// registered for the type, in the same window, and not one was entered),
    /// so within the app the source does the destination's job: on every move
    /// it asks `PillDropRegistry` which well holds the cursor and will take
    /// this address, lights it, and on release drops there.
    private struct PillDragHandle: NSViewRepresentable {
        let addr: String
        let onPress: () -> Void
        /// The session starting (`true`) and ending (`false`), however it ends.
        let onDrag: (Bool) -> Void
        /// The drag image: the pill's look, rendered at the press.
        let preview: @MainActor () -> NSImage?

        func makeNSView(context: Context) -> PillHandleView {
            let view = PillHandleView()
            update(view)
            return view
        }

        func updateNSView(_ view: PillHandleView, context: Context) { update(view) }

        private func update(_ view: PillHandleView) {
            view.addr = addr
            view.onPress = onPress
            view.onDrag = onDrag
            view.preview = preview
        }
    }

    final class PillHandleView: NSView, NSDraggingSource {
        var addr = ""
        var onPress: () -> Void = {}
        var onDrag: (Bool) -> Void = { _ in }
        var preview: @MainActor () -> NSImage? = { nil }
        /// Where the mouse went down, while it is still down and has not yet
        /// travelled far enough to be a drag.
        private var pressedAt: NSPoint?
        /// The drag image, rendered at the press rather than at the pick-up:
        /// rendering a SwiftUI view is the slow part of starting a drag, and
        /// the press is a moment the hand is still anyway.
        private var image: NSImage?
        /// The well under the cursor right now, lit.
        private weak var hovered: PillDropWellView?

        /// The whole reason this view exists. See `PillDragHandle`.
        override var mouseDownCanMoveWindow: Bool { false }

        /// A click on a pill in a background window is a click on the pill,
        /// the way it is for every other control in the shell.
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

        /// Claim the mouse, except when it is plainly not ours. AppKit hit-tests
        /// with the event being dispatched as `NSApp.currentEvent`, which is
        /// what lets a view answer differently for a right-click than for a
        /// left one: right-click and control-click go to the SwiftUI context
        /// menu under us, hover to its tooltip and pointing hand. Everything
        /// else — including a hit-test with no current event to read — is
        /// claimed, because claiming is the whole job.
        override func hitTest(_ point: NSPoint) -> NSView? {
            if let event = NSApp.currentEvent {
                switch event.type {
                case .rightMouseDown, .rightMouseUp, .rightMouseDragged,
                    .mouseMoved, .mouseEntered, .mouseExited, .cursorUpdate:
                    return nil
                case .leftMouseDown where event.modifierFlags.contains(.control):
                    return nil
                default:
                    break
                }
            }
            return super.hitTest(point)
        }

        override func mouseDown(with event: NSEvent) {
            pressedAt = event.locationInWindow
            image = preview()
            onPress()
        }

        override func mouseDragged(with event: NSEvent) {
            guard let start = pressedAt else { return }
            let now = event.locationInWindow
            // A few points of slop, so a slightly unsteady click stays a click.
            guard hypot(now.x - start.x, now.y - start.y) >= 4 else { return }
            pressedAt = nil
            // Plain text, the most portable thing an address can be: what
            // another app takes, and what the phone's wells read.
            let item = NSDraggingItem(pasteboardWriter: addr as NSString)
            item.setDraggingFrame(bounds, contents: image ?? preview())
            let session = beginDraggingSession(with: [item], event: event, source: self)
            session.animatesToStartingPositionsOnCancelOrFail = true
            // The session first, the layout second: unfolding the header is a
            // SwiftUI pass, and the pick-up should not wait on it.
            onDrag(true)
        }

        override func mouseUp(with event: NSEvent) { pressedAt = nil }

        private func well(under screenPoint: NSPoint) -> PillDropWellView? {
            guard let window else { return nil }
            return PillDropRegistry.shared.well(
                at: window.convertPoint(fromScreen: screenPoint), in: window, taking: addr)
        }

        func draggingSession(_ session: NSDraggingSession, movedTo screenPoint: NSPoint) {
            let now = well(under: screenPoint)
            if now !== hovered {
                hovered?.onTargeted(false)
                now?.onTargeted(true)
                hovered = now
            }
            // Fly back only from nowhere. Over a well the image is about to
            // become a pill in the row underneath, and a copy flying home
            // first would say the drop failed.
            session.animatesToStartingPositionsOnCancelOrFail = now == nil
        }

        /// Landed or not: the well under the cursor takes the address if it will
        /// have it, the row's copy of the pill comes back (to be removed by the
        /// source field's re-parse a moment later, if it landed), and the owner
        /// folds the empty fields. `operation` is checked so that, should AppKit
        /// ever start delivering these drops itself, the well is not handed the
        /// same address twice.
        func draggingSession(
            _ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation
        ) {
            if operation.isEmpty, let target = well(under: screenPoint) {
                _ = target.drop([addr])
            }
            hovered?.onTargeted(false)
            hovered = nil
            onDrag(false)
        }

        /// Within the app the drop is a MOVE, done by us (above). Into another
        /// app the address is copied; nothing here is removed.
        func draggingSession(
            _ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext
        ) -> NSDragOperation {
            context == .withinApplication ? .move : .copy
        }
    }

    /// Every drop well currently on screen, so a drag source can find the one
    /// under the cursor without AppKit's help (see `PillDragHandle`).
    @MainActor
    final class PillDropRegistry {
        static let shared = PillDropRegistry()
        private var wells: [ObjectIdentifier: Weak] = [:]
        private struct Weak { weak var view: PillDropWellView? }

        func add(_ well: PillDropWellView) {
            // A well that died without leaving a window first never said
            // goodbye; sweep the empties on the way in.
            wells = wells.filter { $0.value.view != nil }
            wells[ObjectIdentifier(well)] = Weak(view: well)
        }
        func remove(_ well: PillDropWellView) { wells[ObjectIdentifier(well)] = nil }

        /// The well in `window` containing `point` (window coordinates) that
        /// would take `payload`. Wells do not overlap, so the first is the one.
        func well(at point: NSPoint, in window: NSWindow, taking payload: String) -> PillDropWellView? {
            wells.values.compactMap(\.view).first { well in
                well.window === window && !well.isHiddenOrHasHiddenAncestor
                    && well.convert(well.bounds, to: nil).contains(point) && well.accepts(payload)
            }
        }
    }
#endif

// MARK: - drop well

/// A field's surface as a drop target. One name across platforms so the field
/// body reads the same; the mechanics differ underneath.
///
/// On the Mac it is a plain AppKit view (`PillDropWell`) BEHIND the surface that
/// knows its own frame and what it would accept, and a drag finds it through
/// `PillDropRegistry` — not an `NSDraggingDestination`, because AppKit's
/// destination lookup never reaches a view under SwiftUI's hosting view, and
/// SwiftUI's own `dropDestination` never took the session's text either (both
/// traced, both dead). Which is also why, on the Mac, only a PILL lands here:
/// text dragged out of a message or another app has no way in yet. Behind
/// rather than over, so a click on a pill or in the caret's space still reaches
/// them. Elsewhere it is SwiftUI's own `dropDestination`, and text lands too.
///
/// `accepts` is asked on every move, not only at the drop: a well that will not
/// take the payload never lights, instead of lighting and then doing nothing.
private struct RecipientDropWell: ViewModifier {
    @Binding var targeted: Bool
    /// Would this payload change anything here? A pill dragged over its own
    /// field, or prose with nobody in it, is refused up front.
    let accepts: (String) -> Bool
    let drop: ([String]) -> Bool

    func body(content: Content) -> some View {
        #if os(macOS)
            content.background(PillDropWell(targeted: $targeted, accepts: accepts, drop: drop))
        #else
            content.dropDestination(for: String.self) { items, _ in
                drop(items)
            } isTargeted: { targeted = $0 }
        #endif
    }
}

#if os(macOS)
    private struct PillDropWell: NSViewRepresentable {
        @Binding var targeted: Bool
        let accepts: (String) -> Bool
        let drop: ([String]) -> Bool

        func makeNSView(context: Context) -> PillDropWellView {
            let view = PillDropWellView()
            update(view)
            return view
        }

        func updateNSView(_ view: PillDropWellView, context: Context) { update(view) }

        private func update(_ view: PillDropWellView) {
            view.onTargeted = { [targeted = $targeted] in
                if targeted.wrappedValue != $0 { targeted.wrappedValue = $0 }
            }
            view.accepts = accepts
            view.drop = drop
        }
    }

    final class PillDropWellView: NSView {
        var onTargeted: (Bool) -> Void = { _ in }
        var accepts: (String) -> Bool = { _ in false }
        var drop: ([String]) -> Bool = { _ in false }

        /// Not a window handle. SwiftUI routes a click on the content over this
        /// view (pills, caret space, label) past it; a click on the well's bare
        /// padding reaches it, and must not pick up the window.
        override var mouseDownCanMoveWindow: Bool { false }

        /// In the registry while on screen, so a drag can find this well.
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if window != nil {
                PillDropRegistry.shared.add(self)
            } else {
                PillDropRegistry.shared.remove(self)
            }
        }
    }
#endif

// MARK: - flow layout

/// Minimal wrapping row for the send line: pills flow left to right and wrap,
/// and the LAST subview — the text field — stretches to the end of its line so
/// a click anywhere in the well lands the caret.
///
/// Not private: the Groups page flows member chips through the same layout, and
/// two wrapping rows that disagreed by a point would be visible side by side.
struct FlowLine: Layout {
    var spacing: CGFloat = 5

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: proposal.width ?? x, height: y + rowHeight)
    }

    func placeSubviews(
        in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()
    ) {
        var x = bounds.minX
        var y = bounds.minY
        var rowHeight: CGFloat = 0
        for (i, view) in subviews.enumerated() {
            var size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            // The field takes the rest of its line.
            if i == subviews.count - 1 {
                size.width = max(size.width, bounds.maxX - x)
            }
            view.place(
                at: CGPoint(x: x, y: y),
                proposal: ProposedViewSize(width: size.width, height: size.height))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}

// MARK: - the three fields together

/// THE ADDRESS BLOCK: `to`, plus `cc` and `bcc` FOLDED AWAY behind their own
/// labels on its line.
///
/// Folded because most mail has neither, and three wells for a message going to
/// one person is three questions where there was one. The labels are the whole
/// affordance — click "cc", the field unfolds with the caret already in it.
///
/// A FIELD HOLDING ADDRESSES IS NEVER FOLDED, whatever the toggle last said.
/// That is not a nicety: a restored draft whose bcc hid itself would be exactly
/// the invisible recipient this exists to make visible, and invisible in the one
/// direction that matters — the sender believing they are writing to fewer
/// people than they are. So `shown` ORs in "has content", and clicking a full
/// field's label focuses it instead of hiding it.
///
/// THIS is what owns the value the three fields share, which is why the move
/// verbs live here: `Recipients.move` needs all three lists to guarantee an
/// address lands in exactly one of them.
struct RecipientFields<F: Hashable>: View {
    @Binding var recipients: Recipients
    var focus: FocusState<F?>.Binding
    /// Maps a slot to the caller's own focus token — the two composers key their
    /// `@FocusState` differently.
    let field: (RecipientSlot) -> F
    /// Per-slot placeholder override. The reply composer uses it on `bcc`.
    var placeholder: (RecipientSlot) -> String? = { _ in nil }
    /// Group affordances, which belong to the `to` field alone: a group is an
    /// audience, and an audience is who the mail is TO.
    var suggestGroups = false
    var onGroupPicked: ((SendGroup) -> Void)?
    var resolvedGroup: (name: String, count: Int)?
    /// Header lines on a shared card instead of stacked wells. See
    /// `RecipientField.inline`.
    var inline = false
    /// Something the caller hangs on the `to` line ahead of cc/bcc — the pane
    /// composer's group picker.
    var toAccessory: AnyView?

    /// Which optional fields the sender has unfolded this session.
    @State private var revealed: Set<RecipientSlot> = []

    /// The two that fold. `to` is not one of them: a message with no recipient
    /// is not a message.
    private static var optional: [RecipientSlot] { [.cc, .bcc] }

    private func shown(_ slot: RecipientSlot) -> Bool {
        slot == .to || revealed.contains(slot) || !recipients[slot].trimmed.isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: inline ? 0 : Space.s8) {
            ForEach(RecipientSlot.allCases.filter(shown), id: \.self) { slot in
                // On a card the lines are ruled apart; stacked wells have
                // their own edges.
                if inline && slot != .to { Hairline() }
                RecipientField(
                    text: binding(slot), focus: focus, field: field(slot),
                    label: slot.label,
                    // Only `to` names an example address: three wells all
                    // reading "recipient@example.com" reads as three fields
                    // waiting to be filled, when two are ordinarily empty and
                    // correct that way.
                    placeholder: placeholder(slot) ?? (slot == .to ? "recipient@example.com" : ""),
                    suggestGroups: slot == .to && suggestGroups,
                    onGroupPicked: slot == .to ? onGroupPicked : nil,
                    resolvedGroup: slot == .to ? resolvedGroup : nil,
                    moves: RecipientMoves(
                        slot: slot,
                        move: { addr, destination in
                            var next = recipients
                            next.move(addr, to: destination)
                            recipients = next
                            revealed.insert(destination)
                            focus.wrappedValue = field(slot)
                        },
                        remove: { addr in
                            var next = recipients
                            next.remove(addr)
                            recipients = next
                            focus.wrappedValue = field(slot)
                        },
                        addressed: { recipients.slot(of: $0) != nil },
                        dragging: { active in dragging(active) }),
                    accessory: slot == .to ? AnyView(toggles) : nil,
                    inline: inline)
            }
        }
    }

    /// One field's slice of the shared value. Writes go through `Recipients.set`,
    /// not a bare assignment: somebody TYPED into this field who was sitting in
    /// another one has just been moved here — typing an address out is as
    /// explicit as clicking it there — and leaving the old copy behind is the
    /// exact shape this whole block exists to make impossible: an address in To
    /// that the sender believes went out blind.
    private func binding(_ slot: RecipientSlot) -> Binding<String> {
        Binding(
            get: { recipients[slot] },
            set: { value in
                guard recipients[slot] != value else { return }
                var next = recipients
                next.set(slot, to: value)
                recipients = next
            })
    }

    /// The unfold labels, in the block's own micro voice. Lit while their field
    /// is up, so the row doubles as a statement of what this message carries.
    private var toggles: some View {
        HStack(spacing: Space.s8) {
            toAccessory
            ForEach(Self.optional, id: \.self) { slot in
                Button(slot.label) { toggle(slot) }
                    .buttonStyle(.plain)
                    .font(Typo.micro)
                    .foregroundStyle(shown(slot) ? Palette.accent : Palette.inkFaintest)
                    // Two words in the micro voice look exactly like the label
                    // beside them until the pointer says otherwise.
                    .pointingHand()
                    .accessibilityLabel(shown(slot) ? "hide \(slot.label)" : "add \(slot.label)")
            }
        }
    }

    /// A PILL IN THE AIR UNFOLDS THE WHOLE HEADER. The fields people drag TO are
    /// the folded ones — nobody picks up a pill to drop it back where it was —
    /// so cc and bcc open the moment a drag begins, empty or not, and stay open
    /// until it ends. Then whichever are empty fold back, the drop having landed
    /// or not; the header returns to saying only what the message carries.
    ///
    /// The fold waits a turn: a drop writes through the destination's binding
    /// during the drop, and the session's end arrives right behind it, before
    /// SwiftUI has necessarily committed the write we are about to read.
    private func dragging(_ active: Bool) {
        if active {
            withAnimation(.easeOut(duration: 0.15)) { revealed.formUnion(Self.optional) }
        } else {
            DispatchQueue.main.async {
                withAnimation(.easeOut(duration: 0.15)) {
                    revealed = revealed.filter { !recipients[$0].trimmed.isEmpty }
                }
            }
        }
    }

    private func toggle(_ slot: RecipientSlot) {
        // A field with people in it does not fold away — it takes the caret, so
        // the click still does something and what it does is visible.
        guard recipients[slot].trimmed.isEmpty else {
            focus.wrappedValue = field(slot)
            return
        }
        if revealed.contains(slot) {
            revealed.remove(slot)
        } else {
            revealed.insert(slot)
            focus.wrappedValue = field(slot)
        }
    }
}

// MARK: - header lines

/// One line of a mail header: the label in a fixed left column, the value
/// beside it, an optional accessory on the right. What the pane composer's
/// `to`, `cc`, `bcc` and `subject` all are, so the four read as one block
/// rather than four captioned boxes.
struct InlineFieldRow<Content: View>: View {
    let label: String
    var accessory: AnyView?
    @ViewBuilder var content: Content

    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            FieldLabel(label)
                .frame(width: InlineFieldMetrics.label, alignment: .leading)
                .padding(.top, InlineFieldMetrics.labelDrop)
            content
                .font(Typo.ui(13))
                .foregroundStyle(Palette.ink)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let accessory {
                accessory
                    .padding(.top, InlineFieldMetrics.labelDrop)
                    .padding(.leading, Space.s8)
            }
        }
        .padding(.horizontal, InlineFieldMetrics.inset)
        .padding(.vertical, Space.scaled(9))
    }
}

/// What hangs under a header line (the suggestion list) starts in
/// the VALUE column, under what it is about, not under the label.
struct InlineFieldTuck: ViewModifier {
    let active: Bool

    func body(content: Content) -> some View {
        if active {
            content
                .padding(.leading, InlineFieldMetrics.inset + InlineFieldMetrics.label)
                .padding(.trailing, InlineFieldMetrics.inset)
                .padding(.bottom, Space.s8)
        } else {
            content
        }
    }
}

/// Computed, all three: they follow the density setting, and a `static let`
/// would have frozen at whichever density was live when a composer first
/// opened.
@MainActor
enum InlineFieldMetrics {
    /// The card's side inset. The body editor lines its text up on the same
    /// edge.
    static var inset: CGFloat { Space.s12 }
    /// The label column: holds the widest label ("subject") in the micro
    /// voice, so it widens with the type.
    static var label: CGFloat { Typo.measure(58) }
    /// Drops the micro label onto the 13pt value's text line. Derived from
    /// the two type sizes rather than scaled as spacing: it is the gap between
    /// their first lines, and the type curve is what moves that.
    static var labelDrop: CGFloat { Typo.size(13) - Typo.size(10) }
}
