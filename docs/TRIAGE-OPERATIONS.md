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
```

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
a horizon past ten years) are clamped rather than trusted. The retired
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
jitter riding under that ceiling: the ceiling is the longest wait), otherwise
15 to 30 seconds; every probe that fails in turn doubles that wait, up to
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
consumes retry attempts in the usual way. Their uncertain spend is not refunded. A run dropped at
shutdown hands its job back at once, due after the retry ladder's wait for the
attempt it took and keeping that attempt (a job dropped on its last allowed
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
and tool-call counts, and source IDs. Reported usage is retained for refusals,
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
