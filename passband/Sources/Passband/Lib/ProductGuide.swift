// WHAT THE ASSISTANT KNOWS ABOUT PASSBAND ITSELF, and the only place it is
// written down. "How do I reply all?" is a question about the app, not about
// anybody's mail, and the ⌘K agent answers it from here.
//
// NONE OF THIS IS IN THE SYSTEM PROMPT. The guide is a few thousand tokens and
// most questions are about mail, so the prompt carries two lines saying the
// help tools exist (`promptIntro`, `promptTools`) and the content arrives as a
// tool result on the turn that asked for it:
//
//   * product_help(topic)  -> `text`: one hand-written topic.
//   * list_shortcuts()     -> `shortcuts`: Lib/ShortcutCatalog.swift, the same
//                             table the `?` overlay draws.
//   * find_setting(query)  -> `settings`: Lib/SettingsSearch.swift, the same
//                             index the Settings search field ranks.
//
// Two of the three are DERIVED from tables the app already keeps honest for
// its own reasons, so they cannot drift from what is on screen. The topics
// below are the part that can: they are prose about behaviour, written against
// the Mac app, and A FEATURE THAT CHANGES A KEY, A LABEL OR A PAGE OWES ITS
// TOPIC AN EDIT. The suite (Tests/ProductGuideTests.swift) pins the handful of
// keys the prose shares with the shortcut table, and nothing else can.
//
// HOUSE RULES for the prose, because a model reads it and then says it to a
// person:
//   * no em dashes or en dashes (a model that reads them writes them)
//   * say what the person presses and what happens, in the app's own words:
//     "For your eyes", "Sitrep", "done". Never the code's: no tiers, no FYE,
//     no "sealed".
//   * "the user", not "you": the reader of this text is the assistant.
//
// Foundation only, like the two tables it reads, so the suite compiles the
// three files with no app around them.

import Foundation

enum ProductGuide {

    // MARK: - topics

    /// One page of the guide. The raw value is what the model passes as
    /// `topic`, so it is spelled like a tool argument.
    enum Topic: String, CaseIterable, Sendable {
        case gettingAround = "getting_around"
        case sitrep
        case readingMail = "reading_mail"
        case writingMail = "writing_mail"
        case mailList = "mail_list"
        case triage
        case senderRules = "sender_rules"
        case reminders
        case search
        case loginCodes = "login_codes"
        case assistant
        case appAndAccounts = "app_and_accounts"
        case whatsNew = "whats_new"

        /// What the activity chip calls it: "read the guide: writing mail".
        var title: String {
            rawValue.replacingOccurrences(of: "_", with: " ")
        }

        /// What the topic covers, in the words somebody would ASK with. This
        /// is the model's whole basis for picking a topic, so a feature the
        /// line does not name is a feature it will look for somewhere else.
        var covers: String {
            switch self {
            case .gettingAround:
                "the pages, the sidebar rail, navigation keys, undo, zoom, theme"
            case .sitrep:
                "the home board: For your eyes, Reading, the calendar, shipments, billing and "
                    + "receipts cards, flow mode"
            case .readingMail:
                "an open email: moving between messages and emails, done, skip, unsubscribe, "
                    + "attachments, images, chat or email style, the contact card"
            case .writingMail:
                "reply, reply all, forward, new message, Cc and Bcc, send, undo send, send "
                    + "later, drafts, attachments, groups, signature, read tracking"
            case .mailList:
                "the Emails page: all mail, filters, sent, scheduled, spam, noise, browse all"
            case .triage:
                "how mail gets sorted, what done means, fixing a wrong call"
            case .senderRules:
                "rules for a sender (allow, mute, filter) and the Rules page"
            case .reminders:
                "remind me later: parking an email until a time"
            case .search:
                "search, the from: operator, result order, deeper search"
            case .loginCodes:
                "the Auth page: login codes, password resets, security alerts"
            case .assistant:
                "this ⌘K assistant, its key and model, connecting outside agents over MCP"
            case .appAndAccounts:
                "accounts, notifications, updates, usage, audit log, invites, privacy"
            case .whatsNew:
                "what the installed version brought"
            }
        }
    }

    /// The `topic` parameter's description: every topic and what it covers,
    /// built from the enum so a topic added above is offered without a second
    /// edit. This IS sent with every request, which is why each line is short.
    static let topicIndex: String =
        "Which page of the guide to read. "
        + Topic.allCases.map { "\($0.rawValue): \($0.covers)." }.joined(separator: " ")

    // MARK: - the system prompt's share

    /// Appended to the prompt's opening line, so the model knows that a
    /// question about the app is in scope at all.
    static let promptIntro = " You also answer questions about Passband itself."

    /// The one bullet the prompt's tool list gains. THE POINTER, NOT THE
    /// GUIDE: it names the tools and says when to reach for them, and stops.
    static let promptTools = """
        - HELP: product_help, list_shortcuts and find_setting explain Passband itself:
          how to do something, which key does it, where a setting lives. You know
          nothing about this app's controls except what they return, so call one
          before answering any "how do I..." question, and never guess a key.
        """

    // MARK: - product_help

    /// One topic's text. `runningVersion` is the build's marketing version, a
    /// parameter for the reason ReleaseNotes takes it as one: this file never
    /// reads a bundle, so the suite can ask as any version it likes.
    static func text(_ topic: Topic, runningVersion: String?) -> String {
        switch topic {
        case .gettingAround: gettingAround
        case .sitrep: sitrep
        case .readingMail: readingMail
        case .writingMail: writingMail
        case .mailList: mailList
        case .triage: triage
        case .senderRules: senderRules
        case .reminders: reminders
        case .search: search
        case .loginCodes: loginCodes
        case .assistant: assistant
        case .appAndAccounts: appAndAccounts
        case .whatsNew: whatsNew(running: runningVersion)
        }
    }

    private static let gettingAround = """
        Passband is one window. A narrow rail down the left edge switches pages, and \
        most actions have a single-letter key. Letter keys only fire when no text \
        field has focus, and a capital letter means Shift.

        Pages (number keys, no modifier):
        - 1 Sitrep: the home board. What needs the user, what there is to read, and today's records.
        - 2 Emails: every message in one list, plus sent and spam.
        - 3 Auth: login codes, password resets and security alerts.
        - 4 Rules: sender rules.
        - 5 Groups: named sets of recipients.
        - Usage and Settings sit at the bottom of the rail and have no number key. ⌘, opens Settings.

        Moving:
        - ⌘[ goes back and ⌘] goes forward.
        - Esc closes whatever is on top: an overlay, a panel, an open email. From a page it returns to the Sitrep.
        - / opens search, and so does ⌘F from any page. With an email open, ⌘F finds text in that email instead.
        - ⌘K opens this assistant. ⌘⇧K starts a new chat.
        - c or ⌘N starts a new message.
        - ? or ⌘/ shows the keyboard shortcut sheet.
        - ⌘R checks for new mail now.

        Looks:
        - \\ flips between light and dark. Theme and density (Compact or Comfortable) are in Settings > General > Appearance.
        - ⌘= zooms in, ⌘- zooms out, ⌘0 resets. Zoom scales the whole window and is remembered.

        Undo:
        - Most actions (done, remind, send, deleting a rule, group or draft, cancelling a scheduled send) put a chip at the bottom left for 5 seconds. Click it, or press u, to undo.
        - Inside an open email u means unsubscribe instead, so click the chip there. The Inbox menu also has "Undo Last Action".

        The window:
        - Closing the window does not quit Passband. Notifications keep arriving, and clicking the Dock icon reopens it.
        """

    private static let sitrep = """
        The Sitrep is the home page (key 1): a greeting, one headline sentence, and \
        the mail already sorted into what matters.

        Left column:
        - For your eyes: the to-do list. Mail with a real obligation or date, and live conversations with people the user writes to. It shows 10 rows, then a "more" button. Reading an email keeps it there; marking it done clears it.
        - Reading: newsletters, announcements and offers, one card per sender. Clicking a card opens that sender's latest email, and the left and right arrows step through the rest of theirs.

        Right column, records pulled out of the mail (the column hides when all four are empty):
        - Calendar: invites, reservations and changes to them. An item leaves once it starts.
        - Shipments: packages in flight and anything delivered today, with the carrier's own status.
        - Billing: bills, statements and bank alerts from the last day, or since the user last looked.
        - Receipts: today's purchases with their totals.
        - Clicking any record opens the email it came from.

        Keys on For your eyes (a row has to be highlighted first, with j / k or by hovering):
        - j / k or the arrows move. Enter opens the email.
        - e or d marks it done. r replies. h sets a reminder. v fixes a wrong triage call. t writes a rule for that sender.
        - c starts a new message, with or without a highlight.

        Flow mode:
        - Space, or the "flow mode" pill at the top of For your eyes, walks the list one email at a time, starting at the highlighted row or the first one.
        - In the reader during the walk, e is done and next, and s skips and leaves the email on the board. A small "FLOW MODE" card shows both keys. A summary card shows at the end.

        Hover tricks:
        - Hover a package and press e to clear it until its next update. Right-click has "Clear this package".
        - Hover a Reading card and press e to mark everything from that sender done. That one cannot be undone.

        Other things on the page:
        - Clicking the headline asks for a different one. The name in the greeting is set in Settings > General > You.
        - With nothing in For your eyes the page says there is no important mail, shows the day's tally and links to all mail.
        - "ingesting" in the top bar means new mail is still being sorted and will land on the next refresh.
        - Unsubscribes Passband is carrying out show in a tray at the bottom right.
        """

    private static let readingMail = """
        Opening an email (Enter on a row, or a click) opens the reader. A thread \
        reads oldest at the top and opens on the newest message.

        Moving:
        - j or the down arrow goes to the newer message, k or the up arrow to the older one.
        - The right arrow or l opens the next email in the list the user came from. The left arrow opens the previous one.
        - Esc or ⌘[ goes back. If a side panel is open, the first Esc closes that.

        Finding text in the open email:
        - ⌘F opens a find bar at the top right of the reader. Typing highlights every match in the thread and shows a count like "2 of 7".
        - Enter or ⌘G goes to the next match, Shift+Enter or ⌘⇧G to the previous one. The arrows on the bar do the same. It wraps around, across every message in the thread.
        - Esc closes the bar. A second Esc closes the email.
        - It searches the text that is shown. Open "show quoted history" on a message to include its quoted replies.

        Finishing:
        - e or d: done, and close the reader. The header has an "e done" button too.
        - E or D (with Shift): done, and open the next email.
        - S (Shift+S): skip to the next email and leave this one as it is.
        - h: remind me later, then move on. See the reminders topic.
        - In flow mode (Space on the Sitrep) plain e is done and next, and plain s skips.
        - On a Reading sender's stack, tapping e moves to that sender's next email, and holding e fills a ring and marks the whole stack done. E does the same and carries on to the next sender's card.
        - Done is Passband's own status. The email leaves the board and stays in the mailbox; nothing is archived or deleted in Gmail.

        Acting on the sender:
        - u: unsubscribe. Passband asks first, then usually does it in the background, and the reader closes. Progress shows at the bottom right of the Sitrep. If the sender's page needs a choice, a card there offers "Finish on their site". For spam or a sender Passband cannot verify, it opens the unsubscribe page in the browser instead. If the email has no unsubscribe link, the dialog offers "Block sender".
        - t: write a rule for this sender. v: fix a wrong triage call. See sender_rules and triage.
        - Clicking a sender's name or avatar opens their contact card: how much mail has gone each way, any rule that applies, recent threads, and buttons to email them or see their mail.

        How the thread looks:
        - b switches this thread between email cards and chat bubbles, and so does the two-icon switch at the top right. The choice sticks to that thread. The default is Settings > Mail > Threads. Reading mail is always cards.
        - Quoted history is folded behind "show quoted history".
        - Each message shows its To line; a caret reveals Cc and Bcc.
        - Clicking the subject copies it.

        Attachments and images:
        - Clicking an attachment card or an inline image previews it in Quick Look, where the arrow keys move between that message's attachments. The download arrow on a card saves the file.
        - Tracking pixels are removed from every email. If Settings > Mail > Remote images is set to "On demand", a bar on the email loads its images for that one email.

        Replying and forwarding from here are in the writing_mail topic.
        """

    private static let writingMail = """
        Reply, reply all and forward all start from an open email:
        - Reply: press r. A composer opens inline under the thread, answering the newest message, addressed to its sender.
        - Reply all: press Enter. The same inline composer, addressed to everyone on the newest message. It only works when no reply is already open and no text field has focus. An open reply is never converted, so to turn one into a reply all, close it with Esc and press Enter.
        - Forward: press f. It forwards the message selected with j / k, in the side composer, with the original shown under the note. Fill in To and send; the note is optional.
        - From the Sitrep or a row in the Emails list, r opens the email and starts the reply in one step. On a list row f is "search this sender", not forward.
        - The Mac reader has no reply buttons. The keys are the way in.

        New message:
        - c on the Sitrep, the Emails list or the reader, or ⌘N from anywhere.
        - The composer is a pane on the right, and the page beside it stays usable. A corner button turns it into a centred column.
        - The body is markdown, styled as it is typed.

        Recipients, Cc and Bcc:
        - In the side composer, "cc" and "bcc" are words on the To line. Clicking one opens that field.
        - In an inline reply, clicking the "replying to ..." line opens the To, Cc and Bcc fields.
        - Typing a name suggests people the user has written to. A comma, or a space after a full address, turns the text into a pill.
        - To move a recipient, drag the pill to another field, or right-click it for Move to To, Cc or Bcc, and Remove. An address lives in one field at a time.

        Groups:
        - "groups" on the To line addresses a saved group. New messages only, not replies or forwards.
        - Groups are made on the Groups page (key 5): n new, e or Enter edit, x delete.
        - Each group is set to go out one way: To (one email, everyone sees the list), Bcc (one email, nobody sees anyone else) or Individually (a separate email per person).

        Sending:
        - ⌘Enter sends. Plain Enter is a new line.
        - Undo send: the mail is held for 5 seconds, with a chip at the bottom left. Clicking it brings the composer back with everything in it. u does the same, except inside an open email, where u means unsubscribe.
        - If the outbound guard blocks a send because the body looks like it carries a secret, the composer reopens with a "send anyway" button (⌘⇧Enter). Any edit re-locks it.

        Send later:
        - The clock beside send, or ⌘⇧L, opens "Send later". Pick a preset or type a time ("friday", "in 3 hours", "the 24th"), then ⌘Enter schedules it.
        - It goes out on time even with Passband closed.
        - Scheduled mail waits on the Emails page (key 2) under "sent", in a "scheduled" group: s sends it now, x or Backspace cancels it, Enter previews it, and Enter again from the preview pulls it back into the composer to edit.

        Drafts:
        - Everything autosaves about a second after typing stops. Esc closes a composer and keeps the draft.
        - A reply draft comes back when r is pressed on the same message.
        - Other drafts are behind the "drafts" control beside the composer's title: up and down move, Enter opens, Backspace deletes.

        Attachments:
        - "attach" in the composer, dropping files anywhere on it, or pasting an image. 25 MB per file.
        - Pictures go inline where they are dropped. The "inline" / "attached" word on the tray row switches between showing a picture in the body and sending it as a file.

        Signature and tracking:
        - The signature is set in Settings > Mail > Signature. It is added to new messages and replies as the draft starts, so it can be edited per email.
        - "track opens" in the composer adds a tracking pixel to that one email. The default is Settings > Mail > Read tracking. A tracked message shows an "opened" chip once it has been opened.
        """

    private static let mailList = """
        The Emails page (key 2) is every message in one list.

        The list:
        - Grouped by day (Today, Yesterday and so on). Within a day the most important mail is first.
        - Each row has an importance meter (a check once it is done), the sender, the subject with Passband's one-line summary, and a pill naming where triage put it: For your eyes, Reading, Calendar, Shipments, Billing, Receipts, Auth or Noise.
        - Noise is mail triage judged not worth the user's attention. It has no page of its own any more: it is in this list with a grey Noise pill, and never on the Sitrep.
        - Done rows stay in place, dimmed. Older mail loads on scroll.

        Narrowing it:
        - The pills above the list: All, Important, Deadlines, Attachments, and Reminders (mail parked with a reminder).
        - The Categories menu hides whole categories and remembers the choice.
        - Esc steps back out: it clears the filter first, then returns to the Sitrep.

        Keys on a highlighted row:
        - j / k move, Enter opens the email.
        - e or d done, r reply, h remind, v fix triage, t write a rule for this sender.
        - f searches everything from this sender.
        - a opens "browse all": a side panel of mail ranked by importance, with a noise filter slider that + and - move.
        - T goes to Rules, g to Auth, A to the audit log.

        Sent:
        - The "sent" segment at the top. Rows show who the mail went to and, if read tracking fired, an eye with the open count.
        - Scheduled mail waits in a "scheduled" group above it: s send now, x or Backspace cancel, Enter preview.

        Spam:
        - The "spam" chip in the header opens the mail the email provider filtered. That is the provider's verdict, not Passband's: none of it was triaged and none of it sent a notification.
        - i on a row marks it not spam and moves it to the inbox, where Passband then triages it. Passband has no undo for that.
        """

    private static let triage = """
        Passband reads each incoming email and decides what kind of mail it is and \
        where it belongs. The user never sorts mail by hand.

        Where mail lands:
        - For your eyes: needs the user. Deadlines, obligations, and live conversations with people they write to.
        - Reading: newsletters, announcements and offers.
        - Records: the Calendar, Shipments, Billing and Receipts cards on the Sitrep.
        - Auth: login codes and security mail, kept on a page of their own.
        - Noise: everything else. Still in the Emails list, never on the Sitrep, never a notification.

        Done:
        - e or d marks an email done. It leaves the board and stays in the mailbox. Done is Passband's own status: nothing is archived, moved or deleted in Gmail.
        - The chip at the bottom left undoes it for 5 seconds.

        When triage gets one wrong:
        - Press v on the email (Sitrep, Emails list or the reader) for "Fix triage". Pick the correction: put it in For your eyes or take it out, add it to Reading or remove it, change its kind (bill, receipt, delivery, event, promotion and so on), or restrict or allow agent access to it. Typing filters the list and Enter applies.
        - A correction fixes that message and is kept to refine triage. It does not create a rule.
        - To change how a sender is handled from now on, write a sender rule: t on one of their emails. See sender_rules.
        - This assistant can explain why a specific email landed where it did.

        Other:
        - A reminder (h) parks an email and brings it back to For your eyes later. See reminders.
        - Dev mode (Settings > General > Developer) adds a "re-triage 7d" chip that re-runs triage on the last week. Passband is unavailable while that runs.
        - To work through For your eyes one email at a time, use flow mode: Space on the Sitrep. See the sitrep topic.
        - No key archives mail in Gmail. This assistant can archive a message when asked, behind a confirmation card.
        """

    private static let senderRules = """
        A sender rule tells triage what the user wants from a sender, in their own \
        words.

        What a rule is:
        - A match: one address, or a whole domain.
        - A sentence saying what the user wants ("only order confirmations", "always show these").
        - A disposition, which Passband works out from the sentence unless it is set by hand: allow (always surface this sender), mute (keep them out of the Sitrep unless something escalates), or filter (the sentence decides what gets through).

        Making one:
        - From an email: press t in the reader, or on a highlighted row on the Sitrep or in the Emails list. The editor opens for that sender and may suggest a sentence; Tab takes the suggestion.
        - From the Rules page (key 4, or T on the Emails page): n.
        - In the editor: Enter saves, Option+Enter adds a line, Esc cancels and keeps what was typed. "advanced" (⌘D) shows the match pattern and the disposition picker, and with it open Tab cycles the disposition.
        - This assistant can write one when asked. It only adds rules; it never changes one the user wrote.

        Managing them:
        - On the Rules page: j / k move, e or Enter or a click edits, x deletes (with undo).
        - Rules are grouped by what they do (allow, filter, mute) and then by domain.
        - A rule shapes mail from now on. It does not move mail already on the board. The exception is "Block sender", offered when unsubscribing from an email that has no unsubscribe link: it mutes that address and clears their open mail.
        - Rules live in Passband only. They never change anything in Gmail.
        """

    private static let reminders = """
        A reminder parks an email and brings it back later.

        Setting one:
        - Press h on the email: on the Sitrep, in the Emails list, or in the reader, where it then moves on to the next email.
        - Pick a preset (in an hour, this evening, tomorrow morning, tomorrow evening, this weekend, next week, in a week, next month) or type a time: "friday", "aug 30", "in 3 hours", "the 24th". Each choice shows the exact time it means. Enter sets it.

        What happens:
        - The email is marked done until then, so it leaves the board.
        - At that time it comes back to For your eyes, whatever it was before.
        - The chip at the bottom left undoes it for 5 seconds.

        Finding and changing them:
        - Emails page (key 2), then the Reminders pill: parked mail in the order it comes back.
        - Press h again on one to move its time.
        - Backspace on a row there cancels the reminder. The email stays done.
        """

    private static let search = """
        Opening:
        - / from any page, or ⌘F when no email is open. Reopening search brings back the last query, selected so typing replaces it.
        - With an email open, ⌘F finds text inside that email instead (see reading_mail). / still opens search from there.
        - f on a row in the Emails list searches everything from that sender.

        What it searches:
        - Subjects and bodies, by keyword and by meaning. Subject words count more, word forms match (password finds passwords), and a quoted phrase has to appear exactly.
        - Login codes and spam are not searchable.
        - Unfinished mail is listed first, and the matching words are highlighted.

        Operators:
        - from: narrows to a sender. Typing from: opens a menu of the people who write to the user; the arrows pick and Enter or Tab accepts.
        - after: and before: narrow by date.

        Order:
        - A switch in the panel, also at Settings > Mail > Search. "Relevance + recency" ranks newer mail higher when matches are close. "Best match" ranks on the words alone.

        Keys:
        - Down and up move through the results (j / k too, when the field does not have focus). Enter opens the selected hit, or the first one.
        - ⌘Enter asks the agent. Esc closes search.
        - With the field empty, the last 10 searches are listed. Right-click clears them.

        Deeper search:
        - When a search reads like a question ("where is the wifi password"), an agent reads the mail alongside the results and points at the answer with email cards and one line.
        - Settings > Assistant > Deeper search sets when it runs: Automatic, On request (a button to press) or Off. It has a model setting of its own.
        - "Open in chat", or ⌘Enter, continues the same question in this assistant with room for follow-ups.
        """

    private static let loginCodes = """
        Login codes, password resets, sign-in links and security alerts are kept \
        apart from the rest of the mail, on the Auth page (key 3, or g on the Emails \
        page).

        Why:
        - This mail is sensitive, so it is kept out of search, out of this assistant and out of any connected agent. The assistant cannot read a code, by design.
        - The list shows only who sent it and the subject. A body is fetched only when the user reveals it, and every reveal is written to the audit log.

        Using it:
        - j / k move through the list. Filters: Everything, Last hour, Codes only, Risk.
        - r or Enter reveals. A login code shows its digits right there, and c copies it. Other kinds open a one-time reveal panel that is cleared when it closes.
        - Clicking a row opens the whole message in that reveal panel.
        - Sign-in alerts and resets from the last week ask for a decision: "That was me" or "Not me".
        - The auto-shredder card can move old auth mail to Gmail's trash after a set number of days.

        Arrival:
        - Auth mail shows up within seconds, without waiting for triage. The Auth icon on the rail shows live codes counting down.
        """

    private static let assistant = """
        This assistant:
        - ⌘K opens it from anywhere. Esc closes the bar, and the conversation stays until ⌘⇧K (new chat) or an account switch.
        - If an email is open when the question is asked, an "in:" chip names it and "this email" means that one.
        - It can search and read mail, look up shipments, receipts, bills and calendar items, explain why an email was triaged the way it was, show emails as clickable cards, and answer questions about Passband itself.
        - It marks things done, writes sender rules and saves drafts without asking, because those stay inside Passband.
        - Archiving, labelling, sending and unsubscribing each show a confirmation card with Confirm and Decline. A send card also has "Edit in composer", which hands the message to the composer instead of sending it.
        - It cannot see login codes or password resets.
        - The sources it used are listed under the answer. Clicking one opens the email.

        How it is paid for (Settings > Assistant):
        - "Chats via" chooses between the Passband relay, which counts against the plan's monthly assistant budget, and "My own key", an Anthropic API key kept in this Mac's keychain. Where the relay is not offered, a key is required.
        - "Model" picks Haiku (fast and cheap) or Opus (smartest, pricier).
        - The Usage page, at the bottom of the rail, shows asks, tokens and estimated cost.

        Deeper search is the same agent working inside search. See the search topic.

        Connecting an outside agent (Settings > Agents):
        - Passband has an MCP endpoint that Claude Code, Claude Desktop, Codex, Cursor or OpenClaw can connect to. The pane shows the address and a copyable setup snippet for each.
        - Such an agent can read ranked updates and whole threads, search mail, list bills, deadlines and packages, and set sender rules. It cannot send, archive, label or delete, cannot see login codes, and cannot change anything in Gmail.
        - Hosted Passband does not offer this yet. A self-hosted daemon does.
        """

    private static let appAndAccounts = """
        Accounts:
        - Add one from the Accounts menu ("Add Account…") or Settings > Account: sign in with Gmail, or connect a self-hosted server with a pairing code or an API token.
        - Switch with ⌘1 to ⌘9, in the order listed in Settings > Account, or from the account badge at the bottom of the rail once there are two or more. Switching starts a fresh assistant chat.
        - Rename an account by editing its name in Settings > Account. "remove", then confirm, takes it off this Mac. On the last account the button reads "disconnect".
        - Notifications arrive for every account, not only the one on screen.
        - If the Google sign-in expires, a banner on the Sitrep says Passband cannot read the mail. On a hosted account it has a Reconnect button.

        Notifications:
        - A banner fires for urgent mail, deadlines and other mail important enough to surface, and when a tracked email is opened. Everything else arrives silently.
        - Only urgent and deadline banners play a sound. It is chosen in Settings > General > Notifications, where "Test banner" posts one.
        - While Passband is the front window, banners go to Notification Center only. Clicking a banner opens that message.

        Updates:
        - Passband checks for a new version every hour and downloads it in the background. It installs the next time Passband quits.
        - When one is ready, a card at the bottom of the window offers "Restart" to install it now. "Later" hides the card for an hour.
        - While a new version is waiting, a down-arrow icon sits in the sidebar above the share icon. Clicking it brings the card back.
        - The Passband menu has "Check for Updates…". The installed version is printed at the bottom right of Settings.
        - Settings > General > Help has "What's new" for the release notes and "Practice inbox" to replay the tour.

        Usage and audit:
        - Usage, at the bottom of the rail: mail in and out, signal against noise, triage spend and assistant usage, over 7, 30 or 90 days.
        - Audit log (Settings > Audit, or A on the Emails page): every action taken on the mail by the user or by an agent, with undo for the reversible ones.

        Sharing:
        - The gift icon on the rail, or Settings > Account > Invites, sends a friend an invite from the user's own address. Shown only where the server supports it.
        - Settings > Account also has the link to the iPhone app on TestFlight.

        Privacy:
        - Settings > Privacy sets telemetry to Full, Minimal or None.
        """

    /// The newest release this build is at or past, read off the one table the
    /// What's New card reads (Lib/ReleaseNotes.swift), so the chat and the card
    /// cannot describe different releases.
    private static func whatsNew(running: String?) -> String {
        let reopen =
            "The app shows this once per version as a What's New card. Settings > General > "
            + "Help > What's new opens it again."
        let note = running.flatMap { ReleaseNotes.newest(atOrBelow: $0) }
        guard let note = note ?? ReleaseNotes.all.first else {
            return "No release notes are on record for this build. " + reopen
        }
        var lines = [
            running.map { "This Mac is running Passband \($0)." }
                ?? "This build does not report its version; these are the newest notes on record.",
            "Release \(note.version), \(note.date): \(note.headline)",
        ]
        let headings: [(ReleaseSurface, String)] = [
            (.app, "In the Mac app:"),
            (.ios, "On the iPhone (it updates separately, through TestFlight):"),
            (.daemon, "In the daemon (the server that sorts the mail):"),
        ]
        for (surface, heading) in headings {
            let items = note.items(on: surface)
            guard !items.isEmpty else { continue }
            lines.append("")
            lines.append(heading)
            for item in items {
                // A headliner's second line says how to use the thing, which
                // is the half of a release note somebody asks a question for.
                lines.append("- " + [item.text, item.detail].compactMap { $0 }.joined(separator: ". "))
            }
        }
        lines.append("")
        lines.append(reopen)
        return lines.joined(separator: "\n")
    }

    // MARK: - list_shortcuts

    /// The `?` overlay's table, as text. Key caps separated by a space, the
    /// way the overlay sets them side by side.
    static func shortcuts() -> String {
        var lines = [
            "Passband keyboard shortcuts, grouped by where they work. A single letter only "
                + "fires when no text field has focus, and a capital letter means Shift. "
                + "Pressing ? shows this same list in the app."
        ]
        for group in ShortcutCatalog.groups {
            lines.append("")
            lines.append(group.title)
            for item in group.items {
                lines.append("- \(item.keys.joined(separator: " ")): \(item.desc)")
            }
        }
        return lines.joined(separator: "\n")
    }

    // MARK: - find_setting

    /// How many matches one lookup names. Past this the answer is a list to
    /// read rather than a place to go.
    private static let settingsCap = 5

    /// Where a setting lives and what it does, best match first. No query, or
    /// a query that matches nothing, answers with the whole index: it is a
    /// couple of dozen lines, and a model that asked in the wrong words can
    /// pick the right row itself, which beats a second guess at a keyword.
    static func settings(matching query: String?) -> String {
        let opening =
            "Settings opens with ⌘, or from the bottom of the rail. Pressing / there searches "
            + "settings by description."
        let asked = query?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let found = asked.isEmpty ? [] : Array(SettingsSearch.entryMatches(asked).prefix(settingsCap))
        if !found.isEmpty {
            return ([opening, "", "Best matches for \"\(asked)\", best first:"] + found.map(line))
                .joined(separator: "\n")
        }
        let everything = SettingsSearch.entries.filter { $0.card.isAvailable }.map(line)
        let heading =
            asked.isEmpty
            ? "Every setting:"
            : "Nothing matched \"\(asked)\". Every setting, to pick from:"
        return ([opening, "", heading] + everything).joined(separator: "\n")
    }

    /// "Theme (General > Appearance): what it does." A card named after its
    /// own section is said once: "Mail > Mail" is a stutter, not a path.
    private static func line(_ entry: SettingsEntry) -> String {
        let section = entry.card.section.label
        let card = entry.card.label
        let path = section == card ? section : "\(section) > \(card)"
        return "- \(entry.title) (\(path)): \(entry.blurb)"
    }
}
