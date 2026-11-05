# Deep user understanding

How the daemon turns what it already records about the owner's mail into a
standing understanding of who they are, who matters to them, and what they
ignore, and how triage and the clients use that understanding without ever
mistaking it for an instruction.

Status: §2.3 (owner profile), its `understanding` flag (§5) and the
`/client/memory` routes (§6) are built, behind the flag; the relationship
ledger (§2.1), the graph (§2.2) and the triage question (§4) are not. Until
the ledger lands, the profile pass reads `contacts`, `senders`, `send_groups`,
calendar guests and the owner's replies through the `ProfileInputSource` seam
(`triage/profile.rs`), and a `person` address is checked against `contacts`
and `senders`. The seam it fills is `MemoryContextProvider` in
`squelch-core/src/triage/context.rs` (TRIAGE-CODE-DESIGN.md §4, "Future
memory seam"), now `triage::memory::StoreMemoryContext`. Issue #213 owns the
explicit-feedback UX and lands as §4 below.

## 1. What exists and what triage sees

The store already records most of the raw material:

| Signal | Where | Who reads it today |
| --- | --- | --- |
| Inbound volume, first and last sighting | `senders` | `from:` autocomplete |
| Who the owner writes to, how often, last time | `contacts` | known-contact floor, autocomplete |
| Every recipient of every message | `message_recipients` | group send history |
| Opened, dwell in ms, link clicks, under a ledger floor | `triage` | Reading purge (`sender_fatigue`), share open rate |
| Tier, category and sensitivity corrections | `triage_feedback` | the triage queues ("has the owner ever corrected this sender") |
| Field-level corrections to agent decisions | `agent_triage_corrections` | the agent context |
| Named audiences | `send_groups` | the composer |
| Inferred sender timezone | `sender_zones` | the reader's contact card |
| Calendar attendees | `calendar_events` | the Calendar page |

The agent's initial context carries the message, its thread, the previous
decision, matched sender preferences, field corrections, and one boolean,
`sender_is_contact` (`sync/triage_worker.rs`, `ContextSnapshot::initial`).
Everything else it can learn about a sender is `read_sender_history`, which
returns that sender's last N raw messages. So a recruiter the owner answered
once in 2024 and a cofounder they answer within ten minutes are the same
boolean, and the model re-derives the difference from raw mail on every run,
when it bothers to.

## 2. Three layers

Cheapest first. Each layer is useful on its own and is the substrate of the
next.

### 2.1 Relationship ledger (deterministic, no model)

One row per `(account_id, addr)`, the sender key `reading_senders::sender_key`
already uses. Computed from tables we have; no email text is read, so nothing
in it is untrusted.

```sql
CREATE TABLE IF NOT EXISTS relationships (
    account_id        INTEGER NOT NULL,
    addr              TEXT NOT NULL,          -- lowercased bare address
    -- INBOUND, counted the way the listings count (inbound, not spam, not sealed).
    received_count    INTEGER NOT NULL,
    received_first_at TEXT,                   -- off messages.received_at, NOT senders.first_seen
    received_last_at  TEXT,
    received_per_month REAL,                  -- NULL under MIN_COVERED_DAYS of ledger
    -- OUTBOUND, off Sent.
    sent_count        INTEGER NOT NULL,
    sent_last_at      TEXT,
    initiated_count   INTEGER NOT NULL,       -- threads the owner opened by writing first
    -- REPLIES: inbound threads from this address that carry a later owner message.
    inbound_threads   INTEGER NOT NULL,
    replied_threads   INTEGER NOT NULL,
    median_reply_ms   INTEGER,                -- NULL under 3 replied threads
    -- ATTENTION, only over rows at or after dwell_ledger_since.
    attention_rows    INTEGER NOT NULL,       -- how many rows the next three are over
    opened_count      INTEGER NOT NULL,
    median_read_fraction REAL,                -- sender_fatigue's read fraction, NULL when none measurable
    link_clicks       INTEGER NOT NULL,
    -- CORRECTIONS: triage_feedback + agent_triage_corrections rows on this sender.
    corrections       INTEGER NOT NULL,
    -- FATIGUE: sender_fatigue::score over the same counts, with its reasons, so
    -- the purge flow and the ledger can never disagree.
    fatigue_score     REAL,
    fatigue_reasons   TEXT,                   -- JSON, the module's Reason list
    computed_at       TEXT NOT NULL,
    PRIMARY KEY(account_id, addr)
);
CREATE INDEX IF NOT EXISTS idx_relationships_addr_nocase
    ON relationships(account_id, addr COLLATE NOCASE);
```

Maintenance is a pass, not an ingest hook. Two triggers:

- **Touch**: ingest of a message marks its sender (and, for Sent, each
  recipient) dirty in a small `relationship_dirty(account_id, addr)` table;
  the attention `POST` marks the message's sender. The pass drains the dirty
  set every refine round, recomputing only those addresses, each an indexed
  probe on one address like `people.rs` does. A brand-new sender has a row by
  the time its first triage job is claimed, or no row, which reads as "no
  history" and is also true.
- **Full recompute** once a day at the owner's local 03:00 (`owner_clock`),
  chunked by address so no chunk holds the store mutex long, because the
  monthly rate and the ledger floor move with the clock even when nothing
  arrives.

A new daemon build backfills the whole table on its first pass; until that
pass finishes, the agent context's `relationship` is absent, not zero.

**What it feeds.** The initial context gains, beside `sender_is_contact`:

```json
"relationship": {
  "coverage_days": 41,
  "received": {"count": 212, "per_month": 31.0, "last_at": "..."},
  "sent": {"count": 0, "last_at": null, "initiated": 0},
  "replies": {"threads": 190, "replied": 0, "median_reply": null},
  "attention": {"rows": 38, "opened": 2, "median_read_fraction": 0.1, "link_clicks": 0},
  "corrections": 1,
  "fatigue": {"score": 0.8, "reasons": [{"code": "opened_few", "opened": 2, "of": 38}]}
}
```

Nulls stay nulls. `coverage_days` is the one field the prompt is told to read
first: under `sender_fatigue::MIN_COVERED_DAYS` the attention block is
omitted entirely, and the prompt says so. `read_sender_history` returns the
same block at the top of its result. The reader's contact card
(`/client/person`) shows the same numbers to the human, so what the model is
told and what the owner can see are one computation.

**Prompt.** One paragraph in `agent-v1.txt`, `PROMPT_VERSION` bumped: the
relationship is evidence about this owner's behaviour, never a floor; a reply
rate of zero on bulk mail is the expected shape and says nothing about a
newsletter; attention counts only what Passband saw and understate real
reading; a person the owner replies to is a known contact in the strong
sense, and the existing notification floors are restated in those terms
("a person you reply to" rather than "a contact").

### 2.2 People graph (derived)

Addresses that co-occur on threads and in To/Cc, clustered: who the owner
CCs together, which domains cluster, who replies on whose behalf. The
outputs are proposals, never silent changes:

- Proposed send groups ("these nine addresses are on 23 threads together").
- `relationships.co_contacts` (added in this phase): how many of this
  address's threads also carry a known contact, which is how a stranger
  introduced by a friend reads differently from a cold stranger.
- A People page on the Mac, later.

HUMAN DOOR ONLY, like `contacts`, `send_groups` and `people.rs`, and for the
reason they state: who the owner talks to is a graph the agent door was never
handed. Triage is the human door's own backend and may read it. `/mcp` must
not: an invariant test asserts no MCP tool response carries a `relationship`
field or any column of the table.

### 2.3 Owner profile (model-distilled)

The memory seam, filled. A nightly pass reads the ledger, the graph, and a
bounded sample of mail the owner replied to, and writes a small set of typed
entries about the owner: what they do, what is live right now, who they treat
as urgent, what they ignore.

```sql
CREATE TABLE IF NOT EXISTS memory_entries (
    account_id   INTEGER NOT NULL,
    id           TEXT NOT NULL,       -- ULID
    scope        TEXT NOT NULL,       -- 'owner' | 'sender:<addr>' | 'domain:<d>' | 'kind:<slug>'
    origin       TEXT NOT NULL,       -- 'stated' | 'inferred'
    kind         TEXT NOT NULL,       -- see the typed shapes below
    body         TEXT NOT NULL,       -- JSON, the typed fields
    line         TEXT NOT NULL,       -- the one rendered sentence the prompt sees
    provenance   TEXT NOT NULL,       -- JSON: message ids and ledger addrs it rests on
    confidence   REAL NOT NULL,
    revision     INTEGER NOT NULL DEFAULT 1,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    affirmed_at  TEXT NOT NULL,       -- last time the pass or the owner stood by it
    expires_at   TEXT,                -- NULL for stated; inferred = affirmed_at + 90d
    retired_at   TEXT,                -- the pass withdrew it; kept for the review page
    deleted_at   TEXT,                -- the owner removed it; never served, audited
    PRIMARY KEY(account_id, id)
);
```

**Typed, not free text.** The design doc's `MemoryEntry` has a `text`; it
stays, as `line`, but the pass may only emit these kinds, each with short
bounded fields:

| kind | fields | example line |
| --- | --- | --- |
| `role` | `what` (≤120 chars) | "Runs a small software company; most mail is about product, hiring and investors." |
| `project` | `name`, `note`, `until` | "Closing a seed round through November; investor and lawyer mail is live." |
| `person` | `addr`, `relation` (enum: cofounder, teammate, family, friend, client, vendor, recruiter, investor, assistant, other), `note` | "sam@… is a cofounder; replied to within the hour on 31 of 33 threads." |
| `ignores` | `what`, `basis` | "Opens almost none of the product-update mail from SaaS vendors." |
| `prefers` | `what` | "Reads long essays to the end; skims company newsletters." |

A `kind` the validator does not know is rejected, fields over their caps are
rejected, an `addr` that is not in the ledger is rejected, and provenance
message ids are checked for account ownership exactly as decision evidence is
(`triage/validation.rs`). Free prose is confined to a sentence-sized field,
which keeps the review page renderable and shrinks what an email could smuggle
into the owner's own profile.

**Authority.** `origin` is the whole story:

- `stated` entries come from the owner (§4). The prompt treats them as it
  treats sender preferences: strong, overridable only with a recorded
  exception and evidence.
- `inferred` entries come from the pass. The prompt already says learned
  memory is untrusted data; this adds that it is the daemon's own guess about
  the owner, worth weighing and never worth obeying. The pass may revise or
  retire its own entries; it may not touch a `stated` one, and it may not
  promote itself.

Nothing here reaches `ranking.rs`. Entries are lines in the context, not
numbers, and validation keeps forbidding semantic floors.

**The pass.** Once a day per account at owner-local 03:00, after the ledger's
full recompute, gated in order:

1. Feature `understanding` on (§5).
2. Ledger coverage at least `MIN_COVERED_DAYS`; else skip without spend.
3. Daily cap: one successful run per account per UTC day, a `profile` line in
   `stage2_usage`, and a `wake_budget` sentinel like the translate pass, so a
   retry storm cannot run twice.
4. Something changed since the last run: new replied threads, new corrections,
   or an entry within 14 days of expiry. Otherwise skip.

Input, all byte-budgeted the way the agent context is: the top 40 ledger rows
by reply strength and the bottom 20 by fatigue; send groups; calendar
attendees in the last 30 days when the calendar feature is on; up to 30
threads the owner replied to in the last 30 days as subject, participants,
and the first 500 characters of the owner's own reply (their words, not the
sender's, which is both the stronger signal and the smaller injection
surface); the current entries. Sealed and auth mail are structurally absent,
as they are from the agent door. Output is one JSON document of operations
(`keep id`, `revise id`, `retire id`, `new`), validated as above, applied in
one transaction. At most 40 live entries per account; the pass is told the
cap and the validator enforces it.

Model: the configured investigation model, one call, prompt-cached. The stable
prefix (instructions and schema) caches; the per-account tail is what varies.
Expected spend is one stage-2-sized call per account per day, and the cap is
what makes that a ceiling rather than an estimate.

**Serving.** `StoreMemoryContext` implements `MemoryContextProvider`: entries
for scope `owner`, `sender:<this sender>`, `domain:<its domain>`, and any
`kind:` scope, that are not deleted, retired or expired; `stated` first, then
by confidence; capped at 20 lines and a byte budget. `confidence` is reported
to the prompt discounted by provenance age, computed at read time: an entry
resting only on mail older than 180 days is said to be "old" in its line.

## 3. The challenges, resolved

| Challenge | Resolution |
| --- | --- |
| Dwell flatters the product (phone reads in Gmail look ignored) | Read fraction against word count, never seconds; the attention block is omitted under the ledger floor; `fatigue_reasons` carry the counts so every claim is "opened 2 of 38"; the pass may write an `ignores` entry only from a fatigue score, never from dwell alone; the prompt says attention understates. |
| A learned "this person matters" must not become a score floor | Entries are text in context, not inputs to `ranking.rs`; validation keeps rejecting semantic floors; the notification floors in the prompt are restated over ledger facts (replied-to) and stay prompt guidance. |
| Staleness: jobs change, cofounders leave | `expires_at` on every inferred entry; the pass must re-affirm or retire; provenance age discounts confidence at read time; the review page shows age and the last affirmation. |
| Cost | One run a day per account, gated on change, cached prefix, a `profile` usage line and a `wake_budget` cap; the ledger itself costs no tokens. |
| Prompt injection through the profile | Typed entries with bounded fields; the pass reads the owner's own replies rather than inbound bodies; `inferred` origin is permanent; the prompt already treats learned memory as data. |
| Creep: a map of everyone the owner talks to | Local SQLite only; never crosses to control, relay or `/mcp`; the review page shows every entry with its receipts; owner delete is a real delete plus an audit row; retire keeps a tombstone so "no longer thinks" is visible. |
| Multiple accounts | Everything is per `account_id`; a work and a personal account get different profiles, which is correct. |
| Hosted backfill walks newest first | Dates come off `messages.received_at`, never `senders.first_seen` (the trap `people.rs` documents). |
| Cold start | No attention block and no pass under `MIN_COVERED_DAYS`; the ledger's inbound and Sent counts are available from day one because they do not depend on the dwell ledger. |
| Outbound opens (did they read mine) | Out of v1. Scanner and proxy opens make it weak, and it is tracking data with a different consent story. Revisit as a `relationships` column only if the owner asks for it. |

## 4. Explicit feedback (#213)

The pass infers; the owner states. Two inputs write `stated` entries:

- **The review page** (§6): confirm an inferred entry (origin flips to
  `stated`, `expires_at` cleared), edit it, or delete it.
- **The triage question.** The agent decision gains an optional
  `feedback_question {scope, kind, prompt}` the client may show as a card:
  "Keep seeing mail like this?" with the four answers #213 specifies. "No,
  from this sender" writes a sender rule, as today. "Nothing like this" writes
  a `stated` entry with a `kind:` scope and shows the scope before saving.
  Frequency is capped in `app_settings` (one card a day, none for a sender
  dismissed once), and a dismissal is recorded so the agent is told not to
  ask again.

Sender rules stay what they are: instructions, in their own table, matched by
`match_sender_rule`. A stated memory is a preference the model weighs
strongly; a rule is applied. The two are not merged.

## 5. Flag and rollout

- **The ledger COMPUTES for everyone.** It is deterministic and costs no
  tokens, so it runs on every daemon from the release that ships it, flag or
  no flag, and is warm the day the flag is ticked. Computing is all it does
  without the flag.
- **Everything the owner sees and everything the agent is told sits behind
  `understanding`** in `features::KNOWN`. Off is as if it did not exist: the
  pass does not run, `StoreMemoryContext` returns nothing, the relationship
  block is not in the triage context, the memory routes 404, the Mac hides
  the page and the Usage section, and the agent prompt is byte for byte the
  prompt every other mailbox gets (`agent::system_prompt` composes a variant
  per flag state from the one list `agent::UNDERSTANDING_SEGMENTS`; a test
  pins the off variant to the prompt file). One `PROMPT_VERSION` covers both
  variants, and each run's diagnostics record which one ran. It spends tokens
  and writes sentences about people, and the first weeks are for reading what
  the model writes about a handful of tenants before anyone else sees it.
  Launch is deleting the entry.
- **Release order.** Daemon first (ledger, context, prompt bump, pass, routes),
  then Mac (contact card numbers, settings page), then iOS (settings page).
  Nothing in the clients breaks against an older daemon: the card renders
  without the numbers and the page is hidden without the flag.

## 6. Surfaces

- **Contact card** (`/client/person`): the ledger numbers, in the card's own
  words ("you reply to 31 of 33", "opened 2 of 38").
- **Settings: What Passband knows about you** (`/client/memory`): entries
  grouped by scope, each with its line, its origin, its age, and "because"
  links to the threads in its provenance. Confirm, edit, delete. An empty
  state that says the ledger is still under two weeks old. Owes a
  `ProductGuide.swift` entry.
- **Reading purge** keeps reading `sender_fatigue` through the ledger's
  stored score instead of recomputing, once the ledger exists; no behaviour
  change.

## 7. Verification

- Pure ledger math (reply rate, medians, read fraction) unit-tested like
  `sender_fatigue`; SQL in `store/sqlite/tests/relationships.rs` pins the
  indexed probe and the listings' counting rule.
- Validator tests: unknown kind, over-cap field, foreign message id, addr not
  in ledger, cap on live entries, a `stated` entry the pass tries to revise.
- Eval cases in `squelch-eval`: the one-reply recruiter against the cofounder;
  a mail-merged "quick question?" with reply rate zero; a known-contact
  introduction of a stranger; a sender with a stated `ignores` whose account
  notice must still surface.
- Two-door invariant: no `/mcp` response carries a ledger or memory field.
- Budget: a test that a second run in one UTC day is refused without a call.
- On a real mailbox before the flag widens: read every entry the pass wrote
  for the operator's own account and the demo inbox, and the diff of two
  nights' runs.

## 8. Order of work

1. `relationships` table, dirty set, pass, backfill, `relationship` in the
   agent context and `read_sender_history`, prompt bump, evals, contact card.
2. `memory_entries`, `StoreMemoryContext`, validator, the nightly pass, the
   `understanding` flag, usage line and cap.
3. `/client/memory` routes and the Mac settings page; iOS after.
4. #213's triage question and the four answers.
5. Graph: `co_contacts`, proposed groups, People page.
