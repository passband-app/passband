// "I dealt with this one here": tells an event's own daemon that the owner
// handled it on this device, so any other device still showing a banner for
// it takes that banner down. Today the one caller is the Mac's floating login
// code panel: a code copied on the Mac is spent, and the same code sitting on
// the phone's lock screen is noise (and a code left on a lock screen).
//
// The daemon turns it into a silent push to every registered phone
// (`POST /client/events/{id}/handled`, squelch-api/src/events.rs).
//
// Per ACCOUNT, not through `APIClient.shared`: a code can arrive in a mailbox
// other than the one on screen, and the shared client speaks only to that one.
//
// Fire and forget. A clear that fails leaves a banner the human can swipe away
// themselves; nothing here is worth an error on screen, and a 404 from a
// daemon that predates the route is the same answer.

import Foundation

enum HandledSignal {
    private nonisolated(unsafe) static let session = Sessions.ephemeral(
        timeout: 10, resource: 15,
        cachePolicy: .reloadIgnoringLocalCacheData, emptyHeaders: true)

    @MainActor
    static func send(accountId: UUID, eventId: Int) {
        Task {
            guard let settings = await AccountManager.shared.settings(for: accountId),
                let request = request(settings, eventId: eventId)
            else { return }
            // Silent on every failure: the URL names the daemon and the
            // header holds a capability.
            _ = try? await session.data(for: request)
        }
    }

    static func request(_ settings: ConnectionSettings, eventId: Int) -> URLRequest? {
        var base = settings.serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        while base.hasSuffix("/") { base.removeLast() }
        guard let url = URL(string: "\(base)/client/events/\(eventId)/handled") else { return nil }
        var request = URLRequest(url: url, timeoutInterval: 10)
        request.httpMethod = "POST"
        request.setValue("Bearer \(settings.apiToken)", forHTTPHeaderField: "Authorization")
        return request
    }
}
