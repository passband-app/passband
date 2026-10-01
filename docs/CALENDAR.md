# Calendar

Most of what lands on a calendar is born in mail: the invitation, the flight,
the dinner booking, the "does Thursday work?". Passband already reads all of
it. This feature turns that reading into a calendar the user rarely has to
manage, and gives mail the one thing it has never known: what the user's time
looks like.

DRAFT, 2026-09-30. Nothing here is built. Sections marked DECIDED carry a
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
- **No generated prose, ever.** Passband does not draft email. Actions create
  events, hold slots, RSVP through an API, or open the reply box with the
  cursor in it. Inserting a formatted fact the user clicked (a time slot such
  as "Tue Oct 6, 2:00pm PT") is allowed; writing the sentence around it is not.
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
  location, evidence }` (`squelch-core/src/triage/decision.rs`, schema in
  `triage/agent.rs`). The structural detector in `triage/calendar.rs` covers
  invites, updates, cancellations, RSVPs and reservations.
- **A record is per message.** There is no end time and nothing linking a
  confirmation to its later modification or cancellation, so one dinner can be
  three rows.
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
confirmation, a "your reservation was modified" and a cancellation feed the
same row.

- **Identity key**, in order of preference: the iCalendar UID from an `.ics`
  part, a confirmation or booking number, the event page URL, and failing all
  of those the thread plus a normalized title.
- **States:** `ghost`, `held`, `materialized` (written to the real calendar),
  `cancelled`, `dismissed`, `expired` (its time passed).
- **Self-healing.** A later message about the same identity moves, renames or
  cancels the ghost. If the ghost was materialized by Passband, the real event
  follows. That is the property a hand-made calendar entry never has.
- **Sources are kept.** Every message that created, changed, cancelled or
  confirmed a ghost is recorded with its role. That trail is also the event's
  index into its mail (§9).
- **Expiry replaces the 24-hour hide.** A ghost lives at its time and leaves
  when that time passes. `CalendarVisibility` is deleted.
- **Sealing a source message** removes any ghost that message alone fed, as
  with shipments.

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

- The estimate is stored as a duration with its basis and its source tier,
  separate from `start` and from any stated `end`. No consumer can mistake it
  for a fact.
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

### 3.6 Data sketch

**Triage record.** The `event` record gains fields. Each needs
`#[serde(default)]`, because reconcile re-reads every stored `decision_json`
and one that fails to deserialize fails the pass.

| Field | Notes |
|---|---|
| `end` | stated end, only when the mail says so |
| `duration_estimate` | `{ minutes, basis, source }`, only when `end` is absent |
| `event_kind` | the kinds in §3.1 |
| `commitment` | `committed` / `invited` / `proposed`, with evidence |
| `likelihood` | prior band plus reason, for non-committed events |
| `identity_refs` | iCalendar UID, confirmation numbers, event URL |
| `candidates` | the offered slots, for a `proposal` |
| `change` | `created` / `updated` / `cancelled` / `agreed` |

This is a prompt version bump (currently `agent-triage-v4`, with v5 in flight
in PR #342). Rollout is forward-only: history is not re-triaged. A bounded
pass may redo only messages whose existing event record starts in the future.

**Daemon tables**, all human-door only:

- `ghost_events`: identity key, kind, title, start and zone, stated end,
  estimate (minutes, basis, source), location, commitment, state, likelihood
  and reason, the real event id once materialized, and whether Passband
  created that event.
- `ghost_event_sources`: ghost, message, role. The evidence trail.
- `ghost_outcomes`: the ledger behind likelihood (organizer, platform, kind,
  outcome, when).
- `event_duration_priors`: venue key and kind to observed durations.
- Proposals with several candidate slots are several ghosts sharing a group
  key, which collapse to one when the thread settles.

Ghosts are a projection reconciled from stored decisions, the same shape as
`agent_delivery_projections` for shipments.

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

- One key takes a slot: it becomes a held ghost and the reply box opens with
  the cursor in it. The user writes the reply.
- For a real calendar invitation the key is the RSVP itself, an API call. No
  email is written.
- A candidate that is a range ("Friday morning") is shown with what blocks it,
  and taking it holds the free part of the range (DECIDED).
- Availability on the card is the client joining the daemon's candidate slots
  with the merged timeline.

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
- **The recipient's zone.** When the recipient's timezone is known with
  confidence and differs from the user's, the inserted text carries both
  ("Tue Oct 6, 2:00pm PT / 5:00pm ET"). The pane says what it inferred ("Sam
  is usually on Eastern time") before anything is inserted. With no confident
  inference, only the user's zone is inserted.
- **How the zone is inferred**, strongest first: the zone named in a calendar
  invitation from that person; then the UTC offset in the `Date` header of
  their own mail, taken as the most common non-UTC offset over several recent
  messages. Automated mail and clients that stamp UTC are ignored. An offset
  is not a zone: two observations either side of a daylight-saving change pin
  it down, one alone gives only "their time is 3 hours ahead". The daemon does
  not store the sender's offset today (`messages` keeps `received_at` only),
  so it has to be captured at ingest, and it learns forward from there.
- **Holds.** Every slot the user offers becomes a held ghost tied to the
  thread, so the same hour is not offered twice. Holds take effect on send,
  including a scheduled send.
- **Settle.** When the reply picks a slot, triage marks the proposal agreed:
  the chosen hold becomes the event and the others are released. Holds also
  release when the thread goes quiet past the offered times.

The peek alone is table stakes; other clients have had it for years. Holds
and settling are what make it better.

## 8. Real calendar access

Ghosts need no permission. Everything else in this section needs the user's
real calendar.

- **Google Calendar API, in the daemon.** It covers every Passband user (all
  are Gmail today), works on hosted with the Mac asleep, works on iOS, and is
  the only path to RSVP, invitations with attendees, and calendar context in
  triage. Sync is incremental and stores events in the tenant's database.
- **Scope.** One event read and write scope, asked for once. Two consents
  would mean two trips through the unverified-app screen. The exact scope
  names and their classification are to be checked against Google's current
  list before building; the working assumption is that calendar scopes are
  sensitive, not restricted.
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
  swap without re-pairing. It asks only for the new scope, incrementally.
  Whether the old refresh token gains the scope or must be replaced is to be
  tested, not assumed.
- **`login_hint` first.** The reconnect flow does not pin the Google account.
  A calendar grant for the wrong account is worse than a failed reconnect.
- **New signups** see calendar in the one signup consent, safe to untick.
- **Self-host** gets a `squelchd auth` variant.
- **Order of release:** control and warden, then the daemon, then the client.
  A client that meets a daemon without the capability shows ghost mode only.

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
   end with the kind table and model tier, a `/client/timeline` endpoint.
2. **Timeline (client, no new scope).** Opus 5.5 visual pass first. Merged
   timeline model, day column, sitrep agenda replacing the Calendar zone,
   event detail with its mail, month. Ghost actions before the grant: dismiss,
   "going", adjust the end.
3. **Likelihood.** Outcome ledger, blended score, the three bands, the
   one-line reason.
4. **Real calendar.** Scope and optional grant, the reconnect variant and
   `login_hint`, Google sync, real events on the timeline, automatic adds with
   undo, RSVP, self-healing of events Passband created, history tier for
   estimates.
5. **Scheduling.** Candidate slots in triage, availability on the card, the
   compose trigger and time pane, click to insert, holds, settle.
6. **Mail and time together.** Calendar context in triage, folding calendar
   mail into events, gaps and contradictions, EventKit union, iOS views,
   Outlook.

## 13. Not building

- Generated email text of any kind.
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
- Are holds written to the real calendar as tentative events (coworkers see
  the user as busy) or kept inside Passband?
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
