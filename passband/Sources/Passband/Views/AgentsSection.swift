// Settings' Agents pane: connecting an agent the human already runs (Claude,
// OpenClaw, Hermes Agent, Codex…) to this mailbox over MCP.
//
// Four cards, in the order somebody needs them: WHERE the door is and whether
// it is open, HOW to point a given agent at it, what the agent may WRITE (the
// owner's setting, held by the daemon: read only, drafts, or sending), and
// WHAT the agent gets once it is through. The last one is not decoration. Handing an agent your mail is
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
    /// The chip somebody picked, nil until they pick one.
    @State private var agent: AgentKind?
    /// What the daemon says an agent may write. nil until it answers, and for
    /// good on a daemon older than the setting, where the card is not drawn:
    /// such a daemon has no tool that writes mail, so there is nothing to set.
    @State private var access: AgentAccessConfig?
    /// The setting could not be read, for a reason other than the daemon
    /// predating it (which is a 404 and simply draws no card). Kept apart
    /// from "not offered" because the two must not look alike: with this
    /// set, the pane does not know whether the agent can send, and says so
    /// instead of claiming it cannot.
    @State private var accessUnknown = false
    @State private var accessBusy = false
    @State private var accessError: String?

    private var endpoint: String? { AgentConnect.endpoint(serverURL: store.settings?.serverURL) }
    private var status: AgentDoorStatus { connect.status(for: endpoint) }
    private var auth: AgentDoorAuth { status.auth }
    private var agents: [AgentKind] { AgentKind.shown(for: auth) }
    /// The chosen agent, or the first one this door offers when nothing is
    /// chosen yet or the choice is not on its list (Claude Desktop picked,
    /// then the door turned out to be the sign-in one).
    private var shownAgent: AgentKind {
        if let agent, agents.contains(agent) { return agent }
        return AgentKind.defaultChoice(for: auth)
    }
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
                if let access {
                    writeCard(access)
                } else if accessUnknown {
                    writeUnknownCard
                }
                reachCard
            }
        }
        .task(id: endpoint) {
            // Another account's answer is never shown while this one loads.
            access = nil
            accessUnknown = false
            accessError = nil
            accessBusy = false
            await connect.check(serverURL: store.settings?.serverURL)
            await loadAccess()
        }
        // Somebody who has been here knows the door exists, so the one-time
        // ad for it never goes up after this (`AgentNudge`).
        .onAppear { AgentNudge.shared.notePaneSeen() }
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
                SettingsHint("Add an account under Account first. Agent MCP lives on the same server.")
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
                "Nothing answers at /mcp on this server. Hosted Passband doesn't offer Agent MCP yet. A self-hosted daemon does."
            )
        case .unreachable:
            SettingsHint(
                "Agent MCP didn't answer from here. If the daemon is on another machine, check it is running and that this address reaches it."
            )
        case .unknown, .checking:
            EmptyView()
        }
    }

    // MARK: - how to connect one

    private func setupCard(_ endpoint: String) -> some View {
        let agent = shownAgent
        let snippet = agent.snippet(endpoint: endpoint, auth: auth)
        return SectionCard(label: "Set it up") {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: Typo.measure(110)), spacing: Space.s8)], spacing: Space.s8) {
                ForEach(agents) { kind in
                    agentTile(kind)
                }
            }
            AgentActionRow(agent: agent, action: action(agent, endpoint: endpoint), snippet: snippet)
                .id(agent)
                .padding(.top, Space.s4)
            SettingsHint(agent.instruction(for: auth))
            CodeBlock(text: snippet)
            if let signIn = agent.signIn(for: auth) {
                followUp("then sign in with", signIn)
            }
            if let check = agent.check {
                followUp("then check with", check)
            }
        }
    }

    /// The agent's button. A phone has no Cursor or VS Code to open, so an
    /// install link into one becomes the copy button there.
    private func action(_ agent: AgentKind, endpoint: String) -> AgentAction {
        #if os(iOS)
            if agent.installOpensDesktopApp { return agent.copyAction(auth: auth) }
        #endif
        return agent.action(endpoint: endpoint, auth: auth)
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

    /// One agent: its own mark over its name. The selected one carries the
    /// accent on its whole outline, never one edge of it.
    private func agentTile(_ kind: AgentKind) -> some View {
        let active = kind == shownAgent
        return Button {
            withAnimation(Motion.disclose) { agent = kind }
        } label: {
            VStack(spacing: Space.s6) {
                AgentMark(kind: kind, size: Space.scaled(26))
                Text(kind.name)
                    .font(Typo.ui(11, weight: .medium))
                    .lineLimit(1)
                    .minimumScaleFactor(0.85)
            }
            .foregroundStyle(active ? Palette.accent : Palette.inkDim)
            .frame(maxWidth: .infinity)
            .padding(.vertical, Space.s10)
            .padding(.horizontal, Space.s6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .fill(active ? Palette.accentSoft : Palette.canvas.opacity(0.5))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .strokeBorder(active ? Palette.accent.opacity(0.5) : Palette.hairline, lineWidth: active ? 1 : 0.75)
        )
        .accessibilityLabel(kind.name)
        .accessibilityAddTraits(active ? .isSelected : [])
    }

    // MARK: - what it may write

    /// Read the setting for the mailbox on screen. A 404 is a daemon older
    /// than the setting; anything else is "could not find out".
    private func loadAccess() async {
        let asked = endpoint
        do {
            let loaded = try await APIClient.shared.getAgentAccess()
            guard asked == endpoint, !Task.isCancelled else { return }
            access = loaded
            accessUnknown = false
        } catch {
            guard asked == endpoint, !Task.isCancelled else { return }
            access = nil
            accessUnknown = (error as? APIError)?.kind != .notFound
        }
    }

    private var writeUnknownCard: some View {
        SectionCard(label: "What your agent may write") {
            SettingsHint("Could not read this setting from the server, so it is not shown here.")
            Button("try again") {
                Task { await loadAccess() }
            }
            .buttonStyle(.plain)
            .font(Typo.micro)
            .foregroundStyle(Palette.accent)
        }
    }

    /// The owner's one decision about writing, as three whole rows rather than
    /// a segmented control: each level is a different promise, and a promise
    /// needs its sentence beside it, not in a tooltip.
    private func writeCard(_ access: AgentAccessConfig) -> some View {
        SectionCard(label: "What your agent may write") {
            VStack(spacing: Space.s8) {
                ForEach(AgentWriteAccess.allCases) { level in
                    accessOption(level, chosen: access.level == level)
                }
            }
            .disabled(accessBusy)
            switch access.level {
            case .send:
                SettingsHint(
                    "The approval happens in your agent's chat, not in Passband. Passband tells the agent to ask before every send and cannot check that it did. It still blocks anything that looks like a password or key, and every send is in the audit log as your agent's."
                )
                if auth != .signIn {
                    Text(
                        "Agent MCP has no password here. Anything that can reach the address above could send mail as you, so keep it on this computer or your tailnet."
                    )
                    .font(Typo.micro)
                    .foregroundStyle(Palette.warn)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            case .drafts:
                SettingsHint("A draft your agent leaves is marked \"from your agent\" in your drafts.")
            case .some(.none), nil:
                EmptyView()
            }
            if let accessError {
                Text(accessError).font(Typo.micro).foregroundStyle(Palette.danger)
            }
        }
    }

    private func accessOption(_ level: AgentWriteAccess, chosen: Bool) -> some View {
        Button {
            setAccess(level)
        } label: {
            HStack(alignment: .center, spacing: Space.s12) {
                Image(systemName: level.symbol)
                    .font(Typo.ui(12, weight: .medium))
                    .foregroundStyle(chosen ? Palette.accent : Palette.inkFaint)
                    .frame(width: Space.scaled(18))
                VStack(alignment: .leading, spacing: Space.scaled(2)) {
                    Text(level.title)
                        .font(Typo.ui(12, weight: .medium))
                        .foregroundStyle(chosen ? Palette.ink : Palette.inkDim)
                    Text(level.detail)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: Space.s8)
                Image(systemName: chosen ? "checkmark.circle.fill" : "circle")
                    .font(Typo.ui(13, weight: .medium))
                    .foregroundStyle(chosen ? Palette.accent : Palette.inkFaintest)
            }
            .padding(.horizontal, Space.s12)
            .padding(.vertical, Space.s8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(chosen ? Palette.accentSoft : Palette.canvas.opacity(0.5))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(chosen ? Palette.accent.opacity(0.4) : Palette.hairline, lineWidth: 0.75)
        )
        .accessibilityLabel("\(level.title). \(level.detail)")
        .accessibilityAddTraits(chosen ? .isSelected : [])
    }

    /// Fire-and-wait rather than optimistic: the rows render the STORED
    /// answer, so a failed write leaves the choice where the daemon has it.
    /// This is the one setting where showing a level the daemon did not take
    /// would be a lie about who can send as you.
    private func setAccess(_ level: AgentWriteAccess) {
        guard !accessBusy, access?.level != level else { return }
        accessBusy = true
        accessError = nil
        // The mailbox this click was about. An answer that comes back after
        // a switch belongs to the other mailbox and is dropped: painting it
        // here would show one mailbox's level on another's card.
        let asked = endpoint
        Task {
            do {
                let saved = try await APIClient.shared.setAgentAccess(level)
                guard asked == endpoint else { return }
                withAnimation(Motion.disclose) { access = saved }
            } catch {
                guard asked == endpoint else { return }
                accessError = errText(error, "could not save")
            }
            accessBusy = false
        }
    }

    // MARK: - what it gets

    /// What the two columns say, which moves with the owner's write setting:
    /// a "can't send" beside a level that sends would be the pane lying.
    private var reach: (can: [String], cannot: [String]) {
        var can = [
            "Read ranked updates and whole threads",
            "Search your mail",
            "List bills, deadlines and packages",
            "Set sender rules, inside Passband only",
        ]
        var cannot = ["See sign-in codes or password resets"]
        // What the daemon says, or, when it has no such setting at all, the
        // read-only door that daemon is. Nil is "do not know": the setting
        // would not load, or holds a level this build cannot read. Then the
        // columns say nothing about writing mail, rather than a "can't send"
        // that may be false.
        let level: AgentWriteAccess? = access.map(\.level) ?? (accessUnknown ? nil : AgentWriteAccess.none)
        guard let level else {
            cannot.append("Archive, label or delete")
            return (can, cannot)
        }
        switch level {
        case .none:
            cannot.insert("Send, archive, label or delete", at: 0)
            cannot.append("Change anything in Gmail")
        case .drafts:
            can.append("Leave drafts for you to send")
            cannot.insert("Send, archive, label or delete", at: 0)
            cannot.append("Change anything in Gmail")
        case .send:
            can.append("Leave drafts for you to send")
            can.append("Send mail, after asking you")
            cannot.insert("Archive, label or delete", at: 0)
            cannot.append("Send a password or key")
        }
        return (can, cannot)
    }

    private var reachCard: some View {
        SectionCard(label: "What your agent can do") {
            HStack(alignment: .top, spacing: Space.scaled(18)) {
                reachColumn("can", symbol: "checkmark", tint: Palette.positive, items: reach.can)
                reachColumn("can't", symbol: "xmark", tint: Palette.danger, items: reach.cannot)
            }
            if auth == .signIn {
                SettingsHint(
                    "An agent you approve keeps access until it is removed. To remove one, contact support for now."
                )
            } else {
                SettingsHint(
                    "Agent MCP has no password of its own. Anything that can reach the address above can read your triaged mail, so keep it on this computer or your tailnet."
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

/// The head of the setup card: the agent's mark and name, a few words on what
/// its button does, and the button itself in the agent's own colours. A copy
/// reads "Copied" for the confirmation window, like the icon in the code block.
private struct AgentActionRow: View {
    let agent: AgentKind
    let action: AgentAction
    let snippet: String
    @State private var copied = false

    var body: some View {
        // Side by side where there is room; the button drops under the name
        // on a phone rather than squeezing it.
        ViewThatFits(in: .horizontal) {
            HStack(spacing: Space.s12) {
                identity
                Spacer(minLength: Space.s8)
                button
            }
            VStack(alignment: .leading, spacing: Space.s10) {
                identity
                button
            }
        }
    }

    private var identity: some View {
        HStack(spacing: Space.s10) {
            AgentMark(kind: agent, size: Space.scaled(32))
            VStack(alignment: .leading, spacing: Space.s2) {
                Text(agent.name)
                    .font(Typo.zoneTitle)
                    .foregroundStyle(Palette.ink)
                if let note = action.note {
                    Text(note)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                }
            }
            .fixedSize(horizontal: true, vertical: false)
        }
    }

    @ViewBuilder private var button: some View {
        let title = copied ? "Copied" : action.label
        if let brand = agent.brand.button {
            Button { run() } label: {
                HStack(spacing: Space.s6) {
                    // Only a variant of the mark its maker made for this
                    // background goes on the button; the mark is never tinted.
                    if let mark = brand.mark, AgentMark.hasAsset(mark) {
                        Image(mark)
                            .resizable()
                            .scaledToFit()
                            .frame(width: Space.s14, height: Space.s14)
                    }
                    Text(title)
                }
            }
            .buttonStyle(BrandButtonStyle(fill: brand.fill, ink: brand.ink))
        } else {
            Button(title) { run() }
                .buttonStyle(.glassProminent)
                .tint(Palette.accent)
        }
    }

    private func run() {
        switch action.kind {
        // Not `Opener.open`, which hands the OS http(s) only: these links are
        // built here from the mailbox's own address, never taken from mail,
        // and two of them are the agents' own schemes.
        case .open(let url):
            _ = Platform.open(url)
        case .copyAndOpen(let url):
            Clip.copy(snippet, flashing: $copied)
            _ = Platform.open(url)
        case .copy:
            Clip.copy(snippet, flashing: $copied)
        }
    }
}

/// How an agent looks here: its maker's own mark, from AgentLogos.xcassets,
/// and its maker's colour for the button. Where no official mark could be
/// had, the pane's SF Symbol stands in, on a tile in the maker's colour when
/// it has one. Where each mark and colour comes from is noted below. The marks
/// are the makers' files as published, never redrawn or recoloured.
struct AgentBrand {
    /// `AgentLogo/<name>` in AgentLogos.xcassets.
    let logo: String?
    /// The colour of the tile behind the stand-in symbol, when there is no mark.
    let symbolTile: Color?
    let button: ButtonBrand?

    struct ButtonBrand {
        let fill: Color
        let ink: Color
        /// A version of the mark made for `fill`, when the maker publishes one.
        let mark: String?
    }
}

extension AgentKind {
    var brand: AgentBrand {
        // Anthropic press kit (anthropic.com/press-kit): Claude Spark in Clay
        // 0xD97757, Slate 0x151514 for type. Clay is too light for white
        // type, so the button's label is Slate.
        let claude = AgentBrand.ButtonBrand(fill: Color(hex: 0xD97757), ink: Color(hex: 0x151514), mark: nil)
        // OpenAI's black. openai.com/brand refuses automated reads, so its
        // mark could not be fetched from the source and is not drawn here.
        let openAI = AgentBrand.ButtonBrand(fill: Color(hex: 0x000000), ink: .white, mark: nil)
        switch self {
        case .claude, .claudeCode, .claudeDesktop:
            return AgentBrand(logo: "AgentLogo/claude", symbolTile: nil, button: claude)
        case .openClaw:
            // OpenClaw's own app (apps/ios/Sources/Design/OpenClawBrand.swift):
            // the accent fill it keeps dark enough for white content.
            return AgentBrand(
                logo: "AgentLogo/openclaw", symbolTile: nil,
                button: .init(fill: Color(light: 0xB73833, dark: 0xC63E38), ink: .white, mark: nil))
        case .hermes:
            // The Hermes Agent docs site's primary (website/src/css/custom.css):
            // deep amber in light, gold in dark, which takes dark type.
            return AgentBrand(
                logo: "AgentLogo/hermes", symbolTile: nil,
                button: .init(
                    fill: Color(light: 0x8B6508, dark: 0xFFD700),
                    ink: Color(light: Color.white, dark: Color(hex: 0x14142A)), mark: nil))
        case .chatGPT, .codex:
            return AgentBrand(logo: nil, symbolTile: Color(hex: 0x000000), button: openAI)
        case .muse:
            // No mark or colour of Meta's for Muse has been fetched from the
            // source, so the stand-in symbol and the house button.
            return AgentBrand(logo: nil, symbolTile: nil, button: nil)
        case .cursor:
            // Cursor's own "Add to Cursor" badge (cursor.com/deeplink/
            // mcp-install-dark.svg): black at 90%, white type, the light cube.
            return AgentBrand(
                logo: "AgentLogo/cursor", symbolTile: nil,
                button: .init(fill: Color.black.opacity(0.9), ink: .white, mark: "AgentLogo/cursor-on-dark"))
        case .vsCode:
            // code.visualstudio.com/brand: 0x007ACC, and the white icon is the
            // one for blue backgrounds.
            return AgentBrand(
                logo: "AgentLogo/vscode", symbolTile: nil,
                button: .init(fill: Color(hex: 0x007ACC), ink: .white, mark: "AgentLogo/vscode-on-blue"))
        case .other:
            // No maker to speak for, so the house accent.
            return AgentBrand(
                logo: nil, symbolTile: nil, button: .init(fill: Palette.accent, ink: .white, mark: nil))
        }
    }
}

/// An agent's mark at a given size, with clear space inside the frame, or its
/// stand-in symbol.
struct AgentMark: View {
    let kind: AgentKind
    let size: CGFloat

    static func hasAsset(_ name: String) -> Bool {
        #if os(macOS)
            NSImage(named: name) != nil
        #else
            UIImage(named: name) != nil
        #endif
    }

    var body: some View {
        let brand = kind.brand
        Group {
            if let logo = brand.logo, Self.hasAsset(logo) {
                Image(logo)
                    .resizable()
                    .scaledToFit()
                    .frame(width: size * 0.8, height: size * 0.8)
            } else if let tile = brand.symbolTile {
                Image(systemName: kind.symbol)
                    .font(.system(size: size * 0.4, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: size * 0.8, height: size * 0.8)
                    .background(
                        RoundedRectangle(cornerRadius: size * 0.2, style: .continuous).fill(tile)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: size * 0.2, style: .continuous)
                            .strokeBorder(Palette.hairline, lineWidth: 0.75)
                    )
            } else {
                Image(systemName: kind.symbol)
                    .font(.system(size: size * 0.5, weight: .medium))
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// A button in an agent's own colours. The house's press and hover feel: a
/// light wash over the fill on hover, a dip in opacity on press, and a hairline
/// all the way round so black reads on a dark window.
private struct BrandButtonStyle: ButtonStyle {
    let fill: Color
    let ink: Color
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let shape = RoundedRectangle(cornerRadius: 8, style: .continuous)
        return configuration.label
            .font(Typo.ui(12, weight: .semibold))
            .foregroundStyle(ink)
            .padding(.horizontal, Space.s12)
            .padding(.vertical, Space.scaled(7))
            .background(
                ZStack {
                    shape.fill(fill)
                    shape.fill(Color.white.opacity(hovering ? 0.1 : 0))
                }
            )
            .overlay(shape.strokeBorder(Palette.hairlineStrong, lineWidth: 0.75))
            .opacity(configuration.isPressed ? 0.8 : 1)
            .contentShape(shape)
            .onHover { hovering = $0 }
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
