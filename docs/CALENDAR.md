# Calendar

Most of what lands on a calendar is born in mail: the invitation, the flight,
the dinner booking, the "does Thursday work?". Passband already reads all of
it. This feature turns that reading into a calendar the user rarely has to
manage, and gives mail the one thing it has never known: what the user's time
looks like.

DRAFT, 2026-09-30. Only wave 1 (the ghost engine, §3.6) is built; nothing
else here is. Sections marked DECIDED carry a
product decision made that day; everything else is a proposal. §14 lists what
is still open.

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
- **No calendar access.** The only Google scopes are the three Gmail ones and
  the identity pair (`docs/VERIFICATION.md`). Hosted signup and reconnect
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

The prompt is `agent-triage-v6`. It tells the model that `duration_estimate`
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
sent, spam, restricted or sealed keeps its row and its keys and supplies no
facts; one whose decision does not deserialize keeps its row and contributes
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
sent, not spam, not restricted, not sealed by `triage.sensitivity`, the
predicate the other human-door listings use), judged in SQL, so a status no
rebuild has
seen yet cannot serve facts from it. At most 2000. A timed start compares as
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

- **Sitrep agenda.** Replaces the Calendar zone: today and the next few days,
  real events and ghosts together, with the next commitment called out. This
  is the daily surface. Most days the user needs nothing else.
- **Day column.** Hours down the side, real events solid, ghosts by band,
  estimated tails soft. It is the unit the other views are built from, and the
  body of the time pane (§7).
- **Week.** Seven day columns.
- **Month** (DECIDED: in the first version). The shape of the month: multi-day
  spans for trips and stays, load per day, ghost marks, and a day opens its
  column. Not a wall of truncated titles.
- **Event detail.** Title, time (with "~" on an estimate), place, the
  likelihood reason, and the mail behind it (§9).
- **iOS.** The agenda replaces the Calendar card in `MobileRecordsView`. The
  column and month views follow in the parity pass.

A calendar destination joins the rail, directly under Sitrep and above All
mail (DECIDED). Keys start as the preview drew them (`⇧C` opens the calendar;
`d` / `w` / `m` switch views; `y` / `n` / `x` answer an invitation) and are
adjusted after use; each is checked against the existing keymap before it is
bound. The time pane is NOT a `SidePanel`
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
  inference). It learns forward from the day it ships: the raw mail is not
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
  reports `calendar {granted, connect_url, connect_command}` on
  `/client/stats`.
- **Self-host**: `squelchd auth --calendar` mints a separate Calendar slot and
  touches neither Gmail slot.

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
- **New signups** do NOT see calendar yet: signup asks for Gmail only, and the
  calendar is granted later from the calendar page. Folding it into signup is
  a later decision.
- **Self-host** gets a `squelchd auth` variant: `squelchd auth --calendar`
  (BUILT).
- **Order of release:** control and warden, then the daemon, then the client.
  A client that meets a daemon without the capability shows ghost mode only.

### Calendar sync (BUILT in wave 2)

Read only (slice B). Nothing writes to a Google Calendar.

- **What is read.** The primary calendar always, plus every calendar the user
  shows in Google Calendar (`selected`, not hidden or deleted) when the list
  scope was granted; at most 25, the primary first. A calendar that stops
  being shown loses its events at the next pass. Recurring events are read as
  their instances (`singleEvents`).
- **Window.** 30 days back to 400 days ahead. The first read of a calendar is
  bounded by the window; later reads use Google's sync token, which Google
  refuses beside `timeMin`/`timeMax`, so changed events outside the window are
  dropped on arrival. A full read is repeated daily, because a sync token never
  carries an event that moves into the window as it slides. A 410 forgets the
  token and reads in full in the same pass.
- **Loop.** Its own task in `squelchd`, started whether or not calendar is
  granted: without a grant it calls nothing and looks again each minute. With
  one it reads every 5 minutes and backs off from 1 to 30 minutes on errors.
  It never holds the store across a call to Google, and no calendar failure
  reaches Gmail's health.
- **Bounds.** Titles and places laundered like a ghost's (200 characters, a
  meeting link without its query); an organizer only when it is an address; a
  link only to a Google page over https; 5000 events per calendar; 40 pages of
  250 per calendar per pass, past which the calendar is reported unavailable.
- **Tables.** `calendar_calendars` (which calendars are read, sync token, last
  sync, last full sync, last error kind) and `calendar_events` (one row per
  event, keyed by account, calendar and event id, indexed on start). Starts and
  ends use the ghosts' text form and convention: an all-day `end` is the
  inclusive last day (Google's exclusive `end.date` minus one), and none for a
  one-day event. A cancelled event is deleted.
- **Endpoints.** `GET /client/timeline` adds `events` (by start, the ghosts'
  `[from, to)` rule on the start) and `calendar {granted, synced_at, error}`,
  where `error` is `auth`, `quota` or `unavailable`. Events are served only
  while calendar is granted. Declined events are served; the client hides
  them. `POST /client/timeline/refresh` starts a pass, at most once per account
  per 30 seconds (202 either way, `kicked` says which).
- **Match rule.** A ghost carries `matched_event {calendar_id, id}` when a real
  event starts at the same instant (or on the same date, both all-day) and the
  titles are equal after lowercasing and collapsing whitespace, or one contains
  the other. Worked out when the timeline is listed, never stored: it hides a
  duplicate and changes no ghost.
- **Human door only.** Titles, places, organizers and links never reach an
  agent route, a log line or a triage prompt. A log names a calendar by its
  position in the pass, never its id, which is usually an address.

### Verification and disclosure

- Google verification has not started (2026-09-30). It is not held for
  calendar: the review wants a demonstration of each scope in use and this
  feature does not exist yet. Gmail is filed first and calendar is added as an
  amendment. How an amendment is reviewed is to be confirmed with Google's
  current documentation at filing time.
- Until then the calendar grant runs under the same unverified 100-user cap as
  everything else.
- `docs/VERIFICATION.md` calls the Gmail mailbox "the entire product surface".
  That document, the scope constants, the privacy policy and the trust page
  all change BEFORE the scope ships. Calendar content reaching the model
  through triage context (§9) is a disclosure of its own.

### Before anything writes to a real calendar

Wave 1's ghosts only ever show. Before wave 4 lets one become a real event,
or lets mail move or delete one, these exist:

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
estimates and likelihood marked as such) is open. The in-app assistant may
create an event only behind its existing confirm step.

## 11. Feature flag

All of this ships dark behind a feature flag so it can be tested on real
accounts before anyone else sees it. A proper flag system does not exist yet
and is being designed separately; this feature is its first customer. Until
it lands, the daemon capability in §8 gates the server half.

## 12. Build order

Each wave is useful on its own and is committed when verified.

1. **Ghost engine (daemon, no new scope).** Record fields, prompt bump,
   `ghost_events` reconcile with identity and lifecycle, commitment, estimated
   end with the kind table, model tier and the user's corrections as history,
   `/client/timeline` and the ghost PATCH. BUILT (§3.6).
2. **Timeline (client, no new scope).** Opus 5.5 visual pass first. Merged
   timeline model, day column, sitrep agenda replacing the Calendar zone,
   event detail with its mail, month. Ghost actions before the grant: dismiss,
   "going", adjust the end.
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
