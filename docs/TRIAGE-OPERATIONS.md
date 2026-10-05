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
timeout_secs = 90
concurrency = 2
worker_poll_secs = 1
daily_run_cap = 1000
background_daily_run_cap = 200
max_attempts = 6
outage_retry_secs = 300

[triage.context]
initial_thread_messages = 8
max_related_messages = 20
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

[smart_done]
model = "claude-sonnet-5"
effort = "low"
price_in_per_mtok = 2.0
price_out_per_mtok = 10.0
```

`[smart_done]` is the model behind smart done's one question (see "Smart done"
below). It resolves its key, provider and URL from `[stage2]` like every other
model here and is prefixed for a gateway at call time. The prices are the
configured model's own and are what both cost estimators charge the
`smart_done` ledger category; move the model and move them with it. Overrides:
`SQUELCH_SMART_DONE_MODEL`, `SQUELCH_SMART_DONE_EFFORT` (`none` clears it),
`SQUELCH_SMART_DONE_PRICE_IN_PER_MTOK`, `SQUELCH_SMART_DONE_PRICE_OUT_PER_MTOK`.

Environment overrides: `SQUELCH_TRIAGE_MODEL`, `SQUELCH_TRIAGE_REVIEW_MODEL`,
`SQUELCH_TRIAGE_MAX_TURNS`, `SQUELCH_TRIAGE_MAX_TOOL_CALLS`,
`SQUELCH_TRIAGE_TIMEOUT_SECS`, `SQUELCH_TRIAGE_CONCURRENCY`,
`SQUELCH_TRIAGE_DAILY_RUN_CAP`, `SQUELCH_TRIAGE_BACKGROUND_DAILY_RUN_CAP`,
`SQUELCH_TRIAGE_MAX_ATTEMPTS`, and
`SQUELCH_TRIAGE_OUTAGE_RETRY_SECS`. Other agent levers are configured in TOML. Scores order only agent-selected FYE threads;
no weight or importance threshold can add a thread to FYE. Recency uses relevant
message activity, never the time a background job ran.

### Spend and revisit limits

The budget unit remains one bounded model job, not a token or dollar cap. Full
triage permits up to `max_model_turns` calls; an access-only job permits one call.
The account total is `triage.agent.daily_run_cap` (default 1000). Background work
also consumes `background_daily_run_cap` (default 200), within that total. Thus
background cannot consume the last 800 runs reserved for new inbound mail.
Unused arrival capacity is not loaned to background work. Setting the background
cap to zero pauses background jobs. If only the total is lowered, the effective
background cap is clamped to at most total minus one, retaining a reserve.

Migration/backfill, sent-mail access, external-demand access, thread refreshes,
and autonomous revisits are background work. First classification from incremental
sync uses arrival capacity independently of the sender's Date header and push
eligibility. The sync origin is persisted explicitly; an initial mailbox import
cannot pretend to be new arrivals merely because its messages are newly stored.

Legacy stage-1/stage-2 escalation caps, including per-thread/per-sender overrides,
no longer gate agent triage. Their compatibility fields remain exposed but the
agent config explicitly reports `legacy_stage_caps_are_active_ceilings: false`.
Configure agent ceilings through the TOML/environment controls above; moving the
usage-page and warden controls to a dollar budget is [follow-up #216](https://github.com/passband-app/passband/issues/216).
`/client/triage-config` reports the configured/effective background cap, total cap,
and protected arrival capacity. When the true total is exhausted, mail stays
human-readable and visibly pending until the next UTC day.

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
Timestamp changes do not evade lifetime limits. The retired deterministic
`deadline_grace_hours` and `fye_stale_days` sweeps do not run in agent triage.

## Recovery and diagnostics

Jobs have leases, attempt counts, availability times, and bounded error codes.
Invalid output and stale commits retry with exponential delay, then remain failed
for inspection and manual re-triage. Observable output constraints get repair
turns within the same investigation before consuming a new attempt. Provider
configuration failures, transport failures, rate limits, and server errors
instead leave work pending and retry after the shared outage cooldown;
they do not consume the terminal-attempt allowance. After the cooldown, one
investigation probes recovery; a successful response restores normal admission,
and another outage extends the cooldown. Empty refine polling backs off up to
30 seconds and an ingest wake resets the delay. A first-call configuration
rejection refunds its budget reservation. Earlier paid calls remain charged.
Whole-investigation and access-call deadlines are job-local failures: they consume
retry attempts without pausing the account, and their uncertain spend is not
refunded. Daily budget exhaustion defers work to the next UTC day. Investigations for the
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

### Reading attention and the per-sender purge read

`POST /client/v2/messages/{id}/attention` takes `{"dwell_ms": n, "link_clicks": n}`
(either may be omitted) and ADDS the report to that message's totals in
`triage.dwell_ms` and `triage.link_clicks`. One report adds at most 10 minutes and
20 clicks; a message holds at most 1 hour and 1,000 clicks. It answers `204` for
every well-formed report, including one about a message id the daemon does not
know or a sealed message, and writes nothing for those two. That differs from
`/opened`, which answers `404` for an unknown id: clients treat a `404` from the
attention route as "this daemon predates the route" and stop reporting for the
session. The route writes no audit row (it fires several times per message read)
and changes nothing in the agent's `user_state` fingerprint, so a report cannot
make an in-flight triage commit stale. Human door only.

What is stored is two counts per message. No URL, link host, scroll position or
timestamp of a report is kept, and only the owner's own client can write them.

`GET /client/v2/reading/senders` lists each sender that has open mail in Reading
(the same inventory `/client/v2/feed?destination=reading` returns, with the same
`limit`), at most 100 senders, most purge-worthy first:

```
{ "window_days": 90, "ledger_days": 37, "total_senders": 12,
  "senders": [ {
    "sender": "news@example.com", "display_name": "Example News",
    "open_count": 3, "open_message_ids": [812, 790, 771],
    "signals": { "received": 23, "opened": 1, "cleared_unopened": 4,
                 "dwell_sample": 1, "median_dwell_ms": 4000, "clicks": 0,
                 "read_sample": 1, "median_read_fraction": 0.02,
                 "median_words": 900, "per_month": 18.6 },
    "known_contact": false,
    "rule": null,
    "unsubscribe": { "message_id": 812, "auto": true },
    "unsubscribed": null,
    "score": 71,
    "reasons": [ { "code": "rarely_opened", "opened": 1, "received": 23 } ]
  } ] }
```

- Counts cover the sender's inbound, non-spam, non-sealed mail that the owner did
  not write, received since the later of 90 days ago and the start of the open
  ledger (`app_settings.opened_ledger_since`, else the account's `created_at`).
  `ledger_days` is how many days that actually is, and `per_month` scales by it.
- The dwell sample is mail opened at or after the dwell ledger started
  (`dwell_ledger_since`, stamped on the open that added the column) for which a
  client reported any dwell or click. An open with no report behind it, from a
  client that predates the attention route, is not counted as a zero-second look.
  The sample is the newest 25 such opens per sender.
- Dwell is judged against length. A message's read fraction is its dwell over the
  time its words take at 238 words per minute, capped at 1; words are counted in
  `messages.body`, the flattened text triage reads. Mail under 40 words has no
  read fraction.
- `score` is 0 to 100, or `null` under 4 received messages. A sender the owner
  has written to, or one governed by a `surface` rule, is always 0. The scoring
  and the reason codes (`not_enough_history`, `known_contact`, `wanted_by_rule`,
  `kept_recently`, `rarely_opened`, `cleared_unopened`, `skimmed`, `brief_looks`,
  `read_closely`, `never_clicked`, `high_volume`) live in
  `squelch-core/src/triage/sender_fatigue.rs`.
  Reasons carry numbers only; the client writes the sentence.
- `unsubscribe` names the newest of the sender's last three messages that has a
  usable unsubscribe link, found by the same classification
  `POST /client/unsubscribe` uses. `auto` is true only when that route's automatic
  path would run for it. Nothing is fetched to decide either.
- `unsubscribed` is the sender's row in the unsubscribe ledger; `status` is
  `browser` for a row with no job status.
- Opens, dwell and clicks are recorded by Passband clients only. Mail read
  elsewhere looks ignored here, so the scores lean high; the list suggests and
  never acts.

- `kept_at` is when the owner last chose to keep the sender (below), or `null`.

The read writes nothing: it does not stamp `surfaced_at` or `opened_at`.

### Keeping a sender

`POST /client/v2/reading/senders/keep` with `{"sender": "<address>"}` answers
`204` and records, in `reading_sender_keeps`, that the owner looked at this
sender in the purge flow and chose to stay subscribed. For 60 days the sender
scores 0 with the single reason `kept_recently` and smart done never takes
their mail. After 60 days the keep stops affecting anything; `kept_at` is still
returned. A second keep moves the date. The write is audited as
`reading.keep_sender` with the address as its target.

A keep is never deleted. If the owner later unsubscribes from the sender, or
writes a squelch rule that governs them, the newer decision wins when the list
is read and the keep is ignored; keeping again after that makes the keep the
newer decision.

### Who resolved a row: `triage.resolved_by`

`triage.resolved_by` is `NULL` when a person resolved the row, or when a path
older than the column did (the two cannot be told apart for old rows, and old
rows are not backfilled). Smart done writes `smart_done:<batch id>`.

Anything that reads `status = 'done'` as the owner deciding must also require
`resolved_by IS NULL`. The fatigue score does: `cleared_unopened` counts only
rows a person cleared, because smart done reads that score to choose senders
and would otherwise raise it with each sweep.

Every statement that writes `triage.status` also clears `resolved_by`: the done
and reopen routes, a reminder being set or firing, the per-sender resolve, "not
spam", the cutover's reopening of audited auto-closures, and a re-ingest that
resolves the row. A person who reopens or re-does a row has taken it over, and
the batch's undo then leaves it alone.

### Smart done

`POST /client/v2/reading/smart_done` (no body) marks done, as one batch, the
open Reading mail that has expired or that the owner plainly does not want:

```
{ "batch_id": "17",
  "cleared": 43, "senders": 17,
  "by_reason": { "event_passed": 3, "offer_ended": 5, "expired": 4,
                 "ignored_sender": 20, "stale_promo": 11 },
  "message_ids": [812, 790],
  "model": "ok" }
```

`batch_id` is `null` and nothing is recorded when nothing was cleared.

The candidates are the Reading feed's open items (30-day window, the feed's
default limit). Each cleared message has exactly one reason:

1. `event_passed`: the stored decision records an event whose `start` has passed.
2. `offer_ended`: the stored decision's `ends` has passed.
3. `ignored_sender`: the sender's fatigue score, computed by the same function
   and under the same 100-sender cap as the list above, is 75 or more.
4. `stale_promo`: a promotion (the decision's `reading_kind`, else promotional
   and not editorial kinds), received 14 or more days ago, never opened, whose
   stored dates are not still ahead. Content is never stale.
5. Rows none of those took, whose decision has no `start` or `ends`, are put
   to a model once. It answers `expired`, or `offer_ended` / `event_passed`
   when it can tell which.

A time has "passed" 24 hours after it. A date with no time has passed 24 hours
after that date has ended in every timezone (the daemon carries no timezone
database), so an event happening today is never cleared.

Never cleared, whatever the reason: mail from an address the owner has written
to, a sender under a `surface` rule, a sender kept in the last 60 days, sealed
mail, mail with a pending or fired reminder, and mail received in the last 24
hours.

The done is message-scoped, unlike the human done, which resolves the whole
thread. It stamps `resolved_at` and `resolved_by`. Like any done it changes the
agent's `user_state` fingerprint, so a triage pass in flight on one of those
threads commits `Stale` and retries with its usual backoff.

The model pass (`[smart_done]`, default `claude-sonnet-5` at low effort):

- It sees, per row, a row number, the day the mail arrived, and the summary
  triage already wrote, inside the untrusted fence, plus the current time. No
  subject, body, address or message id. A row with no summary, and restricted
  or auth mail, is not asked about.
- At most 60 rows per press, newest first, one attempt, 20 seconds.
- At most 20 passes per account per day.
- `model` reports it: `ok`, or `skipped_nothing_to_ask`, `skipped_unconfigured`
  (no LLM key), `skipped_cap`, `skipped_failed` (refusal, timeout, error). Every
  `skipped_*` press still clears on stored facts.
- Usage is billed to the `smart_done` ledger category, which both cost
  estimators price from `[smart_done]`.

The store mutex is not held during the model call. Rows are read under the
lock, the model is asked with it released, and the write re-reads and re-judges
every row in one transaction, so a row the owner opened, reminded, resolved or
kept in the meantime is judged as it now is.

One audit row per batch: `reading.smart_done`, target the batch id, detail the
count.

`POST /client/v2/reading/smart_done/{batch_id}/undo` answers
`{"reopened": n}`. It reopens the rows that are still done and still stamped
with that batch, each to the status it had (`new` or `open`), and clears their
`resolved_at` and `resolved_by`. Rows a person has touched since are left as
they are. A second undo answers `0`. An unknown batch, or another account's, is
`404`. Audited as `reading.smart_done_undo`.

The triage decision carries an optional `ends` for Reading mail (prompt version
`agent-triage-v6`): when the offer, sale or event the message is about ends,
only when the message states it. Decisions stored before it read as `null`. The
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
