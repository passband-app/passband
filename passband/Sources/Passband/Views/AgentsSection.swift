// Settings' Agents pane: connecting an agent the human already runs (OpenClaw,
// Claude Code, Codex…) to this mailbox over MCP.
//
// Three cards, in the order somebody needs them: WHERE the door is and whether
// it is open, HOW to point a given agent at it, and WHAT the agent gets once it
// is through. The last one is not decoration. Handing an agent your mail is
// the kind of decision people make once and remember, and the honest version
// of it includes the parts the agent cannot do and how the door is locked: a
// self-hosted door has no lock of its own, a hosted one asks each agent to sign
// in with the mailbox's Google account.
//
// Shared by both shells: the phone files it under the account page, the Mac
// beside the mail. This is also where managing connected agents will live
// once the daemon can name them; today it can only say whether the door is
// open, so that is all it claims.

import SwiftUI

struct AgentsSection: View {
    @Environment(AppStore.self) private var store
    @State private var connect = AgentConnect.shared
    @State private var agent: AgentKind = .openClaw

    private var endpoint: String? { AgentConnect.endpoint(serverURL: store.settings?.serverURL) }
    private var status: AgentDoorStatus { connect.status(for: endpoint) }
    private var auth: AgentDoorAuth { status.auth }
    private var agents: [AgentKind] { AgentKind.shown(for: auth) }
    /// The chosen agent, or the first one this door offers when the choice is
    /// not on its list (Claude Desktop picked, then the door turned out to be
    /// the sign-in one).
    private var shownAgent: AgentKind { agents.contains(agent) ? agent : agents[0] }
    /// A hosted mailbox (see `HostedServer`), whose owner cannot touch the
    /// daemon, so the self-host remedies are never offered to them.
    private var hosted: Bool { HostedServer.isHosted(url: store.settings?.serverURL ?? "") }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s16) {
            doorCard
            // Not for a server with no agent door at all: a ready-to-paste
            // command for a 404 is a trap. See `showsSetup` for hosted.
            if let endpoint, AgentConnect.showsSetup(status, hosted: hosted) {
                setupCard(endpoint)
                reachCard
            }
        }
        .task(id: endpoint) {
            await connect.check(serverURL: store.settings?.serverURL)
        }
    }

    // MARK: - where the door is

    private var doorCard: some View {
        SectionCard(label: "Connect an agent") {
            if let endpoint {
                SettingsRow(
                    title: "Endpoint",
                    detail: "Point an agent you already run here. It reads what Passband has triaged, over MCP, and never holds your Gmail sign-in."
                ) {
                    VStack(alignment: .trailing, spacing: Space.s6) {
                        HStack(spacing: Space.s8) {
                            Text(endpoint)
                                .font(Typo.mono(12))
                                .foregroundStyle(Palette.ink)
                                .textSelection(.enabled)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            CopyButton(text: endpoint)
                        }
                        HStack(spacing: Space.s10) {
                            statusDot
                            if status != .checking {
                                Button("check again") {
                                    Task { await connect.check(serverURL: store.settings?.serverURL) }
                                }
                                .buttonStyle(.plain)
                                .font(Typo.micro)
                                .foregroundStyle(Palette.accent)
                            }
                        }
                    }
                }
                statusExplanation(endpoint)
            } else {
                SettingsHint("Add an account under Account first. The agent door lives on the same server.")
            }
        }
    }

    @ViewBuilder private var statusDot: some View {
        switch status {
        case .unknown, .checking:
            StatusDot(color: Palette.inkFaintest, label: "checking…")
        case .open:
            StatusDot(color: Palette.positive, label: "open")
        case .signInRequired:
            StatusDot(color: Palette.positive, label: "open with sign-in")
        case .hostRefused where hosted:
            StatusDot(color: Palette.warn, label: "unavailable")
        case .hostRefused:
            StatusDot(color: Palette.warn, label: "refused by name")
        case .notServed:
            StatusDot(color: Palette.inkFaint, label: "not offered here")
        case .unreachable:
            StatusDot(color: Palette.danger, label: "no answer")
        }
    }

    @ViewBuilder private func statusExplanation(_ endpoint: String) -> some View {
        switch status {
        case _ where AgentConnect.isHostedOutage(status, hosted: hosted):
            SettingsHint(AgentConnect.hostedUnavailable)
        case .open where AgentConnect.isLoopback(endpoint):
            SettingsHint(
                "This address only works on this computer, so the agent has to run here too. For an agent on another machine, reach the daemon over your tailnet instead."
            )
        case .open:
            SettingsHint("Any agent that can reach this address can connect.")
        case .signInRequired:
            SettingsHint(
                "Agents connect with this address alone. There is no key to copy. Each one opens a browser and asks you to sign in with the Google account of this mailbox, then to approve it."
            )
        case .hostRefused:
            let host = AgentConnect.host(of: endpoint) ?? "your-host"
            SettingsHint(
                "Your daemon is up, but it doesn't answer agents that call it by this name. Add this host to SQUELCH_MCP_ALLOWED_HOSTS in the daemon's environment (comma-separated, keeping anything already there) and restart it:"
            )
            CodeBlock(text: host)
        case .notServed:
            SettingsHint(
                "Nothing answers at /mcp on this server. Hosted Passband doesn't open the agent door yet. A self-hosted daemon does."
            )
        case .unreachable:
            SettingsHint(
                "The agent door didn't answer from here. If the daemon is on another machine, check it is running and that this address reaches it."
            )
        case .unknown, .checking:
            EmptyView()
        }
    }

    // MARK: - how to connect one

    private func setupCard(_ endpoint: String) -> some View {
        let agent = shownAgent
        return SectionCard(label: "Set it up", note: agent.name) {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: Typo.measure(142)), spacing: Space.s8)], spacing: Space.s8) {
                ForEach(agents) { kind in
                    agentChip(kind)
                }
            }
            SettingsHint(agent.instruction(for: auth))
            CodeBlock(text: agent.snippet(endpoint: endpoint, auth: auth))
            if let signIn = agent.signIn(for: auth) {
                followUp("then sign in with", signIn)
            }
            if let check = agent.check {
                followUp("then check with", check)
            }
        }
    }

    private func followUp(_ label: String, _ command: String) -> some View {
        HStack(spacing: Space.s8) {
            Text(label)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
            Text(command)
                .font(Typo.mono(10))
                .foregroundStyle(Palette.inkDim)
                .textSelection(.enabled)
        }
    }

    private func agentChip(_ kind: AgentKind) -> some View {
        let active = kind == shownAgent
        return Button {
            withAnimation(Motion.disclose) { agent = kind }
        } label: {
            HStack(spacing: Space.scaled(7)) {
                Image(systemName: kind.symbol)
                    .font(Typo.ui(11, weight: .medium))
                    .frame(width: Space.s14)
                Text(kind.name)
                    .font(Typo.ui(12, weight: .medium))
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .foregroundStyle(active ? Palette.accent : Palette.inkDim)
            .padding(.horizontal, Space.s10)
            .padding(.vertical, Space.scaled(7))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(active ? Palette.accentSoft : Palette.canvas.opacity(0.5))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(active ? Palette.accent.opacity(0.4) : Palette.hairline, lineWidth: 0.75)
        )
        .accessibilityAddTraits(active ? .isSelected : [])
    }

    // MARK: - what it gets

    private var reachCard: some View {
        SectionCard(label: "What your agent can do") {
            HStack(alignment: .top, spacing: Space.scaled(18)) {
                reachColumn(
                    "can", symbol: "checkmark", tint: Palette.positive,
                    items: [
                        "Read ranked updates and whole threads",
                        "Search your mail",
                        "List bills, deadlines and packages",
                        "Set sender rules, inside Passband only",
                    ])
                reachColumn(
                    "can't", symbol: "xmark", tint: Palette.danger,
                    items: [
                        "Send, archive, label or delete",
                        "See sign-in codes or password resets",
                        "Change anything in Gmail",
                    ])
            }
            if auth == .signIn {
                SettingsHint(
                    "An agent you approve keeps access until it is removed. To remove one, contact support for now."
                )
            } else {
                SettingsHint(
                    "The agent door has no password of its own. Anything that can reach the address above can read your triaged mail, so keep it on this computer or your tailnet."
                )
            }
        }
    }

    private func reachColumn(_ title: String, symbol: String, tint: Color, items: [String]) -> some View {
        VStack(alignment: .leading, spacing: Space.s6) {
            Text(title)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.uppercase)
            ForEach(items, id: \.self) { item in
                HStack(alignment: .firstTextBaseline, spacing: Space.scaled(7)) {
                    Image(systemName: symbol)
                        .font(Typo.ui(9, weight: .bold))
                        .foregroundStyle(tint)
                        .frame(width: Space.scaled(11))
                    Text(item)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A command or a config fragment, selectable, with its own copy button. The
/// button copies exactly what is drawn, so there is no second string to drift.
private struct CodeBlock: View {
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: Space.s8) {
            ScrollView(.horizontal, showsIndicators: false) {
                Text(text)
                    .font(Typo.mono(11))
                    .foregroundStyle(Palette.ink)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: true, vertical: true)
                    .padding(.vertical, 1)
            }
            CopyButton(text: text)
        }
        .padding(.horizontal, Space.s12)
        .padding(.vertical, Space.s8)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(Palette.canvas.opacity(0.75))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75)
        )
    }
}

private struct CopyButton: View {
    let text: String
    @State private var copied = false

    var body: some View {
        Button {
            Clip.copy(text, flashing: $copied)
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                .font(Typo.ui(11, weight: .medium))
                .foregroundStyle(copied ? Palette.positive : Palette.inkFaint)
                .frame(width: Space.scaled(18), height: Space.scaled(18))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(copied ? "Copied" : "Copy")
        .accessibilityLabel(copied ? "Copied" : "Copy")
    }
}
