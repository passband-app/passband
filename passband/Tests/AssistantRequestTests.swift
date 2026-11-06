// The assistant's request body (Wire.Request in Transcript.swift) and the
// history budget beside it. What has to hold: the body carries its prompt-cache
// breakpoints on the system prompt and the newest two user turns and nowhere
// else; the same request is the same bytes every time it is built, and a
// question's context is the same bytes in every later request; and pruning the
// history for a request keeps every tool_use with its tool_result, never
// touches the current run's own results, and stays put until the budget is
// passed again.

import Foundation

/// The one outside type Transcript.swift names (see ConversationSnapshotTests).
enum AgentTools {
    enum Tool: String, CaseIterable, Sendable {
        case searchMail = "search_mail"
        case getThread = "get_thread"
    }
}

@main
@MainActor
struct AssistantRequestTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        breakpointsWhereTheyBelong()
        earlierMessagesAreSentAsTheyAreKept()
        theSameRequestIsTheSameBytes()
        aQuestionsContextIsTheSameBytesLater()
        pruningKeepsTheShapeOfTheConversation()
        pruningIsStickyAndStaysUnderBudget()
        theCurrentRunIsNeverPruned()
        aThreadBodyIsCapped()
        theLastStepAnswersInsteadOfFailing()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    /// The step valve ends in an answer: the last request switches tools off
    /// without dropping them (the cached prefix stays the same bytes) and asks
    /// for an answer on the outgoing copy only.
    static func theLastStepAnswersInsteadOfFailing() {
        let history = conversation(turns: 2)
        let normal = json(request(history)) as! [String: Any]
        check(normal["tool_choice"] == nil, "an ordinary step sends no tool_choice")

        var last = request(Wire.wrappingUp(history))
        last.tool_choice = .answerOnly
        let body = json(last) as! [String: Any]
        check((body["tool_choice"] as? [String: Any])?["type"] as? String == "none",
              "the last step sends tool_choice none")
        check((body["tools"] as? [Any])?.count == tools().count, "the last step keeps the tools")

        let sent = Wire.wrappingUp(history)
        check(sent.count == history.count, "wrapping up adds no message")
        if case .blocks(let blocks) = sent.last!.content, case .text(let nudge) = blocks.last! {
            check(nudge == Wire.wrapUpNudge, "the nudge rides last on the newest user message")
        } else {
            check(false, "the nudge rides last on the newest user message")
        }
        if case .blocks(let blocks) = history.last!.content {
            check(blocks.count == 1, "the kept history never carries the nudge")
        }
        check(!Wire.wrapUpNudge.contains("\u{2014}"), "no em dash in the nudge")
    }

    // MARK: - fixtures

    static func tools() -> [Wire.ToolDef] {
        // Built fresh each call, the way the session builds them per request.
        var properties: [String: Wire.ToolDef.Property] = [:]
        for name in ["query", "limit", "after", "before", "from", "thread_id", "cursor", "sort"] {
            properties[name] = .init(type: "string", description: "the \(name)")
        }
        return [
            Wire.ToolDef(
                name: "search_mail", description: "search",
                input_schema: .init(properties: properties, required: ["query"])),
            Wire.ToolDef(
                name: "get_thread", description: "read",
                input_schema: .init(properties: properties, required: ["thread_id"])),
        ]
    }

    static func request(_ messages: [Wire.MessageParam]) -> Wire.Request {
        Wire.Request(
            model: "claude-haiku-4-5", max_tokens: 4096,
            system: [.stable("You are the assistant.")],
            tools: tools(), messages: messages)
    }

    static func json(_ value: some Encodable) -> Any {
        try! JSONSerialization.jsonObject(with: Wire.Request.encoder().encode(value))
    }

    /// Every `cache_control` in a JSON tree, by path.
    static func breakpoints(_ value: Any, path: String = "") -> [String] {
        var found: [String] = []
        if let object = value as? [String: Any] {
            for (key, child) in object {
                if key == "cache_control" { found.append(path) }
                found += breakpoints(child, path: path + "." + key)
            }
        } else if let array = value as? [Any] {
            for (index, child) in array.enumerated() {
                found += breakpoints(child, path: path + "[\(index)]")
            }
        }
        return found.sorted()
    }

    static func turn(_ n: Int, body: Int = 4000) -> [Wire.MessageParam] {
        [
            .init(role: "assistant", content: .blocks([
                .text("Reading thread \(n)."),
                .toolUse(id: "toolu_\(n)", name: "get_thread", input: ["thread_id": .string("t\(n)")]),
            ])),
            .init(role: "user", content: .blocks([
                .toolResult(
                    toolUseId: "toolu_\(n)", content: String(repeating: "x", count: body), isError: false)
            ])),
        ]
    }

    static func conversation(turns: Int, body: Int = 4000) -> [Wire.MessageParam] {
        var messages: [Wire.MessageParam] = [.init(role: "user", content: .text("what did the bank say"))]
        for n in 0..<turns { messages += turn(n, body: body) }
        return messages
    }

    // MARK: - checks

    static func breakpointsWhereTheyBelong() {
        let one = json(request([.init(role: "user", content: .text("hi"))]))
        check(
            breakpoints(one) == [".messages[0].content[0]", ".system[0]"],
            "a first question: the system prompt and the question carry the only breakpoints")
        let body = one as! [String: Any]
        let content = ((body["messages"] as! [Any])[0] as! [String: Any])["content"] as! [Any]
        let block = content[0] as! [String: Any]
        check(block["type"] as? String == "text" && block["text"] as? String == "hi",
            "a plain question is sent as one text block, unchanged")

        let loop = json(request(conversation(turns: 3)))
        check(
            breakpoints(loop) == [".messages[4].content[0]", ".messages[6].content[0]", ".system[0]"],
            "a tool loop: the newest tool result, and the previous request's newest, and nowhere else")
        let last = (((loop as! [String: Any])["messages"] as! [Any])[6] as! [String: Any])["content"] as! [Any]
        let result = last[0] as! [String: Any]
        check(result["type"] as? String == "tool_result" && result["tool_use_id"] as? String == "toolu_2",
            "the marked block is still the tool result it was")
    }

    static func earlierMessagesAreSentAsTheyAreKept() {
        let history = conversation(turns: 2)
        let sent = (json(request(history)) as! [String: Any])["messages"] as! [Any]
        for index in history.indices.dropLast() where index != history.count - 3 {
            let kept = try! JSONSerialization.jsonObject(with: Wire.Request.encoder().encode(history[index]))
            check(
                NSDictionary(dictionary: kept as! [String: Any]).isEqual(sent[index] as! [String: Any]),
                "message \(index), carrying no breakpoint, is sent exactly as the history keeps it")
        }
        let stored = try! Wire.Request.encoder().encode(history)
        check(!String(decoding: stored, as: UTF8.self).contains("cache_control"),
            "the history that is kept carries no breakpoint")
    }

    static func theSameRequestIsTheSameBytes() {
        var seen = Set<Data>()
        for _ in 0..<20 {
            seen.insert(try! Wire.Request.encoder().encode(request(conversation(turns: 2))))
        }
        check(seen.count == 1, "twenty builds of the same request are one byte string (got \(seen.count))")
    }

    static func pruningKeepsTheShapeOfTheConversation() {
        let history = conversation(turns: 4)
        let pruned = Wire.pruned(history, eliding: 2)
        check(pruned.count == history.count, "pruning removes no message")

        func results(_ messages: [Wire.MessageParam]) -> [(String, String)] {
            messages.flatMap { message -> [(String, String)] in
                guard case .blocks(let blocks) = message.content else { return [] }
                return blocks.compactMap {
                    if case .toolResult(let id, let content, _) = $0 { return (id, content) }
                    return nil
                }
            }
        }
        func uses(_ messages: [Wire.MessageParam]) -> [String] {
            messages.flatMap { message -> [String] in
                guard case .blocks(let blocks) = message.content else { return [] }
                return blocks.compactMap { if case .toolUse(let id, _, _) = $0 { return id }; return nil }
            }
        }
        check(uses(pruned) == uses(history), "every tool_use is still there")
        check(results(pruned).map(\.0) == results(history).map(\.0), "every tool_use keeps its tool_result")
        let contents = results(pruned).map(\.1)
        check(contents[0] == Wire.elidedResult && contents[1] == Wire.elidedResult, "the two OLDEST results go")
        check(contents[2] != Wire.elidedResult && contents[3] != Wire.elidedResult, "the newer ones stay")

        let all = Wire.pruned(history, eliding: 99)
        check(results(all).last?.1 != Wire.elidedResult, "the turn being answered is never pruned")
        check(Wire.prunableResults(history) == 3, "only the results before the last message count as prunable")
    }

    static func pruningIsStickyAndStaysUnderBudget() {
        let budget = 10_000
        let target = 6_000
        // Each turn's result is 4,000 characters, about 1,000 tokens.
        let small = conversation(turns: 5)
        check(Wire.elisions(for: small, already: 0, budget: budget, target: target) == 0,
            "under the budget, nothing is pruned")

        let big = conversation(turns: 14)
        let first = Wire.elisions(for: big, already: 0, budget: budget, target: target)
        check(first > 0, "over the budget, some results are pruned")
        check(Wire.estimatedTokens(Wire.pruned(big, eliding: first)) <= target,
            "and the history comes down to the target")

        let grown = big + turn(14)
        let next = Wire.elisions(for: grown, already: first, budget: budget, target: target)
        check(next == first, "one more turn under the budget prunes nothing more, so the cached prefix holds")

    }

    static func theCurrentRunIsNeverPruned() {
        // Three earlier runs' worth of results, then a run of its own reading
        // ten long threads: the budget is passed on the run's own results.
        let earlier = conversation(turns: 3)
        let runStart = earlier.count
        var history = earlier + [.init(role: "user", content: .text("compare the statements"))]
        for n in 10..<20 { history += turn(n, body: 8000) }
        let count = Wire.elisions(
            for: history, already: 0, protectedFrom: runStart, budget: 10_000, target: 6_000)
        check(count == Wire.prunableResults(history, protectedFrom: runStart),
            "over budget, everything before the run may go")
        check(count == 3, "and that is only the three earlier results")
        let sent = Wire.pruned(history, eliding: count, protectedFrom: runStart)
        var kept = 0
        for message in sent.suffix(from: runStart) {
            guard case .blocks(let blocks) = message.content else { continue }
            for block in blocks {
                if case .toolResult(_, let content, _) = block, content != Wire.elidedResult { kept += 1 }
            }
        }
        check(kept == 10, "every result the current run fetched is sent whole")
    }

    static func aQuestionsContextIsTheSameBytesLater() {
        let context = [0: "Context from Passband, not typed by the user:\nToday is Thursday."]
        let first = Wire.withContext([.init(role: "user", content: .text("what is due"))], context)
        guard case .blocks(let blocks) = first[0].content, blocks.count == 2,
            case .text(let lead) = blocks[0], case .text(let question) = blocks[1]
        else { return check(false, "the context is a text block ahead of the question") }
        check(lead == context[0] && question == "what is due", "the context leads, the question follows")

        let later = Wire.withContext(
            conversation(turns: 2).enumerated().map { index, message in
                index == 0 ? .init(role: "user", content: .text("what is due")) : message
            } + [.init(role: "user", content: .text("and next week?"))], context)
        let encoder = Wire.Request.encoder()
        check(
            try! encoder.encode(first[0]) == encoder.encode(later[0]),
            "the first question goes out as the same bytes once the conversation has grown")
        check(Wire.withContext(later, [:]).count == later.count, "no context changes nothing")
        let stored: [Wire.MessageParam] = [.init(role: "user", content: .text("what is due"))]
        _ = Wire.withContext(stored, context)
        if case .text = stored[0].content {
            check(true, "the kept history is not touched")
        } else {
            check(false, "the kept history is not touched")
        }
    }

    static func aThreadBodyIsCapped() {
        check(Wire.cappedThreadText("short") == "short", "a short body is left alone")
        let exact = String(repeating: "a", count: Wire.threadTextCap)
        check(Wire.cappedThreadText(exact) == exact, "a body exactly at the cap is left alone")
        let long = String(repeating: "b", count: Wire.threadTextCap + 1234)
        let capped = Wire.cappedThreadText(long)
        check(capped.hasPrefix(String(repeating: "b", count: Wire.threadTextCap)), "the head is kept")
        check(capped.hasSuffix("[truncated: 1234 more characters]"), "and the note says how much went")
        check(capped.count < Wire.threadTextCap + 60, "and nothing past the cap rides along")
        let emoji = String(repeating: "👩‍👩‍👧", count: Wire.threadTextCap + 5)
        check(Wire.cappedThreadText(emoji).hasSuffix("[truncated: 5 more characters]"),
            "the cap counts characters, never splitting one")
    }
}
