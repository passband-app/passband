// A conversation written down and read back (ConversationSnapshot over the
// types in Transcript.swift). The chat behind a question in search history is
// restored from exactly these bytes, and a follow-up after the restore sends
// its `history` to the provider, so two things have to hold: every block comes
// back as the same block (tool_use inputs included, unknown fields and all),
// and the wire encoding the provider sees is the one it always was. Plus the
// tidying: nothing restored may look as if it is still waiting on a run.

import Foundation

/// The one outside type Transcript.swift names. The real inventory lives in
/// AgentTools.swift, which needs the app; a card only stores the raw value.
enum AgentTools {
    enum Tool: String, CaseIterable, Sendable {
        case searchMail = "search_mail"
        case archiveMessage = "archive_message"
        case sendEmail = "send_email"
    }
}

@main
@MainActor
struct ConversationSnapshotTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        roundTripKeepsEveryBlock()
        wireEncodingIsUnchanged()
        sanitizeSettlesWhatWasMidFlight()
        aNewerShapeIsNotRestored()
        theFileRoundTrips()
        theFileRefusesWhatCannotComeBack()
        anUnreadableFileIsNotAnEmptyOne()
        aHistoryMayOnlyCallOfferedTools()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func sample() -> ConversationSnapshot {
        let action = PendingAction(
            id: UUID(), tool: .archiveMessage, verb: "Archive", detail: nil,
            verifiedSender: "Maya", verifiedSubject: "Proofs", state: .pending)
        let items: [ChatItem] = [
            ChatItem(id: 1, kind: .user, text: "find the proofs"),
            ChatItem(
                id: 2, kind: .tool,
                tool: ToolActivity(useId: "tu1", name: "search_mail", summary: "Searched", state: .ok)),
            ChatItem(
                id: 3, kind: .emails,
                emails: [
                    EmailCard(
                        threadId: "t9", subject: "Proofs", sender: "Maya", date: "Oct 2",
                        snippet: "Attached")
                ]),
            ChatItem(id: 4, kind: .assistant, text: "Here they are."),
            ChatItem(
                id: 5, kind: .citations,
                citations: [ToolCitation(threadId: "t9", subject: "Proofs", sender: "Maya", date: "Oct 2")]),
            ChatItem(id: 6, kind: .action, action: action),
            ChatItem(
                id: 7, kind: .tool,
                tool: ToolActivity(useId: "tu2", name: "archive_message", summary: "Archiving")),
            ChatItem(id: 8, kind: .error, text: "The assistant's connection dropped mid-answer."),
            ChatItem(id: 9, kind: .assistant, text: "  "),
        ]
        let input: [String: JSONValue] = [
            "query": .string("proofs"), "limit": .number(5), "strict": .bool(true),
            "nested": .object(["unknown_field": .array([.null, .string("x")])]),
        ]
        let history: [Wire.MessageParam] = [
            .init(role: "user", content: .text("find the proofs")),
            .init(
                role: "assistant",
                content: .blocks([.text("Looking."), .toolUse(id: "tu1", name: "search_mail", input: input)])),
            .init(
                role: "user",
                content: .blocks([
                    .toolResult(toolUseId: "tu1", content: "{\"items\":[]}", isError: false),
                    .text("narrowed to: proofs from maya"),
                ])),
            .init(role: "assistant", content: .blocks([.text("Here they are.")])),
        ]
        return ConversationSnapshot.sanitized(
            transcript: items, history: history, askedThreadIds: [nil, "t9"], nextItemId: 9)
    }

    static func roundTripKeepsEveryBlock() {
        let original = sample()
        guard let data = try? original.encoded(), let back = ConversationSnapshot.decode(data) else {
            fail("snapshot did not round-trip")
            return
        }
        check(back.transcript.count == original.transcript.count, "transcript length")
        check(back.transcript.map(\.id) == original.transcript.map(\.id), "item ids")
        check(back.transcript.map(\.kind) == original.transcript.map(\.kind), "item kinds")
        check(back.transcript[2].emails == original.transcript[2].emails, "email cards")
        check(back.transcript[4].citations == original.transcript[4].citations, "citations")
        check(back.transcript[5].action?.tool == .archiveMessage, "action tool")
        check(back.transcript[5].action?.id == original.transcript[5].action?.id, "action id")
        check(back.transcript[5].action?.verifiedSender == "Maya", "verified sender")
        check(back.askedThreadIds == [nil, "t9"], "asked thread ids")
        check(back.nextItemId == 9, "next item id")
        // The history must be byte-for-byte what the provider would be sent.
        let enc = JSONEncoder()
        enc.outputFormatting = [.sortedKeys]
        let a = try? enc.encode(original.history)
        let b = try? enc.encode(back.history)
        check(a != nil && a == b, "history re-encodes identically")
        if case .blocks(let blocks) = back.history[1].content,
            case .toolUse(let id, let name, let input) = blocks[1]
        {
            check(id == "tu1" && name == "search_mail", "tool_use id and name")
            check(input == sample().history[1].toolUseInput, "tool_use input, unknown fields kept")
        } else {
            fail("tool_use block did not come back as one")
        }
        if case .blocks(let blocks) = back.history[2].content,
            case .toolResult(let id, let content, let isError) = blocks[0]
        {
            check(id == "tu1" && content == "{\"items\":[]}" && !isError, "tool_result")
        } else {
            fail("tool_result block did not come back as one")
        }
        if case .text(let s) = back.history[0].content {
            check(s == "find the proofs", "plain text content")
        } else {
            fail("a string content came back as blocks")
        }
    }

    static func wireEncodingIsUnchanged() {
        let enc = JSONEncoder()
        enc.outputFormatting = [.sortedKeys]
        let block = Wire.RequestBlock.toolResult(toolUseId: "a", content: "b", isError: true)
        let json = (try? enc.encode(block)).flatMap { String(data: $0, encoding: .utf8) }
        check(
            json == #"{"content":"b","is_error":true,"tool_use_id":"a","type":"tool_result"}"#,
            "tool_result wire shape: \(json ?? "nil")")
        let message = Wire.MessageParam(role: "user", content: .text("hi"))
        let m = (try? enc.encode(message)).flatMap { String(data: $0, encoding: .utf8) }
        check(m == #"{"content":"hi","role":"user"}"#, "string content wire shape: \(m ?? "nil")")
    }

    static func sanitizeSettlesWhatWasMidFlight() {
        let s = sample()
        check(s.transcript.last?.kind == .error, "the blank trailing assistant row is dropped")
        check(s.transcript.count == 8, "only the blank row is dropped")
        check(s.transcript[5].action?.state == .declined, "a pending card reads declined")
        check(s.transcript[6].tool?.state == .failed, "a spinning chip reads failed")
        check(s.transcript[1].tool?.state == .ok, "a finished chip is left alone")
        let running = PendingAction(
            id: UUID(), tool: .sendEmail, verb: "Send", state: .running)
        let executed = PendingAction(
            id: UUID(), tool: .sendEmail, verb: "Send", state: .executed("Sent."))
        let t = ConversationSnapshot.sanitized(
            transcript: [
                ChatItem(id: 1, kind: .action, action: running),
                ChatItem(id: 2, kind: .action, action: executed),
                ChatItem(id: 3, kind: .assistant, text: "Done."),
            ], history: [], askedThreadIds: [], nextItemId: 3)
        check(t.transcript[0].action?.state == .declined, "an in-flight card reads declined")
        check(t.transcript[1].action?.state == .executed("Sent."), "a settled card is left alone")
        check(t.transcript.count == 3, "a spoken assistant row is kept")
    }

    static func aNewerShapeIsNotRestored() {
        var s = sample()
        s.version = ConversationSnapshot.currentVersion + 1
        let data = try? s.encoded()
        check(data.flatMap(ConversationSnapshot.decode) == nil, "unknown version is refused")
        check(ConversationSnapshot.decode(Data("{}".utf8)) == nil, "garbage is refused")
        // A card naming a tool this build does not have fails the decode
        // rather than drawing a card for a tool that is gone.
        if let good = try? sample().encoded(), var text = String(data: good, encoding: .utf8) {
            text = text.replacingOccurrences(of: "\"archive_message\"", with: "\"gone_tool\"")
            check(ConversationSnapshot.decode(Data(text.utf8)) == nil, "unknown tool is refused")
        }
    }

    // MARK: - the file (SavedChat)

    static let now = Date(timeIntervalSince1970: 2_000_000_000)

    static func file(_ entries: [String: SavedChat]) -> Data? {
        try? SavedChat.fileBytes(entries.mapValues { .chat($0) })
    }

    static func theFileRoundTrips() {
        let fresh = SavedChat(savedAt: now.addingTimeInterval(-60), snapshot: sample())
        // One entry as pre-encoded bytes (a save), one encoded by the writer.
        let bytes = try? SavedChat.encodeEntry(fresh)
        let data = bytes.flatMap {
            try? SavedChat.fileBytes(["a \"quoted\" key": .bytes($0), "b": .chat(fresh)])
        }
        guard let data, let back = SavedChat.readFile(data, now: now) else {
            fail("chats file did not round-trip")
            return
        }
        check(Set(back.keys) == ["a \"quoted\" key", "b"], "both keys back, escaping intact")
        check(back["b"]?.savedAt == fresh.savedAt, "saved-at kept")
        check(back["a \"quoted\" key"]?.snapshot.transcript.count == sample().transcript.count, "chat kept")
        check(SavedChat.readFile(Data("{}".utf8), now: now)?.isEmpty == true, "an empty file is no chats")
    }

    static func theFileRefusesWhatCannotComeBack() {
        let old = SavedChat(
            savedAt: now.addingTimeInterval(-SavedChat.maxAge - 1), snapshot: sample())
        let edge = SavedChat(savedAt: now.addingTimeInterval(-SavedChat.maxAge), snapshot: sample())
        var newer = sample()
        newer.version = ConversationSnapshot.currentVersion + 1
        // A file edited (or written by a bug) to hold a live card and a
        // spinning chip, bypassing `sanitized` on the way out.
        let live = ConversationSnapshot(
            transcript: [
                ChatItem(id: 1, kind: .user, text: "archive it"),
                ChatItem(
                    id: 2, kind: .action,
                    action: PendingAction(id: UUID(), tool: .archiveMessage, verb: "Archive", state: .pending)),
                ChatItem(
                    id: 3, kind: .tool,
                    tool: ToolActivity(useId: "x", name: "search_mail", summary: "Searching")),
            ], history: [], askedThreadIds: [nil], nextItemId: 3)
        let data = file([
            "old": old, "edge": edge, "live": SavedChat(savedAt: now, snapshot: live),
            "newer": SavedChat(savedAt: now, snapshot: newer),
        ])
        guard let data, let back = SavedChat.readFile(data, now: now) else {
            fail("chats file did not read")
            return
        }
        check(back["old"] == nil, "a chat past 30 days is dropped")
        check(back["edge"] != nil, "a chat of exactly 30 days is kept")
        check(back["newer"] == nil, "a chat under another version is dropped")
        check(back["live"]?.snapshot.transcript[1].action?.state == .declined, "a live card reads declined")
        check(back["live"]?.snapshot.transcript[2].tool?.state == .failed, "a spinning chip reads failed")
        check(edge.isFresh(now: now) && !old.isFresh(now: now), "isFresh is the 30-day line")
        check(live.restorable?.transcript[1].action?.state == .declined, "restorable sanitizes")
        check(newer.restorable == nil, "restorable refuses another version")
    }

    static func anUnreadableFileIsNotAnEmptyOne() {
        check(SavedChat.readFile(Data("not json".utf8), now: now) == nil, "garbage is unreadable, not empty")
        check(SavedChat.readFile(Data("[1,2]".utf8), now: now) == nil, "a non-object is unreadable")
        let one = file(["a": SavedChat(savedAt: now, snapshot: sample())]) ?? Data()
        check(SavedChat.readFile(one.dropLast(), now: now) == nil, "a truncated file is unreadable")
        // One bad ENTRY costs that entry only.
        let mixed = Data(#"{"bad":{"savedAt":1},"good":"#.utf8)
            + ((try? SavedChat.encodeEntry(SavedChat(savedAt: now, snapshot: sample()))) ?? Data())
            + Data("}".utf8)
        let back = SavedChat.readFile(mixed, now: now)
        check(back != nil && back?["bad"] == nil && back?["good"] != nil, "one bad entry is dropped alone")
    }

    static func aHistoryMayOnlyCallOfferedTools() {
        let s = sample()
        check(s.calledTools == ["search_mail"], "called tools read off the history: \(s.calledTools)")
        check(s.onlyCalls(["search_mail", "get_thread"]), "offered tools restore")
        check(!s.onlyCalls(["get_thread"]), "a tool no longer offered refuses the restore")
        var plain = s
        plain.history = [.init(role: "user", content: .text("hi"))]
        check(plain.onlyCalls([]), "a history with no tool calls restores anywhere")
    }

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok { fail(what) }
    }

    static func fail(_ what: String) {
        failures += 1
        print("FAIL \(what)")
    }
}

extension Wire.MessageParam {
    fileprivate var toolUseInput: [String: JSONValue]? {
        guard case .blocks(let blocks) = content else { return nil }
        for case .toolUse(_, _, let input) in blocks { return input }
        return nil
    }
}
