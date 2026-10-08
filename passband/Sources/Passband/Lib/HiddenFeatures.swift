import Foundation

/// A feature this build has and does not show everybody yet.
///
/// The key is the whole identity, and it is the SAME string the daemon and the
/// control plane use (`squelch-core/src/features.rs`, whose `KNOWN` list is what
/// the operator's `/admin/features` page offers). A flag only the app reads
/// still needs an entry there, or there is no box for anybody to tick.
///
/// Adding one is a `static let` here, an entry in that list, and an
/// `store.hasFeature(.theName)` wherever the app would otherwise show it.
/// Launching it is deleting all three.
struct HiddenFeature: Hashable, Sendable {
    let key: String
}

/// The hidden features that are on for this mailbox, as the daemon reported
/// them on `/client/stats`.
///
/// OFF IS THE ANSWER TO EVERY DOUBT, the same rule the daemon keeps. No stats
/// yet, a daemon too old to send the list, and an empty list all read as
/// "nothing extra", so a hidden feature can fail to appear and can never appear
/// by accident. That also means every caller needs no loading state: before the
/// first stats answer lands, the app looks exactly as it does for somebody who
/// was never let in.
///
/// THERE IS NO LOCAL OVERRIDE HERE, on purpose. To see a hidden feature in a
/// development build, pin it on the daemon you are pointed at
/// (`SQUELCH_FEATURES=calendar squelchd serve`); the app then learns it the way
/// a real tenant's app does, and the gate you are testing is the real one.
struct HiddenFeatures: Equatable, Sendable {
    private let keys: Set<String>

    /// Nothing on.
    static let none = HiddenFeatures(daemon: nil)

    /// `daemon` is `StoreStats.features` exactly as decoded: nil when the
    /// daemon did not say.
    init(daemon: [String]?) {
        keys = Set(daemon ?? [])
    }

    func has(_ feature: HiddenFeature) -> Bool {
        keys.contains(feature.key)
    }
}
