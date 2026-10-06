// PROFILE PHOTOS, one per account: the picture the daemon imported from the
// mailbox's Google profile, kept on this device so the account switcher can
// draw every account's face without asking any daemon for it.
//
// The daemon is the only source. `/client/stats` carries a short tag when it
// holds a photo and omits it when it does not; the bytes are at
// `/client/account/photo`. This file's whole job is to hold the bytes that tag
// names, per account id, and to notice when the tag moves.
//
// ONLY THE ACTIVE ACCOUNT IS EVER FETCHED. The stats that carry the tag are the
// active account's, and `APIClient.shared` is the active account's client, so
// an inactive account's photo is simply the one it had the last time it was
// live. That is why the bytes are on disk rather than in memory: a switcher
// that forgot every other account's face at each launch would be showing one
// picture and a row of initials.
//
// Nothing here is a credential, and nothing here is mail. The files sit beside
// the image cache in Application Support and go when the account does.

import Foundation
import Observation
import SwiftUI

@MainActor
@Observable
final class AccountPhotos {
    static let shared = AccountPhotos()

    /// The decoded pictures, by account id. Observed: a view that reads one
    /// redraws when it arrives or changes.
    private(set) var images: [UUID: PlatformImage] = [:]

    /// The per-account defaults key holding the tag of the photo on disk. In
    /// `AccountIndex.scopedDefaultsKeys`, so removing an account forgets it.
    static let tagKey = "passband.account-photo-tag"

    @ObservationIgnored private let dir: URL
    /// Accounts with a download in flight, so a stats tick every ten seconds
    /// cannot start a second one behind the first.
    @ObservationIgnored private var fetching: Set<UUID> = []

    private init() {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        dir = base.appendingPathComponent("Passband/AccountPhotos", isDirectory: true)
        // A few files of tens of kilobytes each, read once: cheap enough to do
        // here, and it means the first frame of the rail already has a face.
        for record in AccountIndex.load().accounts {
            guard tag(for: record.id) != nil,
                let data = try? Data(contentsOf: file(for: record.id)),
                let image = PlatformImage(data: data)
            else { continue }
            images[record.id] = image
        }
    }

    func image(for id: UUID?) -> PlatformImage? {
        guard let id else { return nil }
        return images[id]
    }

    /// `sync` for whichever account is live, which is the one a stats answer
    /// describes. NOT IN REHEARSAL: the practice mailbox has no owner, and its
    /// stats saying "no photo" must not take the real account's picture away.
    func noteActive(tag: String?) {
        guard !RehearsalMode.isEnabled, let active = AccountManager.shared.activeId else { return }
        sync(tag: tag, for: active)
    }

    /// Hold what the daemon says this account's photo is. Called with every
    /// stats answer for the ACTIVE account; a no-op when the tag has not moved,
    /// which is every call but the first.
    ///
    /// A nil tag from a live daemon means there is no photo: none was ever
    /// imported, or the person took theirs down. Either way the copy here goes.
    func sync(tag: String?, for id: UUID) {
        guard tag != self.tag(for: id) || (tag != nil && images[id] == nil) else { return }
        guard let tag else {
            forget(id)
            return
        }
        guard !fetching.contains(id) else { return }
        fetching.insert(id)
        Task {
            defer { fetching.remove(id) }
            // THE CLIENT IS THE ACTIVE ACCOUNT'S, so the answer only belongs to
            // `id` if `id` is live on both sides of the request. A switch in
            // between means these bytes may be somebody else's face.
            guard AccountManager.shared.activeId == id,
                let data = try? await APIClient.shared.accountPhoto(),
                AccountManager.shared.activeId == id,
                let image = PlatformImage(data: data)
            else { return }
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            // Bytes first, tag second: a crash between the two leaves a stale
            // tag, which reads as "fetch again", never as a wrong picture.
            guard (try? data.write(to: file(for: id), options: .atomic)) != nil else { return }
            UserDefaults.standard.set(tag, forKey: AccountIndex.scopedKey(Self.tagKey, id))
            images[id] = image
        }
    }

    /// Drop one account's photo: it was removed at the source, or the account
    /// was. The defaults key is cleared here too so this is safe to call from
    /// either path in either order.
    func forget(_ id: UUID) {
        try? FileManager.default.removeItem(at: file(for: id))
        UserDefaults.standard.removeObject(forKey: AccountIndex.scopedKey(Self.tagKey, id))
        if images[id] != nil { images[id] = nil }
    }

    private func tag(for id: UUID) -> String? {
        UserDefaults.standard.string(forKey: AccountIndex.scopedKey(Self.tagKey, id))
    }

    private func file(for id: UUID) -> URL {
        dir.appendingPathComponent(id.uuidString)
    }
}

/// An account's mark: its profile photo when one was imported, otherwise the
/// initial in a tinted circle it has always worn. ONE view for the Mac rail and
/// the phone's account list, so a person recognizes an account by the same
/// mark on both.
struct AccountMark: View {
    let account: AccountRecord?
    var size: CGFloat = 26
    /// The initial's type. The Mac rail and the phone list set their own.
    var font: Font = .system(size: 13, weight: .semibold)

    var body: some View {
        Group {
            if let image = AccountPhotos.shared.image(for: account?.id) {
                Image(platformImage: image)
                    .resizable()
                    .interpolation(.high)
                    .scaledToFill()
                    .frame(width: size, height: size)
                    .clipShape(Circle())
                    // A full ring, not a highlight: it is what keeps a pale
                    // photo from dissolving into the rail's material.
                    .overlay(Circle().strokeBorder(Palette.hairline, lineWidth: 0.75))
            } else {
                Text(account?.initial ?? "?")
                    .font(font)
                    .foregroundStyle(Palette.accent)
                    .frame(width: size, height: size)
                    .background(Circle().fill(Palette.accentSoft))
                    .overlay(Circle().strokeBorder(Palette.accent.opacity(0.35), lineWidth: 0.75))
            }
        }
        .accessibilityHidden(true)
    }
}
