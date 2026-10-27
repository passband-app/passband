// The "Needs a decision" cards, drawn by both shells: the Mac's Auth page rail
// and the phone's login-codes page. One stack, so the two never disagree about
// which questions are open (`AuthDecisions.isOpen`), how they group and sort
// (`AuthPolicy.cards`) or what the buttons say (`AuthPolicy.answers`).

import SwiftUI

/// "Needs a decision", its count, and past `AuthPolicy.clearAllAbove` cards a
/// "Clear all" that answers every non-critical question "That was me" after
/// one confirmation. `cardCount` is the COLLAPSED count, the one the rail badge
/// shows, handed in so the caller computes the cards once.
struct AuthDecisionHeader: View {
    let open: [SealedMeta]
    let cardCount: Int
    let showsClearAll: Bool

    @State private var confirming = false

    var body: some View {
        let clearable = AuthPolicy.clearable(open)
        HStack(spacing: Space.s8) {
            Text("Needs a decision")
                .font(Typo.zoneTitle)
                .foregroundStyle(Palette.ink)
            Spacer()
            Text("\(cardCount) open")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
            if showsClearAll && !clearable.isEmpty {
                Button("Clear all") { confirming = true }
                    .buttonStyle(.glass)
                    .foregroundStyle(Palette.inkDim)
                    .font(Typo.micro)
                    .decisionControlSize()
            }
        }
        .confirmationDialog(
            clearable.count == 1
                ? "Clear 1 question from Needs a decision?"
                : "Clear \(clearable.count) questions from Needs a decision?",
            isPresented: $confirming, titleVisibility: .visible
        ) {
            Button("Clear \(clearable.count)") {
                AuthDecisions.shared.setAll(clearable, .mine)
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(AuthPolicy.clearAllLine(cleared: clearable.count, kept: open.count - clearable.count))
        }
    }
}

/// Open questions as cards: critical first, bursts collapsed unless the owner
/// asked to review them one by one. Writes verdicts through `AuthDecisions`;
/// "Not me" also hands the message to `onNotMine`, which opens the reveal.
struct AuthDecisionStack: View {
    let open: [SealedMeta]
    let limit: Int
    @Binding var expanded: Set<Int>
    let onNotMine: (SealedMeta) -> Void

    private var decisions = AuthDecisions.shared

    init(
        open: [SealedMeta], limit: Int, expanded: Binding<Set<Int>>,
        onNotMine: @escaping (SealedMeta) -> Void
    ) {
        self.open = open
        self.limit = limit
        self._expanded = expanded
        self.onNotMine = onNotMine
    }

    var body: some View {
        ForEach(AuthPolicy.cards(open, expanded: expanded).prefix(limit)) { card in
            switch card {
            case .single(let m):
                AuthDecisionCard(
                    meta: m,
                    onMine: { decisions.set(m, .mine) },
                    onNotMine: {
                        // Not a dismissal: record the verdict, then open the
                        // message so the human can act on it. The server puts
                        // it in For your eyes.
                        decisions.set(m, .notMine)
                        onNotMine(m)
                    })
            case .burst(let b):
                AuthBurstCard(
                    burst: b,
                    onAllMine: { decisions.setAll(b.items, .mine) },
                    onReviewEach: {
                        withAnimation(Motion.disclose) { expanded.formUnion(b.memberIds) }
                    })
            }
        }
    }
}

/// The phone's thumb needs a bigger button than the Mac's rail.
private extension View {
    func decisionControlSize() -> some View {
        #if os(macOS)
            controlSize(.small)
        #else
            controlSize(.regular)
        #endif
    }
}

/// One question awaiting the owner's verdict: a sign-in, an account change, an
/// app grant, a reset or a sign-in link. A CRITICAL change (recovery phone
/// moved, 2FA off, password changed) sorts first and reads as the danger it is:
/// the whole card takes the danger tone, edge to edge, with no countdown,
/// because it stays open (and in For your eyes) until answered.
struct AuthDecisionCard: View {
    let meta: SealedMeta
    let onMine: () -> Void
    let onNotMine: () -> Void

    private var answers: (mine: String, notMine: String) { AuthPolicy.answers(meta) }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            if meta.isCritical {
                Label("Critical change", systemImage: "exclamationmark.triangle.fill")
                    .font(Typo.ui(10, weight: .semibold))
                    .foregroundStyle(Palette.danger)
            }
            HStack(spacing: Space.scaled(7)) {
                Avatar(sender: meta.sender, size: 18)
                Text(SenderCache.resolved(meta.sender).displayName)
                    .font(Typo.ui(11, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                Spacer(minLength: Space.s4)
                Text(Fmt.relAge(meta.received_at))
                    .font(Typo.num(10))
                    .foregroundStyle(Palette.inkFaintest)
            }
            // Subject only: the body is sealed and this card never fetches it.
            Text(meta.subject)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkDim)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
            if meta.isCritical {
                Text("A change like this can hand someone the account. It stays in For your eyes until you answer it or mark it done.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                Chip(
                    text: AuthCopy.label(meta.kind), tone: Palette.inkFaint,
                    symbol: AuthCopy.symbol(meta.kind))
            }
            HStack(spacing: Space.scaled(7)) {
                Button(answers.mine, action: onMine)
                    .buttonStyle(.glassProminent)
                    .tint(Palette.accent)
                    .font(Typo.micro)
                Button(answers.notMine, action: onNotMine)
                    .buttonStyle(.glass)
                    .foregroundStyle(Palette.danger)
                    .font(Typo.micro)
            }
            .decisionControlSize()
        }
        .padding(Space.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(meta.isCritical ? Palette.danger.opacity(0.10) : Palette.hairline.opacity(0.5))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .strokeBorder(
                    meta.isCritical ? Palette.danger.opacity(0.35) : .clear, lineWidth: 1)
        )
    }
}

/// Three or more services seeing a new sign-in within a few days: what a new
/// phone or laptop looks like from the inbox. One card instead of nine, with
/// "All me" for the usual answer and "Review each" to lay them out singly.
/// Critical changes are never in one.
struct AuthBurstCard: View {
    let burst: AuthPolicy.Burst
    let onAllMine: () -> Void
    let onReviewEach: () -> Void

    /// One face per service, newest first.
    private var faces: [SealedMeta] {
        var seen = Set<String>()
        return burst.items.filter {
            seen.insert(AuthPolicy.service($0.sender) ?? $0.sender).inserted
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            HStack(spacing: Space.scaled(7)) {
                Image(systemName: "laptopcomputer.and.iphone")
                    .font(Typo.ui(12))
                    .foregroundStyle(Palette.lock)
                Text("New device?")
                    .font(Typo.ui(11, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                Spacer(minLength: Space.s4)
                if let newest = burst.items.first {
                    Text(Fmt.relAge(newest.received_at))
                        .font(Typo.num(10))
                        .foregroundStyle(Palette.inkFaintest)
                }
            }
            Text(AuthPolicy.burstLine(burst))
                .font(Typo.micro)
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: Space.scaled(-4)) {
                ForEach(faces.prefix(6)) { m in
                    Avatar(sender: m.sender, size: 18)
                        .help(SenderCache.resolved(m.sender).displayName)
                }
                if faces.count > 6 {
                    Text("+\(faces.count - 6)")
                        .font(Typo.num(10))
                        .foregroundStyle(Palette.inkFaintest)
                        .padding(.leading, Space.s8)
                }
            }
            HStack(spacing: Space.scaled(7)) {
                Button("All me", action: onAllMine)
                    .buttonStyle(.glassProminent)
                    .tint(Palette.accent)
                    .font(Typo.micro)
                Button("Review each", action: onReviewEach)
                    .buttonStyle(.glass)
                    .foregroundStyle(Palette.inkDim)
                    .font(Typo.micro)
            }
            .decisionControlSize()
        }
        .padding(Space.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(Palette.hairline.opacity(0.5))
        )
    }
}
