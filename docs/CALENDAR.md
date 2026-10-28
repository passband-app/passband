# Calendar

Most of what lands on a calendar is born in mail: the invitation, the flight,
the dinner booking, the "does Thursday work?". Passband already reads all of
it. This feature turns that reading into a calendar the user rarely has to
manage, and gives mail the one thing it has never known: what the user's time
looks like.

DRAFT, started 2026-09-30. BUILT so far: the ghost engine (§3.6), the
optional calendar grant and a calendar sync (§8), creating, editing and
deleting events on the user's own calendars (§8), and the Calendar page in the
Mac client (§5, "Built so far"). Nothing is written to a real calendar except
by the user's own hand, one event at a time. Sections marked DECIDED carry a product
decision; everything else is a proposal. §14 lists what is still open.

## 1. The bar

Mail's rule is "open an individual email as seldom as possible." The calendar
rule is its twin: **never do scheduling arithmetic, and never transcribe mail
into a calendar.** The grid is the escape hatch, like the reader.

Standing rules (DECIDED):

- **The calendar holds commitments. Ghosts hold possibilities.** Something the
  user already did (bought, booked, registered, accepted) goes on the real
  calendar. Something another person is asking for stays a ghost until the
  user agrees.
- **No model-written prose, ever.** Passband does not generate email text.
  Actions create events, hold slots, RSVP through an API, or fill the reply
  box. What may go into a reply is a formatted fact the user clicked (a time
  such as "10am ET on the 21st") and the user's OWN saved reply line around it
  (§6, "One click to accept"). Nothing in a reply is written by a model.
- **Time is one resource.** Availability is the union of every account.
  Identity stays per account (§4).
- **An estimate is never stored as a fact.** A guessed end time lives in its
  own field, is drawn differently, and is labelled as a guess on every door.
- **Passband only edits events Passband created.** Self-healing never touches
  an event the user or anyone else made.
- **Sealed mail never produces a ghost**, the same structural rule that keeps
  it out of receipts, shipments and today's calendar records.

## 2. What exists today

- **Calendar records.** Triage emits `RecordProposal::Event { title, start,
  location, evidence }`, plus the wave 1 fields in §3.6
  (`squelch-core/src/triage/decision.rs`, schema in `triage/agent.rs`). The structural detector in `triage/calendar.rs` covers
  invites, updates, cancellations, RSVPs and reservations.
- **A record is per message.** One dinner can be three records. Wave 1's
  ghosts (§3.6) link a confirmation to its later modification or cancellation
  from the same sender, but the Calendar zone below still lists the
  per-message records.
- **The Calendar zone** in the sitrep right rail (`SitrepZones.swift`, reused
  by `MobileRecordsView.swift` on iOS) lists those records. `CalendarVisibility`
  retires each one 24 hours after its first viewing, because a record has
  nowhere else to live.
- **No calendar access** (as of this section's writing; signup asks for it
  since 2026-10-27, see §8). The only Google scopes were the three Gmail ones
  and the identity pair (`docs/VERIFICATION.md`). Hosted signup and reconnect
  require all three Gmail scopes as a floor (`squelch-control/src/oauth.rs`).
- **One daemon is one mailbox.** The client holds one active account and keeps
  an event stream open for each of the others. Nothing merges across accounts
  today.

## 3. Ghosts

A ghost is a placeholder on the timeline for something mail says might or will
happen, which does not exist on the user's real calendar. Ghosts are derived
from mail, so they need no calendar permission at all.

### 3.1 Sources

| Kind | Example | Commitment |
|---|---|---|
| `ticket` | concert tickets purchased, "you're registered" | committed |
| `travel` | flight, hotel, car rental booking | committed |
| `reservation` | restaurant table confirmed | committed |
| `invite` | calendar invitation, event-platform invite, save the date | invited |
| `proposal` | "can you do Thursday at 2 or Friday morning?" | proposed |
| `hold` | a slot the user offered someone from the compose pane (§7) | proposed |

Bills, deadlines and deliveries are not events. At most they appear in an
all-day gutter; they never become time blocks.

### 3.2 Identity and lifecycle

A ghost is one thing across many messages, the way a shipment is. A
confirmation, a "your reservation was modified" and a cancellation from the
same sender feed the same row.

- **A reference only means something within its sender.** Every record takes
  a scope from its message, and only when the message passed authentication:
  the sender's registrable domain under the Public Suffix List (so
  `hotel.com.mx` and `evil.com.mx` are two senders), or the full address on a
  domain strangers share, where a domain says nothing about who wrote:
  freemail, the large consumer ISPs (`comcast.net`, `btinternet.com`,
  `orange.fr`), the mail.com family, `duck.com`, Apple's relay host
  `privaterelay.appleid.com` (matched as a host: `appleid.com` itself is
  still Apple), and shared tenants (`*.onmicrosoft.com`, `googlegroups.com`).
  Its identity refs (an iCalendar UID, a confirmation or booking number, an
  event page URL) are keyed under that scope. A stranger quoting the user's
  booking number from another domain gets a ghost of their own and cannot
  move, rename or cancel the user's.
- **A ghost belongs to the sender that made it.** It records the creating
  record's scope (`owner_scope`) and its From address (`owner_addr`). A
  record joins it by ref only from that scope, since a merchant confirms from
  one mailbox and cancels from another, and a ref is something the sender
  must know. A record joins it by thread only from that identical address:
  the list of shared domains will never be complete, and two strangers at a
  domain it misses share a scope. Another person's "let's do 8 instead" in
  the same thread is a second ghost, not an edit of the first; the
  scheduling wave handles negotiation properly.
- **The owner changes in one case only.** When the one message that alone
  feeds a ghost is decided again under another scope or address (a re-sync
  filled in or cleared its authentication result), the ghost is re-owned to
  what the message is now, and keeps its id and the user's marks. A ghost
  any other message feeds is never re-owned.
- **Unauthenticated mail speaks for nobody.** Its records match no other
  ghost, never join a ghost other mail feeds, and never commit one.
- **Matching, in order:** a ghost of the same scope holding one of the
  record's scoped refs and starting within a day of it (only an update may
  move a date); with no usable ref, a ghost created from the same address in
  the same thread on the same date (or, for a ref-less update or
  cancellation, that address's only live ghost in the thread); the ghost
  this message alone fed before, within a day, which is what keeps a ghost's
  id and the user's dismissal when a re-triage rewords a title; otherwise a
  new ghost. A cancellation that matches nothing creates nothing: there is no
  event to cancel.
- **Ghosts never merge.** Each record is assigned to exactly one ghost and the
  assignment is remembered. A record that could match several is given to one
  and the others are left alone. A duplicate ghost is an acceptable outcome;
  a ghost lost or rewritten by someone else's mail is not.
- **States:** `ghost`, `held`, `materialized` (written to the real calendar),
  `cancelled`, `dismissed`, `expired` (its time passed).
- **Self-healing.** A later message from the same sender about the same
  identity moves, renames or cancels the ghost. If the ghost was materialized
  by Passband, the real event follows. That is the property a hand-made
  calendar entry never has.
- **Sources are kept.** Every message that created, changed, cancelled or
  confirmed a ghost is recorded with its role. That trail is also the event's
  index into its mail (§9).
- **Expiry replaces the 24-hour hide.** A ghost lives at its time and leaves
  when that time passes. `CalendarVisibility` is deleted.
- **Sealed, spam, sent and restricted mail feeds nothing.** Its records keep
  their place on a ghost, and their refs keep the ghost findable, but they
  supply no facts. A ghost whose facts came from such a message is hidden
  until another source supplies them. Un-sealing or un-spamming the message
  matches its stored records again and finds the same ghost (by its refs, or
  by its thread), even while the ghost's other sources are still sealed, so
  the ghost comes back with the user's marks intact unless the message now
  describes something else. Deleting the message a ghost's facts came from
  hides the ghost too, until another of its messages is decided again.

### 3.3 What goes on the real calendar (DECIDED)

- **Committed** ghosts are added automatically once the user has granted
  calendar access (§8). Before the grant they render as solid ghosts.
- **Invited** and **proposed** ghosts stay ghosts. They materialize on
  evidence of agreement: an RSVP yes, a registration confirmation arriving, or
  the user taking the slot with a key.
- An automatic add shows in the sitrep as added, with undo.
- A per-sender rule ("never add these", "always add these") overrides the
  default, through the existing sender-rule system.

### 3.4 Estimated end (DECIDED in principle)

A ticket says the concert starts at 8. It rarely says when it ends, and a
block with no end is useless for availability. Passband blocks off a
reasonable guess.

**Where the guess comes from, strongest first:**

1. **The mail itself.** Doors and show times, set times, "10pm to 6am", an
   itinerary. If the mail states an end, it is a fact, not an estimate.
2. **The user's own history.** A previous event at the same venue, or of the
   same kind from the same organizer, whose end the user corrected or whose
   real calendar entry is known. "Parties at this venue ran to 6am" beats any
   general knowledge.
3. **The triage model's knowledge** of the venue and the kind of event: a club
   night runs later than a seated concert, a conference day longer than a
   dinner.
4. **A fixed table by kind**, in code, for when nothing better exists.

**Rules:**

- The estimate is stored as a duration with its basis, separate from `start`
  and from any stated `end`, and every resolved end is served with the tier it
  came from. No consumer can mistake it for a fact.
- The timeline draws the known start solid and the guessed tail visibly
  softer. The label reads "ends ~11pm". The tilde means estimate everywhere.
- The estimated tail counts as busy when suggesting times.
- Dragging the end is a correction. It is stored against the venue and kind
  and used as tier 2 next time.
- A real calendar event needs an end, so an auto-added event is written with
  the estimate and marked as estimated in a private property. If later mail
  states the real end, the event is corrected. If the user edits the end in
  their calendar app, that edit wins and is learned from.
- Events cross midnight. A 10pm to 6am party is one block spanning two days in
  every view.
- The triage agent does not search the web for a venue. Doing so would send
  the user's plans to a third party.

### 3.5 Likelihood (DECIDED in principle)

A dinner invitation the user never answers, a "let's grab coffee sometime" and
a paid ticket are three different levels of real. Ghosts that are
possibilities carry a likelihood of becoming commitments, learned over time
and shown in the UI. Committed ghosts have no likelihood; they are certain.

**The score:**

- **A prior from triage**, with a reason and evidence: a personal invitation
  from a known contact scores higher than a mass invite; a conflict with a
  commitment scores lower.
- **An empirical rate from outcomes**, counted per organizer address, then per
  organizer domain or event platform, then per kind, backing off to the next
  level when a level has too few samples.
- The two are blended by sample size. It is counting, not a trained model, so
  it runs in the daemon and every score can be explained in one line: "You
  have said yes to 1 of 14 invitations from this sender."

**Outcomes that teach it:**

- Yes: the user took the slot, RSVP'd yes, a registration confirmation
  arrived, or a matching event appeared on the real calendar.
- No (strong): an explicit dismiss or an RSVP no.
- No (weak): the ghost expired with no action.

**In the UI:** three bands, never a percentage.

- **Likely:** a full ghost.
- **Maybe:** a lighter ghost.
- **Unlikely:** folded into a per-day count ("3 unlikely") that expands. This
  is squelch, applied to the calendar.

**Limits:**

- Likelihood changes presentation and suggestion ranking. It never writes to
  the real calendar and never dismisses a ghost on its own. Only commitment
  evidence materializes an event.
- **What counts as busy** (DECIDED): real events, committed ghosts and likely
  ghosts. A maybe or unlikely ghost never blocks a suggested time.
- Outcomes stay in the account's own database.

### 3.6 Data sketch (BUILT in wave 1)

What wave 1 actually built, daemon only. Everything here is human-door only:
the agent door and `/mcp` gained no ghost route.

**Triage record.** `RecordProposal::Event` gained these fields, each
`#[serde(default)]` because reconcile re-reads stored `decision_json` and one
row that fails to deserialize must not fail a pass:

| Field | Type | Notes |
|---|---|---|
| `end` | `Option<SupportedTime>` | the stated end, only when the mail says so |
| `duration_estimate` | `Option<DurationEstimate { minutes, basis }>` | the model's guess, only with no stated end and a clock start |
| `event_kind` | `EventRecordKind` | `ticket` / `travel` / `reservation` / `invite` / `proposal` / `other` |
| `commitment` | `EventCommitment` | `committed` / `invited` / `proposed` / `unknown` |
| `change` | `EventChange` | `created` / `updated` / `cancelled` / `agreed`: what this message does |
| `identity_refs` | `Vec<String>` | iCalendar UID, confirmation numbers, event URL |
| `likelihood` | `Option<EventLikelihood { band, reason }>` | the triage prior; stored, not yet learned from |

The prompt is `agent-triage-v7` (main's own `v6`, for its login-code rule,
was bumped in parallel with this branch's event fields; the merged prompt
carries both and is neither, so it is `v7`). It tells the model that `duration_estimate`
and `likelihood` are the only fields that are not stated facts, and that
`committed` takes the word of the merchant, venue, platform or organizer
itself. Rollout is forward-only: the version is recorded on new decisions and
nothing is re-triaged. `candidates` (offered slots for a proposal) is deferred
to the scheduling wave (§12, wave 5), and `hold` is not a kind yet.

**Laundering** (`triage/ghost.rs`). Bad model output is cleaned or dropped,
never rejected:

- Title trimmed and capped at 200 characters; an empty title drops the
  record. It is the only reason a record is dropped, apart from the cap of 20
  event records per message (the first 20 kept), because a record's position
  among the kept ones is part of its stored identity.
- A start or end that is neither RFC3339 nor `YYYY-MM-DD` is dropped, and so
  is an end not after the start at the same precision.
- `start_timezone` is kept only as an IANA place zone: a valid name with a
  `/`, not under `Etc/`. "Eastern Standard Time", "PDT", "EST" and "UTC" are
  dropped. A valid alias is kept as written ("US/Eastern"): the zone is
  shown, not counted, so it is validated and not renamed.
- The estimate is clamped to 15 minutes to 3 days, and dropped when an end is
  stated or the start is date-only.
- `basis` and `reason` are capped at 80 characters, and dropped (the minutes
  and the band stay) when they carry a URL, an `@` or a run of 6 or more
  digits, counted across the separators a number is printed with ("call
  555-1234" is a run).
- At most 5 refs per record. A URL ref loses its query, its fragment and
  any `;` parameters on its path; its host is lowercased and its path keeps
  its case (an event id there is case-sensitive). It is kept only when the
  path names one event: a last segment with a digit, or a last segment of 6
  or more characters under another one. A bare site, a
  route whose id lived in the query (Google Calendar's `event?eid=`,
  OpenTable's `book/view?rid=`) and any path with a secret-shaped segment are
  dropped: an unbroken run of more than 24 characters mixing letters and
  digits (a magic link), or a UUID (a cancellation link). A hyphenated slug is
  not a secret, however long. A ref with a URL anywhere in it, glued to a
  label or not ("Event page:https://..."), is that URL alone.
- Any other ref is keyed on ONE code run. Label words ("confirmation",
  "order", "booking", "ref", "flight" and the like) go wherever they stand;
  a clock time ("8pm", "10:30am", "19:00") and a position word with its
  number ("Seat 12A", "Gate 4") go too, and break any run across them. A
  code word has a digit or is all capitals of at most 6 letters. A run is
  code words joined by `-`, `/`, `.` or nothing, never by a space, except
  that a 1 to 3 letter capitals word joins a number one space after it
  ("UA 123", "AB 12345"). The key is the first run with a digit:
  "ABC-1234-XYZ" and "ABC-1234-QRS" are two keys, "Booking ABC-1234-XYZ" is
  the first of them, "OpenTable confirmation 123456789" and "123456789"
  agree, "Order #1234567890 (2 tickets)" is `1234567890`, "Conf #: AB 12345"
  and "Conf AB12345" are both `ab12345`, and "UA 123" and "Flight UA123" are
  both `ua123`. With no digit, only a single all-capitals word that is not a
  status word is kept (a record locator, "QXJZKM"). Dropped: a key under 5
  characters, an all-digit key under 6 digits, and an 8-digit key that is a
  date, so "Table 7 at 8pm" is no ref at all.
- Every URL in a location, glued to a label or not, keeps host and path only
  ("Zoom:https://zoom.us/j/123?pwd=abc" becomes "Zoom:zoom.us/j/123"),
  without `;` parameters on the path. A bare host followed straight by a
  query or fragment is a URL too ("Zoom: zoom.us?pwd=abc" becomes "Zoom:
  zoom.us"). A path with a credential-shaped segment is cut back to the
  host.
- All text, titles included, loses control, bidi and invisible characters
  (zero-width, word joiner, BOM, tag characters). A title with nothing else in
  it is empty, and its record is dropped. That rule fixes each record's
  stored position, so it must not change once ghosts exist in the field.
  The shared filter (`order_link::plain`) is also the merchant name's, so
  since this work a shipment, order or receipt merchant name loses
  zero-width and tag characters too, for every tenant, flag or not (a ZWJ
  emoji sequence in a merchant name comes apart).
- A `basis` or `reason` that laundering blanks is served as null.

**Identity.** Scope is `triage::ghost::sender_scope`, for authenticated mail
only: the sender's registrable domain under the Public Suffix List (the `psl`
crate, private section included), or the full lowercased address on a
domain strangers share (the freemail list in `triage/receipt_match.rs`,
which also names the large consumer ISPs, the mail.com family and
`duck.com`; the relay host `privaterelay.appleid.com`; the shared tenants
`onmicrosoft.com` and `googlegroups.com`); none otherwise. A ghost's
`owner_scope` and `owner_addr` are the scope and lowercased From address of
the record that created it (both NULL from unauthenticated mail). Keys are
`ref:<scope>:<ref>` and nothing else: there is no thread key and no title
key. Each laundered record of a message (by its `position`) is matched once,
against the state before the message's old assignment is removed:

1. By ref: ghosts of the record's scope holding any of its keys. Any record
   but an `updated` one skips a ghost whose start is more than a day from its
   own. Most shared keys win, then the rank below.
2. By thread, only with no usable ref: ghosts fed from the same thread (by
   any source row, eligible or not) whose `owner_addr` is the record's own
   From address, not merely its scope: no list of shared domains is
   complete, and anyone can reply in a thread. Same date wins; failing that,
   a record that is not `created` takes the only live one. With several live
   ones and none on its date it matches nothing, so an ambiguous ref-less
   cancellation cancels nothing.
3. A ghost this message alone fed before (no source row of any other
   message, eligible or not), within a day of the record's start (or with
   either start missing): the one at the record's position, or, when that
   one is claimed or does not qualify, the nearest-starting other one that
   does. Whatever its owner: if the message's scope or address has changed
   since (its authentication result arrived or went on a re-sync), the ghost
   is re-owned to the current ones. This is the only place an owner changes.
4. A new ghost, owned by the record's scope and address. Not for a
   `cancelled` record: one that matched nothing is dropped.

The rank among equals: a ghost the message already fed, then the nearest
start, then the lowest id. A record with no start prefers the ghost next to
start after its mail arrived, then the latest before it. No two records of
one message share a ghost. An unauthenticated record skips 1 and 2. Ghosts
are never merged.

**Daemon tables:**

- `ghost_events`: kind, title, `starts_at` (as given) and `start_timezone`,
  `ends_at` (stated only), `est_minutes` and `est_basis` (the model's only),
  location, commitment, `owner_scope` and `owner_addr`, `mail_state` (`live`
  / `cancelled`),
  likelihood band and reason, `supplier_message_id` (the message whose record
  supplied the facts), `last_message_id`. Derived: `start_sort` (UTC text,
  for the coarse range query) and `venue_key` (what the priors are keyed by).
  The user writes `user_state` (`dismissed` / `going`), `user_state_at`, and
  the end correction as a SPAN from the start: `user_end_minutes` for a timed
  ghost, `user_end_days` for an all-day one. Mail that moves the start moves
  the corrected end with it. Mail that changes the start's precision (timed
  to all-day or back) clears the span at the other precision, so it cannot
  come back after a second change; that is the projection's one write to a
  user column.
- `ghost_event_sources`: the assignment, one row per record: account, ghost,
  message, `position`, role (its `change`). Primary key `(account, message,
  position)`. At most 50 per ghost; a new record past that replaces the
  oldest, and since only the owner's scope adds rows, another sender cannot
  evict the owner's. Rows go with their ghost or their message. Deleting a
  message also clears `supplier_message_id` on the ghost it supplied, which
  hides that ghost until the next reconcile of a message that feeds it.
- `ghost_event_keys`: the scoped ref keys of a ghost's currently assigned
  records whose decision is readable and whose message still has the
  owner's scope, eligible or not (keys are never served, and a record coming
  back from a seal must find its own ghost), recomputed on every rebuild.
  Not unique: a flight number can name two ghosts on two dates.
- `event_duration_priors`: venue key and kind to minutes, written when the
  user corrects a timed end at a known venue, with the ghost that taught it.
  Never written or read for travel, which has no default: one long flight
  from an airport says nothing about the next. Clearing a correction removes
  the prior it wrote while the prior still holds that number. Tier 2 of
  §3.4.

`ghost_outcomes`, the real event id and the Passband-created flag are not
built; they arrive with likelihood (wave 3) and calendar access (wave 4).

**Rebuild** loads exactly a ghost's assigned records from their messages'
stored decisions. Facts come from the newest eligible record that does not
cancel (the newest eligible one if all do), and that message is recorded as
the supplier. `mail_state` is `cancelled` when the newest eligible record
cancels. Each source's message is checked for its CURRENT scope (from its
`from_addr` and `auth_pass` now): one whose scope is no longer the owner's
was assigned under authority it has lost, so it keeps its row and
contributes nothing, no facts, no keys and no commitment, until it is
decided again. The ghost is `committed` only when it has an owner and one of
its eligible, in-scope records is committed or `agreed`; a ghost made from
unauthenticated mail is `unknown` whatever its mail claims. A source that is
sent, spam, restricted, awaiting its access review, or an auth decision keeps
its row and its keys and supplies no facts; one whose decision does not deserialize keeps its row and contributes
nothing. An eligible message's row whose decision no longer carries a record
at that position goes. No rows left deletes the ghost; rows but no eligible
record keeps its facts and keys, clears the supplier, and hides it.

**When it runs.** Inside the triage commit, under a savepoint: an error rolls
back the ghosts alone, logs ids and an error kind, and the decision commits.
The exception is an error after which SQLite has already rolled back the whole
transaction (SQLITE_FULL can): the commit then fails and the job stays open,
as it would with no ghosts. When a message's eligibility changes outside
triage (the access job, the user's `external_access` correction, the
not-spam action), the same guarded reconcile runs again from its STORED
decision and matches its records afresh, rather than rebuilding its old
ghosts by position: a decision that changed while the message was
restricted would otherwise take over the old ghost, marks and all.

**Listing.** A ghost is served only while its supplier is eligible now (not
sent, not spam, and not sealed by the test the Auth page's `sealed_messages`
uses: its access review `allowed`, so neither `restricted` nor still
`pending`, and its stored decision naming no auth kind), judged in SQL, so a
status no rebuild has seen yet cannot serve facts from it. The retired
`triage.sensitivity` column is not read: the agent pipeline no longer writes
it. A consequence: a message whose content snapshot changed (the not-spam
action changes it, since the spam flag is part of it) hides its ghost until
its queued re-review allows it again, and the ghost comes back with the
user's mark. At most 2000. A timed start compares as
an instant; a date-only start is listed from `from`'s date in `from`'s offset
up to `to`'s date in `to`'s own offset, and sorts at midnight in the caller's
offset.

**Endpoints:**

- `GET /client/timeline?from=&to=` (RFC3339, `[from, to)`, default a day ago
  to 90 days ahead, a span over 400 days clamped) returns `{ "ghosts": [...] }`
  by start. Each ghost: `id`, `kind`, `title`, `start`, `start_timezone`,
  `all_day`, `end` (resolved), `end_estimated`, `end_source` (`stated` /
  `user` / `history` / `model` / `default`), `estimate` (the model's
  `{minutes, basis}` as stored), `location`, `commitment`, `state`
  (`dismissed` / `going` / `cancelled` / `expired` / `ghost`), `mail_state`
  (`live` / `cancelled`: what the mail says, since `state` puts the user's
  `going` or `dismissed` over a real cancellation), `verified` (false for a
  ghost made from unauthenticated mail), `likelihood` (null when committed)
  and `sources` (eligible ones only: `message_id`, `thread_id`, `role`). A
  date-only ghost with no stated or corrected end has no end.
- `PATCH /client/timeline/ghosts/{id}` with `state` and or `end`, applied in
  one transaction. An absent field is left alone and `null` clears it. An end
  not after the start, or a timed end on a date-only ghost, is a 400 and
  writes nothing, the state included.
- `/client/stats` carries `"timeline": true`.

### 3.7 Known limits of wave 1 as built

Found by the last review (2026-10-01) and left as they are. None is reachable
from another sender's mail without that sender already sharing the ghost's
scope, and none matters while ghosts are display-only. Each should be fixed
with the wave that first depends on it.

- **A re-decision made while a message is restricted re-keys its ghost.** The
  ghost keeps the keys of ineligible sources so it can be found again. If the
  message is re-triaged while restricted and its record now carries a
  different ref, that new ref becomes a key, and later mail from the same
  sender about that other thing joins the old ghost, with the user's mark.
  Fix: remember each source's keys from when it was last eligible.
- **A change of authentication at re-ingest rebuilds nothing.** A re-fetch
  that changes a message's `auth_pass` or From leaves its ghost served as it
  was until that message is re-triaged. In practice this needs a change to how
  the verdict is read. Fix: refresh the message's ghosts from ingest when
  either value changes.
- **A ref is the first digit-bearing code run, even when that run is then
  dropped.** "Table for 2, confirmation 1234567" and "Hotel confirmation 12345
  / itinerary 6789012" yield no ref, and "Flight UA 123 PNR ABC123" keys on
  the flight number, which two bookings share. The cost is usually a fallback
  to thread matching or a duplicate ghost. Fix: take the first run that
  survives the drops, and prefer a run that follows a label word.
- **A letter-only locator is kept only when it stands alone or follows a
  label** ("Booking ref QXJZKM"), not inside a sentence.
- **Dotted text followed by `#` can be read as a link in a location**
  ("Bldg.West#3"). Fix: accept a bare host only when it ends in a real public
  suffix.
- **Reference matching still trusts a domain.** A stranger at a shared mail
  provider that is not on the list, who also knows the ref, can update a ghost
  created by someone at that same provider. A ghost with no ref cannot be
  reached this way.

### 3.8 Mail about a past event is done (BUILT)

"24 hours after a flight lands, I don't need to see any emails about that
flight." The triage model decides that, not code: whether a booking notice
is still needed (a refund pending, a later leg's tickets, a question to
answer) is a judgment, and hand-written rules for it were never as accurate
as asking. Code does two things around the model: it schedules the question
from the model's own dates, and it enforces a few rails on the answer. It is
NOT behind the `calendar` flag: it runs for every mailbox, like ghost
projection does.

- **The question.** A triage job whose trigger starts with `revisit:` is a
  scheduled re-check. On one, and only on one, the agent may answer `done`
  (a reason and evidence). The prompt tells it to set done when the message
  needs nothing more from the owner, and to keep it (and set a later
  revisit) when anything is still owed or the moment has not really passed.
  It decides from the owner's situation and the mail's facts, never from
  instructions in any email. On new mail the agent also sets its own
  `revisit.at` for the moment a time-bound obligation or event ends (an
  event, trip, reservation or appointment ends, a delivery arrives, an
  invitation's date passes), plus a day, always in the future. Offers, sales
  and codes earn no revisit: they would fill the shared daily revisit cap
  ahead of a flight's re-check.
- **The schedule, for event mail.** Mail already triaged has no revisit, and
  the model may forget one, so the sync loop's poll tick, right after the
  reminder sweep, also asks about event mail
  (`Store::schedule_past_event_revisits`, `event_revisit_pass` in
  `sync/mod.rs`). A message's ask time is 24 hours after the latest of: the
  end the timeline draws for every ghost it feeds (stated, the user's
  correction, the venue's history, the model's guess; the kind's fixed
  default is nobody's date, so an event with no end and no length counts
  from its start, and travel from a day after it leaves); the latest time any
  of those ghosts' mail's own event records mentions, at any precision, its
  length guess believed up to a month (a "check in today" reminder waits for
  the checkout date the booking gave; a ten-day trip waits ten days); and the
  message's own records (mail pooled into another trip's ghost waits for its
  own dates). When that has come, one job with trigger
  `revisit:event_ended:<ask time>` is queued, due now, the same way the
  agent's own revisits are. The model may answer "not yet" and set a later
  revisit; there is no "undatable means never" rule.
- **Which mail is asked about.** Mail that feeds a ghost with an owner
  (authenticated mail made it), a start that has passed, and the owner
  going (committed, or marked going, and not dismissed: a mass invite nobody
  accepted is left to the agent's own revisit); still new, with no reminder,
  received, not spam or archive tier, never auto-done, allowed to feed an
  event, and with only its own sender's words in its thread (no own mail, no
  stranger). These only save model calls that could not close anything.
- **How often.** Asked means answered: a message counts as asked at an ask
  time once a `revisit:` run for it (the agent's own or this one) has
  committed a decision at or after it. A re-check dropped by a content
  change, or failed for good, answered nothing and is asked again. Once
  answered, new mail that moves its dates later asks again only when the ask
  time moves more than 24 hours past the answered one (length guesses
  wobble). Each message gets at most three of these jobs, counted on their
  own so the agent's own revisits cannot use them up; a revisit already
  queued or running is enough; `[revisit]` off means no asks. They are
  claimed ahead of other revisits, and the daily revisit cap throttles the
  runs, so a backlog on rollout drains over days. `event_revisit_asked`
  keeps the last ask time and when the dates were last read, so a tick on
  answered mail whose dates have not changed reads no decision.
- **The rails** (`apply_done` in `store/sqlite/agent_triage.rs`, in the
  decision's own transaction). A done applies only on a `revisit:` trigger,
  to a message not already done (reopened mail still closes: a flight
  email the owner pulled back to keep handy before the flight is still
  unwanted after it), and only when every message in its thread has no
  reminder pending or fired, is received and not spam or archive tier, is
  not the owner's own mail, and was never auto-done before (so undoing the
  agent's own done is final), and only when the message is authenticated and every other
  message in its thread is from the same sender scope (its organisation's
  domain, or the full address on a shared domain): email text is never
  trusted, so a stranger's reply in the thread cannot lead to a done.
  Applied, it is the owner's own Done, the same thread-wide write
  (`AGENT_DONE_THREAD_SQL` in `store/sqlite/attention.rs`: the owner's
  `DONE_THREAD_SQL`, sparing a sibling that holds FYE, minus rows already
  done, which keep their own close time and smart clear's batch), so the
  thread leaves FYE and every band exactly as if the owner had closed it.
  Each message it closed gets an `auto_done` row with the model's reason,
  capped, never logged and never put in a prompt. A RE-CHECK MOVES MAIL
  ONLY BY ITS DONE: it writes no placement at all (FYE flag,
  representative, Reading destination, related threads), so a refused or
  absent done never hides mail by the side door, and an applied done leaves
  the FYE flag as the owner's Done does, so the undo brings the thread
  back. A done that fails a rail is dropped; the rest of the decision
  still commits. Because it is the
  owner's Done, the prompt asks for it only when the model is certain
  nothing in the thread needs the owner.
- **Reversible and sticky.** Done mail stays searchable, and the owner can
  undo or reopen it like any other (the undo reopens the one row the owner
  saw, which brings the thread back). A thread with any `auto_done` row is
  never auto-done again, mail that arrives in it later included, so
  whatever the owner did with it stands. While it
  is still done from the agent, the rule suggester reads it as not yet
  handled, not as the owner clearing that sender's mail unopened.

## 4. One timeline, per-account ownership (DECIDED)

- **Read:** one merged timeline across every account. Events and ghosts are
  tinted by account.
- **Privacy option:** show the other accounts as busy blocks only, so a work
  screen-share does not show a personal appointment.
- **Write:** an event lands on the calendar of the identity the user is acting
  as. Composing from work writes to the work calendar. A ghost from a personal
  reservation writes to the personal calendar. No picker.
- **The merge happens on the client.** Each account gets a timeline feed that
  stays alive while the account is inactive, alongside its existing event
  stream. Rows are keyed by account and id, because per-daemon ids collide.
- **Daemons stay ignorant of each other.** Hosted tenant isolation is
  unchanged. The cost: anything computed across accounts (free time,
  conflicts) is computed on the client.
- The same event can appear in two accounts (an invitation sent to both
  addresses, a shared calendar). The client deduplicates by iCalendar UID.

## 5. Surfaces

The visual design pass is done by Opus 5.5 (DECIDED). Its first round is
`docs/calendar-ux-preview.html`, and these directions from it stand until the
founder says otherwise:

- A ghost is an empty container, never a faded one. Its text stays at full
  strength, so it does not read as a disabled control.
- Fill density is the likelihood scale: solid is real or committed, striped is
  likely, outline is maybe, and unlikely folds into a count.
- An estimated end fades out with no bottom edge and reads "ends ~11pm".
- A committed ghost before the calendar grant looks like a real event, with a
  small mail mark (DECIDED).
- Accounts are two muted fills. Blue is reserved for state: now, next, holds
  and suggested slots.

**Built so far** (Mac client, behind the `calendar` hidden feature, §11): a Calendar destination
second in the rail. The page opens on a conventional month (Month | Agenda in
the header, `m` and `a`): each cell lists its day top-down as chips in the
calendar's colour, trips and stays as named bars across the cells they cover,
"+N more" for what does not fit, the selected day's column of hours beside the
grid, and the item detail in that same pane. The agenda is a "Next" callout,
today's strip, and days with ghosts and real events in time order ("N earlier
today" and "N unlikely" folded). The detail shows the mail behind each item
and the model's own words as attributed quotes; ghosts take going and dismiss.
The connect card is shown once per account as a modal, then a quiet banner,
with a waiting state and a line for the sync state.

Wave 4, from the founder using wave 3 on screen:

- **Number keys.** While the page is offered, Calendar is `2` and every later
  page is one higher (Sitrep 1, Calendar 2, All mail 3, Auth 4, Rules 5,
  Groups 6). While it is not offered the numbers are the ones from before it
  existed. `⇧C` still opens it.
- **No gap under the date.** A bar holds its lane only over the days it
  covers. A lane is free again the day after the span in it ends, in the same
  week row. A cell's own items fill any row left free above a bar before going
  below it, so the first thing in a cell sits directly under its date. The one
  hole left is a day whose only content is a bar in a lower lane after the bar
  above it has ended: the bar is kept straight rather than jogged up
  (`MonthGrid.holes` counts these).
- **The event pane.** `n` (and "New event") and `e` open a pane that takes
  half the window the way the compose pane does, with its chrome, its motion
  and its Esc. Title, calendar (every writable calendar as a tag in its
  colour, the primary one picked), all day, the day and times as typed fields
  with the line saying what will be saved, location, notes. `⌘↵` saves from
  anywhere, `↵` from a single-line field. While it is open the day column and
  the detail are away, the month keeps the other half and stays live, a click
  on a day sets the draft's date, and the draft is on the grid as a dashed
  chip in its calendar's colour that moves as the fields change. In Agenda
  mode the list takes the other half. A refused save keeps the pane and its
  contents and says why in one toast; delete asks once.
- **Leaving the page keeps the draft.** Going to another page with something
  typed in the pane PARKS the draft in the store and hides the pane; coming
  back to the Calendar page restores it as it was. An untouched pane just
  closes. Only the calendar gate closing or an account switch discards a
  draft.
- **Only a definite "nothing happened" gives the fields back.** Every answer
  to a write is one of three kinds (`CalendarWrite.verdict` in
  Lib/CalendarWrite.swift, keyed on the daemon's `code`, §8 "What a refusal
  says"): done (a 2xx the app decodes); nothing happened (a request that
  never left the app, or one of the daemon's refusal codes with its own
  status), which leaves the pane editable with one toast per code; and
  everything else, which MAY have happened and is settled as below: a 504,
  any 500, a 502 without `google_refused` (a proxy's own page), no code or
  an unknown one, a 2xx that would not decode, any transport failure not
  provably before the send (only no host, no connection, no network and a
  refused TLS handshake count as unsent), and `already_creating` (the same create still
  out, which may yet make it: "This event is still being sent. Check again
  in a moment."). When in doubt, the pane holds.
- **When Google's answer is lost.** A save or delete that may have happened
  shows a quiet toast ("Google may have made this change. Refreshing…"), keeps the
  control locked, and reloads the feed up to 3 times, 2 seconds apart. The
  pane closes as a success once the change is visible in the feed. A delete
  not yet seen keeps its question up. A SAVE not yet seen FREEZES the pane
  (`EventPaneState` in Lib/CalendarWrite.swift): the fields go read-only with
  the draft as sent still showing, one line says "Google may have made this
  change. Passband could not confirm it yet.", and the footer offers three
  controls: "check again" (⌘↵: the daemon is asked to sync, the feed is
  reloaded and looked at again; seen closes as a success, unseen stays
  frozen), "send again" (exactly the body already sent, the same create id
  with it; for an edit that tells guests it reads "send again, guests may be
  told twice"; a create's id keeps Google from inviting twice, so it says
  nothing more) and "close" (Esc: the event, if Google made it, arrives with
  the next sync; no draft is kept). Plain ↵ does nothing. There is no way
  from a frozen pane back to an editable one: changing the event after an
  uncertain save means closing and starting a new event, under a new id. A
  "send again" refused outright keeps the pane frozen, since the first send
  may still have worked. A composer asked for over a pane that cannot be
  saved (frozen, or sending) is refused with "Close the event first"; over
  an editable one, "Save or close the event first". The store's decisions
  about the pane (whether a save may run, what "send again" and "check
  again" work from, whether a late answer moves this pane, the toast a
  refusal gets) are pure functions in `EventPaneRules`, tested in
  passband/Tests/EventDraftTests.swift. Calendar writes wait 45 seconds on the client,
  longer than the daemon's 35 second deadline on a write, so the daemon's
  answer (504 included) arrives first.
- **Telling guests only where it is allowed.** On a calendar that may not
  invite (`can_invite` false: one the user only writes to), the pane and the
  delete question offer only the silent controls, with one line saying why;
  the daemon refuses `notify: true` there anyway (§8, "Guests").
- **One pane at a time.** The event pane and the composer use the same half.
  Opening the event pane closes an open composer the way Esc does (its mail is
  kept in drafts). Opening a composer closes an untouched event pane; an event
  with something typed in it is not thrown away, and the composer is refused
  with one line ("Save or close the event first").
- **Location lookup.** The location line suggests places as it is typed, from
  Apple's MapKit (§8). Arrows and `↵` pick, Esc closes the list and not the
  pane, and typed text is replaced only by a pick.
- **Drag to create.** A press and a drag on the day column's hours sweeps a
  span, snapped to 15 minutes and labelled as it goes; letting go opens the
  event pane with that day and those times. A plain click does nothing new, a
  press on an existing block is that block's click, and Esc mid-drag drops it.

Wave 6, from the founder ("lets add a week view too", "you should be able to
change the end date on the create event form", "when i type the title, i
should be able to type 'dinner with steven on friday 4pm'"):

- **Week.** The header reads Month | Week | Agenda (`m` / `w` / `a`), Month
  still the default. The week is seven of the month's day columns over one
  hours axis: the same folded night, hour lines, blocks side by side where
  they overlap, calendar colours, band fills and now line (in today's column
  only), because they are the same views (`HoursGutter`, `HoursWell`).
  All-day items and trips sit in a strip across the top in the month's own
  lanes (`MonthGrid.spanRow`), so a trip reads the same in both views; past
  four lanes a day says "+N more", which opens that day in the month. Today's
  date is on the blue capsule and the selected day's head takes the even 1pt
  edge. `←` `→` move the day, `[` `]` (or `,` `.`) a week, `t` today, `j` /
  `k` and `↑` `↓` walk the selected day's items, `↵` opens. A block clicked
  opens the detail beside the week (in place of it on a narrow window). Drag
  to create works in any day's column, and with the event pane open the week
  keeps its half and draws the draft as the dashed chip. Columns are a
  seventh of the container, both ways; narrow ones shorten their heads ("T
  6") and drop a block's time. The week shares the month's place, so the
  timeline range asked for is the month's and covers it. Arithmetic:
  `Lib/WeekGrid.swift`.
- **The end day.** A timed event shows its end day beside its end time,
  always. Before wave 6 that well was drawn only for an event that ALREADY
  ran past the next morning, so a new event had nothing to change. Left
  empty it follows the start (the next day when the end time is at or before
  the start, so 7pm to 7pm is a whole day) and the well shows that day in
  the dim ink; a day typed into it is the user's and stops following. The
  line under the fields says what will be saved either way.
- **A title that reads itself** (`Lib/TitleParse.swift`, no model). On a NEW
  event the title is read as it is typed for a day ("on friday", "tomorrow",
  "oct 12", "next tue"), a time or range ("4pm", "at noon", "4-6pm", "from 4
  to 5:30") and "with <name>". The wells it fills wear a soft accent wash,
  and a line under the title says what will be saved: the typed title less
  the words that named the day and time ("Dinner with Steven": a name
  after "with" typed in lower case gets its capital, one the user
  capitalised or mixed is left alone, and the first letter gets one only
  when the words that are kept hold no capital; a title nothing came out of
  is saved exactly as typed). Accepting a suggested guest does
  not rewrite the title. It never fights the user: a well
  typed into, or a day clicked on the grid, is never written again; deleting
  the words puts back what was there; Esc in the title, or `⌘Z`, takes the
  title as typed. Ambiguity resolves to nothing: a bare number is never a
  time ("meet at 4", "from 3 to 4 people"), the word after "with" is never a
  day or time ("call with 4pm people"), a month needs its day number ("May
  day parade"), and two different days or two different times anywhere in
  the title are neither. A day or a time is read only when it is LED ("on
  friday", "this fri", "next tue", "at 4:30", "from 4 to 5:30") or is in the
  title's trailing run, where every later word is a day or time word too
  ("dinner with steven friday 4pm"): "Black Friday sale", "Friday dinner"
  and "4pm standup" are titles. A clock needs its am or pm, or a lead and a
  colon ("Isaiah 14:12" is a verse). A weekday in capitals ("SAT"), or with
  a capital after another capitalised word ("Taco Tuesday", "USA Today"), is
  a name; "sun", "sat" and "wed" unled need a time or a date beside them; a
  weekday and a date that disagree ("fri oct 10", a Saturday) are neither. A
  deadline ("ship v2 by friday", "prep for friday": led by for, by, before,
  after or until) fills the day and leaves the title exactly as typed. The
  saved title's capital is decided by the words that are kept, and
  correcting a well by hand never puts the words it replaced back into the
  title. "next tue" is next week's Tuesday by the user's own weeks. An
  edit's title is never read. The line under the wells prints the year when
  it is not this one and ends "in the past" when the start has gone by; a
  weekday typed in the end-day well is read from the start day.
- **A guest suggested, not added.** For each "with <name>" the client asks
  the daemon who that most likely is (`GET /client/calendar/people`, §8) and
  shows the best answer on the guests line in a dashed box: name, address, a
  checkmark and an x, with up to two other candidates after "or". It is not
  a guest until the checkmark: it is not in the draft's guests, does not
  change the save control, and is sent nowhere. A dismissed or accepted
  name is not offered again in that pane, and nothing is offered on a
  calendar that may not invite. `Tab` from the title moves to the box once
  it has been showing for a moment (an answer landing as Tab is pressed does
  not move where Tab goes); `↵` there adds, `⌫` or `x` dismisses, Esc goes
  back to the title, and the keyboard leaves the box when the offer changes
  to somebody else. A lookup that failed is not remembered as "nobody": the
  name is asked again the next time the names change.
- **The assistant drafts events** (§10).

**Keymap as built.** `1` to `6` the rail (Calendar `2` while offered), `⇧C`
the calendar; on the page `m` / `w` / `a` month, week and agenda, `j` / `k` the selection,
`↵` open, `y` going, `x` dismiss, `n` new event, `e` edit, `⌫` delete; in the
month the arrows move the day, `[` `]` (or `,` `.`) the month, `t` today, Esc
closes the item, then the day; in the week `←` `→` move the day, `[` `]` the
week, `↑` `↓` walk the day's items; in the event pane `Tab` from the title
reaches a suggested guest (`↵` adds, `⌫` dismisses), `⌘↵` saves, `↵` saves from a
single-line field or picks a highlighted place, `↑` `↓` move in the place
list, Esc closes the place list, then the delete question, then the pane.

- **Sitrep agenda.** Replaces the Calendar zone: today and the next few days,
  real events and ghosts together, with the next commitment called out. This
  is the daily surface. Most days the user needs nothing else.
- **Day column.** Hours down the side, real events solid, ghosts by band,
  estimated tails soft. It is the unit the other views are built from, and the
  body of the time pane (§7).
- **Week.** Seven day columns. BUILT in wave 6 (above).
- **Month** (DECIDED: in the first version). The shape of the month: multi-day
  spans for trips and stays, load per day, ghost marks, and a day opens its
  column. Not a wall of truncated titles.
- **Event detail.** Title, time (with "~" on an estimate), place, the
  likelihood reason, and the mail behind it (§9).
- **iOS.** The agenda replaces the Calendar card in `MobileRecordsView`. The
  column and month views follow in the parity pass.

A calendar destination joins the rail, directly under Sitrep and above All
mail (DECIDED). Keys started as the preview drew them (`⇧C` opens the calendar;
`d` / `w` / `m` switch views; `y` / `n` / `x` answer an invitation) and are
adjusted after use ("Keymap as built" above is what is bound today); each is
checked against the existing keymap before it is bound. The time pane is NOT a `SidePanel`
view: that panel takes the modal key context, and composer plus side panel is
already a known sore spot (#265). The pane sits beside the composer.

## 6. Scheduling from the sitrep

Someone writes "Thursday 2pm or Friday morning?". The thread's card shows the
answer without the mail being opened:

    Thu 2:00pm   free
    Fri morning  dentist 9 to 10

- One key or click takes a slot. See "One click to accept" below.
- For a real calendar invitation the key is the RSVP itself, an API call. No
  email is written.
- A candidate that is a range ("Friday morning") is shown with what blocks it,
  and taking it holds the free part of the range (DECIDED).
- Availability on the card is the client joining the daemon's candidate slots
  with the merged timeline.

### In the reader: availability beside the offered times

When an open mail lists times ("Monday, October 19th: 10am to 3pm ET",
"Thursday: 11am to 1pm, 2:45 to 4:30pm ET"), each offered time gets a hint
beside it saying whether the user is free then. The founder's example is an
assistant relaying a week of someone else's windows: five days, seven windows,
all in another timezone.

- **What a hint says.** The window in the user's own time, then one of: free;
  free for part of it (the overlap is named); busy, with what blocks it. A
  window outside the user's usual hours says so ("7 to 8:30am for you")
  instead of calling it free.
- **Windows, not just slots.** An offered time is a range, there can be
  several per day, and they carry the sender's zone. The triage record's
  `candidates` (deferred from wave 1) is a list of ranges with a zone, and the
  meeting may be with someone other than the sender.
- **Drawn in the mail itself** (founder's preference, 2026-10-01; to be
  revisited, tracked in issue #352). The hint sits inline, right where the
  offered time is written. The open design problem is trust: a mail must not
  be able to draw something that passes for Passband saying "free", so the
  inline mark has to be something sender HTML cannot imitate, or be verifiable
  at a glance. The alternative considered is the app's own margin, aligned to
  the line, with a compact list under the message when a line cannot be
  located.
- **Taking one.** Clicking a hint that shows the user as free is the one-click
  accept below.
- **Before the calendar grant** the hint is the time conversion alone, which
  needs no permission. Free and busy appear only once the real calendar is
  connected: a ghost-only "free" would be a guess presented as a fact.

### One click to accept (founder's ask, 2026-10-01)

Clicking an offered time the user is free for does three things at once, and
then the only thing left is Send:

1. **Fills the reply.** Greeting, the acceptance line with the chosen time,
   an empty line with the cursor on it for the user's own words, and the
   signature:

       Taiki,

       Let's do 10am ET on the 21st.

       |

   The acceptance line is the user's own saved sentence with the time
   dropped in, not model output. It ships with one plain default and is
   editable in Settings. The time is written in the zone the sender offered
   it in.
2. **Puts it on the calendar.** The chosen time goes on the user's real
   calendar straight away as an event Passband created, so it blocks the time
   from that moment.
3. **Waits for Send.** Discarding the draft removes the event again.

Rules:

- If the sender then sends a calendar invitation for that time (as in the
  example: "I'll shoot over a cal invite"), the invitation replaces Passband's
  placeholder event; it does not sit beside it.
- An offered WINDOW ("10am to 1pm") needs a start and a length. A click takes
  the start of the free part of the window. The length comes from context
  clues in the thread (a stated length, "a quick intro", the size of the
  window), and is one hour when there are none (DECIDED). The real invitation
  corrects it when it arrives. Choosing a later start inside the window is a
  second gesture, to be designed.
- The filled reply is a light draft, a nudge in the right direction. The user
  finishes and personalises it (DECIDED).
- Needs the calendar grant. Before it, the click still fills the reply and
  keeps a hold inside Passband.

## 7. Scheduling from the composer

- **Trigger.** Typing scheduling language ("we should meet at", "are you free",
  a date or weekday) slides in the time pane. Detection is local pattern
  matching on the client, like `SearchIntent`: no model call per keystroke.
- **The pane** shows the day column for the dates in play and a short list of
  suggested slots.
- **Suggestion ranking:** free in the merged timeline; inside the user's usual
  meeting hours; sensible in the recipient's timezone, inferred from the
  `Date` headers of their past mail; not hard against another commitment; and
  ranked down for the morning after a late event.
- **Click a slot** to insert its time as text at the cursor. Nothing else is
  written.
- **A time in a draft is a chip, shown in the user's own time** (DECIDED,
  2026-10-01). In the composer an inserted time reads in the user's zone
  ("7a PT") inside a small card, like the inline code cards. Hovering it says
  what goes out: "recipient sees 10a ET". On send the chip becomes plain text
  in the recipient's zone. The user thinks in their own time and never
  converts; the recipient reads theirs.
- **Every recipient gets a time in their own zone** (DECIDED, 2026-10-01),
  and the zone is ALWAYS written out, never implied. A mail has one body that
  every recipient reads, so the chip sends each distinct recipient zone once,
  labelled: "10a ET / 7a PT". A zone comes from the times the sender offered,
  else from that recipient's inferred zone when it is known with confidence. A
  recipient with no known zone is covered by the user's own zone, which is
  then included.
- **Clicking the chip lists each person with the time they will see**
  ("Taiki: 10a ET", "Jesse: 10a ET", "Dana: 7a PT", "Sam: zone unknown, sees
  7a PT"). This is also where the user sees what was inferred about each
  person and can correct it.
- Not built: a separate copy of the mail for each recipient, each showing
  only their own time. It would break reply-all, because every recipient
  would be answering a different message.
- **How the zone is inferred** (DECIDED: use every signal, and save the result
  on the sender's profile). Strongest first:
  1. A zone the user set by hand on the contact card. It always wins, and is
     kept as they wrote it.
  2. The zone in a calendar invitation that person organized: the IANA `TZID`
     of the first event's start, credited to the invitation's `ORGANIZER`.
     Anyone can write an invitation naming anyone as organizer, so one counts
     only when the mail passed authentication and its From IS the organizer's
     address. Authentication vouches for a domain, and some domains are
     shared by strangers that no list will ever fully name, so an invitation
     a delegate or a calendar address sends for someone records nothing;
     their own mail still teaches their zone through its offsets. A forged
     invitation records nothing. A zone is counted under its canonical name:
     an alias with the same clock as a listed zone all year becomes that zone
     ("US/Pacific" counts as America/Los_Angeles). Where several listed zones
     share that clock, the one named for the same city wins
     ("America/Buenos_Aires" is America/Argentina/Buenos_Aires, not Sao
     Paulo), then the one in the same region ("Australia/West" is Perth),
     then the first listed. So an old and a new spelling of one place count
     as one zone; two names in one region for one clock can still split a
     vote ("Asia/Kuala_Lumpur" counts as Shanghai while "Asia/Singapore" is
     itself). Of their newest 20 invitations not dated ahead, those whose
     mail counts vote (one in spam or sent mail among the 20 does not, and
     no older invitation takes its place), and a zone holding at least half
     of those votes wins. Windows zone names, names without a `/` ("EST",
     "UTC"), UTC starts and the zone in the subject line (Google writes the
     recipient's) are ignored.
  3. The UTC offset in the `Date` header of their own authenticated mail (not
     sent, not spam): at least three non-UTC offsets from the last year,
     newest 500 at most. Mail stamped UTC is ignored, which also discards most
     automated senders, and means a London correspondent only shows up in
     summer.
  Mail dated more than a day ahead counts for neither 2 nor 3: newest comes
  first, so a future date would otherwise hold its place in the window.
  An offset is not a zone. Each offset is checked against a list of about 45
  representative zones at the moment that mail was sent, and the first zone
  in the list's priority order that explains 90% of them wins. It is marked
  high confidence only when no other listed zone explains the same mail.
  Observations either side of a daylight-saving change usually settle that;
  one season alone often does not (Denver and Phoenix agree all winter), and
  then the zone is a best guess marked as lower confidence. A traveller or a
  shared mailbox gets no zone.
- **Where it lives.** Ingest keeps each message's `Date` offset on the message
  row (`received_at` alone loses it, and unauthenticated mail's is kept but
  never counted) and each bound invitation's canonical zone beside it,
  and whenever a message brings a non-zero offset or an invitation, the
  sender's zone is recomputed from those rows and stored with its source and
  confidence. It is served on the person record behind the contact card
  (`GET /client/person`), where the user can see it and correct it
  (`PUT /client/person/timezone`; clearing the correction hands it back to
  inference), both only while the `calendar` flag is on (§11); inference
  runs either way. The Date offset is written by the message upsert itself,
  in the same INSERT as the row, so ingest writes each message once. It
  learns forward from the day it ships: the raw mail is not
  kept, so filling in existing correspondents would take a Gmail request per
  message. Human door only.

The peek alone is table stakes; other clients have had it for years. Holds
and settling are what make it better.

## 8. Real calendar access

Ghosts need no permission. Everything else in this section needs the user's
real calendar.

- **Google Calendar API, in the daemon.** It covers every Passband user (all
  are Gmail today), works on hosted with the Mac asleep, works on iOS, and is
  the only path to RSVP, invitations with attendees, and calendar context in
  triage. Sync is incremental and stores events in the tenant's database.
- **Scope.** `calendar.events` (read and write events on every calendar the
  user can open) plus `calendar.calendarlist.readonly` (which calendars they
  subscribe to and show), asked for once
  (`CALENDAR_SCOPES` in `squelch-core/src/config.rs`). "Calendar is granted"
  means a token's recorded scopes include `calendar.events`; the list scope is
  wanted, not required. Classification is still to be confirmed against
  Google's list at filing; the working assumption is sensitive, not
  restricted. `docs/VERIFICATION.md` carries the declarations and the
  justification.
- **Two doors still hold.** Sync reads. Writes happen only in human-door
  handlers, as with Gmail.
- **EventKit, on the client, as a supplement.** It reads every calendar
  configured on the Mac or phone, including a corporate calendar whose
  administrator blocks third-party apps. It is empty for anyone who has not
  added accounts to the system, cannot create attendees or RSVP, and the
  daemon cannot see it. Use: an optional "include calendars from this Mac"
  source feeding the merged timeline, and a way to prototype the views before
  the API work lands.
- **Outlook** follows through Microsoft Graph with the same shapes.

### Migrating existing users (DECIDED: must be designed in, not bolted on)

**The grant path is BUILT (wave 2, slice A).** What shipped, where it differs
from the plan below:

- **Hosted**: `GET /reconnect?calendar=1&hint=<address>` on the control plane.
  It asks for the three Gmail scopes AND the two calendar scopes in one
  consent, with `include_granted_scopes=true`, and on success runs the
  existing reconnect pipeline unchanged: tenant discovered from the account
  Google says consented, warden recipient and custody check, the one token
  sealed into the Read and Write slots, the durable job (which now records a
  `purpose`, `reconnect` or `calendar`), and the status page. It does NOT ask
  "only for the new scope": an install replaces the tenant's whole
  credentials file, so the calendar grant has to be the SAME token with more
  scopes, and the request has to cover what the replaced token covered.
- **Calendar unticked** at Google: the Gmail floor passes, nothing is
  installed or queued, and the page says calendar was not connected.
- **Signup and reconnect** now also send `include_granted_scopes=true`, so a
  later plain reconnect hands back a token that still carries calendar. They
  do NOT request the calendar scopes.
- **The daemon** records the granted scopes on the stored token
  (`StoredToken.scopes`, from the exchange, every refresh that names them, and
  import), resolves the calendar token with `CalendarCredentials` (Calendar
  slot first, then a Read slot whose scopes include `calendar.events`), and
  reports `calendar {granted, connect_url, connect_command, self_connect}` on
  `/client/stats`.
- **Self-host**: the app's one Connect button works here too. With
  `self_connect: true` the client calls `POST /client/calendar/connect`, and
  the daemon runs the same consent `squelchd auth --calendar` runs (one
  builder, `squelch_core::auth::LoopbackConsent`): it answers the consent URL,
  waits for Google's redirect on a loopback listener (`127.0.0.1:8847`, or
  `SQUELCH_AUTH_PORT`) for at most ten minutes, and stores a separate Calendar
  slot that touches neither Gmail slot. On hosted the endpoint answers 409 and
  the client opens `connect_url`. `squelchd auth --calendar` still does the
  same from a terminal. Checks and storage: docs/SECURITY.md §9.

- **Ghost mode needs no migration.** Every existing user gets the timeline and
  ghosts with zero consent. The grant is asked for at the first moment it is
  needed: seeing real events, automatic adds, or availability.
- **Calendar is an optional grant.** Signup and reconnect currently refuse
  anything less than all three Gmail scopes. Calendar must not join that
  floor, or a user who unticks it at Google is refused entirely. The granted
  scope set is recorded, the daemon reports calendar as a capability on
  `/client` (as it does `invite_sharing`), and the client gates on it.
- **Existing hosted users** grant through a calendar variant of the reconnect
  pipeline, which already does a fresh consent, a durable job and a credential
  swap without re-pairing. BUILT as above: the consent repeats the Gmail
  scopes beside the calendar ones and the token is replaced, not amended.
- **`login_hint` first.** BUILT. The daemon's links carry `hint=<mailbox>`; the
  control plane passes it to Google as `login_hint` and, at the callback,
  installs nothing when a different account consented. The hint is a guard,
  never an authority: the tenant is still found from the account Google names.
- **New signups** are asked for calendar at signup (decided 2026-10-27), even
  while the `calendar` hidden feature is off for them. It is optional, like
  the profile scope: a person who unticks it still gets a mailbox. A daemon
  without the feature holds the scope and calls nothing; when the feature
  turns on, calendar works with no second consent. The callback's feature
  gate applies to the calendar reconnect only, never to signup. Tenants from
  before this still grant through the calendar page.
- **Self-host** connects from the app (`POST /client/calendar/connect`, the
  daemon running the consent on loopback) or from a terminal with
  `squelchd auth --calendar`. Both BUILT, and both build the same consent.
  Since 2026-10-27 plain `squelchd auth` (and `--write`) ends by offering
  that same calendar consent ("[Y/n]", only at a terminal), optional and
  never fatal, while no calendar credential is stored. A separate consent
  into the Calendar slot, so the Read consent screen stays Gmail-only. Not
  on `--export`: an abandoned screen would hold the blob, and importers
  before daemon 0.0.13 refuse a blob with a Calendar entry.
- **Order of release:** control and warden, then the daemon, then the client.
  A client that meets a daemon without the capability shows ghost mode only.

### Calendar sync (BUILT in wave 2)

Read only (slice B). The only writes are the user's own, described under
"Creating and editing events" below.

- **What is read.** The primary calendar always, plus every calendar the user
  shows in Google Calendar (`selected`, not hidden or deleted) when the list
  scope was granted; at most 25, the primary first. A calendar that stops
  being shown loses its events at the next pass. Recurring events are read as
  their instances (`singleEvents`).
- **What is asked for.** Every `events.list` request, the first full one, its
  later pages and every incremental one alike (Google: "Each list request
  should use the same set of query parameters, including the initial
  request"), carries `maxAttendees=100` (it was 1 until wave 5 read guests;
  see "Guests, invited by Google" below), and a `fields` mask naming only
  what the daemon reads: the paging and sync tokens, the calendar's title and
  zone, and of each event its id, status, iCal id, title, description, place,
  colour id, start, end, organizer address and `self` flag,
  `attendeesOmitted`, of each attendee the address, name, `responseStatus`,
  `organizer` and `self`, transparency, link and update time. Attachments,
  conference data and what a guest wrote when answering are never sent. The
  description (added in wave 3, which shows
  it) is laundered and cut to 2000 characters as it is stored, and Google's
  own boilerplate on an event it created from mail ("To see detailed
  information for automatically created events like this one, use the
  official Google Calendar app.", "This event was created from an email you
  received in Gmail.") is dropped with the link after it, so such an event
  shows no notes. A row stored before this is rewritten by the next daily
  full read.
  `calendarList.list` carries a mask of its own, which since wave 3 includes
  each calendar's `backgroundColor`, `colorId` and `accessRole`.
- **Window.** 30 days back to 400 days ahead. The first read of a calendar is
  bounded by the window; later reads use Google's sync token, which Google
  refuses beside `timeMin`/`timeMax`, so changed events outside the window are
  dropped on arrival. A full read is repeated daily, because a sync token never
  carries an event that moves into the window as it slides. A 410 forgets the
  token and reads in full in the same pass. A full read deletes stored events
  it did not see only when it reached Google's last page: a read cut short (a
  failed page, the page cap, shutdown) deletes nothing and keeps the old token.
- **Loop.** Its own task in `squelchd`, started whether or not calendar is
  granted: without a grant it calls nothing and looks again each minute. With
  one it reads every 5 minutes. It never holds the store across a call to
  Google, and no calendar failure reaches Gmail's health.
- **Failures are per calendar.** The whole loop backs off (1 to 30 minutes)
  only when the credential fails, when the primary calendar fails, or when
  Google answers with a quota limit (per user, so the rest of that pass is
  skipped too). Any other calendar that fails (a shared calendar answering 403
  or 404 forever) records its error and its own retry time on its row and
  backs off on its own, from 1 minute doubling to 30, while the others keep
  the 5-minute cadence. A kick that arrives during a quota backoff is
  dropped, not saved for the moment the backoff ends.
- **Shutdown.** A pass in flight is raced against the shutdown signal, so the
  loop stops within moments rather than after each request's 30-second
  timeout in turn; `squelchd` also waits at most 5 seconds for it and then
  aborts it. Each page is its own transaction, so what an interrupted pass
  leaves is consistent: the calendar it was reading keeps its old token and
  rows.
- **Bounds.** Titles and places laundered like a ghost's (200 characters, a
  meeting link without its query); an organizer only when it is an address; a
  link only to a Google page over https; 40 pages of 250 per calendar per
  pass, past which the calendar is reported unavailable. At most 5000 events
  per calendar: Google's order is undefined here, so past the cap the events
  farthest from now by start are dropped (far-future ones first), whichever
  page brought them, and the calendar is marked truncated until a full read
  fits. An all-day event has no zone, even when Google names one on its start.
- **Tables.** `calendar_calendars` (which calendars are read, sync token, last
  sync, last full sync, last error kind, failures in a row, retry time, the
  truncated mark) and `calendar_events` (one row per event, keyed by account,
  calendar and event id, indexed on start). Starts and ends use the ghosts'
  text form and convention: an all-day `end` is the inclusive last day
  (Google's exclusive `end.date` minus one), and none for a one-day event. A
  cancelled event is deleted.
- **Endpoints.** `GET /client/timeline` adds `events` (by start, the ghosts'
  `[from, to)` rule on the start) and `calendar {granted, synced_at, error,
  truncated}`. `error` is `auth`, `quota` or `unavailable`, and reflects the
  credential and the PRIMARY calendar only: a secondary calendar's failure is
  not an account error. `synced_at` is the oldest last read among the primary
  and the secondary calendars not currently failing. One listing serves at
  most 5000 events, counted after the range test and kept nearest now;
  `truncated` is true when the listing cut or a calendar holds fewer events
  than Google has. Events are served only while calendar is granted. Declined
  events are served; the client hides them. `POST /client/timeline/refresh`
  starts a pass, at most once per account per 30 seconds (202 either way,
  `kicked` says which).
- **Match rule.** A ghost carries `matched_event {calendar_id, id}` when a real
  event starts at the same instant (or on the same date, both all-day) and the
  titles are equal after lowercasing and collapsing whitespace, or one contains
  the other. Worked out when the timeline is listed, never stored: it hides a
  duplicate and changes no ghost.
- **The card's place** (`gcal/place.rs`, 2026-10-28). A booking service
  often writes its marketing into the location ("Dingles Public House, Tock is
  now part of the Resy network. Explore even more of our dining network on").
  After each pass that synced, up to 10 locations longer than 40 characters
  with no answer yet (upcoming events first) go one at a time to the small
  `notify` model, which returns the venue and its address when the text gives
  one. Every word of the answer must be a word of the location, checked in
  code; an answer that adds one is not believed and the location is kept. An
  empty answer means the text names no place. Answers are stored per distinct
  location text in `calendar_places` (so a weekly dinner costs one call),
  pruned when no event holds the text, redone when `PROMPT_VERSION` changes,
  billed under `calendar_place`, at most 100 calls a day. The timeline serves
  it as `place` on each event once there is one (`""` for no place); the card
  shows `place`, falling back to `location`, and the editor keeps reading and
  writing `location`.
- **Human door only.** Titles, places, organizers and links never reach an
  agent route, a log line or a triage prompt. The one exception is the card's
  place above: the location string alone, to the small model. A log names a calendar by its
  position in the pass, never its id, which is usually an address.

### Creating and editing events (BUILT in wave 3)

The user creates, edits and deletes events on their own Google calendars from
the calendar page. This is the first thing that writes to a real calendar. It
is a direct user action on a real event, one at a time, not a write derived
from a ghost, so it does not wait on "Before anything writes to a real
calendar" below; it has an audit trail instead. Ghosts are not editable, and
promoting a ghost to a real event is later work.

- **Routes** (human door, bearer authed, `no-store`, in
  `squelch-api/src/calendar_write.rs`):
  - `POST /client/calendar/events` with `{"calendar_id", "title", "start",
    "end", "all_day", "location"?, "description"?, "id"?}` answers 201 with
    the event in the timeline's shape. `id` is the new event's id, chosen by
    the app (below, "A create sent twice").
  - `PATCH /client/calendar/events/{calendar_id}/{id}` with any subset of the
    same fields answers 200 with the event. An absent field is left alone;
    `location` and `description` take `null` to clear. A `calendar_id` in the
    body must be the event's own: moving an event is not offered.
  - `DELETE /client/calendar/events/{calendar_id}/{id}` answers 204. An event
    Google says is already deleted (410) is deleted locally too, and so is
    one it does not know (404) while the calendar is still the user's: the
    primary calendar always is, any other is looked up on the user's
    calendar list (`calendarList.get`), because Google answers 404 too for a
    calendar the user has lost, and for an event they can no longer see.
    "Still the user's" means listed, not `deleted`, AND an `accessRole` of
    `owner` or `writer`: a calendar still listed whose role dropped to a
    reader's is not. Otherwise it is a 410, "that calendar is no longer
    available to you", the row is left for the sync the write kicks, and the
    app says nothing was deleted. A calendar list read that fails is a 502
    and deletes nothing locally.
- **Times.** RFC3339 with an offset for a timed event, `YYYY-MM-DD` for an
  all-day one, both of one precision, the end after the start. An all-day
  `end` is the INCLUSIVE last day, as the timeline serves it (equal to the
  start for one day). Google's `end` is "The (exclusive) end time of the
  event", so the daemon sends the day after the last day and reads Google's
  answer back through the sync's own `read_event`, which subtracts it again.
  An edit that changes any time sends both start and end, the other form set
  to `null` (Google's patch rule: "To delete a field, specify the field and
  set it to `null`"), so an event switched between all-day and timed does not
  keep both.
- **Checked before Google.** Title trimmed, non-empty, at most 200 characters
  (refused, never cut); location and description trimmed and cut to 200 and
  2000; all three lose control and invisible characters and nothing else;
  the
  calendar one being read on which the user is `owner` or `writer`; for an
  edit or delete, a stored event that passes the edit guard. Only then is
  Google called, with `sendUpdates=none` on every write ("No notifications are
  sent"): this wave invites and tells nobody.
- **The edit guard.** An event is `editable` when its calendar's role is
  `owner` or `writer` AND the event is the user's own: Google's
  `organizer.self` ("Whether the organizer corresponds to the calendar on
  which this copy of the event appears"), no organizer named at all, or an
  organizer address equal to the mailbox's (case-insensitive). Another
  organizer's invitation on the user's own calendar is served `editable:
  false` and refused with 403. The guard fails closed: an organizer Google
  names whose address does not read (absent, or not an address) is stored
  as `organizer_unreadable` and is somebody else, and so is an event whose
  stored guest list marks an organizer that is not this calendar
  (`organizer` and not `self`), whatever the organizer field says. A store
  from before the column forgets its sync tokens once, so the next pass
  rereads every calendar in full and sets it. `writerWithoutPrivateAccess` is not offered: it cannot
  write the private events it sees with their details hidden.
- **Sent as typed, stored laundered.** The one deliberate asymmetry: Google
  gets the user's own text whole (a meeting link keeps its passcode, so the
  user's own event still works), while the copy the daemon stores and serves
  is Google's answer read through the sync's `read_event`, laundered like
  every synced event (a link without its query, markup as text).
- **Google first, then local.** Only Google's success changes a row, and what
  is stored is Google's answer (its id, link, organizer and normalized times),
  not the request. A refusal Google answered (a 4xx, or a failure to
  connect at all) is a 502, "Google Calendar did not accept the change",
  code `google_refused`, and leaves every row exactly as it was. A write
  that went out and got NO usable answer (a timeout or a lost connection
  after sending, a 2xx whose body does not read, or Google's own 5xx, which
  its error guide calls "an unexpected error occurred while processing the
  request") is different: Google may have made the change. It is a 504, "Google Calendar may have made this
  change; refresh before trying again", code `uncertain`, stores nothing,
  and kicks a sync, so the client reloads and shows what Google holds before
  anything is offered again (above all a second "tell guests"). So is a
  write Google ACCEPTED whose answer the daemon could not read
  (`failed:answer`) or could not store (`failed:store`): it happened, so it
  is a 504 `uncertain` with its own message, never a refusal the app would
  give the fields back on (verify4 X1). A write is never retried. After every write the sync is kicked,
  so the next pass confirms it.
- **One deadline per write.** Each Google call may take 30 seconds and a
  write can make three (a token refresh, a guest edit's reread, the write),
  more than the app's 45 second wait. So the whole write runs under one 35
  second deadline (`calendar_write::WRITE_DEADLINE`); past it the answer is
  the same 504 and the same sync kick. If the app's own wait runs out anyway,
  or the connection drops mid-request, the app treats it as that 504 too and
  settles against the reloaded page, never "nothing was saved". A request
  that never reached the daemon still says nothing was saved.
- **A create sent twice.** The app names each new event's `id` itself, one
  per open pane: a UUID's 32 hex digits, which Google takes ("characters
  allowed in the ID are those used in base32hex encoding, i.e. lowercase
  letters a-v and digits 0-9", "between 5 and 1024 characters"). Google also
  says "we cannot guarantee that ID collisions will be detected at event
  creation time", so the daemon does not rely on its 409: a create naming an
  id first reads it (`events.get`). An event already there is the answer
  ONLY when it is the event this create would make: the same calendar (the
  read is on the calendar the create names), the same title, location and
  notes (each read back through the daemon's own laundering), start, end,
  all-day flag and guest set (case folded, the calendar's own entry and the
  organizer's left out; a list Google cut short never matches). NOT
  compared: `notify`, which Google's event does not record. That is safe
  only because the app never changes it on a resend: a create is sent again
  only from a frozen pane, and only as the exact body it first sent, so a
  resend never changes `notify` (nor anything else). Then it is answered 200 as it
  stands, with no insert and so no second invitation (audited `ok:existing
  ... notified=false`). Anything else under that id is a 409, "that event id
  is already used for a different event", with no write (audited
  `rejected:id_in_use`). An insert Google answers 409 (`duplicate`, "The
  requested identifier already exists") is read and judged the same way. An
  id already used by an event since deleted is a 409. Any other id shape is
  a 400 before Google hears anything.
- **One create per id at a time.** The daemon holds an in-memory set of
  (account, calendar, id) for the creates in flight. A second create with
  the same key while the first is still running is a 409, "that event is
  already being created", before Google hears anything (audited
  `rejected:in_flight`). The key is let go however the first ends: an
  answer, a refusal, the deadline, or a panic.
- **What this does not close.** The id makes a duplicate unlikely, not
  impossible. A read that lags a just-made insert, followed by an insert
  Google fails to flag as a duplicate, can still make two events with one
  id; and the read is per calendar, so the same id on another calendar is
  not seen. The app's frozen pane (§5) is the other half: after an
  uncertain create it only ever resends the same body to the same calendar.
- **Settling an uncertain write (app).** After an answer that may have
  happened the app reloads the page up to three times. A create is found by
  its id ALONE, on the calendar it was sent to, whatever its title came back
  as: a lookalike, even the user's own new event from another pane, is not
  it (verify5 Y3), and a calendar daemon older than ids (which ignores the
  id) leaves the pane frozen until it is closed by hand. Only a create body
  without an id (none is sent today) falls back to an event that could be
  nobody else's: on that calendar, not on the page before the send, the
  user's own (`editable`, and no guest named as another organizer), with
  the all-day flag, start AND end, and the title (laundered) that were sent.
  Someone else's invitation with the same title at the same
  time is not it (verify4 X3). An edit is confirmed only when EVERY field it
  changed reads AS SENT: times, the all-day flag and the guests exactly, and
  a title, place or notes equal to what was typed once whitespace is
  collapsed and invisible characters dropped. A text field that moved to
  something else (a co-editor's change, or the user's own earlier lost edit)
  is unknown, not confirmed (verify4 X2), and so is text the daemon rewrites
  in ways the app does not copy (a link cut down, markup out, the length
  cap): the pane stays frozen and is closed by hand. A guest's answer or a
  colour changed meanwhile is never looked at. A delete is found when the
  event is gone. A save not found freezes the pane (§5).
- **"Delete and tell guests" that can no longer tell.** If the page reloaded
  under the delete question and the calendar may no longer tell guests, the
  delete is refused with a toast and the question stays up, as a save
  refuses a control that is no longer on offer. It is never sent silently.
- **A disconnect does not lose the write.** Each write's checks, Google call,
  local row and audit row run in a task of their own that the handler
  awaits. If the app goes away mid-request the request is dropped but the
  task is not: Google's answer is still stored and the attempt audited.
- **Errors.** 400 for a bad body (end before start, mixed precision, a title
  empty or over 200, a missing field, an unknown or read-only calendar, two
  `notify` keys), 403 when calendar is not granted or the event is not
  editable, 404 for an unknown event, 409 for a create id already used by a
  deleted event or by a different event, or still being created, 410 for a
  delete on a calendar no longer listed or no longer written to, 502 when
  Google refuses, 504 when Google's answer was lost, the write ran past its
  deadline, or Google accepted it and the answer could not be read or
  stored (above), 500 for anything else. Error bodies are fixed strings and
  never echo the request.
- **What a refusal says.** Every error body from the three write routes is
  `{"error": <fixed sentence>, "code": <code>}`, the code from a CLOSED list
  (`calendar_write::code`). The app keys on the code, and only with the
  code's own status; a code it does not know, or none, is "may have
  happened".

  | code | status | kind |
  |---|---|---|
  | `invalid` | 400 | nothing happened: the body, a guest address, the delete's query |
  | `unknown_calendar` | 400 | nothing happened |
  | `read_only` | 400 | nothing happened |
  | `cannot_invite` | 400 | nothing happened |
  | `guests_read_only` | 400 | nothing happened |
  | `cannot_notify` | 400 | nothing happened |
  | `not_granted` | 403 | nothing happened |
  | `not_editable` | 403 | nothing happened |
  | `not_found` | 404 | nothing happened: no such stored event |
  | `id_in_use` | 409 | nothing happened: the id names a different event |
  | `already_deleted` | 409 | nothing happened: the id's event was deleted |
  | `calendar_gone` | 410 | nothing happened: Google answered the delete 404 and the calendar is no longer the user's to write |
  | `google_refused` | 502 | nothing happened: Google refused the write (or a read or token refresh before it) |
  | `already_creating` | 409 | MAY HAVE: the same create is still out, and may make the event |
  | `uncertain` | 504 | MAY HAVE: the answer was lost, the deadline passed, or Google accepted and the answer could not be read or stored |
  | `internal` | 500 | MAY HAVE: anything else |

  A new code is added to both sides at once, and is "may have happened" to
  any app that predates it.
- **Audit.** One row per attempt in the audit log, in the mail actions' shape:
  action `calendar_event_create`, `calendar_event_edit` or
  `calendar_event_delete`, actor `client-api`, target
  `calendar:<calendar_id>/<event_id>`, detail `ok` or the reason
  (`rejected:body`, `rejected:calendar`, `rejected:read_only`,
  `rejected:not_granted`, `rejected:not_editable`, `failed:target`,
  `failed:google`, `failed:token`, `failed:uncertain`, `failed:answer`,
  `failed:store`,
  `failed:calendar_gone`, `rejected:already_deleted`, `rejected:id_in_use`,
  `rejected:in_flight`; a create answered
  with an event an earlier send made is `ok:existing`). Never a title,
  place, description or guest's address. The target does carry the
  calendar id, which for a personal calendar someone shared with the user is
  its owner's address.
- **Colour and roles on the timeline.** Each event gains `color` (`#rrggbb`:
  the event's own `colorId` from Google's event palette, else its calendar's
  `backgroundColor`, else null), `editable` and `description`. The body gains
  `calendars: [{id, name, color, primary, writable}]`, the calendars being
  read, the primary first. Without the list scope the primary calendar is the
  only one, the user's own (`writable`), with no colour. Google's palettes are
  held in the daemon because `colors.get` does not accept the
  `calendar.events` scope.
- **Not here.** No agent route, no MCP tool, no assistant tool. No RSVP, no
  moving an event between calendars, no recurring-series edits (an edit to an
  instance changes that instance). Guests came in wave 5, below.

### Guests, invited by Google (BUILT in wave 5)

An event can carry guests, and Google Calendar, not Passband, writes and
sends the invitation, the update and the cancellation. Passband composes no
email for this and sends none through Gmail; the existing `calendar.events`
scope covers it. This is the first calendar write that reaches OTHER PEOPLE,
and it cannot be unsent, so the daemon half is built so that nobody is mailed
by accident. The daemon half is BUILT and tested against a fake Calendar
server only: no request of this wave has been sent to the real Google API.

- **What Google's reference says** (read 2026-10-05; the sentences this is
  built on):
  - `sendUpdates`, on insert, patch and delete: "`all`: Notifications are
    sent to all guests." "`externalOnly`: Notifications are sent to
    non-Google Calendar guests only." "`none`: No notifications are sent."
  - `events.patch`: "The field values you specify replace the existing
    values. Fields that you don't specify in the request remain unchanged.
    Array fields, if specified, overwrite the existing arrays; this discards
    any previous array elements."
  - `maxAttendees`: "The maximum number of attendees to include in the
    response. If there are more than the specified number of attendees, only
    the participant is returned."
  - `attendeesOmitted`: "Whether attendees may have been omitted from the
    event's representation."
  - `attendees[].responseStatus`: "Possible values are: `needsAction`,
    `declined`, `tentative`, `accepted`." `attendees[].email`: "This field
    must be present when adding an attendee."
  - The reference also says of the deprecated `sendNotifications` that
    "some emails might still be sent even if you set the value to `false`".
    So `sendUpdates=none` is the daemon asking for silence, not a proof of
    it. See "Not verified" below.
- **The one switch.** A write tells Google `sendUpdates=all` ONLY when the
  request carries the JSON value `notify: true` (`?notify=true` on a
  delete). Absent or `false` sends `none`. Anything else (`null`, `"true"`,
  `1`, two `notify` query parameters, two `notify` keys in a body) is a
  400, never read as yes and never silently as no. `sendUpdates` is always named, and the client type
  (`gcal/client.rs:Notify`) can only say `none` or `all`, defaulting to
  `none`; `externalOnly` is not offered. `notify` alone is not an edit
  ("nothing to change"): there is no "tell them again".
- **Wire.** `POST /client/calendar/events` takes `"guests": ["a@b.c", ...]`
  (optional, at most 50) and `"notify": bool`. `PATCH` takes `"guests"`
  (absent leaves Google's list alone; present is the FULL new list, `[]`
  removes every guest, `null` is refused) and `"notify"`. `DELETE` takes
  `?notify=true|false`. `GET /client/timeline` serves per event
  `guests: [{email, name, response, organizer, self}]` (`response` is
  `accepted`, `declined`, `tentative` or `pending`; always present, `[]`
  when none), `guests_truncated` and `guest_editable`, and per calendar
  `can_invite`.
- **Who may be a guest.** One plain address each (`gcal/write.rs:addr_spec`):
  ASCII `local@domain` with a dotted domain; no display name, angle
  brackets, quoting, comment, address literal, whitespace or control
  character, so a value can only ever be one mailbox. No duplicates once
  case is folded. Never the calendar's owner (the mailbox's own address, or
  the calendar's own id). Each is a 400 with a fixed message.
- **Which calendar may invite** (`gcal/mod.rs:may_invite`). The user's own:
  the primary calendar, or one whose `accessRole` is `owner`. A calendar the
  user merely writes to (`writer`) takes events and takes no guests: an
  invitation from it would go out under someone else's calendar on the
  user's say-so. 400 on create and on edit. For the same reason `notify:
  true` on an EDIT or a DELETE there (an update or a cancellation mailed
  under that calendar's name) is a 400, "guests can only be told about
  changes on your own calendar" (`rejected:cannot_notify`), refused before
  Google hears anything; silent edits and deletes there still work.
- **Which event's guests may change** (`SyncedEvent::guest_editable`).
  `editable`, AND the list is not truncated, AND the user organizes it
  (`organizer.self`, or the organizer is the mailbox's address; an event
  that names no organizer is editable but not one to invite people to), AND
  its calendar may invite. Otherwise a `guests` edit is a 400. (The contract
  named the first three; the calendar condition is folded in so that
  `guest_editable: true` always means the edit will be accepted.)
- **Truncation.** Every events request carries `maxAttendees=100`. Past
  that, Google returns the user's own entry alone and sets
  `attendeesOmitted`; the event is stored with that one entry and
  `guests_truncated`, and its guest list is read-only. So is one with an
  attendee the daemon could not read (no address). A title, time or place
  edit of such an event still works and sends no `attendees` key, so
  Google's list is untouched.
- **A guest edit sends the full list, from Google's own copy.** Because
  patch replaces the array, the edit first reads the event's attendees from
  Google (`events.get`, `fields=attendeesOmitted,attendees`,
  `maxAttendees=100`), then sends: the user's own entry and the organizer's
  exactly as read (this door never removes them), every guest still wanted
  exactly as read (its `responseStatus`, comment, optional flag, extra
  guests), and each new address bare (`{"email"}`), which Google starts at
  `needsAction`. Reading fresh rather than from the stored rows matters: the
  stored copy can be five minutes old, and a guest who answered since would
  have their answer overwritten by the old one. If the fresh read is
  truncated or holds an address-less guest, the edit is refused and nothing
  is patched. Those raw entries live for the one request; only the address,
  name, answer and two flags are ever stored. A guest who answers in the
  instant between the read and the patch can still lose that answer (no
  `If-Match` is sent); the next sync shows what Google holds.
- **Storage.** `calendar_event_guests` (account, calendar, event, position;
  address, name, answer, organizer and self flags), replaced whole with its
  event in one transaction and deleted with it by a trigger, whichever path
  deletes the event. `calendar_events.guests_truncated`. A store from before
  this wave was synced one attendee at a time: the migration clears every
  calendar's sync token so the next pass reads in full with the new
  parameters (Google: "All other query parameters should be the same as for
  the initial synchronization to avoid undefined behavior").
- **Audit.** The row's detail gains ` guests=<n> notified=<true|false>`: the
  number of guests other than the user after the write (before it for a
  delete or a refusal; `<n>+` when the stored list is truncated) and whether
  Google was asked to mail them. On a failure too: a write that timed out
  may have happened, audited `failed:uncertain`. Never a guest's address.
  New reasons: `rejected:cannot_invite`, `rejected:guests_read_only`,
  `rejected:cannot_notify`.
- **Not here.** No agent route, MCP tool or assistant tool. No RSVP to other
  people's invitations. No `externalOnly`. No optional guests, no guest
  permissions (`guestsCanModify` and the rest keep Google's defaults).
- **Not verified** (the fake Calendar server is the daemon's own reading of
  the reference, and nothing here has called Google):
  - that `sendUpdates=none` on an insert or patch that ADDS a guest sends
    that guest nothing at all. The reference's "some emails might still be
    sent" is written beside the deprecated parameter; test with a real
    account before the client offers "save without telling" to anyone;
  - whether Google adds the organizer to `attendees` on an insert that
    names guests (the daemon handles either: it keeps a `self` or
    `organizer` entry when one is there and does not need one);
  - that Google accepts attendee entries handed back with their read-only
    fields (`self`, `organizer`, `id`) in a patch, as its "get followed by
    an update" advice implies;
  - what the invitation looks like when the organizer is a secondary
    calendar the user owns rather than the primary one.

### Who a name is (BUILT in wave 6)

`GET /client/calendar/people?name=<text>`, with the calendar routes behind
the `calendar` feature, bearer authed on the human door and served
`no-store`. It answers `{"people":[{"addr","name"}]}`, at most three, for
the event pane's guest suggestion (§5). No model is involved.

- **Only people the user has written to.** A candidate is a row of
  `contacts` with at least one sent mail. Somebody who has only ever written
  TO the user is never one, so a stranger cannot make themselves a suggested
  guest by sending mail under a matching name. Never the account's own
  address.
- **A given name or its prefix.** The words asked for must each start the
  person's name from its first word ("ste" and "steven n" find "Steven Ng";
  "ng" and "even" do not). The name is the one the user's own sent mail used
  when there is one.
- **Most recent exchange first.** Two-way correspondents rank above people
  the user has only written to, by the older of the last mail each way.
  Mail from them counts only when it is not spam, not sealed (the Auth
  page's own test, never the retired sensitivity column) and passed sender
  authentication.
- **Nothing is added.** The client shows the answer as a suggestion; a guest
  exists only after the user's checkmark, and Google emails nobody without
  the save control that says so (above).
- **A list or a desk is not a person.** A contact with no name from the
  user's own sent mail is named by its newest readable From name; when any
  of that mail carries `List-Unsubscribe`, or a From name shaped like
  "Steven Ng via Team" or "Steven from Acme", the address is not offered at
  all.
- A name under two characters is a 400 (the client asks for nothing
  shorter); over-long or address-like input answers an empty list. The
  probes do not sort: `MAX(received_at)` per sender. The agent door, `/mcp`
  and the assistant have no such route.

### Location lookup (BUILT in wave 4, client only)

The event pane's location line suggests places as it is typed. Google
Calendar's own field is the Places Autocomplete API from Google Maps Platform:
a billed product with its own key, unrelated to the Calendar OAuth grant. It
is NOT used. The client asks Apple instead: `MKLocalSearchCompleter` for names
and addresses, and `MKLocalSearch` for the one picked, which is written into
the field as plain text, "Name, full address" (what Google stores in an
event's location too). No key, no bill, the same on Mac and iOS.

- The text typed into the location field goes to Apple, from the client,
  once it is two characters long. Nothing else about the event does, and the
  daemon is not involved.
- No location permission is requested and no region is set: MapKit biases
  results as it does by default.
- Free text is always allowed. A suggestion replaces what was typed only when
  the user picks it.
- The rules live apart from MapKit (`Lib/PlaceLookup.swift`, tested with a
  fake provider); `Lib/MapKitPlaces.swift` only talks to MapKit.

### Verification and disclosure

- Google verification has not started (2026-09-30). It is not held for
  calendar: the review wants a demonstration of each scope in use and this
  feature does not exist yet. The plan was Gmail first and calendar as an
  amendment. Since 2026-10-27 signup asks for calendar, so calendar is in the
  Gmail submission and the calendar feature has to be demoable before filing
  (or signup stops asking). Trade accepted: every hosted tenant's sealed token
  can now read and write calendars, unused while the feature is off.
- Until then the calendar grant runs under the same unverified 100-user cap as
  everything else.
- `docs/VERIFICATION.md` calls the Gmail mailbox "the entire product surface".
  That document, the scope constants, the privacy policy and the trust page
  all change BEFORE the scope ships. Calendar content reaching the model
  through triage context (§9) is a disclosure of its own.

### Before anything writes to a real calendar

Wave 1's ghosts only ever show. Before a later wave lets one become a real
event, or lets mail move or delete one, these exist:

- **Nothing derived from mail may notify.** Since wave 5 a write can make
  Google email an event's guests (`notify: true`). That switch belongs to a
  human pressing a control that says so. A ghost promoted to a real event, an
  update or cancellation applied from mail, and anything else the daemon
  decides on its own must send `sendUpdates=none` and must never set, add to
  or rebuild a guest list: mail anyone can send would otherwise be able to
  make the user's calendar email third parties, or strip an event's guests.

- **"This mail is wrong."** The user can detach one source from a ghost, so a
  bad message stops feeding it without dismissing the whole event.
- **Backstops on automatic adds.** A committed ghost is added automatically
  only when its sender already has history with the account, never from
  promotional mail, and only from authenticated mail.
- **The user's own reply is the real `agreed`.** A SENT reply accepting the
  time is the trustworthy evidence of agreement. Sent mail contributes nothing
  to ghosts today.
- **One event, two senders.** A booking site and the airline describe one
  flight from two sender scopes, so they are two ghosts by design. The display
  de-duplicates them; identity stays apart.
- **The model's words are attributed.** `basis` and `reason` are the model's
  text and are shown as such, never in Passband's own voice.
- **On a platform, every host shares the scope.** A sender scope is a domain.
  On an event platform every host mails from the platform's domain, so one
  host's message to their guests can cancel or move another host's event if
  the model reads the same ref out of it. Updates and cancellations that
  touch a real event must come from structured platform content (the
  iCalendar UID of a calendar part), not from body text the model read.
- **Evidence must cite the message being decided.** The triage model reads
  the whole thread, so a record's `change`, `commitment` and refs can repeat
  what an earlier message said. They are honoured only when the record's
  evidence cites the message it is attached to.
- **A venue is not an event.** A venue-level URL ref (a restaurant's page, a
  personal meeting room) identifies a place every event there shares. It must
  not identify an event that is written to a real calendar.
- **The all-day range is exact only at midnight.** A date-only ghost is
  listed by date, so a window whose bounds are not midnight in the caller's
  zone can list one in two adjacent windows. Anything that writes from a
  listing uses midnight-aligned windows.

## 9. What only a mail client can do

- **Calendar as a triage input.** Mail from someone the user meets in an hour
  rises. The document sent for the 3pm gets an attention time before 3pm. Mail
  about an event that has passed sinks.
- **The event is the index into its mail.** A key on the flight opens the
  boarding pass attachment and the confirmation number. The ghost's source
  trail (§3.2) is that index.
- **Calendar mail disappears.** With real access, invitations, updates and
  "accepted" notices fold into the event. The record that remains is the
  short list of things awaiting an RSVP.
- **Gaps and contradictions.** A trip with a flight and no hotel. A commitment
  made in prose and never calendared. Two things in one evening. These are the
  candidates for where this goes next (§14).

## 10. Agents

The agent door stays read-only. Whether `/mcp` serves the timeline (with
estimates and likelihood marked as such) is open.

**The in-app assistant drafts events (BUILT in wave 6).** The founder: "lets
give the assistant the ability to create events." It has ONE calendar tool,
`create_calendar_event` (title, start, end, all_day, and optionally a
calendar by name, a location and guest addresses), and the tool writes
nothing:

- **The title and the place are the only text the assistant supplies.**
  Each is one line of at most 200 characters, and over that (or with a line
  break in it) the call is refused, not trimmed. There is no notes or
  description parameter. With guests on an event, Google emails its
  description and place to them, and Passband never puts model-written prose
  in front of other people (§13). A call that sends notes anyway (`notes`,
  `note`, `description`, `body`, `details`, `agenda`, `summary`, `message`,
  in any case) is refused in words rather than dropped, and so is any other
  field the tool does not list ("unknown field"), so the model cannot report
  as saved something that was not; the pane's notes line always opens empty,
  for the user to type in.
- **It never takes the screen or the keyboard from somebody.** It is refused
  unless the assistant's bar is open at that moment (a run goes on after the
  bar closes), and while mail is being written: a typed-in composer or
  inline reply is never closed for it. An email being read is not closed
  either: the pane is parked and waits on the Calendar page. A proposed pane
  never takes the caret, and `↵` and `⌘↵` do nothing in it until the user
  has clicked or typed inside it; its button always works.
- **It loads the calendar once if nothing has.** The feed is otherwise
  loaded only by the Calendar page, so "put this on my calendar" asked from
  an email first reads it (`refreshCalendar`). "No calendar" is said only of
  a feed that arrived with nothing writable; a load that failed says the
  calendar could not be reached.

- It reads the proposal, checks it by the event pane's own rules
  (`EventProposals.draft`, which ends in `EventForm.wire`), and opens the
  event pane on the Calendar page FILLED IN, guests as real guest pills since
  the user asked the assistant for them. It never calls the daemon's write
  routes. The user presses "add event" or "add and invite N" themselves.
  The same rule as mail: the assistant drafts, the person sends.
- The model is answered that the event is drafted and waiting, that nothing
  was added and nobody invited. There is no confirmation card: the pane is
  the confirmation.
- A bad proposal (no title, times that do not read, a calendar that is not
  writable, guests on a calendar that may not invite, the user's own address
  as a guest) is an error the model can correct, and no pane opens.
- A pane that already holds something, on screen or parked, is never
  replaced: the tool answers "finish or close the open event first". A
  proposed pane counts as holding something, so it parks instead of closing
  when the user changes page and a second proposal waits its turn.
- The assistant does not READ the calendar: it gets no event, and a calendar
  name that matches nothing is refused without listing the ones there are.
  No edit and no delete tool. Nothing on `/mcp` or `/client/agent/*`.
- Offered only on the Mac and only while the Calendar page is
  (`AppStore.calendarPageAvailable`, the gate the guide's calendar topic
  uses). With the page off the tool list and the system prompt are the ones
  from before the tool existed, and a call to its name is answered as any
  invented tool is.

## 11. Feature flag

The calendar is the hidden feature `calendar` (`squelch_core::features::CALENDAR`,
in `KNOWN`, so it is a column on `/admin/features`). How hidden features travel
is `docs/FEATURES.md`. For this one:

- **Turning it on.** Hosted: tick the box for a tenant, or "Everyone", on
  `/admin/features`; the daemon hears within five minutes and the app on its
  next stats poll, with no restart and no release. Development and self-host:
  start squelchd with `SQUELCH_FEATURES=calendar`.
- **Off is as if the feature did not exist, on the daemon.** Every route
  behind the page (`GET /client/timeline`, `PATCH /client/timeline/ghosts/{id}`,
  `POST /client/timeline/refresh`, `POST /client/calendar/connect`,
  `POST /client/calendar/events`, `PATCH` and `DELETE
  /client/calendar/events/{calendar_id}/{id}`, `PUT /client/person/timezone`)
  answers the opaque 404
  (`{"error": "not found"}`) from a route layer (`calendar_routes` in
  `squelch-api/src/lib.rs`), after bearer auth and before the handler reads a
  body or calls Google. A refused write writes no audit row: a path that is
  not served is not an attempt at an action. `/client/stats` says `timeline:
  false` and carries no `calendar` object. The sync loop asks the flag at the
  top of every pass and, off, reads no credential and calls Google for
  nothing; the first pass after it turns on syncs. It asks again before each
  calendar of a pass, so a flag turned off mid-pass stops at the next
  calendar (a page already in flight finishes). Each answer is asked at the
  moment of the decision, so a flip takes effect on the next request, pass
  or calendar.
- **Off deletes nothing.** Calendars, events and guest lists already synced
  stay in the store (other people's addresses among them), served by no
  route while the flag is off, and brought up to date by the first pass once
  it is on again. Purging them on off was considered and not done: a flag
  flipped by mistake would cost a full resync of every calendar.
- **What keeps running when it is off,** on purpose: ghost projection in the
  triage commit, and correspondent timezone inference at ingest. Both are
  cheap and never leave the daemon, and keeping them means a mailbox switched
  on sees its recent history at once instead of starting empty. What they
  produce is NOT served while it is off: `GET /client/person` carries no
  `timezone` key (as on main), and `PUT /client/person/timezone` is one of
  the gated routes (404).
- **The hosted calendar consent** (`/reconnect?calendar=1`) is refused for a
  tenant without the feature (its own row or the Everyone default), with
  "Calendar isn't available for this mailbox yet." It is refused at the
  CALLBACK, after Google, not at the start: the start names no tenant by
  design (the `hint` is a guard that is never used to look anything up), so
  refusing there would make the link an oracle for which mailboxes have
  calendar. Nothing is installed or queued. The plain reconnect is untouched.
- **The apps.** `CalendarGate.pageAvailable` (in
  `passband/Sources/Passband/Lib/Agenda.swift`) is the daemon's `timeline` AND
  `store.hasFeature(.calendar)`. Developer mode no longer has anything to do
  with it. The rail tile, `⇧C`, number key 2 and the shift of the later pages,
  the Go menu and the shortcuts sheet all follow it. The rail and the Go menu
  read a stored "page available" value that the stats poller writes only
  when it changes, so they do not rebuild on every poll. The product guide
  behind ⌘K help (`Lib/ProductGuide.swift`), its topic index and the
  assistant's `product_help` schema mention the calendar (its topic, its
  getting-around line, its shortcut group) only while the page is available;
  with it off their text equals main's. If the flag turns off while the page
  is open, the app goes to the Sitrep, closes the event pane (an unsaved
  draft is discarded, as on an account switch; leaving the page by
  navigation instead parks it, §5) and drops the calendar state. A write
  whose answer lands after the gate closed is dropped, with no analytics.

## 12. Build order

Each wave is useful on its own and is committed when verified.

1. **Ghost engine (daemon, no new scope).** Record fields, prompt bump,
   `ghost_events` reconcile with identity and lifecycle, commitment, estimated
   end with the kind table, model tier and the user's corrections as history,
   `/client/timeline` and the ghost PATCH. BUILT (§3.6).
2. **Timeline (client, no new scope).** Opus 5.5 visual pass first. Merged
   timeline model, day column, sitrep agenda replacing the Calendar zone,
   event detail with its mail, month. Ghost actions before the grant: dismiss,
   "going", adjust the end. PARTLY BUILT: a Calendar page with an agenda,
   event detail with its mail, going and dismiss. Not built: the day column,
   week, month, the sitrep agenda, end editing, several accounts merged.
3. **Likelihood.** Outcome ledger, blended score, the three bands, the
   one-line reason.
4. **Real calendar.** Scope and optional grant, the reconnect variant and
   `login_hint`, Google sync, real events on the timeline, automatic adds with
   undo, RSVP, self-healing of events Passband created, real calendar
   entries feeding the history tier for estimates.
5. **Scheduling.** Candidate windows in triage, availability on the card and
   beside offered times in the reader, the
   compose trigger and time pane, click to insert, holds, settle.
6. **Mail and time together.** Calendar context in triage, folding calendar
   mail into events, gaps and contradictions, EventKit union, iOS views,
   Outlook.

## 13. Not building

- Model-written email text of any kind.
- Automatic rescheduling of the user's day.
- Booking-link pages.
- Web lookups about venues or events.
- Bills, deadlines and deliveries as time blocks.

## 14. Open

- Where this goes to be more than a very good calendar. The leading candidate
  is §9's gaps and contradictions: things only a reader of all the mail can
  know about the user's time.
- A reservation in the user's name that someone else booked: committed or
  invited? Leaning committed.
- A time the user accepts with one click is written to the real calendar
  (DECIDED, §6). Still open: whether slots the user merely OFFERS from the
  composer are written too, or stay holds inside Passband.
- The default acceptance line, and whether its wording and time format ("10a"
  or "10am") should follow the user's own past replies.
- How a time chip reads when the recipients span many zones (four or more
  labels in one sentence).
- Which calendar inside an account receives writes when there are several.
  Leaning primary.
- How much an expired, unanswered ghost counts as a "no".
- Whether the bands appear before there is any outcome history, or only the
  triage prior is used silently until then.
- Whether to backfill senders' `Date` offsets for existing correspondents (a
  headers-only fetch) or let the timezone inference learn forward only.
- The month view (hour strips per day, no titles) is the least conventional
  surface and has not been judged by the founder yet.
- Whether the agent door serves the timeline.
- Google's scope names, their classification, and the amendment process, all
  to be verified at build time.
