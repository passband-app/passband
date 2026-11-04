// WHERE THE AGENT CAN SEND SOMEBODY: the pages `show_page` may put on a card in
// the chat, and what each card says. "Where are the rules?" is best answered
// with the place itself, one click away, rather than a path to retrace by hand.
//
// The card's words are the APP'S, never the model's: the model picks a page by
// its wire name and this table supplies the title, glyph and line under it,
// the same honesty rule `show_emails` keeps for an email card. A name the table
// does not have is refused, not drawn.
//
// Nothing here navigates on its own. The card is a button; the user clicks it.

import Foundation

enum PageLink: String, CaseIterable, Sendable {
    case sitrep
    case calendar
    case emails
    case sent
    case spam
    case auth
    case usage
    case discover
    case settingsGeneral = "settings_general"
    case settingsMail = "settings_mail"
    case settingsRules = "settings_rules"
    case settingsGroups = "settings_groups"
    case settingsAssistant = "settings_assistant"
    case settingsAgents = "settings_agents"
    case settingsPrivacy = "settings_privacy"
    case settingsAudit = "settings_audit"
    case settingsAccount = "settings_account"

    /// The Settings pane this link opens, nil for a page off the rail.
    var settingsSection: SettingsSection? {
        switch self {
        case .settingsGeneral: .general
        case .settingsMail: .mail
        case .settingsRules: .rules
        case .settingsGroups: .groups
        case .settingsAssistant: .assistant
        case .settingsAgents: .agents
        case .settingsPrivacy: .privacy
        case .settingsAudit: .audit
        case .settingsAccount: .account
        default: nil
        }
    }

    /// The card's title, in the labels the rail and Settings print.
    var title: String {
        if let section = settingsSection { return "Settings › \(section.label)" }
        switch self {
        case .sitrep: return MainView.sitrep.label
        case .calendar: return MainView.calendar.label
        case .emails: return "Emails › All mail"
        case .sent: return "Emails › Sent"
        case .spam: return "Emails › Spam"
        case .auth: return MainView.auth.label
        case .usage: return MainView.usage.label
        case .discover: return "Discover"
        default: return rawValue
        }
    }

    var symbol: String {
        if let section = settingsSection { return section.symbol }
        switch self {
        case .sitrep: return MainView.sitrep.symbol
        case .calendar: return MainView.calendar.symbol
        case .emails: return MainView.emails.symbol
        case .sent: return "paperplane"
        case .spam: return "xmark.bin"
        case .auth: return MainView.auth.symbol
        case .usage: return MainView.usage.symbol
        case .discover: return "sparkles"
        default: return "arrow.right"
        }
    }

    /// One line under the title: what is there. Also what the model is told
    /// each name means, so the two can never describe different pages.
    var blurb: String {
        switch self {
        case .sitrep: "The home board: what needs you, what to read, today's records."
        case .calendar: "Month, week and agenda, with events found in your mail."
        case .emails: "Every message in one list."
        case .sent: "Mail you sent."
        case .spam: "Gmail's spam folder, to rescue anything real."
        case .auth: "Login codes, password resets and security alerts."
        case .usage: "Mail in and out, signal against noise, triage and assistant spend."
        case .discover: "The ways through your mail, and which ones you have tried."
        case .settingsGeneral: "You, appearance, notifications and help."
        case .settingsMail: "Images, threads, search, translation, signature, read tracking."
        case .settingsRules: "Sender rules: how mail from someone is handled."
        case .settingsGroups: "Named sets of recipients to write to at once."
        case .settingsAssistant: "Which model answers, and whether chats use your own key."
        case .settingsAgents: "Connect an AI agent you already run, and what it may write."
        case .settingsPrivacy: "How much anonymous usage data leaves the app."
        case .settingsAudit: "Every action the agent and this app have taken on your mail."
        case .settingsAccount: "Your mailboxes, the server, invites and the iPhone app."
        }
    }

    /// The pages offered for this mailbox: the Calendar only while it is on,
    /// the rule every other door to it keeps (`AppStore.calendarPageAvailable`).
    static func offered(calendar: Bool) -> [PageLink] {
        allCases.filter { calendar || $0 != .calendar }
    }

    /// The model's key to the names, one line each.
    static func index(calendar: Bool) -> String {
        offered(calendar: calendar).map { "\($0.rawValue): \($0.title). \($0.blurb)" }
            .joined(separator: "\n")
    }
}

extension AppStore {
    /// Go where a `show_page` card points. Every route is one the app already
    /// has a door for, so landing here is landing exactly where the rail, the
    /// menu or a key would have put you.
    @MainActor
    func open(_ page: PageLink) {
        if let section = page.settingsSection {
            openSettings(section)
            return
        }
        switch page {
        case .sitrep: setView(.sitrep)
        case .calendar: setView(.calendar)
        case .emails: openMail(.inbox)
        case .sent: openMail(.sent)
        case .spam: openMail(.spam)
        case .auth: setView(.auth)
        case .usage: setView(.usage)
        case .discover: setView(.discovery)
        default: break
        }
    }
}
