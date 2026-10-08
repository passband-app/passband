import Foundation

// The Agents pane's two jobs that can be wrong without looking wrong: reading
// the probe's answer (an arbitrary 401 must not pass for a sign-in door), and
// the text it hands somebody to paste (a snippet with a token slot or a
// placeholder in it gets pasted with the slot still in).

@main
@MainActor
struct AgentConnectTests {
    static var checks = 0

    static func expect(_ condition: Bool, _ message: String) {
        checks += 1
        if !condition {
            print("FAIL: \(message)")
            exit(1)
        }
    }

    static let metadata =
        #"Bearer resource_metadata="https://alice.passband.email/.well-known/oauth-protected-resource/mcp""#

    static func main() {
        classifies()
        signInSnippets()
        openSnippetsUnchanged()
        noEmDashesInCopy()
        hostedMailboxes()
        accessAndTeamCopy()
        chipOrder()
        installLinks()
        newAgentSnippets()
        brandAssets()
        print("ok: \(checks) agent connect checks passed")
    }

    static func classifies() {
        let none = Data()
        expect(
            AgentConnect.classify(statusCode: 401, wwwAuthenticate: metadata, body: none) == .signInRequired,
            "a 401 pointing at protected resource metadata is the sign-in door")
        expect(
            AgentConnect.classify(
                statusCode: 401, wwwAuthenticate: metadata + #", error="invalid_token""#, body: none)
                == .signInRequired,
            "the invalid_token form of the same challenge is still the sign-in door")
        expect(
            AgentConnect.classify(
                statusCode: 401,
                wwwAuthenticate: #"bearer RESOURCE_METADATA="https://a.example/.well-known/oauth-protected-resource""#,
                body: none) == .signInRequired,
            "scheme and parameter names are case-insensitive")
        expect(
            AgentConnect.classify(
                statusCode: 401, wwwAuthenticate: #"Basic realm="x", Bearer resource_metadata="https://a.example/m""#,
                body: none) == .signInRequired,
            "a joined header with a Bearer challenge among others still counts")

        for bare: String? in [
            nil, "", "Bearer", #"Bearer realm="passband""#, #"Basic realm="proxy""#,
            #"Basic realm="resource_metadata""#, #"Bearer error="invalid_token""#,
            #"Basic resource_metadata="https://a.example/m""#,
            #"Bearer resource_metadata="""#,
        ] {
            let found = AgentConnect.classify(statusCode: 401, wwwAuthenticate: bare, body: none)
            expect(found != .signInRequired, "a 401 with \(bare ?? "no header") is not a sign-in door")
            expect(found == .unreachable, "a bare 401 lands where unrecognised answers land")
        }
        expect(
            AgentConnect.classify(statusCode: 403, wwwAuthenticate: metadata, body: none) == .hostRefused,
            "the challenge header does not turn a Host refusal into a sign-in door")

        let initialize = Data(#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}"#.utf8)
        expect(AgentConnect.classify(statusCode: 200, wwwAuthenticate: nil, body: initialize) == .open, "200 + MCP is open")
        expect(
            AgentConnect.classify(statusCode: 200, wwwAuthenticate: nil, body: Data("<html>".utf8)) == .unreachable,
            "200 from a web page is not open")
        expect(AgentConnect.classify(statusCode: 403, wwwAuthenticate: nil, body: none) == .hostRefused, "403")
        expect(AgentConnect.classify(statusCode: 404, wwwAuthenticate: nil, body: none) == .notServed, "404")
        expect(AgentConnect.classify(statusCode: 410, wwwAuthenticate: nil, body: none) == .notServed, "410")
        expect(AgentConnect.classify(statusCode: 503, wwwAuthenticate: nil, body: none) == .unreachable, "503")

        expect(AgentDoorStatus.signInRequired.auth == .signIn, "the sign-in door gets sign-in setup")
        for other: AgentDoorStatus in [.unknown, .checking, .open, .hostRefused, .notServed, .unreachable] {
            expect(other.auth == .none, "\(other) keeps the open door's setup")
        }
    }

    static func signInSnippets() {
        let url = "https://alice.passband.email/mcp"
        let kinds = AgentKind.shown(for: .signIn)
        expect(kinds.contains(.claude), "the sign-in door offers the Claude connector")
        expect(!kinds.contains(.claudeDesktop), "the sign-in door drops the mcp-remote bridge")
        expect(!AgentKind.shown(for: .none).contains(.claude), "the open door does not offer a connector")

        let forbidden = ["token", "bearer", "authorization", "api_key", "apikey", "secret", "<", ">", "your-", "xxx", "..."]
        for kind in AgentKind.allCases {
            let texts = [kind.snippet(endpoint: url, auth: .signIn), kind.signIn(for: .signIn) ?? ""]
            expect(texts[0].contains(url), "\(kind) sign-in snippet carries the real address")
            for text in texts {
                for word in forbidden {
                    expect(
                        !text.lowercased().contains(word),
                        "\(kind) sign-in text has no credential slot or placeholder (\(word)): \(text)")
                }
            }
            expect(kind.signIn(for: .none) == nil, "\(kind) has no sign-in step at the open door")
            expect(!kind.instruction(for: .signIn).isEmpty, "\(kind) says where its snippet goes")
        }
        expect(
            AgentKind.claudeCode.snippet(endpoint: url, auth: .signIn)
                == "claude mcp add --transport http passband \(url)",
            "Claude Code: the documented http form")
        expect(AgentKind.claudeCode.signIn(for: .signIn) == "claude mcp login passband", "Claude Code login")
        expect(AgentKind.codex.snippet(endpoint: url, auth: .signIn) == "codex mcp add passband --url \(url)", "Codex add")
        expect(AgentKind.codex.signIn(for: .signIn) == "codex mcp login passband", "Codex login")
        expect(
            AgentKind.openClaw.snippet(endpoint: url, auth: .signIn)
                == "openclaw mcp add passband --url \(url) --transport streamable-http --auth oauth",
            "OpenClaw saves the server as OAuth")
        expect(AgentKind.openClaw.signIn(for: .signIn) == "openclaw mcp login passband", "OpenClaw login")
        expect(AgentKind.claude.snippet(endpoint: url, auth: .signIn) == url, "a connector takes the address alone")
        expect(AgentKind.cursor.signIn(for: .signIn) == nil, "Cursor signs in from its own UI")
        expect(AgentKind.chatGPT.snippet(endpoint: url, auth: .signIn) == url, "ChatGPT takes the address alone")
        expect(AgentKind.hermes.signIn(for: .signIn) == "hermes mcp login passband", "Hermes login")
        expect(AgentKind.vsCode.signIn(for: .signIn) == nil, "VS Code signs in from its own UI")
    }

    /// The self-hosted door's text, pinned to what it was before the sign-in
    /// door existed.
    static func openSnippetsUnchanged() {
        let url = "http://box.tailnet.ts.net:8848/mcp"
        expect(
            AgentKind.openClaw.snippet(endpoint: url, auth: .none)
                == "openclaw mcp add passband --url \(url) --transport streamable-http",
            "OpenClaw open form")
        expect(
            AgentKind.claudeCode.snippet(endpoint: url, auth: .none) == "claude mcp add --transport http passband \(url)",
            "Claude Code open form")
        expect(AgentKind.codex.snippet(endpoint: url, auth: .none) == "codex mcp add passband --url \(url)", "Codex open form")
        expect(
            AgentKind.cursor.snippet(endpoint: url, auth: .none) == """
                {
                  "mcpServers": {
                    "passband": { "url": "\(url)" }
                  }
                }
                """, "Cursor open form")
        expect(
            AgentKind.claudeDesktop.snippet(endpoint: url, auth: .none) == """
                {
                  "mcpServers": {
                    "passband": {
                      "command": "npx",
                      "args": ["-y", "mcp-remote", "\(url)", "--allow-http"]
                    }
                  }
                }
                """, "Claude Desktop open form, with --allow-http for plain http off loopback")
        expect(
            AgentKind.other.snippet(endpoint: url, auth: .none) == """
                {
                  "mcpServers": {
                    "passband": { "type": "http", "url": "\(url)" }
                  }
                }
                """, "generic open form")
        for kind in AgentKind.shown(for: .none) {
            expect(kind.instruction(for: .none) == kind.instruction, "\(kind) open instruction unchanged")
        }
        expect(
            AgentKind.claudeDesktop.instruction
                == "Add this to claude_desktop_config.json (Settings, Developer, Edit Config), then restart Claude. It needs Node for npx.",
            "Claude Desktop open instruction")
    }

    /// A standing rule for this app's copy. Scans every string literal in the
    /// pane and its model, so a new line cannot slip one in.
    static func noEmDashesInCopy() {
        let files = [
            "Sources/Passband/Model/AgentConnect.swift",
            "Sources/Passband/Views/AgentsSection.swift",
            "Sources/Passband/Lib/HostedServer.swift",
        ]
        let literal = try! NSRegularExpression(pattern: #""[^"\n]*""#)
        for path in files {
            guard let source = try? String(contentsOfFile: path, encoding: .utf8) else {
                expect(false, "read \(path) (run from passband/)")
                return
            }
            let range = NSRange(source.startIndex..., in: source)
            var literals = 0
            for match in literal.matches(in: source, range: range) {
                literals += 1
                let text = String(source[Range(match.range, in: source)!])
                expect(!text.contains("\u{2014}"), "no em dash in \(path): \(text)")
            }
            expect(literals > 0, "\(path) was actually scanned")
        }
        for kind in AgentKind.allCases {
            for auth in [AgentDoorAuth.none, .signIn] {
                let copy = [kind.name, kind.instruction(for: auth), kind.signIn(for: auth) ?? ""]
                expect(!copy.joined().contains("\u{2014}"), "no em dash in \(kind) copy")
            }
        }
    }

    /// A hosted person is never handed a daemon remedy. 403 and "no answer"
    /// (a 5xx lands there) read as one honest outage line, and the self-host
    /// snippets stay off screen until the door says how it is entered.
    static func hostedMailboxes() {
        expect(HostedServer.isHosted(url: "https://alice.passband.email"), "a tenant host is hosted")
        expect(HostedServer.isHosted(url: "https://Alice.Passband.Email/"), "hosts compare case-insensitively")
        expect(HostedServer.isHosted(host: "alice.passband.email"), "the connect screen's host form")
        for selfHost in [
            "http://127.0.0.1:8848", "http://box.tailnet.ts.net:8848", "https://mail.example.com",
            "https://notpassband.email", "https://alice.passband.email.example.com", "", "not a url",
        ] {
            expect(!HostedServer.isHosted(url: selfHost), "\(selfHost) is not hosted")
        }
        expect(
            AgentConnect.classify(statusCode: 503, wwwAuthenticate: nil, body: Data()) == .unreachable,
            "a 503 is the no-answer case, which carries the hosted wording")

        for status: AgentDoorStatus in [.hostRefused, .unreachable] {
            expect(AgentConnect.isHostedOutage(status, hosted: true), "hosted \(status) is the outage line")
            expect(!AgentConnect.isHostedOutage(status, hosted: false), "self-hosted \(status) keeps its remedy")
            expect(!AgentConnect.showsSetup(status, hosted: true), "no self-host snippets for hosted \(status)")
            expect(AgentConnect.showsSetup(status, hosted: false), "self-hosted \(status) still shows setup")
        }
        for status: AgentDoorStatus in [.unknown, .checking, .open, .signInRequired, .notServed] {
            expect(!AgentConnect.isHostedOutage(status, hosted: true), "hosted \(status) is not an outage")
        }
        expect(AgentConnect.showsSetup(.signInRequired, hosted: true), "hosted sign-in door shows setup")
        for status: AgentDoorStatus in [.unknown, .checking, .notServed] {
            expect(!AgentConnect.showsSetup(status, hosted: true), "hosted \(status) shows no setup yet")
        }
        expect(!AgentConnect.showsSetup(.notServed, hosted: false), "not served shows no setup")
        let line = AgentConnect.hostedUnavailable
        expect(line.contains("contact support"), "the outage line says where to go")
        expect(!line.contains("SQUELCH_"), "the outage line names no daemon setting")
        expect(!line.contains("\u{2014}"), "no em dash in the outage line")

        // The pane must actually route through the hosted check, and the
        // self-host remedies must still be there for self-host.
        guard let pane = try? String(contentsOfFile: "Sources/Passband/Views/AgentsSection.swift", encoding: .utf8) else {
            expect(false, "read the pane")
            return
        }
        expect(pane.contains("AgentConnect.isHostedOutage(status, hosted: hosted)"), "the pane checks for a hosted outage")
        expect(pane.contains("AgentConnect.showsSetup(status, hosted: hosted)"), "the pane gates setup on hosted")
        expect(pane.contains("SQUELCH_MCP_ALLOWED_HOSTS in the daemon's environment"), "self-host 403 copy kept")
        expect(pane.contains("check it is running and that this address reaches it"), "self-host no-answer copy kept")
    }

    static func accessAndTeamCopy() {
        guard let pane = try? String(contentsOfFile: "Sources/Passband/Views/AgentsSection.swift", encoding: .utf8) else {
            expect(false, "read the pane")
            return
        }
        // Sign-ins renew silently, so "until it expires" was never true.
        expect(
            pane.contains("An agent you approve keeps access until it is removed. To remove one, contact support for now."),
            "the access line says what is true")
        expect(!pane.contains("expires"), "no promise that access runs out on its own")
        let claude = AgentKind.claude.instruction(for: .signIn)
        expect(claude.contains("Team or Enterprise"), "the connector covers team plans")
        expect(claude.contains("Organization settings"), "and names where an owner adds it")
        expect(AgentKind.claude.instruction(for: .signIn) == AgentKind.claude.instruction, "one connector instruction")
    }

    /// The chips each door offers, in order, and the one it opens on. The web
    /// agents (Claude, ChatGPT) cannot reach a loopback or tailnet address, so
    /// the self-hosted door never lists them.
    static func chipOrder() {
        expect(
            AgentKind.shown(for: .signIn)
                == [.claude, .openClaw, .hermes, .muse, .chatGPT, .claudeCode, .codex, .cursor, .vsCode, .other],
            "sign-in door leads with Claude, OpenClaw and Hermes Agent")
        expect(
            AgentKind.shown(for: .none)
                == [.openClaw, .hermes, .claudeCode, .codex, .cursor, .vsCode, .claudeDesktop, .other],
            "self-hosted door leads with OpenClaw and Hermes Agent, keeps Claude Desktop")
        expect(AgentKind.defaultChoice(for: .signIn) == .claude, "the sign-in door opens on Claude")
        expect(AgentKind.defaultChoice(for: .none) == .openClaw, "the self-hosted door opens on OpenClaw")
        for web in [AgentKind.claude, .muse, .chatGPT] {
            expect(!AgentKind.shown(for: .none).contains(web), "\(web) is not offered at the self-hosted door")
            expect(AgentKind.shown(for: .signIn).contains(web), "\(web) is offered at the sign-in door")
        }
        for auth in [AgentDoorAuth.none, .signIn] {
            let shown = AgentKind.shown(for: auth)
            expect(Set(shown).count == shown.count, "no chip twice")
            expect(shown.last == .other, "Other stays last")
        }
        expect(Set(AgentKind.shown(for: .none) + AgentKind.shown(for: .signIn)) == Set(AgentKind.allCases), "every agent is offered somewhere")
    }

    static func installLinks() {
        // An address with characters that must be encoded: a port, a path,
        // a query and a space.
        let odd = "https://alice.passband.email:8443/mcp?x=a b&y=1"
        let claude = AgentKind.claudeInstallLink(endpoint: odd).absoluteString
        expect(
            claude
                == "https://claude.ai/customize/connectors?modal=add-custom-connector&connectorName=Passband&connectorUrl=https%3A%2F%2Falice.passband.email%3A8443%2Fmcp%3Fx%3Da%20b%26y%3D1",
            "Claude link is the documented form, address percent-encoded: \(claude)")
        let plain = "https://mcp.example.com/"
        expect(
            AgentKind.claudeInstallLink(endpoint: plain).absoluteString
                == "https://claude.ai/customize/connectors?modal=add-custom-connector&connectorName=Example&connectorUrl=https%3A%2F%2Fmcp.example.com%2F"
                .replacingOccurrences(of: "Example", with: "Passband"),
            "Claude link matches the docs' own example")
        let claudeQuery = URLComponents(url: AgentKind.claudeInstallLink(endpoint: odd), resolvingAgainstBaseURL: false)?
            .queryItems ?? []
        expect(claudeQuery.first { $0.name == "connectorUrl" }?.value == odd, "the connector URL decodes to the address")

        let url = "https://alice.passband.email/mcp"
        expect(
            AgentKind.claude.action(endpoint: url, auth: .signIn)
                == AgentAction(label: "Add to Claude", note: nil, kind: .open(AgentKind.claudeInstallLink(endpoint: url))),
            "Claude's button opens the install link")

        for address in [url, odd, "http://box.tailnet.ts.net:8848/mcp", "http://127.0.0.1:8848/mcp"] {
            let cursor = AgentKind.cursorInstallLink(endpoint: address).absoluteString
            let prefix = "cursor://anysphere.cursor-deeplink/mcp/install?name=passband&config="
            expect(cursor.hasPrefix(prefix), "Cursor link is the documented form: \(cursor)")
            // Undo exactly what Cursor's generator does: URI-decode, then %2B.
            let encoded = String(cursor.dropFirst(prefix.count))
            let base64 = (encoded.removingPercentEncoding ?? "").replacingOccurrences(of: "%2B", with: "+")
            let json = Data(base64Encoded: base64).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: String] }
            expect(json == ["url": address], "Cursor config decodes to {url: address} for \(address): \(base64)")
            expect(!encoded.contains("+") && !encoded.contains("/") && !encoded.contains("="), "Cursor config is URI-safe")

            let vscode = AgentKind.vsCodeInstallLink(endpoint: address).absoluteString
            expect(vscode.hasPrefix("vscode:mcp/install?"), "VS Code link is the documented form")
            let vsJSON = String(vscode.dropFirst("vscode:mcp/install?".count)).removingPercentEncoding
                .flatMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [String: String] }
            expect(vsJSON == ["name": "passband", "type": "http", "url": address], "VS Code config decodes for \(address)")
            for auth in [AgentDoorAuth.none, .signIn] {
                expect(
                    AgentKind.cursor.action(endpoint: address, auth: auth).kind
                        == .open(AgentKind.cursorInstallLink(endpoint: address)),
                    "Cursor's button opens its link at either door")
                expect(
                    AgentKind.vsCode.action(endpoint: address, auth: auth).kind
                        == .open(AgentKind.vsCodeInstallLink(endpoint: address)),
                    "VS Code's button opens its link at either door")
            }
        }
        // A '+' in the base64 must survive the round trip Cursor expects.
        let plusy = "https://a.example/mcp?q=>>>"
        let plusBase64 = Data(AgentKind.compactJSON(["url": plusy]).utf8).base64EncodedString()
        expect(plusBase64.contains("+"), "the fixture does produce a '+' in base64")
        expect(
            AgentKind.cursorInstallLink(endpoint: plusy).absoluteString.contains("%252B"),
            "'+' is %2B, then encoded again, as Cursor's generator does")

        let chat = AgentKind.chatGPT.action(endpoint: url, auth: .signIn)
        expect(chat.kind == .copyAndOpen(URL(string: "https://chatgpt.com/plugins")!), "ChatGPT copies, then opens Plugins")
        expect(chat.label == "Copy and open ChatGPT", "ChatGPT button label")

        let copies: [(AgentKind, AgentDoorAuth, String)] = [
            (.openClaw, .signIn, "Copy command"), (.openClaw, .none, "Copy command"),
            (.claudeCode, .signIn, "Copy command"), (.codex, .none, "Copy command"),
            (.hermes, .signIn, "Copy config"), (.hermes, .none, "Copy config"),
            (.claudeDesktop, .none, "Copy config"), (.other, .signIn, "Copy config"),
            (.cursor, .none, "Copy config"), (.vsCode, .signIn, "Copy config"),
            (.muse, .signIn, "Copy prompt"),
        ]
        for (kind, auth, label) in copies {
            expect(kind.copyAction(auth: auth) == AgentAction(label: label, note: nil, kind: .copy), "\(kind) copy fallback is \(label)")
        }
        for kind in [AgentKind.openClaw, .hermes, .muse, .claudeCode, .codex, .claudeDesktop, .other] {
            for auth in [AgentDoorAuth.none, .signIn] {
                expect(kind.action(endpoint: url, auth: auth).kind == .copy, "\(kind) has no link scheme, so it copies")
            }
        }
        expect(AgentKind.cursor.installOpensDesktopApp && AgentKind.vsCode.installOpensDesktopApp, "desktop links")
        expect(!AgentKind.claude.installOpensDesktopApp, "Claude's link is a web page, fine on a phone")
    }

    static func newAgentSnippets() {
        let hosted = "https://alice.passband.email/mcp"
        expect(
            AgentKind.hermes.snippet(endpoint: hosted, auth: .signIn) == """
                mcp_servers:
                  passband:
                    url: "\(hosted)"
                    auth: oauth
                """, "Hermes sign-in config")
        let selfHost = "http://box.tailnet.ts.net:8848/mcp"
        expect(
            AgentKind.hermes.snippet(endpoint: selfHost, auth: .none) == """
                mcp_servers:
                  passband:
                    url: "\(selfHost)"
                """, "Hermes open config has no auth line")
        expect(AgentKind.hermes.check == "hermes mcp test passband", "Hermes check")
        let ask = AgentKind.muse.snippet(endpoint: hosted, auth: .signIn)
        expect(ask.contains("URL: \(hosted)\n"), "Muse is told the address on its own line")
        expect(ask.contains("OAuth"), "Muse is told to sign in, since it asks otherwise")
        expect(AgentKind.muse.snippetIsPrompt && !AgentKind.muse.isCommand, "Muse's snippet is a message, not a command")
        expect(AgentKind.muse.signIn(for: .signIn) == nil && AgentKind.muse.check == nil, "Muse has no CLI to name")
        expect(AgentKind.hermes.signIn(for: .none) == nil, "no Hermes login at the open door")
        for auth in [AgentDoorAuth.none, .signIn] {
            expect(
                AgentKind.vsCode.snippet(endpoint: selfHost, auth: auth) == """
                    {
                      "servers": {
                        "passband": { "type": "http", "url": "\(selfHost)" }
                      }
                    }
                    """, "VS Code config")
        }
        let forbidden = ["token", "bearer", "authorization", "api_key", "apikey", "secret", "<", ">", "your-", "xxx", "..."]
        for kind in [AgentKind.hermes, .muse, .chatGPT, .vsCode] {
            for auth in [AgentDoorAuth.none, .signIn] {
                for url in [hosted, selfHost] {
                    let text = kind.snippet(endpoint: url, auth: auth)
                    expect(text.contains(url), "\(kind) snippet carries the real address")
                    for word in forbidden {
                        expect(!text.lowercased().contains(word), "\(kind) snippet has no placeholder (\(word)): \(text)")
                    }
                }
            }
        }
        for kind in AgentKind.allCases {
            for auth in [AgentDoorAuth.none, .signIn] {
                let action = kind.action(endpoint: hosted, auth: auth)
                let copy = [action.label, action.note ?? "", kind.instruction(for: auth)].joined()
                expect(!copy.contains("\u{2014}"), "no em dash in \(kind) button copy")
                expect(!action.label.isEmpty, "\(kind) has a button")
            }
        }
    }

    /// Every mark the pane names is in the asset catalog, as a vector file the
    /// maker published, so a renamed imageset cannot silently drop a logo back
    /// to its SF Symbol.
    static func brandAssets() {
        guard let pane = try? String(contentsOfFile: "Sources/Passband/Views/AgentsSection.swift", encoding: .utf8) else {
            expect(false, "read the pane")
            return
        }
        let named = try! NSRegularExpression(pattern: #""AgentLogo/([a-z-]+)""#)
        let names = Set(
            named.matches(in: pane, range: NSRange(pane.startIndex..., in: pane)).map {
                String(pane[Range($0.range(at: 1), in: pane)!])
            })
        expect(names.count >= 7, "the pane names its marks: \(names.sorted())")
        let root = "Sources/Passband/AgentLogos.xcassets/AgentLogo"
        for name in names {
            let set = "\(root)/\(name).imageset"
            guard let data = FileManager.default.contents(atPath: "\(set)/Contents.json"),
                let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            else {
                expect(false, "\(name) has an imageset")
                continue
            }
            let props = json["properties"] as? [String: Any]
            expect(props?["preserves-vector-representation"] as? Bool == true, "\(name) stays vector")
            let images = json["images"] as? [[String: Any]] ?? []
            expect(!images.isEmpty, "\(name) has a file")
            for image in images {
                let file = image["filename"] as? String ?? ""
                expect(file.hasSuffix(".svg"), "\(name) is an SVG: \(file)")
                expect(FileManager.default.fileExists(atPath: "\(set)/\(file)"), "\(name)'s \(file) is there")
            }
        }
    }
}
