// The app's half of event reminders and the Tickets button (docs/CALENDAR.md,
// "Reminders and tickets"): what a tap on a reminder opens, what the card's
// Tickets button opens, and the owner's switch. The banner's words and the
// frame are Model/CalendarReminder.swift's, which the phone's notification
// extension compiles too; nothing here is reachable from there.
//
// ONE OPENER FOR BOTH DOORS. A reminder's "Show tickets" and the card's
// Tickets button show the same thing the same way: an attachment in Quick
// Look (a `.pkpass` offers Wallet on the phone), else the mail in the reader.
// The Quick Look half needs a view in the window (the Mac's panel asks the
// responder chain who drives it), so the store only names the file
// (`ticketPreview`) and `TicketPreviewHost`, mounted once at the root, opens it.

import Foundation

/// The switch as the settings card shows it.
enum CalendarReminderPrefState: Sendable, Equatable {
    /// Not asked yet, or asked and still waiting.
    case unknown
    /// The daemon predates reminders (404): nothing to switch.
    case notOffered
    /// The read failed for any other reason.
    case unreadable
    case loaded(Bool)
}

/// The last answer each account's daemon gave, kept on this device so a frame
/// that arrives in the second after the switch went off is not shown. The
/// daemon is the authority (it stops firing); this is only the echo.
enum CalendarReminderPref {
    private static let base = "passband.calendar.remindersOff"

    static func knownOff(_ account: UUID) -> Bool {
        UserDefaults.standard.bool(forKey: AccountIndex.scopedKey(base, account))
    }

    static func remember(enabled: Bool, for account: UUID) {
        let key = AccountIndex.scopedKey(base, account)
        if enabled {
            UserDefaults.standard.removeObject(forKey: key)
        } else {
            UserDefaults.standard.set(true, forKey: key)
        }
    }
}

/// One ticket to show (`AppStore.ticketPreview`).
struct TicketPreviewRequest: Identifiable, Equatable {
    let id = UUID()
    let attachment: Attachment
    let messageId: Int
    let threadId: String

    /// A wallet pass, which the phone offers to add to Wallet.
    var isWalletPass: Bool {
        attachment.mime.lowercased().hasPrefix("application/vnd.apple.pkpass")
            || attachment.filename.lowercased().hasSuffix(".pkpass")
    }
}

extension AppStore {
    // MARK: - opening

    /// The card's Tickets button.
    func openTickets(_ tickets: CalendarTickets) {
        let file = EventBanner.CalendarFile(
            messageId: tickets.message_id, threadId: tickets.thread_id,
            attachmentId: tickets.attachment?.id, filename: tickets.attachment?.filename,
            mime: tickets.attachment?.mime)
        if file.attachmentId != nil {
            showTicketFile(file, from: .calendar)
        } else {
            openTicketMail(file, from: .calendar)
        }
    }

    /// A reminder's tap, once its mailbox is the one on screen (`Notifier`).
    func openCalendarReminder(_ target: EventBanner.CalendarReminderTap.Target) {
        switch target {
        case .attachment(let file): showTicketFile(file, from: .notification)
        case .mail(let file): openTicketMail(file, from: .notification)
        case .event(let id, let start): openCalendarItem(id, start: start)
        case .none:
            if calendarPageAvailable, activeView != .calendar { setView(.calendar) }
        }
    }

    /// The event's card on the Calendar page: the page, on a grid, turned to
    /// the event's day with its detail open, as a search hit opens
    /// (`openCalendarSearchHit`). Turning the page is what fetches the
    /// month, so the row is found once the feed lands.
    func openCalendarItem(_ id: String, start: String?, now: Date = Date()) {
        guard calendarPageAvailable else { return }
        if activeView != .calendar { setView(.calendar) }
        guard activeView == .calendar else { return }
        if !calendarMode.isGrid { setCalendarMode(.month) }
        let cal = Calendar.current
        let day = start.flatMap { AgendaTime.parse($0, zone: nil, calendar: cal)?.date } ?? now
        selectCalendarItem(id, on: cal.startOfDay(for: day), now: now)
        #if os(iOS)
            calendarSearchOpen = id
        #endif
    }

    private func openTicketMail(_ file: EventBanner.CalendarFile, from source: OpenSource) {
        if file.threadId.isEmpty {
            openMessage(file.messageId, from: source)
        } else {
            openThread(file.threadId, focusMessage: file.messageId, from: source)
        }
    }

    /// Quick Look on one attachment. A reminder's `open` names it by id
    /// alone, so its name and type are read off the thread, which is also
    /// the check that it is still there and still this message's. Anything
    /// Quick Look must not be handed (`AttachmentKinds.isPreviewable`) opens
    /// the mail instead.
    private func showTicketFile(_ file: EventBanner.CalendarFile, from source: OpenSource) {
        guard let attachmentId = file.attachmentId else { return openTicketMail(file, from: source) }
        let opened = epoch
        Task {
            var attachment: Attachment?
            if !file.threadId.isEmpty,
                let view = try? await ThreadPrefetch.shared.fetch(file.threadId, fresh: ThreadPrefetch.warmFresh)
            {
                attachment =
                    view.messages.first(where: { $0.id == file.messageId })?.attachments?
                    .first(where: { $0.id == attachmentId })
            }
            guard isCurrent(opened) else { return }
            if attachment == nil, let name = file.filename {
                attachment = Attachment(
                    id: attachmentId, filename: name, mime: file.mime ?? "application/octet-stream",
                    size: 0, downloadable: true)
            }
            guard let attachment, AttachmentKinds.isPreviewable(attachment) else {
                return openTicketMail(file, from: source)
            }
            ticketPreview = TicketPreviewRequest(
                attachment: attachment, messageId: file.messageId, threadId: file.threadId)
        }
    }

    // MARK: - the switch

    func refreshCalendarReminderPref() async {
        let e = epoch
        let account = AccountManager.shared.activeId
        do {
            let next = try await APIClient.shared.getCalendarReminderSettings()
            guard e == epoch else { return }
            calendarReminderPref = .loaded(next.enabled)
            if let account { CalendarReminderPref.remember(enabled: next.enabled, for: account) }
        } catch {
            guard e == epoch else { return }
            if (error as? APIError)?.kind == .notFound {
                calendarReminderPref = .notOffered
            } else if case .loaded = calendarReminderPref {
                // The stored switch did not move because the network blinked.
            } else {
                calendarReminderPref = .unreadable
            }
        }
    }

    /// Throws so the card can say why; the switch only moves to what the
    /// daemon confirmed.
    func setCalendarReminders(_ enabled: Bool) async throws {
        let e = epoch
        let account = AccountManager.shared.activeId
        try await APIClient.shared.setCalendarReminderSettings(enabled: enabled)
        guard e == epoch else { return }
        calendarReminderPref = .loaded(enabled)
        if let account { CalendarReminderPref.remember(enabled: enabled, for: account) }
    }
}
