// THE AGENT-DOOR AD. A week into the app, once, for somebody whose mailbox has
// an agent door they have never looked at: a card in the bottom-right corner,
// the iPhone ad's sibling, that shows what the door is for in two seconds.
//
// THE HERO IS THE PITCH. The same live P7 phosphor (AdCard.swift), and rising
// out of it a conversation with your own assistant: you ask "What needs me
// today?", it checks Passband, and its answer streams in as three short lines
// that could only have come from a triaged inbox: a payment due, a package
// arriving, and nothing else. A chat, not a terminal, because the assistants
// this leads with (Claude, OpenClaw, Hermes Agent) are ones people talk to.
// Nothing in it writes, drafts, sends or archives, because the door cannot.
// Reduce Motion, or a window that is not key, holds the finished answer still.
//
// THE SEAL IS THE SECOND FEATURE: sign-in codes and password reset links are
// withheld from every agent. The card claims those two and no more, on
// purpose: an informational security alert ("new sign-in to your account")
// may be readable, per docs/SECURITY.md, so "security alerts" is not on the
// card. It also makes no promise about what else an agent may do later.
// Agents are named in plain text and nowhere drawn.
//
// ONE BUTTON, to Settings > Agents, which closes the card on the way. No QR
// and no link: the setup is a paste into an agent on this Mac or the
// person's own, and the pane has every agent's snippet.
//
// A FIXED COMPOSITION like the iPhone ad: every size is a literal, an
// `AdCard` constant or `fixed:` type on purpose, none follows the density
// setting, and the card is the iPhone ad's footprint.
// NOT MODAL, bottom-RIGHT, and NO `.defaultAction`: an unbidden card does not
// get to own Return.

import SwiftUI

struct AgentNudgeModal: View {
    @Environment(AppStore.self) private var store
    private var nudge: AgentNudge { AgentNudge.shared }

    var body: some View {
        if nudge.showingNudge {
            AgentAdCard { opened in
                nudge.showingNudge = false
                if opened { store.openSettings(.agents) }
                Analytics.capture("agent_nudge_closed", ["opened": opened])
            }
            .adCardPlacement()
        }
    }
}

extension AgentNudge {
    /// Every other one-time card that can be on screen: the share ask, the
    /// iPhone ad, the tour (or the practice inbox being set up), What's New,
    /// and the update card. Two of these at once is a pile.
    static func othersShowing(_ store: AppStore) -> Bool {
        ShareNudge.shared.showingNudge
            || TestFlightNudge.shared.showingNudge
            || store.tour.active
            || store.tour.preparingPractice
            || store.whatsNew.active
            || Updater.shared.phase != .idle
    }

    /// The rule over the live app: the door this mailbox's server has, asked
    /// the way an agent would, and the audit's one trace of an agent. Both run
    /// only once the free conditions pass (see `askIfEarned`).
    func askIfEarned(store: AppStore) {
        guard store.connStatus == .connected,
            let endpoint = AgentConnect.endpoint(serverURL: store.settings?.serverURL)
        else { return }
        Task {
            await askIfEarned(
                othersShowing: { Self.othersShowing(store) },
                door: { await AgentConnect.probe(endpoint) },
                agentSeen: {
                    // The daemon clamps at 500, the whole ledger it will hand
                    // over. Nil when it did not answer.
                    guard let rows = try? await APIClient.shared.getAudit(limit: 500) else { return nil }
                    return Self.agentSeen(inAuditActors: rows.map(\.actor))
                })
        }
    }
}

private struct AgentAdCard: View {
    let close: (_ opened: Bool) -> Void

    @Environment(\.controlActiveState) private var activeState
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var arrived = false
    @State private var leaving = false
    @State private var hovering = false

    var body: some View {
        VStack(spacing: 0) {
            AdHero(live: activeState == .key, pill: "NEW  ·  AGENT MCP", close: { dismiss(opened: false) }) {
                AgentSessionMock(reduceMotion: reduceMotion, live: activeState == .key)
                    .offset(y: arrived ? 18 : 220)
                    .animation(
                        reduceMotion ? nil : .spring(response: 0.75, dampingFraction: 0.8).delay(0.6),
                        value: arrived)
            }
            words
        }
        .adCardChrome(arrived: arrived, leaving: leaving, label: "Connect an agent to Passband")
        .onAppear {
            withAnimation(reduceMotion ? .easeOut(duration: 0.2) : .spring(response: 0.55, dampingFraction: 0.82).delay(0.35)) {
                arrived = true
            }
        }
    }

    // MARK: - the words

    private var words: some View {
        // Spaced a touch tighter than the iPhone ad's so the card, with its
        // extra line, stays inside that card's footprint.
        VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Your agent, briefed.")
                    .font(Typo.hero(25, fixed: true))
                    .foregroundStyle(Palette.ink)
                Text("Claude, OpenClaw, Hermes Agent and others can read your triaged inbox. Ask yours what needs you.")
                    .font(Typo.ui(12, fixed: true))
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
            }

            sealedNote

            HStack(spacing: 10) {
                Button { dismiss(opened: true) } label: {
                    HStack(spacing: 6) {
                        Text("Connect an agent")
                        Image(systemName: "arrow.right")
                            .font(Typo.ui(10, weight: .bold, fixed: true))
                            .offset(x: hovering ? 2 : 0)
                    }
                    .font(Typo.ui(12, weight: .semibold, fixed: true))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 7)
                    .background(Palette.accent.opacity(hovering ? 1 : 0.92), in: Capsule())
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .onHover { inside in withAnimation(.easeOut(duration: 0.15)) { hovering = inside } }
                .help("Open Settings, Agents")

                Text("Settings  ›  Agents")
                    .font(Typo.ui(10, weight: .medium, fixed: true))
                    .foregroundStyle(Palette.inkFaintest)
            }
        }
        .adWordsPadding()
    }

    /// THE SEAL, as a feature rather than fine print: sign-in codes and
    /// security mail are sealed inside Passband and never handed to an agent
    /// at all. Scoped to exactly that: it says nothing about what else an
    /// agent can or cannot do. The tile is a code you cannot read, drawn in
    /// the card's own type, not a padlock.
    private var sealedNote: some View {
        HStack(alignment: .center, spacing: 11) {
            VStack(spacing: 3) {
                Text("CODE")
                    .font(Typo.ui(6.5, weight: .bold, fixed: true).width(.expanded))
                    .tracking(0.5)
                    .foregroundStyle(Palette.inkFaint)
                Text("••••••")
                    .font(Typo.ui(11, weight: .bold, design: .monospaced, fixed: true))
                    .tracking(-0.5)
                    .foregroundStyle(Palette.ink)
            }
            .frame(width: 50, height: 38)
            .background(Palette.ink.opacity(0.05), in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75))
            .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 2) {
                Text("Sign-in codes stay sealed.")
                    .font(Typo.ui(12, weight: .semibold, fixed: true))
                    .foregroundStyle(Palette.ink)
                Text("Codes and password reset links never reach your agent. They stay in Passband.")
                    .font(Typo.ui(10, weight: .medium, fixed: true))
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    // MARK: - verbs

    private func dismiss(opened: Bool) {
        // A double click lands inside the fade: one close, one event.
        guard !leaving else { return }
        withAnimation(.easeIn(duration: 0.2)) { leaving = true }
        Task {
            try? await Task.sleep(for: .milliseconds(200))
            close(opened)
        }
    }
}

// MARK: - the conversation

/// A conversation with your own assistant, drawn in SwiftUI with no product's
/// name or mark on it: your question, a beat while it checks Passband, its
/// answer streaming in, a rest, and round again. Always dark, like the iPhone
/// ad's phone: it is a screen inside a night scene, whatever the Mac's
/// appearance.
///
/// The mail in it is the practice inbox's (Cedar Credit, Rainforest): invented,
/// and already the house's.
///
/// Not private: the What's New walk plays the same conversation on the agent
/// door's page, so the ad and the release note show one thing.
struct AgentSessionMock: View {
    let reduceMotion: Bool
    let live: Bool

    /// How far the loop has got. The defaults are the finished answer, which
    /// is also the still frame.
    @State private var asked = true
    @State private var checking = CheckState.done
    @State private var answerCounts = Self.answer.map(\.count)
    @State private var shown = true
    @State private var pulse = false

    enum CheckState { case hidden, running, done }

    private static let size = CGSize(width: 256, height: 176)
    private static let ground = Color(hex: 0x0E1420)
    private static let bubble = Color(hex: 0x1C2634)
    private static let ink = Color(hex: 0xE9EEF5)
    private static let dim = Color(hex: 0x9AA7B8)
    private static let faint = Color(hex: 0x66748A)
    private static let accent = Color(hex: 0x4E9BEA)
    private static let ok = Color(hex: 0x6CCB8F)
    private static let body = Font.system(size: 10.5)

    private static let question = "What needs me today?"

    /// One line of the answer: runs of plain and bright text, so the stream
    /// can count characters across them.
    private struct Line {
        let runs: [(text: String, bright: Bool)]
        var count: Int { runs.reduce(0) { $0 + $1.text.count } }
    }

    private static let answer: [Line] = [
        Line(runs: [("$86.20", true), (" to Cedar Credit, due ", false), ("Friday", true)]),
        Line(runs: [("Your notebooks", true), (" arrive today", false)]),
        Line(runs: [("Nothing else needs you.", false)]),
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            VStack(alignment: .leading, spacing: 0) {
                userBubble
                    .frame(maxWidth: .infinity, alignment: .trailing)
                    .opacity(asked ? 1 : 0)
                    .scaleEffect(asked ? 1 : 0.9, anchor: .bottomTrailing)
                    .padding(.bottom, 6)
                checkLine
                    .opacity(checking == .hidden ? 0 : 1)
                    .padding(.bottom, 5)
                replyBubble
                    .opacity(answerCounts[0] > 0 ? 1 : 0)
            }
            .opacity(shown ? 1 : 0)
            .padding(.horizontal, 11)
            .padding(.top, 8)
            Spacer(minLength: 0)
        }
        .frame(width: Self.size.width, height: Self.size.height, alignment: .top)
        .background(Self.ground.opacity(0.9), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(.white.opacity(0.1), lineWidth: 0.75))
        .shadow(color: Self.accent.opacity(0.32), radius: 22)
        .shadow(color: .black.opacity(0.5), radius: 10, y: 6)
        // The phone's pose, gentler: an object standing in the scene rather
        // than a dialog pasted on it, and the type still crisp.
        .rotation3DEffect(.degrees(-5), axis: (x: 0.12, y: 1, z: 0), perspective: 0.6)
        .task(id: live) { await loop() }
        .accessibilityHidden(true)
    }

    /// Whose conversation this is, and what it can see: your assistant, with
    /// Passband connected.
    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: "sparkle")
                .font(.system(size: 8, weight: .bold))
                .foregroundStyle(.white)
                .frame(width: 17, height: 17)
                .background(
                    LinearGradient(
                        colors: [Self.accent, Color(hex: 0x2B5FA8)],
                        startPoint: .topLeading, endPoint: .bottomTrailing),
                    in: Circle())
            Text("Your assistant")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Self.ink)
            Spacer(minLength: 4)
            HStack(spacing: 4) {
                Circle().fill(Self.ok).frame(width: 4.5, height: 4.5)
                Text("Passband")
                    .font(.system(size: 8.5, weight: .medium))
                    .foregroundStyle(Self.dim)
            }
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(.white.opacity(0.06), in: Capsule())
        }
        .padding(.horizontal, 10)
        .frame(height: 26)
        .overlay(alignment: .bottom) {
            Rectangle().fill(.white.opacity(0.07)).frame(height: 0.5)
        }
    }

    private var userBubble: some View {
        Text(Self.question)
            .font(Self.body.weight(.medium))
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(Self.accent, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    /// The assistant reaching for Passband: a beat of "checking", then done.
    private var checkLine: some View {
        HStack(spacing: 5) {
            if checking == .done {
                Image(systemName: "checkmark")
                    .font(.system(size: 7, weight: .bold))
                    .foregroundStyle(Self.ok)
            } else {
                HStack(spacing: 2.5) {
                    ForEach(0..<3, id: \.self) { i in
                        Circle()
                            .fill(Self.dim)
                            .frame(width: 3.5, height: 3.5)
                            .opacity(pulse ? (i == 1 ? 1 : 0.35) : (i == 1 ? 0.35 : 1))
                    }
                }
            }
            Text(checking == .done ? "Read your inbox in Passband" : "Checking Passband")
                .font(.system(size: 8.5, weight: .medium))
                .foregroundStyle(Self.faint)
        }
        .frame(height: 11, alignment: .leading)
    }

    private var replyBubble: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(Self.answer.indices, id: \.self) { i in
                Text(Self.attributed(Self.answer[i], count: answerCounts[i]))
                    .font(Self.body)
                    .lineLimit(1)
                    .frame(height: 13, alignment: .leading)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .frame(width: 214, alignment: .leading)
        .background(Self.bubble, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    /// The first `count` characters of a line, bright runs bright.
    private static func attributed(_ line: Line, count: Int) -> AttributedString {
        var out = AttributedString()
        var left = count
        for run in line.runs where left > 0 {
            var piece = AttributedString(String(run.text.prefix(left)))
            piece.foregroundColor = run.bright ? ink : dim
            if run.bright { piece.font = body.weight(.semibold) }
            out += piece
            left -= run.text.count
        }
        return out
    }

    // MARK: - the loop

    private func showAll() {
        asked = true
        checking = .done
        answerCounts = Self.answer.map(\.count)
        shown = true
    }

    /// Ask, check, answer, rest, forever, while the window is key. Reduce
    /// Motion, and a window in the background, get the finished answer held.
    private func loop() async {
        guard live, !reduceMotion else {
            showAll()
            return
        }
        // The "checking" dots breathe on their own clock while the loop runs.
        let breathe = Task { @MainActor in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(320))
                withAnimation(.easeInOut(duration: 0.3)) { pulse.toggle() }
            }
        }
        defer { breathe.cancel() }

        while !Task.isCancelled {
            asked = false
            checking = .hidden
            answerCounts = Self.answer.map { _ in 0 }
            withAnimation(.easeOut(duration: 0.3)) { shown = true }
            guard await pause(900) else { return }

            withAnimation(.spring(response: 0.38, dampingFraction: 0.72)) { asked = true }
            guard await pause(650) else { return }

            withAnimation(.easeOut(duration: 0.2)) { checking = .running }
            guard await pause(1100) else { return }
            withAnimation(.easeOut(duration: 0.2)) { checking = .done }
            guard await pause(280) else { return }

            for i in Self.answer.indices {
                for n in stride(from: 2, through: Self.answer[i].count, by: 2) {
                    answerCounts[i] = n
                    guard await pause(24) else { return }
                }
                answerCounts[i] = Self.answer[i].count
                guard await pause(i == Self.answer.count - 2 ? 420 : 220) else { return }
            }

            // Hold the answer long enough to read twice, then clear.
            guard await pause(4400) else { return }
            withAnimation(.easeIn(duration: 0.35)) { shown = false }
            guard await pause(550) else { return }
        }
    }

    /// Sleep, and say whether the loop should go on.
    private func pause(_ ms: Int) async -> Bool {
        try? await Task.sleep(for: .milliseconds(ms))
        return !Task.isCancelled
    }
}
