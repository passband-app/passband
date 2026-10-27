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

    struct Request: Encodable {
        var model: String
        var max_tokens: Int
        var system: String
        var tools: [ToolDef]
        var messages: [MessageParam]
        /// ALWAYS true: `LLMProxy.stream` asks for `text/event-stream` and the
        /// accumulator can only read frames, so a body that forgot this would
        /// hand a whole JSON message to an SSE parser that drops it.
        var stream = true
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
