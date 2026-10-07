// Connecting an outside agent to the mailbox: where the agent door is, whether
// it answers, and what each agent wants pasted into it.
//
// THE AGENT DOOR IS NOT THE APP'S DOOR. This app talks to `/client/*` with a
// bearer token; an agent talks to `/mcp` with a credential of its own or with
// nothing at all, never the app's. A self-hosted daemon's only guard on `/mcp`
// is the Host header (loopback, plus SQUELCH_MCP_ALLOWED_HOSTS). A hosted
// mailbox either does not publish the path at all or serves it behind OAuth:
// the agent gets a 401 naming where to sign in, opens a browser, the person
// signs in with the Google account of the mailbox and approves it. The token
// that comes out of that lives in the agent. This app never sees it, mints it
// or holds it, so there is nothing here to copy and nothing here to revoke.
// So "is the agent door open" is a question only a request can answer, and the
// probe below asks it the way an agent would: an MCP `initialize`, with NO
// Authorization header, because an agent will not have one either.
//
// Settings' Agents pane reads `AgentConnect.shared`. It is a shared object
// rather than view state so the answer outlives leaving the pane.

import Foundation
import Observation

/// What a probe of `<server>/mcp` found.
enum AgentDoorStatus: Equatable, Sendable {
    /// Not asked yet, or not askable (no server, the practice inbox).
    case unknown
    case checking
    /// Answered an MCP `initialize`. An agent that can reach this address can
    /// connect.
    case open
    /// 401 with `WWW-Authenticate: Bearer resource_metadata=...`: the door is
    /// served behind MCP OAuth (a hosted mailbox). An agent connects with the
    /// address alone and signs in through a browser. A 401 WITHOUT the
    /// metadata pointer is not this: nothing an agent could follow, so it is
    /// filed with the other answers this code does not recognise.
    case signInRequired
    /// 403: the daemon is there and refused the Host name. It answers agents
    /// only by the names in SQUELCH_MCP_ALLOWED_HOSTS (and localhost).
    case hostRefused
    /// 404/410: nothing is mounted at `/mcp` here. What a hosted mailbox says
    /// today, since its Ingress publishes the human door only.
    case notServed
    /// No answer, or one this code does not recognise.
    case unreachable

    /// Which set of instructions fits this door.
    var auth: AgentDoorAuth { self == .signInRequired ? .signIn : .none }
}

/// How an agent gets through the door, which decides what the setup card says.
/// `.none` is the self-hosted door (no auth, Host guard only) and its snippets
/// are exactly what they always were. `.signIn` is the OAuth door: the address
/// only, then the agent's own sign-in step.
enum AgentDoorAuth: Equatable, Sendable {
    case none, signIn
}

/// An agent somebody might want to connect, and how that agent spells "add an
/// MCP server". Every snippet is the agent's own documented form, with the
/// daemon's real address already in it: a snippet with a placeholder is a
/// snippet somebody pastes with the placeholder still in. The same goes for the
/// sign-in door: each form below is the one that agent's docs give for a remote
/// server that uses OAuth, and none of them carries a token, because there is
/// no token to carry. Where an agent's docs were checked:
///   Claude Code  code.claude.com/docs/en/mcp (add --transport http; mcp login)
///   Codex        learn.chatgpt.com/docs/extend/mcp (mcp add --url; mcp login)
///   Cursor       cursor.com/docs/context/mcp (url in mcp.json; OAuth by DCR)
///   Claude       claude.com/docs/connectors/custom/remote-mcp (custom connector)
///   OpenClaw     docs.openclaw.ai/cli/mcp/registry (add --auth oauth) and
///                docs.openclaw.ai/cli/mcp/transports (mcp login)
///   Hermes Agent hermes-agent.nousresearch.com/docs/user-guide/features/mcp and
///                .../docs/reference/mcp-config-reference (mcp_servers, auth:
///                oauth, mcp login, mcp test, /reload-mcp)
///   ChatGPT      developers.openai.com/api/docs/guides/custom-mcp-server (web
///                only; Plugins, Add custom MCP server; no prefilled link)
///   VS Code      code.visualstudio.com/docs/copilot/reference/mcp-configuration
///                (servers, type http) and .../guides/mcp-developer-guide
///                (the vscode:mcp/install link)
///   Muse         meta.com/help/artificial-intelligence/1687253048996149 (ask
///                Muse to create a Custom Connector; Meta does not review
///                them). Meta's page gives no wording and never says MCP. That
///                Muse takes a streamable HTTP address with OAuth this way is
///                from server makers' own Muse guides (sealgate.ai/docs/
///                connect-clients/muse, wave.co/integrations/muse), so the
///                prompt below is theirs in shape, not Meta's.
/// And the install links, in `action(endpoint:auth:)`:
///   Claude       claude.com/docs/connectors/building/directory-vs-custom
///   Cursor       cursor.com/docs/context/mcp/install-links
enum AgentKind: String, CaseIterable, Identifiable, Sendable {
    case claude, openClaw, hermes, muse, chatGPT, claudeCode, codex, cursor, vsCode, claudeDesktop, other

    /// The agents worth offering at a given door, in chip order. The first is
    /// the one the pane opens on.
    ///
    /// Claude, Muse and ChatGPT reach a server from their makers' machines, not
    /// this one, so they cannot see a loopback or tailnet address and are
    /// offered only at the sign-in door, which is a public one.
    ///
    /// The sign-in door drops Claude Desktop's mcp-remote bridge for the
    /// connector, which is Claude's own route to a remote server with OAuth
    /// and works in claude.ai and the apps alike.
    static func shown(for auth: AgentDoorAuth) -> [AgentKind] {
        switch auth {
        case .none: [.openClaw, .hermes, .claudeCode, .codex, .cursor, .vsCode, .claudeDesktop, .other]
        case .signIn: [.claude, .openClaw, .hermes, .muse, .chatGPT, .claudeCode, .codex, .cursor, .vsCode, .other]
        }
    }

    /// The chip a door opens on.
    static func defaultChoice(for auth: AgentDoorAuth) -> AgentKind { shown(for: auth)[0] }

    var id: String { rawValue }

    var name: String {
        switch self {
        case .openClaw: "OpenClaw"
        case .hermes: "Hermes Agent"
        case .muse: "Muse"
        case .chatGPT: "ChatGPT"
        case .vsCode: "VS Code"
        case .claudeCode: "Claude Code"
        case .codex: "Codex"
        case .cursor: "Cursor"
        case .claudeDesktop: "Claude Desktop"
        case .claude: "Claude"
        case .other: "Other"
        }
    }

    var symbol: String {
        switch self {
        case .openClaw: "pawprint"
        case .hermes: "paperplane"
        case .muse: "sparkles"
        case .chatGPT: "message"
        case .vsCode: "curlybraces"
        case .claudeCode: "terminal"
        case .codex: "chevron.left.forwardslash.chevron.right"
        case .cursor: "cursorarrow.rays"
        case .claudeDesktop: "macwindow"
        case .claude: "bubble.left.and.bubble.right"
        case .other: "puzzlepiece.extension"
        }
    }

    /// Where the snippet goes, said before the snippet.
    func instruction(for auth: AgentDoorAuth) -> String {
        switch (self, auth) {
        case (.claude, _), (.claudeDesktop, .signIn):
            "Add to Claude opens Claude's Add custom connector dialog with Passband filled in. Check it and select Add. To do it by hand, open Customize, then Connectors, and paste this address. On a Team or Enterprise plan, an owner adds it under Organization settings first, then each member connects."
        case (.hermes, .signIn):
            "Add this to ~/.hermes/config.yaml, inside mcp_servers if you already have one. Sign in from a terminal, then run /reload-mcp in Hermes."
        default: instruction
        }
    }

    /// The open door's instruction, unchanged since before the sign-in door.
    var instruction: String {
        switch self {
        case .openClaw: "Run this on the machine your OpenClaw gateway runs on."
        case .claudeCode: "Run this in a terminal. Add --scope user to have it in every project."
        case .codex: "Run this in a terminal. The CLI and the IDE extension share it."
        case .cursor: "Add this to ~/.cursor/mcp.json, or to .cursor/mcp.json in one project."
        case .claudeDesktop:
            "Add this to claude_desktop_config.json (Settings, Developer, Edit Config), then restart Claude. It needs Node for npx."
        case .claude: instruction(for: .signIn)
        case .hermes:
            "Add this to ~/.hermes/config.yaml, inside mcp_servers if you already have one, then run /reload-mcp in Hermes."
        case .muse:
            "Send this to Muse in a new chat. Muse builds the connector itself, then shows a link to sign in to Passband. Meta does not review custom connectors."
        case .chatGPT:
            "On chatgpt.com, open Plugins, select the plus button and choose Add custom MCP server. Paste this address as the Server URL, choose OAuth, and select Create as a plugin. Then install it from your personal plugins. In a workspace, an admin can turn custom servers off."
        case .vsCode: "Add this to .vscode/mcp.json in one project, or run MCP: Add Server from the Command Palette."
        case .other: "Most MCP clients take a streamable HTTP server in this shape."
        }
    }

    /// The agent's own command for starting its sign-in, run after the
    /// snippet. Only at the sign-in door, and only where the agent's docs name
    /// one: Cursor and Claude sign in from their own UI when they first
    /// connect, and "Other" cannot be spoken for.
    func signIn(for auth: AgentDoorAuth) -> String? {
        guard auth == .signIn else { return nil }
        switch self {
        case .openClaw: return "openclaw mcp login passband"
        case .claudeCode: return "claude mcp login passband"
        case .codex: return "codex mcp login passband"
        case .hermes: return "hermes mcp login passband"
        case .cursor, .vsCode, .claudeDesktop, .claude, .muse, .chatGPT, .other: return nil
        }
    }

    /// How to see it worked, when the agent has a way to say so.
    var check: String? {
        switch self {
        case .openClaw: "openclaw mcp doctor passband --probe"
        case .claudeCode: "claude mcp list"
        case .codex: "codex mcp list"
        case .hermes: "hermes mcp test passband"
        default: nil
        }
    }

    /// Whether `snippet` is a shell command (one line, run it) or config
    /// (a file fragment, paste it).
    var isCommand: Bool {
        switch self {
        case .openClaw, .claudeCode, .codex: true
        case .hermes, .cursor, .vsCode, .claudeDesktop, .claude, .muse, .chatGPT, .other: false
        }
    }

    /// Whether `snippet` is the address alone (a form field takes it) rather
    /// than a command or a config fragment.
    func snippetIsAddress(auth: AgentDoorAuth) -> Bool {
        switch self {
        case .claude, .chatGPT: true
        case .claudeDesktop: auth == .signIn
        default: false
        }
    }

    /// Whether `snippet` is a message to send the agent itself, for an agent
    /// that is set up by being asked.
    var snippetIsPrompt: Bool { self == .muse }

    /// Whether the agent's install link opens a desktop app. A phone has no
    /// Cursor or VS Code to open, and a custom scheme nothing claims does
    /// nothing at all, so the phone gets the copy button instead.
    var installOpensDesktopApp: Bool {
        switch self {
        case .cursor, .vsCode: true
        default: false
        }
    }

    /// The one button under the instruction: the agent's own install link
    /// where its docs give one, otherwise a copy of the snippet. A link that
    /// only prefills is still the person's to confirm in the agent.
    func action(endpoint url: String, auth: AgentDoorAuth) -> AgentAction {
        switch self {
        case .claude where auth == .signIn:
            return AgentAction(
                label: "Add to Claude", note: nil,
                kind: .open(Self.claudeInstallLink(endpoint: url)))
        case .cursor:
            return AgentAction(
                label: "Add to Cursor", note: "opens Cursor and asks it to add Passband",
                kind: .open(Self.cursorInstallLink(endpoint: url)))
        case .vsCode:
            return AgentAction(
                label: "Add to VS Code", note: "opens VS Code and asks it to add Passband",
                kind: .open(Self.vsCodeInstallLink(endpoint: url)))
        case .chatGPT:
            // ChatGPT has no prefilled link, so the next best thing is the
            // address on the pasteboard and the page it gets pasted into.
            return AgentAction(
                label: "Copy and open ChatGPT", note: "copies this address and opens ChatGPT Plugins",
                kind: .copyAndOpen(Self.chatGPTPlugins))
        default:
            return copyAction(auth: auth)
        }
    }

    /// The copy fallback, which is also what a phone gets for an agent whose
    /// link opens a desktop app.
    func copyAction(auth: AgentDoorAuth) -> AgentAction {
        let label =
            isCommand
            ? "Copy command"
            : snippetIsAddress(auth: auth) ? "Copy address" : snippetIsPrompt ? "Copy prompt" : "Copy config"
        return AgentAction(label: label, note: nil, kind: .copy)
    }

    /// `encodeURIComponent`: everything but A-Z a-z 0-9 and -_.!~*'() is
    /// percent-encoded, which is what the Claude, Cursor and VS Code link
    /// docs each say to apply.
    nonisolated static func encodeURIComponent(_ text: String) -> String {
        var allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")
        allowed.insert(charactersIn: "-_.!~*'()")
        return text.addingPercentEncoding(withAllowedCharacters: allowed) ?? text
    }

    /// Compact JSON with keys in a fixed order and slashes left alone, the
    /// shape `JSON.stringify` gives for these flat objects.
    nonisolated static func compactJSON(_ object: [String: String]) -> String {
        let data =
            (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes]))
            ?? Data()
        return String(decoding: data, as: UTF8.self)
    }

    /// Claude's Add custom connector dialog, prefilled. The person reviews
    /// the values in claude.ai and confirms; nothing is added by the link.
    nonisolated static func claudeInstallLink(endpoint url: String) -> URL {
        URL(
            string: "https://claude.ai/customize/connectors?modal=add-custom-connector&connectorName=Passband&connectorUrl="
                + encodeURIComponent(url))!
    }

    /// Cursor's MCP install deeplink. The config is the value that would sit
    /// under the server's name in mcp.json, base64 encoded. Built the way
    /// Cursor's own link generator builds it: `+` in the base64 becomes
    /// `%2B` first, then the whole value is URI-component encoded.
    nonisolated static func cursorInstallLink(endpoint url: String) -> URL {
        let config = Data(compactJSON(["url": url]).utf8).base64EncodedString()
            .replacingOccurrences(of: "+", with: "%2B")
        return URL(
            string: "cursor://anysphere.cursor-deeplink/mcp/install?name=passband&config="
                + encodeURIComponent(config))!
    }

    /// VS Code's MCP install link: the `--add-mcp` object, stringified and
    /// URI-component encoded, as the whole query.
    nonisolated static func vsCodeInstallLink(endpoint url: String) -> URL {
        URL(
            string: "vscode:mcp/install?"
                + encodeURIComponent(compactJSON(["name": "passband", "type": "http", "url": url])))!
    }

    /// Where a custom MCP server is added in ChatGPT, on the web.
    nonisolated static let chatGPTPlugins = URL(string: "https://chatgpt.com/plugins")!

    /// What to paste for this door. At the sign-in door it is the address and
    /// nothing else: no header, no key, no placeholder for one.
    func snippet(endpoint url: String, auth: AgentDoorAuth) -> String {
        guard auth == .signIn else { return snippet(endpoint: url) }
        switch self {
        case .openClaw:
            // `--auth oauth` saves the server as OAuth so `mcp login` can sign
            // it in; `add` skips its probe while authorization is pending.
            return "openclaw mcp add passband --url \(url) --transport streamable-http --auth oauth"
        case .claude, .chatGPT:
            // A custom connector takes the address in a form field, so the
            // address is the whole snippet.
            return url
        case .hermes:
            // The open door's block plus the line that makes Hermes run the
            // OAuth flow for it.
            return snippet(endpoint: url) + "\n    auth: oauth"
        case .claudeDesktop:
            // Not offered at this door (see `shown(for:)`), but if asked, the
            // connector form is still the honest answer rather than a bridge
            // config that would have to carry the sign-in itself.
            return url
        case .claudeCode, .codex, .cursor, .vsCode, .muse, .other:
            // Already the address alone; each signs in on the 401.
            return snippet(endpoint: url)
        }
    }

    /// The open door's snippet, unchanged since before the sign-in door.
    func snippet(endpoint url: String) -> String {
        switch self {
        case .openClaw:
            return "openclaw mcp add passband --url \(url) --transport streamable-http"
        case .claudeCode:
            return "claude mcp add --transport http passband \(url)"
        case .codex:
            return "codex mcp add passband --url \(url)"
        case .cursor:
            return """
                {
                  "mcpServers": {
                    "passband": { "url": "\(url)" }
                  }
                }
                """
        case .claudeDesktop:
            // Claude Desktop's config file launches stdio servers only, so a
            // remote one goes through the mcp-remote bridge. The bridge refuses
            // plain http to anything but localhost unless told otherwise.
            var args = ["\"-y\"", "\"mcp-remote\"", "\"\(url)\""]
            if AgentConnect.isPlainHTTPToRemoteHost(url) { args.append("\"--allow-http\"") }
            return """
                {
                  "mcpServers": {
                    "passband": {
                      "command": "npx",
                      "args": [\(args.joined(separator: ", "))]
                    }
                  }
                }
                """
        case .claude, .chatGPT:
            return url
        case .muse:
            // Muse has no field for an address: it is asked, and writes its
            // own client. The lines are the questions it otherwise asks back.
            // Not offered at the open door (see `shown(for:)`), where Meta's
            // machines could not reach the address anyway.
            return """
                Create a Custom Connector for a remote MCP server, then connect to it.
                Name: Passband
                Transport: streamable HTTP
                URL: \(url)
                Sign-in: OAuth. Show me the sign-in link when you have it.
                Then list its tools to confirm it works.
                """
        case .hermes:
            return """
                mcp_servers:
                  passband:
                    url: "\(url)"
                """
        case .vsCode:
            return """
                {
                  "servers": {
                    "passband": { "type": "http", "url": "\(url)" }
                  }
                }
                """
        case .other:
            return """
                {
                  "mcpServers": {
                    "passband": { "type": "http", "url": "\(url)" }
                  }
                }
                """
        }
    }
}

/// What the button under an agent's instruction does.
struct AgentAction: Equatable, Sendable {
    enum Kind: Equatable, Sendable {
        /// Open the agent's install link.
        case open(URL)
        /// Put the snippet on the pasteboard, then open where it is pasted.
        case copyAndOpen(URL)
        /// Put the snippet on the pasteboard.
        case copy
    }

    let label: String
    /// A few words beside the button on what it does, for a link.
    let note: String?
    let kind: Kind
}

@MainActor
@Observable
final class AgentConnect {
    static let shared = AgentConnect()

    /// The last probe's answer, and the server it was asked of. Keyed so that
    /// switching accounts cannot show one daemon's answer under another's URL.
    private(set) var status: AgentDoorStatus = .unknown
    private(set) var statusFor: String?

    /// Bumped by every check, so only the newest one may write its answer.
    /// Keying on the endpoint alone is not enough: A, then B, then A again
    /// leaves the first A probe looking current when it lands.
    private var generation = 0

    // MARK: - the endpoint

    /// `<server>/mcp`, from the address this app already talks to. Nil with no
    /// server configured.
    nonisolated static func endpoint(serverURL: String?) -> String? {
        guard var base = serverURL?.trimmingCharacters(in: .whitespacesAndNewlines),
            !base.isEmpty
        else { return nil }
        while base.hasSuffix("/") { base.removeLast() }
        return base + "/mcp"
    }

    /// The host an agent will put in its Host header, which is what the daemon
    /// checks against SQUELCH_MCP_ALLOWED_HOSTS.
    nonisolated static func host(of url: String) -> String? {
        URLComponents(string: url)?.host
    }

    nonisolated static func isLoopback(_ url: String) -> Bool {
        guard let host = host(of: url)?.lowercased() else { return false }
        return host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
    }

    /// Whether mcp-remote needs `--allow-http` for this URL. Its exemption is
    /// narrower than `isLoopback`: only `localhost` and `127.0.0.1`, so an
    /// `http://[::1]` address still needs the flag.
    nonisolated static func isPlainHTTPToRemoteHost(_ url: String) -> Bool {
        guard url.lowercased().hasPrefix("http://") else { return false }
        let host = host(of: url)?.lowercased()
        return host != "localhost" && host != "127.0.0.1"
    }

    // MARK: - hosted mailboxes

    /// What a hosted mailbox says when its door answers 403 or not at all (a
    /// 5xx included, which `classify` files as `.unreachable`). The self-host
    /// copy for those answers tells somebody to edit the daemon's environment
    /// or check it is running; a hosted person can do neither, and
    /// SQUELCH_MCP_ALLOWED_HOSTS must never be set on hosted at all. Mid
    /// rollout a hosted door can say either, so this is the one honest line.
    nonisolated static let hostedUnavailable =
        "Agent MCP isn't available for this mailbox right now. Try again later, and contact support if it stays that way."

    /// Whether `status` from a hosted mailbox is one of those answers.
    nonisolated static func isHostedOutage(_ status: AgentDoorStatus, hosted: Bool) -> Bool {
        hosted && (status == .hostRefused || status == .unreachable)
    }

    /// Whether the setup and capability cards belong under this answer. A
    /// self-hosted daemon shows them for anything but "not served", since its
    /// owner is about to fix whatever the status says. A hosted mailbox shows
    /// them only once the door has said how it is entered: the self-host
    /// snippets and their "no password of its own" line are wrong there, and
    /// there is nothing to set up while it is down.
    nonisolated static func showsSetup(_ status: AgentDoorStatus, hosted: Bool) -> Bool {
        if hosted { return status == .signInRequired || status == .open }
        return status != .notServed
    }

    // MARK: - the probe

    /// Ask the door. Safe to call repeatedly; the newest call's answer wins.
    func check(serverURL: String?) async {
        generation &+= 1
        let mine = generation
        guard !RehearsalMode.isEnabled, let endpoint = Self.endpoint(serverURL: serverURL) else {
            status = .unknown
            statusFor = nil
            return
        }
        status = .checking
        statusFor = endpoint
        let found = await Self.probe(endpoint)
        // A newer check started meanwhile, or this one was cancelled (the
        // pane went away): either way this answer is not the one to show.
        guard generation == mine, let found else { return }
        status = found
    }

    /// The status for this endpoint, or `.unknown` if the last probe was of a
    /// different one.
    func status(for endpoint: String?) -> AgentDoorStatus {
        guard let endpoint, endpoint == statusFor else { return .unknown }
        return status
    }

    /// One MCP `initialize`, the first thing any agent sends. Deliberately NOT
    /// through APIClient: that would attach the app's bearer token, and a door
    /// that only opens for the token is a door no agent can use.
    /// Nil when the probe was cancelled, which is not an answer about the door.
    nonisolated static func probe(_ endpoint: String) async -> AgentDoorStatus? {
        guard let url = URL(string: endpoint) else { return .unreachable }
        var req = URLRequest(url: url, timeoutInterval: 8)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("application/json, text/event-stream", forHTTPHeaderField: "Accept")
        req.httpBody = Data(
            #"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"passband-probe","version":"1"}}}"#
                .utf8)
        let session = URLSession(configuration: .ephemeral)
        defer { session.finishTasksAndInvalidate() }
        do {
            let (data, response) = try await session.data(for: req)
            guard let http = response as? HTTPURLResponse else { return .unreachable }
            // Leave nothing behind: the daemon keeps a session per initialize
            // until told otherwise. Only a 2xx opened one; a 401 never did.
            if (200..<300).contains(http.statusCode),
                let id = http.value(forHTTPHeaderField: "Mcp-Session-Id")
            {
                var close = URLRequest(url: url, timeoutInterval: 4)
                close.httpMethod = "DELETE"
                close.setValue(id, forHTTPHeaderField: "Mcp-Session-Id")
                _ = try? await session.data(for: close)
            }
            return classify(
                statusCode: http.statusCode,
                wwwAuthenticate: http.value(forHTTPHeaderField: "WWW-Authenticate"),
                body: data)
        } catch {
            if Task.isCancelled || (error as? URLError)?.code == .cancelled { return nil }
            return .unreachable
        }
    }

    /// What one answer to the probe means. Pure, so the mapping is asserted in
    /// a test rather than discovered against a live daemon.
    nonisolated static func classify(statusCode: Int, wwwAuthenticate: String?, body: Data) -> AgentDoorStatus {
        switch statusCode {
        case 200..<300:
            // A 200 from something that is not an MCP server (a catch-all
            // web page) is not an open door.
            let text = String(decoding: body, as: UTF8.self)
            return text.contains("\"protocolVersion\"") ? .open : .unreachable
        case 401:
            // Only the MCP OAuth answer counts: a Bearer challenge that points
            // at protected resource metadata, which is what an agent follows
            // to find where to sign in. Any other 401 (a proxy's basic auth, a
            // stray login wall) is not a door an agent can use.
            return isMCPOAuthChallenge(wwwAuthenticate) ? .signInRequired : .unreachable
        case 403: return .hostRefused
        case 404, 410: return .notServed
        default: return .unreachable
        }
    }

    /// Whether a `WWW-Authenticate` value is a Bearer challenge carrying a
    /// `resource_metadata` parameter (RFC 9728 section 5.1). Case-insensitive
    /// on the scheme and the parameter name, as both RFCs say. URLSession joins
    /// repeated headers with commas, so any challenge in the value may carry it.
    nonisolated static func isMCPOAuthChallenge(_ header: String?) -> Bool {
        guard let header = header?.lowercased() else { return false }
        guard header.range(of: #"(^|[\s,])bearer(\s|$)"#, options: .regularExpression) != nil else {
            return false
        }
        return header.range(of: #"(^|[\s,])resource_metadata\s*=\s*"?[^\s",]+"#, options: .regularExpression) != nil
    }
}
