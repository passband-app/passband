// THE AGENT'S DATA, without the agent: the wire shapes the session sends and
// the rows the tray draws. Moved out of Assistant.swift for the reason
// JSONValue lives on its own: a conversation is now also something written to
// disk and read back (ConversationSnapshot, QuestionChats), and the codec that
// does that is the kind of code a green build says nothing about. Here it
// compiles with just its data types, so test.sh can round-trip one.
//
// THE WIRE ENCODING IS THE PROVIDER'S, and the snapshot reuses it rather than
// inventing a second one: every `Decodable` below reads exactly what the
// matching `encode` writes, so a history saved today is the same bytes the
// next request would send.
//
// The one dependency outside this file is `AgentTools.Tool`, a String-backed
// enum whose Codable comes free from its raw value (see the bottom).

import Foundation

// MARK: - wire shapes

/// Minimal Anthropic /v1/messages shapes — just the fields this loop writes.
/// Hand-rolled rather than an SDK because LLMProxy makes the HTTP call and
/// AnthropicStream reads the response; this only builds the body.
enum Wire {
    struct ToolDef: Encodable {
        var name: String
        var description: String
        var input_schema: Schema

        struct Schema: Encodable {
            var type = "object"
            var properties: [String: Property]
            var required: [String]?
        }

        struct Property: Encodable {
            var type: String
            var description: String
            /// A closed vocabulary for a string parameter. Encoded as JSON
            /// Schema's `enum`, spelled `values` here because the keyword makes
            /// every call site a backtick.
            var values: [String]? = nil
            /// Element type, for an array parameter.
            var items: Items? = nil

            struct Items: Encodable { var type = "string" }

            private enum CodingKeys: String, CodingKey {
                case type, description, items
                case values = "enum"
            }
        }
    }

    /// A content block we SEND back (assistant echo or tool results).
    ///
    /// Decodable only so a saved conversation can be read back: `init(from:)`
    /// reads exactly the keys `encode` writes and nothing else.
    enum RequestBlock: Codable, Sendable {
        case text(String)
        case toolUse(id: String, name: String, input: [String: JSONValue])
        case toolResult(toolUseId: String, content: String, isError: Bool)

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case .text(let text):
                try c.encode("text", forKey: .type)
                try c.encode(text, forKey: .text)
            case .toolUse(let id, let name, let input):
                try c.encode("tool_use", forKey: .type)
                try c.encode(id, forKey: .id)
                try c.encode(name, forKey: .name)
                try c.encode(input, forKey: .input)
            case .toolResult(let toolUseId, let content, let isError):
                try c.encode("tool_result", forKey: .type)
                try c.encode(toolUseId, forKey: .tool_use_id)
                try c.encode(content, forKey: .content)
                try c.encode(isError, forKey: .is_error)
            }
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            switch try c.decode(String.self, forKey: .type) {
            case "text":
                self = .text(try c.decode(String.self, forKey: .text))
            case "tool_use":
                self = .toolUse(
                    id: try c.decode(String.self, forKey: .id),
                    name: try c.decode(String.self, forKey: .name),
                    input: try c.decode([String: JSONValue].self, forKey: .input))
            case "tool_result":
                self = .toolResult(
                    toolUseId: try c.decode(String.self, forKey: .tool_use_id),
                    content: try c.decode(String.self, forKey: .content),
                    isError: try c.decode(Bool.self, forKey: .is_error))
            case let other:
                // A block this build never writes. Failing the whole decode is
                // the honest answer: a history with a hole in it is one the
                // provider would reject on the next turn anyway.
                throw DecodingError.dataCorruptedError(
                    forKey: .type, in: c, debugDescription: "unknown block type \(other)")
            }
        }

        private enum CodingKeys: String, CodingKey {
            case type, text, id, name, input, tool_use_id, content, is_error
        }
    }

    struct MessageParam: Codable, Sendable {
        var role: String
        var content: Content

        enum Content: Codable, Sendable {
            case text(String)
            case blocks([RequestBlock])

            /// A bare string is `.text`, an array is `.blocks`: the two shapes
            /// `encode` writes.
            init(from decoder: Decoder) throws {
                let c = try decoder.singleValueContainer()
                if let s = try? c.decode(String.self) {
                    self = .text(s)
                } else {
                    self = .blocks(try c.decode([RequestBlock].self))
                }
            }

            func encode(to encoder: Encoder) throws {
                var c = encoder.singleValueContainer()
                switch self {
                case .text(let s): try c.encode(s)
                case .blocks(let b): try c.encode(b)
                }
            }
        }
    }

    /// A prompt-cache breakpoint. The default five-minute entry: a tool loop's
    /// requests are seconds apart, and every read refreshes it.
    struct CacheControl: Encodable, Equatable {
        var type = "ephemeral"
    }

    /// The system prompt, as one block carrying a cache breakpoint (the tools
    /// render ahead of it, so it caches them too). NOTHING IN IT MAY CHANGE
    /// within a conversation: the provider renders tools, then system, then
    /// messages, so a byte changed here re-writes the whole history behind
    /// it. What does change (the time, the open email, the local hits) rides
    /// on the question it was asked with instead (`withContext`).
    struct SystemBlock: Encodable, Equatable {
        var type = "text"
        var text: String
        var cache_control: CacheControl?

        static func stable(_ text: String) -> SystemBlock {
            SystemBlock(text: text, cache_control: CacheControl())
        }
    }

    /// THE REQUEST BODY. Up to three cache breakpoints: the system prompt,
    /// the last block of the last message, and the last block of the message
    /// two before it, which is where the previous request's breakpoint sat.
    /// The second is a read point for when one turn adds more blocks than the
    /// provider's lookback from the newest breakpoint (about 20). The message
    /// breakpoints are added HERE, on the way out, so the history that is
    /// kept (and saved with a question, see ConversationSnapshot) never
    /// carries one.
    struct Request: Encodable {
        var model: String
        var max_tokens: Int
        var system: [SystemBlock]
        var tools: [ToolDef]
        var messages: [MessageParam]
        /// ALWAYS true: `LLMProxy.stream` asks for `text/event-stream` and the
        /// accumulator can only read frames, so a body that forgot this would
        /// hand a whole JSON message to an SSE parser that drops it.
        var stream = true

        private enum CodingKeys: String, CodingKey {
            case model, max_tokens, system, tools, messages, stream
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(model, forKey: .model)
            try c.encode(max_tokens, forKey: .max_tokens)
            try c.encode(system, forKey: .system)
            try c.encode(tools, forKey: .tools)
            var list = c.nestedUnkeyedContainer(forKey: .messages)
            for (index, message) in messages.enumerated() {
                if index == messages.count - 1 || index == messages.count - 3 {
                    try list.encode(CacheMarked(message: message))
                } else {
                    try list.encode(message)
                }
            }
            try c.encode(stream, forKey: .stream)
        }

        /// The encoder every request is written with. SORTED KEYS, because a
        /// tool's `properties` is a Swift dictionary, whose order is not stable
        /// even between two dictionaries built the same way in one process:
        /// measured, five builds of the same request gave two different tool
        /// sections, and the tools are the first bytes of the cached prefix.
        static func encoder() -> JSONEncoder {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            return encoder
        }
    }

    /// The last message, with a cache breakpoint on its last block. A plain
    /// string is sent as one text block so it has somewhere to carry it.
    private struct CacheMarked: Encodable {
        var message: MessageParam

        private enum CodingKeys: String, CodingKey { case role, content }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(message.role, forKey: .role)
            let blocks: [RequestBlock]
            switch message.content {
            case .text(let text): blocks = [.text(text)]
            case .blocks(let b): blocks = b
            }
            var list = c.nestedUnkeyedContainer(forKey: .content)
            for (index, block) in blocks.enumerated() {
                if index == blocks.count - 1 {
                    try list.encode(MarkedBlock(block: block))
                } else {
                    try list.encode(block)
                }
            }
        }
    }

    /// A block with `cache_control` beside its own fields, in the same object.
    private struct MarkedBlock: Encodable {
        var block: RequestBlock

        private enum CodingKeys: String, CodingKey { case cache_control }

        func encode(to encoder: Encoder) throws {
            try block.encode(to: encoder)
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(CacheControl(), forKey: .cache_control)
        }
    }

    // MARK: history budget

    /// What a pruned tool result says instead of what it said. The call that
    /// produced it is still in the history beside it, so the model can see
    /// what it asked for and ask again.
    static let elidedResult =
        #"{"elided":"This result was removed from the conversation to save space. Call the tool again if you need it."}"#

    /// A rough token count for a history: UTF-8 bytes over four, about right
    /// for English and JSON and an undercount for CJK (three bytes a
    /// character). Only the budget below reads it, and the budget is a
    /// ceiling with room under it, not an exact limit.
    static func estimatedTokens(_ messages: [MessageParam]) -> Int {
        var bytes = 0
        for message in messages { bytes += size(message) }
        return bytes / 4
    }

    private static func size(_ message: MessageParam) -> Int {
        switch message.content {
        case .text(let text): return text.utf8.count
        case .blocks(let blocks): return blocks.reduce(0) { $0 + size($1) }
        }
    }

    private static func size(_ block: RequestBlock) -> Int {
        switch block {
        case .text(let text): return text.utf8.count
        case .toolUse(let id, let name, let input):
            return id.utf8.count + name.utf8.count + ((try? JSONEncoder().encode(input).count) ?? 0)
        case .toolResult(let id, let content, _): return id.utf8.count + content.utf8.count
        }
    }

    /// Where every tool result that may be pruned sits, oldest first: before
    /// `protectedFrom`, and never in the last message.
    private static func prunable(
        _ messages: [MessageParam], protectedFrom: Int
    ) -> [(message: Int, block: Int, bytes: Int)] {
        let end = min(protectedFrom, messages.count - 1)
        guard end > 0 else { return [] }
        var found: [(message: Int, block: Int, bytes: Int)] = []
        for m in 0..<end {
            guard case .blocks(let blocks) = messages[m].content else { continue }
            for (b, block) in blocks.enumerated() {
                if case .toolResult(let id, let content, _) = block {
                    found.append((m, b, id.utf8.count + content.utf8.count))
                }
            }
        }
        return found
    }

    /// The history as it is SENT, with the `count` oldest prunable tool
    /// results (see `elisions`) replaced by `elidedResult`. Nothing else
    /// changes: every tool_use keeps its tool_result, every turn stays where
    /// it was, and the history that is kept is not touched.
    static func pruned(
        _ messages: [MessageParam], eliding count: Int, protectedFrom: Int = .max
    ) -> [MessageParam] {
        guard count > 0 else { return messages }
        var out = messages
        for spot in prunable(messages, protectedFrom: protectedFrom).prefix(count) {
            guard case .blocks(var blocks) = out[spot.message].content,
                case .toolResult(let id, _, let isError) = blocks[spot.block]
            else { continue }
            blocks[spot.block] = .toolResult(toolUseId: id, content: elidedResult, isError: isError)
            out[spot.message].content = .blocks(blocks)
        }
        return out
    }

    /// How many tool results may be pruned: those before `protectedFrom`
    /// (the current run's first message: a run never loses what it fetched
    /// itself) and before the last message.
    static func prunableResults(_ messages: [MessageParam], protectedFrom: Int = .max) -> Int {
        prunable(messages, protectedFrom: protectedFrom).count
    }

    /// How many of the oldest prunable tool results to prune before the next
    /// request, given how many already are. STICKY, so the prefix the cache
    /// holds stays put: nothing more is pruned until the history passes
    /// `budget` again, and then enough goes to bring it down to `target`, well
    /// under the budget, so the next prune is many turns away. Each prune
    /// costs one cache miss on everything after the first result it changes.
    /// A history over budget on the current run's own results alone stays
    /// over: those are what the model is working from.
    static func elisions(
        for messages: [MessageParam], already: Int, protectedFrom: Int = .max,
        budget: Int = historyBudget, target: Int = historyTarget
    ) -> Int {
        let spots = prunable(messages, protectedFrom: protectedFrom)
        let note = elidedResult.utf8.count
        var count = min(already, spots.count)
        var bytes = 0
        for message in messages { bytes += size(message) }
        for spot in spots.prefix(count) { bytes -= max(0, spot.bytes - note) }
        guard bytes / 4 > budget else { return count }
        while count < spots.count, bytes / 4 > target {
            bytes -= max(0, spots[count].bytes - note)
            count += 1
        }
        return count
    }

    /// The history as it is SENT with each question's context in front of
    /// it: `context[i]` is a text block put ahead of message i's own. What a
    /// question was asked under (the time, the email open, the local hits)
    /// changes from question to question, so it cannot sit in the system
    /// prompt without re-writing the whole cached history behind it; on the
    /// question it belongs to, it is the same bytes in every later request.
    /// Held beside the history rather than in it, so a saved question keeps
    /// what it always kept.
    static func withContext(_ messages: [MessageParam], _ context: [Int: String]) -> [MessageParam] {
        guard !context.isEmpty else { return messages }
        var out = messages
        for (index, text) in context where out.indices.contains(index) && !text.isEmpty {
            switch out[index].content {
            case .text(let question): out[index].content = .blocks([.text(text), .text(question)])
            case .blocks(let blocks): out[index].content = .blocks([.text(text)] + blocks)
            }
        }
        return out
    }

    /// The history's ceiling, in estimated tokens, on top of the system prompt
    /// and the tools (about 6k). Past it the oldest tool results go, down to
    /// `historyTarget`. Most of a long history's weight is thread bodies from
    /// earlier turns, which a later turn rarely needs whole.
    static let historyBudget = 40_000
    static let historyTarget = 24_000

    /// The most of one message's text get_thread hands the model, in
    /// characters (about 2k tokens). Well past the snippet, which is the
    /// point of opening a thread, and short enough that one newsletter cannot
    /// outweigh the rest of the conversation in every later turn. The tool
    /// result is also what a saved question keeps, so it is kept capped too.
    static let threadTextCap = 8_000

    /// One message's text for get_thread: the head, up to `threadTextCap`
    /// characters, and a note saying how much was left out.
    static func cappedThreadText(_ text: String, cap: Int = threadTextCap) -> String {
        guard text.count > cap else { return text }
        let rest = text.count - cap
        return String(text.prefix(cap)) + "\n[truncated: \(rest) more characters]"
    }
}

// MARK: - citations

/// A source the assistant actually consulted — surfaced as an answer citation.
struct ToolCitation: Identifiable, Sendable, Hashable, Codable {
    var threadId: String
    var subject: String
    var sender: String
    var date: String
    var id: String { threadId }
}

/// One email the assistant chose to SHOW — a clickable card in the transcript,
/// richer than a citation. Every field comes from the daemon (the show_emails
/// dispatcher re-reads each thread), never from the model's own description:
/// a card the user will click on has the same honesty bar as a confirm card.
struct EmailCard: Identifiable, Sendable, Hashable, Codable {
    var threadId: String
    var subject: String
    var sender: String
    var date: String
    var snippet: String
    var id: String { threadId }
}


// MARK: - transcript

/// One row of the tray. A FLAT struct rather than an enum with payloads,
/// because the two hot mutations are "append a delta to this bubble's text" and
/// "flip this chip's state": both are one in-place field write here, where an
/// enum would rebuild (and re-copy) the payload on every token.
struct ChatItem: Identifiable, Sendable, Codable {
    enum Kind: String, Sendable, Codable { case user, assistant, tool, action, error, citations, emails }

    /// Monotonic per session, so a row keeps its identity while its text grows.
    let id: Int
    let kind: Kind
    /// user / assistant / error copy. GROWS IN PLACE while a turn streams.
    var text: String = ""
    var tool: ToolActivity? = nil
    var action: PendingAction? = nil
    var citations: [ToolCitation] = []
    var emails: [EmailCard] = []
}

/// A tool the model called, as the tray shows it.
struct ToolActivity: Sendable, Codable {
    enum State: String, Sendable, Equatable, Codable { case running, ok, failed }

    /// The provider's tool_use id. The chip is opened while the message is
    /// still streaming and finished after it closes, so this is how the
    /// executor finds its own row again.
    var useId: String
    var name: String
    var summary: String
    var state: State = .running
}

/// A Gmail-touching action, parked on the human. Carries only what the card
/// needs to state its case: a verb, whatever object was cheaply available from
/// the tool input, and (for a send) the mail itself to preview.
struct PendingAction: Identifiable, Sendable, Codable {
    enum State: Sendable, Equatable, Codable {
        case pending
        /// Approved and in flight.
        case running
        case declined
        /// Handed to the composer for the user to finish by hand.
        case handedOff
        /// Done, with the past-tense line the card settles on.
        case executed(String)
        case failed(String)
    }

    let id: UUID
    var tool: AgentTools.Tool
    /// Imperative, for the card's title ("Archive", "Send").
    var verb: String
    /// Anything else worth stating, in the model's own words: label changes,
    /// what a send is a reply to. NEVER the identity of what is being touched —
    /// see below.
    var detail: String?
    /// WHO AND WHAT, AS THE DAEMON REPORTS THEM. Resolved by a `get_thread`
    /// before the card is parked, never taken from the model's description.
    /// THE TAP IS THE SECURITY BOUNDARY: a card that names its target only by
    /// opaque id, or by a line the model wrote, is not informed consent — it
    /// asks a human to authorize something they cannot audit. On a reply send,
    /// `verifiedSender` IS the recipient.
    var verifiedSender: String?
    var verifiedSubject: String?
    // send_email only, and the reason "edit in composer" can exist at all.
    var replyToMessageId: Int?
    var to: String?
    /// The copy lists the model asked for, if any. On the card for the same
    /// reason `to` is: approving a send means seeing everyone it reaches, and
    /// a blind copy the confirmation did not mention is a recipient the user
    /// authorized without knowing.
    var cc: String?
    var bcc: String?
    var subject: String?
    var body: String?
    var state: State = .pending
}

/// A tool by its wire name, which is what a saved card stores. A name this
/// build no longer has fails the decode, and the caller falls back to asking
/// the question again rather than drawing a card for a tool that is gone.
extension AgentTools.Tool: Codable {}
