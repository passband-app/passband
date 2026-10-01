// EVERY WORD THE DISCOVERY RING SAYS, in one place: the eight sets' names and
// lines, their keys, and the found moment. The raw values on `FeatureSet` are
// persisted and leave the machine, so the names a person reads live here and
// are free to change.
//
// THE TOUR'S VOICE. Calm, plain, warm, second person, short. GOOD NEWS ONLY:
// a line says what a set gives you, never how many are left or what you have
// not done. No em dashes anywhere (a test-free rule, so read before you add).

import SwiftUI

extension FeatureSet {
    /// What the menu and the moment call the set.
    var title: String {
        switch self {
        case .clear: "The basics"
        case .flow: "Flow mode"
        case .later: "Not now"
        case .teach: "Teach Passband"
        case .find: "Find anything"
        case .read: "Your reading"
        case .glance: "At a glance"
        case .write: "Writing"
        }
    }

    /// The menu's one line. An invitation for somebody who has not tried it,
    /// and still true for somebody who has.
    var line: String {
        switch self {
        case .clear: "Open a message, mark it done, and bring it back if you change your mind."
        case .flow: "Press Space and take For Your Eyes one email at a time, without going back to the board."
        case .later: "Not the right moment? Send a message away and it comes back when you said."
        case .teach: "Tell Passband what matters in your own words, fix a sort, or unsubscribe in one key."
        case .find: "Search everything you have, or simply ask your inbox a question."
        case .read: "Newsletters gather into a stack you read at your own pace and clear in one go."
        case .glance: "Plans, packages, banking, receipts and sign-in codes, answered without opening a thing."
        case .write: "Write, reply, send it later, or write to a whole group at once."
        }
    }

    /// The keys that reach the set, as the keycaps show them. A HOLD is its
    /// own cap label rather than a second cap, so "hold e" reads as one key.
    var keys: [String] {
        switch self {
        case .clear: ["e", "u"]
        case .flow: ["space", "E", "⇧S"]
        case .later: ["h"]
        case .teach: ["t", "v", "u"]
        case .find: ["/", "⌘K"]
        case .read: ["hold e"]
        case .glance: ["e", "c"]
        case .write: ["c", "r"]
        }
    }

    /// The set's mark, for the shared placeholder vignette and anywhere a
    /// glyph has to stand in for the set.
    var symbol: String {
        switch self {
        case .clear: "checkmark.circle"
        case .flow: "arrow.forward.circle"
        case .later: "clock"
        case .teach: "text.bubble"
        case .find: "magnifyingglass"
        case .read: "books.vertical"
        case .glance: "shippingbox"
        case .write: "square.and.pencil"
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
        case .glance: "Answered at a glance."
        case .write: "A new way to write."
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
        case .glance: "No email needed opening."
        case .write: "It is always there in the composer."
        }
    }

    /// The state in words, for VoiceOver and the tile's tag. Unseen is not
    /// "locked" or "not found": it is something new to try.
    func stateWords(_ state: SetState) -> String {
        switch state {
        case .unseen: "new to try"
        case .found: "found"
        case .kept: "a habit"
        }
    }
}

/// The menu's own words.
enum DiscoveryCopy {
    static let eyebrow = "WAYS THROUGH YOUR MAIL"
    static let headline = "More than one way through."
    static let subline = "Each of these is ready whenever you are. Try whichever looks good."
    static let momentEyebrow = "SOMETHING NEW"
    static let ringLabel = "Ways through your mail"
}
