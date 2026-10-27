// THE SMART CLEAR RECEIPT. One press cleared a batch of Reading mail nobody
// looked at one by one, so what it took is put on the table: the count, a
// line per reason in terms a person can agree with, and an Undo that says how
// long it lasts by draining.
//
// NOT A DIALOG. A corner card in the family of the unsubscribe tray and the
// iPhone ad, under the tray in the sitrep's bottom-right (see ActionLayer):
// it reports something that already happened, and the board it happened to
// stays usable beside it. No button on it owns Return.
//
// Every number here is something that went away. Sender names never appear:
// the daemon sends counts, and a receipt is not a list to re-read.
//
// ON A PHONE the same card rides above the tab bar, full width, in
// MobileToastHost's stack, and the full list is a sheet
// (`SmartClearListSheet`) rather than a page in a content pane the phone does
// not have. The card, the ring and the list's rows are one code path.

import SwiftUI

/// The list's row paper. The Mac's `pagePaper`, restated here because the
/// page scaffold that owns it is Mac-only.
private let smartClearPaper = Color(light: 0xF8FAFD, dark: 0x151E2A)

struct SmartClearReceiptCard: View {
    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let receipt: AppStore.SmartClearReceipt

    @State private var arrived = false
    @State private var counted = false

    private var lines: [SmartClearCopy.Line] { SmartClearCopy.lines(byReason: receipt.byReason) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            VStack(alignment: .leading, spacing: Space.s2) {
                Text(SmartClearCopy.headline(cleared: counted ? receipt.cleared : 0))
                    .font(Typo.serif(23, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .monospacedDigit()
                    .contentTransition(.numericText(value: Double(counted ? receipt.cleared : 0)))
                if let from = SmartClearCopy.senders(receipt.senders) {
                    Text(from)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                }
            }
            .padding(.top, Space.s6)

            if !lines.isEmpty {
                breakdown
                    .padding(.top, Space.s12)
            }
            if let note = receipt.modelNote {
                Text(note)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .padding(.top, Space.s8)
            }
            if receipt.items?.isEmpty == false {
                Button {
                    store.openSmartClearList()
                } label: {
                    HStack(spacing: Space.s4) {
                        Text(SmartClearCopy.viewList)
                            .font(Typo.ui(11.5, weight: .semibold))
                        Image(systemName: "chevron.right")
                            .font(Typo.ui(8.5, weight: .bold))
                    }
                    .foregroundStyle(Palette.accentInk)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Every email this cleared, with why. The undo timer waits while you look.")
                .padding(.top, Space.s10)
            }
            if receipt.batchId != nil {
                Rectangle()
                    .fill(Palette.hairline)
                    .frame(height: 1)
                    .padding(.top, Space.s12)
                undo
                    .padding(.top, Space.s10)
            }
        }
        .padding(.horizontal, ToastMetrics.hPadding)
        .padding(.vertical, ToastMetrics.vPadding)
        #if os(macOS)
            .frame(width: Typo.measure(286), alignment: .leading)
        #else
            // The host gives it the screen's width; it fills that.
            .frame(maxWidth: .infinity, alignment: .leading)
        #endif
        .toastGlass()
        // In from the corner it lives in: the right on the Mac, the
        // bottom (the tab bar's edge) on a phone.
        #if os(macOS)
            .offset(x: arrived ? 0 : 28, y: arrived ? 0 : 14)
        #else
            .offset(y: arrived ? 0 : 24)
        #endif
        .opacity(arrived ? 1 : 0)
        .onAppear {
            withAnimation(
                reduceMotion
                    ? .easeOut(duration: 0.2) : .spring(response: 0.5, dampingFraction: 0.84)
            ) { arrived = true }
            if reduceMotion {
                counted = true
            } else {
                withAnimation(.easeOut(duration: 0.8).delay(0.2)) { counted = true }
            }
            AccessibilityNotification.Announcement(spoken).post()
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(spoken)
    }

    private var spoken: String {
        var parts = [SmartClearCopy.headline(cleared: receipt.cleared)]
        if let from = SmartClearCopy.senders(receipt.senders) { parts.append(from) }
        parts += lines.map(\.spoken)
        #if os(macOS)
            if receipt.batchId != nil { parts.append("Press U to undo") }
        #else
            if receipt.batchId != nil { parts.append("Undo is available") }
        #endif
        return parts.joined(separator: ". ")
    }

    private var header: some View {
        HStack(spacing: Space.scaled(5)) {
            Image(systemName: "checkmark.rectangle.stack")
                .font(Typo.ui(9, weight: .semibold))
            Text("SMART CLEAR")
                .font(Typo.ui(8.5, weight: .bold).width(.expanded))
                .tracking(0.6)
            Spacer(minLength: Space.s8)
            Button {
                store.dismissSmartClearReceipt()
            } label: {
                Image(systemName: "xmark")
                    .font(Typo.ui(9, weight: .semibold))
                    .foregroundStyle(Palette.inkFaint)
                    .frame(width: Space.scaled(18), height: Space.scaled(18))
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Close")
            .accessibilityLabel("Close")
        }
        .foregroundStyle(Palette.accent.opacity(0.9))
    }

    /// The reasons, numbers in a column of their own so they read down
    /// as a tally and the words read across as sentences.
    private var breakdown: some View {
        VStack(alignment: .leading, spacing: Space.scaled(5)) {
            ForEach(lines, id: \.self) { line in
                HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                    Text("\(line.count)")
                        .font(Typo.num(12, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .frame(width: Typo.measure(20), alignment: .trailing)
                    Text(line.text)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(line.spoken)
            }
        }
    }

    private var undo: some View {
        HStack(spacing: Space.s8) {
            Button {
                #if os(iOS)
                    Haptics.commit()
                #endif
                Task { await store.undoSmartClear() }
            } label: {
                HStack(spacing: Space.s6) {
                    #if os(macOS)
                        Kbd("u")
                    #endif
                    Text(receipt.undoing ? "Bringing them back" : "Undo")
                        .font(Typo.ui(12, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                    if !receipt.undoing {
                        SmartClearRing(clock: receipt.clock)
                    }
                }
                #if os(macOS)
                    .padding(.horizontal, Space.s10)
                    .padding(.vertical, Space.scaled(5))
                #else
                    // A thumb's target, not a pointer's.
                    .padding(.horizontal, Space.s14)
                    .padding(.vertical, Space.s8)
                #endif
                .background(Capsule(style: .continuous).fill(Palette.accentSoft))
                .overlay(
                    Capsule(style: .continuous)
                        .strokeBorder(Palette.accent.opacity(0.35), lineWidth: 0.75)
                )
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .disabled(receipt.undoing)
            .help("Put all of it back in Reading (U)")
            Spacer(minLength: 0)
            Text("everything comes back")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
        }
    }
}

/// The undo window as a ring, drawn FROM THE CLOCK: it drains with it
/// and holds still exactly while the clock is stopped.
struct SmartClearRing: View {
    let clock: SmartClearClock
    private static let size: CGFloat = 11

    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 30, paused: clock.isPaused)) { ctx in
            ZStack {
                Circle()
                    .stroke(Palette.hairlineStrong, lineWidth: 1.75)
                Circle()
                    .trim(from: 0, to: clock.fraction(at: ctx.date))
                    .stroke(Palette.accent, style: StrokeStyle(lineWidth: 1.75, lineCap: .round))
                    .rotationEffect(.degrees(-90))
            }
            .frame(width: Self.size, height: Self.size)
        }
        .accessibilityHidden(true)
    }
}

// MARK: - the full list's rows, on both platforms

/// One reason's emails, as a card of rows. Shared by the Mac's page and
/// the phone's sheet.
struct SmartClearGroupSection: View {
    let group: SmartClearList.Group

    var body: some View { section(group) }

    private func section(_ group: SmartClearList.Group) -> some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            Text(group.heading)
                .font(Typo.ui(13, weight: .semibold))
                .foregroundStyle(Palette.ink)
            VStack(spacing: 0) {
                ForEach(Array(group.items.enumerated()), id: \.element.id) { i, item in
                    if i > 0 { Rectangle().fill(Palette.hairline).frame(height: 1) }
                    row(item)
                }
            }
            .background(RoundedRectangle(cornerRadius: Radius.well, style: .continuous).fill(smartClearPaper))
            .overlay(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .strokeBorder(Palette.hairline, lineWidth: 1)
            )
        }
    }

    /// Subject and sender are email-derived: Text only, never markup.
    private func row(_ item: SmartClearItem) -> some View {
        let subject = item.subject?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return HStack(alignment: .center, spacing: Space.s10) {
            Avatar(sender: "\(item.displayName) <\(item.sender)>", size: Space.scaled(24))
            VStack(alignment: .leading, spacing: Space.s2) {
                Text(verbatim: subject.isEmpty ? SmartClearCopy.noSubject : subject)
                    .font(Typo.row)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .truncationMode(.tail)
                HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                    Text(verbatim: item.displayName)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    if item.displayName != item.sender {
                        Text(verbatim: item.sender)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .layoutPriority(-1)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            VStack(alignment: .trailing, spacing: Space.s2) {
                Text(SmartClearCopy.why(item.reason))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                if item.receivedAt != nil {
                    Text(Fmt.relAge(item.receivedAt))
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }
            }
            .fixedSize()
        }
        .padding(.horizontal, Space.s12)
        .padding(.vertical, Space.s8)
        .help(subject)
        .accessibilityElement(children: .combine)
    }
}

#if os(macOS)
    // MARK: - the full list

    /// EVERYTHING ONE SMART CLEAR CLEARED, to audit: subject, sender and why,
    /// grouped by why. A full page in the content pane. Not a
    /// reader: nothing here opens.
    ///
    /// THE UNDO CLOCK IS STOPPED WHILE THIS IS UP (the store stops it on the
    /// way in and starts it on the way out, by every route, including this
    /// view simply going away). Undo takes the whole batch back, as the
    /// receipt's does. Confirm, and Esc, go back to the board with the
    /// receipt still up and its countdown carrying on from where it stopped:
    /// leaving must never undo, and must never cost the window either.
    struct SmartClearListPage: View {
        @Environment(AppStore.self) private var store
        let receipt: AppStore.SmartClearReceipt

        private var groups: [SmartClearList.Group] { SmartClearList.groups(receipt.items ?? []) }

        var body: some View {
            FullPage(title: "Smart clear", subtitle: "what it cleared") {
                Text(SmartClearCopy.headline(cleared: receipt.cleared))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
            } content: {
                ScrollView {
                    VStack(alignment: .leading, spacing: Space.s24) {
                        Text(SmartClearCopy.listTitle)
                            .font(Typo.serif(30, weight: .medium))
                            .foregroundStyle(Palette.ink)
                        ForEach(groups) { group in
                            SmartClearGroupSection(group: group)
                        }
                    }
                    .frame(maxWidth: Typo.measure(680), alignment: .leading)
                    .padding(.horizontal, Space.s24)
                    .padding(.vertical, Space.s32)
                    .frame(maxWidth: .infinity)
                }
            } bottom: {
                HStack(spacing: Space.s12) {
                    if receipt.batchId != nil {
                        SmartClearRing(clock: receipt.clock)
                        Text(SmartClearCopy.listPaused)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                    }
                    Spacer(minLength: Space.s8)
                    if receipt.batchId != nil {
                        PageButton(key: "u", label: receipt.undoing ? "Bringing them back" : "Undo all") {
                            undo()
                        }
                        .disabled(receipt.undoing)
                    }
                    PageButton(key: "return", label: "Confirm", role: .primary) {
                        store.closeSmartClearList()
                    }
                }
            }
            .keyContext(.modal)
            .keyBindings(.modal, [
                KeyBinding("u", "undo all of it") { undo() },
                KeyBinding("Enter", "confirm") { store.closeSmartClearList() },
                KeyBinding("Escape", "confirm") { store.closeSmartClearList() },
                KeyBinding("/", "nothing") {},
            ])
            // Left by a route nobody named: the clock still starts again.
            .onDisappear { store.closeSmartClearList() }
            .transition(.opacity)
        }

        private func undo() {
            guard receipt.batchId != nil, !receipt.undoing else { return }
            Task { await store.undoSmartClear() }
        }

    }
#endif

#if os(iOS)
    // MARK: - the full list, on a phone

    /// THE PHONE'S FULL LIST: the Mac page's content as a sheet, because a
    /// phone has no content pane to take over. The same contract: the undo
    /// clock is stopped while it is up (the store stops it in
    /// `openSmartClearList` and starts it again in `closeSmartClearList`,
    /// which the presenting binding calls on every way out, a drag down
    /// included), and leaving never undoes.
    struct SmartClearListSheet: View {
        @Environment(AppStore.self) private var store
        let receipt: AppStore.SmartClearReceipt

        private var groups: [SmartClearList.Group] { SmartClearList.groups(receipt.items ?? []) }

        var body: some View {
            NavigationStack {
                ScrollView {
                    VStack(alignment: .leading, spacing: Space.s20) {
                        VStack(alignment: .leading, spacing: Space.s4) {
                            Text(SmartClearCopy.listTitle)
                                .font(Typo.serif(26, weight: .medium))
                                .foregroundStyle(Palette.ink)
                            Text(SmartClearCopy.headline(cleared: receipt.cleared))
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaint)
                        }
                        ForEach(groups) { group in
                            SmartClearGroupSection(group: group)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, Space.s8)
                    .padding(.bottom, 28)
                }
                .background(Palette.canvas)
                .navigationTitle("Smart clear")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { store.closeSmartClearList() }
                    }
                }
                .safeAreaInset(edge: .bottom) {
                    if receipt.batchId != nil { undoBar }
                }
            }
        }

        /// The undo, with the stopped ring beside it saying the window is
        /// waiting. Undo takes the whole batch back, as the receipt's does.
        private var undoBar: some View {
            HStack(spacing: Space.s10) {
                SmartClearRing(clock: receipt.clock)
                Text(SmartClearCopy.listPaused)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .lineLimit(2)
                Spacer(minLength: Space.s8)
                Button {
                    guard !receipt.undoing else { return }
                    Haptics.commit()
                    Task { await store.undoSmartClear() }
                } label: {
                    Text(receipt.undoing ? "Bringing them back" : "Undo all")
                        .font(Typo.ui(14, weight: .semibold))
                        .padding(.horizontal, Space.s6)
                }
                .buttonStyle(.glass)
                .disabled(receipt.undoing)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, Space.s10)
            .background(.bar)
        }
    }
#endif
