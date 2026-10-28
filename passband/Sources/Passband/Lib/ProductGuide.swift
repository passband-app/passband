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
        case calendar
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
                "the pages, the sidebar rail, the Discover page and its progress ring, navigation "
                    + "keys, undo, zoom, theme"
            case .sitrep:
                "the home board: For your eyes, Reading, the calendar, shipments, billing and "
                    + "receipts cards, flow mode"
            case .calendar:
                "the Calendar page: month, week and agenda, events guessed from mail, connecting "
                    + "Google Calendar, new, edited and deleted events, guests"
            case .readingMail:
                "an open email: moving between messages and emails, done, skip, unsubscribe, "
                    + "attachments, images, chat or email style, the contact card, translated "
                    + "mail, show original, languages you read"
            case .writingMail:
                "reply, reply all, forward, new message, Cc and Bcc, send, undo send, send "
                    + "later, drafts, attachments, groups, signature, read tracking"
            case .mailList:
                "the Emails page: all mail, filters, sent, scheduled, spam, noise, browse all"
            case .triage:
                "how mail gets sorted, what done means, fixing a wrong call"
            case .senderRules:
                "rules for a sender (allow, mute, filter) and the Rules pane in Settings"
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

    /// THE TOPICS ON OFFER for this mailbox. The calendar topic only while
    /// the Calendar page is (`AppStore.calendarPageAvailable`, handed in):
    /// a guide that described a page the rail does not have would have the
    /// assistant send somebody looking for it. With the page off, this is
    /// the list from before the calendar existed, byte for byte, and so is
    /// everything else the guide says.
    static func topics(calendar: Bool) -> [Topic] {
        calendar ? Topic.allCases : Topic.allCases.filter { $0 != .calendar }
    }

    /// The `topic` parameter's description: every topic on offer and what it
    /// covers, built from the enum so a topic added above is offered without
    /// a second edit. This IS sent with every request, which is why each line
    /// is short.
    static func topicIndex(calendar: Bool) -> String {
        "Which page of the guide to read. "
            + topics(calendar: calendar).map { "\($0.rawValue): \($0.covers)." }.joined(separator: " ")
    }

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
    ///
    /// `onPhone` is the same kind of fact, handed in for the same reason: the
    /// iPhone app is numbered on its own line and keeps its own release notes
    /// (Lib/PhoneReleaseNotes.swift), so "what's new" there is a different
    /// table read against a different version.
    ///
    /// `calendar` is whether the Calendar page is on for this mailbox: it
    /// puts the page into the getting-around list. The calendar topic itself
    /// is asked for only while it is on (`topics(calendar:)`).
    static func text(_ topic: Topic, runningVersion: String?, onPhone: Bool = false, calendar: Bool = false) -> String {
        keyed(page(topic, runningVersion: runningVersion, onPhone: onPhone, calendar: calendar), calendar: calendar)
    }

    /// THE DIGIT THAT OPENS A PAGE IS NEVER WRITTEN IN THE PROSE. A topic
    /// says "the Emails page (key {key:emails})" and the number is put in
    /// here, from the Navigate row of the table the `?` sheet draws for this
    /// mailbox (`ShortcutCatalog.groups(calendar:)`): Emails is 2 with the
    /// Calendar page off and 3 with it on, and the guide says whichever the
    /// sheet does. With the page off the text is main's, byte for byte.
    ///
    /// A name the row does not have is left as written, braces and all, so
    /// the suite's "no brace left in any topic" check catches a page renamed
    /// in one place only.
    static func keyed(_ text: String, calendar: Bool) -> String {
        guard text.contains("{key:") else { return text }
        guard let row = ShortcutCatalog.groups(calendar: calendar).first(where: { $0.title == "Navigate" })?.items.first
        else { return text }
        var out = text
        for (name, key) in zip(row.desc.components(separatedBy: " · "), row.keys) {
            out = out.replacingOccurrences(of: "{key:\(name)}", with: key)
        }
        return out
    }

    private static func page(_ topic: Topic, runningVersion: String?, onPhone: Bool, calendar: Bool) -> String {
        switch topic {
        case .gettingAround: gettingAround(calendar: calendar)
        case .sitrep: sitrep
        case .calendar: Self.calendar
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
        case .whatsNew: onPhone ? whatsNewOnPhone(running: runningVersion) : whatsNew(running: runningVersion)
        }
    }

    /// The pages on the number keys, in rail order, Calendar second while
    /// it is on (`CalendarGate.numbered`'s rule, restated: this file stands
    /// alone under test). Numbered when the text is built, so a page added
    /// renumbers the ones after it here exactly as it does on the rail.
    private static func numberedPages(calendar: Bool) -> String {
        var pages = [
            "Sitrep: the home board. What needs the user, what there is to read, and today's records.",
            "Emails: every message in one list, plus sent and spam.",
            "Auth: login codes, password resets and security alerts.",
            "Rules: sender rules. A pane in Settings, not on the rail; the key opens Settings > Rules.",
            "Groups: named sets of recipients. A pane in Settings, not on the rail; the key opens Settings > Groups.",
        ]
        if calendar {
            pages.insert(
                "Calendar: events guessed from mail and, once connected, Google Calendar. ⇧C also "
                    + "opens it. See the calendar topic.", at: 1)
        }
        return pages.enumerated().map { "- \($0.offset + 1) \($0.element)" }.joined(separator: "\n")
    }

    private static func gettingAround(calendar: Bool) -> String {
        gettingAroundHead + numberedPages(calendar: calendar) + "\n" + gettingAroundTail
    }

    private static let gettingAroundHead = """
        Passband is one window. A narrow rail down the left edge switches pages, and \
        most actions have a single-letter key. Letter keys only fire when no text \
        field has focus, and a capital letter means Shift.

        Pages (number keys, no modifier):

        """

    private static let gettingAroundTail = """
        - Usage and Settings sit at the bottom of the rail and have no number key. ⌘, opens Settings.
        - The ring just above them opens Discover: a list of the ways through mail (the basics, flow mode, reminders, search, teaching Passband, the reading stack, smart clear) with a short guide to each on the right. The ring fills clockwise as the user tries each one on their own mail, and a feature used on three separate days is marked a habit. Nothing ever empties it. Esc leaves the page.

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

        Clearing out Reading (a pill at the top right of the Reading card, on the Mac):
        - "smart clear" clears what has plainly run its course in one press: events that already happened, offers that ended, mail that is no longer current, promotions over a week old, and promotions from senders the user never opens or only glances at. It can take up to about 20 seconds. A receipt card at the bottom right then says how many emails were cleared and why, with an Undo button. Undo, or the u key, puts all of it back, and stays for 30 seconds.
        - "View full list" on that receipt opens a page listing every email that was cleared: its subject, its sender and why. The undo timer is paused while that page is open. On it, u undoes everything, and Return or Esc confirms and goes back with the timer carrying on from where it stopped.

        Other things on the page:
        - Clicking the headline asks for a different one. The name in the greeting starts as the first name on the mailbox's Google profile (or the first part of the address when that is not available) and is changed in Settings > General > You.
        - With nothing in For your eyes the page says there is no important mail, shows the day's tally and links to all mail.
        - "ingesting" in the top bar means new mail is still being sorted and will land on the next refresh.
        - Unsubscribes Passband is carrying out show in a tray at the bottom right.
        - If a sender keeps emailing after an unsubscribe, a notice appears at the bottom left with "Block sender" and "Keep". When that sender also sends mail worth having, the notice adds "Smart block": it shows a rule that keeps the important kinds and drops the promotion, and saves it as a filter rule on that address.
        """

    private static let calendar = """
        The Calendar page is on for this mailbox. Passband turns it on per mailbox from \
        its side, and there is no setting for it in the app. It is second on the rail: \
        key {key:calendar}, or ⇧C from anywhere. Emails is then key {key:emails}, Auth {key:auth}, Rules {key:rules} and Groups {key:groups}. \
        It is on the Mac only; the iPhone has no Calendar page yet.

        What is on it:
        - Events guessed from mail (invitations, bookings, plans in a thread), drawn solid, striped or outlined by how likely they are. They need no permission.
        - Real events from Google Calendar, once it is connected, in each calendar's own colour.
        - Connecting: the "Connect calendar" card or button opens Google's consent screen. Unticking calendar there leaves the mailbox working and simply connects no calendar.

        Views:
        - m shows the month, w the week, a the agenda. The month is the default. Trips and stays are bars across the days they cover, and "+N more" is what does not fit in a day.
        - In the month the arrow keys move the day, [ and ] go to the previous and next month, and t goes to today. Enter opens the selected day: its hours in a column beside the month.
        - The week shows seven days side by side under one column of hours, with all-day events and trips in a strip across the top. The left and right arrows move the day, [ and ] go to the previous and next week, t goes to today, and j / k or the up and down arrows step through the selected day's events.
        - j / k move the selection. Enter opens the mail behind an event guessed from mail. y marks it going (again: not going), x dismisses it.

        Making and changing events (only on the user's own calendars):
        - n, or "New event", opens the event pane. It takes half the window, like the composer, and the month stays usable beside it: clicking a day sets the event's date.
        - Pressing and dragging down a day's hours (the month's day column, or any day of the week view) sweeps a time, in quarter hours; letting go opens the event pane with that day and those times.
        - e edits the selected event. ⌫ deletes it, after asking once.
        - The pane has the title, the calendar, all day, the day and times, a location that suggests places from Apple Maps as it is typed (arrows and Enter pick one), guests and notes.
        - A timed event shows its end day beside its end time. Left alone it follows the start day, or the next day when the end time is at or before the start; typing a day into it sets it, for an event that runs overnight or over several days.
        - A new event's title reads itself as it is typed, from its end. "dinner friday 4pm" and "Dinner with Steven Friday 4pm" both fill in Friday and 4pm, and the day and times they filled are tinted. Capitals make no difference. Only the last words are read: a closing "with steven" is skipped over ("dinner friday 4pm with steven"), and a day or a time anywhere earlier is part of the title ("Black Friday sale", "Friday dinner", "4pm standup").
        - The title is saved exactly as typed unless the day or time is led by "on", "at" or "@". Then those words come out: "dinner with steven on friday 4pm" is saved as "Dinner with Steven", and a "saves as" line under the title shows the title that will be saved. That line appears only when the saved title differs from the typed one. Without the lead nothing comes out: "taco tuesday" goes on Tuesday and is still called "taco tuesday".
        - It is all or nothing. When the last words cannot all be read, or name two different days or times, or a weekday and a date that do not match, or a year already over, nothing is filled in. A time needs its am or pm ("4pm"), or "at" and a colon ("at 4:30"); a bare number is not read, so "dinner friday at 7" fills in nothing and wants "7pm". A range has to end after it starts ("4-6pm", or an evening into the morning like "10pm-2am").
        - "for", "before", "after", "until", "of" and "the" in front of a day or time make it part of the title: "out of office until friday" and "prep for friday" fill in nothing. "ship v2 by friday" fills in Friday and keeps its words. "sat", "sun", "wed", "mar", "may" and "jun" are read only after "on", "this" or "next", or beside another day or time ("gym on sat", "gym sat 9am").
        - The line under the when fields says what will be saved, with the year when it is not this one and "in the past" when the start has gone by. A day or time set by hand is never overwritten, and words in the title that would have set it stay in the title. Deleting the words puts back what was there, and Esc in the title (or ⌘Z) keeps the title exactly as typed and puts the day and times back.
        - "with steven" in a new event's title also suggests a guest: the Steven the user most recently exchanged mail with, in a dashed box on the guests line with a checkmark and an x. The checkmark adds them as a guest, the x dismisses the suggestion. Tab from the title moves to it; there Enter adds and ⌫ dismisses. A suggestion is not a guest, and nobody is invited, until the checkmark and then the save control.
        - ⌘↵ saves from anywhere in the pane. Plain Enter saves from a one-line field, but only when the event has no guests. With guests, plain Enter does nothing: only ⌘↵ or a click on the save control does. An event the assistant drafted is the exception, described below.
        - Google, not Passband, emails the guests, and only when the control says so: "add and invite 3", "save and tell guests" or "delete and tell guests". "save without telling" and "delete without telling" change the event and tell nobody.
        - Esc closes the place list first, then the delete question, then the pane. The event pane and the composer share that half of the window, so only one is open at a time.

        The assistant and the calendar:
        - Asked to put something on the calendar, the assistant drafts it: the event pane opens on the Calendar page filled in with a title, the times, a place and any guests the user asked for. The title and the place are the only text it supplies, one line each. It writes no notes; those are the user's to type.
        - It never adds the event and never sends anything. The user reads the draft and adds it with the pane's own control.
        - The keys wait. A drafted pane does not take the keyboard, and Enter and ⌘↵ do nothing in it (a note says "Read it, then click add") until the user has clicked in the pane or typed in it. After that they work as in any event without guests.
        - When the draft has guests, only the button invites them: a click on the control that names them, such as "add and invite 2". ⌘↵ never does, however much of the pane has been clicked or typed in, and plain Enter never saves an event with guests. The line above the button says so.
        - If an email is open when the assistant drafts, the email stays open and the draft waits on the Calendar page; it appears when that email is closed or the user goes to the page. It never closes mail the user is writing: with something typed in a composer or a reply, the assistant is told to wait, and a draft already waiting stays out of sight until that mail is sent or closed.
        - A draft is made only while the assistant's bar is open. Starting a new chat or switching mailbox while it is working drafts nothing.
        - The assistant cannot read the calendar, and it cannot edit or delete events. If an event is already open with unsaved changes, it asks the user to finish or close that one first.
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

        Mail in other languages:
        - An email written in another language opens in English. A line above the message says "translated from French" (or whichever language it was), and "show original" beside it switches that one message to what the sender wrote. "show translation" switches it back. The subject at the top follows the message it came from.
        - While the translation is still being written, the original shows with "translating from French…" above it, and the English takes its place when it is ready. If it has not arrived after about ten minutes, the line says "translation not ready yet · showing the original", and opening the thread again checks again.
        - If Passband could not confirm that every detail carried over, the line also says the translation may be incomplete. Check the original for numbers, codes and links before acting on it.
        - Search finds the email by the words of either version. A reply or a forward uses the original.
        - Settings > Mail > Translation lists the languages you read. Mail in those is shown as written and not translated. Adding a language leaves mail already translated as it is, with "show original" still there; removing one means mail in it is translated when it is opened. With no languages listed, everything not in English is translated.

        Attachments and images:
        - Clicking an attachment card or an inline image previews it in Quick Look, where the arrow keys move between that message's attachments. The download arrow on a card saves the file.
        - Tracking pixels are removed from every email. If Settings > Mail > Remote images is set to "On demand", a bar on the email loads its images for that one email.

        Replying and forwarding from here are in the writing_mail topic.
        """

    private static let writingMail = """
        Reply, reply all and forward all start from an open email:
        - Reply: press r. A composer opens inline under the thread, answering the newest message, addressed to its sender.
        - On the Mac the inline composer is part of the conversation: it sits under the newest message, scrolls with the thread, and grows taller as the reply gets longer instead of scrolling inside a fixed box.
        - Reply all: press Enter. The same inline composer, addressed to everyone on the newest message. It only works when no reply is already open and no text field has focus. An open reply is never converted, so to turn one into a reply all, close it with Esc and press Enter.
        - Forward: press f. It forwards the message selected with j / k, in the side composer, with the original shown under the note. Fill in To and send; the note is optional.
        - From the Sitrep or a row in the Emails list, r opens the email and starts the reply in one step. On a list row f is "search this sender", not forward.
        - The Mac reader has no reply buttons. The keys are the way in.
        - On the iPhone there are no keys. Scroll to the end of an open email for the reply and reply all buttons, or swipe one message to the left for Reply all, Reply and Forward on that message. The reply is written in a card under the thread and scrolls with it.

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
        - Groups are made in Settings > Groups (key {key:groups}): n new, e or Enter edit, x delete.
        - On the iPhone, Groups is in Settings (the person icon at the top left of the Sitrep, then Settings at the bottom): + makes one, tap opens it, swipe a group to the left to edit or delete it.
        - Each group is set to go out one way: To (one email, everyone sees the list), Bcc (one email, nobody sees anyone else) or Individually (a separate email per person).

        Sending:
        - ⌘Enter sends. Plain Enter is a new line.
        - Undo send: the mail is held for 5 seconds, with a chip at the bottom left. Clicking it brings the composer back with everything in it. u does the same, except inside an open email, where u means unsubscribe.
        - If the outbound guard blocks a send because the body looks like it carries a secret, the composer reopens with a "send anyway" button (⌘⇧Enter). Any edit re-locks it.

        Send later:
        - The clock beside send, or ⌘⇧L, opens "Send later". Pick a preset or type a time ("friday", "in 3 hours", "the 24th"), then ⌘Enter schedules it.
        - It goes out on time even with Passband closed.
        - Scheduled mail waits on the Emails page (key {key:emails}) under "sent", in a "scheduled" group: s sends it now, x or Backspace cancels it, Enter previews it, and Enter again from the preview pulls it back into the composer to edit.

        Drafts:
        - Everything autosaves about a second after typing stops. Esc closes a composer and keeps the draft.
        - The trash can in the side composer's top corner deletes the message instead of keeping it. It asks first (Enter deletes, Esc goes back), and it cannot be undone.
        - A reply draft comes back when r is pressed on the same message.
        - Other drafts are behind the "drafts" control beside the composer's title: up and down move, Enter opens, Backspace deletes.

        Attachments:
        - "attach" in the composer, dropping files anywhere on it, or pasting an image. 25 MB per file.
        - Pictures go inline where they are dropped. The "inline" / "attached" word on the tray row switches between showing a picture in the body and sending it as a file.

        Signature and tracking:
        - The signature is set in Settings > Mail > Signature. It is added to new messages and replies as the draft starts, so it can be edited per email.
        - "track opens" in the composer adds a tracking pixel to that one email. The default is Settings > Mail > Read tracking. Once a tracked message has been opened, the thread shows "x opens" under its date; clicking it lists every open with its time, and says who the message went to.
        """

    private static let mailList = """
        The Emails page (key {key:emails}) is every message in one list.

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
        - T goes to Rules and A to the audit log, both in Settings. g goes to Auth.

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
        - From Settings > Rules (key {key:rules}, or T on the Emails page): n. Typing in the match field lists senders who have written to the user, by name or address; typing *@ lists whole domains instead. Arrows pick, Enter or Tab takes one, Esc hides the list.
        - In the editor: Enter saves, Option+Enter adds a line, Esc cancels and keeps what was typed. "advanced" (⌘D) shows the match pattern and the disposition picker, and with it open Tab cycles the disposition.
        - This assistant can write one when asked. It only adds rules; it never changes one the user wrote.

        Managing them:
        - In Settings > Rules: j / k move, e or Enter or a click edits, x deletes (with undo).
        - On the iPhone, Rules is in Settings (the person icon at the top left of the Sitrep, then Settings at the bottom): + makes one, tap a rule to edit it, swipe it to the left to delete it (with undo). Swiping a mail row to the left and choosing Tune writes a rule for that sender.
        - Rules are grouped by what they do (allow, filter, mute) and then by domain.
        - A rule shapes mail from now on. It does not move mail already on the board. The exception is "Block sender", offered when unsubscribing from an email that has no unsubscribe link: it mutes that address and clears their open mail.
        - Rules live in Passband only. They never change anything in Gmail.
        """

    private static let reminders = """
        A reminder parks an email and brings it back later.

        Setting one:
        - Press h on the email: on the Sitrep, in the Emails list, or in the reader, where it then moves on to the next email.
        - On the iPhone: swipe a mail row to the right and choose Remind, or tap "remind me" under the subject of an open email.
        - Pick a preset (in an hour, this evening, tomorrow morning, tomorrow evening, this weekend, next week, in a week, next month) or type a time: "friday", "aug 30", "in 3 hours", "the 24th". Each choice shows the exact time it means. Enter sets it.

        What happens:
        - The email is marked done until then, so it leaves the board.
        - At that time it comes back to For your eyes, whatever it was before.
        - The chip at the bottom left undoes it for 5 seconds.

        Finding and changing them:
        - Emails page (key {key:emails}), then the Reminders pill: parked mail in the order it comes back.
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
        - Search covers the mail Passband has indexed. Where the older-mail archive is on, that includes up to a year of mail from before Passband was connected, added gradually in the background and listed with finished mail, and the assistant can say how far back search currently reaches.
        - Results are one list ranked by the search order setting, with the matching words highlighted, on the Mac and the iPhone.

        Finding a sender:
        - Typing a name or an email address lists up to three matching senders above the results, each with how many emails they have sent. Down arrow and Enter, Tab, or a click picks one.
        - Picking a sender shows every email from them, newest first, in one list. The field then reads from: and their address, and more words typed after it search within their mail.
        - It works the same way in the phone's Search tab: tap the sender.

        Operators:
        - from: narrows to a sender. Typing from: opens a menu of the people who write to the user; the arrows pick and Enter or Tab accepts.
        - after: and before: narrow by date.
        - in:sent (or from:me) narrows to mail the user wrote, and to: narrows to mail addressed to someone, by name or address. "in:sent to:dan" is everything the user sent Dan.

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
        - On the iPhone it works the same way in the Search tab, above the results, with the same setting under Assistant on the Account page. The expand arrow on it opens the conversation in the agent chat once it has finished. The Ask the agent row above the results does what the Mac's Ask agent button does: it asks the agent, then opens the conversation in the chat once there is one. With Deeper search off it is turned off, as on the Mac.
        """

    private static let loginCodes = """
        Login codes, password resets, sign-in links, account changes, app access grants, sign-in receipts and security alerts are kept \
        apart from the rest of the mail, on the Auth page (key {key:auth}, or g on the Emails \
        page).

        Why:
        - This mail is sensitive, so it is kept out of search, out of this assistant and out of any connected agent. The assistant cannot read a code, by design.
        - The list shows only who sent it and the subject. A body is fetched only when the user reveals it, and every reveal is written to the audit log.

        Using it:
        - j / k move through the list. Filters: Everything, Last hour, Codes only, Risk.
        - r or Enter reveals. A login code shows its digits right there, and c copies it. Other kinds open a one-time reveal panel that is cleared when it closes.
        - Clicking a row opens the whole message in that reveal panel.
        - The auto-shredder card can move old auth mail to Gmail's trash after a set number of days.

        Needs a decision (the right of the Auth page; the top of the phone's login-codes page):
        - Sign-in alerts, account changes (a new recovery phone, 2FA changed, a new SSH key or access token, a card added to a wallet), app access grants, password resets and sign-in links ask whether it was the user. Resets and sign-in links answer "I asked" or "I did not", app access "I allowed it" or "I didn't", and the rest "That was me" or "Not me".
        - A question stays open for 7 days, or 14 for account changes, app access and security alerts.
        - A critical change (recovery email or phone changed or removed, 2FA turned off or moved to a new device, password or account email changed, a phone number or SIM moved) is marked Critical, sorts first and stays open until it is answered or marked done. It asks even when the same email also holds a login code. Once the user's Passband server is updated, it also sits in For your eyes until it is answered or marked done.
        - Once the user's Passband server is updated, a sign-in alert can be explained, and then asks nothing: by a login code or sign-in link from the same service that the user revealed in Passband just before it, or by Google's own "You shared some Google Account data with" receipt naming that service. Its row then says so. Account changes, app access, security alerts and critical changes are never explained away, and on an older server nothing is.
        - When three or more services see a new sign-in within three days, they share one "New device?" card with "All me" and "Review each". Only sign-ins group this way: account changes and app access always get a card of their own.
        - When more than six cards are open (a "New device?" card counts once), "Clear all" beside the count answers every one "That was me" after a confirmation. Critical changes are left open, to be answered one by one. Each cleared question can still be undone from its row.
        - "Not me" opens the message. Once the user's Passband server is updated, it also puts the message in For your eyes, and an answer given on one device shows on the others; until then, answers stay on the device where they were given.
        - An answer can be taken back: the answered row on the Auth page, and the reveal panel, say what was answered and offer Undo. The reveal panel also asks the question itself, so an alert opened from For your eyes can be answered right there.
        - Login codes, verifications and sign-in receipts ("you shared Google Account data with X") do not ask, unless the same email is also a critical change: they are proof of something the user just did.
        - Auth mail in For your eyes shows its summary and a lock. Opening it reveals it once in the same panel, with the same audit entry.

        Arrival:
        - Auth mail shows up within seconds, without waiting for triage. The Auth icon on the rail shows live codes counting down.
        - On the Mac, a fresh login code floats in a small panel at the top of the screen, over whatever app is in front, with Copy and Dismiss buttons. It does not bring Passband forward or take the keyboard. Copy puts the code on the clipboard and closes the panel. Left alone for 60 seconds, it closes and the code arrives as an ordinary banner instead.
        - Settings > General > Notifications > "Float login codes" turns the panel off, and codes arrive as banners.
        - By default the panel appears even during a Focus or Do Not Disturb. "Respect Do Not Disturb", just below that setting, holds it back while any Focus is on: the code then arrives as a banner, which the Focus handles like any other. Turning it on asks macOS for permission to see the Focus status.
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
        - Passband has an MCP endpoint (Agent MCP) that Claude, OpenClaw, Hermes Agent, Muse, ChatGPT, Claude Code, Codex, Cursor, VS Code or Claude Desktop can connect to. The pane shows the address, a button that adds it where the agent has an install link (Claude, Cursor, VS Code) or copies the setup otherwise, and the setup snippet for each. Muse is set up by sending it a message, which the pane writes.
        - Such an agent can read ranked updates and whole threads, search mail, list bills, deadlines and packages, and set sender rules. It cannot archive, label or delete, and cannot see login codes. Unless the user changed the setting below, it can leave drafts and cannot send.
        - "What your agent may write" sets whether it can write mail, per mailbox. "Read only": it cannot. "Drafts only" (the default): it can leave drafts, marked "from your agent" in the drafts list and in the composer when one is opened, for the user to read, edit and send. An agent can also ask Passband on the Mac to bring a draft it wrote up in the composer: Passband comes to the front with it, unless the user is in the middle of writing something in Passband, in which case nothing moves and a toast says the draft is waiting. A draft opened this way is sent with the send button, not ⌘Enter, so a keystroke meant for another app cannot send it. If the Mac app is not open, the agent gives a passband:// link that opens the draft. When the agent asks for a review, a phone with Passband and notifications on also gets a notification, "Your agent left a draft", naming who it is to and the subject, at most once per draft and not for several drafts in quick succession; tapping it opens the draft in the composer on the phone, ready to read, edit and send with the send button. This never sends anything. "Send, with your approval": it can send from the user's address, and is told to show each message and wait for the user's go-ahead in its own chat first. Passband cannot check that it asked. It blocks anything that looks like a password or key, and every agent send is in the audit log.
        - Hosted Passband does not offer this yet. A self-hosted daemon does.
        """

    private static let appAndAccounts = """
        Accounts:
        - Add one from the Accounts menu ("Add Account…") or Settings > Accounts: sign in with Gmail, or connect a self-hosted server with a pairing code or an API token.
        - Switch with ⌘1 to ⌘9, in the order listed in Settings > Accounts, or from the account badge at the bottom of the rail once there are two or more. Switching starts a fresh assistant chat.
        - Rename an account by editing its name in Settings > Accounts. "remove", then confirm, takes it off this Mac. On the last account the button reads "disconnect".
        - The account badge and the switcher show the photo from the mailbox's Google profile when Passband has been allowed to read it, and the account's initial otherwise. An account connected before this existed shows its initial until it is reconnected.
        - Notifications arrive for every account, not only the one on screen.
        - If the account's server is not answering at launch, Passband keeps retrying on its own. After a couple of seconds the loading screen offers "Try again now", "Switch to" each other account, and "Connect a different server". The Accounts menu and ⌘1 to ⌘9 switch from there too. Nothing is removed: the Connect screen's "Try saved connection again" goes back to the saved account.
        - If the Google sign-in expires, a banner on the Sitrep says Passband cannot read the mail. On a hosted account it has a Reconnect button.

        Notifications:
        - A banner fires for urgent mail, deadlines and other mail important enough to surface, and when a tracked email is opened. Everything else arrives silently.
        - Only urgent and deadline banners play a sound. It is chosen in Settings > General > Notifications, where "Test banner" posts one.
        - While Passband is the front window, banners go to Notification Center only. Clicking a banner opens that message.

        Updates:
        - Passband checks for a new version every hour and downloads it in the background. It installs the next time Passband quits.
        - When one is ready, a card at the bottom of the window offers "Restart" to install it now. "Later" hides the card for an hour.
        - While a new version is waiting, a down-arrow icon sits near the bottom of the sidebar. Clicking it brings the card back.
        - The Passband menu has "Check for Updates…". The installed version is printed at the bottom right of Settings.
        - Settings > General > Help has "What's new" for the release notes and "Practice inbox" to replay the tour.
        - On the iPhone, "What's new" is in Settings (the person icon at the top left of the Sitrep, then Settings at the bottom), in General. It shows the iPhone's own release notes full screen: swipe left for the next page, right to go back, and the x at the top right closes it. The iPhone has no practice inbox.

        Usage and audit:
        - Usage, at the bottom of the rail: mail in and out, signal against noise, triage spend and assistant usage, over 7, 30 or 90 days.
        - Audit log (Settings > Audit, or A on the Emails page): every action taken on the mail by the user or by an agent, with undo for the reversible ones.

        Sharing:
        - Settings > Accounts > Invites sends a friend an invite from the user's own address. Shown only where the server supports it.
        - Settings > Accounts also has the link to the iPhone app on TestFlight.

        Privacy:
        - Settings > Privacy sets telemetry to Full, Minimal or None.
        """

    /// The iPhone's answer, read off the table its own walk reads, so the chat
    /// and the walk cannot describe different releases. A build older than the
    /// first phone note says so instead of reciting a note for a version that
    /// is not installed.
    private static func whatsNewOnPhone(running: String?) -> String {
        let reopen =
            "The iPhone shows this once per version, full screen, a page per feature. To see it "
            + "again, tap the person icon at the top left of the Sitrep, then Settings, then General, then "
            + "What's new."
        guard let running else {
            return "This build does not report its version, so there are no release notes to read. " + reopen
        }
        guard let note = PhoneReleaseNotes.newest(atOrBelow: running) else {
            return "This iPhone is running Passband \(running). No release notes are on record for it."
        }
        var lines = [
            "This iPhone is running Passband \(running).",
            "Release \(note.version), \(note.date): \(note.headline)",
            "",
        ]
        for feature in note.features {
            lines.append("- \(feature.title). \(feature.detail)")
        }
        for aside in note.also {
            lines.append("- " + aside.text)
        }
        lines.append("")
        lines.append(reopen)
        return lines.joined(separator: "\n")
    }

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
    ///
    /// `calendar`: the table as the overlay draws it for this mailbox
    /// (`ShortcutCatalog.groups(calendar:)`), so the calendar's keys are
    /// recited only where the page is on.
    static func shortcuts(calendar: Bool = false) -> String {
        var lines = [
            "Passband keyboard shortcuts, grouped by where they work. A single letter only "
                + "fires when no text field has focus, and a capital letter means Shift. "
                + "Pressing ? shows this same list in the app."
        ]
        for group in ShortcutCatalog.groups(calendar: calendar) {
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
