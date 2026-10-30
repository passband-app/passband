// PostHog capture, written natively. The official SDK is deliberately NOT
// used: build.sh compiles a flat swiftc file list with no package resolution,
// and posthog-ios drags in vendored libwebp for a screenshot replay that does
// not exist on macOS anyway. Everything a Mac app needs from PostHog (events,
// screens, session analytics) is one JSON POST to /batch, and session replay
// is a second lane to /s/ (see `SessionReplay`), so the client lives here
// where all of it can be read.
//
// PRIVACY: nothing content-shaped goes through this file, and that is
// ENFORCED, not just observed: every event name and every string value must
// appear in the closed vocabulary below or it is dropped (and trips an assert
// in a debug build). Email data — subjects, senders, bodies, labels, questions
// — could only ever leave as a free-form string, so free-form strings cannot
// leave at all.
//
// There is exactly ONE string that leaves without being in that vocabulary: the
// hosted `analytics_id`, adopted as the install's distinct_id by `adopt` below.
// It is not an exception to the rule, it is the rule enforced a second way — a
// UUID shape check admits hex and dashes and nothing else, which carries as
// much mail as a closed set does. Read `adopt`'s comment before adding a second
// one; the honest count of bypasses in this file is one, and it should stay one.

import Foundation

// Only for the device idiom in the event context below, and fenced the way
// Pairing.swift fences its own UIKit import: the Mac target must not so much as
// name a framework it does not have, and the fence sits directly over the one
// expression that needs it so the two can never drift apart.
#if os(iOS)
    import UIKit
#endif

enum Analytics {
    /// PostHog PROJECT API key (phc_…, public by design — it can only ingest).
    /// Empty disables analytics entirely; the env var wins for dev overrides.
    private static let apiKey = "phc_uuYkpwXQYxrz33f4bQsi5ozAQ6LL5kPsnrnU7vVvshni"
    private static let host = "https://us.i.posthog.com"

    private static let client: PostHogClient? = {
        // A tester rehearsing the onboarding is not a user onboarding: no
        // uploader at all for that launch. The customer's practice inbox runs
        // in the ordinary app and reports like the rest of it (see `capture`).
        guard !RehearsalMode.launchedStandalone else { return nil }
        let env = ProcessInfo.processInfo.environment
        // A dev build is not a user: its launches and clicks landed in the
        // same project as the fleet's and read as real usage on the
        // dashboard. Only a shipped build (build-release.sh on the Mac, the
        // Release config on iOS, both of which define PASSBAND_RELEASE) gets
        // the baked-in key. A dev build reports only when PASSBAND_POSTHOG_KEY
        // is set on purpose, which is how analytics work gets tested.
        #if PASSBAND_RELEASE
            let key = env["PASSBAND_POSTHOG_KEY"] ?? apiKey
        #else
            guard let key = env["PASSBAND_POSTHOG_KEY"] else { return nil }
        #endif
        let endpoint = env["PASSBAND_POSTHOG_HOST"] ?? host
        guard key.hasPrefix("phc_"), let url = URL(string: "\(endpoint)/batch/"),
            let replayURL = URL(string: "\(endpoint)/s/")
        else { return nil }
        return PostHogClient(apiKey: key, endpoint: url, snapshotEndpoint: replayURL)
    }()

    /// The Settings > Privacy level, read straight from UserDefaults on every
    /// event — thread-safe where the main-actor Prefs is not, and a change in
    /// the pane applies to the very next capture without any plumbing.
    private static var level: TelemetryLevel {
        TelemetryLevel(
            rawValue: UserDefaults.standard.string(forKey: TelemetryLevel.prefKey) ?? "")
            ?? .full
    }

    /// Install lifecycle observers and record the first screen. Call once from
    /// the app delegate; every other entry point is safe to call regardless.
    static func start() {
        guard let client else { return }
        _ = installDay  // stamp the install on the first launch that can report
        let nc = NotificationCenter.default
        nc.addObserver(
            forName: Platform.didBecomeActiveNotification, object: nil, queue: nil
        ) { _ in lifecycle("Application Opened") }
        nc.addObserver(
            forName: Platform.didResignActiveNotification, object: nil, queue: nil
        ) { _ in
            lifecycle("Application Backgrounded")
            client.flush()
        }
        nc.addObserver(
            forName: Platform.willTerminateNotification, object: nil, queue: nil
        ) { _ in client.flushBlocking() }
    }

    /// Lifecycle events ride at the `minimal` level — they are what makes a
    /// session visible at all.
    private static func lifecycle(_ event: String) {
        guard level != .none else { return }
        client?.capture(event)
    }

    /// The closed set of event names this app emits. A new event is added HERE
    /// first — an unknown name at a call site is a bug, not a data point.
    private static let allowedEvents: Set<String> = [
        "$screen", "Application Opened", "Application Backgrounded",
        "email_archived", "email_done", "email_reopened", "email_labeled",
        "email_remind", "email_not_spam",
        "block_rule_created", "compose_opened", "compose_send",
        "thread_opened", "thread_live_arrival", "undo_fired", "assistant_asked",
        "search_deeper_started",
        "triage_corrected", "triage_digest", "rule_created", "rule_deleted",
        "notification_opened", "sealed_revealed",
        // Smart re-notify (docs/RENOTIFY.md): a second banner posted, and
        // tapped. A trigger word, a message count and one bool (a deadline
        // was a reason); never who or what.
        "renotify_shown", "renotify_opened",
        // Event reminders (docs/CALENDAR.md, "Reminders and tickets"): a
        // banner posted, and tapped. The lead in minutes and a few bools; the
        // tap says only which kind of thing it opened. Never the event.
        "calendar_reminder_shown", "calendar_reminder_opened",
        "shipment_cleared",
        // Smart clear on the Reading zone: how much it cleared, split by the
        // daemon's reason codes as counts, and whether it was taken back.
        "smart_clear", "smart_clear_undone",
        "connect_succeeded", "connection_lost", "connection_restored",
        "account_added",
        "tour_started", "tour_completed", "tour_skipped", "whats_new_shown",
        "invite_sent", "invite_nudge_accepted", "invite_nudge_dismissed",
        "ios_nudge_closed", "agent_nudge_closed",
        "group_created", "group_updated", "group_deleted",
        // The calendar page: opened, the
        // connect card's two answers, the user's word on a ghost, and the
        // three writes to a real calendar. Bools and a closed state word;
        // never a title, a place, a time, a calendar or an address. A write
        // carries three bools: `all_day`, `guests` (the event has any) and
        // `notified` (Google was asked to email them). Never how many
        // guests, and never who.
        "calendar_opened", "calendar_connect_clicked", "calendar_connect_dismissed",
        "calendar_ghost_marked",
        "calendar_event_created", "calendar_event_edited", "calendar_event_deleted",
        // Discovery: which feature sets get found and which get kept. `full`
        // only: this is the "which actions are used" layer, not whether the
        // product works. Whether anybody visits the page is its `$screen`
        // (`discovery`, a MainView), so it has no event of its own.
        "feature_found", "feature_kept",
        // Frustration, from session replay's click stream (`SessionReplay`):
        // three clicks in one spot inside a second, and a click after which
        // nothing on screen changed. PostHog's own names, so its recordings
        // list can filter on them. `full` only, like the replay they mark.
        "$rageclick", "$dead_click",
        // Derived, never captured by a call site: see `milestones`.
        "milestone", "catchup_completed",
    ]

    /// The counter events that ride at `minimal`: anonymous counts and
    /// closed-vocabulary enums that measure whether the product works (sends,
    /// triage volume, corrections, connection health) with no behavioral
    /// detail. `full` adds the remaining action verbs (`compose_opened`,
    /// `email_labeled`) — the "which actions are used" layer.
    private static let minimalEvents: Set<String> = [
        "email_archived", "email_done", "email_reopened", "email_remind",
        "email_not_spam",
        "block_rule_created", "compose_send", "thread_opened",
        // One bool: mail landed in the thread on screen, and whether the reader
        // was at the end of it (carried to the new message) or up in the
        // history (held in place, pointed at it). Anonymous, and it is the only
        // measure of whether live-loading is worth the round trips.
        "thread_live_arrival",
        "undo_fired", "assistant_asked",
        "triage_corrected", "triage_digest", "rule_created", "rule_deleted",
        "notification_opened", "sealed_revealed",
        // Whether the second banner earns its interruption is the "does it
        // work" question for re-notify, so both ride with the first banner's.
        "renotify_shown", "renotify_opened",
        // And whether a reminder is worth its interruption, the same way.
        "calendar_reminder_shown", "calendar_reminder_opened",
        "connect_succeeded", "connection_lost", "connection_restored",
        "account_added",
        // Onboarding carries where it started from and the practice step it
        // ended on. Whether the first run explains itself is exactly the
        // "does the product work" question this level exists for. Every
        // other event captured while the practice inbox is up carries
        // `practice: true` (see `capture`), so a fixture rule or a fixture
        // done never counts as the real thing.
        "tour_started", "tour_completed", "tour_skipped",
        // Sharing carries counts and nothing else: how many invites went, how
        // many did not, and whether the one-time ask was taken up. No address
        // is anywhere near this, at any level.
        "invite_sent", "invite_nudge_accepted", "invite_nudge_dismissed",
        // The TestFlight ask closing, with one bool: whether the link was
        // copied. A QR scan leaves no trace, so this is a floor on uptake.
        "ios_nudge_closed",
        // The agent-door ad closing, with one bool: whether its button was
        // taken to Settings > Agents. Whether an agent then connects is not
        // measured here, at any level.
        "agent_nudge_closed",
        // Send groups carry a mode and a member COUNT. Whether people organize
        // their correspondents into audiences at all, and which shape they
        // reach for, is the "does the product work" question for this feature;
        // who is in one is not measured anywhere, at any level.
        "group_created", "group_updated", "group_deleted",
        // The funnel: one event per install per first-time, and the first
        // backfill's size and duration. Both are the most "does the product
        // work" numbers there are, and neither carries a behavioral detail.
        "milestone", "catchup_completed",
    ]

    /// The closed set of STRING property values allowed off the machine.
    /// Bools and numbers pass freely — cardinality that small cannot carry
    /// mail. Strings can, so only these exact ones exist.
    private static let allowedStrings: Set<String> =
        Set(MainView.allCases.map(\.rawValue)).union([
            // compose_send / compose_opened
            "new", "reply", "forward", "sent", "guard_blocked", "forbidden", "failure",
            // compose_send's outcome for a mail put in the daemon's outbox.
            "scheduled",
            // undo_fired kinds: `String(describing:)` of every UndoKind case.
            // Listed by hand, so a new case must land here too or its undo
            // trips the debug assert.
            "archive", "done", "label", "ruleDelete", "groupDelete", "remind", "send",
            "draftDelete", "schedule", "scheduledCancel", "ghostDismiss", "skip",
            // calendar_ghost_marked states: the PATCH's own words, plus
            // "cleared" for the null that takes the mark off. "calendar" (the
            // page, and thread_opened's source from it) is a MainView.
            "going", "dismissed", "cleared",
            // calendar_opened's `view`: which way the page was drawn when it
            // opened (CalendarViewMode's raw values).
            "agenda", "month", "week",
            // group_* modes — how an audience is addressed. "individual" is the
            // only one that is not also a header name.
            "to", "bcc", "individual",
            // triage_corrected axes and wire values — the daemon's closed
            // TriageAxis::allowed vocabulary, mirrored in TriageTargets.
            "kinds", "destinations", "show_in_fye", "external_access", "true", "false",
            "reading", "records", "correspondence", "editorial", "promotional", "bill",
            "receipt", "financial_update", "delivery", "event_reservation", "account_service",
            "authentication_security",
            "tier", "category", "sensitivity",
            "past_due", "deadline", "signal", "noise",
            "invoice", "autopay_bill", "banking_statement", "transaction_alert",
            "marketing", "general", "sealed", "normal", "unset",
            // rule_created dispositions
            "surface", "squelch", "filtered",
            // tour_started sources (TourSource). "settings" is a MainView
            // above; "rehearsal" never leaves the machine (no client on a
            // standalone launch) but has to be in the vocabulary or the
            // debug assert fires on the tester's own build.
            "first_run", "rehearsal",
            // assistant_asked models — and search_deeper_started's, which is
            // the same closed set because it is the same picker's vocabulary.
            "haiku", "opus",
            // search_deeper_started triggers: which signal started the deeper
            // search lane (SearchIntent.Trigger). The query itself is the
            // reader's mail and goes nowhere near this event; these two say
            // only which RULE fired.
            "question", "no_strict_hits", "requested",
            // assistant_asked transports
            "relay", "byok",
            // invite_sent sources — where the share sheet was raised from
            // (`ShareOrigin`). "settings" is also a MainView above; a Set union
            // makes the duplicate free, and naming it here is what keeps this
            // list readable as the vocabulary of THIS event.
            "rail", "settings", "nudge",
            // renotify_* triggers: the daemon's closed Trigger set.
            "client_online", "meeting_end",
            // calendar_reminder_opened targets: what the tap showed.
            // "event" and "none" are named here; nothing else says them.
            "attachment", "mail", "event", "none",
            // feature_found / feature_kept sets: `FeatureSet`'s raw values, the
            // discovery page's seven rows. Listed by hand like the undo kinds,
            // so a new set must land here too or its find trips the debug assert.
            "clear", "flow", "later", "teach", "find", "read", "sweep",
            // thread_opened sources (OpenSource): WHICH SURFACE the reader was
            // entered from. "sitrep", "emails" are MainViews above, "reading" a
            // triage kind, "reply" a compose kind; the rest are named here.
            // "groups" was a MainView until the page moved into Settings. "reader" is the walk from one queued thread to the
            // next, which is an open the reader made, not the person.
            "sitrep_card", "search", "browse", "assistant", "deeper_search",
            "notification", "reader", "tour", "groups",
            // Verb origins (Via): how the verb was INPUT, and (Surface) which
            // page it was input on. "process", "sitrep", "emails" are
            // MainViews; "archive"/"done" already exist as undo kinds.
            "key", "click", "swipe", "assistant",
            // milestone kinds: the first time this install did each thing.
            "first_connect", "first_triage", "first_open", "first_action",
            "first_send", "first_rule", "first_ask", "first_search",
            // identify: the person-level enums. TelemetryLevel raw values and
            // ThreadStyleDefault raw values, nothing else.
            "full", "minimal", "none", "auto", "classic", "bubbles",
        ])

    /// The funnel events that describe onboarding rather than happen inside
    /// it — never tagged `practice`, whatever mode the app is in.
    ///
    /// The two discovery events ride with them. Practice mail never reaches the
    /// ledger (`DiscoveryLedger.counts`), so the only find that can fire while
    /// the practice inbox is up is the tour's own completion granting `clear`,
    /// and that one is as real as `tour_completed` is.
    private static let onboardingEvents: Set<String> = [
        "tour_started", "tour_completed", "tour_skipped",
        "feature_found", "feature_kept",
    ]

    /// Screen views ride at `minimal` alongside lifecycle events.
    static func screen(_ name: String) {
        guard level != .none else { return }
        guard allowedStrings.contains(name) else {
            assertionFailure("analytics: screen name outside vocabulary")
            return
        }
        currentScreen.set(name)
        client?.capture("$screen", properties: ["$screen_name": name])
    }

    /// The last screen named, already vocabulary-checked. Session replay
    /// labels its frames with it (the rrweb meta event's `href`).
    static let currentScreen = LockedValue("")

    // MARK: - session replay

    /// Replay records only at `full`: a recording of the layout is the most
    /// behavioral thing this file sends, so it rides with the action verbs and
    /// stops the moment the level drops. Read per tick, like every event.
    static var replayEnabled: Bool { client != nil && level == .full }

    /// True when the next replay record must be a full snapshot: the session
    /// rotated (or none has started), and a recording cannot open on a click.
    static var replayNeedsFull: Bool { client?.snapshotNeedsFull ?? false }

    /// Send rrweb records for this session's recording. Every String in them
    /// must be one of our tags, a style string in our exact grammar with grey
    /// paints, the screen URL for an allowed screen name, or an icon this
    /// process rendered (`ReplayWireframe.validate`); a record with anything
    /// else is dropped whole, and a debug build stops on it.
    static func replay(_ records: [[String: Any]], full: Bool) {
        guard replayEnabled else { return }
        // Checked on the client's queue, not here: a full snapshot is
        // hundreds of style strings through a regex, and this is called from
        // the main thread.
        client?.captureSnapshot(records, isFull: full) {
            ReplayWireframe.validate($0, extra: allowedStrings)
        }
    }

    /// Counter events (`minimalEvents`) ride at `minimal`; the remaining
    /// action verbs are `full`-only.
    static func capture(_ event: String, _ properties: [String: Any] = [:]) {
        switch level {
        case .none: return
        case .minimal: guard minimalEvents.contains(event) else { return }
        case .full: break
        }
        guard allowedEvents.contains(event) else {
            assertionFailure("analytics: event outside vocabulary: \(event)")
            return
        }
        var safe: [String: Any] = [:]
        for (key, value) in properties {
            switch value {
            case is Bool, is Int, is Double:
                safe[key] = value
            case let s as String where allowedStrings.contains(s):
                safe[key] = s
            default:
                assertionFailure("analytics: value outside vocabulary for \(key)")
            }
        }
        // The practice inbox is the real app on fixture mail, so its `e`, its
        // rule save and its undo all arrive here as the ordinary events. One
        // bool keeps them out of the real counts without a second vocabulary.
        // The onboarding funnel itself is the exception: those three ARE the
        // real thing, and a dashboard that filters practice out must keep them.
        if RehearsalMode.isEnabled, !onboardingEvents.contains(event) {
            safe["practice"] = true
        }
        client?.capture(event, properties: safe)
        noteMilestone(event, safe)
    }

    // MARK: - milestones

    /// The funnel, derived rather than captured: the FIRST time an install
    /// does each of these, one `milestone` event goes out with the kind and
    /// how many days after install it happened. Derived inside `capture` so
    /// a call site cannot forget it and a refactor cannot move it: the table
    /// is the whole definition.
    ///
    /// Each entry is the event that proves the milestone and a predicate on
    /// its (already vocabulary-checked) properties. `compose_send` is a send
    /// only when it went; `rule_created` is a first rule only when it was not
    /// an edit of one.
    private static let milestones:
        [(event: String, kind: String, when: @Sendable ([String: Any]) -> Bool)] =
        [
            ("connect_succeeded", "first_connect", { _ in true }),
            ("triage_digest", "first_triage", { ($0["total"] as? Int ?? 0) > 0 }),
            ("thread_opened", "first_open", { _ in true }),
            ("email_done", "first_action", { _ in true }),
            ("email_archived", "first_action", { _ in true }),
            ("compose_send", "first_send", { $0["outcome"] as? String == "sent" }),
            ("rule_created", "first_rule", { !($0["edit"] as? Bool ?? false) }),
            ("assistant_asked", "first_ask", { _ in true }),
            ("search_deeper_started", "first_search", { _ in true }),
        ]

    private static func noteMilestone(_ event: String, _ props: [String: Any]) {
        // The practice inbox is not the product working, and its events carry
        // `practice: true` for the same reason.
        guard !RehearsalMode.isEnabled else { return }
        for m in milestones where m.event == event && m.when(props) {
            let key = "app.passband.analytics.milestone.\(m.kind)"
            let defaults = UserDefaults.standard
            guard !defaults.bool(forKey: key) else { continue }
            defaults.set(true, forKey: key)
            client?.capture("milestone", properties: ["kind": m.kind, "day": installDay])
        }
    }

    /// Whole days since this install first ran with analytics on. Stamped
    /// the first time it is read, so it counts from the first launch that
    /// could have sent anything.
    private static var installDay: Int {
        let key = "app.passband.analytics.installedAt"
        let defaults = UserDefaults.standard
        var at = defaults.double(forKey: key)
        if at == 0 {
            at = Date().timeIntervalSince1970
            defaults.set(at, forKey: key)
        }
        return max(0, Int((Date().timeIntervalSince1970 - at) / 86400))
    }

    /// Whether this install was paired through a hosted link: the one string
    /// bypass in this file (`adopt`) is also the one way to know.
    static var isHosted: Bool { UserDefaults.standard.bool(forKey: adoptedKey) }

    /// Person properties, sent as a `$set` so PostHog can cohort installs
    /// without a join: hosted or self-run, how many mailboxes, which platform
    /// (the envelope already carries it), which telemetry level, which thread
    /// style. Every value goes through the same closed-vocabulary check as an
    /// event property, so a String here is an enum's raw value or nothing.
    /// Rides at `minimal`: a cohort is not a behavior.
    static func identify(_ properties: [String: Any]) {
        guard level != .none else { return }
        var safe: [String: Any] = ["hosted": isHosted, "telemetry_level": level.rawValue]
        for (key, value) in properties {
            switch value {
            case is Bool, is Int, is Double:
                safe[key] = value
            case let s as String where allowedStrings.contains(s):
                safe[key] = s
            default:
                assertionFailure("analytics: person value outside vocabulary for \(key)")
            }
        }
        client?.capture("$set", properties: ["$set": safe])
    }

    /// At-most-daily events, for periodic snapshot counters like
    /// `triage_digest`. UserDefaults-backed so a relaunch does not re-emit;
    /// the level gate stays in `capture`.
    static func daily(_ event: String, _ properties: [String: Any] = [:]) {
        guard level != .none else { return }  // don't stamp a day we sent nothing
        // Not from the practice inbox: a digest of fixture counts would burn
        // the day's stamp and silence the real one — on a first run, the
        // install's first real digest of all.
        guard !RehearsalMode.isEnabled else { return }
        let key = "app.passband.analytics.daily.\(event)"
        let last = UserDefaults.standard.double(forKey: key)
        let now = Date().timeIntervalSince1970
        guard now - last >= 24 * 3600 else { return }
        UserDefaults.standard.set(now, forKey: key)
        capture(event, properties)
    }

    /// Marks that a hosted `aid` has already been adopted. First aid wins,
    /// forever.
    private static let adoptedKey = "app.passband.analytics.aidAdopted"

    /// Adopt the analytics id a hosted pairing link carried, as this install's
    /// PostHog `distinct_id`.
    ///
    /// WHY THIS EXISTS: PostHog otherwise sees a random per-install UUID, and
    /// the control plane sees an email address, and nothing joins them — so
    /// "did anyone we invited ever open the app" is unanswerable. The `aid` is
    /// the join key, minted control-plane side and opaque on both ends. PostHog
    /// still never sees an email; it sees an id that only the control plane can
    /// resolve to a person, which is the whole design.
    ///
    /// THIS BYPASSES THE EVENT VOCABULARY ON PURPOSE. Every other string that
    /// leaves this file must appear in `allowedStrings`, because mail content
    /// could only ever escape as a free-form string. A UUID is not free-form:
    /// the shape check below reduces it to hex and dashes at fixed offsets,
    /// which carries exactly as much mail as a closed set does — none. That
    /// check IS the guarantee here, so it is not optional, and the id must NOT
    /// be added to `allowedStrings`/`allowedEvents` instead: those sets are
    /// closed and enumerable and a per-person id would end both properties.
    ///
    /// THREAT MODEL: any web page can open a `passband://` URL, so a hostile
    /// page can name an `aid` of its choosing. It buys nothing. Adoption
    /// happens only after a claim AND a successful connect against the daemon
    /// the SAME link named, so the attacker needs a live, unspent pairing code
    /// for a daemon the victim then completes pairing with — at which point
    /// they had the mailbox, not merely the analytics. There is no read-back
    /// channel either: the id is written outward and never surfaced. Residual
    /// risk is analytics pollution (one install's events filed under an id of
    /// the attacker's choosing), and nothing about mail, credentials, or the
    /// account.
    static func adopt(analyticsId: String) {
        guard UUID(uuidString: analyticsId) != nil else {
            assertionFailure("analytics: adopted id is not a uuid")
            return
        }
        // FIRST AID WINS. A `distinct_id` is per-install and an install is one
        // human; the multi-account case is that one human running two mailboxes,
        // not two people sharing a Mac. The funnel this exists for is "the
        // person we invited started using the app", so the FIRST hosted account
        // is the one that answers it — a second account's aid is simply not
        // adopted, and the person keeps the identity their history is under.
        let defaults = UserDefaults.standard
        guard !defaults.bool(forKey: adoptedKey) else { return }
        defaults.set(true, forKey: adoptedKey)

        // With telemetry off the id still flips, but no alias event is sent.
        // The flip costs nothing and is not a transmission: it means that if
        // the user later turns telemetry back on, what they send arrives as
        // THEM rather than opening a third identity. The alias is a real event,
        // so it waits — while the user has said nothing leaves, nothing leaves.
        client?.adoptDistinctId(analyticsId.lowercased(), mergeHistory: level != .none)
    }
}

/// Buffered batch uploader. All state is confined to `queue`; the public
/// surface is safe to call from any thread or actor.
final class PostHogClient: @unchecked Sendable {
    private let apiKey: String
    private let endpoint: URL
    private let snapshotEndpoint: URL
    private let queue = DispatchQueue(label: "app.passband.analytics")

    private var buffer: [[String: Any]] = []
    private var inFlight = false

    /// Session replay's own lane: `$snapshot` events go to /s/, not /batch,
    /// and they are big (a full snapshot each), so they get a small buffer of
    /// their own and never crowd an ordinary event out of the main one.
    private var snapshots: [[String: Any]] = []
    private var snapshotsInFlight = false
    /// The session the recording's last full snapshot opened. A record under
    /// any other session id has nothing to draw on, so it waits for a full one.
    private var snapshotSession = ""
    private static let snapshotCap = 40
    private static let snapshotFlushAt = 12

    // Session analytics need only a shared $session_id on the events; PostHog
    // derives duration and entry/exit server-side. The id must be a UUIDv7 —
    // the ingestion pipeline rejects sessions whose ids do not time-order.
    // Rotation mirrors posthog-ios: 30 idle minutes ends a session, and one
    // session never exceeds 24 hours.
    private var sessionId = ""
    private var sessionStart = Date.distantPast
    private var lastActivity = Date.distantPast

    /// Anonymous install id. Analytics for a single-user app needs stability,
    /// not identity, so a random UUID minted once is the whole story — until a
    /// hosted pairing hands over an `aid`, at which point `adoptDistinctId`
    /// swaps this for the control plane's opaque per-person id so the two
    /// halves of the funnel are the same row. Still a UUID, still carrying no
    /// address: the control plane holds the email, PostHog holds the id, and
    /// nothing on this machine can join them.
    ///
    /// A `var`, but a queue-confined one: every read and write below happens
    /// inside `queue`. Initialized directly in `init` rather than on the queue,
    /// which is safe because nothing can enqueue against a client that has not
    /// finished being constructed, and the lazy `static let` that builds it is
    /// the memory barrier that publishes the initial value to every later
    /// queue block.
    private var distinctId: String

    private static let bufferCap = 500
    private static let flushAt = 20
    private static let flushEvery: TimeInterval = 15

    /// Where the install id lives across launches. An adopted `aid` is written
    /// straight back to this same key rather than to one beside it: after
    /// adoption the aid simply IS this install's id, so a relaunch reads it
    /// like any other and needs no re-adoption logic at all.
    private static let distinctIdKey = "app.passband.analytics.distinctId"

    private let iso: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    /// PostHog's own device buckets: an iPad is a Tablet, everything else this
    /// target runs on is a Mobile.
    ///
    /// Asked of the KERNEL rather than of `UIDevice.current.userInterfaceIdiom`,
    /// which the SDK marks main-actor. This is not a style preference: the
    /// client is a lazy `static let`, so its initializer runs on whichever
    /// thread captures the first event, and reading a main-actor API from
    /// there is exactly the thing the compiler now refuses. The model
    /// identifier is a process constant and answers the same question without
    /// an actor to hop to.
    #if os(iOS)
        private static var deviceType: String {
            var info = utsname()
            uname(&info)
            let machine = withUnsafeBytes(of: &info.machine) { raw in
                String(decoding: raw.prefix(while: { $0 != 0 }), as: UTF8.self)
            }
            // A simulator reports the HOST's architecture there ("arm64"), so
            // the device it is pretending to be comes from the environment.
            let model =
                ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] ?? machine
            return model.hasPrefix("iPad") ? "Tablet" : "Mobile"
        }
    #endif

    /// The per-event envelope PostHog reads to place a session on a platform.
    ///
    /// The platform half is COMPILED, not detected: this file ships in both the
    /// Mac and the iOS target, and a hardcoded "macOS" here reported every
    /// iPhone as a desktop — which does not merely mislabel iOS, it poisons
    /// every whole-app number by folding two platforms into one. `$os_version`
    /// was always honest (ProcessInfo answers per-process); only the two
    /// constants beside it were not.
    private let context: [String: Any] = {
        let info = Bundle.main.infoDictionary ?? [:]
        let os = ProcessInfo.processInfo.operatingSystemVersion
        let version = info["CFBundleShortVersionString"] as? String ?? "0"
        var ctx: [String: Any] = [
            "$app_name": "Passband",
            "$app_version": version,
            "$app_build": info["CFBundleVersion"] as? String ?? "0",
            "$os_version": "\(os.majorVersion).\(os.minorVersion).\(os.patchVersion)",
            "$lib": "passband-native",
            "$lib_version": version,
        ]
        #if os(iOS)
            ctx["$os_name"] = "iOS"
            ctx["$device_type"] = PostHogClient.deviceType
        #else
            ctx["$os_name"] = "macOS"
            ctx["$device_type"] = "Desktop"
        #endif
        return ctx
    }()

    init(apiKey: String, endpoint: URL, snapshotEndpoint: URL) {
        self.apiKey = apiKey
        self.endpoint = endpoint
        self.snapshotEndpoint = snapshotEndpoint

        if let saved = UserDefaults.standard.string(forKey: Self.distinctIdKey) {
            distinctId = saved
        } else {
            distinctId = UUID().uuidString.lowercased()
            UserDefaults.standard.set(distinctId, forKey: Self.distinctIdKey)
        }

        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + Self.flushEvery, repeating: Self.flushEvery)
        timer.setEventHandler { [weak self] in
            self?.flushLocked()
            self?.flushSnapshotsLocked()
        }
        timer.resume()
        flushTimer = timer
    }
    private var flushTimer: DispatchSourceTimer?

    func capture(_ event: String, properties: [String: Any] = [:]) {
        // JSON-shaped dictionaries are not Sendable by type, but every one of
        // them is confined to `queue` — the box states that instead of warning.
        let boxed = Unchecked(properties)
        queue.async {
            let now = Date()
            var props = self.context
            props["$session_id"] = self.currentSessionId(now)
            for (k, v) in boxed.value { props[k] = v }
            self.buffer.append([
                "event": event,
                "distinct_id": self.distinctId,
                "timestamp": self.iso.string(from: now),
                "properties": props,
            ])
            if self.buffer.count > Self.bufferCap {
                self.buffer.removeFirst(self.buffer.count - Self.bufferCap)
            }
            if self.buffer.count >= Self.flushAt { self.flushLocked() }
        }
    }

    func flush() {
        queue.async {
            self.flushLocked()
            self.flushSnapshotsLocked()
        }
    }

    /// Terminate-path flush: the process is about to exit, so give the upload
    /// a bounded moment to leave the machine instead of dying in the buffer.
    /// Events go first; the recording's tail is worth less than they are.
    func flushBlocking(timeout: TimeInterval = 1.5) {
        let sem = DispatchSemaphore(value: 0)
        queue.async {
            self.flushLocked { sem.signal() }
            self.flushSnapshotsLocked()
        }
        _ = sem.wait(timeout: .now() + timeout)
    }

    /// Whether the replay must open (or reopen) with a full snapshot: no
    /// recording yet, or the session it opened has since ended (30 idle
    /// minutes, or 24 hours). Read from `replayState`, a lock-guarded mirror,
    /// NOT from `queue`: the replay asks on the main thread every tick, and
    /// `queue` may be busy encoding and gzipping a flush of big frames.
    var snapshotNeedsFull: Bool {
        let st = replayState.get()
        let now = Date()
        let live =
            !st.sessionId.isEmpty
            && now.timeIntervalSince(st.lastActivity) <= 30 * 60
            && now.timeIntervalSince(st.sessionStart) <= 24 * 3600
        return !live || st.snapshotSession != st.sessionId
    }

    /// The session facts the main thread needs, copied out of `queue` each
    /// time they change. Written only from inside `queue`.
    private struct ReplayState: Sendable {
        var sessionId = ""
        var sessionStart = Date.distantPast
        var lastActivity = Date.distantPast
        var snapshotSession = ""
    }
    private let replayState = LockedValue(ReplayState())

    /// One `$snapshot` event carrying rrweb records, filed under the session
    /// the ordinary events are on (which is what puts them in one recording).
    /// `$window_id` is the session id too: one window, one recording.
    func captureSnapshot(
        _ records: [[String: Any]], isFull: Bool,
        validate: @escaping @Sendable ([[String: Any]]) -> Bool
    ) {
        let boxed = Unchecked(records)
        queue.async {
            guard validate(boxed.value) else {
                assertionFailure("analytics: replay record outside vocabulary")
                return
            }
            let now = Date()
            let sid = self.currentSessionId(now)
            if isFull {
                self.snapshotSession = sid
                var st = self.replayState.get()
                st.snapshotSession = sid
                self.replayState.set(st)
            } else if sid != self.snapshotSession {
                // A click in a session with no picture yet: nothing to place it on.
                return
            }
            var props = self.context
            props["$session_id"] = sid
            props["$window_id"] = sid
            props["$snapshot_source"] = "web"
            props["$snapshot_data"] = boxed.value
            // The id rides in the properties too, as PostHog's SDKs send it.
            props["distinct_id"] = self.distinctId
            self.snapshots.append([
                "event": "$snapshot",
                "distinct_id": self.distinctId,
                "timestamp": self.iso.string(from: now),
                "uuid": UUID().uuidString.lowercased(),
                "api_key": self.apiKey,
                "properties": props,
            ])
            // Oldest first. A recording with a hole in it still plays from
            // the next full snapshot; one that never uploads does not.
            self.trimSnapshots()
            if self.snapshots.count >= Self.snapshotFlushAt { self.flushSnapshotsLocked() }
        }
    }

    /// Switch this install's `distinct_id` to an id the control plane minted,
    /// optionally telling PostHog that the two ids are one person.
    ///
    /// `$create_alias` is the merge, and its direction is the part that is easy
    /// to get backwards: the ENVELOPE carries the OLD id and the `alias`
    /// property carries the NEW one, matching what posthog-js and posthog-python
    /// send. PostHog folds the aliased id into the one already on the event, so
    /// naming the new id in the envelope would merge the wrong direction and
    /// strand this install's history.
    ///
    /// Everything is done inside `queue` because `distinctId` and `buffer` live
    /// there, and doing it in one block is also what makes the ordering true:
    /// events already buffered keep the id they were captured under, because
    /// they happened to the old identity. Only what is captured after this
    /// block runs belongs to the new one.
    ///
    /// `mergeHistory: false` still flips the id — see `Analytics.adopt`.
    func adoptDistinctId(_ newId: String, mergeHistory: Bool) {
        queue.async {
            let old = self.distinctId
            // Idempotent: the same aid arrives again on every re-pair and every
            // hosted sign-in, and a second alias to an id we already answer to
            // would be a wasted event at best.
            guard newId != old else { return }

            if mergeHistory {
                let now = Date()
                var props = self.context
                props["$session_id"] = self.currentSessionId(now)
                props["distinct_id"] = old
                props["alias"] = newId
                self.buffer.append([
                    "event": "$create_alias",
                    "distinct_id": old,
                    "timestamp": self.iso.string(from: now),
                    "properties": props,
                ])
                // The cap trims from the FRONT, so the alias — appended last —
                // is the one event a full buffer cannot cost us.
                if self.buffer.count > Self.bufferCap {
                    self.buffer.removeFirst(self.buffer.count - Self.bufferCap)
                }
            }

            self.distinctId = newId
            UserDefaults.standard.set(newId, forKey: Self.distinctIdKey)
            // Sent now rather than on the next 15s tick: the alias is what makes
            // the funnel joinable, and the moment right after pairing is exactly
            // when a first-run install is most likely to be quit.
            self.flushLocked()
        }
    }

    // MARK: - queue-confined

    private func currentSessionId(_ now: Date) -> String {
        if sessionId.isEmpty
            || now.timeIntervalSince(lastActivity) > 30 * 60
            || now.timeIntervalSince(sessionStart) > 24 * 3600
        {
            sessionId = Self.uuidV7(now)
            sessionStart = now
        }
        lastActivity = now
        var st = replayState.get()
        st.sessionId = sessionId
        st.sessionStart = sessionStart
        st.lastActivity = now
        replayState.set(st)
        return sessionId
    }

    private func flushLocked(_ done: (@Sendable () -> Void)? = nil) {
        guard !inFlight, !buffer.isEmpty else {
            done?()
            return
        }
        let batch = Unchecked(buffer)
        buffer.removeAll()
        inFlight = true

        var req = URLRequest(url: endpoint)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try? JSONSerialization.data(
            withJSONObject: ["api_key": apiKey, "batch": batch.value])

        URLSession.shared.dataTask(with: req) { _, response, error in
            self.queue.async {
                self.inFlight = false
                let code = (response as? HTTPURLResponse)?.statusCode ?? 0
                // Requeue only what can succeed later: network failures and
                // server errors. A 4xx means the payload itself is bad and a
                // retry would loop forever — those events are dropped.
                if error != nil || code == 0 || code >= 500 {
                    self.buffer.insert(contentsOf: batch.value, at: 0)
                    if self.buffer.count > Self.bufferCap {
                        self.buffer.removeFirst(self.buffer.count - Self.bufferCap)
                    }
                }
                done?()
            }
        }.resume()
    }

    /// The replay lane's upload: a bare JSON array of events, each carrying
    /// its own `api_key`, gzipped. Same requeue rule as the events; a batch
    /// PostHog refuses (4xx) is dropped, and so is its full snapshot's claim
    /// to have opened the recording (`discardSnapshots`). Frames buffered before the user turned telemetry
    /// below `full` are thrown away here rather than sent late.
    private func flushSnapshotsLocked() {
        guard Analytics.replayEnabled else {
            discardSnapshots(snapshots)
            snapshots.removeAll()
            return
        }
        guard !snapshotsInFlight, !snapshots.isEmpty else { return }
        let batch = Unchecked(snapshots)
        snapshots.removeAll()
        guard
            let json = try? JSONSerialization.data(withJSONObject: batch.value),
            let body = Gzip.compress(json)
        else {
            discardSnapshots(batch.value)
            return
        }
        snapshotsInFlight = true

        var req = URLRequest(url: snapshotEndpoint)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("gzip", forHTTPHeaderField: "Content-Encoding")
        req.httpBody = body

        URLSession.shared.dataTask(with: req) { _, response, error in
            self.queue.async {
                self.snapshotsInFlight = false
                let code = (response as? HTTPURLResponse)?.statusCode ?? 0
                if error != nil || code == 0 || code >= 500 {
                    self.snapshots.insert(contentsOf: batch.value, at: 0)
                    self.trimSnapshots()
                } else if !(200..<300).contains(code) {
                    self.discardSnapshots(batch.value)
                }
            }
        }.resume()
    }

    /// Hold the replay buffer to its cap, oldest out first.
    private func trimSnapshots() {
        guard snapshots.count > Self.snapshotCap else { return }
        let over = snapshots.count - Self.snapshotCap
        discardSnapshots(Array(snapshots.prefix(over)))
        snapshots.removeFirst(over)
    }

    /// Snapshot events that will never reach PostHog. If one of them held a
    /// full snapshot (rrweb type 2), it may have been the one that opens the
    /// recording, so the next record must be a full snapshot again; without
    /// this, later pointer records would play onto nothing.
    private func discardSnapshots(_ events: [[String: Any]]) {
        let lostFull = events.contains { event in
            let data = (event["properties"] as? [String: Any])?["$snapshot_data"] as? [[String: Any]]
            return data?.contains { $0["type"] as? Int == 2 } ?? false
        }
        guard lostFull else { return }
        snapshotSession = ""
        var st = replayState.get()
        st.snapshotSession = ""
        replayState.set(st)
    }

    /// UUIDv7: 48-bit unix milliseconds, then version/variant bits over random.
    private static func uuidV7(_ date: Date) -> String {
        var b = [UInt8](repeating: 0, count: 16)
        let ms = UInt64(date.timeIntervalSince1970 * 1000)
        for i in 0..<6 { b[i] = UInt8((ms >> (8 * UInt64(5 - i))) & 0xFF) }
        for i in 6..<16 { b[i] = UInt8.random(in: 0...255) }
        b[6] = (b[6] & 0x0F) | 0x70
        b[8] = (b[8] & 0x3F) | 0x80
        let hex = b.map { String(format: "%02x", $0) }.joined()
        var s = hex
        for offset in [8, 13, 18, 23] {
            s.insert("-", at: s.index(s.startIndex, offsetBy: offset))
        }
        return s
    }
}

/// One lock-guarded value, readable from any thread.
final class LockedValue<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: T
    init(_ value: T) { self.value = value }
    func get() -> T { lock.withLock { value } }
    func set(_ v: T) { lock.withLock { value = v } }
}

/// Sendability asserted by the owner, not the type: used for JSON-shaped
/// values that only ever cross into the client's one serial queue.
private struct Unchecked<T>: @unchecked Sendable {
    let value: T
    init(_ value: T) { self.value = value }
}
