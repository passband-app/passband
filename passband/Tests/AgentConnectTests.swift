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
        expect(
            AgentKind.shown(for: .none) == [.openClaw, .claudeCode, .codex, .cursor, .claudeDesktop, .other],
            "the open door's list is the one it always had")

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
            expect(literals > 10, "\(path) was actually scanned")
        }
        for kind in AgentKind.allCases {
            for auth in [AgentDoorAuth.none, .signIn] {
                let copy = [kind.name, kind.instruction(for: auth), kind.signIn(for: auth) ?? ""]
                expect(!copy.joined().contains("\u{2014}"), "no em dash in \(kind) copy")
            }
        }
    }
}
