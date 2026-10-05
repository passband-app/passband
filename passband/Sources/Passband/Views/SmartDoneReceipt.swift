// THE SMART DONE RECEIPT. One press cleared a batch of Reading mail nobody
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

#if os(macOS)
    import SwiftUI

    struct SmartDoneReceiptCard: View {
        @Environment(AppStore.self) private var store
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        let receipt: AppStore.SmartDoneReceipt

        @State private var arrived = false
        @State private var counted = false

        private var lines: [SmartDoneCopy.Line] { SmartDoneCopy.lines(byReason: receipt.byReason) }

        var body: some View {
            VStack(alignment: .leading, spacing: 0) {
                header
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(SmartDoneCopy.headline(cleared: counted ? receipt.cleared : 0))
                        .font(Typo.serif(23, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .monospacedDigit()
                        .contentTransition(.numericText(value: Double(counted ? receipt.cleared : 0)))
                    if let from = SmartDoneCopy.senders(receipt.senders) {
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
                if receipt.modelMissed {
                    Text(SmartDoneCopy.modelMissedNote)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .padding(.top, Space.s8)
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
            .padding(.horizontal, Space.s14)
            .padding(.top, Space.s12)
            .padding(.bottom, Space.s12)
            .frame(width: Typo.measure(286), alignment: .leading)
            .passbandGlass(.pane, cornerRadius: 16, tint: Palette.glassTintStrong)
            .shadow(color: .black.opacity(0.28), radius: 24, y: 12)
            .offset(x: arrived ? 0 : 28, y: arrived ? 0 : 14)
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
            var parts = [SmartDoneCopy.headline(cleared: receipt.cleared)]
            if let from = SmartDoneCopy.senders(receipt.senders) { parts.append(from) }
            parts += lines.map(\.spoken)
            if receipt.batchId != nil { parts.append("Press U to undo") }
            return parts.joined(separator: ". ")
        }

        private var header: some View {
            HStack(spacing: Space.scaled(5)) {
                Image(systemName: "checkmark.rectangle.stack")
                    .font(Typo.ui(9, weight: .semibold))
                Text("SMART DONE")
                    .font(Typo.ui(8.5, weight: .bold).width(.expanded))
                    .tracking(0.6)
                Spacer(minLength: Space.s8)
                Button {
                    store.dismissSmartDoneReceipt()
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
                    Task { await store.undoSmartDone() }
                } label: {
                    HStack(spacing: Space.s6) {
                        Kbd("u")
                        Text(receipt.undoing ? "Bringing them back" : "Undo")
                            .font(Typo.ui(12, weight: .semibold))
                            .foregroundStyle(Palette.ink)
                        if !receipt.undoing {
                            UndoCountdown(
                                startedAt: receipt.createdAt, expiresAt: receipt.expiresAt,
                                tint: Palette.accent)
                        }
                    }
                    .padding(.horizontal, Space.s10)
                    .padding(.vertical, Space.scaled(5))
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
#endif
