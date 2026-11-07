// `t`, teach Passband: the pure parts. What the toast says about what a
// sentence became, and what to do when the daemon is too old to have the
// teach route. Foundation only, so `test.sh teach` can pin both.

import Foundation

enum Teach {
    /// The daemon's own cap on one sentence.
    static let maxLength = 1000

    /// Shown under the field once the sentence gets close to the cap.
    static let lengthWarning = 900

    /// The placeholder when there is no sender to suggest from.
    static let example =
        "e.g. emails to Alex about grant funding for Northwind Bakery: never show me those"

    /// The undo chip after a save, naming who the rule ended up about.
    static func savedLabel(_ result: TeachResult) -> String {
        guard result.isSender else {
            let merged = result.merged?.reduce(0) { $0 + $1.replaced_ids.count } ?? 0
            if merged > 0 {
                return "got it. merged \(merged) similar rules to make room"
            }
            return "got it. applies to all your mail"
        }
        guard let pattern = result.match_pattern?.trimmingCharacters(in: .whitespaces),
            !pattern.isEmpty
        else { return "got it. saved as a sender rule" }
        if pattern.hasPrefix("*@") {
            return "got it. applies to all mail from \(pattern.dropFirst(2))"
        }
        return "got it. applies to mail from \(pattern)"
    }

    /// The toast after the undo of a teach.
    static let undoneText = "forgot that"

    /// What to do when `POST /client/teach` answers 404, which is what a daemon
    /// from before the route says. With a sender on the card the old behaviour
    /// still works: a sender rule on that sender's pattern, outcome inferred.
    /// With none there is nothing an old daemon can save.
    enum Fallback: Equatable {
        case senderRule(pattern: String)
        case needsUpdate
    }

    static func fallback(sender: String?) -> Fallback {
        guard let sender, !sender.trimmingCharacters(in: .whitespaces).isEmpty else {
            return .needsUpdate
        }
        let pattern = SenderID.patternFromSender(sender).lowercased()
        // A whole-domain rule on a shared mail host would cover strangers; the
        // daemon narrows the same way (`triage::teach::SHARED_MAIL_HOSTS`).
        if pattern.hasPrefix("*@"), sharedMailHosts.contains(String(pattern.dropFirst(2))) {
            return .senderRule(pattern: SenderID.address(sender).lowercased())
        }
        return .senderRule(pattern: pattern)
    }

    static let sharedMailHosts: Set<String> = [
        "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com",
        "yahoo.com", "icloud.com", "me.com", "mac.com", "aol.com", "proton.me",
        "protonmail.com", "pm.me", "fastmail.com", "hey.com", "gmx.com", "zoho.com",
    ]

    static let needsUpdateMessage =
        "Your Passband server needs an update to learn this. For now, press t on an email to teach it about that sender."

    /// Whether save is open for this text.
    static func canSave(_ text: String) -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return !trimmed.isEmpty && trimmed.count <= maxLength
    }

    /// A general rule's text as a short undo label: first line, clipped.
    static func shortLabel(_ text: String, limit: Int = 40) -> String {
        let line =
            text.split(whereSeparator: \.isNewline).first.map(String.init)?
            .trimmingCharacters(in: .whitespaces) ?? ""
        guard line.count > limit else { return line }
        return String(line.prefix(limit - 1)).trimmingCharacters(in: .whitespaces) + "…"
    }
}
