// Local UI preferences — UserDefaults-backed, app-wide, reactive. Per-device
// view state, not account state. Views read through @Observable, so a change in
// Settings re-renders every open consumer (e.g. the email frames) immediately.

import Foundation
import SwiftUI

// `SettingsSection` — the sub-nav's own enum, which the stored section below is
// typed as — lives in Lib/SettingsSearch.swift, beside the card index that
// files every setting under one. The taxonomy is one thing; this file only
// remembers which of it you were last looking at.

/// HOW SEARCH ORDERS ITS RESULTS, and the reader's standing answer to it.
/// Sent on every search as `sort=`; the daemon ranks accordingly.
///
/// A preference rather than a control on the search bar itself. Somebody who
/// wants one of these wants it every time, and a per-search picker is a
/// decision re-asked on every keystroke. `recent` is the default because mail
/// is not a document corpus: the thread you are looking for is usually the one
/// that moved.
enum SearchSortChoice: String, CaseIterable, Sendable {
    case recent, bestMatch = "best_match"

    var label: String {
        switch self {
        case .recent: "Relevance + recency"
        case .bestMatch: "Best match"
        }
    }

    /// What the setting actually promises, in one line, under the picker.
    var blurb: String {
        switch self {
        case .recent: "Newer mail ranks higher when matches are close."
        case .bestMatch: "Rank on the words alone, however old the mail is."
        }
    }
}

/// How much developer telemetry (PostHog) leaves the app. Opt-out: `full` is
/// the default. `minimal` keeps sessions, screen views, and the anonymous
/// counter events (sends, triage volume, corrections, connection health — see
/// `Analytics.minimalEvents`) but drops the remaining action verbs; `full`
/// also records Mac sessions as layout only (`SessionReplay`);
/// `none` sends nothing at all. No level carries content at any time: every
/// event string that leaves is in `Analytics.allowedStrings`, and a replay
/// frame holds only those, fixed greys and numbers (`ReplayWireframe.validate`).
///
/// The key is public because Analytics reads the raw value straight from
/// UserDefaults — capture can fire off the main actor, where Prefs lives.
enum TelemetryLevel: String, CaseIterable, Sendable {
    case full, minimal, none

    static let prefKey = "passband.pref.telemetry"

    var label: String {
        switch self {
        case .full: "Full"
        case .minimal: "Minimal"
        case .none: "None"
        }
    }
}

/// The banner chime. `system` is the macOS default alert; the rest are bundled
/// CAFs named for what they are on the air. Referenced by file name because
/// UNNotificationSound only resolves names, never paths — Notifier installs the
/// files where the system looks (see `Notifier.installSounds`).
enum NotificationSound: String, CaseIterable, Sendable {
    case system, squelch, `static`, morse, carrier

    var label: String {
        switch self {
        case .system: "Default"
        case .squelch: "Squelch"
        case .static: "Static"
        case .morse: "Morse"
        case .carrier: "Carrier"
        }
    }

    /// Bundle resource name under Resources/Sounds, nil for the system sound.
    var resourceName: String? {
        self == .system ? nil : label
    }

    /// The file name in ~/Library/Sounds — prefixed, because that folder is
    /// shared by every app on the machine and shows up in system sound pickers.
    var installedFileName: String? {
        resourceName.map { "Passband \($0).caf" }
    }
}

// `DeeperSearchChoice` is NOT here: it lives beside the rule that reads it
// (Lib/DeeperSearch.swift), which is pure so test.sh can assert what `off`
// does to a lane that is already running. This file is only its storage.

/// Two palettes selected explicitly, or `system` to follow the OS. Dark is the
/// default (see the registered defaults in `Prefs.init`).
enum ThemeChoice: String, CaseIterable, Sendable {
    case system, light, dark

    var label: String {
        switch self {
        case .system: "Auto"
        case .light: "Light"
        case .dark: "Dark"
        }
    }

    var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}

@MainActor
@Observable
final class Prefs {
    static let shared = Prefs()

    private let defaults = UserDefaults.standard

    private enum Key {
        static let loadRemoteImages = "passband.pref.loadRemoteImages"
        static let settingsSection = "passband.pref.settingsSection"
        static let developerMode = "passband.pref.developerMode"
        /// Still the tour's old key, on purpose: an install that finished the
        /// tour has already had its first look and is owed no first-load veil.
        static let firstLoadSeen = "passband.pref.tourCompleted"
        static let lastSeenReleaseNotes = "passband.pref.lastSeenReleaseNotes"
        static let lastSeenPhoneNotes = "passband.pref.lastSeenPhoneNotes"
        static let readerHint = "passband.pref.readerHint"
        static let theme = "passband.pref.theme"
        static let zoom = "passband.pref.zoom"
        static let density = "passband.pref.density"
        static let threadStyle = "passband.pref.threadStyle"
        static let searchIncludeRelated = "passband.pref.searchIncludeRelated"
        static let searchSort = "passband.pref.searchSort"
        static let notificationSound = "passband.pref.notificationSound"
        static let floatLoginCodes = "passband.pref.floatLoginCodes"
        static let loginCodesRespectFocus = "passband.pref.loginCodesRespectFocus"
        static let userName = "passband.name"
        static let signature = "passband.pref.signature"
        static let assistantModel = "passband.assistant.model"
        static let assistantTransport = "passband.assistant.transport"
        static let searchLaneModel = "passband.search.lane.model"
        static let deeperSearch = "passband.search.deeper"
        static let mailHiddenZones = "passband.pref.mailHiddenZones"
        static let telemetry = TelemetryLevel.prefKey
    }

    private init() {
        defaults.register(defaults: [
            Key.loadRemoteImages: true,
            Key.settingsSection: SettingsSection.general.rawValue,
            Key.developerMode: false,
            Key.firstLoadSeen: false,
            // Dark by default: the app (and its onboarding scene) is designed
            // night-first. A stored choice, including an explicit Auto, wins.
            Key.theme: ThemeChoice.dark.rawValue,
            Key.zoom: 1.0,
            Key.density: Self.defaultDensity.rawValue,
            Key.threadStyle: ThreadStyleDefault.auto.rawValue,
            Key.searchIncludeRelated: false,
            Key.searchSort: SearchSortChoice.recent.rawValue,
            Key.notificationSound: NotificationSound.system.rawValue,
            Key.floatLoginCodes: true,
            Key.loginCodesRespectFocus: false,
            Key.telemetry: TelemetryLevel.full.rawValue,
            Key.deeperSearch: DeeperSearchChoice.automatic.rawValue,
        ])
        _loadRemoteImages = defaults.bool(forKey: Key.loadRemoteImages)
        _settingsSection =
            SettingsSection(rawValue: defaults.string(forKey: Key.settingsSection) ?? "")
            ?? .general
        _developerMode = defaults.bool(forKey: Key.developerMode)
        _firstLoadSeen = defaults.bool(forKey: Key.firstLoadSeen)
        _lastSeenReleaseNotes = defaults.string(forKey: Key.lastSeenReleaseNotes)
        _lastSeenPhoneNotes = defaults.string(forKey: Key.lastSeenPhoneNotes)
        _theme = ThemeChoice(rawValue: defaults.string(forKey: Key.theme) ?? "") ?? .dark
        _zoom = Zoom.clamp(defaults.double(forKey: Key.zoom))
        _density =
            DensityChoice(rawValue: defaults.string(forKey: Key.density) ?? "")
            ?? Self.defaultDensity
        _threadStyle =
            ThreadStyleDefault(rawValue: defaults.string(forKey: Key.threadStyle) ?? "") ?? .auto
        _searchIncludeRelated = defaults.bool(forKey: Key.searchIncludeRelated)
        _searchSort =
            SearchSortChoice(rawValue: defaults.string(forKey: Key.searchSort) ?? "") ?? .recent
        _notificationSound =
            NotificationSound(rawValue: defaults.string(forKey: Key.notificationSound) ?? "")
            ?? .system
        _floatLoginCodes = defaults.bool(forKey: Key.floatLoginCodes)
        _loginCodesRespectFocus = defaults.bool(forKey: Key.loginCodesRespectFocus)
        _telemetry =
            TelemetryLevel(rawValue: defaults.string(forKey: Key.telemetry) ?? "") ?? .full
        _userName = defaults.string(forKey: Key.userName) ?? ""
        // ABSENT, NOT EMPTY, is what earns the default. The Settings editor
        // stores on every keystroke, so a signature somebody cleared is a
        // stored "" and stays none; only an install that never touched the
        // field signs with the house line.
        _signature = defaults.string(forKey: Key.signature) ?? Prefs.defaultSignature
        _assistantModel =
            AssistantModel.migrating(rawValue: defaults.string(forKey: Key.assistantModel) ?? "")
            ?? .haiku
        _assistantTransport =
            AssistantTransport(rawValue: defaults.string(forKey: Key.assistantTransport) ?? "")
            ?? .relay
        _searchLaneModel =
            AssistantModel.migrating(rawValue: defaults.string(forKey: Key.searchLaneModel) ?? "")
            ?? .haiku
        _deeperSearch =
            DeeperSearchChoice(rawValue: defaults.string(forKey: Key.deeperSearch) ?? "")
            ?? .automatic
        _mailHiddenZones = Set(defaults.stringArray(forKey: Key.mailHiddenZones) ?? [])

    }

    /// Load remote (network) images in email HTML automatically. When false,
    /// each message shows a per-email "load images" opt-in instead.
    private var _loadRemoteImages: Bool
    var loadRemoteImages: Bool {
        get { _loadRemoteImages }
        set {
            _loadRemoteImages = newValue
            defaults.set(newValue, forKey: Key.loadRemoteImages)
        }
    }

    private var _settingsSection: SettingsSection
    var settingsSection: SettingsSection {
        get { _settingsSection }
        set {
            _settingsSection = newValue
            defaults.set(newValue.rawValue, forKey: Key.settingsSection)
        }
    }

    /// How search orders its results. Read at FETCH time rather than captured
    /// when a panel opens, so changing it in Settings is in force on the very
    /// next search without anything having to observe anything.
    private var _searchIncludeRelated: Bool
    var searchIncludeRelated: Bool {
        get { _searchIncludeRelated }
        set {
            _searchIncludeRelated = newValue
            defaults.set(newValue, forKey: Key.searchIncludeRelated)
        }
    }

    private var _searchSort: SearchSortChoice
    var searchSort: SearchSortChoice {
        get { _searchSort }
        set {
            _searchSort = newValue
            defaults.set(newValue.rawValue, forKey: Key.searchSort)
        }
    }

    /// Developer mode: exposes re-triage affordances (masthead + thread viewer).
    private var _developerMode: Bool
    var developerMode: Bool {
        get { _developerMode }
        set {
            _developerMode = newValue
            defaults.set(newValue, forKey: Key.developerMode)
        }
    }

    /// Whether this Mac has had its first look at a mailbox: the first-load
    /// veil was seen through, dismissed early, or never needed (the mailbox
    /// had nothing to catch up on). Either way it is never shown again here.
    private var _firstLoadSeen: Bool
    var firstLoadSeen: Bool {
        get { _firstLoadSeen }
        set {
            _firstLoadSeen = newValue
            defaults.set(newValue, forKey: Key.firstLoadSeen)
        }
    }

    /// The newest release whose notes this install has been shown, or nil for
    /// an install that has never seen the card. NOT registered with a default:
    /// absence is a real state (see WhatsNew), and a registered "" would be
    /// indistinguishable from a stamp somebody wrote.
    private var _lastSeenReleaseNotes: String?
    var lastSeenReleaseNotes: String? {
        get { _lastSeenReleaseNotes }
        set {
            _lastSeenReleaseNotes = newValue
            defaults.set(newValue, forKey: Key.lastSeenReleaseNotes)
        }
    }

    /// The iPhone's stamp: the newest PHONE release whose notes this install
    /// has been shown (or been stamped past, as a newcomer). Its own key and
    /// not `lastSeenReleaseNotes`, because the two are versions on different
    /// lines read against different tables. The phone's numbers trail the
    /// Mac's, so one field holding either, depending on which app wrote it, is
    /// a Mac stamp that silences several phone releases the day a backup, a
    /// shared suite or a careless read brings it across. Unregistered, for the
    /// reason the Mac's is: absence is a real state.
    /// Where the reader's one-time hint stands (ReaderHintRule.Stored), or nil
    /// before the reader has ever opened on this Mac. Not registered: absence
    /// is the state that earns the centred beat.
    var readerHint: ReaderHintRule.Stored? {
        get { defaults.string(forKey: Key.readerHint).flatMap(ReaderHintRule.Stored.init) }
        set { defaults.set(newValue?.rawValue, forKey: Key.readerHint) }
    }

    private var _lastSeenPhoneNotes: String?
    var lastSeenPhoneNotes: String? {
        get { _lastSeenPhoneNotes }
        set {
            _lastSeenPhoneNotes = newValue
            defaults.set(newValue, forKey: Key.lastSeenPhoneNotes)
        }
    }

    /// Developer telemetry level. Analytics gates on the UserDefaults value
    /// this writes, so a change here takes effect on the very next event.
    private var _telemetry: TelemetryLevel
    var telemetry: TelemetryLevel {
        get { _telemetry }
        set {
            _telemetry = newValue
            defaults.set(newValue.rawValue, forKey: Key.telemetry)
        }
    }

    private var _theme: ThemeChoice
    var theme: ThemeChoice {
        get { _theme }
        set {
            _theme = newValue
            defaults.set(newValue.rawValue, forKey: Key.theme)
        }
    }

    /// The whole window's scale, ⌘+ / ⌘- / ⌘0 (see App/Zoom.swift). Always a
    /// step on `Zoom.steps`.
    private var _zoom: Double
    var zoom: Double {
        get { _zoom }
        set {
            _zoom = Zoom.clamp(newValue)
            defaults.set(_zoom, forKey: Key.zoom)
        }
    }

    /// Compact is the default: it is the layout the app was drawn in, and
    /// comfortable is the answer for whoever finds that too dense rather than
    /// a change made to everybody. The phone offers no switch at all (see
    /// AppearanceSection) until somebody has looked at the comfortable curve
    /// on a real one.
    private static let defaultDensity = DensityChoice.compact

    /// How much room the interface gives each thing (see Lib/Density.swift).
    /// `Space` and `Typo` read this on every access, so flipping it re-lays
    /// out every view that takes its metrics from them.
    private var _density: DensityChoice
    var density: DensityChoice {
        get { _density }
        set {
            guard newValue != _density else { return }
            // BEFORE the value moves, so before any body can run against it:
            // every measured email height on file was taken at the old padding
            // and type, and a frame's body reads that file directly. Wiped
            // from a view's `onChange` instead, the first layout after the
            // flip had already painted at the stale heights.
            FrameHeights.shared.wipeAll()
            _density = newValue
            defaults.set(newValue.rawValue, forKey: Key.density)
        }
    }

    /// How threads are drawn where nothing else has been said — including
    /// `auto`, which is an instruction to read the thread rather than a style
    /// (see ThreadStyle.automatic). A thread the reader has switched by hand
    /// keeps its own answer (ThreadStyleLedger) and ignores this.
    private var _threadStyle: ThreadStyleDefault
    var threadStyle: ThreadStyleDefault {
        get { _threadStyle }
        set {
            _threadStyle = newValue
            defaults.set(newValue.rawValue, forKey: Key.threadStyle)
        }
    }

    private var _notificationSound: NotificationSound
    var notificationSound: NotificationSound {
        get { _notificationSound }
        set {
            _notificationSound = newValue
            defaults.set(newValue.rawValue, forKey: Key.notificationSound)
        }
    }

    /// Whether an arriving login code floats over whatever app is in front
    /// (see App/LoginCodePanel.swift). Off, the code is an ordinary banner.
    private var _floatLoginCodes: Bool
    var floatLoginCodes: Bool {
        get { _floatLoginCodes }
        set {
            _floatLoginCodes = newValue
            defaults.set(newValue, forKey: Key.floatLoginCodes)
        }
    }

    /// Whether a Focus (Do Not Disturb included) holds the floating panel
    /// back, leaving the code to the banner the system then filters itself.
    /// Off by default: reading the Focus takes a permission, and nobody is
    /// asked for one until they turn this on.
    private var _loginCodesRespectFocus: Bool
    var loginCodesRespectFocus: Bool {
        get { _loginCodesRespectFocus }
        set {
            _loginCodesRespectFocus = newValue
            defaults.set(newValue, forKey: Key.loginCodesRespectFocus)
        }
    }

    /// The human's display name, for the Sitrep greeting only. Client-side:
    /// the human door has no such field. Set in Settings > You; seeded once
    /// below.
    private var _userName: String
    var userName: String {
        get { _userName }
        set {
            _userName = newValue.trimmingCharacters(in: .whitespaces)
            defaults.set(_userName, forKey: Key.userName)
        }
    }

    /// First-run display name: the given name on the mailbox's Google profile,
    /// the first time the daemon has one. Greets "Ada", never "alovelace97":
    /// there is no address fallback, because nothing on the greeting can
    /// correct a bad guess in place any more. No given name, no name.
    ///
    /// Fires AT MOST ONCE per install. The guard is the stored key's absence,
    /// not emptiness, so a name cleared in Settings stays cleared and an
    /// account switch never re-guesses over a name you already have. The one
    /// other state it writes over is OUR OWN earlier guess, the local part of
    /// this same mailbox (what installs seeded before the given name existed),
    /// so those stop greeting by their address too. A name cleared in
    /// Settings is the empty string, not the local part, and stays cleared.
    func seedUserName(fromGivenName given: String?, email: String?) {
        guard let name = given?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty
        else { return }
        let stored = defaults.object(forKey: Key.userName) as? String
        guard stored == nil || (stored == Self.localPart(of: email) && stored != name) else { return }
        _userName = name
        defaults.set(name, forKey: Key.userName)
    }

    /// Text before the first "@", or nil when the string is not address-shaped
    /// or has nothing there. Not `split`: it omits empty subsequences, so
    /// "@gmail.com" would hand back the domain.
    private static func localPart(of email: String?) -> String? {
        guard let email, let at = email.firstIndex(of: "@") else { return nil }
        let local = String(email[..<at])
        return local.isEmpty ? nil : local
    }

    /// What every install signs with until its human says otherwise. Markdown,
    /// like any other signature, so the dashes need the blank line
    /// `signatureSeed` puts above them: "--" directly under a line of text is
    /// a CommonMark heading underline, not a separator.
    static let defaultSignature = "--\n\nSent from [Passband](https://passband.app)"

    /// The email signature, as markdown — same dialect as the compose body.
    /// Stored raw (no trimming in the setter: it is bound to a live editor, and
    /// normalizing under the caret would fight every trailing newline typed).
    private var _signature: String
    var signature: String {
        get { _signature }
        set {
            _signature = newValue
            defaults.set(newValue, forKey: Key.signature)
        }
    }

    /// What a fresh compose body starts as: empty when no signature is stored,
    /// otherwise the signature two newlines down, so typing starts above it.
    var signatureSeed: String {
        let sig = signature.trimmed
        return sig.isEmpty ? "" : "\n\n" + sig
    }

    /// True while a compose body holds nothing the user typed: blank, or only
    /// the signature. The draft machinery treats such a body as empty — it
    /// must neither block a restore nor be worth saving as a draft.
    ///
    /// COMPARED TRIMMED, not against the seed byte for byte. A picture added
    /// to a fresh composer and taken out again closes up one of the seed's two
    /// leading newlines, and that body is as untyped as the one it started as:
    /// an exact match called it written, so it sent, and Esc kept it as a
    /// draft of nothing but the signature.
    func isBodyUntouched(_ body: String) -> Bool {
        let typed = body.trimmed
        return typed.isEmpty || typed == signature.trimmed
    }

    private var _assistantModel: AssistantModel
    var assistantModel: AssistantModel {
        get { _assistantModel }
        set {
            _assistantModel = newValue
            defaults.set(newValue.rawValue, forKey: Key.assistantModel)
        }
    }

    /// WHAT THE USER PICKED, not what runs. The effective transport is relay
    /// only when this says relay AND the daemon advertises `assistant_relay`
    /// (see `AssistantSession.run`); on a self-host daemon that never does, the
    /// relay default is inert and every ask is BYOK regardless.
    private var _assistantTransport: AssistantTransport
    var assistantTransport: AssistantTransport {
        get { _assistantTransport }
        set {
            _assistantTransport = newValue
            defaults.set(newValue.rawValue, forKey: Key.assistantTransport)
        }
    }

    /// The model the SEARCH LANE runs on, independent of the ⌘K chat's. Its own
    /// preference because the two spend differently: the lane runs on every
    /// deeper query, unasked, while the chat runs when somebody types a
    /// question — so Haiku is the honest default here even for a reader who
    /// picked Opus for their chat.
    private var _searchLaneModel: AssistantModel
    var searchLaneModel: AssistantModel {
        get { _searchLaneModel }
        set {
            _searchLaneModel = newValue
            defaults.set(newValue.rawValue, forKey: Key.searchLaneModel)
        }
    }

    /// Whether a question-shaped search may start the agent lane by itself.
    private var _deeperSearch: DeeperSearchChoice
    var deeperSearch: DeeperSearchChoice {
        get { _deeperSearch }
        set {
            _deeperSearch = newValue
            defaults.set(newValue.rawValue, forKey: Key.deeperSearch)
        }
    }

    /// The sitrep zones the all-mail page's category picker has turned off,
    /// as the wire's `placement` strings ("shipments", "noise", …). Remembered:
    /// somebody who never wants shipping mail in their list wants that every
    /// time they open it.
    private var _mailHiddenZones: Set<String>
    var mailHiddenZones: Set<String> {
        get { _mailHiddenZones }
        set {
            _mailHiddenZones = newValue
            defaults.set(newValue.sorted(), forKey: Key.mailHiddenZones)
        }
    }

    /// Flip light <-> dark. `system` resolves against the current appearance
    /// first so the toggle always visibly changes something.
    func flipTheme() {
        switch theme {
        case .light: theme = .dark
        case .dark: theme = .light
        case .system:
            theme = Platform.isDarkAppearance ? .light : .dark
        }
    }
}

/// Model options offered in Settings. Cheap-first: Haiku is the default.
///
/// These raw values are BARE ids, which is correct for BYOK (Anthropic's own
/// endpoint rejects a provider prefix) and is deliberately not the whole story
/// on hosted: the daemon's relay qualifies the id before it reaches the
/// gateway, because only the daemon knows which endpoint is downstream. See
/// `qualify_model_for_gateway` in squelch-api.
///
/// A raw value here must also appear in the hosted allow-lists
/// (`SQUELCH_CONTROL_ASSISTANT_MODELS` and the gateway's provider key, which
/// `squelch-control llm sync` writes) or a hosted turn 403s. `claude-opus-4-8`
/// was offered here long after it had fallen out of both, so the Opus option
/// could not have worked on hosted even once the prefix was right.
enum AssistantModel: String, CaseIterable, Sendable {
    case haiku = "claude-haiku-4-5"
    case opus = "claude-opus-5"

    /// Raw values that used to name a still-offered option, mapped forward.
    /// Without this, `AssistantModel(rawValue:)` fails for anyone whose stored
    /// preference predates the rename and they are silently moved to Haiku,
    /// which is a quieter kind of wrong than an error.
    static func migrating(rawValue: String) -> AssistantModel? {
        switch rawValue {
        case "claude-opus-4-8": .opus
        default: AssistantModel(rawValue: rawValue)
        }
    }

    var shortLabel: String {
        switch self {
        case .haiku: "Haiku"
        case .opus: "Opus"
        }
    }

    var label: String {
        switch self {
        case .haiku: "Haiku 4.5: fast and cheap (recommended)"
        case .opus: "Opus 5: smartest, pricier"
        }
    }

    /// Rough $/1M rates, for the local cost estimate on the Usage page.
    var rates: (input: Double, output: Double) {
        switch self {
        case .haiku: (1, 5)
        case .opus: (5, 25)
        }
    }
}

/// Where a ⌘K ask travels: through the user's own daemon (the hosted relay) or
/// straight to the provider with their own key. `relay` is the default because
/// on hosted the relay IS the product, and BYOK users flip the switch once and
/// it sticks. Harmless on self-host: a daemon that never advertises the
/// capability never runs relay whatever this says.
enum AssistantTransport: String, CaseIterable, Sendable {
    case relay = "relay"
    case byok = "byok"

    var label: String {
        switch self {
        case .relay: "Passband relay"
        case .byok: "My own key"
        }
    }
}
