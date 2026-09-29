// WHO A MESSAGE IS ADDRESSED TO, as one value instead of three loose strings.
//
// Every composer in the app — the pane, the reader's inline reply, the phone's
// sheet — holds the same three comma-joined header values, and every one of
// them needs the same four operations: split a list into the tokens a human
// clicked on, move one of those tokens to another field, drop it, and count
// what is left. Doing that against `String` at each call site is how To and Cc
// end up disagreeing about who is on a message.
//
// PURE ON PURPOSE: no SwiftUI, no store, no wire. The composer state owns three
// strings (they are what goes on the wire, verbatim); this type is a VIEW of
// them that knows the rules. Which is also what lets the suite test the one
// operation with real consequences — moving somebody to Bcc — without an app.
//
// THE MOVE IS THE POINT. `move(_:to:)` takes an address out of every field
// before putting it in the destination, so an address is in exactly one of the
// three when it returns. Anything less is how a person ends up in To and Bcc at
// once, which is not a blind copy of anything: their address is right there in
// the header everyone can read, and the sender believed otherwise.
//
// THE READING SIDE lives here too (`RecipientLine`, `Recipients.reading`): the
// reader shows the same three headers on mail that has already been sent, and it
// has to split them by the same grammar the composer wrote them with. Two
// splitters would eventually disagree about `"Doe, John" <j@x>`, and the two
// halves of the app would name different people.

import Foundation

/// Which of the three recipient headers. Ordered as they are written and read.
enum RecipientSlot: String, CaseIterable, Sendable, Hashable {
    case to, cc, bcc

    /// The header's own lowercase spelling — what the composer labels the field
    /// with, and what review calls the row.
    var label: String { rawValue }
}

/// The three recipient headers of one composition, each a comma-joined list
/// exactly as it goes out.
struct Recipients: Sendable, Equatable {
    var to: String = ""
    var cc: String = ""
    var bcc: String = ""

    init(to: String = "", cc: String = "", bcc: String = "") {
        self.to = to
        self.cc = cc
        self.bcc = bcc
    }

    subscript(slot: RecipientSlot) -> String {
        get {
            switch slot {
            case .to: to
            case .cc: cc
            case .bcc: bcc
            }
        }
        set {
            switch slot {
            case .to: to = newValue
            case .cc: cc = newValue
            case .bcc: bcc = newValue
            }
        }
    }

    /// The tokens in one field, as the sender wrote them — display names and
    /// all. What the composer draws pills from.
    func tokens(_ slot: RecipientSlot) -> [String] { Self.split(self[slot]) }

    /// How many people one field names.
    func count(_ slot: RecipientSlot) -> Int { tokens(slot).count }

    /// Every addressee, in header order. Used for the "reaches N people" lines.
    var all: [String] { RecipientSlot.allCases.flatMap(tokens) }

    /// Which field holds this address, if any.
    func slot(of token: String) -> RecipientSlot? {
        let wanted = Self.key(token)
        guard !wanted.isEmpty else { return nil }
        return RecipientSlot.allCases.first { slot in
            tokens(slot).contains { Self.key($0) == wanted }
        }
    }

    /// MOVE ONE ADDRESSEE TO ANOTHER FIELD, carrying the token whole — display
    /// name included, because that is how the sender addressed them.
    ///
    /// Removed from EVERY field before it is appended, not just from the one it
    /// came from. Two reasons, and the second is the one with teeth: the caller
    /// does not have to know where the token was, and a person who somehow ended
    /// up in two lists is repaired rather than duplicated. Leaving them in To
    /// while adding them to Bcc would be the worst outcome available — their
    /// address sits in a header every recipient reads, and the sender believes
    /// they blind-copied someone.
    ///
    /// Matching is on the BARE MAILBOX, case-folded, so `Bob <bob@x>` and
    /// `BOB@X` are one person however the two lists happen to spell them.
    mutating func move(_ token: String, to destination: RecipientSlot) {
        let key = Self.key(token)
        guard !key.isEmpty else { return }
        let carried = token.trimmingCharacters(in: .whitespacesAndNewlines)
        remove(token)
        var addrs = tokens(destination)
        addrs.append(carried)
        self[destination] = Self.join(addrs)
    }

    /// Drop an addressee from wherever they are. Idempotent: removing someone
    /// who was never there is not an error, it is the state you asked for.
    mutating func remove(_ token: String) {
        let key = Self.key(token)
        guard !key.isEmpty else { return }
        for slot in RecipientSlot.allCases {
            self[slot] = Self.join(tokens(slot).filter { Self.key($0) != key })
        }
    }

    /// REPLACE ONE HEADER'S WHOLE LIST, and take everyone it now names out of
    /// the other two. This is what a field's own editing writes through:
    /// TYPING an address into To is as explicit a statement as dragging the
    /// pill there, so the rule that a person sits in exactly one header has to
    /// hold however they arrived — not only when they came through `move`.
    ///
    /// Half-typed fragments claim nobody: a token with no `@` has no key, and
    /// `key` returning "" is what keeps two of them from matching each other or
    /// evicting a real address somewhere else.
    mutating func set(_ slot: RecipientSlot, to list: String) {
        self[slot] = list
        let claimed = Set(Self.split(list).map(Self.key).filter { !$0.isEmpty })
        guard !claimed.isEmpty else { return }
        for other in RecipientSlot.allCases where other != slot {
            self[other] = Self.join(tokens(other).filter { !claimed.contains(Self.key($0)) })
        }
    }

    /// THE ADDRESSES IN A DROPPED PAYLOAD. A pill dragged in from another field
    /// arrives as its own token; text dragged out of a message, or out of
    /// another app, arrives as whatever was under the selection. Only tokens
    /// that ARE addresses count — a key with an `@` and no whitespace, so a
    /// sentence that happens to mention somebody is not pilled as them — and
    /// each person once, under the first spelling they arrived in.
    ///
    /// Lines split before commas: a column of addresses copied out of a
    /// spreadsheet is a list too, just one that never learned about commas.
    static func dropped(_ text: String) -> [String] {
        var seen = Set<String>()
        return text.split(whereSeparator: \.isNewline).flatMap { split(String($0)) }.filter { token in
            let k = key(token)
            guard !k.isEmpty, !k.contains(where: \.isWhitespace) else { return false }
            return seen.insert(k).inserted
        }
    }

    // MARK: - list grammar

    /// Split an address-list header on the commas that SEPARATE addresses. A
    /// comma inside a quoted display name (`"Doe, John" <j@x>`) is part of the
    /// name, and splitting there invents a recipient who does not exist — and,
    /// worse, destroys the one who did.
    static func split(_ list: String) -> [String] {
        var out: [String] = []
        var current = ""
        var quoted = false
        for ch in list {
            if ch == "\"" { quoted.toggle() }
            if ch == ",", !quoted {
                out.append(current)
                current = ""
            } else {
                current.append(ch)
            }
        }
        out.append(current)
        return out
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    static func join(_ addrs: [String]) -> String {
        addrs.joined(separator: ", ")
    }

    /// THE IDENTITY OF AN ADDRESSEE: the bare mailbox, case-folded. Everything
    /// that compares two recipients compares this, so the same person written
    /// three ways is still one person.
    ///
    /// `Bob Smith <bob@x.test>` and `"Doe, John" <j@x.test>` yield the address
    /// between the angle brackets; a bare `bob@x.test` yields itself. Anything
    /// with no `@` at all yields "", which every caller treats as "not an
    /// address yet" — that is the half-typed fragment in a live field, and it
    /// must never match another one.
    static func key(_ token: String) -> String {
        let trimmed = token.trimmingCharacters(in: .whitespacesAndNewlines)
        var addr = trimmed
        // The LAST angle pair: a display name may legitimately contain one
        // (`"<the boss>" <b@x>`), and the address is always the final bracket.
        if let open = trimmed.lastIndex(of: "<"), let close = trimmed.lastIndex(of: ">"),
            open < close
        {
            addr = String(trimmed[trimmed.index(after: open)..<close])
        }
        addr = addr.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return addr.contains("@") ? addr : ""
    }
}

// MARK: - the reading side

/// WHAT HAPPENED TO ONE PERSON between the previous message in a thread and this
/// one. `present` is the ordinary case and carries no mark at all.
enum RecipientMark: Sendable, Equatable {
    /// On this message and on the one before it — or on the first message of a
    /// thread, where there is nothing to have changed from.
    case present
    /// Brought into the conversation by this message.
    case added
    /// Dropped by this message: NOT a recipient of it, and shown anyway, on the
    /// header they used to be on. A person quietly taken off a thread is the
    /// change you most want to notice and the one nothing in a mail client ever
    /// tells you about.
    case removed
}

/// One addressee as the reader draws them.
struct RecipientPerson: Identifiable, Equatable, Sendable {
    /// `Name <addr>` as the header spelled it.
    let token: String
    /// The readable half: a display name, a bare address, or "you".
    let name: String
    let mark: RecipientMark

    /// Unique WITHIN A LINE, which is all a ForEach here needs: one line never
    /// holds the same mailbox twice (the same person cannot be a recipient and a
    /// ghost of the same header).
    var id: String { token }
}

/// One header's line: who is on it, and who left it.
struct RecipientLine: Identifiable, Equatable, Sendable {
    let slot: RecipientSlot
    let people: [RecipientPerson]

    var id: String { slot.rawValue }
    var label: String { slot.label }

    /// The addresses spelled out, for the pointer and for VoiceOver: the names
    /// are the readable half, this is the half that says WHICH bob@ it was, and
    /// it is the only place a mark is spelled in words.
    var full: String {
        let parts = people.map { person -> String in
            switch person.mark {
            case .present: person.token
            case .added: "\(person.token) (added)"
            case .removed: "\(person.token) (removed)"
            }
        }
        return "\(label): " + parts.joined(separator: ", ")
    }
}

/// EVERYONE ON ONE MESSAGE: who wrote it and who it named. The unit the reader
/// diffs, because neither half answers "who is in this conversation" alone.
///
/// The sender is in here for a reason with teeth. A reply does not name its own
/// author in its To, and the message it answers did not name its author either —
/// so a diff over recipients alone reports the two most ordinary events in mail
/// (Maya writes, Daniel replies) as Daniel leaving and Maya joining. Every thread
/// would light up, and the mark would mean nothing by the second message.
struct MessageAudience: Equatable, Sendable {
    /// `Name <addr>` or a bare address, as the `From:` header spelled it.
    let sender: String
    let recipients: Recipients

    /// Every bare mailbox on the message, sender included.
    var keys: Set<String> {
        var out = Set(recipients.all.map(Recipients.key).filter { !$0.isEmpty })
        let from = Recipients.key(sender)
        if !from.isEmpty { out.insert(from) }
        return out
    }
}

/// Every recipient line of one message, plus what changed since the last one.
struct RecipientDigest: Equatable, Sendable {
    let lines: [RecipientLine]
    /// People actually addressed by this message — ghosts are not recipients of
    /// it and are not counted as any.
    let count: Int
    let added: Int
    let removed: Int

    var isEmpty: Bool { lines.isEmpty }

    /// WHETHER THIS MESSAGE MOVED THE ROOM. The reader opens the block on it,
    /// because a change in who is listening is the one thing here worth
    /// interrupting a read for — and it stays shut the rest of the time, which
    /// is most of the time.
    var changed: Bool { added > 0 || removed > 0 }

    static let none = RecipientDigest(lines: [], count: 0, added: 0, removed: 0)
}

extension Recipients {
    /// The lines to draw for one message, in header order, skipping the headers
    /// that named nobody.
    ///
    /// `nil` and `""` both yield no line and are NOT the same statement upstream:
    /// nil is "nobody has read this message's headers yet" (an old daemon, or one
    /// whose recipients sweep has not walked back this far) and "" is "read, and
    /// the header named nobody". The difference decides what the daemon does
    /// next; down here they draw the same nothing.
    ///
    /// `me` is the account's own address, printed as "you" wherever it appears —
    /// the reader knows they were on the mail, and their own address spelled out
    /// among four others is the one name on the line carrying no information.
    ///
    /// `previous` is the message BEFORE this one in the thread, and `nil` when
    /// there is none or when its headers were never read. Nil means NO MARKS:
    /// "everyone here is new" is a claim, and an unread predecessor is not
    /// evidence for it.
    @MainActor
    static func reading(
        _ current: MessageAudience, me: String?, previous: MessageAudience? = nil
    ) -> RecipientDigest {
        let mine = me?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() ?? ""
        // Compared on the BARE MAILBOX, which is what `key` is for: the same
        // person is written `you@x.com`, `You <you@x.com>` and `YOU@X.COM` across
        // three headers of the same message.
        func display(_ token: String) -> String {
            if !mine.isEmpty, key(token) == mine { return "you" }
            return SenderCache.resolved(token).displayName
        }
        // ACROSS THE WHOLE MESSAGE, not per header: somebody moved from To to Cc
        // is still in the conversation, and marking that as an addition and a
        // removal would cry wolf on the one signal this exists for.
        let here = current.keys
        // A predecessor that named NOBODY (a blind copy, `undisclosed-
        // recipients:;`) is no baseline either: its audience is its sender
        // alone, and diffing against that would mark everyone on the next
        // message as newly added.
        let before = previous.flatMap { $0.recipients.all.isEmpty ? nil : $0.keys }

        var added = 0
        var removed = 0
        var ghosted: Set<String> = []
        var lines: [RecipientLine] = []
        var count = 0

        for slot in RecipientSlot.allCases {
            var people: [RecipientPerson] = []
            for token in current.recipients.tokens(slot) {
                let mark: RecipientMark =
                    if let before, !before.contains(key(token)) { .added } else { .present }
                if mark == .added { added += 1 }
                people.append(RecipientPerson(token: token, name: display(token), mark: mark))
                count += 1
            }
            // The ghosts of this header: on the previous message, on no header of
            // this one, and not its author. Deduped across slots so somebody who
            // was in both To and Cc leaves one ghost rather than two.
            //
            // NEVER FROM A BCC, which is the other way this mark could cry wolf:
            // a blind copy is invisible to every other recipient, so the next
            // message in the thread cannot name them and never does. Ghosting
            // them would report a removal on every thread the user ever blind-
            // copied anyone on — and report it, of all places, to the one person
            // who knows it did not happen.
            guard slot != .bcc else {
                if !people.isEmpty { lines.append(RecipientLine(slot: slot, people: people)) }
                continue
            }
            for token in previous?.recipients.tokens(slot) ?? [] {
                let k = key(token)
                guard !k.isEmpty, !here.contains(k), !ghosted.contains(k) else { continue }
                ghosted.insert(k)
                removed += 1
                people.append(
                    RecipientPerson(token: token, name: display(token), mark: .removed))
            }
            if !people.isEmpty {
                lines.append(RecipientLine(slot: slot, people: people))
            }
        }
        return RecipientDigest(lines: lines, count: count, added: added, removed: removed)
    }
}
