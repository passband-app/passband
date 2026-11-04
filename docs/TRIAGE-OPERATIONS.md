# Agent triage operations

This rewrite switches directly to model-owned triage. There is no shadow pipeline.
The product decisions are in [TRIAGE-REWRITE.md](TRIAGE-REWRITE.md); this file
explains the implementation and tuning surface.

## Execution

Ingest parses and sanitizes MIME, stores the message, and atomically queues work.
It does not classify, seal, infer receipts, close bills, or apply sender rules.
The legacy triage columns hold neutral compatibility values until projected by
an API. They are not the source of destination membership.

Two durable workers operate independently:

- Notification: a small model assesses auth and notification importance. Every
  auth assessment qualifies, including informational login/security alerts.
- Investigation: a bounded agent can read messages, threads, sender history,
  search results, stored records, and text/JSON attachments; optionally request
  a configured review model; then commit an evidence-backed decision.

Explicit model Delivery records also feed carrier polling. Retraction removes
agent-created polling rows when no proposal remains, while preserving existing
legacy tracking and carrier observations. MCP deadlines and shipments read the
canonical decisions and guarded carrier facts.

Binary/PDF attachment extraction is not implemented by the agent evidence reader.
It returns an explicit unsupported result; the prompt prohibits claiming to have
read unavailable evidence. Stored plain text is paged, with Unicode character
positions. Context and tool results have separate byte budgets.

Model output owns categories, Reading membership, typed record facts, thread-level FYE
membership, extracted facts, auth classification, external access, and proposed
revisits. Structural validation checks IDs, evidence, numeric ranges, and observed
revisions. User done/snooze/corrections and completed actions remain authoritative.
Corrections survive content revision changes; thread-level FYE choices apply to
every sibling. External read guards independently honor human restrictions even
if an older assessment was incorrectly marked allowed.
Archive rows (`is_archive = 1`) get no job at ingest. Their access check is queued
only when an external caller surfaces them: trigger `archive_access` (kind `access`)
from an agent search's keyword matches or from an explicit thread open at the agent
door, both inside the ceilings below. `source_access` is never queued on an archive
row. Until then external thread reads omit them rather than close the thread; see
SECURITY.md section 4.

`archive_access` is charged to the total and access-lane budgets AND to its own
`archive_access_daily_cap` (default 50, config file only; 0 stops surfacing). A
search queues at most ten and a thread open at most eight, never more than twenty
outstanding per account, never more than today's sub-cap can still pay for, and nothing once
today's access budget is spent, or the total. The job is claimed in the `manual:`
tier only while it has never been deferred or retried (`last_error IS NULL`); after
that it waits its turn like any other job, on the access lane's budget either way,
and a grant on the access lane releases it only within its sub-cap's room. These
jobs are not in the "triaging N" count, the budget-parked count or the queue
gauge; the held count includes them. A failed one may be asked for again after 24
hours. Any other trigger that reaches a row still in the archive is also queued as
kind `access`, never `triage`: only the owner's single-message re-triage or filing
correction promotes the row to live mail first.
Only matching sender rules are supplied as preferences and may be overridden with an evidenced
exception. Memory has a read-only context interface; no memory editor ships.

## Client projections

Process and the TUI use the server's FYE ordering. Reading and Records default to
unfinished items from the last 30 days. Their API routes accept `since` or
`all_time=true`, plus `include_done=true`; `since` and `all_time` are mutually
exclusive. MCP record tools request all time where their contract needs older facts.
Usage counts pending triage separately, without writing semantic results back into
legacy tier columns. Bill records expose amount, due date, and autopay.

Authentication events persist `is_auth` independently of access restrictions, so
foreground clients can present login alerts as well as actionable auth. Relay
payloads still carry only the event ID; the authenticated client fetches content.

## Configuration

All fields below are optional TOML settings. These are starting defaults, not
quality claims. Provider credentials and transport resolution continue to use the
existing `[stage2]` connection settings; `[triage.agent].model` overrides its model.
The independent notification model uses existing `[notify]` settings.

```toml
[triage.agent]
# model = "deployment-model"
# review_model = "optional-review-model"
max_model_turns = 4
max_tool_calls = 8
max_review_calls = 1
# Per model turn; a whole investigation gets timeout_secs x (max_model_turns + 1).
timeout_secs = 90
concurrency = 2
worker_poll_secs = 1
daily_run_cap = 10000
background_daily_run_cap = 200
access_daily_run_cap = 500
archive_access_daily_cap = 50
max_attempts = 6
outage_retry_secs = 300

[triage.context]
initial_thread_messages = 8
max_related_messages = 20
# The newest thread siblings ride with a whole (capped) body; the rest carry
# an excerpt of this many characters, flagged body_excerpted.
full_sibling_bodies = 2
sibling_excerpt_chars = 400
# The most of a condensed body (the target's, the newest siblings') that rides
# whole into the initial context, in characters; past it the model pages the
# stored text with read_message (body_truncated, next_offset).
full_body_chars = 16000
# The most of a body the access check reads, links shortened and quotes kept;
# past it the body is cut and the check told so (body_truncated).
access_body_chars = 8000
max_context_bytes = 120000
max_tool_result_bytes = 24000

[triage.ranking]
urgency_weight = 30.0
action_need_weight = 25.0
personal_relevance_weight = 20.0
recency_weight = 20.0
waiting_weight = 5.0
importance_weight = 2.0
recency_half_life_hours = 24.0
waiting_saturation_days = 7.0

[smart_clear]
model = "claude-sonnet-5"
effort = "low"
price_in_per_mtok = 2.0
price_out_per_mtok = 10.0
```

`[smart_clear]` is the model behind smart clear's one question (see "Smart clear"
below). It resolves its key, provider and URL from `[stage2]` like every other
model here and is prefixed for a gateway at call time. The prices are the
configured model's own and are what both cost estimators charge the
`smart_clear` ledger category; move the model and move them with it. File
only: there is deliberately no env var, so a change takes hold at the daemon roll.

Environment overrides: `SQUELCH_TRIAGE_MODEL`, `SQUELCH_TRIAGE_REVIEW_MODEL`,
`SQUELCH_TRIAGE_MAX_TURNS`, `SQUELCH_TRIAGE_MAX_TOOL_CALLS`,
`SQUELCH_TRIAGE_TIMEOUT_SECS`, `SQUELCH_TRIAGE_CONCURRENCY`,
`SQUELCH_TRIAGE_DAILY_RUN_CAP`, `SQUELCH_TRIAGE_BACKGROUND_DAILY_RUN_CAP`,
`SQUELCH_TRIAGE_ACCESS_DAILY_RUN_CAP`, `SQUELCH_TRIAGE_MAX_ATTEMPTS`, and
`SQUELCH_TRIAGE_OUTAGE_RETRY_SECS`. Other agent levers are configured in TOML. Scores order only agent-selected FYE threads;
no weight or importance threshold can add a thread to FYE. Recency uses relevant
message activity, never the time a background job ran.

### Spend and revisit limits

The budget unit remains one bounded model job, not a token or dollar cap. Full
triage permits up to `max_model_turns` calls; an access-only job permits one call.
The account total is `triage.agent.daily_run_cap` (default 10000). It is a
runaway breaker, not a working budget: new mail should never be left untriaged
by it, so it sits far above what any real mailbox receives in a day. Background
work also consumes `background_daily_run_cap` (default 200), within that total.
Together with the access lane's cap below, that bounds ordinary spend; the two
lanes leave 9300 runs for new inbound mail.
Unused arrival capacity is not loaned to background work. Setting the background
cap to zero pauses background jobs. If only the total is lowered, the two lanes
are cut to fit under it, as the next paragraph says, retaining a reserve.
Access checks (kind `access`: the owner's sent mail, spam, and archive rows a
caller asked to read) are charged to their own `access_daily_run_cap` (default
500) rather than the background cap, so a spammy day cannot defer the backfill
to tomorrow, nor the backfill the owner's sent mail. The two lanes fit under the
total minus one together: a lowered total cuts the access lane first, or splits
the room between the two when the background cap alone reaches it, the daemon
notes the caps in force on stderr at boot, and `/client/triage-config` reports
them and what they leave for new mail.

Migration/backfill, thread refreshes, and autonomous revisits are background
work; sent-mail access and external-demand access are access work, on the
access lane's budget. First classification from incremental
sync uses arrival capacity independently of the sender's Date header and push
eligibility. The sync origin is persisted explicitly; an initial mailbox import
cannot pretend to be new arrivals merely because its messages are newly stored.

Legacy stage-1/stage-2 escalation caps, including per-thread/per-sender overrides,
no longer gate agent triage. Their compatibility fields remain exposed but the
agent config explicitly reports `legacy_stage_caps_are_active_ceilings: false`.
Configure agent ceilings through the TOML/environment controls above; moving the
usage-page and warden controls to a dollar budget is [follow-up #216](https://github.com/passband-app/passband/issues/216).
`/client/triage-config` reports the configured/effective background and access
caps, the total cap, and the arrival capacity the two lanes leave. When the true total is exhausted, mail stays
human-readable and visibly pending until the next UTC day.

To see where a mailbox stands and to lift a cap for the rest of the day:

```sh
squelchd triage status
squelchd triage grant 500                # 500 more runs today, for new mail
squelchd triage grant 500 --background   # 500 more background runs today
squelchd triage grant 500 --access       # 500 more access checks today
squelchd triage grant 0                  # take the grant back
```

A grant works on a running daemon, lasts until midnight UTC, and replaces any
earlier grant for the day. It releases as many held jobs as the caps now have
room for, new mail first, then background work, then access checks; the rest
stay parked rather than running only to be turned away again. Background and
access runs still count toward the total, so a lane's grant only helps while
the total has room. Autonomous revisits also
answer to `[revisit].daily_cap`, which a grant does not raise, so a grant never
releases them. Each run is a model job, so a grant is spend. The scrape exports
the same numbers as `squelchd_triage_daily_runs`,
`squelchd_triage_daily_run_cap` and `squelchd_triage_jobs_held` (scopes
`arrival`, `background`, `access` and `revisit`).

**Lifting the lanes for one tenant: `uncapped_triage`.** A hidden feature
(docs/FEATURES.md), ticked per tenant on `/admin/features`, for a mailbox whose
mail must never wait for tomorrow. While it is on the daemon drops the
background and access lane caps, and the moment it comes on it releases every
job those caps parked (archive search checks only as far as their own sub-cap
has room). Untick it and the configured caps apply again from the next job.
What it keeps:

- The total, `daily_run_cap`. It never holds a real day's mail and is the one
  breaker left between a runaway (a fan-out re-triage, a stuck loop) and the
  tenant's monthly LLM budget, which, once spent, stops all triage, new mail
  included, until it resets.
- `[revisit].daily_cap` and `archive_access_daily_cap`. Neither is mail
  arriving.

Spend is still charged to every lane, so `squelchd triage status` and the
`squelchd_triage_daily_*` gauges can read over a cap (`access 900 of 500`)
while the flag is on. That is expected; the status command runs outside the
daemon and does not know the flag. The flag works through the tenant's daemon,
so a box ticked before that daemon carries this code does nothing yet.

A sender rule applies to mail that arrives after it is written. Creating,
editing, or deleting one re-triages nothing already stored: every re-triage is a
full investigation, and a catch-all `*` rule (a content filter) once queued the
whole 30-day window. Jobs queued by the old fan-out are closed on the next open.

The fast notification lane retains its separate `[notify].daily_cap` and
`SQUELCH_NOTIFY_DAILY_CAP`; investigation backlog cannot consume that allowance.

Agent revisits use existing `[revisit]` settings and their `SQUELCH_REVISIT_*`
overrides: `enabled`, `batch_per_cycle`, `daily_cap`, `max_per_message`,
`max_per_message_lifetime`, `min_lead_hours`, `max_horizon_days`, and
`dedupe_window_hours`. Defaults allow 4 pending revisits, 6 lifetime revisits,
one-hour minimum lead, a 400-day horizon, 12-hour deduplication, and 50 revisit
investigations/account/day, within the shared account/background ceilings.
Timestamp changes do not evade lifetime limits. Absurd values (a negative lead,
a horizon past ten years) are clamped rather than trusted. The same settings
govern the past-event re-check: a day after the dates the model gave for a
trip, ticket or booking, the poll tick queues a `revisit:event_ended:<time>`
job, and on any `revisit:` job the agent may mark the message done, past
the rails in `apply_done` (docs/CALENDAR.md §3.8). Code schedules; the model
decides; the owner can always undo it. The re-check is asked only about events
the owner is going to, at most three times a message under its own count (not
the lifetime limit), counts as asked only once a `revisit:` run has committed
an answer, and is claimed ahead of other revisits under the shared daily cap.
Only moments that end a message's use (an event, trip, reservation or
appointment ends, a delivery arrives, an invitation's date passes) earn the
model's own revisit. A `revisit:` run moves mail only by its done: it writes
no placement, so a revisit cannot lift mail into FYE or change its actions.
TODO: expired promotions (a sale, offer or promo code that has ended) do not
earn a revisit yet: they are about half of a mailbox's decisions and would
push a landed flight's re-check back by days under the shared 50/day cap; do
it once promo expiry has its own cap or lane, or a cheap path that needs no
investigation. The retired
deterministic sweeps' `deadline_grace_hours` and `fye_stale_days` knobs are
gone; a config file that still sets them is ignored.

## Recovery and diagnostics

Jobs have leases, attempt counts, availability times, and bounded error codes.
Invalid output and stale commits retry with exponential delay, then remain failed
for inspection and manual re-triage. Observable output constraints get repair
turns within the same investigation before consuming a new attempt. Provider
configuration failures, transport failures, rate limits, and server errors
instead leave work pending and retry after a shared cooldown; they do not
consume the terminal-attempt allowance. A configuration failure (a rejected
key, an unknown model) stands the account off for `outage_retry_secs`, less up
to 15 seconds of jitter so a fleet rejected together does not probe together.
A rate limit, a server error or a transport failure (a connection refused, the
client's own timeout) stands it off for the `retry-after` the provider sent
when it sent one (a second to `outage_retry_secs`, with up to 15 seconds of
jitter riding under that ceiling: the ceiling is the longest wait). Without
one, the first server error or transport failure after a success opens no
cooldown at all (one probe retries at once), and a 429 waits 15 to 30
seconds (a rate limit the host's tenants share must not be retried by all of
them in the same instant); every probe that fails in turn doubles the wait,
from 15 seconds, up to
`outage_retry_secs`, and a `retry-after` shorter than the rung reached no
longer shortens it, so a throttle that is not lifting is probed as the outage
it is. After the cooldown, one investigation probes recovery; a successful
response restores normal admission (and the first rung), and another outage
extends the cooldown. Empty refine
polling backs off up to 30 seconds and an ingest wake resets the delay. A
first-call configuration rejection refunds its budget reservation. Earlier
paid calls remain charged. Deadlines are job-local failures that never pause
the account: each model turn has `timeout_secs`, the investigation
`timeout_secs` for every turn it may take and one turn more, and an attempt
that overruns is retried; a second timeout in a row fails the job for
inspection when the provider has answered since the first (an earlier turn of
the same run, or another investigation: the message is what is slow, and every
timed-out attempt is a billed request that re-sends the same context), while a
provider answering nothing, or a process that has not yet seen it answer,
leaves the job to its ordinary attempts, pending rather than failed. An access-call deadline
consumes retry attempts in the usual way. Their uncertain spend is not refunded. A job whose model call failed for any other reason (a deadline, an empty
answer) retries at once, then after 15 s, 30 s, 60 s and so on, never more than
five minutes apart; a failure of the store or the worker itself (a busy
database, a context that changed under the run) waits 30 s, 60 s and so on up
to an hour, so a busy spell cannot spend a job's attempts in seconds. A run
dropped at shutdown hands its job back at once, due after that local ladder's
wait for the attempt it took and keeping that attempt (a job dropped on its last allowed
attempt is failed instead); a process
that dies outright, or a run
whose last write the store refused, holds the lease (the run's deadline and
30 s for an investigation, the access deadline and 30 s for an access check)
until it lapses. Daily budget exhaustion defers work to the next UTC day. Investigations for the
same thread are serialized, including related-source assessment jobs. Immediate
requests coalesce by message/content revision, independent of trigger text.
Changes during a lease accumulate at most one follow-up. Retry absorbs that
follow-up into its existing attempt sequence; trigger churn never resets exhausted
work. A new explicit manual retry can restart it. Scheduled revisits retain their
separate bounded lifetime policy.
Commits compare content, user-state, preference, and evidence revisions within a
transaction. Stale output cannot replace newer user/model state. Passive listing
changes do not invalidate investigations. Worker outcomes appear in
`squelchd_triage_verdicts_total{stage="agent",outcome="..."}`; applied outcomes
also refresh the existing last-success metric.

The fast notification assessment also persists auth classification, score,
reason, model, prompt version, and timestamp for the human inspector.
Applied runs record prompt version, model, configuration, token usage, model-turn
and tool-call counts, and source IDs. Both are kept indefinitely. A retention
pass that deletes them after `HISTORY_RETENTION_DAYS` (keeping each message's
newest applied revisit run, slimmed, because the revisit pass reads it) exists
as `SqliteStore::prune_history`, but nothing schedules it: this history is the
raw material for triage evaluation and cost work, and stays until that work
decides what it needs. Reported usage is retained for refusals,
truncation and invalid output as well. Truncation returns to the bounded caller
instead of silently making another paid transport call. Missing usage remains
unknown; the existing ledger is not durable per-call monetary settlement. Raw provider failures are not persisted as
diagnostics. The decision includes reasons, evidence locations, rule exceptions,
and ranking inputs. No credentials should appear in summaries or notification
text; the model is instructed to describe authentication without reproducing it.

On the first worker pass for an account, cutover queues recent history plus
unresolved/reminded mail silently, skipping messages already queued or assessed.
New arrivals take priority over the migration backlog. It preserves explicit user state and reverses
only identifiable audited legacy bill auto-closures. Historical automatic `done`
rows without reliable provenance cannot be distinguished from a human dismissal
and are conservatively retained. Manual re-triage can reassess chosen messages.

## Surfaces and delivery

`/client/v2/feed` provides the authoritative FYE, Reading, and Records projections.
Reading includes promotional mail by default. Categories and destinations overlap.
`/client/v2/messages/{id}` opens the exact human message while work is pending;
`/opened` acknowledges that message. Human reads do not depend on external access.
The embedded assistant and MCP use separate guarded reads that fail closed for
pending/restricted sources and derivatives of those sources. Reading a legacy
thread through an external agent queues missing access assessments in bounded
batches; ordinary reads never reset exhausted jobs. Human reads and cache probes
never schedule model work. Access jobs use the configured small notification
model with a separate one-call schema, no tools, no placements/records/attention,
and no autonomous follow-ups. Input that cannot fit is rejected without granting
access, rather than silently truncated. Its token usage is recorded separately as
`agent_access`, using the notification model's price configuration. Human TUI
reads remain available while assessments are pending.

### Reading attention and per-sender statistics

`POST /client/v2/messages/{id}/attention` takes `{"dwell_ms": n, "link_clicks": n}`
(either may be omitted) and ADDS the report to that message's totals in
`triage.dwell_ms` and `triage.link_clicks`. One report adds at most 10 minutes and
20 clicks; a message holds at most 1 hour and 1,000 clicks. It answers `204` for
every well-formed report, including one about a message id the daemon does not
know or about mail known to be kept close (an auth or restricted decision, a
restricting correction, access state `restricted`, or a legacy sealed row), and
writes nothing for those two. Mail whose access assessment is still pending IS
written: it is being read before triage reached it, and a refused report is
lost for good. That is safe because every reader of these counts leaves
kept-close mail out when it reads, so a count on a row that later proves to be
a login code is never used. That differs from
`/opened`, which answers `404` for an unknown id: clients treat a `404` from the
attention route as "this daemon predates the route" and stop reporting for the
session. The route writes no audit row (it fires several times per message read)
and changes nothing in the agent's `user_state` fingerprint, so a report cannot
make an in-flight triage commit stale. Human door only.

What is stored is two counts per message. No URL, link host, scroll position or
timestamp of a report is kept, and only the owner's own client can write them.

Smart clear scores each sender with open mail in Reading (the 30-day
inventory `/client/v2/feed?destination=reading&since=...` returns, not the
board the default feed shows: see "The Reading board" below), at most
100 senders, from
per-sender statistics read in `store/sqlite/reading_senders.rs`. Nothing about
them is served to a client.

- Counts cover the sender's inbound, non-spam mail that the owner did not write,
  received since the later of 90 days ago and the start of the open ledger
  (`app_settings.opened_ledger_since`, else the account's `created_at`).
- Mail that is kept close is not counted. The definition lives in one place
  (`store/sqlite/mod.rs`), in two layers. `known_sealed_mail_sql!`: the stored decision is auth mail, or it
  restricts external access, or a correction does, or the access assessment
  came back `restricted`, or the legacy `triage.sensitivity` column says
  sealed. Nothing in the agent pipeline writes that column any more, so a guard
  on the column alone protects nothing that arrives today. `sealed_mail_sql!`
  is that plus mail whose access assessment is still pending. Readers (these
  counts, smart clear's scan and write) use the strict one: unassessed mail does not count for its sender and is not swept,
  which corrects itself when triage commits. The attention write alone uses the
  looser one.
- Mail that failed authentication (`messages.auth_pass = 0`) is not counted
  either: the sender is the From address, and a forgery in a sender's name is not that sender's record. Mail
  never checked (`NULL`) still counts.
- `per_month` scales `received` by the days the ledger covers. Under 7 covered
  days it is `null`, with no volume term in the score and no `high_volume`
  reason.
- `cleared_unopened` counts mail the OWNER cleared: done, never opened, not
  resolved by smart clear (`resolved_by IS NULL`), and with no reminder pending
  (a snooze is interest, not dismissal).
- The dwell sample is mail opened at or after the dwell ledger started
  (`dwell_ledger_since`, stamped on the open that added the column) for which a
  client reported any dwell or click. An open with no report behind it, from a
  client that predates the attention route, is not counted as a zero-second look.
  The sample is the newest 25 such opens per sender.
- Dwell is judged against length. A message's read fraction is its dwell over the
  time its words take at 238 words per minute, capped at 1; words are counted in
  `messages.body`, the flattened text triage reads. Mail under 40 words has no
  read fraction.
- `score` is 0 to 100. It is `null`, with the reason `not_enough_history`, under
  4 received messages or under 14 covered days of ledger: on a young ledger
  everything is unopened and there is no pattern to read. A sender the owner
  has written to, one governed by a `surface` rule, or one kept recently is
  always 0, and that answer comes first. The scoring
  and the reason codes (`not_enough_history`, `known_contact`, `wanted_by_rule`,
  `kept_recently`, `rarely_opened`, `cleared_unopened`, `skimmed`, `brief_looks`,
  `read_closely`, `never_clicked`, `high_volume`) live in
  `squelch-core/src/triage/sender_fatigue.rs`.
- Opens, dwell and clicks are recorded by Passband clients only. Mail read
  elsewhere looks ignored here, so the scores lean high. Smart clear (below)
  acts on the score: a sender at 75 or more has their unopened open Reading
  mail marked done. That is why 75 needs a second signal on top of "never
  opened", and why the sweep is undoable.

### Relationship ledger

`relationships` holds one row per address the owner has live mail with
(docs/UNDERSTANDING.md §2.1): inbound count and dates (off `received_at`),
the contacts row's sent count, threads the owner started, inbound threads and
how many the owner later wrote into, the median reply time (3 replied threads
or more), attention, corrections, and the fatigue score. Attention and the
fatigue score are one `SenderMeasurer` result, the purge flow's own, so the
two never disagree. It is not email text and it is not a floor.

A recount is bounded. `received_count` and the arrival dates are one
index-only walk of `idx_messages_from_received`, less the address's sealed
mail, which is counted from partial indexes over the account's sealed rows
(`idx_triage_sealed`, `idx_agent_decisions_sealed`,
`idx_agent_state_restricted`, `idx_agent_corrections_sealed`), so the count
follows the listings' rule without a probe per message. The arrival range may
end on a sealed message. Threads, replies, the name and the field corrections read only the
address's newest 500 inbound messages. A recount whose received, sent and
initiated counts have not changed skips those reads and refreshes only
attention, fatigue and the monthly rate, unless its mark is full
(`relationship_dirty.full`) or the nightly walk is running. On a synthetic
240,000-message store with one 40,000-message sender, a full recount takes
about 36 ms and an unchanged one about 33 ms, of which about 28 ms is the
purge flow's `SenderMeasurer`.

These add an address to `relationship_dirty`:

- ingest (the sender, or each recipient of Sent mail);
- Sent ingest also marks the senders of the thread's inbound mail, so a reply
  to a Reply-To address credits the From;
- not-spam;
- the Sent-history contacts harvest;
- the attention POST;
- a decision or access check that seals a message, and any owner correction.

Marks of the last kind, and the thread-sender marks, are full: the recount
skips its unchanged-counts shortcut. The daemon has no mark-as-spam or
local-delete path, so neither marks anything; the nightly walk catches
anything no mark saw. Every refine round, before the triage pass, recomputes
up to 200 dirty addresses, 25 per store lock. Once per owner-local day from
03:00 the whole table is recomputed in the same chunks; the day it last
completed is `sync_state` mailbox `relationships_daily`. A store that
predates the table is filled by the same walk on the first round, stamped
`relationships_backfill` only when the walk completes. The walk runs at most
8 chunks (200 addresses) per refine round and keeps its place in
`app_settings` key `relationships_walk_cursor`, so triage interleaves with
it and a restart resumes where it stopped. Each chunk finds its addresses by
range seeks on the senders, contacts, recipients and ledger indexes.

The triage context carries the row as `relationship` beside
`sender_is_contact`, and `read_sender_history` returns it as `relationship`
at the top of its result. The key is absent before the backfill stamp and
for an address with no row. `attention` is left out while `coverage_days`
(days since the later of the open ledger floor and the 90-day window, the
`SenderMeasurer`'s own coverage, so the same floor the fatigue score uses) is
under 14. The block is in no revision fingerprint, so a recount never makes
a run stale. Prompt `agent-triage-v21` reads it as evidence and restates the
65-point personal-mail floor over "a person the owner replies to": replied
above zero, initiated above zero, sent count above zero, or
`sender_is_contact`. The ledger can add a person to the floor and never
removes anyone `sender_is_contact` granted it. Eval
replays (`squelch-eval`) drop the block, because the ledger has no history
to replay as of a past moment.

Known limits:

- Archive-tier mail is not counted, so a correspondent whose history is all
  older than the backfill window starts with only the window's counts.
- The recent window uses `known_sealed_mail_sql!`, so mail whose access check
  is pending counts. The dirty set drains before the triage pass, so a fresh
  arrival is always pending when its mark drains. A decision or access check
  that seals a message, and any owner correction, marks the sender for a full
  recount, so a sealed message leaves the threads within a round. The
  attention columns use the purge flow's strict statements.
- `sent_count` and `sent_last_at` are the contacts row's, so a recipient the
  contacts seed filtered out has initiated threads but a zero sent count.

### Kept senders

A row in `reading_sender_keeps` records that the owner chose to keep a sender.
For 60 days the sender scores 0 with the single reason `kept_recently` and
smart clear never takes their mail. After 60 days the keep stops affecting
anything. If the owner later unsubscribes from the sender, or writes a squelch
rule that governs them, the newer decision wins and the keep is ignored.

### Who resolved a row: `triage.resolved_by`

`triage.resolved_by` is `NULL` when a person resolved the row, or when a path
older than the column did (the two cannot be told apart for old rows, and old
rows are not backfilled). Smart clear writes `smart_clear:<batch id>`.

Anything that reads `status = 'done'` as the owner deciding must also require
`resolved_by IS NULL`. The column does not do that for a reader by itself. Two
readers check it today: the fatigue score's `cleared_unopened` (smart clear reads
that score to choose senders and would otherwise raise it with each sweep), and
the rule suggester's sender digest, which reports a swept row as open so its
"cleared without opening" count is the owner's. The triage agent does not: its
context and its `user_state` fingerprint still see a swept row as a plain done.

Every statement that writes `triage.status` also clears `resolved_by`: the done
and reopen routes, a reminder being set or firing, the per-sender resolve, "not
spam", the cutover's reopening of audited auto-closures, and a re-ingest that
resolves the row. The per-sender resolve (run when the owner unsubscribes from
or mutes a sender) also clears it on that sender's rows that were ALREADY done
by a sweep, so undoing the sweep later does not reopen mail the owner has since
dealt with by sender. A person who reopens or re-does a row has taken it over, and
the batch's undo then leaves it alone.

### Smart clear

`POST /client/v2/reading/smart_clear` (no body) marks done, as one batch, the
open Reading mail that has expired or that the owner plainly does not want:

```
{ "batch_id": "17",
  "cleared": 43, "senders": 17,
  "by_reason": { "event_passed": 3, "offer_ended": 5, "expired": 4,
                 "ignored_sender": 20, "skimmed_sender": 6, "stale_promo": 11 },
  "message_ids": [812, 790],
  "rows": [ { "message_id": 812, "reason": "offer_ended",
              "sender": "deals@shop.example", "sender_name": "Shop Deals",
              "subject": "Last day: 20% off", "received_at": "2026-09-28T14:02:11+00:00" } ],
  "model": "ok", "model_detail": null }
```

`model` is `ok` or a `skipped_*` code (see the handler). With `skipped_failed`,
`model_detail` says why, from a closed set: `refused`, `timeout`, `discarded`
(the model answered and its answer failed the checks below) or `error`.

`batch_id` is `null` and nothing is recorded when nothing was cleared; `rows`
and `message_ids` are then empty.

`rows` is the receipt's full list: one entry per message this press wrote done,
the same messages in the same order as `message_ids`, each with why it was
taken (`reason` is one of the `by_reason` keys, and `by_reason` is the tally of
`rows`). `sender` is the address as stored, `sender_name` the display name or
an empty string. The subject, sender and date are read by primary key inside
the transaction that wrote the row, for exactly the rows it wrote. They are in
the response only: the batch's stored `detail` keeps ids, reasons and prior
status, and no mail text. The route is human door only, and sealed mail is
never cleared, so a sealed subject cannot appear here.

The candidates are the Reading feed's open items: the 30-day window and the
feed's default limit. Mail older than that window is never swept. That is
intended: the owner cannot see it in Reading to check the sweep against.

Never cleared, whatever else is true:

- mail from an address the owner has written to, a sender under a `surface`
  rule, a sender kept in the last 60 days;
- mail that is kept close, by the strict definition above (auth mail,
  restricted mail, pending access, legacy sealed rows);
- mail with a pending or fired reminder;
- mail whose thread is showing in FYE, by the FYE listing's own function (the
  decision's attention plus the owner's per-thread override);
- mail whose decision carries a bill, receipt, delivery or financial update:
  `done` is one status for every inventory, so clearing it from Reading would
  take it off Billing or Shipping too. An event record does not protect;
- a row an earlier smart clear took and the owner undid. An undo records the
  ids it gave back on the batch, and for 90 days no press takes them again;
- mail received in the last 24 hours.

Then the stored dates are read. Times are read in the four shapes the triage
agent writes: a timestamp with an offset, a local date and time with no offset
(`2026-10-10T19:00:00` or `2026-10-10T19:00`, which is most timed events, and
the same with a space in place of the `T`), and a bare date. With no timezone database, a local time or a bare date is taken to
be over at its last occurrence anywhere on Earth (12 hours behind UTC), which
can only make a row wait longer.

- Evidence dates are every event `start` and `ends`. One that was already over
  when the mail arrived is ignored (a wrong year, or a recap of something past).
- Holds are an unresolved action's `due` and `factors.attention_at` (each on the
  decision or on the thread's current attention), and the decision's
  `revisit.at`. They can keep a row and
  are never a reason to clear one.
- A date that is present and cannot be read ("next Friday evening"; event starts
  are not format-checked at triage) counts as still ahead. The row is left alone
  and the model is not asked about it.

If any of those is still ahead, and is within 90 days of the mail's arrival, the
mail is still live and nothing below takes it. A date further out than 90 days
("valid until 2099") does not keep a row alive.

Each cleared message has exactly one reason, first match:

1. `event_passed`: every event the stored decision records has passed.
2. `offer_ended`: the stored decision's `ends` has passed.
3. `ignored_sender`: the sender's fatigue score, computed by the same function
   and under the same 100-sender cap as the list above, is 75 or more, the
   owner never opened this message, and it was received at or after the open
   ledger began (before that, "never opened" cannot be known). On a ledger under 14 days old no sender has
   a score, so this takes nothing.
4. `skimmed_sender`: a promotion from a sender whose promotions the owner
   habitually glances at and closes (`sender_fatigue::habitually_skimmed`),
   judged over the sender's sampled opens (the 25 newest reported) cut to
   those whose decision is a Reading promotion carrying no record, so glances
   at the same sender's receipts do not count: at least 3 such opens, no link
   followed in any of them, and a median read fraction under 10% with a median
   look under 8 seconds (or, when too little of the mail is long enough to have
   a fraction, a median look under 3 seconds). This message was never opened, or was opened for under 3 seconds
   with no click; an open no client reported on is left alone. There is no
   ledger-age floor: every input is a look the owner took, so it is evidence
   from the first day. Known contacts, `surface` rules and kept senders never
   qualify. A sender whose mail is opened, or whose reported dwell or clicks
   change, while the model is being asked loses the mark for that press.
5. `stale_promo`: a promotion (the decision's `reading_kind`, else promotional
   and not editorial kinds), received 7 or more days ago and never opened in
   Passband. Mail from before the open ledger counts: a promotion a week old
   that is still in Reading has had its week, wherever it was read. Content is
   never stale.
6. Rows none of those took, whose decision has no usable date, are put to a
   model once. It answers `expired`, or `offer_ended` / `event_passed` when it
   can tell which.

A time has "passed" 24 hours after it. A date with no time has passed 24 hours
after that date has ended in every timezone (the daemon carries no timezone
database), so an event happening today is never cleared. An event has only a
start, so a multi-day event is treated as over a day after it begins.

The done is message-scoped, unlike the human done, which resolves the whole
thread. It stamps `resolved_at` and `resolved_by`. Like any done it changes the
agent's `user_state` fingerprint, so a triage pass in flight on one of those
threads commits `Stale` and retries with its usual backoff.

The model pass (`[smart_clear]`, default `claude-sonnet-5` at low effort):

- It is sent, per row, a row number, the day the mail arrived, and the summary
  triage already wrote, inside the untrusted fence, plus the current time. The
  subject, body, sender address, display name and message id are not fields of
  the prompt. A summary is prose about the mail and often names the sender, so
  that is a statement about fields, not about every word. A row with no
  summary, and restricted or auth mail, is not asked about.
- Its yes is checked. An `expired` answer counts only for a row whose summary,
  as the model was shown it (the first 220 characters), says when: a day word
  (a month or weekday name, today, tomorrow, tonight, day, days, hours, week,
  weekend, EOD, a named holiday, month names in Spanish, French and German) or
  a number written as a date or time (`10/15`, `2026-10-15`, `15th`, `5pm`,
  `17:00`, `May 5`, `Sat 12`). A bare number ("20% off", "Top 10") is not a date.
- The whole pass is discarded, and reported as `skipped_failed`, when the model
  says yes to summaries that say nothing about time: 3 or more such yeses, or
  20% of the yeses once there are at least 5. A model following the prompt
  never does that. A backstop also discards a pass that calls 80% or more of 10
  or more rows expired. Half a batch really having expired is ordinary (the
  week after a sale), so the share alone is not treated as a sign of trouble.
- That check is a floor, not protection. In one real mailbox 46% of Reading
  summaries passed the gate before its word list was widened, and more pass
  now, so a pass steered to say yes only to those rows can still take a good
  share of what was asked. What bounds it is that a press is one batch of at
  most 60 model-judged rows, undone exactly by one call, and that an undo is
  remembered.
- A verdict is applied only if the row's stored summary is still the one that
  was sent (compared by fingerprint under the write's lock), so a message
  re-triaged during the call is left alone.
- At most 60 rows per press, newest first, one attempt, 20 seconds.
- At most 20 passes per account per day.
- `model` reports it: `ok`, or `skipped_nothing_to_ask`, `skipped_unconfigured`
  (no LLM key), `skipped_cap`, `skipped_failed` (refusal, timeout, error). Every
  `skipped_*` press still clears on stored facts.
- Usage is billed to the `smart_clear` ledger category, which both cost
  estimators price from `[smart_clear]`.

The store mutex is not held during the model call. Rows are read under the
lock, the model is asked with it released, and the write re-reads and re-judges
every row in one transaction, so a row the owner opened, reminded, resolved or
kept in the meantime is judged as it now is. The sender scores measured by the
first read are reused by the write instead of being measured twice; who the
owner said to leave alone, and each row's own state, are always read again. A
sender any of whose mail was opened after the first read is treated as unscored
for that press, since an open is what lowers a score.

One press at a time per account: a press that arrives while another is running
answers `429` without touching the store.

One audit row per batch: `reading.smart_clear`, target the batch id, detail the
count.

`POST /client/v2/reading/smart_clear/{batch_id}/undo` answers
`{"reopened": n}`. It walks the batch's recorded list by primary key and
reopens the rows that are still done and still stamped with that batch, each to
the status it had (`new` or `open`), clearing their `resolved_at` and
`resolved_by`. Rows a person has touched since are left as
they are. The ids it reopened are recorded on the batch, and smart clear does not
take those rows again. A second undo answers `0`. An unknown batch, or another
account's, is `404`. A batch whose record cannot be read answers `500` and is
left as it was, not marked undone, so it can still be undone once repaired.
Audited as `reading.smart_clear_undo`.

Known limits:

- The one-press-at-a-time guard is released when the request ends. A client that
  disconnects while the first locked read is still running frees it before that
  read finishes.
- The stale-score check reads a sender's newest open over their whole history
  through the sender index, not just the 90-day window.
- The per-sender resolve (unsubscribe, mute) matches the sender with
  `LOWER(TRIM(from_addr))`, which the sender index cannot serve.
- An event record has only a start, so a multi-day event is over a day after it
  begins.

The triage decision carries an optional `ends` for Reading mail (prompt version
`agent-triage-v14`): when the offer, sale or event the message is about ends,
only when the message states it. It is validated with the same parser smart clear
reads it with, so a local time with no offset is accepted. Decisions stored before it read as `null`. The
prompt version is a label in each run's metadata; changing it re-triages
nothing.

Record cards read canonical Receipt, Banking, Calendar and Delivery decisions;
marketing cards read promotional Reading decisions. Model date-only deadlines
retain their calendar date and expire at the end of that day in the user's local
timezone. Feed recency clamps future sender dates to trusted local receipt state.
The shredder and Auth pending counts use the current actionable-auth assessment,
not retired `sealed` labels; informational login alerts remain readable.

The full agent can request a notification after a fast decline. Both paths share
one unique arrival-event ledger; opening, completing, or snoozing the message
suppresses a later arrival request. Transport delivery remains at least once:
logical deduplication does not promise exactly-once APNs presentation. The Auth
lookup reads model-assessed actionable auth, but never opens an automatic modal
or emits its own notification. The ordered FYE list has no separate auth insert.

The feedback UX is tracked separately in
[issue #213](https://github.com/passband-app/passband/issues/213).
Live-mail quality and latency evaluation is still needed before judging the
starting model/budget/ranking defaults. Unit and mocked-provider tests verify the
contracts, not the accuracy of a production model on a user's inbox.

## Upgrade order

1. Back up the database consistently, including its WAL, and record deployed versions.
2. Deploy the daemon first. Verify authenticated `GET /client/v2/capabilities`
   advertises `triage_version: 2`, `server_ranked_fye`, `reading`, and
   `pending_message_read`, then check pending-job and provider-error telemetry.
3. Ship the matching desktop and mobile clients. A new client against an older
   daemon displays an explicit daemon-upgrade requirement; there is no legacy
   placement fallback. Temporary network/server failures retry automatically.
   Account switches probe before changing active state. Notification taps wait
   for an authenticated connection; failed switches park until a meaningful new
   connection or explicit tap, without an immediate drain loop.
4. Verify a fresh message progresses through notification assessment and triage,
   its push opens the exact message, and actionable auth appears in the shredder.

Rollback requires a compatible daemon/client set and a database recovery plan
that accounts for mail and human actions received after the backup. This worktree
has not been deployed.

## Validation

See [TRIAGE-AUDIT.md](TRIAGE-AUDIT.md) for audit regressions and verification.
Tests use mocked providers. Live-mail accuracy and latency still need evaluation;
a green suite does not establish model quality. Binary/PDF evidence extraction,
provider-specific conversation-prefix caching, and removal of the remaining
legacy compatibility modules are separate follow-ups.

Records is an umbrella for Calendar, Shipping, Billing, and Receipts, not an
independent placement. Only Reading is selectable in `destinations`; record
membership is derived from typed `records` facts. The aggregate Records API
remains available, but never includes a message solely because of a placement
label. On upgrade, legacy Records placements and corrections are removed while
preserving typed facts and other choices. Messages with a legacy Records label
but no typed facts are queued once for re-triage (`records_cleanup`).

## The Reading board

The default Reading feed (`/client/v2/feed?destination=reading` with no `since`
and no `all_time=true`) is the BOARD the owner sees. It is the 30-day Reading
inventory with one part cut down: the mail the first-run backfill brought in
(`triage.first_load`, stamped at ingest for backfill-origin mail only). The two
parts are merged newest first.

- **Mail that is not first-load** (everything that arrives after onboarding,
  and all mail on an account onboarded before first-load stamping existed):
  the plain 30-day inventory. No week window, no cap. Every new Reading email
  shows up.
- **First-load mail**: the newest 49 first-load Reading messages of the 7
  calendar days, in the owner's zone, ending on the day of the NEWEST
  first-load message (any destination), COUNTING MAIL ALREADY DONE, less the
  done ones. That newest message is the leading edge of the first run, so the
  onboarding board is "the week before you joined, at most 49". It is fixed:
  it does not slide with `now`, live mail does not move it, and its mail then
  ages off with the ordinary 30-day rule. Because done mail keeps its place,
  clearing never refills it with older backlog. A heavy day is cut by message,
  never as a whole day, so a flood cannot empty it.

`READING_BOARD_DAYS` and `READING_BOARD_MAX` in `store/sqlite/agent_triage.rs`.

First-load mail off the board is not hidden or changed in the store: it stays
open in All mail and search. Smart clear and the per-sender purge work on the
whole 30-day inventory, so they also see the first-load backlog the board
leaves off, and smart clear's count can be larger than what leaves the board.
A caller naming its own dates, and the agent door's `get_reading`, get the
inventory.
