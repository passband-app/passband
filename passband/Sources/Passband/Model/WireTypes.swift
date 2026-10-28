// Swift mirrors of the squelch-core / squelch-api JSON contracts under
// /client/*. THESE SHAPES ARE A WIRE CONTRACT: field names and enum string
// values match the Rust serde output exactly (rename_all = "snake_case"), so do
// not "improve" them. Every enum decodes leniently (an unknown wire value falls
// back) so a newer daemon can add a variant without bricking an older client.

import Foundation

// MARK: - lenient enum helper

/// A String-backed enum that never fails to decode: an unrecognised wire value
/// becomes `unknownFallback`. Without this, one new server-side tier value
/// would fail the whole page decode.
protocol LenientRawEnum: RawRepresentable, Codable, Sendable, Hashable
where RawValue == String {
    static var unknownFallback: Self { get }
}

extension LenientRawEnum {
    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? Self.unknownFallback
    }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(rawValue)
    }
}

// MARK: - lenient optional field

/// An OPTIONAL FIELD THAT CAN NEVER FAIL ITS PARENT: absent, null, or a value
/// of the wrong shape all decode as nil. For a field the rest of the message
/// does not need, on a type something else must not lose over it (the
/// notification extension decodes `ClientMessage` too, and a message that
/// fails to decode is a push that never shows). Encodes as an absent key when
/// nil, so the round trip through a cache is the wire's own shape.
@propertyWrapper
struct LenientOptional<T: Codable & Sendable & Hashable>: Codable, Sendable, Hashable {
    var wrappedValue: T?
    init(wrappedValue: T?) { self.wrappedValue = wrappedValue }
    init(from decoder: Decoder) throws {
        wrappedValue = try? decoder.singleValueContainer().decode(T.self)
    }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        if let wrappedValue { try c.encode(wrappedValue) } else { try c.encodeNil() }
    }
}

extension KeyedDecodingContainer {
    /// The synthesized decode asks for the wrapper with `decode`, which throws
    /// on a missing key; this overload is what makes an absent key nil.
    func decode<T>(_ type: LenientOptional<T>.Type, forKey key: Key) throws -> LenientOptional<T> {
        (try? decodeIfPresent(LenientOptional<T>.self, forKey: key)) ?? LenientOptional(wrappedValue: nil)
    }
}

extension KeyedEncodingContainer {
    /// And nil is left OFF the wire rather than written as null.
    mutating func encode<T>(_ value: LenientOptional<T>, forKey key: Key) throws {
        guard let wrapped = value.wrappedValue else { return }
        try encode(wrapped, forKey: key)
    }
}

// MARK: - sender pair

/// A wire type carrying the `from_name` / `from_addr` pair, so the one true
/// rendering of a sender lives in a single place.
protocol SenderStringConvertible {
    var from_addr: String { get }
    var from_name: String? { get }
}

extension SenderStringConvertible {
    /// "Name <addr>" when a display name exists, else the bare address — the
    /// shape `SenderID.parse` expects.
    var senderString: String {
        if let n = from_name, !n.isEmpty { return "\(n) <\(from_addr)>" }
        return from_addr
    }
}

// MARK: - core enums

enum Tier: String, LenientRawEnum {
    case pastDue = "past_due"
    case deadline
    case signal
    case noise
    static var unknownFallback: Tier { .signal }

    /// Display label used by chips/debug rows.
    var label: String { rawValue }
}

enum AttentionStatus: String, LenientRawEnum {
    case new, open, done
    static var unknownFallback: AttentionStatus { .new }
}

enum Disposition: String, LenientRawEnum, CaseIterable {
    case surface, squelch, filtered
    static var unknownFallback: Disposition { .squelch }

    /// User-facing label. The wire values are the daemon's, not UI verbs — the
    /// chips read allow/mute/filter while `surface`/`squelch` stay on the wire.
    var label: String {
        switch self {
        case .surface: "allow"
        case .squelch: "mute"
        case .filtered: "filter"
        }
    }

    var hint: String {
        switch self {
        case .surface: "allow: always surface this sender, never mute them"
        case .squelch: "mute: keep out of the sitrep unless it escalates"
        case .filtered: "filter: a plain english line decides what gets through, in either direction"
        }
    }
}

/// Server-side sitrep bucket (query param `band` on /client/updates).
enum Band: String, Sendable, CaseIterable {
    case standing, new, open
}

// MARK: - notification events

/// core::types::EventKind — why an event was emitted. Server-side precedence
/// when classifying a verdict is urgent > deadline > surfaced.
enum EventKind: String, LenientRawEnum {
    /// Tier is past_due/deadline — the dated-obligation tiers, immune to
    /// thresholds. A subset of the standing band, which also holds live
    /// correspondence; a row in the band on that footing earns no event here.
    case urgent
    /// A deadline on a message that is not itself urgent-tier.
    case deadline
    /// Importance landed at or above the notify threshold.
    case surfaced
    /// Somebody opened the user's OWN tracked outbound mail. Not a triage
    /// verdict at all: `sender` is the account's own address, `one_line` is the
    /// sent subject, and `importance` is a fixed placeholder rather than a
    /// score. One per message ever, however many times it is reopened.
    case opened
    /// An unheard-of kind still earns a banner; it only loses urgent styling.
    static var unknownFallback: EventKind { .surfaced }
}

/// core::types::Event — one row of the durable notification log, delivered as
/// SSE frames on GET /client/events. Every field is a snapshot taken at
/// emission time, so a client renders the whole notification from this row with
/// no second round trip.
///
/// SEALED MAIL DOES APPEAR HERE, as of the notify lane (docs/NOTIFY.md §11.6) —
/// it used to be the one category that could not, which is why auth mail had to
/// be polled for. What is still true, and is the invariant that made the change
/// safe at all, is that a sealed row carries NO BODY-DERIVED TEXT: `one_line` is
/// a fixed phrase generated from `sealed_kind` alone ("Login code arrived"),
/// never the subject and never the code. See docs/SECURITY.md §4.
struct Event: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var kind: EventKind
    var message_id: Int
    var thread_id: String
    var tier: Tier
    var importance: Int
    var sender: String
    var one_line: String
    /// Snapshotted RFC3339 text, or nil. Display copy only.
    var deadline: String?
    var created_at: String
    /// Why the message was sealed, when it was. ABSENT on every ordinary event
    /// and on every row a pre-notify-lane daemon serves — which is why it is
    /// optional rather than merely nullable, and why nothing else about this
    /// type moved: the synthesized decoder must go on accepting a frame that
    /// has never heard of the key.
    ///
    /// NOT display copy and not a body: it is the ROUTING signal. Non-nil means
    /// this event is the fast word that auth mail has landed, and the client
    /// answers it by going and asking `/client/sealed` — see
    /// `EventBanner.routing(for:)`.
    var sealed_kind: SealedKind?
    var is_auth: Bool?
    var isAuth: Bool { is_auth ?? false }
}

// MARK: - updates

/// Per-property triage justifications. An absent key means no reason was
/// recorded for that property.
struct FieldReasons: Codable, Sendable, Hashable {
    var importance: String?
    var deadline: String?
    var tier: String?
}

/// core::types::AttentionUpdate. Rust `#[serde(flatten)]`s `Update` in, so the
/// wire shape is ONE flat object. `surfaced_at == nil` => NEW.
struct AttentionUpdate: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var thread_id: String
    var tier: Tier
    var importance: Int
    var sender: String
    var one_line: String
    var reason: String
    var deadline: String?
    var deadline_date: String?
    var displayDeadline: String? { deadline ?? deadline_date }
    var matched_rule: Int?
    var field_reasons: FieldReasons?
    /// ABSENT on rows served by a pre-attachment daemon — treat as false.
    var has_attachments: Bool?
    /// The `From:` display name. ABSENT on rows served by an older daemon, and
    /// on the agent door, which is why every read goes through `senderString`
    /// rather than this directly.
    var from_name: String?
    /// The mail's own `Subject` and the opening of its body. The inventory the
    /// mail pages read serves both on every row; the legacy band listing serves
    /// them on the spam page only, where `one_line` is empty and these are what
    /// the row says instead. So: present on the mail pages, absent on the bands.
    ///
    /// UNTRUSTED, and more pointedly than `from_name`: this is text out of the
    /// corpus written to talk readers into things. It renders as words on a
    /// row, is never handed to a model, and never reaches the agent door.
    var subject: String?
    var preview: String?
    var status: AttentionStatus
    /// When the mail arrived. ABSENT from an older daemon, which is why every
    /// read goes through `arrivedAt` and falls back to `surfaced_at`.
    var received_at: String? = nil
    /// The sitrep zone this mail lands in (`SitrepZone`). Absent from an older
    /// daemon and from rows triage has not decided yet.
    var placement: String? = nil
    var surfaced_at: String?
    var resolved_at: String?
    /// A PENDING reminder: when the daemon will pull this back into the bands.
    /// Non-nil implies the row is done — setting a reminder resolves the thread
    /// — so a row carrying one is not "unfinished", it is parked. HUMAN DOOR
    /// ONLY: the agent door never serves either of these fields, which is why
    /// both are optional rather than merely nullable.
    var remind_at: String?
    /// A reminder that FIRED, cleared the next time the row is resolved. It is
    /// what re-enters the standing band regardless of tier: a reminder is the
    /// user declaring the mail owed attention, and that outranks a verdict.
    var reminded_at: String?
    /// The auth kinds triage gave this message (`decision.auth.kinds` on the v2
    /// feed row, raw strings). Set only by the feed adapter (`AgentFeedItem.row`);
    /// nil on every other listing. Non-empty means auth mail, which a For-your-
    /// eyes row shows sealed and opens in the reveal (Lib/AuthPolicy.swift).
    var auth_kinds: [String]? = nil
    /// The decision's own `auth.critical`, beside `auth_kinds`.
    var auth_critical: Bool? = nil

    var hasAttachments: Bool { has_attachments ?? false }
    /// Auth mail by the row's own decision.
    var isAuthRow: Bool { !(auth_kinds ?? []).isEmpty }

    /// The stamp a mail list dates and sorts this row by: arrival, or first
    /// listing when the daemon is too old to say when it arrived.
    var arrivedAt: String? { received_at ?? surfaced_at }

    /// Whether a reminder is waiting to fire on this row.
    var hasPendingReminder: Bool { !(remind_at ?? "").isEmpty }

    /// WHAT THE MIDDLE OF THE ROW SAYS when nothing triaged the mail: the
    /// subject, then as much of the body's opening as fits. nil whenever there
    /// IS a summary, so a row falls back to the mail's own words only where
    /// there is nothing better — which today is the spam page and only the spam
    /// page.
    ///
    /// The subject is blank-aware because a cleared `Subject` header is real
    /// mail with a real body, and "(no subject)" beside its opening line is a
    /// readable row where an empty one looks like a rendering bug. The preview
    /// gets no such filler: a body that opens with nothing simply says nothing.
    ///
    /// Both are flattened here rather than at ingest. A body's own newlines are
    /// its formatting, and a snippet that keeps them turns a one-line row into
    /// a paragraph shaped by whoever sent it.
    ///
    /// The caps are a GUARD, not a layout decision — deliberately past what any
    /// row can show, so the visible cut is the one the row's own width makes.
    /// A cap tight enough to bite would put an ellipsis in the middle of a row
    /// with space left on it. What they are actually for is the subject with no
    /// upper bound at all: the daemon caps the snippet at 200 characters and
    /// caps a `Subject` header at nothing.
    var mailText: (subject: String, preview: String)? {
        guard one_line.isEmpty, subject != nil || preview != nil else { return nil }
        return (
            (subject ?? "").displaySubject.flattenedLine(cap: 300),
            (preview ?? "").flattenedLine(cap: 300)
        )
    }
}

/// The wire calls the address `sender` here and `from_addr` everywhere else, so
/// this is the one place that reconciles the two. With it, an update renders its
/// sender through exactly the same path as a message, a receipt or a search hit.
extension AttentionUpdate: SenderStringConvertible {
    var from_addr: String { sender }
}

// MARK: - thread / messages

/// core::types::Attachment. `downloadable` is false when the raw bytes were NOT
/// stored (over the ingest cap): metadata exists but the bytes route 410s.
struct Attachment: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var filename: String
    var mime: String
    var size: Int
    var downloadable: Bool
    /// The part's Content-ID with its angle brackets already off, which is what
    /// a body's `<img src="cid:…">` names — see CidImages. ABSENT on a daemon
    /// that predates the field and null for a part that declared none, so it
    /// stays optional: the client's answer to both is the same, which is to drop
    /// the reference rather than paint a broken image.
    var content_id: String?
}

/// core::types::ClientMessage — the HUMAN-door message shape. `html` is a
/// server-side-sanitized (ammonia) string, or nil for plain-text-only mail.
struct ClientMessage: Codable, Sendable, Identifiable, Hashable, SenderStringConvertible {
    var id: Int
    var from_addr: String
    var from_name: String?
    var received_at: String
    var content: String
    var html: String?
    /// THIS MESSAGE'S OWN subject header, not the thread's. ABSENT on a daemon
    /// too old to send it, which is why the reader falls back to
    /// `ClientThreadView.subject` rather than showing nothing.
    ///
    /// They differ more often than a thread view suggests: a reply that retitled
    /// the conversation, a forward pulled back in, a list that stamps its own
    /// prefix. `f` forwards THE SELECTED MESSAGE, so the chrome and the outgoing
    /// subject have to name the message rather than the conversation it landed
    /// in — the daemon's own fallback quotes this same header.
    var subject: String?
    /// WHO ELSE WAS ON THIS MESSAGE, one field per header, each a comma-joined
    /// `Name <addr>` list exactly as the header spelled it. ABSENT on a daemon
    /// too old to send them, and absent on a row whose headers have never been
    /// read (the daemon's one-shot recipients sweep is still walking back
    /// through the mailbox) — both draw nothing, which is what the reader did
    /// before these existed.
    ///
    /// `""` is the DIFFERENT statement that the header was read and named
    /// nobody. It also draws nothing; the distinction is the daemon's, and the
    /// client is careful not to collapse the two on its way past.
    ///
    /// `bcc_addrs` only ever arrives on mail this account sent. The daemon
    /// refuses to keep a `Bcc:` header on received mail, where it is the
    /// sender's leak or a forgery rather than who the mail went to.
    var to_addrs: String?
    var cc_addrs: String?
    var bcc_addrs: String?
    var attachments: [Attachment]?
    /// True for a message the USER authored, false for anything received.
    /// ABSENT on an older daemon, which reads as unknown — and unknown is
    /// drawn as theirs (see `fromMe`), which is where every message sat before
    /// the chat style existed, and must not make the reader treat a reply as
    /// somebody else's mail. Aligns the chat bubbles, and picks which message
    /// in a thread a reminder lands on: see ThreadViewer's `h`.
    var is_sent: Bool?
    /// THE PROVIDER'S SPAM VERDICT on this message. ABSENT on a daemon too old
    /// to send it, and nil reads as false — a client that could not tell would
    /// rather say nothing than accuse a message.
    ///
    /// The reader needs it because a thread opened from the spam page looks
    /// exactly like any other thread once it is on screen: same chrome, same
    /// bubbles, no tier, no one-liner. This is the only thing that can tell the
    /// reader the mail in front of them was filtered rather than delivered.
    var is_spam: Bool?
    /// This message's OWN triage verdict — ABSENT on a pre-highlight daemon.
    /// Drives the in-thread attention highlight: the bands show one row per
    /// thread, so the reader is where "which message is the reason" is answered.
    var tier: Tier?
    var deadline: String?
    /// Whether the attention row is still unresolved; a resolved obligation
    /// must not keep glowing.
    var attention_open: Bool?
    var one_line: String?
    /// True when `from_addr` is somebody the user has sent mail to (the daemon's
    /// Sent-derived contacts, computed per request). ABSENT on a pre-tracking
    /// daemon, which reads as unknown — the strict side. Governs the reader's
    /// tracker strip: see `allowsTrackers`.
    var sender_known: Bool?
    /// THIS MESSAGE IN ENGLISH, when it was written in another language (see
    /// docs/TRANSLATION.md). ABSENT on a daemon too old to translate, on mail
    /// that is already English, and on a translation that failed, and all
    /// three read the same way: the original, with nothing said about it.
    /// LENIENT so the notification extension, which decodes this type too,
    /// never fails a push over a field it does not draw: a value of the wrong
    /// shape (a bare string, an array) reads as no translation at all, rather
    /// than failing the whole message.
    @LenientOptional var translation: ClientTranslation? = nil

    var attachmentList: [Attachment] { attachments ?? [] }

    /// The translation the reader may show INSTEAD of the original: one that
    /// has finished and actually carries a body. A pending or deferred one
    /// shows the original under a "translating" note, so it is not this.
    var readyTranslation: ClientTranslation? {
        guard let translation, translation.ready else { return nil }
        return translation
    }

    /// THE MESSAGE AS THE READER DRAWS IT. The translated subject and body
    /// swapped in, or the message untouched when there is no ready translation
    /// or the reader asked for the original. Everything that ACTS on mail
    /// (reply, forward, quoting) keeps the original: this is for drawing, and
    /// for sizing what is drawn.
    ///
    /// A translation without html is drawn as plain text even when the
    /// original had html: the translated text is the thing to read, and
    /// showing the untranslated document around it would be showing the
    /// original.
    func presented(original: Bool = false) -> ClientMessage {
        guard !original, let t = readyTranslation else { return self }
        var shown = self
        if !t.subject.isEmpty { shown.subject = t.subject }
        if let html = t.html, !html.isEmpty {
            shown.html = html
            if !t.content.isEmpty { shown.content = t.content }
        } else {
            shown.html = nil
            shown.content = t.content
        }
        return shown
    }

    /// The side of the conversation this message is on, for the chat style.
    /// `nil` (old daemon) is THEIRS: an unknown side drawn as the user's own
    /// would right-align somebody else's mail under their name.
    var fromMe: Bool { is_sent ?? false }

    /// Whether this message's tracking pixels may load. Trusted people are
    /// allowed to learn their mail was opened; everyone else is stripped as
    /// before. `nil` (old daemon) is NOT allowed — the default stays private.
    var allowsTrackers: Bool { sender_known ?? false }

    /// The highlight predicate: an UNRESOLVED row in a dated-obligation tier.
    /// Deliberately NARROWER than the for-your-eyes band, which also carries
    /// live correspondence: the mark answers "which message is the obligation",
    /// and a mark on every message from a known contact answers nothing.
    var needsAttention: Bool {
        (attention_open ?? false) && (tier == .pastDue || tier == .deadline)
    }
}

/// core::types::ClientTranslation: one message's English rendering. Decoded
/// LENIENTLY, field by field: a missing or mistyped field takes its empty
/// value rather than failing the message it rides on, because a thread that
/// will not decode is a thread that will not open, and the translation is the
/// optional half of it. An unknown status reads as `done`, which shows only
/// when it carries a body (`ready`).
struct ClientTranslation: Codable, Sendable, Hashable {
    enum Status: String, LenientRawEnum {
        /// ARCHIVE MAIL: a translation is available and nothing has been
        /// spent on it. Reading the thread does not buy it (the prefetch reads
        /// threads nobody opened); the reader DISPLAYING the thread asks, over
        /// `APIClient.translateThread`, and then it is `pending`.
        case deferred
        case pending, done, incomplete
        static var unknownFallback: Status { .done }
    }

    /// ISO code, e.g. "fr".
    var source_lang: String
    /// English name, e.g. "French". What the reader says.
    var source_language: String
    var status: Status
    var subject: String
    var content: String
    var html: String?
    /// What the daemon's fact check found in the original and not in the
    /// translation (numbers, codes, links). Only on `incomplete`.
    var missing_facts: [String]?

    init(
        source_lang: String, source_language: String, status: Status, subject: String,
        content: String, html: String? = nil, missing_facts: [String]? = nil
    ) {
        self.source_lang = source_lang
        self.source_language = source_language
        self.status = status
        self.subject = subject
        self.content = content
        self.html = html
        self.missing_facts = missing_facts
    }

    private enum CodingKeys: String, CodingKey {
        case source_lang, source_language, status, subject, content, html, missing_facts
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func text(_ key: CodingKeys) -> String {
            ((try? c.decodeIfPresent(String.self, forKey: key)) ?? nil) ?? ""
        }
        source_lang = text(.source_lang)
        source_language = text(.source_language)
        status = ((try? c.decodeIfPresent(Status.self, forKey: .status)) ?? nil) ?? .done
        subject = text(.subject)
        content = text(.content)
        html = (try? c.decodeIfPresent(String.self, forKey: .html)) ?? nil
        missing_facts = (try? c.decodeIfPresent([String].self, forKey: .missing_facts)) ?? nil
    }

    /// The language as the reader names it: the English name, else the code,
    /// else a phrase that is still a sentence.
    var languageName: String {
        let name = source_language.trimmingCharacters(in: .whitespaces)
        if !name.isEmpty { return name }
        let code = source_lang.trimmingCharacters(in: .whitespaces)
        return code.isEmpty ? "another language" : code.uppercased()
    }

    /// Finished, and with something to show. A `done` that arrived with no
    /// body at all is treated as no translation rather than a blank message.
    var ready: Bool {
        (status == .done || status == .incomplete)
            && (!(html ?? "").isEmpty || !content.isEmpty)
    }

    /// Not here yet, and coming: the reader shows the original and says it is
    /// translating. A deferred one is drawn the same way, because the reader
    /// that shows it is the one that asks for it.
    var awaiting: Bool { status == .pending || status == .deferred }
}

/// core::types::ClientThreadView (GET /client/thread/{id}).
struct ClientThreadView: Codable, Sendable, Hashable {
    /// Reusable body caches require an explicit current server assessment.
    /// Pending or restricted threads remain readable in the active reader.
    var cache_allowed: Bool?
    var thread_id: String
    var subject: String
    var messages: [ClientMessage]
}

extension ClientThreadView {
    /// THIS THREAD WITH `fresh`'S TRANSLATIONS LAID OVER IT, and nothing else
    /// changed: what the reader's translation poll adopts, so a refetch made
    /// only to see whether the English has landed cannot move, re-order or
    /// re-render anything else under somebody reading. A message `fresh` does
    /// not carry keeps what it had; one `fresh` carries with NO translation
    /// (it failed, and the daemon leaves failures off the wire) loses it.
    /// nil when not one translation changed, so the caller can skip the write.
    func mergingTranslations(from fresh: ClientThreadView) -> ClientThreadView? {
        var incoming: [Int: ClientTranslation?] = [:]
        for m in fresh.messages where incoming[m.id] == nil { incoming[m.id] = .some(m.translation) }
        var merged = self
        var changed = false
        for i in merged.messages.indices {
            guard let t = incoming[merged.messages[i].id], t != merged.messages[i].translation
            else { continue }
            merged.messages[i].translation = t
            changed = true
        }
        return changed ? merged : nil
    }

    /// Same messages, same order: the only case the poll may merge into. Any
    /// other difference is an arrival, which the in-place refresh owns.
    func sameMessages(as other: ClientThreadView) -> Bool {
        messages.map(\.id) == other.messages.map(\.id)
    }

    /// How many messages have a translation READY to show. Part of the
    /// reader's measuring-pass identity: a translation landing is a new
    /// document to size under a new key.
    var readyTranslations: Int { messages.reduce(0) { $0 + ($1.readyTranslation == nil ? 0 : 1) } }
}

// MARK: - shipments

enum Carrier: String, LenientRawEnum {
    case ups, usps, fedex, dhl, amazon, unknown
    static var unknownFallback: Carrier { .unknown }

    var label: String {
        switch self {
        case .unknown: "carrier"
        case .ups, .usps, .dhl: rawValue.uppercased()
        case .fedex: "FedEx"
        case .amazon: "Amazon"
        }
    }

    /// Domain whose favicon stands in for the carrier; nil falls back to a glyph.
    var faviconDomain: String? {
        switch self {
        case .ups: "ups.com"
        case .usps: "usps.com"
        case .fedex: "fedex.com"
        case .dhl: "dhl.com"
        case .amazon, .unknown: nil
        }
    }
}

enum ShipmentStatus: String, LenientRawEnum {
    case ordered
    case shipped
    case outForDelivery = "out_for_delivery"
    case delivered
    case exception
    static var unknownFallback: ShipmentStatus { .ordered }

    var label: String {
        switch self {
        case .ordered: "ordered"
        case .shipped: "shipped"
        case .outForDelivery: "out for delivery"
        case .delivered: "delivered"
        case .exception: "exception"
        }
    }
}

struct Shipment: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var account_id: Int
    var tracking_number: String
    var carrier: Carrier
    var item_name: String
    var status: ShipmentStatus
    var tracking_url: String?
    /// Thread of the latest email whose status stuck; absent from an older
    /// daemon, and then the card simply isn't clickable.
    var thread_id: String?
    var first_seen: String
    var last_update: String
    /// THE CARRIER-POLL FIELDS. Every one of them is absent from an older
    /// daemon's rows, so every one is optional and every reader must render the
    /// pre-poll card when it is nil.
    ///
    /// Carrier-estimated delivery; nil when the carrier gives none. A POLL is
    /// the only thing that ever sets it — no email carries one.
    var eta: String?
    /// The carrier's own status words, verbatim, including the ones our five-rung
    /// `status` could not express. Nil until the first poll. SOMEBODY ELSE'S
    /// TEXT: flatten and cap it before drawing it.
    var carrier_status_raw: String?
    /// When it landed, from whichever path saw it first; nil until delivered.
    var delivered_at: String?
    /// Last carrier poll ATTEMPT; nil = never asked.
    var last_polled_at: String?
    /// Consecutive polls the carrier answered with "no record of this number".
    /// Rate limits and transport errors never count, which is what makes a
    /// nonzero value evidence about the NUMBER rather than about the network.
    var poll_failures: Int?
    /// THE ORDER FIELDS, absent from a daemon older than order linking.
    ///
    /// Who sold it, as the customer knows the store. Never the carrier.
    var merchant: String?
    /// Every order this card carries, deduped by the daemon.
    var orders: [ShipmentOrder]?
    /// The OTHER tracking numbers the daemon folded into this card because they
    /// share an order. Empty (or absent) for a lone package.
    var legs: [ShipmentLeg]?

    /// Is any package on this card still on its way? The representative, or
    /// any leg. A daemon picks an undelivered representative whenever there is
    /// one, so the legs are belt and braces against one that does not.
    var anyUndelivered: Bool {
        status != .delivered || (legs ?? []).contains { $0.status != .delivered }
    }

    /// Does the card stay on the shipments rail? While ANY of its packages is
    /// still coming, yes; once all have landed, only on the day the
    /// representative did (`isToday` judges the timestamp, so a test can pin
    /// the clock).
    func staysOnRail(isToday: (String?) -> Bool) -> Bool {
        anyUndelivered || isToday(delivered_at ?? last_update)
    }

    /// The item name as a LABEL: emoji dropped, whitespace collapsed. The
    /// daemon lifts this out of a subject line and its strip leaves pictographs
    /// standing, so "🚚 Your order from X" reaches us with the truck attached.
    /// Empty when nothing survives (a name that was only decoration), which is
    /// each caller's cue to use its own fallback.
    var displayItem: String { item_name.withoutEmoji }

    /// The merchant as a label, same strip as the item name. Empty when absent.
    var displayMerchant: String { (merchant ?? "").withoutEmoji }

    /// The card's name: what is in the box, else who sold it, else "Package".
    /// Never the carrier: the carrier is already the badge beside it.
    var displayTitle: String {
        let item = displayItem
        if !item.isEmpty { return item }
        let store = displayMerchant
        return store.isEmpty ? "Package" : store
    }

    /// Order refs as printed on the card, each with ONE leading "#".
    var displayOrderRefs: [String] {
        (orders ?? []).compactMap {
            var ref = $0.order_ref.withoutEmoji
            while ref.hasPrefix("#") { ref.removeFirst() }
            ref = ref.trimmingCharacters(in: .whitespaces)
            return ref.isEmpty ? nil : "#\(ref)"
        }
    }

    /// The quiet line under the title: merchant, order refs, and a box count
    /// when the daemon merged several. The merchant is left out when it is
    /// already the title. nil when there is nothing to say.
    var orderLine: String? {
        var parts: [String] = []
        let store = displayMerchant
        if !store.isEmpty, store != displayTitle { parts.append(store) }
        let refs = displayOrderRefs
        if !refs.isEmpty { parts.append(refs.joined(separator: ", ")) }
        let boxes = (legs ?? []).count + 1
        if boxes > 1 { parts.append("\(boxes) packages") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// One order a shipment card carries (core::types::ShipmentOrder).
struct ShipmentOrder: Codable, Sendable, Hashable {
    var merchant: String?
    var order_ref: String
}

/// Another tracking number merged into a card (core::types::ShipmentLeg).
/// Carrier and status decode leniently, like `Shipment`'s own.
struct ShipmentLeg: Codable, Sendable, Hashable, Identifiable {
    var id: Int
    var carrier: Carrier
    var tracking_number: String
    var status: ShipmentStatus
    var tracking_url: String?
    var last_update: String
    var delivered_at: String?
}

// MARK: - receipts / calendar / banking

struct Receipt: Codable, Sendable, Identifiable, Hashable, SenderStringConvertible {
    var id: Int
    var account_id: Int
    var message_id: Int
    var thread_id: String?
    var from_addr: String
    var from_name: String?
    var amount: Double?
    var currency: String?
    var received_at: String
}

enum CalendarKind: String, LenientRawEnum {
    case invite, update, cancellation, response, reservation, event
    static var unknownFallback: CalendarKind { .invite }

    /// Row tag for the non-default kinds; a plain invite needs no label.
    var tag: String? {
        switch self {
        case .invite, .event: nil
        case .update: "updated"
        case .cancellation: "canceled"
        case .response: "rsvp"
        case .reservation: "reservation"
        }
    }
}

struct CalendarUpdate: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var message_id: Int
    var thread_id: String?
    var kind: CalendarKind
    var event_title: String?
    var starts_at: String?
    var organizer: String?
    var received_at: String
    var start_timezone: String? = nil
}

enum BankingKind: String, LenientRawEnum {
    case statement, update, bill
    case transactionAlert = "transaction_alert"
    case autopay
    static var unknownFallback: BankingKind { .statement }

    var tag: String {
        switch self {
        case .bill: "bill"
        case .statement: "statement"
        case .update: "update"
        case .transactionAlert: "alert"
        case .autopay: "autopay"
        }
    }
}

struct BankingRecord: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var message_id: Int
    var thread_id: String?
    var from_addr: String?
    var kind: BankingKind
    var institution: String?
    var amount: Double?
    var currency: String?
    var account_hint: String?
    var received_at: String
    var due: String? = nil
    var autopay: Bool? = nil
}

// MARK: - send groups

/// How a group addresses its members. A property of the AUDIENCE, chosen when
/// the group is made rather than per message: an investor list is
/// individually-addressed every time or it is not one.
enum GroupMode: String, Codable, Sendable, CaseIterable, Hashable {
    /// One message, everyone in To. They see each other.
    case to
    /// One message, everyone in Bcc. They do not.
    case bcc
    /// One message PER member. Replies come back as private threads and read
    /// receipts are per recipient.
    case individual

    var label: String {
        switch self {
        case .to: "To"
        case .bcc: "Bcc"
        case .individual: "Individually"
        }
    }

    /// What picking this mode does to the composer, said in the one line the
    /// picker has room for. No em dashes: user-facing copy.
    var blurb: String {
        switch self {
        case .to: "everyone sees the whole list"
        case .bcc: "nobody sees anyone else"
        case .individual: "one separate email each"
        }
    }

    var symbol: String {
        switch self {
        case .to: "person.2"
        case .bcc: "eye.slash"
        case .individual: "arrow.triangle.branch"
        }
    }
}

/// One mailbox in a group.
struct GroupMember: Codable, Sendable, Hashable, Identifiable {
    var addr: String
    var display_name: String?

    var id: String { addr }

    /// What a pill or a member row shows: the name when the daemon has one,
    /// the address otherwise.
    var label: String {
        guard let name = display_name, !name.isEmpty else { return addr }
        return name
    }
}

/// GET /client/groups. `members` is EMPTY on the list read and populated on the
/// single-group read, so a sidebar listing is never a contacts dump.
struct SendGroup: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var name: String
    var slug: String
    var mode: GroupMode
    var note: String
    var member_count: Int
    var members: [GroupMember]?
    var last_sent_at: String?
    var created_at: String
    var updated_at: String
}

struct GroupBody: Codable, Sendable {
    var name: String
    var mode: GroupMode
    var note: String
    var members: [GroupMember]
}

/// GET /client/groups/{id}/history — one entry, from either source. A RECORDED
/// send names `group_send_id`; a DERIVED one (mail matched against the current
/// membership, which is what lets a new group show the year that preceded it)
/// names only the message.
struct GroupHistoryEntry: Codable, Sendable, Identifiable, Hashable {
    var group_send_id: Int?
    var message_id: Int?
    var thread_id: String?
    var subject: String
    var snippet: String
    var sent_at: String
    var mode: GroupMode
    /// How many of the group this reached.
    var reached: Int
    /// The denominator: the snapshot taken at send time for a recorded entry,
    /// the CURRENT membership for a derived one.
    var group_size: Int
    /// Recipients a fan-out never reached. Always 0 for a derived entry.
    var failed: Int
    var opens: Int

    /// Stable across the two sources: a recorded entry is keyed by its send, a
    /// derived one by its message. Both can be absent in principle, so the date
    /// backs them up.
    var id: String {
        if let sendId = group_send_id { return "s\(sendId)" }
        if let messageId = message_id { return "m\(messageId)" }
        return "t\(sent_at)"
    }

    /// True when this went to every member it was measured against.
    var reachedEveryone: Bool { group_size > 0 && reached >= group_size }

    /// Was this mail SENT THROUGH the group, or merely matched to it afterwards?
    ///
    /// The distinction decides what the reach count MEANS, and getting it wrong
    /// libels the mailbox. A recorded send aimed at everyone, so "1 of 2" is a
    /// shortfall and worth saying loudly. A derived entry is ordinary mail the
    /// daemon matched to this membership after the fact — it never aimed at the
    /// group, so "1 of 2" describes nothing that went wrong, and rendering it in
    /// the shortfall's own styling turns a year of normal correspondence into a
    /// page of apparent delivery failures.
    var isRecorded: Bool { group_send_id != nil }
}

struct GroupHistoryPage: Codable, Sendable {
    var items: [GroupHistoryEntry]
    var next_offset: Int?
}

struct GroupLimits: Codable, Sendable {
    var max_members: Int
}

// MARK: - rules

struct SenderRule: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var account_id: Int
    var match_pattern: String
    var want_text: String
    var disposition: Disposition
    var updated_at: String
}

struct CreateRuleBody: Codable, Sendable {
    var match_pattern: String
    var want: String
    /// ABSENT asks the daemon to infer the outcome from `want` — the owner's own
    /// sentence, and nothing else, ever. Encodes as absent when nil, the same way
    /// `source_message_id` does. A caller that means one specific outcome (the
    /// block flow, an undo restoring a deleted rule, an explicit override in the
    /// editor's advanced section) still names it, and a named value wins.
    var disposition: Disposition?
    /// The message the block was invoked FROM, when there is one on screen: a
    /// squelch rule resolves it server-side (blocking is that email's
    /// disposition). Encodes as absent when nil, so the rule editor's plain
    /// creates are unchanged.
    var source_message_id: Int? = nil
    /// Asks the daemon to ALSO resolve every open message already on file from
    /// this sender, not just the one the rule was created from. Encodes as
    /// absent when nil and the server reads absent as false, so only a caller
    /// that means "clear this sender out of my inbox" sends it.
    ///
    /// Deliberately separate from an explicit `.squelch`: stating the outcome
    /// says what the rule IS, this says what to do about mail that already
    /// arrived. An undo recreating a deleted mute rule needs the first without
    /// the second, so it omits this and nothing gets swept.
    var sweep: Bool? = nil
}

/// `GET /client/rules/suggest`. Null when the daemon has no model, too little
/// history, or no clear pattern.
struct RuleSuggestion: Codable, Sendable {
    var suggestion: String?
}

/// `GET /client/unsubscribes/smart_block`. Null when the sender sends nothing
/// worth keeping apart, or the daemon could not say.
struct SmartBlockAnswer: Codable, Sendable {
    var smart_block: SmartBlockProposal?
}

/// A filtered rule proposed for a sender who ignored an unsubscribe: `rule` is
/// the sentence that gets saved, `keeps` and `drops` are what it means at a
/// glance. All three are model text about the sender's own subjects, so they
/// are shown as plain Text and saved only on the owner's click.
struct SmartBlockProposal: Codable, Sendable, Hashable {
    var rule: String
    var keeps: String
    var drops: String
}

/// GET /client/v2/fye/headline. Null when the daemon has no model, the band is
/// empty, or the model had nothing to say.
struct FyeHeadline: Codable, Sendable {
    var headline: String?
    /// False when the daemon's call FAILED (timeout, provider error) rather
    /// than the model answering: worth asking again. Absent on the first
    /// daemons to serve the route, which read as settled.
    var settled: Bool?
}

/// POST /client/rules and PUT /client/rules/{id}.
struct CreatedRule: Codable, Sendable {
    var rule_id: Int
    /// What the rule ACTUALLY ended up as, which is the only place the client
    /// can learn the answer when it asked for inference. Optional because a
    /// pre-inference daemon answers with `rule_id` alone: absent means "the
    /// server did not say", never "no disposition".
    var disposition: Disposition?
}

// MARK: - unsubscribe

enum UnsubscribeMethod: String, LenientRawEnum {
    case browser
    case oneClick = "one_click"
    case mailto
    /// An automatic unsubscribe the daemon is still working on, or one that
    /// ended up needing the user.
    case auto
    /// Done by reading the sender's page and, at most, pressing its one button.
    case page
    /// Done in a remote browser the daemon steered through the sender's page:
    /// a preference center switched off, a form filled in.
    case agent
    static var unknownFallback: UnsubscribeMethod { .browser }
}

/// Where an automatic unsubscribe stands. Absent on a legacy row, where the
/// client opened the link itself and there was never a job.
enum UnsubStatus: String, LenientRawEnum {
    case running, done, opened
    case needsYou = "needs_you"
    /// A status this client does not know is shown as nothing at all, which is
    /// what `opened` is: settled, and the user's.
    static var unknownFallback: UnsubStatus { .opened }
}

enum UnsubResolution: String, LenientRawEnum {
    /// `filtered` is a smart block: a filtered rule in place of a mute.
    case blocked, filtered, dismissed
    static var unknownFallback: UnsubResolution { .dismissed }
}

struct UnsubscribeResult: Codable, Sendable {
    var method: String
    var sender: String
    /// The link to open: present on the browser path, absent when the daemon
    /// took the job (`method == "auto"`).
    var url: String?
    var status: UnsubStatus? = nil
    /// Why an automatic request fell back to the browser: `spam` or
    /// `unverified`. Absent otherwise.
    var auto_refused: String? = nil
}

/// `POST /client/unsubscribes/opened`.
struct UnsubscribeRecordAck: Codable, Sendable {
    var sender: String
    var status: UnsubStatus?
}

struct UnsubscribeRecord: Codable, Sendable, Hashable, Identifiable {
    var sender: String
    var requested_at: String
    var method: UnsubscribeMethod
    var violation_count: Int
    var last_violation_at: String?
    var resolution: UnsubResolution?
    // Optional for older daemons, which have no automatic unsubscribe.
    var status: UnsubStatus? = nil
    /// The ORIGINAL link, for `needs_you`.
    var status_url: String? = nil
    /// choose_topics | needs_browser | link_error | unclear, and from the
    /// daemon's browser sign_in | captcha | check_email | your_call, for
    /// `needs_you`. Copy for each is `UnsubscribeTray.why`.
    var status_reason: String? = nil
    var status_at: String? = nil
    /// The smart block the daemon has worked out for this sender, and whether
    /// it has worked one out at all: asked with no proposal is a settled no.
    /// Both absent from a daemon without smart blocks.
    var smart_block: SmartBlockProposal? = nil
    var smart_block_asked: Bool? = nil

    var id: String { sender }
}

// MARK: - reading smart clear

/// `POST /client/v2/reading/smart_clear`: the daemon marked expired and plainly
/// unwanted Reading mail done, and says what it took and why.
struct SmartClearResult: Decodable, Sendable, Hashable {
    /// The handle the undo takes. nil when nothing was cleared, or when this
    /// daemon keeps no batch to undo.
    var batch_id: String?
    var cleared: Int
    var senders: Int
    /// Reason code to count. Codes this build has no line for are still
    /// counted in `cleared`; `SmartClearCopy` folds them into one plain line.
    var by_reason: [String: Int]
    var message_ids: [Int]
    /// Whether the model pass ran: `ok`, or one of the `skipped_` codes.
    var model: String?
    /// With `skipped_failed`, why: `refused`, `timeout`, `discarded` or
    /// `error`. nil otherwise, and on a daemon that does not say.
    var model_detail: String?
    /// One per cleared message, for the full list. nil on a daemon that
    /// does not send them: the list is then not offered, never faked.
    var rows: [Row]?

    /// One cleared message. Everything but the id and the sender is
    /// optional, and `reason` stays a string: a code from a newer daemon
    /// must cost a heading, not the decode.
    struct Row: Decodable, Sendable, Hashable {
        var message_id: Int
        var reason: String?
        var sender: String
        var sender_name: String?
        var subject: String?
        var received_at: String?
    }

    /// A row that cannot be decoded is dropped by itself.
    private struct RowSlot: Decodable {
        var row: Row?
        init(from decoder: Decoder) throws { row = try? Row(from: decoder) }
    }

    private enum CodingKeys: String, CodingKey {
        case batch_id, cleared, senders, by_reason, message_ids, model, model_detail, rows
    }

    init(
        batch_id: String?, cleared: Int, senders: Int, by_reason: [String: Int],
        message_ids: [Int], model: String? = "ok", rows: [Row]? = nil
    ) {
        self.rows = rows
        self.batch_id = batch_id
        self.cleared = cleared
        self.senders = senders
        self.by_reason = by_reason
        self.message_ids = message_ids
        self.model = model
    }

    /// Lenient about everything but the count: a receipt can be drawn from
    /// `cleared` alone, and a daemon a release ahead must not cost the whole
    /// answer over a field this build does not read.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        batch_id = try c.decodeIfPresent(String.self, forKey: .batch_id)
        cleared = try c.decode(Int.self, forKey: .cleared)
        senders = try c.decodeIfPresent(Int.self, forKey: .senders) ?? 0
        by_reason = try c.decodeIfPresent([String: Int].self, forKey: .by_reason) ?? [:]
        message_ids = try c.decodeIfPresent([Int].self, forKey: .message_ids) ?? []
        model = try c.decodeIfPresent(String.self, forKey: .model)
        model_detail = try? c.decodeIfPresent(String.self, forKey: .model_detail)
        rows = (try? c.decodeIfPresent([RowSlot].self, forKey: .rows))?.compactMap(\.row)
    }
}

/// `POST /client/v2/reading/smart_clear/{batch_id}/undo`.
struct SmartClearUndone: Decodable, Sendable, Hashable {
    var reopened: Int
}

// MARK: - search / audit / stats

struct SearchHit: Codable, Sendable, Identifiable, Hashable {
    // Prepared once at ingestion; never decoded from or sent over the wire.
    var displaySnippet: String? = nil
    enum CodingKeys: String, CodingKey {
        case is_done, subject_matches, snippet_matches, id, thread_id
        case from_addr, from_name, subject, received_at, snippet, legs, from_me
    }

    // Optional for compatibility with older daemons: unknown is not unfinished.
    var is_done: Bool? = nil
    var subject_matches: [String]? = nil
    var snippet_matches: [String]? = nil
    var id: Int
    var thread_id: String
    var from_addr: String
    var from_name: String?
    var subject: String
    var received_at: String
    var snippet: String
    /// Which retrieval legs produced this row: `["keyword"]`, `["vector"]`, or
    /// both when both did (docs/SEARCH.md §4.8). OPTIONAL because an older
    /// daemon sends no such field, and a client that required it would fail to
    /// decode every hit from one — the whole search panel dark over a
    /// diagnostic.
    var legs: [String]?
    /// The account wrote this message (its own sent mail). OPTIONAL because
    /// an older daemon sends no such field.
    var from_me: Bool? = nil
}

/// WHAT RETRIEVAL DID WITH THE READER'S WORDS, straight from the daemon: how
/// many messages carry ALL the terms, how many carry ANY, and how common each
/// term is in the mailbox. Facts for the classifier in `SearchIntent`, which
/// otherwise has only the shape of the query to go on.
///
/// Every count is account-scoped and excludes sealed AND spam rows on the
/// daemon side. That is not a nicety: a document frequency that counted sealed
/// mail would be an oracle for what sealed mail contains, one query at a time.
struct SearchDiagnostics: Codable, Sendable, Hashable {
    /// Messages matching ALL the terms. Zero over three or more terms is the
    /// motivating query's own shape: the words are right, no one message uses
    /// them all.
    var strict_hits: Int
    /// Messages matching ANY of them.
    var any_hits: Int
    var terms: [Term]

    /// One term as the daemon ranked it, with the number of messages it
    /// appears in.
    struct Term: Codable, Sendable, Hashable {
        var text: String
        var df: Int
    }
}

/// A page of search hits WITH the daemon's account of the search that produced
/// them. Its own envelope rather than `Page<SearchHit>` because the extra
/// fields are search's alone, and because every one of them is optional: the
/// panel must keep working against a daemon that predates wave 1 and sends
/// none of them (the classifier then has only the query's shape to judge on).
struct SearchPage: Codable, Sendable {
    var items: [SearchHit]
    var next_cursor: String?
    /// Which mode actually ran (`keyword`, `semantic`, `hybrid`), as the daemon
    /// resolved it. A String rather than `SearchMode` so a mode this client has
    /// never heard of decodes instead of failing the page.
    var match_kind: String?
    /// The order the hits were ranked under, echoed back.
    var sort: String?
    var diagnostics: SearchDiagnostics?
    /// The date (`YYYY-MM-DD`) search reaches back to. Sent by the agent
    /// search only, and absent on an empty mailbox or an older daemon.
    var indexed_since: String?
    /// How far back the older-mail archive is meant to reach. Absent when the
    /// archive is off or the daemon predates it.
    var archive: SearchArchive?
}

/// The agent search's account of the older-mail archive. Every field optional,
/// so a daemon that adds or drops one still decodes.
struct SearchArchive: Codable, Sendable {
    /// The date the archive is working back to.
    var target: String?
    /// Whether it has got there.
    var walk_complete: Bool?
    /// The standing rule for older mail, always `checked_on_first_match`.
    var older_mail: String?
}

struct AuditEntry: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var account_id: Int
    var ts: String
    var actor: String
    var action: String
    var target: String?
    var detail: String?
    var target_sender: String?
    var target_subject: String?
}

struct BandCounts: Codable, Sendable, Hashable {
    var standing: Int
    var new: Int
    var open: Int
}

struct Stage2Stats: Codable, Sendable, Hashable {
    var est_cost_usd_today: Double?
}

/// Gmail's own INBOX unread counters, as the daemon last saw them. NOT a
/// passband number: it is what the mailbox says before triage has an opinion,
/// which is the only honest way to say how much mail was waiting.
struct InboxUnread: Codable, Sendable, Hashable {
    var messages: Int
    var threads: Int
}

struct StoreStats: Codable, Sendable, Hashable {
    var tier_counts: [String: Int]
    /// The mailbox this daemon serves. The client's ONLY source for its own
    /// address — nothing else on the human door names the account — and it is
    /// what seeds the greeting's display name on a first run. ABSENT on a
    /// daemon too old to say, so nil means "we do not know who this is", which
    /// is not the same as an anonymous mailbox: nothing is seeded and the
    /// greeting stays bare, exactly as it did before this field existed.
    var account_email: String?
    /// The mailbox owner's given name, from their Google profile. ABSENT when
    /// the daemon does not know it: an older daemon, or a Google grant from
    /// before the name was asked for. Seeds the greeting in place of the
    /// address's local part when it is there.
    var account_given_name: String?
    /// The tag of the profile photo the daemon holds for this account, served
    /// at `/client/account/photo`. ABSENT means there is no photo, and the
    /// account wears its initial. See `AccountPhotos`.
    var account_photo: String?
    var total: Int
    var sealed: Int
    /// How many messages the mail PROVIDER filed as spam — its own count, not a
    /// `tier_counts` entry, because spam is not a tier. ABSENT on a daemon too
    /// old to say, and nil reads as "no spam page to offer" rather than zero:
    /// a client that showed a `0 spam` chip against a daemon that simply cannot
    /// answer would be inventing a fact about the mailbox.
    var spam: Int?
    /// When the daemon last FETCHED the provider's spam folder. Nil means it
    /// never has — which is the normal state, because the sync loop does not
    /// track that folder and only fetches it when the page is opened.
    ///
    /// The page needs this to tell "still fetching" from "fetched, and there is
    /// nothing in it": both are an empty list on the wire, and telling somebody
    /// their provider filtered nothing when nobody has looked is a lie the UI
    /// would have no way to take back.
    var spam_synced_at: String?
    var last_history_id: Int?
    var bands: BandCounts
    var last_surfaced_at: String?
    var stage2: Stage2Stats?
    /// ABSENT on a daemon too old to fetch it, and absent for as long as the
    /// first fetch has not landed — so nil means "we do not know", never zero.
    /// Callers must have copy for both.
    var inbox_unread: InboxUnread?
    /// Whether this daemon can relay ⌘K assistant calls to a hosted gateway
    /// (POST /client/assistant/messages). ABSENT on a daemon too old to say —
    /// and nil reads exactly like false: BYOK is the only assistant door.
    var assistant_relay: Bool?
    /// Whether this daemon can mint and send invites (POST /client/invites).
    /// ABSENT on a daemon too old to say, and on every self-host, and nil reads
    /// exactly like false: no button, no nudge, nothing offered.
    var invite_sharing: Bool?
    /// The hidden features an operator has turned on for this mailbox, as
    /// keys. ABSENT on a daemon too old to say, and nil reads exactly like
    /// empty: nothing extra is shown. Read it through `AppStore.hasFeature`,
    /// never directly; see `HiddenFeatures`.
    var features: [String]?
    /// Whether an iPhone app has EVER connected to this mailbox. ABSENT on a
    /// daemon too old to say, and nil reads as "do not know": the TestFlight
    /// ask never fires on a guess, because telling somebody about the app they
    /// are already using is worse than not telling them.
    var ios_connected: Bool?
    /// Whether `forward_of_message_id` on /client/actions/send MEANS anything
    /// here. ABSENT on a daemon too old to say, and nil must read as false for a
    /// reason no other capability flag has: serde ignores unknown fields, so an
    /// old daemon handed a forward answers a cheerful 200 "sent" having dropped
    /// the id on the floor and mailed the covering note ALONE. There is no error
    /// to catch and no undo for a send, so the only place to stop it is before
    /// the composer opens. See `AppStore.forwardingAvailable`.
    var forwarding: Bool?
    /// Whether Gmail still opens for this mailbox, and what to do if it does
    /// not. ABSENT on a daemon too old to say and on any door wired without a
    /// metrics registry, and nil must read as "we do not know" rather than as
    /// "connected": claiming a mailbox is fine on a daemon that cannot see is
    /// the failure this whole field exists to end.
    var gmail: GmailHealth?
    /// A mailbox catch-up in flight. ABSENT is the normal state; presence is
    /// the explanation for a triage queue that is not moving.
    var catch_up: CatchUpProgress?
    /// Whether Gmail sync is working right now, and if not why and when it
    /// tries again. ABSENT on an older daemon or a door without a metrics
    /// registry; nil means "we do not know", never "healthy". See `SyncStatus`.
    var sync: SyncHealth?
    /// Whether triage can run and how much work it has. Same absence rule.
    var triage: TriageHealth?
    /// Whether `attachment_ids` on a send and on a draft mean anything here.
    /// ABSENT on a daemon too old to stage files, and absence reads as no —
    /// the same silent-failure shape as `forwarding`: an old daemon ignores
    /// the key and mails the words without the files. See
    /// `AppStore.composeAttachmentsAvailable`.
    var compose_attachments: Bool?
    /// Whether this daemon keys new-message drafts by id (many per account,
    /// POST /client/drafts + PUT /client/drafts/{id}). ABSENT on an older
    /// daemon, which keeps ONE new-message slot, and absence reads as no: the
    /// new routes would 404 there, and the old nil-key PUT is the only write
    /// that means anything. See `AppStore.draftsV2Available` and
    /// `DraftSaver.save`, which also has to tell "no" from "not asked yet".
    var drafts_v2: Bool?
    /// Whether `include_sent` on `/client/search` exists, so a search can ask
    /// for plain ranked order (no Not done / Done split) and keep the owner's
    /// sent mail. ABSENT on an older daemon, and absence reads as no: that
    /// daemon ignores the key, and dropping `unfinished_first` there would
    /// lose sent mail from keyword results. See `AppStore.searchPlainOrder`.
    var search_plain_order: Bool?
    /// Whether this daemon keeps an outbox (`/client/scheduled`). ABSENT on an
    /// older daemon, and absence reads as no: every schedule door is hidden
    /// rather than offered as a 404 on the click. See
    /// `AppStore.scheduledSendAvailable`.
    var scheduled_send: Bool?
    /// Whether `GET /client/timeline` and `PATCH /client/timeline/ghosts/{id}`
    /// exist. ABSENT on an older daemon, which 404s both, and absence reads as
    /// no: the calendar page is not offered at all (`AppStore.calendarPageAvailable`).
    var timeline: Bool?
    /// Whether this mailbox has granted Google Calendar, and how to grant it.
    /// ABSENT on a daemon too old to have the grant path, and absence reads as
    /// "do not know", never as "not granted": with no way to connect there is
    /// nothing true to ask, so no connect card or banner is drawn.
    ///
    /// LENIENT: a `calendar` this client cannot read (no `granted`, not an
    /// object) reads as absent. This struct is everyone's ten-second poll, and
    /// one optional field must never be able to fail the whole decode.
    @LenientDecoded var calendar: CalendarAccess? = nil
}

/// An optional field that decodes to nil instead of throwing when its value
/// is malformed. For a field on a struct whose decode must not fail because
/// of it; a missing key or `null` is nil as usual, and encoding writes the
/// value or nothing.
@propertyWrapper
struct LenientDecoded<Value: Codable & Sendable & Hashable>: Codable, Sendable, Hashable {
    var wrappedValue: Value?

    init(wrappedValue: Value?) { self.wrappedValue = wrappedValue }

    init(from decoder: Decoder) throws {
        wrappedValue = try? decoder.singleValueContainer().decode(Value.self)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(wrappedValue)
    }
}

extension KeyedDecodingContainer {
    /// What synthesized `Decodable` calls for a `@LenientDecoded` property: a
    /// missing key is nil rather than `keyNotFound`, and a bad value is nil.
    func decode<V>(_: LenientDecoded<V>.Type, forKey key: Key) throws -> LenientDecoded<V> {
        LenientDecoded(wrappedValue: try? decodeIfPresent(V.self, forKey: key))
    }
}

extension KeyedEncodingContainer {
    /// The key is left out when the value is nil, as a plain optional's is.
    mutating func encode<V>(_ value: LenientDecoded<V>, forKey key: Key) throws {
        try encodeIfPresent(value.wrappedValue, forKey: key)
    }
}

/// The calendar grant off /client/stats. How a mailbox connects: hosted sends
/// `connect_url` (the control plane's consent, with the account pinned); a
/// self-host daemon that can run Google's consent itself says
/// `self_connect: true`, and the app asks it for the link
/// (`POST /client/calendar/connect`). Either may be sent while granted (a
/// renewal is the same consent again); neither means there is nothing to
/// offer, which is harmless.
struct CalendarAccess: Codable, Sendable, Hashable {
    var granted: Bool
    var connect_url: String?
    /// The shell command older self-host daemons send. Decoded so their stats
    /// still read, and never used: the terminal is a docs hint now, not a
    /// path in the app.
    var connect_command: String?
    /// Lenient: absent on an older daemon, and a malformed value must not
    /// cost the rest of the grant.
    @LenientDecoded var self_connect: Bool? = nil
}

/// `POST /client/calendar/connect` on a self-host daemon: Google's authorize
/// link for this mailbox, good for `expires_in` seconds. Both optional, so a
/// body without a link reads as "no link" rather than a decode failure.
struct CalendarConnectStart: Codable, Sendable, Hashable {
    var url: String?
    var expires_in: Int?
}

/// The 30-day re-walk the daemon falls back to when Gmail's history cursor
/// expires, which is what happens to any mailbox that goes quiet for a week.
///
/// It is the sync loop's longest single call and triage cannot run until it
/// finishes, so without this object a working mailbox is indistinguishable from
/// a hung one — which is exactly how it looked the day it was added.
struct CatchUpProgress: Codable, Sendable, Hashable {
    var done: Int
    var total: Int
}

/// Gmail sync health off /client/stats. `gmail.connected` answers only "is
/// the credential alive"; a mailbox Gmail is rate-limiting has a good
/// credential and still syncs nothing, and this is where that shows.
struct SyncHealth: Codable, Sendable, Hashable {
    /// The last poll succeeded and no failure has followed.
    var ok: Bool
    /// RFC3339, the daemon's last successful Gmail poll. THE source for
    /// "synced 4m ago": the client's own poll of the daemon is not a sync.
    var last_success_at: String?
    /// Why the current outage is happening. Present only while `ok` is false.
    var error: SyncErrorInfo?
    /// RFC3339, when the daemon tries Gmail again. Only while `ok` is false.
    var next_retry_at: String?
    var consecutive_failures: Int?
}

struct SyncErrorInfo: Codable, Sendable, Hashable {
    /// `auth`, `quota`, `http` or `network`.
    var kind: String
    /// A closed constant such as `RATE_LIMIT_EXCEEDED`. For hover text and
    /// developer mode ONLY: it is a code, not a sentence.
    var reason: String
    var at: String?
}

/// Triage worker state off /client/stats.
struct TriageHealth: Codable, Sendable, Hashable {
    /// Triage spends nothing until a Gmail poll succeeds.
    var waiting_for_sync: Bool
    /// RFC3339, the end of a model-provider cooldown, when one is in force.
    var cooldown_until: String?
    /// Jobs waiting for, and currently running through, triage.
    var queued: Int
    var leased: Int
    /// Jobs parked on a spent daily budget, and when the first resumes.
    var budget_parked: Int
    var budget_resumes_at: String?
}

/// The daemon's answer to "is this mailbox still connected".
///
/// A dead refresh token is invisible from everywhere a person looks: mail
/// simply stops arriving, which is indistinguishable from a quiet week. The
/// daemon has always detected it exactly and, until this field, told only
/// Prometheus — so an operator could find out and the person with the empty
/// mailbox could not.
struct GmailHealth: Codable, Sendable, Hashable {
    /// False while the refresh token is dead. The only field that is always
    /// present when this object is.
    var connected: Bool
    /// RFC3339, when the current outage began. Present only while disconnected,
    /// and it is when the mailbox went dark rather than when it last retried.
    var disconnected_since: String?
    /// Where to re-consent. Present only while disconnected AND only on hosted:
    /// a self-host mailbox is repaired with `squelchd auth` at a shell, which is
    /// not a link anything can offer. Nil is therefore the self-host branch, not
    /// an error.
    var reconnect_url: String?
}

// MARK: - calendar timeline

/// What `GET /client/timeline` answers: the ghosts Passband derived from mail
/// and, once the read-only calendar sync has landed on the daemon, the user's
/// real events beside them.
///
/// EVERY ROW DECODES ALONE. One ghost a newer daemon shapes differently must
/// cost that ghost, not the page: the page is somebody's week, and a decode
/// error would blank all of it. `events` and `calendar` are ABSENT on a daemon
/// that has ghosts but no calendar sync, and that is the normal state today.
struct TimelineFeed: Codable, Sendable, Hashable {
    var ghosts: [TimelineGhost]
    var events: [TimelineEvent]?
    var calendar: TimelineCalendarStatus?
    /// Wave 3: the calendars being synced, primary first. ABSENT on an older
    /// daemon, and then nothing on the page offers to create an event.
    var calendars: [TimelineCalendar]?

    init(
        ghosts: [TimelineGhost] = [], events: [TimelineEvent]? = nil,
        calendar: TimelineCalendarStatus? = nil, calendars: [TimelineCalendar]? = nil
    ) {
        self.ghosts = ghosts
        self.events = events
        self.calendar = calendar
        self.calendars = calendars
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ghosts = (try c.decodeIfPresent([OneRow<TimelineGhost>].self, forKey: .ghosts) ?? [])
            .compactMap(\.value)
        events = try c.decodeIfPresent([OneRow<TimelineEvent>].self, forKey: .events)?
            .compactMap(\.value)
        calendar = try? c.decodeIfPresent(TimelineCalendarStatus.self, forKey: .calendar)
        calendars = (try? c.decodeIfPresent([OneRow<TimelineCalendar>].self, forKey: .calendars))?
            .map { $0.compactMap(\.value) }
    }

    /// A row that may fail to decode without failing its array.
    private struct OneRow<T: Decodable>: Decodable {
        let value: T?
        init(from decoder: Decoder) throws { value = try? T(from: decoder) }
    }
}

/// `kind` on a ghost: what sort of thing the mail described.
enum GhostKind: String, LenientRawEnum {
    case other, ticket, travel, reservation, invite, proposal
    static var unknownFallback: GhostKind { .other }
}

/// How far the user is already in. `committed` means they bought, booked,
/// registered or accepted; it is certain and carries no likelihood.
enum GhostCommitment: String, LenientRawEnum {
    case unknown, committed, invited, proposed
    static var unknownFallback: GhostCommitment { .unknown }
}

/// The served state: the user's word (`going`, `dismissed`) over the mail's
/// (`cancelled`) over the clock (`expired`).
enum GhostState: String, LenientRawEnum {
    case ghost, going, dismissed, cancelled, expired
    static var unknownFallback: GhostState { .ghost }
}

/// What the mail says, apart from the user's word: a ghost can be `going`
/// and also `cancelled` here, and the page must say both.
enum GhostMailState: String, LenientRawEnum {
    case live, cancelled
    static var unknownFallback: GhostMailState { .live }
}

/// Which tier produced the resolved end. Only `stated` and `user` are facts.
enum GhostEndSource: String, LenientRawEnum {
    case stated, user, history, model
    case defaulted = "default"
    static var unknownFallback: GhostEndSource { .defaulted }
}

/// The likelihood band. Three of them, never a number.
enum GhostBand: String, LenientRawEnum {
    case likely, maybe, unlikely
    /// A band this client does not know is drawn as the middle one: shown,
    /// but not planned around.
    static var unknownFallback: GhostBand { .maybe }
}

/// What one source message did to the ghost.
enum GhostSourceRole: String, LenientRawEnum {
    case created, updated, cancelled, agreed
    static var unknownFallback: GhostSourceRole { .updated }
}

struct GhostEstimate: Codable, Sendable, Hashable {
    var minutes: Int
    /// The model's own words for why. Model text, so the page quotes it and
    /// says whose it is rather than speaking it in Passband's voice.
    var basis: String?
}

struct GhostLikelihood: Codable, Sendable, Hashable {
    var band: GhostBand
    /// Model text, like `GhostEstimate.basis`.
    var reason: String?
}

struct GhostSource: Codable, Sendable, Hashable {
    var message_id: Int
    var thread_id: String
    var role: GhostSourceRole
}

/// The real event a ghost duplicates, by the daemon's display match. Display
/// de-duplication only: it says draw the event, not the ghost.
struct GhostMatchedEvent: Codable, Sendable, Hashable {
    var calendar_id: String
    var id: String
}

/// One ghost as `ghost_view` serves it (squelch-api handlers.rs). `start` and
/// `end` are `YYYY-MM-DD` for a date-only ghost and RFC3339 with an offset
/// otherwise. Optional where a daemon of this wave could omit the key.
struct TimelineGhost: Codable, Sendable, Hashable, Identifiable {
    var id: Int
    var kind: GhostKind
    var title: String
    var start: String
    var start_timezone: String?
    var all_day: Bool
    var end: String?
    var end_estimated: Bool?
    var end_source: GhostEndSource?
    var estimate: GhostEstimate?
    var location: String?
    var commitment: GhostCommitment
    var state: GhostState
    var mail_state: GhostMailState?
    /// False for a ghost made from mail that did not pass authentication.
    /// Nil (an older shape) draws no mark rather than guessing either way.
    var verified: Bool?
    var likelihood: GhostLikelihood?
    var sources: [GhostSource]
    /// Slice B. Absent until the daemon syncs real events.
    var matched_event: GhostMatchedEvent?
}

enum TimelineEventStatus: String, LenientRawEnum {
    case confirmed, tentative
    static var unknownFallback: TimelineEventStatus { .confirmed }
}

enum TimelineSelfResponse: String, LenientRawEnum {
    case accepted, declined, tentative
    case needsAction = "needs_action"
    static var unknownFallback: TimelineSelfResponse { .needsAction }
}

/// One real event off the user's Google Calendar (read-only sync). `id` is
/// unique only within its calendar.
struct TimelineEvent: Codable, Sendable, Hashable, Identifiable {
    var id: String
    var calendar_id: String
    var calendar_name: String?
    /// Google allows an event with no title.
    var title: String?
    var start: String
    var end: String?
    var all_day: Bool
    var start_timezone: String?
    /// The location as Google holds it: what the editor shows and writes.
    var location: String?
    /// The location cleaned by the daemon's small model for display (a
    /// booking service's marketing cut away): what the card shows. "" when
    /// the location names no place. Nil until the daemon has cleaned it, and
    /// from an older daemon: the card shows `location`.
    var place: String?
    var status: TimelineEventStatus?
    /// Nil when the user is not an attendee (their own event).
    var self_response: TimelineSelfResponse?
    /// False when the event is marked free.
    var busy: Bool?
    var organizer: String?
    var html_link: String?
    /// Wave 3. The calendar's colour as Google reports it, or the event's own
    /// override, "#rrggbb". Nil from an older daemon: drawn in the neutral.
    var color: String?
    /// Wave 3. The user can write to this calendar AND the event is not
    /// another organizer's. Nil (an older daemon) is NOT editable: no Edit,
    /// no Delete, no `e`.
    var editable: Bool?
    /// Wave 3. The event's notes, laundered and capped like `location`.
    var description: String?
    /// Wave 5. Who is on the event, the user included when Google lists
    /// them. Nil from an older daemon and on an event with nobody on it.
    var guests: [TimelineGuest]?
    /// Wave 5. Google holds more guests than the daemon stored. The list is
    /// then a part of the truth and is never sent back as the whole of it.
    var guests_truncated: Bool?
    /// Wave 5. The user may change who is on it: editable, not truncated,
    /// and theirs to organize. Nil (an older daemon) is NO, and is also how
    /// the client knows a daemon cannot invite at all (Lib/EventGuests.swift).
    var guest_editable: Bool?
    /// Gmail made this event itself from the user's mail (Google's
    /// `eventType: fromGmail`). Google refuses edits to it, so `editable` is
    /// false; the card says where it came from. Nil from an older daemon.
    var from_gmail: Bool?
}

/// How a guest answered, in the daemon's four words.
enum GuestResponse: String, LenientRawEnum {
    case accepted, declined, tentative, pending
    static var unknownFallback: GuestResponse { .pending }
}

/// Wave 5: one person on an event. Only the address is required; a row
/// without one is dropped, and it costs the event nothing.
struct TimelineGuest: Codable, Sendable, Hashable {
    var email: String
    var name: String?
    var response: GuestResponse?
    var organizer: Bool?
    /// The user's own row. `self` on the wire.
    var isSelf: Bool?

    enum CodingKeys: String, CodingKey {
        case email, name, response, organizer
        case isSelf = "self"
    }

    init(
        email: String, name: String? = nil, response: GuestResponse? = nil, organizer: Bool? = nil,
        isSelf: Bool? = nil
    ) {
        self.email = email
        self.name = name
        self.response = response
        self.organizer = organizer
        self.isSelf = isSelf
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        email = try c.decode(String.self, forKey: .email)
        name = try? c.decodeIfPresent(String.self, forKey: .name)
        response = try? c.decodeIfPresent(GuestResponse.self, forKey: .response)
        organizer = try? c.decodeIfPresent(Bool.self, forKey: .organizer)
        isSelf = try? c.decodeIfPresent(Bool.self, forKey: .isSelf)
    }
}

extension TimelineEvent {
    /// THE NEW KEYS COST ONLY THEMSELVES. A `color` that is not a string, or
    /// an `editable` that is not a bool, from some daemon mid-change, leaves
    /// that key nil rather than dropping the event (the synthesized decoder
    /// would throw, and the feed's row guard would drop the whole row). In an
    /// extension so the memberwise init stays.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        calendar_id = try c.decode(String.self, forKey: .calendar_id)
        calendar_name = try c.decodeIfPresent(String.self, forKey: .calendar_name)
        title = try c.decodeIfPresent(String.self, forKey: .title)
        start = try c.decode(String.self, forKey: .start)
        end = try c.decodeIfPresent(String.self, forKey: .end)
        all_day = try c.decode(Bool.self, forKey: .all_day)
        start_timezone = try c.decodeIfPresent(String.self, forKey: .start_timezone)
        location = try c.decodeIfPresent(String.self, forKey: .location)
        status = try c.decodeIfPresent(TimelineEventStatus.self, forKey: .status)
        self_response = try c.decodeIfPresent(TimelineSelfResponse.self, forKey: .self_response)
        busy = try c.decodeIfPresent(Bool.self, forKey: .busy)
        organizer = try c.decodeIfPresent(String.self, forKey: .organizer)
        html_link = try c.decodeIfPresent(String.self, forKey: .html_link)
        color = try? c.decodeIfPresent(String.self, forKey: .color)
        editable = try? c.decodeIfPresent(Bool.self, forKey: .editable)
        description = try? c.decodeIfPresent(String.self, forKey: .description)
        // Each guest decodes alone, and a `guests` of the wrong shape is nil.
        guests = (try? c.decodeIfPresent([OneGuest].self, forKey: .guests))?.compactMap(\.value)
        guests_truncated = try? c.decodeIfPresent(Bool.self, forKey: .guests_truncated)
        guest_editable = try? c.decodeIfPresent(Bool.self, forKey: .guest_editable)
    }

    private struct OneGuest: Decodable {
        let value: TimelineGuest?
        init(from decoder: Decoder) throws { value = try? TimelineGuest(from: decoder) }
    }
}

/// Wave 3: one calendar being synced, from the feed's `calendars`. Primary
/// first. Only `writable` ones are offered in the event editor.
struct TimelineCalendar: Codable, Sendable, Hashable, Identifiable {
    var id: String
    var name: String?
    var color: String?
    var primary: Bool?
    var writable: Bool?
    /// Wave 5. Events on it may carry guests: the primary calendar, or one
    /// the user owns. Nil from an older daemon (`EventGuests.mayInvite`).
    var can_invite: Bool?
}

enum TimelineSyncError: String, LenientRawEnum {
    case auth, quota, unavailable
    static var unknownFallback: TimelineSyncError { .unavailable }
}

struct TimelineCalendarStatus: Codable, Sendable, Hashable {
    var granted: Bool
    var synced_at: String?
    var error: TimelineSyncError?
    /// The daemon served only the nearest events of a longer list. ABSENT on
    /// a daemon too old to cut one, and absence reads as false.
    var truncated: Bool?
}

/// `PATCH /client/timeline/ghosts/{id}` answers this.
struct GhostPatchResult: Codable, Sendable, Hashable {
    var updated: Bool
    var id: Int
}

// MARK: - invites

/// What GET /client/invites answers: whether sharing is possible at all, and
/// the one number the invite mail would be able to say about this mailbox.
///
/// `open_percent` is NIL far more often than not, and that is by design on the
/// daemon's side (too new a mailbox, too little mail, or a rate not worth
/// quoting). The sheet must have copy for both, and it must never invent one.
struct InviteAvailability: Codable, Sendable, Hashable {
    var can_share: Bool
    /// WHY not, when `can_share` is false. `no_control_plane` is a property of
    /// the deployment and nothing the reader can act on; `no_write_credential`
    /// names a command that fixes it. Absent when sharing is possible, and on a
    /// daemon too old to say — which the sheet reads as the first, the more
    /// conservative of the two, because telling somebody to run a command that
    /// will not help is worse than telling them nothing.
    var reason: String?
    var open_percent: Int?
    /// The DRAFT the composer opens on: the daemon's first version of the mail,
    /// which the user then edits. Absent when this daemon cannot share, because
    /// there is no mail to draft.
    ///
    /// Written by the daemon rather than the app because the one thing in it
    /// that must be TRUE - the open rate - is a fact only that machine can
    /// compute.
    var subject: String?
    var body: String?
    /// The token in `body` that becomes each friend's own invite. Sent rather
    /// than hardcoded so the composer can name it in its own hint and refuse a
    /// draft that has lost it, without the two ever disagreeing.
    var invite_marker: String?
}

/// One friend's outcome. `error` is copy the daemon wrote for a human; it never
/// carries a status code or anything about the invite code itself.
struct InviteResult: Codable, Sendable, Hashable, Identifiable {
    var email: String
    var sent: Bool
    var error: String?
    var id: String { email }
}

/// What POST /client/invites answers. `remaining` is nil when nothing was
/// minted at all, so it is never rendered as "0 left" for a press that failed
/// before it reached the control plane.
struct InviteSendResponse: Codable, Sendable, Hashable {
    var results: [InviteResult]
    var remaining: Int?
}

// MARK: - usage

struct UsageRow: Codable, Sendable, Hashable, Identifiable {
    var day: String
    var calls: Int
    var input_tokens: Int
    var output_tokens: Int
    /// This day's spend, priced like the window total. ABSENT from a daemon
    /// older than per-day pricing; `UsageSeries` then pro-rates the total by
    /// tokens so the chart still draws.
    var est_cost_usd: Double?
    var id: String { day }
}

struct UsageTotals: Codable, Sendable, Hashable {
    var calls: Int
    var input_tokens: Int
    var output_tokens: Int
    var est_cost_usd: Double
}

struct UsageCategory: Codable, Sendable, Hashable {
    var rows: [UsageRow]
    var totals: UsageTotals
    var model: String
    var provider: String?
}

struct UsageResponse: Codable, Sendable, Hashable {
    var rows: [UsageRow]
    var totals: UsageTotals
    var provider: String?
    var model: String
    var categories: [String: UsageCategory]?
}

// MARK: - mail activity

/// One UTC day of the mailbox's own traffic (GET /client/mail-activity).
/// `received` is everything that arrived — sealed included, spam never — and
/// the four tiers partition the non-sealed part of it; mail the pipeline has
/// not reached yet is received and in no tier.
struct MailActivityDay: Codable, Sendable, Hashable, Identifiable {
    var day: String
    var received: Int
    var sent: Int
    var sealed: Int
    var past_due: Int
    var deadline: Int
    var signal: Int
    var noise: Int
    var pending: Int?
    var id: String { day }
}

struct MailActivityResponse: Codable, Sendable, Hashable {
    /// The window actually served, clamped by the daemon.
    var days: Int
    /// First and last day keys of that window, inclusive — the daemon's
    /// calendar, which is what the rows are keyed against.
    var since: String
    var until: String
    /// Sparse: only days with mail, ascending.
    var rows: [MailActivityDay]
}

// MARK: - triage config

enum TriageConfigSource: String, LenientRawEnum {
    case `default`, config, override
    static var unknownFallback: TriageConfigSource { .default }

    var note: String {
        switch self {
        case .config: "(from config file)"
        case .override: "(app override)"
        case .default: "(default)"
        }
    }
}

struct TriageConfigSources: Codable, Sendable, Hashable {
    var thread_daily_cap: TriageConfigSource
    var sender_daily_cap: TriageConfigSource
    var global_daily_cap: TriageConfigSource
}

struct TriageStage1: Codable, Sendable, Hashable {
    var model: String
    var global_daily_cap: Int
    var source: TriageConfigSource
    var avg_calls_per_day: Double
    var avg_tokens_in_per_call: Double?
    var avg_tokens_out_per_call: Double?
    var price_in_per_mtok: Double
    var price_out_per_mtok: Double
}

struct TriageConfig: Codable, Sendable, Hashable {
    var thread_daily_cap: Int
    var sender_daily_cap: Int
    var global_daily_cap: Int
    var sources: TriageConfigSources
    var avg_inbound_per_day: Double
    var avg_stage2_calls_per_day: Double
    var avg_tokens_in_per_call: Double?
    var avg_tokens_out_per_call: Double?
    var price_in_per_mtok: Double
    var price_out_per_mtok: Double
    var stage1: TriageStage1
    var stage2_model: String
}

struct TriageConfigPatch: Codable, Sendable {
    var thread_daily_cap: Int?
    var sender_daily_cap: Int?
    var global_daily_cap: Int?
    var stage1_global_daily_cap: Int?
}

// MARK: - sealed

/// core::types::SealedKind — why a message was sealed out of the sitrep.
/// Lenient like the rest, but it must NOT collapse an unknown value onto a known
/// case: otp/verification auto-reveal, so a kind this build has never heard of
/// keeps its raw string in `.unknown`, stays inert, and re-encodes verbatim.
enum SealedKind: LenientRawEnum {
    case otp
    case passwordReset
    case magicLink
    case loginAlert
    case securityAlert
    case verification
    /// A security setting on the owner's account changed (recovery phone, 2FA,
    /// password, SSH key, a card added to a wallet...). The serious ones also
    /// carry `SealedMeta.critical`.
    case accountChange
    /// A third-party app was granted access to the account.
    case appGrant
    /// Proof the owner used one account to sign in to another service ("You
    /// shared some Google Account data with X"). Never asks a question.
    case signinReceipt
    case unknown(String)

    var rawValue: String {
        switch self {
        case .otp: "otp"
        case .passwordReset: "password_reset"
        case .magicLink: "magic_link"
        case .loginAlert: "login_alert"
        case .securityAlert: "security_alert"
        case .verification: "verification"
        case .accountChange: "account_change"
        case .appGrant: "app_grant"
        case .signinReceipt: "signin_receipt"
        case .unknown(let raw): raw
        }
    }

    /// Total, so the LenientRawEnum decode never has to fall back — and so no
    /// wire string is ever dropped on the way in.
    init(rawValue: String) {
        switch rawValue {
        case "otp": self = .otp
        case "password_reset": self = .passwordReset
        case "magic_link": self = .magicLink
        case "login_alert": self = .loginAlert
        case "security_alert": self = .securityAlert
        case "verification": self = .verification
        case "account_change": self = .accountChange
        case "app_grant": self = .appGrant
        case "signin_receipt": self = .signinReceipt
        default: self = .unknown(rawValue)
        }
    }

    /// Unreachable — `init(rawValue:)` never fails; the protocol still wants it.
    static var unknownFallback: SealedKind { .unknown("") }

    /// Identity IS the wire string, so `.unknown("otp")` can never become a
    /// second spelling of `.otp` that set membership would miss.
    static func == (a: SealedKind, b: SealedKind) -> Bool { a.rawValue == b.rawValue }
    func hash(into hasher: inout Hasher) { hasher.combine(rawValue) }
}

struct SealedMeta: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var thread_id: String
    var sender: String
    var subject: String
    var kind: SealedKind?
    var received_at: String
    /// A takeover-class account change (recovery phone moved, 2FA off, password
    /// changed...). ABSENT from a daemon older than server-side verdicts, and
    /// that absence is how the client tells the two apart: a daemon that
    /// serves this key also serves `verdict` and the PUT that writes it (see
    /// Lib/AuthPolicy.swift, `speaksVerdicts`).
    var critical: Bool? = nil
    /// The owner's ruling, "mine" or "not_mine", or null while unruled. Kept as
    /// the raw string so a value this build has never heard of decodes as
    /// "no verdict" instead of failing the whole list.
    var verdict: String? = nil
    /// The message id of the code, link or receipt that explains this alert
    /// (the same service, within minutes). Never set on a critical change.
    var explained_by: Int? = nil
}

/// POST /client/sealed/{id}/reveal's optional `?source=`: where the reveal came
/// from. Absent means interactive (a page the human acted on).
enum RevealSource {
    static let notification = "notification"
}

/// PUT /client/sealed/{id}/verdict. `verdict` nil CLEARS the ruling, and the
/// key must still be on the wire as an explicit null: the shared encoder drops
/// nil optionals, so this one encodes by hand.
struct SealedVerdictBody: Encodable, Sendable {
    var verdict: String?

    private enum CodingKeys: String, CodingKey { case verdict }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(verdict, forKey: .verdict)
    }
}

/// POST /client/sealed/{id}/reveal. `body` is a sensitive one-time reveal:
/// hold in view state only, never persist.
struct RevealedSealed: Codable, Sendable {
    var id: Int
    var thread_id: String
    var sender: String
    var from_name: String?
    var subject: String
    var kind: SealedKind?
    var received_at: String
    var body: String
    var html: String?
}

// MARK: - envelopes / action bodies

struct Page<T: Codable & Sendable>: Codable, Sendable {
    var items: [T]
    var next_cursor: String?
}

/// POST /client/v2/messages/{id}/attention: how long the message was at the
/// reader's focus and how many links in its body were activated, since the
/// last report. The daemon ADDS these to what it holds, so either may be 0 and
/// a message read in three sittings is three bodies.
///
/// Two numbers and nothing else, on purpose: which link, where it went and
/// what the mail said are not the daemon's to learn from this route.
struct AttentionBody: Encodable, Sendable {
    var dwell_ms: UInt64
    var link_clicks: UInt32
}

struct ArchiveBody: Codable, Sendable {
    var message_id: Int
    var confirm: Bool
}

/// "Not spam": one message out of the provider's spam folder and back into the
/// inbox. Same shape as `ArchiveBody` because it is the same kind of act — one
/// message, one confirmed Gmail write — but its own type, so a future field on
/// either cannot silently appear on the other.
struct NotSpamBody: Codable, Sendable {
    var message_id: Int
    var confirm: Bool
}

struct LabelBody: Codable, Sendable {
    var message_id: Int
    var add: [String]?
    var remove: [String]?
    var confirm: Bool
}

struct SendBody: Codable, Sendable {
    var reply_to_message_id: Int?
    /// The message being PASSED ON, and the whole of a forward on this wire:
    /// the daemon quotes the original, carries its attachments over and starts
    /// a NEW thread from it. MUTUALLY EXCLUSIVE with `reply_to_message_id` —
    /// the daemon rejects a body naming both — and it requires a non-empty
    /// `to`, because a forward has nobody to derive a recipient from. An empty
    /// `body` is fine here and only here: passing mail on without a word of
    /// your own is the ordinary case.
    var forward_of_message_id: Int?
    var to: String?
    /// PRESENT — `""` INCLUDED — means this client is stating the whole copy
    /// list and the daemon does not consult the parent's. OMITTED means derive
    /// it, which is what a composer that has not yet learned the derived set
    /// must send: asserting an empty Cc it never had would silently narrow a
    /// reply-all to one person. See `ComposeState.recipientsStated`.
    var cc: String?
    /// Blind recipients, comma-joined. Omitted when there are none — nothing
    /// derives these, since no header of a parent records who was blind-copied
    /// on it, so absent and `""` mean the same thing here unlike `cc`. Filtered
    /// server-side against `to` and `cc`, so a person on both lists is delivered
    /// once rather than twice.
    var bcc: String?
    /// ADDRESS A SEND GROUP. What it does is the GROUP's mode, not this
    /// message's: `to`/`bcc` groups are already expanded into the fields above,
    /// so this is attribution; an `individual` group makes the daemon fan out and
    /// answer with a batch id instead of a sent message.
    var group_id: Int?
    /// Omitted (not "") on a reply: the daemon derives `Re: <parent subject>`
    /// only when the field is absent — `Some("")` is an explicit empty subject.
    var subject: String?
    var body: String
    /// `"markdown"`: `body` is markdown source — it goes out verbatim as the
    /// text/plain part and the daemon renders the HTML alternative beside it.
    /// Absent = single-part plain text, exactly the pre-markdown wire.
    var body_format: String?
    var confirm: Bool
    var override_guard: Bool?
    /// Server-side draft to delete once the send succeeds.
    var draft_id: Int?
    /// Mint a read-tracking pixel for this send. OMITTED when false, the same
    /// way `subject` is: absent reads as false server-side. `true` on a daemon
    /// with no tracking base_url is NOT an error — the mail goes out untracked.
    var include_tracker: Bool?
    /// Answer EVERYONE on the parent, not just its sender. Omitted when false,
    /// like the two above. The daemon derives the recipient set itself at send
    /// time — the client never enumerates it into `to`, so a stale or failed
    /// preview cannot change who the mail actually reaches.
    var reply_all: Bool?
    /// The staged files this send carries, tray order. Omitted when there are
    /// none. Which go inline is not said here: the daemon reads that off the
    /// body's `cid:` references, the same rule the tray shows.
    var attachment_ids: [Int]?
}

/// GET /client/messages/{id}/reply_recipients?all=true — the addresses a reply
/// to this message would carry, as the daemon derives them. Asked for so the
/// review pane can state the REAL set rather than guess at it; the send derives
/// it again server-side, so this answer is display-only and never rides back.
/// `cc` is absent or empty when the parent has no other participants.
struct ReplyRecipients: Codable, Sendable, Hashable {
    var to: String
    var cc: String?
}

// MARK: - read tracking

/// GET /client/messages/{id}/opens — one recorded fetch of a sent message's
/// pixel. `{id}` is the LOCAL message id, i.e. the `echo_message_id` a send
/// handed back; an untracked or unknown id answers with an empty list, so
/// asking about somebody else's message is a legal question with a dull answer.
struct MessageOpen: Codable, Sendable, Hashable {
    /// Unix SECONDS — the one stamp on this wire that is not RFC3339 text.
    var opened_at: Int
    var user_agent: String?
    /// "proxied" or "unknown". Kept as a String because the daemon's set is
    /// open-ended: an unrecognised value must read as the weaker claim rather
    /// than be collapsed onto the stronger one.
    var classification: String

    var date: Date { Date(timeIntervalSince1970: TimeInterval(opened_at)) }

    /// The fetch came from Gmail's image proxy, which may be warming a cache
    /// rather than showing a human the mail. The copy says so.
    var viaProxy: Bool { classification == "proxied" }
}

struct MessageOpensResult: Codable, Sendable {
    var opens: [MessageOpen]
}

/// GET/POST /client/tracking-config. `configured` false means the daemon has
/// nowhere to point a pixel, so every send goes out untracked whatever the
/// client asks — hide the affordance rather than offer a dead switch.
/// `default_enabled` is remembered CLIENT preference: the daemon never reads it
/// when sending, every send states its own `include_tracker`.
struct TrackingConfig: Codable, Sendable, Hashable {
    var default_enabled: Bool
    var configured: Bool
}

/// POST /client/tracking-config. An omitted field leaves the stored value alone.
struct TrackingConfigBody: Codable, Sendable {
    var default_enabled: Bool?
}

/// One language the daemon's detector knows, by ISO 639-1 code and English
/// name ("fr", "French").
struct TranslateLanguage: Codable, Sendable, Hashable, Identifiable {
    var code: String
    var name: String
    var id: String { code }
}

/// GET/POST /client/translate-config (docs/TRANSLATION.md): the languages the
/// owner reads, which the daemon leaves untranslated. `active` false means the
/// daemon has no model to translate with, so the set changes nothing yet.
///
/// Decoded LENIENTLY, field by field, and a malformed `available` entry is
/// dropped rather than failing the list: this is a settings card, and a card
/// that will not decode is a setting nobody can reach.
struct TranslateConfig: Codable, Sendable, Hashable {
    var active: Bool
    /// Lowercase ISO codes, as stored.
    var read_languages: [String]
    /// Every language the detector knows except English, sorted by name.
    var available: [TranslateLanguage]

    init(active: Bool, read_languages: [String], available: [TranslateLanguage]) {
        self.active = active
        self.read_languages = read_languages
        self.available = available
    }

    private enum CodingKeys: String, CodingKey { case active, read_languages, available }

    /// Decodes one array element as whatever it is, so a bad one can be
    /// skipped without stopping the container.
    private struct Skip: Decodable { init(from decoder: Decoder) throws {} }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        active = ((try? c.decodeIfPresent(Bool.self, forKey: .active)) ?? nil) ?? false
        var codes: [String] = []
        if var list = try? c.nestedUnkeyedContainer(forKey: .read_languages) {
            while !list.isAtEnd {
                if let code = try? list.decode(String.self) {
                    let normal = code.lowercased()
                    if !normal.isEmpty, !codes.contains(normal) { codes.append(normal) }
                } else if (try? list.decode(Skip.self)) == nil {
                    break
                }
            }
        }
        read_languages = codes
        var langs: [TranslateLanguage] = []
        if var list = try? c.nestedUnkeyedContainer(forKey: .available) {
            while !list.isAtEnd {
                if let lang = try? list.decode(TranslateLanguage.self), !lang.code.isEmpty {
                    langs.append(TranslateLanguage(code: lang.code.lowercased(), name: lang.name))
                } else if (try? list.decode(Skip.self)) == nil {
                    break
                }
            }
        }
        available = langs
    }

    /// What a chip says: the detector's English name, or the bare code for a
    /// stored language the detector no longer lists, so it can still be removed.
    func name(of code: String) -> String {
        available.first { $0.code == code }?.name ?? code.uppercased()
    }

    /// The add menu: everything known and not already chosen, in the daemon's
    /// (alphabetical) order.
    var addable: [TranslateLanguage] {
        available.filter { !read_languages.contains($0.code) }
    }
}

/// POST /client/translate-config. Replaces the whole set; the daemon drops
/// unknown codes and "en".
struct TranslateConfigBody: Codable, Sendable {
    var read_languages: [String]
}

/// What the settings card knows about the translate config. Not a wire type,
/// but kept beside it so the read's failure rule is testable without the app.
enum TranslateConfigState: Sendable, Equatable {
    /// Not asked yet, or asked and still waiting.
    case unknown
    /// The daemon predates the setting (404): every language is translated
    /// and there is nothing to choose.
    case notOffered
    /// The read failed for any other reason. Must not look like `notOffered`.
    case unreadable
    case loaded(TranslateConfig)

    /// What a failed read leaves showing. A 404 always wins (the daemon was
    /// replaced by an older one); anything else keeps a set already shown,
    /// because the stored set did not change when the network blinked.
    func failed(notFound: Bool) -> TranslateConfigState {
        if notFound { return .notOffered }
        if case .loaded = self { return self }
        return .unreadable
    }
}

/// What an agent connected over Agent MCP may WRITE to this mailbox. The
/// owner's setting, held by the daemon per mailbox and checked there on every
/// agent call; this app only shows and sets it. Each level includes the ones
/// before it.
enum AgentWriteAccess: String, Codable, Sendable, CaseIterable, Identifiable {
    case none, drafts, send

    var id: String { rawValue }

    var title: String {
        switch self {
        case .none: "Read only"
        case .drafts: "Drafts only"
        case .send: "Send, with your approval"
        }
    }

    var detail: String {
        switch self {
        case .none: "Your agent reads and reports. It cannot write mail."
        case .drafts: "Your agent can leave drafts here. You read, edit and send them yourself."
        case .send:
            "Your agent can send from your address. It is told to show you each message and wait for your go-ahead first."
        }
    }

    var symbol: String {
        switch self {
        case .none: "eye"
        case .drafts: "square.and.pencil"
        case .send: "paperplane"
        }
    }
}

/// GET / PUT /client/settings/agent-access. `write_access` is a plain string
/// on the wire so a level this build does not know (a newer daemon's) decodes
/// rather than failing the whole card; `level` reads it, and an unknown one is
/// nil, which the pane shows as nothing selected.
struct AgentAccessConfig: Codable, Sendable, Hashable {
    var write_access: String

    var level: AgentWriteAccess? { AgentWriteAccess(rawValue: write_access) }
}

struct AgentAccessBody: Codable, Sendable {
    var write_access: String
}

/// GET /client/contacts — one recipient-autocomplete hit, ranked server-side
/// (prefix > substring, then frequency and recency).
struct ContactHit: Codable, Sendable, Equatable, Identifiable {
    var addr: String
    var display_name: String?
    var sent_count: Int
    var last_sent_at: String?

    var id: String { addr }
}

/// One answer to "who is steven" (GET /client/calendar/people): somebody the
/// user has written to whose name starts that way, most recent exchange
/// first. The event pane offers the first as a guest; it never adds one.
struct NamedPerson: Codable, Sendable, Hashable {
    var addr: String
    var name: String?
}

/// The route's envelope. Lenient: a daemon that answers without the list
/// offers nobody.
struct NamedPeople: Codable, Sendable, Equatable {
    var people: [NamedPerson]

    init(people: [NamedPerson] = []) { self.people = people }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        people = (try? c.decodeIfPresent([NamedPerson].self, forKey: .people)) ?? []
    }
}

/// One row of the daemon's sender directory (GET /client/senders): somebody who
/// has written to this account, for the search field's `from:` menu. The other
/// direction of mail from `ContactHit`, which is who the user writes TO.
struct SenderHit: Codable, Sendable, Equatable, Identifiable {
    var addr: String
    var display_name: String?
    var msg_count: Int
    var last_received_at: String

    var id: String { addr }
}

/// One row of the sender directory rolled up by host (GET
/// /client/sender-domains): everyone at one domain who has written to this
/// account, for the rule editor's `*@` suggestions. The host is exactly what
/// the addresses carry, subdomain and all, because that is what a rule's glob
/// has to match.
struct SenderDomainHit: Codable, Sendable, Equatable, Identifiable {
    var domain: String
    /// How many addresses at the host. nil when the row was rolled up on this
    /// side from a page of senders, which cannot know.
    var sender_count: Int?
    var msg_count: Int
    var last_received_at: String?

    var id: String { domain }
}

/// GET /client/person?addr= — the reader's contact card: everything the daemon
/// knows about one address. The optional parts are ABSENT when there is
/// nothing to say (no mail from them, never written to, no rule), and sealed
/// mail is in no count and no list on it.
struct PersonCard: Codable, Sendable, Equatable {
    struct Received: Codable, Sendable, Equatable {
        var count: Int
        var first_at: String
        var last_at: String
    }
    struct Sent: Codable, Sendable, Equatable {
        var count: Int
        var last_at: String?
    }
    struct Rule: Codable, Sendable, Equatable {
        var id: Int
        var match_pattern: String
        var want_text: String
        var disposition: Disposition
    }
    struct Group: Codable, Sendable, Equatable, Identifiable {
        var id: Int
        var name: String
    }
    struct RecentThread: Codable, Sendable, Equatable, Identifiable {
        var thread_id: String
        var subject: String
        var last_at: String
        var id: String { thread_id }
    }

    var addr: String
    var display_name: String?
    var is_self: Bool
    var known_contact: Bool
    var received: Received?
    var sent: Sent?
    var rule: Rule?
    var groups: [Group]
    var recent_threads: [RecentThread]
    /// Where the daemon thinks they are, or where the user said. Absent when
    /// it has no idea, and on a daemon older than the inference.
    var timezone: Timezone?

    /// Only the zone's name is read here: the event pane wants the hour of
    /// an event that may be months off, and the name is what knows that
    /// date's offset. Never throws, so a shape this client does not know
    /// costs the card nothing.
    struct Timezone: Codable, Sendable, Equatable {
        var zone: String?

        init(zone: String?) { self.zone = zone }

        init(from decoder: Decoder) throws {
            let c = try? decoder.container(keyedBy: CodingKeys.self)
            zone = c.flatMap { try? $0.decodeIfPresent(String.self, forKey: .zone) }
        }
    }
}

struct StatusResult: Codable, Sendable {
    var status: String
    var message_id: Int?
}

struct SendResult: Codable, Sendable {
    /// `"sent"` for an ordinary send; `"sending"` for a fan-out, which has left
    /// with a batch to watch rather than a message to open.
    var status: String
    /// The `group_sends` row a fan-out started. Present only on `"sending"`.
    var group_send_id: Int?
    /// How many people that fan-out is going to.
    var recipients: Int?
    /// The sent copy as it landed in the local store, and its thread — both null
    /// when the send succeeded but the echo has not been ingested yet.
    var echo_message_id: Int?
    var thread_id: String?
}
struct RefreshResult: Codable, Sendable { var triggered: Bool }
struct RetriageResult: Codable, Sendable { var reset: Int }
struct UnsubResolutionResult: Codable, Sendable {
    var sender: String
    var resolution: String
}

// MARK: - drafts

/// One saved draft. LOCAL and human-door only: never a Gmail draft, never
/// synced, never reachable from the agent door. `to` is the send endpoint's own
/// spelling of the field, not the store's `to_addr`.
struct DraftView: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    /// The message this answers. nil on a forward and on plain new mail, which
    /// on a `drafts_v2` daemon is one of MANY rows told apart by `id` (an older
    /// daemon still holds at most one).
    var reply_to_message_id: Int?
    /// The message this passes on. Mutually exclusive with
    /// `reply_to_message_id`; both nil is a plain new message.
    ///
    /// THIS AND THE FOUR BELOW ARE OPTIONAL for the reason `cc` is: an older
    /// daemon sends no such keys, and a decode that failed over them would
    /// lose every draft rather than the context it could not describe.
    var forward_of_message_id: Int?
    /// The parent's thread, so the picker can open a reply draft where it
    /// belongs. nil for plain new mail, and for a parent no longer stored.
    var thread_id: String?
    /// The parent's subject and sender, for the picker's `re:` / `fwd:` line.
    /// Display only; nothing is sent from them.
    var parent_subject: String?
    var parent_from: String?
    var parent_from_addr: String?
    var to: String
    /// The other two lists. A draft that could not hold these would silently
    /// lose the audience of a bcc send. OPTIONAL here for exactly one reason:
    /// an older daemon sends neither field, and a decode that failed over it
    /// would lose the whole draft rather than the two lists it could not carry.
    var cc: String?
    var bcc: String?
    var subject: String
    var body: String
    var created_at: String
    var updated_at: String
    /// The files the draft holds. OPTIONAL for the reason `cc` is: an older
    /// daemon sends no key, and a restore must not fail over it.
    var attachments: [OutboundAttachment]?
    /// An agent wrote this draft over Agent MCP, rather than the owner. Kept
    /// through the owner's own edits. nil from a daemon older than the flag.
    var from_agent: Bool? = nil
}

/// PUT /client/drafts — upsert keyed on `reply_to_message_id`. Every text field
/// is optional server-side (a half-composed draft is the normal case), but the
/// composer always knows all three, so all three go.
/// One file staged for a send, as the daemon describes it: everything but the
/// bytes. `content_id` is the `cid:` token the body references it by.
struct OutboundAttachment: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var filename: String
    var mime: String
    var size: Int
    var content_id: String
}

struct DraftBody: Codable, Sendable {
    var reply_to_message_id: Int?
    /// The forward key, one row per forwarded message like replies. nil is
    /// OMITTED from the JSON (synthesized Codable skips a nil optional), so an
    /// older daemon never sees the key at all.
    var forward_of_message_id: Int?
    var to: String
    /// Sent unconditionally, `""` and all: unlike the SEND route, absence has
    /// no second meaning here — nothing is ever derived into a draft — and a
    /// composer that cleared its Bcc has to be able to say so.
    var cc: String
    var bcc: String
    var subject: String
    var body: String
    /// The staged files this draft holds — EXACTLY these, so a file taken out
    /// of the tray is released by the next save. Always sent, `[]` included.
    var attachment_ids: [Int]
}

/// POST /client/drafts and PUT /client/drafts/{id}: a plain new-message draft,
/// named by the ROUTE (create, or this id) rather than by a key in the body.
/// Same field rules as `DraftBody`: everything always sent, `""` and `[]`
/// included, so an emptied field is said rather than left stale.
struct NewDraftBody: Codable, Sendable {
    var to: String
    var cc: String
    var bcc: String
    var subject: String
    var body: String
    var attachment_ids: [Int]
}

// MARK: - scheduled send (the outbox)

/// POST /client/scheduled: EXACTLY the send route's body, plus when. Encoded
/// flat, as one JSON object, because the daemon reads it as its own `SendBody`
/// with one extra key; nesting the send would be a second shape to keep in
/// step with the first.
struct ScheduleBody: Encodable, Sendable {
    var send: SendBody
    /// RFC3339. The daemon accepts any offset and refuses the past and
    /// anything more than a year out (400).
    var send_at: String

    private enum Keys: String, CodingKey { case send_at }

    func encode(to encoder: Encoder) throws {
        try send.encode(to: encoder)
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(send_at, forKey: .send_at)
    }
}

/// 201 from POST /client/scheduled.
struct ScheduledCreated: Codable, Sendable, Hashable {
    var id: Int
    var send_at: String
}

/// The frozen send body an outbox row holds, as a cancel hands it back. EVERY
/// key optional: the daemon serializes its `SendBody` with absent options
/// skipped, and a cancel of a row whose request no longer parses answers
/// `null` for the whole thing (see `ScheduledCancel.request`). No `draft_id`
/// (scheduling consumed the draft) and no `send_at` (the row's column, not
/// the request's).
struct ScheduledRequest: Codable, Sendable, Hashable {
    var reply_to_message_id: Int?
    var forward_of_message_id: Int?
    var to: String?
    /// PRESENT, `""` included, means the composer had stated its copy list;
    /// absent means the daemon derives it. Same rule as `SendBody.cc`.
    var cc: String?
    var bcc: String?
    var group_id: Int?
    var subject: String?
    var body: String?
    var body_format: String?
    var confirm: Bool?
    var override_guard: Bool?
    var include_tracker: Bool?
    var reply_all: Bool?
    var attachment_ids: [Int]?
}

/// 200 from DELETE /client/scheduled/{id}: the row is `cancelled`, and this is
/// everything needed to put it back in a composer. `attachments` are the files
/// it held, now UNCLAIMED on the daemon: the reopened composer's next autosave
/// claims them again, well inside the unclaimed sweep's window.
struct ScheduledCancel: Codable, Sendable {
    var status: String
    var id: Int
    /// nil when the daemon could not parse its own frozen request (the worker
    /// fails such a row as `failed:request`). There is nothing to reopen then.
    var request: ScheduledRequest?
    var attachments: [OutboundAttachment]
}

/// 202 from POST /client/scheduled/{id}/now: queued for the worker's next
/// tick, NOT sent. The listing shows it go.
struct ScheduledNow: Codable, Sendable {
    var status: String
    var id: Int
}

/// One row of GET /client/scheduled: a mail still owed an outcome or the
/// user's attention, oldest `send_at` first. Settled rows (`sent`,
/// `cancelled`) are never listed.
struct ScheduledSendView: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var send_at: String
    /// `scheduled`, `sending` or `failed`.
    var state: String
    /// Claims so far. A `scheduled` row with attempts > 0 is waiting out a
    /// retry, and `last_error` says why.
    var attempts: Int
    /// A redacted reason (`failed:gmail`, `rejected:no_write_credential`, ...).
    /// Never content.
    var last_error: String?
    /// STICKY: an earlier attempt may have reached Gmail, whatever
    /// `last_error` says. Absent from a daemon older than the flag (nil).
    var maybe_sent: Bool? = nil
    /// `""` when the request had none (a reply's audience derives at fire
    /// time); never absent, like a draft's.
    var to: String
    /// The frozen request's copy lists, `""` when it had none. nil from a
    /// daemon older than these fields, which the preview reads as "not said".
    var cc: String? = nil
    var bcc: String? = nil
    /// `""` when none was typed; `parent_subject` is what a row can show then.
    var subject: String
    var body: String
    var reply_to_message_id: Int?
    var forward_of_message_id: Int?
    /// Upload order. Always present, `[]` when none.
    var attachments: [OutboundAttachment]
    /// Parent context for a reply or a forward, exactly as `DraftView` carries
    /// it. Null for new mail.
    var thread_id: String?
    var parent_subject: String?
    var parent_from: String?
    var parent_from_addr: String?
    var created_at: String
    var updated_at: String
}

// MARK: - triage debug / shredder / feedback / marketing

struct TriageDebug: Codable, Sendable, Hashable {
    var message_id: Int
    var subject: String
    var importance: Int
    var tier: String
    var category: String?
    var one_line: String
    var reason: String
    var field_reasons: FieldReasons?
    var deadline: String?
    var matched_rule_id: Int?
    var status: String
    var surfaced_at: String?
    var resolved_at: String?
    var stage1_model_used: String?
    var model_used: String?
    var needs_stage2: Bool
    var extractor_model_used: String?
    var created_at: String
    /// Optional so a client pointed at a daemon older than the field still
    /// decodes the page rather than failing the whole read.
    var thread_id: String?
}

/// What `GET /client/retriage` answers: how far the live dev re-triage has got.
/// `total == 0` means no run is in flight. A daemon too old to serve the route
/// 404s, which is NOT zero — the modal has to say "no counter" rather than
/// "finished". See `RetriageRun`.
struct RetriageProgress: Codable, Sendable, Hashable {
    var total: Int
    var done: Int
    /// The oldest live stamp: when the counted run began. Absent exactly when
    /// `total` is 0.
    var started_at: String?
    /// Jobs of the run the daemon has deferred because a daily triage budget
    /// ran out, and the earliest time one of them runs again. Optional because
    /// an older daemon does not send them; absent reads as "not paused".
    var budget_parked: Int?
    var budget_resumes_at: String?
    /// Why an unfinished run is not moving, when something outside the queue
    /// holds it. Absent on an older daemon and whenever the run can progress.
    var blocked: RetriageBlocked?
}

struct RetriageBlocked: Codable, Sendable, Hashable {
    /// `waiting_for_sync`, `provider_cooldown` or `budget`. A String and not an
    /// enum so a reason a newer daemon adds decodes instead of failing the poll.
    var reason: String
    /// RFC3339, when the blocker is expected to lift, if anyone knows.
    var until: String?
}

struct ShredStats: Codable, Sendable, Hashable {
    var enabled: Bool
    var after_days: Int
    var shredded_recent: Int
    var shredded_total: Int
    var last_shredded_at: String?
    var pending: Int
    var write_ready: Bool
}

struct ShredderPatch: Codable, Sendable {
    var enabled: Bool?
    var after_days: Int?
}

struct ShredderRunResult: Codable, Sendable {
    var shredded: Int
    var stats: ShredStats
}

struct TriageFeedback: Codable, Sendable, Identifiable, Hashable {
    var id: Int
    var message_id: Int
    var corrected_at: String
    var dimension: String
    var from_value: String?
    var to_value: String
    var sender: String
    var subject: String
    var note: String?
}

/// store::MarketingOffer (GET /client/marketing). Deliberately carries NO url —
/// a model-emitted, email-derived link rendered as clickable is a
/// prompt-injection lever.
struct MarketingOffer: Codable, Sendable, Hashable {
    var message_id: Int
    var thread_id: String
    var sender: String
    var subject: String
    var brand: String?
    var offer: String?
    var discount: String?
    var code: String?
    var expires_at: String?
    var received_at: String
}

// MARK: - sent

/// One row of GET /client/sent — mail the user WROTE, newest first (the daemon
/// orders on received_at DESC, id DESC and the page renders that order as-is).
///
/// A deliberately different shape from `AttentionUpdate`: outbound mail has no
/// tier, no importance and no triage status, because nothing triaged it. What it
/// has instead is `to` (the display recipient list, comma-joined "Name <addr>",
/// EMPTY when the header carried nothing usable) and `opens` — the count of
/// recorded read receipts, which is 0 both for a send nobody opened and for a
/// send that never armed the pixel. Those two are the same silence by design;
/// see ReadReceipt.swift.
struct SentItem: Codable, Sendable, Identifiable, Hashable {
    /// The local message id, so a row keys and prefetches like any other.
    var id: Int
    var thread_id: String
    var to: String
    var subject: String
    var snippet: String
    var sent_at: String
    var opens: Int
}

// MARK: - query params

struct UpdatesParams: Sendable {
    var since: String?
    var min_importance: Int?
    var tier: Tier?
    var status: AttentionStatus?
    var band: Band?
    var limit: Int?
    var cursor: String?
    /// Keyset paging: rows that arrived before this stamp (and, at that
    /// instant, below `beforeId`). Exclusive with `cursor`.
    var before: String? = nil
    var beforeId: Int? = nil
    /// Narrow to rows with a PENDING reminder, soonest first. Named for the
    /// flag rather than the query value (`reminders=pending`) because unlike
    /// every field above it, it is a filter with its own ORDER: the server
    /// sorts these by remind_at, not by arrival, and it serves done rows —
    /// which every other read here would have excluded.
    var remindersPending: Bool
    /// SWAP THE LISTING OVER TO THE PROVIDER'S SPAM FOLDER (`spam=only`). Not a
    /// tier and not a band: spam is Gmail's verdict, nothing in Passband
    /// triaged these rows, and every other read on this route excludes them.
    /// A flag rather than an enum on this side because there are only two
    /// states a client can ask for and the daemon rejects anything else.
    var spamOnly: Bool

    init(
        since: String? = nil,
        min_importance: Int? = nil,
        tier: Tier? = nil,
        status: AttentionStatus? = nil,
        band: Band? = nil,
        limit: Int? = nil,
        cursor: String? = nil,
        remindersPending: Bool = false,
        spamOnly: Bool = false
    ) {
        self.since = since
        self.min_importance = min_importance
        self.tier = tier
        self.status = status
        self.band = band
        self.limit = limit
        self.cursor = cursor
        self.remindersPending = remindersPending
        self.spamOnly = spamOnly
    }
}

// MARK: - Agent triage v2

/// Connection capability negotiation deliberately has no legacy triage fallback.
struct TriageCapabilities: Decodable, Sendable {
    var triage_version: Int
    var server_ranked_fye: Bool
    var reading: Bool
    var pending_message_read: Bool

    var isSupported: Bool {
        triage_version == 2 && server_ranked_fye && reading && pending_message_read
    }
}

/// The server owns membership and order. These fields are presentation facts;
/// clients must never reconstruct placement from importance or email kind.
struct AgentFeed: Codable, Sendable {
    var total_count: Int?
    var version: Int
    var ranked_at: String
    var items: [AgentFeedItem]
}

struct AgentFeedItem: Codable, Sendable {
    var message_id: Int
    var thread_id: String
    var from_addr: String
    /// The `From:` display name. ABSENT from a daemon older than the feed's
    /// own `from_name`, and then the card names the sender from the address,
    /// which is how a mailer with a hashed local-part came to be titled with
    /// 32 hex characters.
    var from_name: String?
    var subject: String
    var received_at: String
    var decision: AgentMessageDecision
    var attention: AgentAttention
    var score: Double

    /// Adapter for shared row/reader controls while those controls still use
    /// AttentionUpdate. The neutral legacy tier is never used for membership.
    var row: AttentionUpdate {
        var row = AttentionUpdate(
            id: message_id, thread_id: thread_id, tier: .signal,
            importance: Int((attention.factors.importance * 100).rounded()),
            sender: from_addr,
            one_line: attention.summary.isEmpty ? decision.summary : attention.summary,
            reason: decision.reason,
            deadline: attention.factors.attention_at?.value,
            from_name: from_name,
            subject: subject, status: .new, surfaced_at: received_at)
        row.auth_kinds = decision.auth?.kinds
        row.auth_critical = decision.auth?.critical
        return row
    }

    var readingRow: AttentionUpdate {
        var result = row
        result.one_line = decision.summary
        return result
    }

    /// A Reading piece meant to be read for its own sake (article, essay, post)
    /// rather than a promotion. Decisions stored before `reading_kind` existed
    /// fall back to the kinds triage already gave them: editorial and not
    /// promotional.
    var isReadingContent: Bool {
        if let kind = decision.reading_kind { return kind == "content" }
        return decision.kinds.contains("editorial") && !decision.kinds.contains("promotional")
    }
}

struct AgentMessageDecision: Codable, Sendable {
    var kinds: [String]
    var destinations: [String]
    /// "content" or "promotion" on Reading mail. ABSENT from an older daemon
    /// and from decisions stored before it existed.
    var reading_kind: String?
    var summary: String
    var reason: String
    var records: [AgentRecordProposal]?
    /// `auth`: which auth kinds triage found. Optional throughout so a
    /// decision shaped any other way still decodes.
    var auth: AgentAuthAssessment? = nil
}

/// core::triage::decision::AuthAssessment, the two fields a client reads.
/// Kinds stay raw strings (`sign_in_link`, not the sealed `magic_link`).
struct AgentAuthAssessment: Codable, Sendable {
    var kinds: [String]?
    var critical: Bool?

    init(kinds: [String]? = nil, critical: Bool? = nil) {
        self.kinds = kinds
        self.critical = critical
    }

    /// Field by field and forgiving: a feed whose consumer cannot decode one
    /// row loses the whole page, and this block is presentation, not membership.
    init(from decoder: Decoder) throws {
        guard let c = try? decoder.container(keyedBy: CodingKeys.self) else {
            kinds = nil
            critical = nil
            return
        }
        kinds = (try? c.decodeIfPresent([String].self, forKey: .kinds)) ?? nil
        critical = (try? c.decodeIfPresent(Bool.self, forKey: .critical)) ?? nil
    }
}

/// Presentation consumes explicit record facts; it never infers kinds from copy.
struct AgentRecordProposal: Codable, Sendable {
    var kind: String
    var merchant: String?
    var amount: Double?
    var currency: String?
    var title: String?
    var start: AgentSupportedTime?
    var institution: String?
    var description: String?
    var due: AgentSupportedTime?
    var autopay: Bool?
}

extension AgentFeed {
    var receipts: [Receipt] {
        items.flatMap { item in
            (item.decision.records ?? []).enumerated().compactMap { index, record in
                guard record.kind == "receipt" else { return nil }
                return Receipt(id: item.message_id * 1000 + index, account_id: 0,
                    message_id: item.message_id, thread_id: item.thread_id,
                    from_addr: item.from_addr, from_name: record.merchant,
                    amount: record.amount, currency: record.currency, received_at: item.received_at)
            }
        }
    }

    var calendar: [CalendarUpdate] {
        items.flatMap { item in
            (item.decision.records ?? []).enumerated().compactMap { index, record in
                guard record.kind == "event" else { return nil }
                return CalendarUpdate(id: item.message_id * 1000 + index,
                    message_id: item.message_id, thread_id: item.thread_id, kind: .event,
                    event_title: record.title, starts_at: record.start?.value,
                    organizer: nil, received_at: item.received_at,
                    start_timezone: record.start?.timezone)
            }
        }
    }

    var banking: [BankingRecord] {
        items.flatMap { item in
            (item.decision.records ?? []).enumerated().compactMap { index, record in
                guard record.kind == "financial_update" || record.kind == "bill" else { return nil }
                return BankingRecord(id: item.message_id * 1000 + index,
                    message_id: item.message_id, thread_id: item.thread_id,
                    from_addr: item.from_addr, kind: record.kind == "bill" ? .bill : .update,
                    institution: record.kind == "bill" ? record.merchant : record.institution,
                    amount: record.amount, currency: record.currency, account_hint: nil,
                    received_at: item.received_at, due: record.due?.value, autopay: record.autopay)
            }
        }
    }

    var marketing: [MarketingOffer] {
        items.filter { $0.decision.kinds.contains("promotional") }.map { item in
            MarketingOffer(message_id: item.message_id, thread_id: item.thread_id,
                sender: item.from_addr, subject: item.subject, brand: nil,
                offer: item.decision.summary, discount: nil, code: nil, expires_at: nil,
                received_at: item.received_at)
        }
    }
}
struct AgentAttention: Codable, Sendable {
    var show_in_fye: Bool
    var state: String
    var summary: String
    var factors: AgentAttentionFactors
}
struct AgentAttentionFactors: Codable, Sendable {
    var urgency: Double
    var action_need: Double
    var personal_relevance: Double
    var importance: Double
    var attention_at: AgentSupportedTime?
}
struct AgentSupportedTime: Codable, Sendable {
    var value: String
    var timezone: String?
}
struct HumanMessageEnvelope: Codable, Sendable {
    var message_id: Int
    var thread: ClientThreadView
}


struct AgentTriageInspection: Decodable, Sendable {
    var decision: AgentMessageDecision?

    private enum CodingKeys: String, CodingKey { case decision }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        // An explicit null means pending. A missing key is an incompatible
        // response, never permission to overwrite an unknown placement list.
        decision = try values.decode(AgentMessageDecision?.self, forKey: .decision)
    }
}
