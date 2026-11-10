// EVERY WORD THE DISCOVERY PAGE AND RING SAY, in one place: the seven sets'
// names and lines, their keys, the page's headings and the found moment. The
// raw values on `FeatureSet` are persisted and leave the machine, so the names
// a person reads live here and are free to change.
//
// THE APP'S VOICE. Calm, plain, warm, second person, short. GOOD NEWS ONLY:
// a line says what a set gives you, never how many are left or what you have
// not done. No em dashes anywhere (a test-free rule, so read before you add).

import SwiftUI

extension FeatureSet {
    /// What the page and the moment call the set.
    var title: String {
        switch self {
        case .clear: "The basics"
        case .flow: "Flow mode"
        case .later: "Not now"
        case .teach: "Teach Passband"
        case .find: "Find anything"
        case .read: "Your reading"
        case .sweep: "Smart clear"
        }
    }

    /// The row's one line. An invitation for somebody who has not tried it,
    /// and still true for somebody who has.
    var line: String {
        switch self {
        case .clear: "Open a message, mark it done, and bring it back if you change your mind."
        case .flow: "Press Space and take For Your Eyes one email at a time, without going back to the board."
        case .later: "Not the right moment? Send a message away and it comes back when you said."
        case .teach: "Tell Passband what matters in your own words, fix a sort, or unsubscribe in one key."
        case .find: "Search everything you have, or simply ask your inbox a question."
        case .read: "Newsletters gather into a stack you read at your own pace and clear in one go."
        case .sweep: "One press clears what has run its course: events that passed, offers that ended, mail you never open."
        }
    }

    /// The keys that reach the set, as the row's keycaps show them. A HOLD is its
    /// own cap label rather than a second cap, so "hold e" reads as one key.
    var keys: [String] {
        switch self {
        case .clear: ["e", "u"]
        case .flow: ["space", "E", "S"]
        case .later: ["h"]
        case .teach: ["t", "v", "u"]
        case .find: ["/", "⌘K"]
        case .read: ["hold e"]
        // No key of its own: it is the pill in Reading's corner.
        case .sweep: []
        }
    }

    // MARK: - the guide

    /// The guide's steps, in the order a hand does them. A step with no key
    /// is something Passband does, not something to press. Keys are labelled
    /// as the app's own hints label them.
    var guide: [GuideStep] {
        switch self {
        case .clear: [
            .init("↵", "Open any message from the board."),
            .init("e", "Done. It leaves the board and the next one is waiting."),
            .init("u", "Changed your mind? It comes straight back."),
        ]
        case .flow: [
            .init("space", "On the board, press Space. For Your Eyes opens one email at a time."),
            .init("E", "Done, and straight on to the next one."),
            .init("S", "Not sure yet? Save it for later and keep walking."),
            .init(nil, "At the end you are back on the board, with the list behind you."),
        ]
        case .later: [
            .init("h", "On any message, press h and say when you want it back."),
            .init("↵", "Set it. The message leaves the board."),
            .init(nil, "When the time comes, it is back at the top as if it had just arrived."),
        ]
        case .teach: [
            .init("t", "Teach Passband what you want, in your own words."),
            .init("v", "Sorted wrong? Say where it belongs."),
            .init("u", "Inside a newsletter, unsubscribe without hunting for the link."),
            .init(nil, "Passband sorts with all of it in mind from then on."),
        ]
        case .find: [
            .init("/", "Search every message you have. Results arrive as you type."),
            .init("⌘K", "Ask a question in plain words and get the answer from your mail."),
        ]
        case .read: [
            .init(nil, "Newsletters gather in Your reading and stay off the board."),
            .init("↵", "Open one whenever you like. Nothing in the stack is waiting on you."),
            .init("hold e", "Finished, or not in the mood? Hold e to clear the whole stack."),
        ]
        case .sweep: [
            .init(nil, "Press smart clear at the top right of Your reading."),
            .init(nil, "Passband clears what has plainly run its course, and says how much and why."),
            .init("u", "Not sure about it? Undo puts all of it back."),
        ]
        }
    }

    // MARK: - the found moment

    /// The moment's headline: what was found, in the past tense of the thing
    /// the person just did.
    var foundTitle: String {
        switch self {
        case .clear: "You found the basics."
        case .flow: "You found flow mode."
        case .later: "You found Not now."
        case .teach: "You taught Passband something."
        case .find: "You found search."
        case .read: "You cleared a reading stack."
        case .sweep: "You found smart clear."
        }
    }

    /// The moment's second line: what it means from here. Never a next task.
    var foundLine: String {
        switch self {
        case .clear: "Done is one key, and nothing is ever lost."
        case .flow: "Press Space whenever you want to walk the list again."
        case .later: "It will be back when you asked for it."
        case .teach: "It sorts with that in mind from now on."
        case .find: "Anything in your mail is a few letters away."
        case .read: "New issues will gather there for whenever you like."
        case .sweep: "Whatever has run its course is one press away."
        }
    }

    /// The state in words, for VoiceOver. Unseen is not "locked" or "not
    /// found": it is something new to try.
    func stateWords(_ state: SetState) -> String {
        switch state {
        case .unseen: "new to try"
        case .found: "unlocked"
        case .kept: "unlocked, and a habit"
        }
    }
}

struct GuideStep {
    let key: String?
    let text: String
    init(_ key: String?, _ text: String) {
        self.key = key
        self.text = text
    }
}

/// The page's own words.
enum DiscoveryCopy {
    /// The header bar, in the routed pages' title + small caps pairing.
    static let pageTitle = "Discover"
    static let pageSubtitle = "ways through your mail"

    static let headline = "More than one way through."
    /// Everything unlocked: the top section has gone, and the page says so
    /// calmly rather than throwing a party.
    static let completeHeadline = "You know every way through."

    /// The owner's word, verbatim. Do not restyle it into "Found".
    static let unlockedHeading = "Unlocked"
    /// Over the one card at the top: the most everyday set not yet unlocked.
    static let startHere = "START HERE"
    static let nextUp = "TRY THIS NEXT"
    /// The rest of what is new, under the card.
    static let moreHeading = "After that"
    static let keptTag = "A HABIT"
    static let freshTag = "JUST UNLOCKED"

    /// Over the guide's title. Unseen is an invitation, never "locked".
    static func guideEyebrow(_ state: SetState) -> String {
        switch state {
        case .unseen: "NEW TO TRY"
        case .found: "UNLOCKED"
        case .kept: "A HABIT"
        }
    }

    static let momentEyebrow = "SOMETHING NEW"
    static let ringLabel = "Ways through your mail"
}
