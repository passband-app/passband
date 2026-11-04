-- squelch local store schema. Multi-tenant shaped: every account-owned row
-- carries account_id. Applied on open (idempotent).

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS accounts (
    id         INTEGER PRIMARY KEY,
    email      TEXT UNIQUE NOT NULL,
    created_at TEXT NOT NULL,
    -- Which kind of mailbox this is (`crate::mail::MailProvider`): the sync
    -- engine and the write door pick their provider from it. Every account
    -- before providers existed was Gmail, which is what the default says.
    provider   TEXT NOT NULL DEFAULT 'gmail'
);

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    gmail_msg_id TEXT NOT NULL,
    thread_id   TEXT NOT NULL,
    from_addr   TEXT NOT NULL,
    from_name   TEXT,
    subject     TEXT NOT NULL,
    received_at TEXT NOT NULL,
    snippet     TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    -- Sanitized HTML body baked at ingest (docs/SECURITY.md §1), NULL for
    -- plain-text-only mail. Served ONLY through the human door; never crosses
    -- /mcp, which flattens to `body` text. Sealed mail stores it like `body` —
    -- storage is fine, serving is guarded.
    body_html   TEXT,
    is_sent     INTEGER NOT NULL DEFAULT 0,
    -- DISPLAY RECIPIENTS, one column per header: the mailboxes as the headers
    -- spelled them, comma-joined `Name <addr>` (bare addr with no display
    -- name). Empty string means "looked, and the headers named nobody"; NULL
    -- means NOBODY HAS LOOKED YET, which is what the one-shot recipients
    -- backfill queue reads (`cc_addrs IS NULL`, the column every filled row
    -- gains together with the other two).
    --
    -- ON EVERY MESSAGE, not just sent mail: a reader that shows who a message
    -- was from and not who else it went to cannot tell a note from a broadcast,
    -- and "was I the only one asked?" is the question a thread is opened with.
    -- Both are served through the human door only — the agent door's
    -- `SanitizedMessage` carries no recipients at all (docs/SECURITY.md), so a
    -- display name written by a stranger never reaches /mcp.
    to_addrs    TEXT,
    cc_addrs    TEXT,
    -- BCC IS SENT MAIL'S ALONE, and structurally so: `set_message_recipients`
    -- writes NULL here for anything with `is_sent = 0`, whatever a caller
    -- passes. Gmail strips the header on delivery and keeps it on the sender's
    -- own copy, so on the user's own mail this is the blind list they chose; on
    -- received mail a `Bcc:` header is at best the sender's leak and at worst
    -- forged, and in neither case is it "who this went to".
    bcc_addrs   TEXT,
    -- Raw `List-Unsubscribe` header value, NULL when absent. Consumed ONLY by
    -- the human door's unsubscribe endpoint.
    list_unsubscribe TEXT,
    -- 1 when the mail advertised RFC 8058 one-click unsubscribe
    -- (`List-Unsubscribe-Post: List-Unsubscribe=One-Click`).
    list_unsub_one_click INTEGER NOT NULL DEFAULT 0,
    -- EMAIL-AUTHENTICATION VERDICT, read at ingest from the TOPMOST
    -- `Authentication-Results` header (the one Gmail itself wrote): 1 = DMARC
    -- passed or an aligned DKIM signature did, 0 = Gmail evaluated it and
    -- neither held, NULL = never evaluated (no Gmail header, or the row was
    -- ingested before this column existed).
    --
    -- NULL MUST READ AS "NO": every consumer tests `= 1` (or `Some(true)`),
    -- so absence of proof withholds what the verdict would grant. It gates:
    --   * the human door's PERMISSIVE known-sender tracking-pixel bypass;
    --   * calendar ghosts (docs/CALENDAR.md §3.2): only an authenticated
    --     record has a sender scope, so only it can carry identity keys, join
    --     or edit a ghost, or mark one committed;
    --   * correspondent timezones (§7): only authenticated mail's Date offset
    --     counts, and an invitation is recorded only from authenticated mail
    --     bound to its organizer.
    -- So the residual gap `sync::ingest::extract_auth_pass` describes (a
    -- verdict forged on an ingestion path where Gmail prepends nothing)
    -- reaches all three, not only the pixel. Nothing is backfilled: any
    -- re-sync refills the column through the message upsert.
    auth_pass   INTEGER,
    -- WHERE FROM AND THE REAL SENDER DISAGREE (#639): compact JSON of
    -- `types::SenderSignals` (sender, via, reply_to, envelope_domain,
    -- name_spoof). '{}' = read, nothing disagreed; NULL = never read (sent
    -- mail, rows ingested before the column). Facts for the reader and
    -- triage, never a gate. Only `via` claims authentication, and it is read
    -- through the same trust gate as `auth_pass`.
    sender_signals TEXT,
    -- THE PROVIDER'S SPAM VERDICT, not ours: 1 when this message was fetched
    -- from Gmail's SPAM label, 0 otherwise. Gmail sorted it; nothing in squelch
    -- did, and no LLM ever looks at it (the ingest branch stamps a neutral row
    -- and the 'n/a' stage markers, exactly as it does for sent mail).
    --
    -- IT IS A STRUCTURAL EXCLUSION, NOT A TIER. Tier has no `spam` variant for
    -- the same reason it has no `sealed` one: these rows must be absent from
    -- every band, queue, count and search rather than ranked last within them,
    -- so every listing pairs `is_spam = 0` with its `is_sent = 0`. The ONE
    -- surface that passes 1 is the human door's spam page.
    --
    -- STICKY TO 0 on conflict (`MIN`), like `is_sent` and for the same reason: a
    -- message ever seen outside the spam label can never be hidden by a later
    -- spam sighting. Nothing moves a row the other way except the user's own
    -- "not spam", which clears it here and in Gmail together.
    is_spam     INTEGER NOT NULL DEFAULT 0,
    -- THE `Date` HEADER'S UTC OFFSET in minutes, east positive (-240 for New
    -- York in summer, 330 for India), as the sender's clock stamped it.
    -- `received_at` is normalized to UTC, so this is the only place the offset
    -- survives. NULL when the header did not parse, and on every row ingested
    -- before the column existed (the raw mail is not kept, so nothing can
    -- backfill it locally). Zero is stored as zero; the timezone inference is
    -- what ignores it. The observation lives ON THE ROW so a re-ingest cannot
    -- count it twice: the per-sender histogram is always a query
    -- (`sqlite::zones`).
    date_offset_minutes INTEGER,
    -- THE ARCHIVE TIER: 1 when the row was written by the archive walk, mail
    -- older than the backfill window that exists to be FOUND (search, the
    -- reader, the Sent page, a sender's history) and for nothing else. No model
    -- reads it at ingest and nothing may spend on it unprompted: it is absent
    -- from every band, count, inventory and queue, and from the triage agent's
    -- evidence until an explicit agent-door surfacing has had its access
    -- assessed.
    --
    -- WRITTEN ONCE, AT INSERT, AND NEVER CHANGED. Unlike `is_sent` and
    -- `is_spam` there is no conflict rule, because there is no conflict: an
    -- archive ingest of an id already stored is a no-op, and so is a live
    -- sighting of an archive row (`SqliteStore::ingest_message_inner`).
    is_archive  INTEGER NOT NULL DEFAULT 0,
    UNIQUE(account_id, gmail_msg_id)
);

CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(account_id, thread_id);
-- Thread reads need the earliest message as well as the ordered conversation.
-- Without this index, ORDER BY received_at LIMIT 1 can choose the account's
-- date index and walk unrelated messages while holding the shared store mutex.
CREATE INDEX IF NOT EXISTS idx_messages_thread_received
    ON messages(account_id, thread_id, received_at);
CREATE INDEX IF NOT EXISTS idx_messages_received ON messages(account_id, received_at);
-- Sender lookups: the escalation context assembles one sender's track record per
-- queued row, inside a per-row loop that runs every sync tick. Unindexed, that is
-- a full scan of `messages JOIN triage` per escalated row, taken while holding
-- the store's single connection mutex.
CREATE INDEX IF NOT EXISTS idx_messages_from ON messages(account_id, from_addr);
-- NOCASE twin of the sender index, for the per-sender reads that compare
-- `from_addr = ? COLLATE NOCASE` (ingest keeps the header's spelling, so the
-- column is not normalized). The BINARY index above cannot serve that
-- comparison; see `idx_contacts_addr_nocase` for what the walk cost.
CREATE INDEX IF NOT EXISTS idx_messages_from_nocase
    ON messages(account_id, from_addr COLLATE NOCASE);

CREATE TABLE IF NOT EXISTS contacts (
    account_id INTEGER NOT NULL,
    addr       TEXT NOT NULL,
    sent_count INTEGER NOT NULL DEFAULT 0,
    first_seen TEXT NOT NULL,
    -- Recency + pretty-name columns for recipient autocomplete. last_sent_at is
    -- RFC3339 UTC (lexicographically ordered); display_name comes from the Sent
    -- harvest's To/Cc mailbox names and may lag for post-harvest contacts.
    last_sent_at TEXT,
    display_name TEXT,
    PRIMARY KEY(account_id, addr)
);
-- THE LOOKUP INDEX. Every reader compares `addr` under COLLATE NOCASE (the
-- Sent-history harvest lowercases, the per-message Sent ingest keeps the
-- header's spelling, so neither side can be assumed normalized), and SQLite
-- uses an index only when its collation MATCHES the comparison's. The primary
-- key above is BINARY, so `addr = ?2 COLLATE NOCASE` walked every contact of
-- the account instead of seeking one. In the standing band that walk is
-- CORRELATED, once per message in the 30-day window: on a thousand-message
-- store the band cost 140ms and the header's `stats()` 300ms, per client, every
-- 10s, under the store mutex both doors and the sync loop wait on; a bigger
-- mailbox paid seconds, and that queue was the whole p95. With this index both
-- read in ~2ms. Measured 2026-08-27; tests/attention.rs pins the seek.
CREATE INDEX IF NOT EXISTS idx_contacts_addr_nocase
    ON contacts(account_id, addr COLLATE NOCASE);

-- THE SENDER DIRECTORY: one row per address that has written to this account,
-- for the search field's `from:` autocomplete (`/client/senders`). The twin of
-- `contacts` over the other direction of mail: contacts are the people the user
-- writes TO, seeded from Sent recipients, and cannot answer `from:` for the Dan
-- who has only ever replied.
--
-- MAINTAINED AT INGEST (`senders::bump_sender_conn`, from the message upsert
-- and the not-spam action), not computed per keystroke: a GROUP BY over
-- `messages` with a substring LIKE on two columns measured 110-160 ms at 100k
-- rows, taken while holding the store's single connection mutex. `msg_count`
-- is RECOMPUTED from `messages` on each bump rather than incremented, so a
-- re-sighting of the same gmail_msg_id cannot double count, and it follows the
-- same `is_sent = 0 AND is_spam = 0` rule every listing does.
--
-- SEALED IS NOT KNOWN HERE. Sensitivity is triage's verdict, after ingest, so
-- a sender whose only mail is sealed still has a row; `search_senders` requires
-- at least one non-sealed, non-spam inbound message before offering one. The
-- primary key is the lookup: the reader's LIKE cannot use an index anyway, and
-- a mailbox has thousands of senders, not millions.
CREATE TABLE IF NOT EXISTS senders (
    account_id       INTEGER NOT NULL,
    addr             TEXT NOT NULL,
    -- The newest display name seen on this sender's mail, NULL if never one.
    display_name     TEXT,
    msg_count        INTEGER NOT NULL DEFAULT 0,
    -- Both RFC3339 UTC, so MAX() and ORDER BY read them as time.
    first_seen       TEXT NOT NULL,
    last_received_at TEXT NOT NULL,
    PRIMARY KEY(account_id, addr)
);

CREATE TABLE IF NOT EXISTS sender_rules (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL,
    match_pattern TEXT NOT NULL,
    want_text     TEXT NOT NULL,
    disposition   TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE(account_id, match_pattern)
);

-- ATTENTION LIFECYCLE (sitrep seen-ledger):
--   status       'new' | 'open' | 'done'. A row starts 'new'; the first time it
--                flows OUT through ANY read door it is promoted to 'open' and
--                stamped surfaced_at. An archive/send/dismiss sets 'done' +
--                resolved_at.
--   surfaced_at  first time any door surfaced this row, NULL until then.
--   resolved_at  when the row reached status='done'.
--   remind_at    a PENDING reminder (see the column comments below).
--   reminded_at  a reminder that already fired.
-- Sealed rows carry these columns but are structurally absent from every
-- non-local surface, so they are never surfaced or stamped.
CREATE TABLE IF NOT EXISTS triage (
    message_id      INTEGER PRIMARY KEY,
    account_id      INTEGER NOT NULL,
    importance      INTEGER NOT NULL DEFAULT 0,
    tier            TEXT NOT NULL DEFAULT 'noise',
    sensitivity     TEXT NOT NULL DEFAULT 'normal',
    sealed_kind     TEXT,
    one_line        TEXT NOT NULL DEFAULT '',
    reason          TEXT NOT NULL DEFAULT '',
    -- Per-property triage justifications: a JSON object
    -- {importance?,deadline?,tier?} of short human-readable reasons. HUMAN-DOOR
    -- ONLY — the agent-door read path leaves it unset, so it never crosses /mcp.
    field_reasons   TEXT,
    deadline        TEXT,
    matched_rule_id INTEGER,
    -- STAGE-1 LLM marker. NULL = still needs the Stage-1 refine pass (its
    -- heuristic seed values are provisional). Otherwise the Stage-1 model id,
    -- 'rule' when a sender rule already decided the row (no model spend),
    -- 'human' when the account owner corrected the verdict by hand, 'n/a' for
    -- sealed and sent mail, or one of the two NO-VERDICT sentinels:
    --   'stale-skip'      too old for the pass's horizon; no model was asked.
    --   'heuristic-only'  a model WAS asked and did not answer (refusal or a
    --                     permanent failure), so the seed stands.
    -- Both leave the row on its heuristic seed and they are easy to conflate,
    -- which is why they are separate strings: they are opposite facts, and the
    -- row is the only place either one is recorded. Rows stamped before the
    -- split may carry 'heuristic-only' for either reason and cannot be
    -- re-attributed — nothing records when a row was processed.
    stage1_model_used TEXT,
    -- Set to 1 by `triage::router::should_escalate` over the Stage-1 verdict, or
    -- at ingest for a Filtered rule needing want_text evaluation, to mark the row
    -- for Stage-2 escalation.
    needs_stage2    INTEGER NOT NULL DEFAULT 0,
    -- How many times this row has been RE-EVALUATED (see `triage_revisits`).
    -- Bounded by `RevisitConfig::max_per_message_lifetime` so a verdict that
    -- keeps answering "check again tomorrow" terminates instead of billing
    -- forever.
    revisit_count   INTEGER NOT NULL DEFAULT 0,
    -- WHICH router arm escalated the row ('buried_bill' | 'unverified_urgency' |
    -- 'scam_shape' | 'exception' | 'invoice' | 'sender_corrected' | 'boundary' |
    -- 'model_unsure'). NULL when the row did not escalate. Stored so the
    -- escalation MIX can be inspected: the arms are the design, and tuning them
    -- blind to which one fires is guesswork.
    escalation_reason TEXT,
    -- STAGE-2 LLM marker. NULL = not yet Stage-2 processed. The Stage-2 queue
    -- predicate is `stage1_model_used IS NOT NULL AND needs_stage2=1 AND
    -- model_used IS NULL AND sensitivity='normal'`.
    model_used      TEXT,
    -- CATEGORIZE-THEN-EXTRACT: one coarse LLM category ('general' | 'invoice' |
    -- 'banking_statement' | 'transaction_alert'), NULL when no LLM ever looked.
    -- A category with a registered specialist extractor queues the row for a
    -- structured second pass. SEALED ROWS NEVER RUN THE LLM STAGES, so their
    -- category stays NULL — which is what excludes them from every queue.
    category        TEXT,
    -- SPECIALIST-EXTRACTOR marker. NULL = not yet through its category's
    -- extractor (or the category has none); otherwise the extractor model id or
    -- a 'skip-*' sentinel. Queue predicate: `category IN (<extractable>) AND
    -- extractor_model_used IS NULL AND sensitivity='normal'`.
    extractor_model_used TEXT,
    -- SHIPMENTS-EXTRACTOR trigger + marker, stamped at INGEST from a LOOSE
    -- shipping signal (`triage::shipment::has_loose_shipping_signal`) rather than
    -- from an LLM category:
    --   NULL        no shipping signal at ingest — this row never queues.
    --   'pending'   queued for the shipments extractor.
    --   anything    a processed marker: the extractor model id, or one of the
    --               'stale-skip' / 'apply-failed' / 'extract-failed' sentinels.
    -- Its own column and its own queue BECAUSE the extract queue routes on
    -- `triage.category` (there is no shipping category) and excludes
    -- receipt-bearing rows, which most order confirmations are. Re-ingest
    -- PRESERVES a processed marker and only refreshes 'pending'; sealing NULLs
    -- it, and retriage re-pends it.
    ship_extract_model TEXT,
    -- WHEN A HUMAN LAST ASKED for this row to be re-triaged (RFC3339 UTC), NULL
    -- when nobody ever has. Stamped by `Store::retriage_reset` on exactly the
    -- rows it requeues, and read by the LLM passes as a FORCE: a re-triage is an
    -- explicit request, so for `RETRIAGE_FORCE_WINDOW` after it the row bypasses
    -- the age-based stale skip that would otherwise stamp it processed without a
    -- model call. The window is what keeps the force from outliving the request:
    -- the same row can re-enter a queue months later through a revisit, and a
    -- permanent stamp would quietly buy every one of those a frontier call.
    retriage_at     TEXT,
    -- "REMIND ME LATER" (RFC3339 UTC), NULL when no reminder is pending. Setting
    -- it also marks the thread done, so a snoozed mail leaves every band at once
    -- instead of sitting there nagging until its date arrives. The sweep in the
    -- sync loop is what un-defers it: at `remind_at <= now` the row goes back to
    -- status='open' and the stamp MOVES to `reminded_at`, so exactly one of the
    -- two is ever set — pending or fired, never both.
    remind_at       TEXT,
    -- WHEN A PENDING REMINDER FIRED, NULL until one does. Read as a standing-band
    -- arm (see `STANDING_BAND`): a fired reminder outranks tier, because the user
    -- personally declared this mail owed attention and the triage model's opinion
    -- of it stopped mattering at that moment. Cleared when a NEW reminder is set,
    -- and left alone by a plain clear — clearing a pending reminder says nothing
    -- about one that already came due.
    reminded_at     TEXT,
    -- WHEN THE USER OPENED THIS MAIL, NULL until they do.
    --
    -- NOT `surfaced_at`, AND THE DIFFERENCE IS THE WHOLE POINT. A row is
    -- surfaced the first time it flows out through ANY read door, which
    -- includes appearing in a list the user scrolled past; it means "this was
    -- in front of them". This says they opened it. One is what we showed, the
    -- other is what they read, and only the second can answer "how much of my
    -- mail do I still have to open myself".
    --
    -- WRITTEN BY THE CLIENT SAYING SO (`POST /client/thread/{id}/opened`), not
    -- by the thread GET, and that route's comment has the reasoning: the client
    -- prefetches threads nobody looked at, and opens warmed ones without asking
    -- this daemon anything. Serving a body is evidence of neither.
    --
    -- IT SEES ONLY PASSBAND. Mail read in Gmail on a phone is never stamped
    -- here, so anything computed from it UNDERSTATES opens and flatters the
    -- product. Every consumer has to know that; see `Store::share_open_rate`,
    -- which is the only one, and which says so.
    opened_at       TEXT,
    -- HOW LONG THIS MAIL WAS ON SCREEN, in milliseconds, summed over every
    -- report the owner's client has sent (`POST
    -- /client/v2/messages/{id}/attention`). A duration and nothing else: not
    -- what was scrolled to, not when. Clamped per report and in total (see
    -- `store::agent_triage::MAX_DWELL_TOTAL_MS`), so a reader left open over
    -- lunch cannot make one newsletter look studied.
    --
    -- ZERO MEANS TWO DIFFERENT THINGS, and only `dwell_ledger_since` tells them
    -- apart: a row opened before the client could report dwell has 0 for no
    -- behavioural reason. Every statistic over this column therefore counts
    -- only rows whose `opened_at` is at or after that floor; see
    -- `migrate::stamp_dwell_ledger_start`.
    --
    -- NOT part of the agent's `user_state` fingerprint, on purpose: a dwell
    -- report arrives while mail is being read, which is exactly when a triage
    -- pass may be in flight, and it changes nothing that pass decided.
    dwell_ms        INTEGER NOT NULL DEFAULT 0,
    -- HOW MANY LINKS IN THIS MAIL THE OWNER FOLLOWED, summed the same way and
    -- under the same floor. A COUNT ONLY: no URL, no host, no position is
    -- ever stored, here or anywhere else.
    link_clicks     INTEGER NOT NULL DEFAULT 0,
    -- MAY THIS MESSAGE EVER NOTIFY, and from when (RFC3339 UTC). NULL means
    -- never: a backfill row, the user's own sent copy, or mail that was already
    -- older than `notify.freshness_window_secs` the first time we saw it.
    --
    -- Written by the SYNC ENGINE on the row's FIRST INSERT only and preserved
    -- verbatim on conflict, because only the engine knows which sync path an
    -- ingest is on — and that, not the sender's `Date:` header, is the honest
    -- answer to "is this new mail or are we re-reading the archive". The stamp
    -- replaces a per-emission freshness check that measured `messages.received_at`,
    -- which could not tell old mail we are backfilling from new mail we were
    -- slow to reach and so ate 24.7% of notify-worthy mail outright
    -- (docs/NOTIFY.md §2a). Emission now measures `now - notify_eligible_at`
    -- against `notify.rescue_window_secs`, so a verdict that came back late
    -- still buzzes and only a genuinely stale one is dropped.
    --
    -- NOT BACKFILLED: every row that predates the column is NULL, which is the
    -- silent direction.
    notify_eligible_at TEXT,
    -- BROUGHT IN BY THE FIRST-RUN BACKFILL, written on first insert only. A
    -- thread whose live mail is all first-load shares a small FYE allowance
    -- (`agent_thread_attention.first_load`). 0 on every pre-existing row.
    first_load      INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'new',
    surfaced_at     TEXT,
    resolved_at     TEXT,
    -- WHO RESOLVED THIS ROW, when it was not a person. NULL means a human did,
    -- or a path older than this column: the two are indistinguishable, which
    -- is the gap this column closes going forward (docs/TRIAGE-OPERATIONS.md,
    -- "Historical automatic `done` rows without reliable provenance cannot be
    -- distinguished from a human dismissal").
    --
    -- The one writer today is smart clear, which stamps
    -- `smart_clear:<smart_clear_batches.id>`. That stamp is what makes the batch
    -- undoable.
    --
    -- IT DOES NOT, BY ITSELF, KEEP A SWEEP OUT OF ANYTHING. A reader that
    -- takes `status = 'done'` to mean the owner decided has to check this
    -- column too, and only two do: the fatigue score's `cleared_unopened`
    -- (`reading_senders::SENDER_COUNTS_SQL`), which smart clear itself reads
    -- and would otherwise feed, and the rule suggester's digest
    -- (`rules::SENDER_RULE_DIGEST_SQL`). Everything else still sees a swept
    -- row as plain done, the triage agent's context included (its
    -- `user_state` fingerprint and its prompt treat any done as the owner's).
    -- That one is deliberate for now; a new reader is not excused by it.
    --
    -- ANY LATER HUMAN STATUS WRITE CLEARS IT. A person who reopens or re-does
    -- a row has taken it over, and an undo of the batch must then leave it
    -- alone. Every statement that writes `status` therefore writes this too.
    resolved_by     TEXT,
    created_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_triage_sensitivity ON triage(account_id, sensitivity);
CREATE INDEX IF NOT EXISTS idx_triage_status ON triage(account_id, status);
-- `idx_triage_remind_at ON triage(account_id, remind_at) WHERE remind_at IS NOT
-- NULL` and its twin `idx_triage_reminded_at` (same shape over `reminded_at`,
-- for the fired reminders every Standing read lists) are created in migrate.rs,
-- NOT here, for the same reason as `idx_shipments_order_ref` below: this file
-- runs in full on every open, BEFORE the column migrations, so an index over a
-- migrated column would fail ("no such column: remind_at") on every pre-existing
-- DB and make the store unopenable.

CREATE TABLE IF NOT EXISTS deadlines (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    amount     REAL,
    currency   TEXT,
    due_at     TEXT NOT NULL,
    past_due   INTEGER NOT NULL DEFAULT 0,
    source     TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_deadlines_due ON deadlines(account_id, due_at);

-- PACKAGE TRACKING. One row per (account, tracking_number), extracted from
-- NON-SEALED shipping mail. The tracking number is REQUIRED — it is the dedupe
-- key, so mail without one is skipped — and a later email about the same number
-- UPDATES the row. `status` walks the ordered<shipped<out_for_delivery<delivered
-- ladder (exception is a flag) and never regresses a delivered shipment.
--
-- SECURITY: detection runs ONLY for sensitivity='normal' mail, so this table has
-- no sealed rows BY CONSTRUCTION and needs no sealed join on read.
CREATE TABLE IF NOT EXISTS shipments (
    id              INTEGER PRIMARY KEY,
    account_id      INTEGER NOT NULL,
    tracking_number TEXT NOT NULL,
    carrier         TEXT NOT NULL,
    item_name       TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'shipped',
    tracking_url    TEXT,
    last_message_id INTEGER,
    first_seen      TEXT NOT NULL,
    last_update     TEXT NOT NULL,
    -- CARRIER POLLING. Everything below is filled by the carrier API, never by
    -- mail, and NULL/0 until the first poll. The carrier is ground truth for
    -- `status` (see `triage::shipment::ShipmentStatus::reconcile_carrier`), but
    -- a poll NEVER moves `last_message_id` — no message backs it.
    --
    -- The carrier's own latest status string, verbatim. Recorded even when it
    -- maps to no `status` value, so the client can show what the carrier said.
    carrier_status_raw TEXT,
    -- Carrier-estimated delivery; NULL when the carrier gives none.
    eta             TEXT,
    -- When the package landed, stamped by whichever path saw it first (a
    -- delivered email or a poll) and never overwritten after.
    delivered_at    TEXT,
    -- Last poll ATTEMPT, success or permanent failure. NULL = never polled.
    last_polled_at  TEXT,
    -- Consecutive PERMANENT poll failures (an unknown/expired number), the
    -- retirement cap for the poll queue. Transient errors do not count, and a
    -- successful poll resets it to 0.
    poll_failures   INTEGER NOT NULL DEFAULT 0,
    -- The RETAILER's identifier for the purchase ("112-3456789-1234567"), when
    -- the mail carried one. NOT a dedupe key — `tracking_number` still is — but
    -- the join back to `shipment_orders`, so a tracking-bearing email can absorb
    -- the staged order it belongs to. NULL when no order reference was found.
    order_ref       TEXT,
    -- The MERCHANT NAMESPACE for `order_ref`: the registrable domain of the
    -- feeding message's sender, lowercased. An order reference is only unique
    -- WITHIN a merchant — "Order #1042" from two shops is two purchases — so
    -- every order_ref lookup is scoped by this column. NULL alongside a NULL
    -- order_ref, and on rows written before the column existed.
    order_merchant  TEXT,
    -- Which message named the CURRENT `order_merchant` when the triage agent
    -- wrote it (a store name). NULL for the old extractor's domain merchant.
    -- The agent door serves the merchant only when this message is allowed.
    order_merchant_msg INTEGER,
    -- IMMUTABLE PROVENANCE: the message that CREATED this row, written once on
    -- INSERT and never updated. `last_message_id` moves to whichever mail most
    -- recently advanced the row, so it answers "who touched this last", not
    -- "who minted this" — and the extractor's phantom reaping must only ever
    -- reach rows the message in hand actually created.
    created_by_message_id INTEGER,
    -- Which message's extraction supplied the CURRENT `item_name`. Usually
    -- `last_message_id`, but three paths DONATE a name onto a row another mail
    -- feeds (staged-order promotion, the order-ref-only adoption, the thread
    -- adoption), and sealing a message must scrub the text it contributed
    -- wherever it landed. NULL when no name, or on pre-column rows.
    item_name_msg   INTEGER,
    -- WHICH MECHANISM supplied the current `item_name`: 'regex' (the ingest
    -- detector lifting it out of a subject or body) or 'llm' (the shipments
    -- extractor). The sibling of `item_name_msg`, which answers WHICH MESSAGE.
    -- An llm name replaces a regex one outright; a regex name never replaces an
    -- llm one; within a source, longer-wins. Reset to 'regex' whenever the name
    -- is scrubbed (sealing, re-triage), so a source marker whose name is gone
    -- cannot lock a later regex name out.
    --
    -- `shipment_orders` deliberately has NO such column: only the extractor ever
    -- writes that table, so every name in it is 'llm' by construction.
    item_name_source TEXT NOT NULL DEFAULT 'regex',
    -- USER CLEAR (RFC3339, NULL = not cleared): the user said "stop showing me
    -- this". READ-SIDE ONLY, and it is never un-set: a listing hides the row
    -- only while `last_update <= cleared_at`, so the moment anything advances
    -- `last_update` past this stamp the row returns by itself. The row keeps
    -- being polled the whole time — polling is what produces that update.
    cleared_at      TEXT,
    -- THE CARRIER'S ANSWER CLOCK (RFC3339, NULL = never answered): the last
    -- poll the carrier actually answered for this number, written only by
    -- `apply_carrier_track`. `last_polled_at` is the ATTEMPT clock and stamps
    -- on every try, answered or not, because the poll queue rotates on it. The
    -- listing's "a carrier is vouching for this row" reads THIS one, so a
    -- number whose answers stopped (transient errors, an unparseable body) is
    -- vouched for only as long as its last real answer is inside the window.
    last_answered_at TEXT,
    UNIQUE(account_id, tracking_number)
);

CREATE INDEX IF NOT EXISTS idx_shipments_status ON shipments(account_id, status);
-- `idx_shipments_order_ref ON shipments(account_id, order_ref)` is created in
-- migrate.rs, NOT here. This file runs in full on every open, BEFORE the column
-- migrations, so an index over a migrated column would fail ("no such column:
-- order_ref") on every pre-existing DB and make the store unopenable.

-- ORDERS STAGING. A purchase the shipments extractor recognized but that carries
-- NO TRACKING NUMBER yet (the order confirmation arrives days before the ship
-- notice). Keyed by (account, merchant, order_ref) instead of a tracking number,
-- so it cannot live in `shipments` — that table's identity IS the tracking
-- number. When the ship notice lands with both the order reference and a number,
-- the staged row is promoted into `shipments` and deleted here.
--
-- The MERCHANT is part of the key on purpose: "Order #1042" is unique only
-- within the shop that issued it, and an unnamespaced key lets one retailer's
-- confirmation donate its product name onto another retailer's package.
-- `order_merchant` is NOT NULL (defaulting to '' for an underivable sender)
-- because SQLite treats every NULL in a UNIQUE index as distinct, which would
-- silently disable the staging upsert's ON CONFLICT.
--
-- SECURITY: written only from the shipments extractor, whose queue gates on
-- sensitivity='normal', so this table has no sealed rows BY CONSTRUCTION.
-- Sealing a message still deletes the rows it fed and scrubs the names it merely
-- donated (`item_name_msg`), and so does a re-triage.
CREATE TABLE IF NOT EXISTS shipment_orders (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    order_ref TEXT NOT NULL,
    order_merchant TEXT NOT NULL DEFAULT '',
    item_name TEXT NOT NULL DEFAULT '',
    thread_id TEXT NOT NULL DEFAULT '',
    last_message_id INTEGER,
    -- Which message's extraction supplied the CURRENT `item_name` — the pointer
    -- above moves to the latest feeder, so it cannot answer that on its own.
    item_name_msg INTEGER,
    first_seen TEXT NOT NULL,
    last_update TEXT NOT NULL,
    UNIQUE(account_id, order_merchant, order_ref)
);

CREATE INDEX IF NOT EXISTS idx_shipment_orders_msg ON shipment_orders(account_id, last_message_id);

-- RECEIPTS. One row per (account, message): money ALREADY PAID, extracted from
-- NON-SEALED past-transaction mail. Records, not obligations — auto-resolved
-- (triage.status='done') at ingest so they live only in the Receipts category.
-- `from_addr`/`from_name` are stored so the client renders a merchant name with
-- no extra join; `amount`/`currency` are best-effort (NULL is still a receipt).
-- Detection is independent of shipment detection, so one mail can be both. A
-- landing receipt may also AUTO-CLOSE one matching open bill (audited as
-- bill.auto_close — see triage/receipt_match.rs).
--
-- SECURITY: detection runs ONLY for sensitivity='normal' mail, so this table has
-- no sealed rows BY CONSTRUCTION and needs no sealed join on read.
CREATE TABLE IF NOT EXISTS receipts (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    message_id  INTEGER NOT NULL,
    from_addr   TEXT NOT NULL,
    from_name   TEXT,
    amount      REAL,
    currency    TEXT,
    received_at TEXT NOT NULL,
    UNIQUE(account_id, message_id)
);

CREATE INDEX IF NOT EXISTS idx_receipts_received ON receipts(account_id, received_at);

-- CALENDAR UPDATES. One row per (account, message): invite / update /
-- cancellation / RSVP response, extracted from NON-SEALED calendar mail. Like
-- receipts these are RECORDS (the user's real calendar is the source of truth),
-- auto-resolved at ingest so they live only in the Calendar zone.
-- `event_title`/`starts_at`/`organizer` are best-effort — classification comes
-- from the structural subject shape, not extraction. The /client/calendar window
-- filters on received_at (mail arrival), NOT starts_at (event time).
--
-- SECURITY: detection runs ONLY for sensitivity='normal' mail, so this table has
-- no sealed rows BY CONSTRUCTION and needs no sealed join on read.
CREATE TABLE IF NOT EXISTS calendar_updates (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    message_id  INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    event_title TEXT,
    starts_at   TEXT,
    organizer   TEXT,
    received_at TEXT NOT NULL,
    UNIQUE(account_id, message_id)
);

CREATE INDEX IF NOT EXISTS idx_calendar_received ON calendar_updates(account_id, received_at);

-- BANKING. One row per (account, message): a STATEMENT or a TRANSACTION ALERT,
-- extracted by the banking specialist extractor from a NON-SEALED row the LLM
-- categorized. Like receipts these are RECORDS, so the extractor auto-resolves
-- the triage row (status='done') and they live only in the Banking zone. For a
-- statement `amount` is the TOTAL balance, never the minimum payment; for an
-- alert it is the transaction amount. `account_hint` is ONLY ever a masked
-- last-4 tail ("…1234") — the extractor post-validates, reducing anything longer
-- to a tail or NULL, so a full account number is never stored.
--
-- A receipt and a banking row must never DOUBLE-CREATE: the deterministic
-- receipt detector runs at INGEST, before the categorizer, and the extractor
-- queue excludes any message that already has a receipts row.
--
-- SECURITY: sealed rows never run the LLM stages, so they carry category=NULL
-- and are absent from the extractor queue (the extractor also enforces the
-- release-mode sealed guard). No sealed rows here BY CONSTRUCTION.
CREATE TABLE IF NOT EXISTS banking (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER NOT NULL,
    message_id   INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    institution  TEXT,
    amount       REAL,
    currency     TEXT,
    account_hint TEXT,
    received_at  TEXT NOT NULL,
    UNIQUE(account_id, message_id)
);

CREATE INDEX IF NOT EXISTS idx_banking_received ON banking(account_id, received_at);

-- MARKETING. One row per message the LLM categorized `marketing`: brand, the
-- offer in one line, the discount, a promo code, and when it expires. Written by
-- the marketing specialist extractor (triage/extract/marketing.rs).
--
-- SECURITY: like `banking`, NO sealed rows BY CONSTRUCTION — sealed mail never
-- runs the LLM stages, so it carries a NULL category and is absent from every
-- extractor queue.
--
-- This extractor does NOT auto-resolve its triage row (banking does). Marketing
-- is noise-tier already, and resolving it would drop it out of the flat Emails
-- inbox, whose whole promise is that it hides nothing.
CREATE TABLE IF NOT EXISTS marketing (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    message_id  INTEGER NOT NULL,
    brand       TEXT,
    offer       TEXT,
    discount    TEXT,
    -- Shape-validated in the extractor: short, alphanumeric, never a sentence
    -- or a URL. See sanitize_code.
    code        TEXT,
    -- YYYY-MM-DD, and only when plausible relative to received_at.
    expires_at  TEXT,
    received_at TEXT NOT NULL,
    UNIQUE(account_id, message_id)
);

CREATE INDEX IF NOT EXISTS idx_marketing_received ON marketing(account_id, received_at);

-- UNSUBSCRIBES. One row per (account, sender_addr): the human-door record that
-- the user asked a sender to stop, plus the "did they honor it?" violation
-- ledger. A fresh request RESETS violation_count/last_violation_at/resolution,
-- restarting the 72h grace clock. `method` is how it went out: 'browser' (the
-- client opened the link), 'one_click' or 'page' (the daemon's automatic job,
-- docs/SECURITY.md §7), or 'auto' while that job is running or needs the user.
-- `source_message_id` is nullable (the message may later be deleted).
-- `resolution` is blocked|dismissed|NULL, NULL meaning the request is still
-- outstanding and the violation detector armed.
--
-- VIOLATION SEMANTICS, applied in the ingest transaction: storing a NON-SENT
-- inbound message whose sender has an unresolved row, more than 72h after
-- requested_at, increments violation_count and stamps last_violation_at.
--
-- AUTOMATIC UNSUBSCRIBES (`method` auto | one_click | page) carry a `status`:
-- running -> done | needs_you, and needs_you -> opened once the user follows
-- `status_url` to finish it themselves. The violation detector is armed only
-- for a request that actually went out: status NULL (legacy browser rows),
-- done, or opened. A job still running, or one waiting on the user, has not
-- asked the sender for anything yet, so their next email is not a violation.
-- `requested_at` moves to the moment it went out, so the 72h grace runs from
-- there rather than from the click.
CREATE TABLE IF NOT EXISTS unsubscribes (
    id                INTEGER PRIMARY KEY,
    account_id        INTEGER NOT NULL,
    sender_addr       TEXT NOT NULL,
    requested_at      TEXT NOT NULL,
    method            TEXT NOT NULL,
    source_message_id INTEGER,
    violation_count   INTEGER NOT NULL DEFAULT 0,
    last_violation_at TEXT,
    resolution        TEXT,
    status            TEXT,
    status_url        TEXT,
    status_reason     TEXT,
    smart_block       TEXT,
    status_at         TEXT,
    UNIQUE(account_id, sender_addr)
);

CREATE INDEX IF NOT EXISTS idx_unsubscribes_requested ON unsubscribes(account_id, requested_at);

-- READING SENDER KEEPS. One row per (account, sender_addr): the owner looked at
-- this sender in the Reading purge flow and chose to stay subscribed. The
-- flow then leaves them alone for `KEEP_QUIET_DAYS`; after that the row no
-- longer affects the score and is kept only as the date it happened.
-- `sender_addr` is the lowercased bare address, as in `unsubscribes`. A second
-- keep refreshes `kept_at`.
--
-- NOTHING DELETES A ROW. A later unsubscribe or squelch rule for the same
-- sender simply outranks it at read time (the newer decision wins; see
-- `reading_senders`), so no rule or unsubscribe writer has to know this table
-- exists.
CREATE TABLE IF NOT EXISTS reading_sender_keeps (
    account_id  INTEGER NOT NULL,
    sender_addr TEXT NOT NULL,
    kept_at     TEXT NOT NULL,
    PRIMARY KEY (account_id, sender_addr)
);

-- SMART CLEAR BATCHES. One row per press of "clean this out" that cleared
-- anything. `triage.resolved_by = 'smart_clear:<id>'` on each row it took is
-- the membership; `detail` is JSON carrying each cleared message with the
-- reason it went and the status it had, so an undo restores exactly that.
-- `undone_at` is stamped by the first undo and makes every later one a no-op.
-- That undo also writes `reopened` into `detail`: the ids it gave back. Smart
-- done reads those (for batches undone in the last 90 days) and never takes
-- such a row again, because an undo is the owner saying "not those".
CREATE TABLE IF NOT EXISTS smart_clear_batches (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    undone_at   TEXT,
    cleared     INTEGER NOT NULL,
    detail      TEXT NOT NULL
);
-- For the one read that is not by id: an account's recently undone batches.
CREATE INDEX IF NOT EXISTS idx_smart_clear_batches_undone
    ON smart_clear_batches(account_id, undone_at);

-- AUTH-MAIL SHREDDER (retention). One row per message the retention pass moved
-- to Gmail Trash, so the "N shredded" figure is a real ledger and a message can
-- never be shredded twice.
--
-- WRITTEN BY THE HUMAN DOOR ONLY: the sync daemon holds a `gmail.readonly`
-- credential by hard invariant and physically cannot trash anything.
--
-- POLICY (both keys live in `app_settings`, per account):
--   shred_enabled    '1' | '0'  — DEFAULT OFF; deleting mail on a timer only
--                                 ever happens after a deliberate opt-in.
--   shred_after_days  integer   — default 30. Sealed mail older than this is
--                                 trashed.
--
-- TRASH, NEVER DELETE: the pass calls Gmail's /trash, so everything it touches
-- stays recoverable. The write credential is `gmail.modify`, which CANNOT
-- permanently delete — the blast radius is capped by the scope itself.
CREATE TABLE IF NOT EXISTS shred_log (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER NOT NULL,
    message_id   INTEGER NOT NULL,
    gmail_msg_id TEXT NOT NULL,
    sender       TEXT NOT NULL,
    kind         TEXT,
    received_at  TEXT NOT NULL,
    shredded_at  TEXT NOT NULL,
    UNIQUE(account_id, message_id)
);

CREATE INDEX IF NOT EXISTS idx_shred_at ON shred_log(account_id, shredded_at);

-- TRIAGE FEEDBACK. Every human override of the triage pipeline lands here. This
-- is a TRAINING SET, not a UI log: "the model said X, the human said Y" pairs to
-- refine prompts and heuristics against what actually went wrong.
--
-- 1. `original` carries a JSON SNAPSHOT of the whole triage row at correction
--    time, including which model produced it — a label without the features that
--    produced it is near-worthless for refinement.
-- 2. `sender`/`subject` are DENORMALIZED rather than joined from `messages`,
--    because the shredder and the user both delete mail, and a training set that
--    silently loses its inputs cannot be told apart from an empty one.
--
-- NOT unique per message: correcting the same email twice is two facts, and a
-- human changing their mind is itself signal. Append-only, ordered by
-- `corrected_at`.
CREATE TABLE IF NOT EXISTS triage_feedback (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER NOT NULL,
    message_id   INTEGER NOT NULL,
    corrected_at TEXT NOT NULL,
    -- Which axis the human overrode: 'tier' | 'category' | 'sensitivity'.
    -- READ BY THE INGEST UPSERT, not only by the eval queries: a
    -- 'sensitivity' row here is what tells `ingest_message` that a PERSON
    -- decided whether this message is auth mail, so a re-walk's heuristic seed
    -- may not overwrite it in either direction (docs/SECURITY.md §4).
    dimension    TEXT NOT NULL,
    -- What triage had (NULL when the row never had a value for that axis).
    from_value   TEXT,
    -- What the human says it should have been.
    to_value     TEXT NOT NULL,
    -- JSON snapshot of the triage row as it was (note 1 above).
    original     TEXT NOT NULL,
    -- Denormalized on purpose (note 2 above).
    sender       TEXT NOT NULL,
    subject      TEXT NOT NULL,
    -- Optional free-text from the human ("this is a receipt, not a bill").
    note         TEXT
);

CREATE INDEX IF NOT EXISTS idx_triage_feedback_at
    ON triage_feedback(account_id, corrected_at);
-- The two shapes every triage queue asks of this table, once per queued row:
-- "has the owner ever corrected this SENDER" (the router's strongest signal) and
-- "did they correct THIS MESSAGE" (the row a model may never overwrite).
CREATE INDEX IF NOT EXISTS idx_triage_feedback_sender
    ON triage_feedback(account_id, sender);
-- NOCASE twin of the sender index: the triage queues probe this table with
-- `sender = m.from_addr COLLATE NOCASE` once per candidate row, and a BINARY
-- index cannot serve a NOCASE comparison (see `idx_contacts_addr_nocase`).
CREATE INDEX IF NOT EXISTS idx_triage_feedback_sender_nocase
    ON triage_feedback(account_id, sender COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_triage_feedback_msg
    ON triage_feedback(account_id, message_id);

-- SCHEDULED RE-EVALUATIONS. A verdict is true as of a moment, and some mail
-- stops mattering when a date passes (see `crate::triage::revisit`). Each row is
-- one future point at which a message should be scored again.
--
-- A row fires at most once: `fired_at` moves NULL -> a timestamp and stays. A
-- FIRED row is then permanent history — the audit trail of why a message's
-- verdict changed after the fact — while PENDING rows are replaced wholesale
-- each time a new verdict lands, so a re-triaged message carries one current
-- schedule rather than the accumulated intentions of every verdict it ever had.
-- The pass reads `fired_at IS NULL AND revisit_at <= now`, which is exactly the
-- index below.
CREATE TABLE IF NOT EXISTS triage_revisits (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    -- When to look again (RFC3339 UTC).
    revisit_at TEXT NOT NULL,
    -- Short reason, shown back to the model on re-evaluation. MODEL-AUTHORED for
    -- source='model', therefore untrusted text: neutralized before it renders
    -- inside any prompt.
    reason     TEXT NOT NULL,
    -- 'model' (the classifier named the date) | 'deadline' | 'fye_stale' (both
    -- automatic; see `crate::triage::revisit::RevisitSource`).
    source     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    -- NULL while pending; stamped when the revisit pass consumes the row.
    fired_at   TEXT
);

CREATE INDEX IF NOT EXISTS idx_triage_revisits_due
    ON triage_revisits(account_id, fired_at, revisit_at);
CREATE INDEX IF NOT EXISTS idx_triage_revisits_msg
    ON triage_revisits(account_id, message_id);

-- Gmail sync cursor, keyed by a logical mailbox string. The engine's cursor
-- row is keyed mailbox='history': uidvalidity is unused (0) and last_uid holds
-- the account's historyId (a u64 that fits SQLite's i64 INTEGER). The engine
-- now reads and writes `sync_cursor` below; for a Gmail account that row is
-- still written alongside it, so a rolled-back build resumes where this one
-- stopped. The one-time sweeps keep their own done flags here.
CREATE TABLE IF NOT EXISTS sync_state (
    account_id  INTEGER NOT NULL,
    mailbox     TEXT NOT NULL,
    uidvalidity INTEGER NOT NULL,
    last_uid    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account_id, mailbox)
);

-- THE SYNC ENGINE'S CURSOR, one per account, opaque: whatever the account's
-- mail source (`crate::mail::MailSource`) encodes its change-feed position as.
-- Gmail's is the decimal historyId; nothing outside the source reads inside it.
-- A Gmail account written before this table existed has no row here and is
-- adopted from `sync_state` mailbox='history' on read.
CREATE TABLE IF NOT EXISTS sync_cursor (
    account_id INTEGER PRIMARY KEY,
    cursor     TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- WHERE AN IMAP ACCOUNT'S MESSAGES ARE, private to the IMAP mail source
-- (`crate::mail::imap::MessageLocations`): one row per (message, folder) the
-- source has seen it in. `msg_id` is the stable id (`messages.gmail_msg_id`),
-- a hash that cannot be turned back into a place to fetch from, hence this
-- table. `message_id` is the normalized Message-ID TEXT, kept for the
-- stale-location fallback (`SEARCH HEADER Message-ID`); NULL for mail without
-- one. A row is a hint, never the truth: the source re-checks UIDVALIDITY and
-- the UID on every use and drops rows that have gone stale. A Gmail account
-- never has a row here. No foreign key: like `sync_cursor`, a row for a gone
-- account is an orphan nothing reads.
CREATE TABLE IF NOT EXISTS imap_locations (
    account_id  INTEGER NOT NULL,
    msg_id      TEXT NOT NULL,
    folder      TEXT NOT NULL,
    uidvalidity INTEGER NOT NULL,
    uid         INTEGER NOT NULL,
    message_id  TEXT,
    seen_at     TEXT NOT NULL,
    PRIMARY KEY(account_id, msg_id, folder)
);

-- WHAT EACH IMAP ID IS (Amendment J, `crate::mail::imap::header_digest`):
-- "h1:" + sha256 of the canonical headers plus Reply-To (never the body),
-- one per stable id, recorded at first sight and never replaced. A later
-- sighting under the same id whose digest differs is a different message
-- with an id of its own. A value without the "h1:" prefix is an Amendment I
-- (body-bound) digest from an earlier build: read as none, and replaced by
-- the first header digest recorded. A new table rather than a column on
-- `imap_locations`, whose rows are per folder and come and go with moves:
-- what a message IS does not.
CREATE TABLE IF NOT EXISTS imap_digests (
    account_id  INTEGER NOT NULL,
    msg_id      TEXT NOT NULL,
    content_sha TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY(account_id, msg_id)
);

-- WHICH BYTES EACH IMAP ID MAY SERVE (review fix3 F1,
-- `crate::mail::imap::body_fingerprint`): "b1:" + sha256 of the BODY[TEXT]
-- bytes of the copy an id's content was first read from, recorded at that
-- first fetch and never replaced. A copy whose body differs (a re-send
-- reusing the Message-ID and the identity headers) is never served under the
-- id, from any folder. `from_key` is a hash of that copy's From addresses: a
-- distinct (colliding) id gets a thread of its own only when its author
-- differs (F4). No row: an id never fetched, or one from an earlier build,
-- bound at its next fetch from its earliest location.
CREATE TABLE IF NOT EXISTS imap_bodies (
    account_id  INTEGER NOT NULL,
    msg_id      TEXT NOT NULL,
    body_fp     TEXT NOT NULL,
    from_key    TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY(account_id, msg_id)
);

-- GMAIL-SIDE INBOX UNREAD COUNTS (labels/INBOX messagesUnread/threadsUnread),
-- refreshed once per sync cycle. One row per account, overwritten each fetch:
-- this mirrors Gmail's current state, it is not a history. Sync's own tables
-- cannot answer it — they hold only what the backfill window ingested, and
-- nothing here ever learns that Gmail marked a message read.
--
-- NO ROW MEANS NEVER FETCHED, which readers must serve as absence: zero unread
-- is a real and different answer.
CREATE TABLE IF NOT EXISTS inbox_unread (
    account_id INTEGER NOT NULL PRIMARY KEY,
    messages   INTEGER NOT NULL,
    threads    INTEGER NOT NULL,
    fetched_at TEXT NOT NULL
);

-- THE ACCOUNT'S GOOGLE PROFILE: display name and profile photo, fetched by the
-- sync loop from the People API when the grant carries the optional
-- userinfo.profile scope. One row per account, overwritten whole on each
-- successful fetch, so a photo the person removed goes away here too.
--
-- NO ROW MEANS NEVER FETCHED (an older grant without the scope, or a People
-- API that has never answered), and readers serve that as absence. A row with
-- a NULL photo is a real answer: the account has a name and no picture of its
-- own. `photo_tag` is a short hash of the bytes, which is all a client needs to
-- know whether the copy it holds is current.
CREATE TABLE IF NOT EXISTS account_profile (
    account_id INTEGER NOT NULL PRIMARY KEY,
    name       TEXT,
    given_name TEXT,
    photo      BLOB,
    photo_mime TEXT,
    photo_tag  TEXT,
    fetched_at TEXT NOT NULL
);

-- STAGE-2 BUDGET (circuit breaker). model_calls counts API attempts, incremented
-- BEFORE the call so retry storms cannot exceed the cap. Two scopes share the
-- table, keyed by thread_id:
--   * per-thread-per-day: thread_id = the message's real thread id.
--   * global-per-account-per-day: thread_id = the sentinel '__global__', which
--     no real Gmail thread id (hex) can collide with.
CREATE TABLE IF NOT EXISTS wake_budget (
    account_id  INTEGER NOT NULL,
    thread_id   TEXT NOT NULL,
    day         TEXT NOT NULL,
    model_calls INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account_id, thread_id, day)
);

-- LLM USAGE LEDGER. One row per (account, UTC day, category): running API usage
-- totals. `category` is 'stage1', 'stage2', or a specialist-extractor line such
-- as 'extract_banking' — extractors run on the stage-1 model but bill to their
-- own line so per-specialist cost stays visible. `calls` counts SUCCESSFUL
-- classify responses that carried a usage block. Cost is NOT stored: the human
-- door computes it at read time from config-driven per-MTok prices.
-- `input_tokens` is the UNCACHED prompt remainder; prompt-cache writes and
-- reads sit in their own columns because they price differently (kept in sync
-- with the additive migration in migrate.rs for pre-existing DBs).
CREATE TABLE IF NOT EXISTS stage2_usage (
    account_id    INTEGER NOT NULL,
    day           TEXT NOT NULL,
    category      TEXT NOT NULL DEFAULT 'stage2',
    calls         INTEGER NOT NULL DEFAULT 0,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account_id, day, category)
);

-- RUNTIME APP SETTINGS. A per-account key/value table for knobs a client can
-- change WITHOUT editing config.toml or restarting the daemon — the Stage-2
-- daily-cap overrides and the shredder policy keys. Precedence at read time: an
-- app_settings OVERRIDE beats config/env, which beats the built-in default.
-- `value` is TEXT, parsed by the reader.
CREATE TABLE IF NOT EXISTS app_settings (
    account_id INTEGER NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    UNIQUE(account_id, key)
);

-- EMAIL ATTACHMENTS. One row per attachment part extracted from a message's full
-- RFC822 at ingest — real attachments AND cid-inline parts. `data` holds the
-- decoded bytes, or NULL when the part exceeded the ingest cap (10 MB per
-- attachment / 25 MB per message): the metadata still lands (downloadable=false
-- on the wire, byte endpoint 410). `size_bytes` is always the real decoded size.
--
-- SECURITY: attachments are STORED for sealed mail like the body, and guarded on
-- serving — `attachment_bytes` requires the parent's sensitivity='normal', so a
-- sealed parent is indistinguishable from a nonexistent id (both 404), and the
-- metadata-listing thread view already 404s any sealed thread.
CREATE TABLE IF NOT EXISTS attachments (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    message_id  INTEGER NOT NULL,
    filename    TEXT NOT NULL,
    mime        TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL,
    data        BLOB,              -- NULL when over the ingest cap (metadata only)
    -- The part's normalized Content-ID (brackets stripped), NULL when it declared
    -- none. What an <img src="cid:..."> in the stored body_html resolves against.
    -- Deliberately NOT in the UNIQUE key below: widening that key would re-open
    -- the duplicate-file ingest DoS it exists to absorb. Two inline parts alike in
    -- everything but their cid therefore still collapse to one row, and the
    -- second img is simply dropped from the body rather than painted broken.
    content_id  TEXT,
    UNIQUE(account_id, message_id, filename, size_bytes)
);

CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(account_id, message_id);

-- KEYWORD SEARCH INDEX, mirroring `messages(subject, body)` — the upsert keeps
-- the two in step, so it can be rebuilt from `messages` at any time.
--
-- `porter unicode61`: unicode61 is the tokenizer fts5 would default to (folds
-- case and diacritics, splits on punctuation), with the Porter stemmer in front
-- of it so "passwords" finds "password" and "shipping" finds "shipped". A
-- mailbox is written in prose by people who did not know what you would later
-- search for, and an exact-word index makes the reader guess which form they
-- used. Stemming is applied to the QUERY as well by fts5 itself, so both sides
-- meet at the same stem, and `snippet()` still returns the ORIGINAL text.
--
-- `prefix = '2 3'`: two- and three-character prefixes get their own indexes.
-- The as-you-type fetch (`partial=1`) asks for `"<tail>"*` on every keystroke
-- and the FIRST keystrokes are the expensive ones: with no prefix index fts5
-- expands `"a"*` by walking the whole term list, and the diagnostics counts
-- ask for the same expansion again beside the page. Two lengths and no more —
-- past three characters the expansion is already narrow, and each extra length
-- is another index to write on every message.
--
-- Changing this line means an existing index has to be rebuilt, and that is
-- automatic: `migrate.rs` READS THIS STATEMENT out of the embedded schema and
-- rebuilds any `messages_fts` whose stored definition differs from it at all.
-- There is no second copy to update and no list of options to remember to
-- extend, so an edit here cannot ship unmigrated.
--
-- `tsubject`/`tbody` are the ENGLISH TRANSLATION (docs/TRANSLATION.md), from
-- `message_translations` while its status is `done` or `incomplete`, NULL
-- otherwise. Columns are APPENDED so every positional reference to 0 (subject)
-- and 1 (body) keeps its meaning; bm25 weighs them 4.0 and 1.0 like the
-- originals. Every writer fills all four: the message upsert, the translate
-- pass when a translation lands, a heal that drops one, and the rebuild.
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    subject, body, tsubject, tbody, tokenize = 'porter unicode61', prefix = '2 3'
);

-- ON-BOX SEMANTIC RECALL. A sqlite-vec `vec0` table holding one embedding per
-- NON-SEALED message; the rowid is `messages.id`, so a KNN hit joins straight
-- back. `account_id` is a vec0 METADATA column so KNN queries can constrain by
-- account in the WHERE clause.
--
-- SECURITY: SEALED MESSAGES ARE NEVER INSERTED HERE. Every write is gated on
-- `embeddable_mail_sql!` (store/sqlite/mod.rs): not spam, not known sealed (an
-- auth decision, a restriction, the retired `triage.sensitivity` column), and
-- either decided with access settled or the owner's own mail. A vector whose
-- message becomes sealed (or undecided again) is deleted at that event, and a
-- sweep at every store open deletes any left from before the gate
-- (`purge_forbidden_vectors_conn`). The read side re-checks access anyway.
--
-- DIMENSION: float[384] matches BGE-small-en-v1.5, and the store asserts the
-- configured embedder matches at attach time. Changing it means editing this
-- literal AND resetting the dev db — there are no migrations for a vec0 table.
CREATE VIRTUAL TABLE IF NOT EXISTS message_vecs USING vec0(
    message_id INTEGER PRIMARY KEY,
    embedding  FLOAT[384],
    account_id INTEGER
);

-- THE ENGLISH TRANSLATION'S VECTOR (docs/TRANSLATION.md), one per message whose
-- `message_translations` row is `done` or `incomplete`, embedded the same way
-- as the original (`message_embed_text(subject, body)`). The original's vector
-- in `message_vecs` is untouched; the KNN legs take the nearer of the two per
-- message. Same width, same metadata column, and the same rule that a vec0
-- table has no foreign keys: whoever deletes a translation deletes this row.
CREATE VIRTUAL TABLE IF NOT EXISTS message_translation_vecs USING vec0(
    message_id INTEGER PRIMARY KEY,
    embedding  FLOAT[384],
    account_id INTEGER
);

-- Audit log for the HUMAN DOOR (squelch-api /client/*). Every sealed-body reveal
-- and every write action appends a row here BEFORE returning; MCP never reads or
-- writes it. One internal writer also exists: the ingest receipt->bill
-- auto-close (actor='ingest', action='bill.auto_close'), so that state change
-- stays explainable.
CREATE TABLE IF NOT EXISTS audit_log (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    ts         TEXT NOT NULL,
    actor      TEXT NOT NULL,
    action     TEXT NOT NULL,
    target     TEXT,
    detail     TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_account_ts ON audit_log(account_id, ts);
-- The Auth page asks whether the owner revealed a message (an explained
-- sign-in alert needs the code it pairs with to have been used).
CREATE INDEX IF NOT EXISTS idx_audit_target ON audit_log(account_id, action, target);

-- NOTIFICATION EVENT LOG. The source of truth for "something worth interrupting
-- the user happened", written by the SYNC ENGINE at each triage verdict — never
-- by a store write method, because only the engine knows which sync path it is
-- on and whether the mail is fresh. Delivery adapters each track their OWN
-- monotonic cursor; there is deliberately no global 'delivered' flag, which
-- would make the second client a refactor.
--
-- Every column besides the ids is a DENORMALIZED SNAPSHOT at emission time, so a
-- client can render the notification from this row alone (the iOS NSE fetches
-- one row by id after an opaque push and has no second round-trip to spend).
--
-- ONE EVENT PER MESSAGE, EVER: UNIQUE(message_id) plus INSERT OR IGNORE at the
-- writer, which is what makes re-ingest and the refine passes idempotent. The
-- storm guard proper is `triage.notify_eligible_at`, stamped once at ingest:
-- a backfill row is structurally unable to reach this table, and a re-scan
-- re-ingests rows that already carry their stamp, so it cannot manufacture one.
--
-- SECURITY: NO SEALED CONTENT IS EVER REPRESENTED HERE. A sealed message may
-- now produce a row, but only a KIND-DERIVED one: `sealed_kind` plus a fixed
-- sentence from the table in docs/NOTIFY.md §11.6 ("Login code arrived"), the
-- sender address, and nothing the mail said. The subject, the body and the code
-- itself never reach this table, because the lane that writes the row is handed
-- a value with no field to carry them. An OTP on a lock screen would undo the
-- entire seal design; a phone that buzzes to say a code has landed does not.
-- A human sealing a message AFTER it notified makes `correct_triage` REDACT the
-- row (sender/one_line/deadline blanked, structure kept). It does NOT delete:
-- `id` is the rowid, and deleting the newest row would let the next insert reuse
-- that id, which every durable cursor would then skip forever.
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY,   -- monotonic; the clients' cursor
    account_id  INTEGER NOT NULL,
    message_id  INTEGER NOT NULL UNIQUE,
    thread_id   TEXT NOT NULL,
    kind        TEXT NOT NULL,         -- urgent | deadline | surfaced | opened
    is_auth     INTEGER NOT NULL DEFAULT 0 CHECK(is_auth IN (0,1)),
    tier        TEXT NOT NULL,
    importance  INTEGER NOT NULL,
    sender      TEXT NOT NULL,
    one_line    TEXT NOT NULL,
    deadline    TEXT,                  -- RFC3339 snapshot, or NULL
    -- Legacy subtype retained for replay compatibility. New events carry is_auth
    -- independently of external-agent access and always open the exact email.
    sealed_kind TEXT,
    created_at  TEXT NOT NULL
);

-- The read pattern is exactly "rows after my cursor, for my account".
CREATE INDEX IF NOT EXISTS idx_events_account_id ON events(account_id, id);

-- SMART RE-NOTIFY (issue #625, docs/RENOTIFY.md). Written only while the
-- `renotify` hidden feature is on for the account.
--
-- `client_sessions`: one row per Mac event stream (`/client/events?opens=1`),
-- open while `disconnected_at` is NULL. What it answers is "how long was the
-- Mac away before this connection", which is the client-online trigger.
-- Rows a crash left open are closed at the next daemon start. Pruned to the
-- newest few hundred per account on every insert.
CREATE TABLE IF NOT EXISTS client_sessions (
    id              INTEGER PRIMARY KEY,
    account_id      INTEGER NOT NULL,
    platform        TEXT NOT NULL,
    connected_at    TEXT NOT NULL,
    disconnected_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_client_sessions_account ON client_sessions(account_id, id);

-- One re-notify banner (`renotify_batches`) and the messages it named
-- (`renotify_items`, in banner order). `opened_at` is the banner tap the app
-- reports; whether each message was HANDLED after it is derived on read from
-- the message's own state, never stored, so it cannot drift from it.
CREATE TABLE IF NOT EXISTS renotify_batches (
    id                INTEGER PRIMARY KEY,
    account_id        INTEGER NOT NULL,
    trigger           TEXT NOT NULL,     -- client_online | meeting_end
    fired_at          TEXT NOT NULL,
    listeners         INTEGER NOT NULL,  -- Macs the frame was sent to
    opened_at         TEXT,
    opened_message_id INTEGER
);
CREATE INDEX IF NOT EXISTS idx_renotify_batches_account ON renotify_batches(account_id, id);
CREATE TABLE IF NOT EXISTS renotify_items (
    batch_id   INTEGER NOT NULL REFERENCES renotify_batches(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    position   INTEGER NOT NULL,
    -- The due time a due-soon re-notify was for (RFC 3339), NULL for an
    -- ordinary one. One due-soon re-notify per message per due time.
    due_at     TEXT,
    PRIMARY KEY(batch_id, position)
);
CREATE INDEX IF NOT EXISTS idx_renotify_items_message ON renotify_items(account_id, message_id);

-- THE NOTIFY DECISION LEDGER: one row per (message, lane) recording what that
-- lane decided about notifying, and nothing else (docs/NOTIFY.md §11.4).
--
-- A TABLE, NOT COLUMNS ON `triage`. Two lanes decide independently about the
-- same message — the fast lane at ingest, the deliberate lane behind it — and
-- the questions worth asking are cross-lane: rescued (`deliberate/sent` over a
-- `fast/declined_by_model`), overturned, confirmed. Those are JOINS over rows.
-- Folded into `triage` they would be a widening pair of columns per lane that
-- every unrelated read carries, and the second lane's write would be an UPDATE
-- over the first lane's answer rather than a fact standing beside it.
--
-- APPEND-ONLY. Written with INSERT OR IGNORE, NEVER UPDATEd, NEVER DELETEd: the
-- UNIQUE below makes a re-triage, or a Stage-2 pass over a row Stage-1 already
-- recorded, a silent no-op instead of a rewrite of history. What one lane
-- decided the first time it reached a message IS the labeled example; a row that
-- moves is not evidence of anything.
--
-- A row exists ONLY for a message carrying a `triage.notify_eligible_at` stamp,
-- so backfill and mail already stale at first sight produce nothing at all. That
-- is what keeps the table from being 95% rows about mail that was never a
-- candidate.
--
-- SECURITY: NO EMAIL-DERIVED TEXT. Not the one_line, not the subject, not the
-- sender. Every column is an id, a closed-vocabulary string, a score or a
-- duration, so this table cannot leak by construction — which is why it needs no
-- sensitivity gate and why a sealed message may be recorded here by kind.
CREATE TABLE IF NOT EXISTS notify_decisions (
    id                INTEGER PRIMARY KEY,
    account_id        INTEGER NOT NULL,
    message_id        INTEGER NOT NULL,
    -- 'fast' | 'deliberate' (crate::metrics::NotifyLane).
    lane              TEXT NOT NULL,
    -- sent | would_send | declined_by_model | unavailable | suppressed | expired
    -- (crate::metrics::NotifyDecision). ONE closed set for both lanes, so the
    -- cross-lane questions are joins over one vocabulary rather than two that
    -- have to be reconciled.
    decision          TEXT NOT NULL,
    -- 0-100: THE SCORE THAT WAS ON THE TABLE WHEN THIS DECISION WAS MADE,
    -- whatever the decision, and NULL only when there was none. So a
    -- `declined_by_model` always carries one, a `sent` carries the applied
    -- importance (known-contact floor included), an `expired` or a `suppressed`
    -- reached at emission time carries the score it was about to act on, and
    -- `unavailable` — plus a `suppressed` settled at `candidate` time, before
    -- anything scored it — carries NULL. Never invented: a 0 stood in for
    -- "nobody scored this" would read as "the model said no", which is the one
    -- distinction the eval query in docs/NOTIFY.md §11.11 turns on. Filter on
    -- `decision`, not on this column being NULL.
    notify_importance INTEGER,
    -- Model id | 'sealed' | 'heuristic' | NULL. Which ANSWER decided, which is
    -- what makes a decline a labeled example of a particular model's judgement
    -- rather than of the daemon's mood that day.
    model_used        TEXT,
    -- First sight (`triage.notify_eligible_at`) to this decision, milliseconds.
    -- Fast lane only: the deliberate lane's latency is the triage pipeline's,
    -- which the triage metrics already own.
    latency_ms        INTEGER,
    created_at        TEXT NOT NULL,
    -- ONE DECISION PER LANE PER MESSAGE, forever. This is the append-only rule
    -- expressed where it cannot be forgotten by a caller.
    UNIQUE(message_id, lane)
);

-- The eval read is exactly "this account's rows since <date>, in order".
CREATE INDEX IF NOT EXISTS idx_notify_decisions_account_created
    ON notify_decisions(account_id, created_at);

-- REGISTERED PUSH DEVICES. One row per APNs device token the user's phone handed
-- to their own daemon over the human door. Only the pusher task reads it.
--
-- `token` is UNIQUE and re-registration is an UPSERT, because iOS hands the app
-- its token on EVERY launch and a chatty app would otherwise grow one row per
-- cold start. `last_registered_at` is the liveness signal.
--
-- Rows die exactly two ways: the human door's DELETE, and APNs answering `410
-- Unregistered` — the relay passes that status back verbatim precisely so this
-- daemon, not shared infrastructure, owns the cleanup.
--
-- PRIVACY: a device token is the user's capability material. It is never logged
-- (the pusher logs row ids, or an 8-char prefix at most) and never crosses the
-- agent door.
CREATE TABLE IF NOT EXISTS devices (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL,
    -- Hex APNs device token, UNIQUE across accounts (one device, one row).
    -- Re-registering another account's token is REFUSED, not rebound: the
    -- upsert's conflict update carries
    -- `WHERE devices.account_id = excluded.account_id`, so registration can never
    -- silently repoint an existing device's pushes.
    token              TEXT NOT NULL UNIQUE,
    platform           TEXT NOT NULL DEFAULT 'ios',
    -- Opaque client-minted label for the account this device filed the
    -- registration under, echoed back on every push aimed at it. The receiving
    -- extension has nothing else to go on: event ids are per-daemon ints, so a
    -- phone holding two mailboxes cannot tell whose event 41 just arrived. NULL
    -- for anything registered before the field existed, and for the macOS
    -- client, which never asks a daemon to push at all.
    tag                TEXT,
    -- The APNs environment this token was minted in, as the client declared it:
    -- `production` (TestFlight / App Store) or `sandbox` (Xcode dev builds,
    -- simulators). It is a property of the TOKEN, not of the daemon: a token
    -- sent to the wrong environment is rejected by APNs, so ONE global switch
    -- cannot serve a mixed fleet. NULL = the client did not say, and the pusher
    -- falls back to its configured environment (then the relay's default).
    environment        TEXT,
    created_at         TEXT NOT NULL,
    last_registered_at TEXT NOT NULL
);

-- The pusher's read is exactly "every token for my account".
CREATE INDEX IF NOT EXISTS idx_devices_account ON devices(account_id);

-- LOCAL DRAFTS. One unsent composition per reply target, one per forwarded
-- message, and ANY NUMBER of new-message drafts per account (both keys NULL),
-- each addressed by its own `id`. A row carries at most one of the two keys.
--
-- HUMAN-DOOR DATA ONLY: served exclusively by /client/drafts, never synced to
-- Gmail Drafts, and never visible on /mcp — an unsent draft is the user's own
-- thinking, not mail the agent door was handed (two-door invariant).
--
-- Uniqueness is PARTIAL indexes rather than UNIQUE(account_id,
-- reply_to_message_id), so the key only binds rows that carry it. The NULL key
-- is NOT unique any more: new-message drafts are many by design. Its index
-- (`idx_drafts_new`) keeps the old name on purpose, as a ROLLBACK GUARD: an
-- older binary's `CREATE UNIQUE INDEX IF NOT EXISTS idx_drafts_new` checks
-- only the name, so it is a no-op instead of a UNIQUE violation on every open
-- (see migrate.rs).
-- The forward key's index (`idx_drafts_forward`) is created in migrate.rs, NOT
-- here: this file runs before `migrate`, and on an existing DB the index would
-- name a column that does not exist yet.
CREATE TABLE IF NOT EXISTS drafts (
    id                  INTEGER PRIMARY KEY,
    account_id          INTEGER NOT NULL,
    reply_to_message_id INTEGER,          -- NULL = not a reply
    forward_of_message_id INTEGER,        -- NULL = not a forward
    to_addr             TEXT NOT NULL DEFAULT '',
    -- The composer's other two recipient lists, comma-joined exactly as
    -- to_addr holds the visible ones. A draft that could not hold these would
    -- restore addressed to fewer people than it was written for -- and for a
    -- bcc that loss is silent, since nothing else on screen would show the
    -- audience had ever been set.
    cc_addr             TEXT NOT NULL DEFAULT '',
    bcc_addr            TEXT NOT NULL DEFAULT '',
    subject             TEXT NOT NULL DEFAULT '',
    body                TEXT NOT NULL DEFAULT '',
    -- 1 = an agent on the agent door wrote this row (issue #394), 0 = the
    -- owner did. Set once at insert and never cleared by an edit: the owner
    -- finishing an agent's draft does not make its first words theirs, and
    -- the composer says so until it is sent or discarded.
    from_agent          INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_drafts_reply
    ON drafts(account_id, reply_to_message_id) WHERE reply_to_message_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_drafts_new
    ON drafts(account_id) WHERE reply_to_message_id IS NULL;

-- FILES STAGED FOR A SEND. The composer uploads each file the moment it is
-- attached (POST /client/compose/attachments) and the row waits here, bytes
-- and all, until a send names it by id or a draft claims it. HUMAN-DOOR DATA
-- like `drafts`: never synced anywhere, never visible on /mcp.
--
-- LIFETIME is the draft's, or nothing's. `draft_id` is NULL from upload until
-- the next draft save claims the row; a send consumes every row it names, a
-- draft delete takes its rows with it, and an unclaimed row older than a day
-- (a composer closed before its first autosave, a failed send, a crash) is
-- swept on the next upload. The bytes are the whole point of the row, so
-- unlike inbound `attachments` there is no metadata-only state: over the cap
-- the upload is refused rather than recorded.
--
-- `content_id` is the `cid:` token an inline image is referenced by from the
-- body. Minted client-side (a UUID) so the composer can write the reference
-- into the markdown before the upload lands; validated, or minted here, when
-- the client sent none.
CREATE TABLE IF NOT EXISTS outbound_attachments (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    draft_id    INTEGER,              -- NULL until a draft save claims it
    -- The SECOND claim: set when scheduling moves the file from its draft to a
    -- `scheduled_sends` row, cleared again by a cancel. At most one of the two
    -- claim columns is set. Added to old DBs by migrate.rs; its index lives
    -- there too, since this file runs before migrate.
    scheduled_send_id INTEGER,
    filename    TEXT NOT NULL,
    mime        TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL,
    content_id  TEXT NOT NULL,
    data        BLOB NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_outbound_attachments_draft
    ON outbound_attachments(account_id, draft_id);

-- THE OUTBOX: mail the user has finished and told the daemon to send LATER
-- (docs/SCHEDULED-SEND.md). HUMAN-DOOR DATA ONLY, like `drafts` and
-- `outbound_attachments`: served on /client/scheduled, never synced, never
-- visible on /mcp. The row lives in the daemon rather than the client so a
-- closed laptop lid does not lose the mail.
--
-- NOT A DRAFT, on purpose. Drafts are edited by their KEYED UPSERT, and every
-- autosave goes through it; a scheduled reply parked there would be rewritten
-- the next time the user opened a reply to the same message. A frozen,
-- guard-checked request needs to be unreachable by any autosave path.
--
-- `request` is the send route's own JSON body, frozen at schedule time
-- (`confirm` forced true, `draft_id` stripped): every field the send body
-- grows later is scheduled for free, and the worker deserializes and calls the
-- same send the handler calls with no mapping code. CONTENT: never logged,
-- never audited, never copied into `last_error`.
--
-- STATE MACHINE:
--   scheduled --claim (lease)--> sending --settle--> sent
--        ^                         |  |
--        |---- retry / requeue ----'  '--settle (attempts spent)--> failed
--   scheduled | failed --cancel--> cancelled      failed --send now--> scheduled
-- A row is sent ONLY from `sending` under a lease, and a lease that lapsed
-- (a crash mid-send) is answered by searching Gmail for `message_id_header`
-- BEFORE the row is retried, as is every re-fire of a row whose earlier
-- attempt may have gone (`maybe_sent`): that minted Message-ID is the whole
-- double-send defense. Every settle names the claim it settles (`attempts`),
-- so a stale worker's settle is a no-op. `sending` is never cancellable: the
-- mail is in Gmail's hands. `sent` and `cancelled` rows are swept a week
-- after they settle, `failed` rows after thirty days.
--
-- Timestamps are RFC3339 text written by one formatter, so the `send_at <= ?`
-- and `lease_until <= ?` comparisons are text comparisons that order right.
CREATE TABLE IF NOT EXISTS scheduled_sends (
    id                INTEGER PRIMARY KEY,
    account_id        INTEGER NOT NULL,
    request           TEXT NOT NULL,
    send_at           TEXT NOT NULL,
    state             TEXT NOT NULL DEFAULT 'scheduled',
    lease_until       TEXT,              -- set while `sending`, NULL otherwise
    attempts          INTEGER NOT NULL DEFAULT 0,
    last_error        TEXT,              -- a REDACTED reason, never content
    message_id_header TEXT NOT NULL,     -- `<48 hex@passband>`, minted at schedule time
    sent_message_id   INTEGER,           -- local echo id once sent
    sent_thread_id    TEXT,
    -- STICKY 1 once any attempt MAY have reached Gmail (a failure after the
    -- send was posted, or a lapsed lease): every later fire searches Gmail for
    -- `message_id_header` first. Added to older DBs by migrate.rs.
    maybe_sent        INTEGER NOT NULL DEFAULT 0,
    -- Not before this may recovery ask Gmail about a lapsed `sending` row
    -- again (set when a search could not be made). Added by migrate.rs too.
    recover_at        TEXT,
    -- When the row's last attempt ENDED (its retry or failed settle; a lapsed
    -- lease's end on a requeue). A `maybe_sent` row is not fired again before
    -- this plus the recovery grace, whoever pulled it forward. Added by
    -- migrate.rs.
    last_attempt_at   TEXT,
    -- The first time a pre-fire search could not be made, kept across the
    -- "not now" deferrals that follow it, so an unreachable search gives up
    -- instead of deferring forever. NULL once a search answers. Migrate.rs.
    search_deferred_since TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_scheduled_sends_due
    ON scheduled_sends(account_id, state, send_at);

-- OUTBOUND READ TRACKING. One `send_trackers` row per tracked send: the minted
-- token IS the pixel URL's path segment, so it is the only thing a recipient's
-- mail client ever hands back. Tracking is opt-in per send AND requires
-- `[tracking] base_url`; an untracked send writes nothing here.
--
-- `message_id` is the LOCAL messages.id of the echoed copy of the sent mail,
-- backfilled after the send's echo lands, and NULL when the echo did not (a
-- sealed or failed echo). Timestamps are unix seconds, not the RFC3339 text the
-- mail tables use — nothing here is a mail date, and the opens feed carries
-- epoch integers end to end.
--
-- PRIVACY: a token is unguessable capability material minted per send. It is
-- never logged and never crosses the agent door; /mcp does not know these
-- tables exist.
CREATE TABLE IF NOT EXISTS send_trackers (
    token      TEXT PRIMARY KEY,
    account_id INTEGER NOT NULL,
    message_id INTEGER,
    created_at INTEGER NOT NULL
);
-- The reads that start from a MESSAGE rather than a token (`message_opens`, the
-- sent listing's and the group listing's per-row open counts) need this, or
-- each is a scan of every tracker the account ever minted. Partial, in the
-- file's usual shape: every one of them constrains `message_id = ...`, which
-- implies NOT NULL, and a tracker with no message is one nothing reads by
-- message.
CREATE INDEX IF NOT EXISTS idx_send_trackers_message
    ON send_trackers(account_id, message_id) WHERE message_id IS NOT NULL;

-- ONE ROW PER OBSERVED OPEN, append-only. Rows arrive two ways: the daemon's own
-- unauthenticated GET /t/:token, and the opens poller draining the relay. Both
-- insert ONLY for a token that names an existing `send_trackers` row, so an
-- unknown token leaves no trace.
--
-- `classification` is what the fetch's User-Agent implies, never a claim about a
-- human: 'proxied' for Gmail's image proxy (an open that may be a cache warm),
-- 'unknown' otherwise.
CREATE TABLE IF NOT EXISTS message_opens (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    token          TEXT NOT NULL,
    opened_at      INTEGER NOT NULL,
    user_agent     TEXT,
    classification TEXT NOT NULL DEFAULT 'unknown'
);

-- The read pattern is exactly "every open for this token".
CREATE INDEX IF NOT EXISTS idx_message_opens_token ON message_opens(token);

-- PER-DEVICE HUMAN-DOOR TOKENS. One row per credential the human door will
-- accept, issued by `squelchd token issue` or by a successful pairing claim.
-- This is what lets the door say "this phone, revoked" instead of "rotate the
-- one shared secret and re-key every client".
--
-- SQUELCH_API_TOKEN is NOT represented here and never will be: the env var stays
-- the self-host master key, checked first and independently, so a store that has
-- lost every row still has an operator way in.
--
-- `token_hash` is the lowercase hex SHA-256 of the FULL presented token, prefix
-- included. THE PLAINTEXT IS NEVER STORED — it exists once, at mint, on its way
-- to the operator's terminal or the pairing response. UNIQUE both dedupes the
-- (astronomically unlikely) collision and gives the per-request point lookup its
-- index; verification is a lookup by hash, so there is no scan to time.
--
-- `revoked_at` is a TOMBSTONE, not a delete: revocation must be visible in
-- `token list` after the fact, and reusing an id would be a footgun for anything
-- that recorded one. A row with `revoked_at` set can never authenticate again.
--
-- `last_used_at` is liveness only, written at most once a minute per token (see
-- `verify_device_token`) so an SSE-driven client cannot turn every request into
-- a write.
--
-- `client_*` is WHICH APP last presented this token: a platform from a closed
-- set, a dotted numeric version, and when. All three are NULL until a client
-- that names itself (`X-Passband-Client`) uses the token, and they move
-- TOGETHER or not at all. `client_seen_at` is NOT `last_used_at`: the iOS
-- notification extension presents the same token every time a push lands and
-- names nothing, so `last_used_at` moves with no app open and this does not.
-- A CHANGED platform or version only replaces the stored one after it has been
-- quiet for an hour (`CLIENT_CHANGE_THROTTLE_SECS`), which is what stops a
-- token holder minting a metric series per request.
--
-- `client_action_at` is the last time that app DID something with this token:
-- a successful write on the client door (see `note_client_action`). Narrower
-- than `client_seen_at`, which an open app moves by polling. NULL until the
-- first one, and it is read with `client_platform`, so it says which app acted.
--
-- `client_timezone` is the IANA zone that app's device was in
-- (`X-Passband-Timezone`), written with `client_seen_at` and validated first.
-- The owner's zone is the one the most recently used app reported
-- (`owner_zone`), which is what every agent reads "now" in. NULL until an app
-- that sends it connects; a request without the header leaves it as it was.
CREATE TABLE IF NOT EXISTS device_tokens (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER NOT NULL,   -- -> accounts.id
    token_hash   TEXT NOT NULL UNIQUE,
    -- Operator/device-supplied label, trimmed and length-capped by the store.
    -- Never secret, and the only thing `token list` prints besides timestamps.
    name         TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at   TEXT,
    client_platform TEXT,
    client_version  TEXT,
    client_seen_at  TEXT,
    client_action_at TEXT,
    client_timezone TEXT
);

-- `token list` reads exactly "every token for my account".
CREATE INDEX IF NOT EXISTS idx_device_tokens_account ON device_tokens(account_id);

-- PAIRING CODES. The short-lived, human-transcribable bridge from a paired
-- device to a device token: `squelchd pair` mints one, the app presents it to
-- POST /client/pair, and the claim trades it for a token.
--
-- ONE ACTIVE CODE PER ACCOUNT, enforced by the mint deleting every prior row for
-- the account. A code is worth ~40 bits, which is only safe because it is
-- single-use, expires in minutes, and burns after a handful of misses — so
-- letting several accumulate would multiply the guessing surface for no gain.
-- That also bounds the table at one row per account, which is why there is no
-- index on `code_hash`: the claim's lookup has nothing to scan.
--
-- `code_hash` follows the same discipline as `device_tokens.token_hash`
-- (lowercase hex SHA-256 of the normalized code); the plaintext is never stored.
-- NOT UNIQUE, deliberately: uniqueness would be a cross-account oracle at mint.
--
-- `failed_attempts` counts misses against the live code, not against a caller
-- (the claim is unauthenticated and has no caller identity). At the cap the row
-- is DELETED, which is why burn, expiry and a wrong code are one answer: the
-- claim finds no active row in all three cases and cannot tell them apart.
--
-- `claimed_at` is the one-shot marker. It is a stamp rather than a delete so a
-- replay of a just-used code takes the same "no active row" path as everything
-- else, and the successful pairing stays visible until the next mint.
-- `id` is AUTOINCREMENT, which almost nothing here is, and the audit log is why.
-- The mint SUPERSEDES by DELETE, so a plain INTEGER PRIMARY KEY recycles the
-- rowid and two consecutive pairing windows both audit as `code:1` — mint, claim
-- and burn rows from different windows become impossible to tell apart. A
-- monotonic id costs one extra row in `sqlite_sequence` on a table that holds at
-- most one row, and buys an unambiguous ledger.
CREATE TABLE IF NOT EXISTS pairing_codes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL,   -- -> accounts.id
    code_hash       TEXT NOT NULL,
    expires_at      TEXT NOT NULL,
    failed_attempts INTEGER NOT NULL DEFAULT 0,
    claimed_at      TEXT,
    created_at      TEXT NOT NULL
);

-- The mint's supersede reads exactly "every code for my account".
CREATE INDEX IF NOT EXISTS idx_pairing_codes_account ON pairing_codes(account_id);

-- NORMALIZED SENT RECIPIENTS: one row per (sent message, bare address), written
-- from the SAME faithful mailbox list `messages.to_addrs` is rendered from.
--
-- `to_addrs` is a DISPLAY string (`"Doe, Jane" <j@x>, bob@y`), so the only way to
-- ask "who did I send this to" against it is a `LIKE '%addr%'` scan per address
-- — which is what the send-group history would have had to do, per member, per
-- query. This table makes that an indexed join instead.
--
-- FAITHFUL, NOT FILTERED — deliberately unlike `contacts`, which drops the
-- account's own address and robot addresses to protect "people I know". This
-- answers "who did this go to", so a note to self and a `support@` both belong.
-- `addr` is lowercased and deduped per message; display names live in
-- `to_addrs` and are no part of the key.
--
-- SENT MAIL ONLY. Received mail has no `to_addrs` and writes nothing here, so
-- the table can never be read as an inbound-recipient index.
CREATE TABLE IF NOT EXISTS message_recipients (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,   -- -> messages.id
    addr       TEXT NOT NULL,      -- lowercased bare address, no display name
    PRIMARY KEY(account_id, message_id, addr)
);

-- The group-history read is "every sent message naming any of these addresses",
-- so the address is the leading column.
CREATE INDEX IF NOT EXISTS idx_message_recipients_addr
    ON message_recipients(account_id, addr);

-- A NAMED AUDIENCE the user can address as one ("preseed investors" -> 12
-- mailboxes). HUMAN-DOOR DATA ONLY: served by `/client/groups`, never visible on
-- /mcp — who the user talks to as a bloc is not something the agent door was
-- handed (two-door invariant).
--
-- NOT NAMED `groups`: `GROUPS` is a SQLite window-frame keyword, and while
-- SQLite would accept it unquoted today, every query naming it is one grammar
-- change away from a syntax error.
--
-- `mode` IS THE GROUP'S OWN PROPERTY, not the composer's, because it is a fact
-- about the audience rather than about one message: an investor list is
-- individually-addressed every time or it is not one.
--   'to'         one message, every member in To — they see each other
--   'bcc'        one message, every member in Bcc — they do not
--   'individual' one message PER member, sent separately
-- Unrecognized values read as 'to' (the visible, least surprising shape); see
-- `GroupMode::parse`.
--
-- `slug` is the lowercased, whitespace-collapsed `name`: the uniqueness key, and
-- what composer autocomplete matches, so "Preseed Investors" and
-- "preseed investors" cannot both exist.
CREATE TABLE IF NOT EXISTS send_groups (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    name       TEXT NOT NULL,
    slug       TEXT NOT NULL,
    mode       TEXT NOT NULL DEFAULT 'to',
    note       TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(account_id, slug)
);

-- MEMBERSHIP. `addr` is the lowercased bare address and half the primary key, so
-- adding someone twice is a no-op rather than a duplicate recipient. The
-- `display_name` is a convenience copy for rendering the member list without a
-- join; `contacts` remains the source of truth for who anyone is.
--
-- ON DELETE CASCADE: `PRAGMA foreign_keys = ON` is set at the top of this file,
-- so dropping a group takes its membership with it.
CREATE TABLE IF NOT EXISTS group_members (
    group_id     INTEGER NOT NULL REFERENCES send_groups(id) ON DELETE CASCADE,
    account_id   INTEGER NOT NULL,
    addr         TEXT NOT NULL,
    display_name TEXT,
    added_at     TEXT NOT NULL,
    PRIMARY KEY(group_id, addr)
);

-- The member-side of the history join, and the composer's "expand this group".
CREATE INDEX IF NOT EXISTS idx_group_members_addr
    ON group_members(account_id, addr);

-- ONE ROW PER GROUP SEND, whatever shape it took: a single To/Bcc message or a
-- fan-out of N. This is the RECORDED half of a group's history; the derived half
-- (`message_recipients` joined against `group_members`) is what makes a group
-- created today show the year of mail that preceded it. A group send is recorded
-- here AND matches the derived query, so the history read excludes any message a
-- row here already claims.
--
-- `recipients` is a SNAPSHOT COUNT taken at send time. Membership is mutable, and
-- "sent to 12" must not silently become "sent to 15" when three people join
-- later.
--
-- `group_id` does NOT cascade-delete: deleting a group is a statement about who
-- you will address next, not a licence to erase what you already sent. The rows
-- outlive the group, and the history read tolerates a dangling id.
CREATE TABLE IF NOT EXISTS group_sends (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    group_id   INTEGER NOT NULL,
    subject    TEXT NOT NULL,
    mode       TEXT NOT NULL,
    sent_at    TEXT NOT NULL,
    recipients INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_group_sends_group
    ON group_sends(account_id, group_id, sent_at);

-- ONE ROW PER (GROUP SEND, RECIPIENT). For a To/Bcc send every row names the
-- same `message_id`; for a fan-out each names its own, which is what makes read
-- receipts per-recipient in that mode.
--
-- `message_id` is NULL until the echo of that send lands (and stays NULL when it
-- never does — a sealed or failed echo), exactly like `send_trackers.message_id`.
--
-- `status` is 'sent' | 'failed'. A FAN-OUT CAN PARTIALLY FAIL, and that is a
-- first-class state rather than an error: eleven investors got the update and one
-- did not, and the one thing worse than that is not knowing which. `error` holds
-- the redacted reason for the failed row.
CREATE TABLE IF NOT EXISTS group_send_recipients (
    group_send_id INTEGER NOT NULL REFERENCES group_sends(id) ON DELETE CASCADE,
    account_id    INTEGER NOT NULL,
    addr          TEXT NOT NULL,
    message_id    INTEGER,
    status        TEXT NOT NULL DEFAULT 'sent',
    error         TEXT,
    PRIMARY KEY(group_send_id, addr)
);

-- Agentic triage owns placement and access. Legacy triage only supplies explicit
-- user lifecycle state during the transition; its semantic verdict is ignored.
CREATE TABLE IF NOT EXISTS agent_message_state (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    content_snapshot TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    access TEXT NOT NULL DEFAULT 'pending' CHECK(access IN ('pending','allowed','restricted')),
    PRIMARY KEY(account_id, message_id)
);
CREATE TABLE IF NOT EXISTS agent_triage_jobs (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('triage','notification','deliberate_notification','access')),
    trigger TEXT NOT NULL,
    input_revision INTEGER NOT NULL,
    arrival_eligible INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','leased','completed','failed')),
    available_at TEXT NOT NULL,
    lease_token TEXT,
    lease_until TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    UNIQUE(account_id, message_id, kind, input_revision, trigger)
);
CREATE INDEX IF NOT EXISTS idx_agent_jobs_claim ON agent_triage_jobs(kind,state,available_at);
-- Probe one manual request by equality, including its computed trigger. The
-- trigger expression is on the outer row, so this covering index can serve it.
-- It also carries last_error and available_at, so the progress read can say a
-- run is parked on the daily budget without touching the table. It replaces
-- idx_agent_jobs_manual_progress, which lacked those two columns.
DROP INDEX IF EXISTS idx_agent_jobs_manual_progress;
CREATE INDEX IF NOT EXISTS idx_agent_jobs_retriage_progress
    ON agent_triage_jobs(account_id,message_id,trigger,kind,state,last_error,available_at);

-- Tenant queue scans and active per-thread lease exclusion must not inspect all
-- completed historical jobs on each idle worker poll.
CREATE INDEX IF NOT EXISTS idx_agent_jobs_account_ready ON agent_triage_jobs(account_id,state,kind,available_at);
CREATE INDEX IF NOT EXISTS idx_agent_jobs_active_message ON agent_triage_jobs(account_id,message_id,kind,lease_until) WHERE state='leased';
-- Completed history never belongs in the worker's candidate scan.
CREATE INDEX IF NOT EXISTS idx_agent_jobs_pending_claim
    ON agent_triage_jobs(account_id,available_at,id)
    WHERE state IN ('queued','leased');
CREATE TABLE IF NOT EXISTS agent_message_decisions (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    decision_json TEXT NOT NULL,
    decided_at TEXT NOT NULL,
    PRIMARY KEY(account_id,message_id)
);
-- The few decisions that can hold FYE on their own (`auth_fye_threads_cte!`):
-- a partial index on EXACTLY the expression that query filters by, so FYE
-- membership never parses every decision in the mailbox. Guarded by
-- `json_valid` inside a CASE (evaluated lazily, unlike AND): an index
-- expression that raised on malformed JSON would make that row's INSERT fail.
CREATE INDEX IF NOT EXISTS idx_decisions_critical ON agent_message_decisions(account_id, message_id)
    WHERE CASE WHEN json_valid(decision_json) THEN json_extract(decision_json,'$.auth.critical') END = 1;
CREATE TABLE IF NOT EXISTS agent_message_destinations (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    destination TEXT NOT NULL CHECK(destination = 'reading'),
    PRIMARY KEY(account_id,message_id,destination)
);
CREATE TABLE IF NOT EXISTS agent_thread_attention (
    decision_message_id INTEGER NOT NULL,
    account_id INTEGER NOT NULL,
    thread_id TEXT NOT NULL,
    message_id INTEGER NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    show_in_fye INTEGER NOT NULL,
    attention_json TEXT NOT NULL,
    relevant_activity TEXT NOT NULL,
    unresolved_since TEXT,
    -- THE THREAD IS FIRST-LOAD MAIL: its attention was written by triage of
    -- the first-run backfill and no live mail or human re-triage has touched
    -- it since. Such threads share a small FYE allowance
    -- (`FIRST_LOAD_FYE_CAP`), so a new account opens on a short board rather
    -- than a month of backlog. 0 on every pre-existing row (migrate.rs).
    first_load INTEGER NOT NULL DEFAULT 0,
    -- HELD BACK BY THAT ALLOWANCE: the model picked FYE (so `show_in_fye` is
    -- 0 only because of the cap) and the thread waits to be let in. A held
    -- thread comes back when it turns time-sensitive (`time_sensitive`) or
    -- when something asks for more of the backlog; closing other rows never
    -- refills the board on its own. Always 0 when `first_load` is 0.
    held INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account_id,thread_id)
);
CREATE TABLE IF NOT EXISTS agent_decision_sources (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    source_message_id INTEGER NOT NULL,
    source_revision INTEGER NOT NULL,
    PRIMARY KEY(account_id,message_id,source_message_id)
);
CREATE TABLE IF NOT EXISTS agent_triage_runs (
    id INTEGER PRIMARY KEY,
    job_id INTEGER NOT NULL,
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    completed_at TEXT NOT NULL
);
-- One row per run, kept indefinitely (`retention.rs` can prune it to
-- HISTORY_RETENTION_DAYS, but nothing schedules that pass),
-- so without this the diagnostics read (`agent_diagnostics`: the newest 25
-- runs for one message) is a scan of the whole window. The first open after
-- this shipped built it over the whole table, synchronously, before the
-- daemon served: a one-time cost in proportion to the run history, paid once.
CREATE INDEX IF NOT EXISTS idx_agent_triage_runs_message ON agent_triage_runs(account_id, message_id);
CREATE TABLE IF NOT EXISTS agent_triage_cutover (
    account_id INTEGER PRIMARY KEY,
    completed_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_triage_corrections (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    field TEXT NOT NULL,
    value_json TEXT NOT NULL,
    source_revision INTEGER NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(account_id,message_id,field)
);

-- Ownership marker: only agent-created polling rows are deleted on retraction.
CREATE TABLE IF NOT EXISTS agent_delivery_projections (
    account_id INTEGER NOT NULL,
    shipment_id INTEGER NOT NULL,
    PRIMARY KEY(account_id,shipment_id)
);

-- `shipment_order_links` (which orders each package carries) is created in
-- migrate.rs, NOT here: its creation triggers a one-shot backfill, which only
-- works if this file cannot have created it first.

-- CALENDAR GHOSTS: placeholders for what mail says will or might happen, a
-- projection of the agent's `event` records (sqlite/ghosts.rs). HUMAN-DOOR
-- ONLY. New tables with no backfill, so they live here and need no migrate
-- seam: an older DB gets them on its next open.
--
-- Columns are projection-owned and rebuilt from the assigned records' stored
-- decisions on every reconcile, EXCEPT the user_* columns, which only the user
-- writes. `ends_at` is a STATED end and `est_minutes`/`est_basis` the MODEL's
-- guess: an estimate is never stored as a fact.
--
-- `start_sort` is `starts_at` as a sortable UTC string (`YYYY-MM-DDTHH:MM:SSZ`,
-- a date-only start at 00:00Z of its date), for the listing's coarse SQL
-- range only: `starts_at` keeps the mail's own offset, so it cannot be
-- compared as text, and the exact order is decided in Rust. `venue_key` is
-- `triage::ghost::venue_key(location)`, so the duration prior joins in SQL.
--
-- `owner_scope` is the sender scope (`triage::ghost::sender_scope`) of the
-- record that created the ghost; NULL for a ghost made from unauthenticated
-- mail. `owner_addr` is that record's From address, lowercased (NULL when
-- `owner_scope` is): matching by thread needs the identical address, because
-- no list of domains strangers share is ever complete; matching by ref needs
-- only the scope. Only a record of the owner's scope is ever assigned to the
-- ghost, so another sender replying in its thread makes a ghost of its own,
-- and a NULL-owner ghost takes nothing from other mail. The two are set at
-- creation and change in one case only: a re-decision of the one message
-- that alone feeds the ghost re-owns it to that message's current scope and
-- address (its authentication result can arrive or go on a re-sync).
--
-- `supplier_message_id` is the message whose record supplied the served facts.
-- The listing serves a ghost only while THAT message is eligible (not sent,
-- spam, restricted or sealed), so a status change no rebuild has seen yet
-- cannot serve facts from mail that may no longer feed a ghost. NULL when no
-- assigned record is eligible, or when the supplier was deleted: the facts
-- are kept, the ghost is hidden until a rebuild.
--
-- The user's end correction is a SPAN from the start (`user_end_minutes` for a
-- timed ghost, `user_end_days` for an all-day one), so mail that moves the
-- start moves the end with it. When mail changes the start's precision the
-- rebuild CLEARS the span at the other precision (the one projection write to
-- a user column), so a correction cannot come back after a second change.
--
-- No CHECK on kind, commitment or user_state: those vocabularies are expected
-- to grow (held, materialized), and SQLite cannot alter a CHECK.
CREATE TABLE IF NOT EXISTS ghost_events (
    id                  INTEGER PRIMARY KEY,
    account_id          INTEGER NOT NULL,
    kind                TEXT NOT NULL,
    title               TEXT NOT NULL,
    starts_at           TEXT,
    start_sort          TEXT,
    start_timezone      TEXT,
    ends_at             TEXT,
    est_minutes         INTEGER,
    est_basis           TEXT,
    location            TEXT,
    venue_key           TEXT,
    commitment          TEXT NOT NULL,
    owner_scope         TEXT,
    owner_addr          TEXT,
    mail_state         TEXT NOT NULL DEFAULT 'live' CHECK(mail_state IN ('live','cancelled')),
    likelihood_band     TEXT,
    likelihood_reason   TEXT,
    supplier_message_id INTEGER,
    last_message_id     INTEGER,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    user_state          TEXT,
    user_state_at       TEXT,
    user_end_minutes    INTEGER,
    user_end_days       INTEGER
);
CREATE INDEX IF NOT EXISTS idx_ghost_events_start ON ghost_events(account_id, start_sort);
-- How later mail finds a ghost: `ref:<sender scope>:<ref>` for every identity
-- ref of the ghost's currently assigned records whose decision is readable and
-- whose message is still of the owner's scope, eligible or not (keys are never
-- served, and a record coming back from a seal must find its own ghost),
-- recomputed on each rebuild, so a key leaves with its record. NOT unique: two
-- ghosts may hold one key (a flight number on two dates), and matching picks
-- between them.
CREATE TABLE IF NOT EXISTS ghost_event_keys (
    account_id INTEGER NOT NULL,
    ghost_id   INTEGER NOT NULL,
    key        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ghost_event_keys_key ON ghost_event_keys(account_id, key);
CREATE INDEX IF NOT EXISTS idx_ghost_event_keys_ghost ON ghost_event_keys(account_id, ghost_id);
-- The ASSIGNMENT of one record to one ghost: record `position` (its index
-- among the message's laundered event records, `triage::ghost::launder_events`)
-- of `message_id`. A record belongs to exactly one ghost; ghosts are never
-- merged. `role` is the record's `change`. Every record had the ghost's
-- `owner_scope` when it was assigned; a rebuild re-reads its message's scope
-- and a record whose message no longer has it contributes nothing.
CREATE TABLE IF NOT EXISTS ghost_event_sources (
    account_id INTEGER NOT NULL,
    ghost_id   INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    position   INTEGER NOT NULL,
    role       TEXT NOT NULL,
    PRIMARY KEY(account_id, message_id, position)
);
CREATE INDEX IF NOT EXISTS idx_ghost_event_sources_ghost ON ghost_event_sources(account_id, ghost_id);
-- The user's own corrections of how long things at a venue run, by kind:
-- tier 2 of the estimated end. Never for a kind with no default duration
-- (travel). `ghost_id` is the ghost whose correction wrote the current value,
-- so clearing that correction can take back what it taught.
CREATE TABLE IF NOT EXISTS event_duration_priors (
    account_id INTEGER NOT NULL,
    venue_key  TEXT NOT NULL,
    kind       TEXT NOT NULL,
    minutes    INTEGER NOT NULL,
    ghost_id   INTEGER,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(account_id, venue_key, kind)
);
-- Keys and sources go with their ghost, and a source row with its message,
-- whoever deletes it, as `shipment_order_links` follows `shipments`. A ghost
-- the deleted message alone fed goes too: nothing is left to say it exists.
CREATE TRIGGER IF NOT EXISTS ghost_event_rows_follow_ghost
AFTER DELETE ON ghost_events BEGIN
    DELETE FROM ghost_event_keys WHERE account_id = OLD.account_id AND ghost_id = OLD.id;
    DELETE FROM ghost_event_sources WHERE account_id = OLD.account_id AND ghost_id = OLD.id;
END;
CREATE TRIGGER IF NOT EXISTS ghost_event_sources_follow_message
AFTER DELETE ON messages BEGIN
    DELETE FROM ghost_events
    WHERE account_id = OLD.account_id
      AND id IN (SELECT ghost_id FROM ghost_event_sources
                 WHERE account_id = OLD.account_id AND message_id = OLD.id)
      AND NOT EXISTS (SELECT 1 FROM ghost_event_sources s
                      WHERE s.account_id = OLD.account_id AND s.ghost_id = ghost_events.id
                        AND s.message_id <> OLD.id);
    -- A surviving ghost whose supplier was this message still carries that
    -- message's facts, and no SQL can rebuild them from another source. So
    -- it is HIDDEN (no supplier) until the next reconcile of a message that
    -- feeds it, rather than served under another message's name.
    UPDATE ghost_events SET supplier_message_id = NULL
    WHERE account_id = OLD.account_id AND supplier_message_id = OLD.id;
    DELETE FROM ghost_event_sources WHERE account_id = OLD.account_id AND message_id = OLD.id;
END;
-- THE AUTO-DONE LEDGER (docs/CALENDAR.md §3.8): one row per message the
-- daemon, not the owner, marked done, written in the same transaction as the
-- status. `source` says what did it: 'agent_revisit' is the triage agent's
-- `done` on a scheduled re-check, applied past the rails in
-- `agent_triage::apply_done`. A message with a row here is never auto-done
-- again, so an owner who reopens or undoes one keeps it; the row stays when
-- they do. `reason` is MODEL-AUTHORED and UNTRUSTED, capped on the way in:
-- never logged, never placed in a prompt. While the triage row's
-- `resolved_at` is still `done_at`, the done is the daemon's and not the
-- owner's (the rule suggester reads it so).
CREATE TABLE IF NOT EXISTS auto_done (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    source     TEXT NOT NULL,
    reason     TEXT NOT NULL,
    done_at    TEXT NOT NULL,
    PRIMARY KEY(account_id, message_id)
);
-- The row goes with its message, as the ghost sources do.
CREATE TRIGGER IF NOT EXISTS auto_done_follows_message
AFTER DELETE ON messages BEGIN
    DELETE FROM auto_done WHERE account_id = OLD.account_id AND message_id = OLD.id;
END;
-- THE PAST-EVENT RE-CHECK LEDGER (docs/CALENDAR.md §3.8): one row per message
-- the poll tick queued a `revisit:event_ended:<ask_at>` triage job for
-- (`SqliteStore::schedule_past_event_revisits`), or whose ask a `revisit:` run
-- already answered. `ask_at` is the last ask time queued or answered, computed
-- from the model's own event dates. Whether it was ANSWERED is read from
-- `agent_triage_runs` (an `applied` run of a `revisit:` job at or after it),
-- not from this row: a dropped or failed job is asked again. `asked_at` is
-- when its dates were last read, so a tick on answered mail whose ghosts and
-- source decisions have not changed since reads no decision JSON for it. Ids
-- and times only.
CREATE TABLE IF NOT EXISTS event_revisit_asked (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    ask_at     TEXT NOT NULL,
    asked_at   TEXT NOT NULL,
    -- 1: read and ruled out (not eligible, or somebody else's words in its
    -- thread); read again only once its dates or decision change.
    skipped    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account_id, message_id)
);
-- The row goes with its message, as the auto-done ledger's does.
CREATE TRIGGER IF NOT EXISTS event_revisit_asked_follows_message
AFTER DELETE ON messages BEGIN
    DELETE FROM event_revisit_asked WHERE account_id = OLD.account_id AND message_id = OLD.id;
END;

-- GOOGLE CALENDAR, READ ONLY (docs/CALENDAR.md §8, "Calendar sync"). Written
-- only by the calendar sync loop, read only by the human door's timeline:
-- HUMAN-DOOR ONLY, never on an agent route, never in a triage prompt.
--
-- One row per calendar the account has read. `calendar_id` is `primary` for
-- the primary calendar (Google's alias for it, and all the daemon knows
-- without the list scope), Google's id otherwise. `selected` is whether the
-- latest pass reads it; a calendar that stops being read keeps its row with
-- `selected = 0` and loses its events and sync state. `sync_token` carries
-- incremental sync; `full_synced_at` is the last full read of the window, which
-- is repeated daily because a sync token never carries an event that moved
-- INTO the window as it slides. `last_error_kind` is `auth` / `quota` /
-- `unavailable`, cleared by the next good read. `failures` counts reads that
-- failed in a row and `retry_at` is when a SECONDARY calendar is tried again,
-- so one broken shared calendar backs off on its own. `truncated` says the
-- per-calendar event cap dropped events (the farthest from now). `color` is
-- `#rrggbb` (Google's `backgroundColor`), `access_role` Google's `accessRole`
-- (`owner` and `writer` are the calendars the app may write events to); the
-- primary calendar read without the list scope is `owner` with no colour.
CREATE TABLE IF NOT EXISTS calendar_calendars (
    account_id      INTEGER NOT NULL,
    calendar_id     TEXT NOT NULL,
    summary         TEXT,
    is_primary      INTEGER NOT NULL DEFAULT 0,
    selected        INTEGER NOT NULL DEFAULT 1,
    time_zone       TEXT,
    sync_token      TEXT,
    synced_at       TEXT,
    full_synced_at  TEXT,
    last_error_kind TEXT,
    failures        INTEGER NOT NULL DEFAULT 0,
    retry_at        TEXT,
    truncated       INTEGER NOT NULL DEFAULT 0,
    color           TEXT,
    access_role     TEXT,
    PRIMARY KEY(account_id, calendar_id)
);
-- The events of the synced window (a month back, 400 days ahead), recurring
-- events as their instances. `starts_at` / `ends_at` use the ghosts' text form
-- and convention (`YYYY-MM-DD` for all-day, RFC3339 with Google's offset
-- otherwise; an all-day end is the INCLUSIVE last day, converted from
-- Google's exclusive `end.date`, and NULL for a one-day event) and
-- `start_sort` the ghosts' UTC sort key. `status` is `confirmed` or `tentative`: a cancelled event is
-- deleted, never stored. `transparent` is "free" on the user's calendar.
-- `color_id` is the event's own colour (an id into Google's event palette),
-- `organizer_self` Google's `organizer.self` (this calendar organizes it),
-- `description` laundered text of at most 2000 characters. Also written,
-- one row at a time, after Google accepts the user's own create or edit.
-- `guests_truncated` says Google holds more attendees than
-- `calendar_event_guests` does for this event (Google's `attendeesOmitted`,
-- set past 100): its guest list is then read-only. `organizer_unreadable`
-- says Google named an organizer that is not this calendar and whose address
-- could not be read: somebody else's event, never the user's own to edit.
CREATE TABLE IF NOT EXISTS calendar_events (
    account_id      INTEGER NOT NULL,
    calendar_id     TEXT NOT NULL,
    event_id        TEXT NOT NULL,
    ical_uid        TEXT,
    status          TEXT NOT NULL,
    summary         TEXT,
    location        TEXT,
    starts_at       TEXT NOT NULL,
    start_sort      TEXT NOT NULL,
    ends_at         TEXT,
    all_day         INTEGER NOT NULL,
    start_timezone  TEXT,
    organizer_email TEXT,
    self_response   TEXT,
    transparent     INTEGER NOT NULL DEFAULT 0,
    html_link       TEXT,
    updated_at      TEXT,
    color_id        TEXT,
    organizer_self  INTEGER NOT NULL DEFAULT 0,
    description     TEXT,
    guests_truncated INTEGER NOT NULL DEFAULT 0,
    organizer_unreadable INTEGER NOT NULL DEFAULT 0,
    from_gmail      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account_id, calendar_id, event_id)
);
CREATE INDEX IF NOT EXISTS idx_calendar_events_start ON calendar_events(account_id, start_sort);
-- An event's attendees as Google lists them, at most 100, in Google's order
-- (`position`). OTHER PEOPLE'S ADDRESSES: HUMAN-DOOR ONLY like the event
-- itself, never logged, never in the audit log (which carries a count).
-- `response` is `accepted` / `declined` / `tentative` / `pending`;
-- `is_organizer` and `is_self` are Google's `organizer` and `self` flags.
-- Replaced whole each time the event is written, and gone with it, whoever
-- deletes it (the trigger), so no path can leave a guest list behind.
CREATE TABLE IF NOT EXISTS calendar_event_guests (
    account_id   INTEGER NOT NULL,
    calendar_id  TEXT NOT NULL,
    event_id     TEXT NOT NULL,
    position     INTEGER NOT NULL,
    email        TEXT NOT NULL,
    name         TEXT,
    response     TEXT NOT NULL,
    is_organizer INTEGER NOT NULL DEFAULT 0,
    is_self      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account_id, calendar_id, event_id, position)
);
CREATE TRIGGER IF NOT EXISTS calendar_event_guests_follow_event
AFTER DELETE ON calendar_events BEGIN
    DELETE FROM calendar_event_guests
    WHERE account_id = OLD.account_id AND calendar_id = OLD.calendar_id
      AND event_id = OLD.event_id;
END;
-- The calendar card's PLACE for an event location (gcal/place.rs): what the
-- small model made of `raw`, a location exactly as stored on calendar_events.
-- `place` NULL means the text names no place, and the card shows none. Keyed
-- by the text, so every event at one place shares one call. A row whose text
-- no event holds any more is pruned after each batch. HUMAN DOOR ONLY, like
-- the events it describes.
-- PASSBAND'S OWN EVENTS (gcal/own.rs): which event on the user's calendar
-- Passband made for a ghost, in place of Gmail's copy, and keeps current from
-- the mail. `state` is `owned` (kept current), `released` (the user deleted
-- it or dismissed the ghost: never adopted again), `refused` (Google would
-- not delete Gmail's copy: left alone) or `undo` (that, and taking Passband's
-- copy back off failed too: retried each pass). `fingerprint` is what the mail said
-- when Passband last wrote the event. One row per ghost; a ghost that goes
-- takes its row with it, the event stays. HUMAN DOOR ONLY.
CREATE TABLE IF NOT EXISTS calendar_owned (
    account_id  INTEGER NOT NULL,
    ghost_id    INTEGER NOT NULL,
    calendar_id TEXT NOT NULL,
    event_id    TEXT NOT NULL,
    state       TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY(account_id, ghost_id)
);
-- A write gcal/own.rs had refused by Google (not a quota answer, not a lost
-- reply), by the event it was aimed at, so the same step is not planned and
-- spent again every pass. Left alone until `refused_at` is a week old, then
-- pruned and tried once more. HUMAN DOOR ONLY.
CREATE TABLE IF NOT EXISTS calendar_own_refused (
    account_id  INTEGER NOT NULL,
    calendar_id TEXT NOT NULL,
    event_id    TEXT NOT NULL,
    refused_at  TEXT NOT NULL,
    PRIMARY KEY(account_id, calendar_id, event_id)
);
CREATE TABLE IF NOT EXISTS calendar_places (
    account_id     INTEGER NOT NULL,
    raw            TEXT NOT NULL,
    -- The place's name, and its street address when the location gives one
    -- (gcal/place.rs). Both NULL: the location names no place.
    place          TEXT,
    address        TEXT,
    prompt_version TEXT NOT NULL,
    cleaned_at     TEXT NOT NULL,
    PRIMARY KEY(account_id, raw)
);

-- CALENDAR REMINDERS (gcal/remind.rs, docs/CALENDAR.md "Reminders and
-- tickets"): the small model's plan for one item, `e:<calendar>/<event>` or
-- `g:<ghost id>`, made for one `version` (a hash of what it read) under one
-- prompt. `plan_json` holds whether to remind, the lead, the laundered line,
-- what a tap opens, the tickets pick and the ghosts it said are the same
-- thing, all held to the input in code. Pruned when the item is gone or over
-- (its end, not its start: the tickets button is wanted at the door).
-- HUMAN DOOR ONLY.
CREATE TABLE IF NOT EXISTS calendar_reminders (
    account_id     INTEGER NOT NULL,
    item_key       TEXT NOT NULL,
    version        TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    plan_json      TEXT NOT NULL,
    planned_at     TEXT NOT NULL,
    PRIMARY KEY(account_id, item_key)
);
-- Each reminder that fired: once per item per start (`start_key`, the start
-- as a UTC instant or a date), so a moved event can fire again, at most
-- twice a day (gcal/remind.rs `may_fire`). `covers` is a JSON array of the
-- ghost keys the item stood for, so a ghost and its event never both fire.
-- `frame` is the `calendar_reminder` frame as sent, which the phone fetches
-- by `id` (`GET /client/calendar/reminders/{id}`): AUTOINCREMENT, so an id is
-- never reused once a quiet week's prune empties the table, and a late tap
-- on an old notification cannot open a newer reminder. `fired_at` is Unix
-- seconds. Kept 7 days. HUMAN DOOR ONLY.
CREATE TABLE IF NOT EXISTS calendar_reminder_fires (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL,
    item_key   TEXT NOT NULL,
    start_key  TEXT NOT NULL,
    fired_at   INTEGER NOT NULL,
    covers     TEXT NOT NULL DEFAULT '[]',
    frame      TEXT NOT NULL,
    UNIQUE(account_id, item_key, start_key)
);
-- The planning backoff (gcal/remind.rs `backoff`): an item whose last asks
-- failed, how many in a row, and when it may be asked again (Unix seconds).
-- Cleared by a plan; pruned with the plans. HUMAN DOOR ONLY.
CREATE TABLE IF NOT EXISTS calendar_reminder_backoff (
    account_id INTEGER NOT NULL,
    item_key   TEXT NOT NULL,
    failures   INTEGER NOT NULL,
    retry_at   INTEGER NOT NULL,
    PRIMARY KEY(account_id, item_key)
);
-- The reminder planner's model calls ATTEMPTED per UTC day (timeouts and
-- failures too, which the usage ledger never hears of), so the daily cap
-- holds across restarts. Days before yesterday are pruned. HUMAN DOOR ONLY.
CREATE TABLE IF NOT EXISTS calendar_reminder_calls (
    account_id INTEGER NOT NULL,
    day        TEXT NOT NULL,
    calls      INTEGER NOT NULL,
    PRIMARY KEY(account_id, day)
);

-- CALENDAR SEARCH (docs/CALENDAR.md §10.1): a word index over the synced
-- events, for the search box, the assistant and Agent MCP. `calendar_search_keys`
-- gives each event a stable integer id, because `calendar_events` has none (an
-- implicit rowid can be renumbered by VACUUM), and that id is the FTS rowid.
-- Written in ONE place, `upsert_event`, inside the event's own transaction and
-- after its guests; deleted with the event by the trigger below, whichever path
-- deletes it. `people` is guest names, `addresses` every address the event
-- holds: the organizer's and guests', a name that is an address, and each one
-- found in the title, place or description (cut out of those columns). The
-- agent door's queries never name `addresses`. NOT STEMMED: porter would store
-- "meeting" as "meet" and a half-typed "meetin" would find nothing; plurals
-- are folded in the query instead (`gcal::search::forms`). An UPDATE drops the
-- row (the trigger), so an older daemon that edits events leaves the index
-- out of step, never stale, and the open after it reindexes.
CREATE TABLE IF NOT EXISTS calendar_search_keys (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    calendar_id TEXT NOT NULL,
    event_id    TEXT NOT NULL,
    UNIQUE(account_id, calendar_id, event_id)
);
CREATE VIRTUAL TABLE IF NOT EXISTS calendar_search_fts USING fts5(
    title, place, notes, people, addresses,
    tokenize = 'unicode61', prefix = '2 3'
);
CREATE TRIGGER IF NOT EXISTS calendar_search_drops_an_edited_event
AFTER UPDATE ON calendar_events BEGIN
    DELETE FROM calendar_search_fts WHERE rowid IN (
        SELECT id FROM calendar_search_keys
        WHERE account_id = OLD.account_id AND calendar_id = OLD.calendar_id
          AND event_id = OLD.event_id);
    DELETE FROM calendar_search_keys
    WHERE account_id = OLD.account_id AND calendar_id = OLD.calendar_id
      AND event_id = OLD.event_id;
END;
CREATE TRIGGER IF NOT EXISTS calendar_search_follows_event
AFTER DELETE ON calendar_events BEGIN
    DELETE FROM calendar_search_fts WHERE rowid IN (
        SELECT id FROM calendar_search_keys
        WHERE account_id = OLD.account_id AND calendar_id = OLD.calendar_id
          AND event_id = OLD.event_id);
    DELETE FROM calendar_search_keys
    WHERE account_id = OLD.account_id AND calendar_id = OLD.calendar_id
      AND event_id = OLD.event_id;
END;

-- A CORRESPONDENT'S TIMEZONE (docs/CALENDAR.md §7), inferred at ingest and
-- served on the contact card. HUMAN-DOOR ONLY. `addr` is COLLATE NOCASE in
-- the column itself, so the key, the index and every comparison agree that
-- `Dan@X.com` and `dan@x.com` are one person without each query having to
-- remember (see `idx_contacts_addr_nocase` for what forgetting cost).
--
-- One row per invitation that named a zone: the iCalendar ORGANIZER, credited
-- with the TZID of the first DTSTART under its canonical name
-- (`timezone::canonical_zone`). Written ONLY for authenticated mail whose From
-- is the organizer's own address (`zones::invitation_is_bound`), so a forged
-- invitation leaves no row. Keyed by message, so a re-ingest
-- replaces its own row instead of adding one, and the row goes with its
-- message (trigger below). Rows are written for spam and sent mail too (never
-- for an invitation the account itself organized); the inference takes the
-- newest 20 not dated more than a day ahead and counts those whose message is
-- inbound and not spam, so a message rescued from spam starts counting
-- without a rewrite.
CREATE TABLE IF NOT EXISTS sender_zone_invites (
    account_id  INTEGER NOT NULL,
    addr        TEXT NOT NULL COLLATE NOCASE,
    zone        TEXT NOT NULL,
    message_id  INTEGER NOT NULL,
    -- The message's received_at, RFC3339 UTC: "newest invitations" order.
    observed_at TEXT NOT NULL,
    PRIMARY KEY(account_id, addr, message_id)
);
CREATE INDEX IF NOT EXISTS idx_sender_zone_invites_message
    ON sender_zone_invites(account_id, message_id);
-- The recompute's read: one person's newest invitations up to the future
-- bound, straight off this index in order (`message_id` is the tie-break),
-- so only the newest few are ever looked up in `messages`.
CREATE INDEX IF NOT EXISTS idx_sender_zone_invites_recent
    ON sender_zone_invites(account_id, addr, observed_at, message_id);
CREATE TRIGGER IF NOT EXISTS sender_zone_invites_follow_message
AFTER DELETE ON messages BEGIN
    DELETE FROM sender_zone_invites
    WHERE account_id = OLD.account_id AND message_id = OLD.id;
END;
-- The result, one row per person. `zone`/`source`/`confidence` are the
-- INFERENCE's (source 'invite' or 'offsets'; all three NULL when it has no
-- answer) and are rewritten whenever a message that carries an observation
-- from this person is ingested. `user_zone` is the user's override, written
-- only by `PUT /client/person/timezone` and never by inference; the read
-- side serves it first. Never a row for the account's own address.
CREATE TABLE IF NOT EXISTS sender_zones (
    account_id  INTEGER NOT NULL,
    addr        TEXT NOT NULL COLLATE NOCASE,
    zone        TEXT,
    source      TEXT,
    confidence  TEXT,
    updated_at  TEXT NOT NULL,
    user_zone   TEXT,
    PRIMARY KEY(account_id, addr)
);

-- Independent model assessments for human notification diagnostics. Multiple
-- attempts remain inspectable indefinitely (`retention.rs` can prune them, but
-- nothing schedules that pass);
-- the notification ledger still records delivery, and it, not
-- this table, is what stops a second model call.
CREATE TABLE IF NOT EXISTS notification_assessments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    lane TEXT NOT NULL,
    is_auth INTEGER NOT NULL CHECK(is_auth IN (0, 1)),
    importance INTEGER NOT NULL CHECK(importance BETWEEN 0 AND 100),
    one_line TEXT NOT NULL,
    reason TEXT NOT NULL,
    model TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    assessed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_notification_assessments_latest
    ON notification_assessments(account_id, message_id, lane, id DESC);

-- Scheduling metadata stays separate from the immutable request/run history.
CREATE TABLE IF NOT EXISTS agent_job_lanes (
    job_id INTEGER PRIMARY KEY REFERENCES agent_triage_jobs(id) ON DELETE CASCADE,
    foreground INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS agent_triage_followups (
    job_id INTEGER PRIMARY KEY REFERENCES agent_triage_jobs(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    trigger TEXT NOT NULL,
    arrival_eligible INTEGER NOT NULL DEFAULT 0,
    -- A human asked for this one message while its job was running, so the
    -- re-run keeps the foreground lane (migrate.rs adds it to older DBs).
    foreground INTEGER NOT NULL DEFAULT 0
);
-- FYE is a thread-level human choice, so a sibling cannot undo the correction.
CREATE TABLE IF NOT EXISTS agent_thread_preferences (
    account_id INTEGER NOT NULL,
    thread_id TEXT NOT NULL,
    show_in_fye INTEGER NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(account_id,thread_id)
);

-- Attention may be updated from a different conversation without borrowing that
-- conversation's message identity, categories, summary, or typed records.
CREATE TABLE IF NOT EXISTS agent_attention_sources (
    account_id INTEGER NOT NULL,
    thread_id TEXT NOT NULL,
    source_message_id INTEGER NOT NULL,
    source_revision INTEGER NOT NULL,
    PRIMARY KEY(account_id,thread_id,source_message_id)
);
CREATE INDEX IF NOT EXISTS idx_agent_attention_source_reverse ON agent_attention_sources(account_id,source_message_id);
CREATE INDEX IF NOT EXISTS idx_agent_decision_source_reverse ON agent_decision_sources(account_id,source_message_id);

-- Repair beta related-thread projections without losing their evidence guard.
-- Classification belongs to the target; the updating decision is provenance.
--
-- ON EVERY OPEN, not behind a done-flag: the message upsert takes Gmail's
-- thread id as it stands, so a message Gmail re-threads can leave an
-- attention row whose decision message is no longer in its thread, the shape
-- the beta wrote.
--
-- THE PROBE IS PINNED to `idx_messages_thread`, which holds (account, thread,
-- id) and so answers it from the index alone. By rowid the planner reads each
-- decision message's row, three times over every thread with attention: 111 ms
-- of every open at 50k messages, and a read of thousands of scattered pages
-- on a cold file. Pinned, it is 17 ms. The index is created above, in this
-- same batch.
INSERT OR IGNORE INTO agent_attention_sources(account_id,thread_id,source_message_id,source_revision)
SELECT a.account_id,a.thread_id,d.source_message_id,d.source_revision
FROM agent_thread_attention a JOIN agent_decision_sources d
  ON d.account_id=a.account_id AND d.message_id=a.decision_message_id
WHERE NOT EXISTS (SELECT 1 FROM messages m INDEXED BY idx_messages_thread WHERE m.account_id=a.account_id
                  AND m.id=a.decision_message_id AND m.thread_id=a.thread_id);
INSERT OR IGNORE INTO agent_attention_sources(account_id,thread_id,source_message_id,source_revision)
SELECT a.account_id,a.thread_id,a.decision_message_id,COALESCE(s.revision,0)
FROM agent_thread_attention a
LEFT JOIN agent_message_state s ON s.account_id=a.account_id AND s.message_id=a.decision_message_id
WHERE NOT EXISTS (SELECT 1 FROM messages m INDEXED BY idx_messages_thread WHERE m.account_id=a.account_id
                  AND m.id=a.decision_message_id AND m.thread_id=a.thread_id);
UPDATE agent_thread_attention SET decision_message_id=message_id
WHERE NOT EXISTS (SELECT 1 FROM messages m INDEXED BY idx_messages_thread WHERE m.account_id=agent_thread_attention.account_id
                  AND m.id=agent_thread_attention.decision_message_id AND m.thread_id=agent_thread_attention.thread_id);

-- THE ARCHIVE WALK'S CURSOR, one row per account. `walked_to` is the oldest
-- instant the walk has fully covered: everything between it and the backfill
-- window is stored. Created at `now - backfill_days` on first use and only ever
-- moved backwards, a chunk at a time; the walk is complete once it reaches
-- `now - archive_days`, and raising that setting resumes it from here.
--
-- `covered_to` is the FORWARD edge: the newest instant up to which mail that
-- never carried INBOX or SENT (filed by a filter, archived before a sync) has
-- been stored. It starts where `walked_to` started and only ever moves
-- forwards, staying a day behind the clock. NULL on a row written before the
-- column existed, which reads as `walked_to`.
CREATE TABLE IF NOT EXISTS archive_walk (
    account_id INTEGER PRIMARY KEY,
    walked_to  TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    covered_to TEXT
);

-- THE OWNER'S RULING ON AUTH MAIL ("That was me" / "Not me"), one row per
-- message. Written only by the human door (`PUT /client/sealed/{id}/verdict`,
-- audited) and served only there; no agent-door path returns a verdict. It
-- moves FYE membership: `not_mine` puts the message there, and `mine` takes a
-- critical account change back out (see `auth_fye_sql!`). The FYE list is
-- shared with the agent door, so an UNRESTRICTED message's presence there can
-- reflect a ruling; restricted mail never reaches the agent wherever it sits.
-- It also keeps evidence from the shredder. No row is no ruling. It outlives
-- re-triage on purpose: the owner ruled on the message, not on one model
-- run's reading of it. A ruling on a legacy sealed row (no agent decision) is
-- kept and served but never moves FYE.
CREATE TABLE IF NOT EXISTS auth_verdicts (
    account_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    verdict    TEXT NOT NULL CHECK(verdict IN ('mine','not_mine')),
    decided_at TEXT NOT NULL,
    PRIMARY KEY(account_id, message_id)
);
-- Auto-translate (docs/TRANSLATION.md): the English version of mail written in
-- another language, BESIDE the message. `messages.subject`/`body`/`body_html`
-- are never rewritten. A SERVED translation (`done`, `incomplete`) IS part of
-- the agent's content snapshot, hashed in only when one exists. Ingest
-- inserts `pending` (live) or `deferred` (archive tier, or a backfill over
-- the day's translate budget) for a non-English message; spam, sent, own and
-- sealed mail never get a row. Timestamps are
-- unix seconds. `missing_facts` is a JSON array, set when `incomplete`.
-- `not_needed` is terminal: the translator read the message as English, so
-- the local detection was a false positive and nothing is served.
-- `rejected` is terminal too: the engine refused its own output (a reverse or
-- fact check it could not pass), so nothing is served and nothing is retried.
--
-- `lease` is the claim token: stamped by every claim, matched by every write
-- of a result, cleared by every terminal write. A result whose lease is gone
-- (a heal dropped and re-queued the row, or the lease expired and another
-- pass took it) writes nothing. `triage_wait_until` is how long agent triage
-- holds this message's job back while the row is `pending`, so foreign mail
-- is normally triaged once, on the English (docs/TRANSLATION.md, Pipeline);
-- set for newly arrived live mail only. `releases` counts consecutive claims
-- handed back on a configuration or outage failure without spending an
-- attempt; past two, the next message-shaped such failure (a timeout, a 400)
-- is charged to the row, but only if the gateway has answered some other
-- translation since `first_released_at` (unix MILLISECONDS, the first release
-- of the run, or the last charge; NULL after any verdict). A total outage
-- therefore never spends an attempt.
CREATE TABLE IF NOT EXISTS message_translations (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL,
    source_lang TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('deferred','pending','done','incomplete','failed','not_needed','rejected')),
    subject TEXT,
    body TEXT,
    body_html TEXT,
    snippet TEXT,
    missing_facts TEXT,
    model TEXT,
    prompt_version INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    lease TEXT,
    triage_wait_until INTEGER,
    releases INTEGER NOT NULL DEFAULT 0,
    first_released_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
-- The translate pass's claim: one account's rows by status.
CREATE INDEX IF NOT EXISTS idx_message_translations_claim
    ON message_translations(account_id, status, updated_at);

-- THE RELATIONSHIP LEDGER (docs/UNDERSTANDING.md §2.1): one row per address
-- the owner has live mail with, either way, holding what the owner's own
-- behaviour says about them. Deterministic, no model, and NOTHING IN IT IS
-- EMAIL TEXT: every column is a count or a timestamp off `messages`,
-- `contacts`, `message_recipients`, `triage` and the two correction tables,
-- so the triage agent reads it as trusted context beside `sender_is_contact`.
-- The arithmetic is `crate::relationship`; the counting is
-- `store::sqlite::relationships`.
--
-- HUMAN DOOR AND TRIAGE ONLY, like `contacts`, `send_groups` and the contact
-- card: who the owner answers and how fast is the correspondent graph the
-- agent door (/mcp) was never handed. Triage is the human door's own backend
-- and may read it; no /mcp tool response may carry a column of it, and an
-- invariant test in squelch-mcp holds that.
--
-- MAINTAINED BY A PASS, NOT AT INGEST. Ingest, the not-spam action and the
-- attention POST each add one row to `relationship_dirty` below and nothing
-- else; the refine lane drains that set every round and recomputes only those
-- addresses, each as indexed probes on ONE address, never a directory scan.
-- A full recompute runs once per owner-local day at 03:00, chunked so no chunk
-- holds the store mutex long, because the monthly rate and the ledger floors
-- move with the clock even when no mail arrives. A daemon upgraded onto an
-- existing store fills the table once on its first pass (`sync_state`
-- mailbox 'relationships_backfill'); until then the agent context carries NO
-- `relationship` key at all, which reads as "nothing known" and is true,
-- where a table of zeros would read as "known, and ignored".
--
-- NO ROW IS NO HISTORY. An address with no live mail in either direction
-- (only archive-tier mail, or mail since deleted) has its row removed, and
-- the account's own address never has one.
CREATE TABLE IF NOT EXISTS relationships (
    account_id        INTEGER NOT NULL,
    -- Lowercased bare address, the key `reading_senders::sender_key` spells.
    addr              TEXT NOT NULL,
    -- The newest name on this address's counted mail, else the name the
    -- owner's Sent mail addressed them by (`contacts.display_name`). Stored so
    -- the list read needs no join. NOT `senders.display_name`, which takes a
    -- name from sealed mail too (see `people.rs`).
    display_name      TEXT,
    -- INBOUND, counted the way the listings count: inbound, not provider spam,
    -- not known to be sealed (`known_sealed_mail_sql!`; why not the strict
    -- macro is in `relationships`), not the owner's own mail, and NOT THE
    -- ARCHIVE TIER, which is absent from every count.
    received_count    INTEGER NOT NULL,
    -- Off `messages.received_at`, NEVER `senders.first_seen`. The directory's
    -- date is the first SIGHTING, and the backfill walks newest first, so it
    -- would date a ten-year correspondent to last Tuesday.
    received_first_at TEXT,
    received_last_at  TEXT,
    -- Thirty-day rate since the address first wrote. NULL under
    -- `sender_fatigue::MIN_COVERED_DAYS` of history with them.
    received_per_month REAL,
    -- OUTBOUND: the Sent-derived `contacts` row, which is also the
    -- known-contact floor, so the ledger and triage agree on who that is.
    sent_count        INTEGER NOT NULL,
    sent_last_at      TEXT,
    -- Threads whose FIRST message is the owner's and names this address as a
    -- recipient (`message_recipients`): conversations the owner started.
    initiated_count   INTEGER NOT NULL,
    -- REPLIES: inbound threads from this address, and those of them that
    -- carry a LATER owner-sent message.
    inbound_threads   INTEGER NOT NULL,
    replied_threads   INTEGER NOT NULL,
    -- From the address first writing into a thread to the owner's first
    -- message after it. NULL under 3 replied threads.
    median_reply_ms   INTEGER,
    -- ATTENTION, over inbound mail received at or after the later of the
    -- dwell ledger floor, the open ledger floor and the Reading statistics'
    -- 90-day window. Before the dwell ledger a zero is not a measurement
    -- (`migrate::stamp_dwell_ledger_start`). It SEES ONLY PASSBAND: mail read
    -- in Gmail on a phone looks unopened here, so all four understate.
    attention_rows    INTEGER NOT NULL,       -- how many rows the next three are over
    opened_count      INTEGER NOT NULL,
    median_read_fraction REAL,                -- sender_fatigue's read fraction, NULL when none measurable
    link_clicks       INTEGER NOT NULL,
    -- `triage_feedback` plus `agent_triage_corrections` rows on this address.
    corrections       INTEGER NOT NULL,
    -- `sender_fatigue::score` as the Reading purge computes it for this
    -- address (the same `SenderMeasurer`), scaled to 0..1, with its reasons as
    -- the module's JSON `Reason` list, so the purge flow and the ledger can
    -- never disagree. NULL score when the module declined to score.
    fatigue_score     REAL,
    fatigue_reasons   TEXT,
    computed_at       TEXT NOT NULL,
    PRIMARY KEY(account_id, addr)
);
-- Every writer stores `addr` lowercased, but readers are handed whatever a
-- header spelled, so the probes compare under NOCASE, which the BINARY
-- primary key cannot serve (see `idx_contacts_addr_nocase`).
CREATE INDEX IF NOT EXISTS idx_relationships_addr_nocase
    ON relationships(account_id, addr COLLATE NOCASE);

-- THE LEDGER'S DIRTY SET: addresses whose row is owed a recompute. Written on
-- the hot paths (ingest, not-spam, the attention POST) as one
-- `INSERT OR IGNORE` of a lowercased address and nothing more; drained by the
-- refine lane, which deletes each address under the same lock it recomputed
-- it under, so a mark that lands mid-drain is never lost. A duplicate mark is
-- free, which is why there is no timestamp: "dirty" is a set, not a log.
CREATE TABLE IF NOT EXISTS relationship_dirty (
    account_id INTEGER NOT NULL,
    addr       TEXT NOT NULL,
    PRIMARY KEY(account_id, addr)
);
