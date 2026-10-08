# Performance: findings and plan

A read-only audit of the whole tree at `29c4680` (2026-10-08): the daemon
(store, sync, triage, HTTP doors, runtime), the Mac and iPhone client, the
hosted stack (warden, control, relay), and the build. Eight parallel passes,
one per area, then every top finding re-read by hand. The store pass also
loaded the real schema into a 50k-message synthetic database and ran `EXPLAIN
QUERY PLAN` and timings, so the store numbers below are measured, not guessed
(absolute milliseconds are relative to that box; the ratios are what matter).

Nothing here is implemented. Every finding names its file and line, what the
code does now, what it costs, the fix, impact (High / Med / Low), effort
(S / M / L), and whether the claim is **[verified]** by reading or measurement
or **[plausible]** (mechanism verified, size estimated). Section 6 lists what
is already right so nobody re-proposes it, and section 7 lists the changes
that look obvious and are wrong.

## 1. The shape of it

**The daemon's cost is idle churn, not work.** One hosted tenant with nobody
connected makes 48 Gmail HTTPS requests a minute, wakes a timer ~26 times a
minute, opens two write transactions a second that find nothing, and fsyncs
the WAL roughly every five seconds for a cursor that did not move. None of
that is mail. A box of 100 tenants spends a vCPU on TLS and JSON to learn
nothing. The single-mutex SQLite store makes every one of those no-ops a
serialization point for the human door, and under the hosted CPU quota tokio
runs on **one worker thread**, so a slow query is a stalled daemon.

**The store has never been told what its data looks like.** No `ANALYZE` has
ever run, so the planner guesses `account_id = ?` is selective on a
single-account file and walks whole tables; the code already pins eight
queries with `INDEXED BY` to work around it. One `PRAGMA optimize` turns a
457 ms access check into 0.2 ms. The stats endpoint the phone polls every ten
seconds is a full-account five-way join that costs about a second at 50k
messages, under the mutex.

**LLM spend is mostly discretionary.** The onboarding backfill runs 1,000
background jobs at `xhigh` effort because effort is chosen per account rather
than per job: roughly $30-40 per new tenant for work nobody watches land. A
single 90 s wall clock across all agent turns, retried six times with each
attempt billed, and a 5-minute account-wide cooldown on one `429` are the
other two.

**The client is already well built; what is left is launch and churn.** It
has `@Observable` with compare-before-assign, off-main decoding, memoized
formatters, a live WebKit pool. What it lacks is a persisted read model, so
every launch is a spinner until a serial capabilities probe and a feed round
trip return, and a poll that keeps running at full rate while the window is
hidden or closed, re-downloading dozens of thread bodies a minute.

**Hosted capacity is gated by a number that is no longer true.** Each tenant
reserves 384Mi because that was the resident ONNX session; the daemon now
unloads that session when idle. Re-deriving the request from a week of real
working-set data is a config change worth roughly 2x tenants per box.

## 2. The plan

Ordered by payoff per hour. Each tranche is separately mergeable; A is the
one to do first and needs no design. The numbers in `[ ]` point at section 5.

### Tranche A: small, verified, no design needed

Daemon store (one PR):

| # | Change | Impact | Effort |
|---|---|---|---|
| A1 | `PRAGMA analysis_limit=400; PRAGMA optimize` after `migrate()` and on the retention pass [S1] | High | S |
| A2 | `synchronous=NORMAL`, `cache_size=-32000`, `temp_store=MEMORY`, `journal_size_limit=64MB` [S6] | Med-High | S |
| A3 | Index `triage(account_id, reminded_at) WHERE reminded_at IS NOT NULL` (233 ms to 0 on every Standing read); `send_trackers(account_id, message_id)`; `agent_triage_runs(account_id, message_id)` [S4, S10] | High | S |
| A4 | Wrap the read-path `ensure_source_assessments` enqueues and `upsert_message_vector` in one transaction each [S6] | Med | S |

Daemon idle churn (one PR):

| # | Change | Impact | Effort |
|---|---|---|---|
| A5 | Notification lane: wake on a `Notify` from the two producers, idle backoff 1 to 30 s (copy the refine lane), read-only `EXISTS` before the `BEGIN IMMEDIATE` claim [Y2] | High | S |
| A6 | Vector drain only when a `vectors_dirty` flag is set (embed error, embedder attach), not every round [Y3] | Med-High | S |
| A7 | Poll lane: skip the cursor write when unchanged; write `inbox_unread` only when counts change; one unfiltered `history.list` split by label client-side; `labels.get` only after a walk that moved; adaptive interval 5 s to 60 s idle, reset on the existing refresh `Notify` or an SSE receiver [Y1, H2] | High | M |
| A8 | Cache the `StoredToken` in memory until near expiry; bypass on the 401 retry [Y4] | Med | S |
| A9 | Scheduled-send loop sleeps until `MIN(send_at)` (floor 10 s) and wakes on a `Notify` from the POST [H9] | Low-Med | S |

Daemon runtime and doors (one PR):

| # | Change | Impact | Effort |
|---|---|---|---|
| A10 | `worker_threads(2..4)` and `max_blocking_threads(32)` explicitly; log `num_workers()` at boot [H1] | High | S |
| A11 | One `GmailWriteClient` built in `ApiState::with_write_credentials`, cloned per action [H6] | Med | S |
| A12 | `CompressionLayer` (gzip + br, `Fastest`) on the client and console routers [H7] | Med | S |
| A13 | `TimeoutLayer` ~20 s and a global concurrency limit on `/client/*`; give hyper a timer so its header-read timeout is live [H11] | Low-Med | S |

Triage spend (one PR):

| # | Change | Impact | Effort |
|---|---|---|---|
| A14 | Effort per job: `xhigh` only for foreground arrival and manual jobs inside the onboarding window; migration, revisit and access at `stage2.effort` [T1] | High | S |
| A15 | Per-turn deadline (or `xhigh` >= 180 s); cap `agent_timeout` retries at 1-2 or retry at `low`; `429`/`529` cooldown = `retry-after` or 15-30 s with jitter, keep 300 s for config failures [T3, T9] | Med-High | S |
| A16 | Cap the target body at ~12-16k chars in `condense_initial_context`, setting `body_truncated`/`next_offset` so the pager works [T2] | Med-High | S |
| A17 | Omit the `request_review` step from the schema and prompt when `review_model` is `None` [T10] | Low-Med | S |
| A18 | Access lane: `condense_body` + ~8k-char cap; its own daily budget key [T11] | Low-Med | S |

Hosted (config, one PR plus one `kubectl`):

| # | Change | Impact | Effort |
|---|---|---|---|
| A19 | `count/secrets` >= 4 x `pods` + slack; commit the live quota into `70-tenant-limits.yaml` so `kubectl apply` does not reset admission to five tenants [O4, O5] | Med | S |
| A20 | Relay opens store: `journal_mode=WAL`, `synchronous=NORMAL`; purge and trim on a background interval, not in the pixel handler [O3] | Med (High under a campaign) | S |

Build (one PR):

| # | Change | Impact | Effort |
|---|---|---|---|
| A21 | Run the CI disk purge in the background; give the squelchd image job its own GHA cache `scope=`; `FROM rust:1.94-slim` (or `COPY rust-toolchain.toml`) in every Dockerfile [B2a, B15, B3] | Med | S |
| A22 | `keyring = { default-features = false }`, Apple store under `cfg(target_os = "macos")` [B4] | Med (build time) | S |
| A23 | `git rm -r squelch-desktop` [B8] | Low | S |

Client (two PRs, Mac and iOS):

| # | Change | Impact | Effort |
|---|---|---|---|
| A24 | Pause or stretch the 10 s poll when the app is inactive, the window occluded or closed; let the resident SSE stream be the wake [C2] | High | S-M |
| A25 | Warm paths skip any cached thread regardless of age; iOS warms visible rows only with `boardFresh` [C3] | High | S |
| A26 | Launch image/hero warm: visible rows now, the rest deferred to `.background` after idle, skipped on low power / expensive path [C4] | Med-High | S |
| A27 | iOS WebKit pool caps (3/1/1) and a drain on memory warning and `.background` [V1] | Med-High (iOS) | S |
| A28 | Narrow `accountEmail` / `spamCount` derived properties so the reader and inbox stop subscribing to the whole `stats` struct [V2] | Med | S |
| A29 | `headlineKey` computed once in `performPull`; `isReadingMail` as a stored `Set`; `MailRow` subject/gist as `let`s; `minimumInterval` on the hold ring; one `TimelineView` per list [C8, V3, V7, V9, V10] | Low-Med | S |

### Tranche B: structural daemon work

| # | Change | Impact | Effort |
|---|---|---|---|
| B1 | Store calls off the runtime thread: `ingest_one` (parse + sanitize + transaction), the triage worker's evidence reads, and the MCP tool bodies behind `spawn_blocking`, the way `handlers::blocking` already does [H1, Y6, T4] | High | M |
| B2 | A store write-generation counter; memoize `/client/v2/feed`, `/client/stats` and the `/metrics` snapshot per generation; `ETag`/`If-None-Match` + 304 on feed, stats and `/client/updates` before touching the store; client sends the header and skips decode on 304 [H3, H4, C7] | High | M |
| B3 | `stats()`: incremental tier/band counters or a per-account cache invalidated by writes; materialize `is_auth` / `show_in_fye` as columns written at commit so no JSON function runs per row; same for `sealed_messages` [S3, S9] | High | M |
| B4 | `queue_dependent_refreshes` recurses through the existing reverse indexes instead of materializing every edge (329 ms to ~0 at 50k, inside the ingest transaction today) [S5] | High | M |
| B5 | Compute the external-access allowed set once per agent-door call (the `AccessCache` pattern), and fold the per-item access checks in `get_inbox_updates` into SQL [S2, H8] | High | M |
| B6 | Migration done-flags: a `schema_meta` version row; each one-shot heal checks and stamps it; schema.sql tail repairs behind the same gate [S7] | Med | S |
| B7 | A daily retention pass in the shredder loop: completed jobs and runs > 30 d, opens > 180 d, events > 90 d and below the lowest device cursor, audit > 1 y; `PRAGMA optimize` and an FTS `'merge'` in the same pass [S11, S12] | Med | S |
| B8 | FTS: skip the rewrite when subject and body are unchanged; external-content table (`content='messages'`) to halve disk [S12, Y8] | Med | M |
| B9 | Catch-up diffs listed ids against the store in chunks and fetches only unknown ones (today it re-fetches and rewrites the whole 30-day window) [Y5] | Med-High | M |
| B10 | Sent-contacts harvest bounded (`newer_than:2y`), checkpointed like `heal_blank_bodies`, run through the fetch window [Y7] | Med | M |
| B11 | Agent prompt: split the user content into a stable `context` block with `cache_control` and a growing `investigation` block; make `notification.*` and `external_access.reason` nullable with word caps on `summary`/`reason` [T5, T6] | Med | S (needs the replay eval for T6) |
| B12 | `context()` loads `initial_thread_messages + 1`, derives the revision from the stored per-message content hashes instead of re-hashing bodies, and the notify lane gets a one-message loader [T4] | Med | M |
| B13 | `agent_deliveries::reconcile` reads a projection (or filters with `json_each` in SQL) instead of decoding every decision in the account under the write lock [S8] | Med | M |
| B14 | `reqwest` `gzip` (+ `http2`) on the Google clients with a "gzip" User-Agent; a `static` ammonia `Builder`; a cap on stored `body_html` [Y9, Y10] | Low-Med | S |
| B15 | `prepare_cached` on the hot statements; raise the statement cache capacity past rusqlite's 16 [S14] | Low-Med | S |
| B16 | CI: one `tests/main.rs` per crate with `mod` declarations (17 links in squelch-api become 1), `client_api.rs` split into a module directory; then `cargo nextest` [B2b] | Med-High | M |
| B17 | squelchd image: cargo-chef dependency layer; platforms as a matrix (native arm64 runner) with manifest merge; `lld` for the aarch64 cross link [B1] | High (release time) | M |

### Tranche C: client launch

| # | Change | Impact | Effort |
|---|---|---|---|
| C1 | Persist `SitrepData` + `StoreStats` (+ zones, inbox page) per account as JSON in Application Support, written off-main and debounced from the compare-before-assign sites; load it in `loadSettings()` after the keychain read and mount `MainShell` at once in a "restoring" state; run the capabilities probe concurrently with the first pull [C1] | High | M |
| C2 | Records-feed derivations (receipts/banking/calendar) inside the task-group child, returned split; `zones` as per-zone stored properties [C6] | Med | S |
| C3 | Assistant: `cache_control` on the last tool definition / system block (the relay proxy must pass it through); cap `get_thread` text per message; prune history to a token budget [C5] | Med-High | S-M |
| C4 | Keep sitrep and emails mounted across rail navigation (or restore `ScrollPosition`); persist measured frame heights keyed by (message, width, style, density) and measure with remote images off [V8, V12] | Low-Med | M |
| C5 | Mac release build: `-Xlinker -dead_strip`, `strip -x` before codesign with a dSYM kept; audit the two variable fonts, four CAF sounds and three rehearsal JPGs [B9] | Low-Med | S |

### Tranche D: scale, gated on measurement

| # | Change | Impact | Effort |
|---|---|---|---|
| D1 | Confirm `squelchd_embedder_loaded` shows unload live on `daemon-0.0.12`; pull a week of `container_memory_working_set_bytes` p50/p99 per tenant; re-derive `MEMORY_REQUEST` (likely 160-224Mi) and `CPU_REQUEST` (50m); raise `requests.*`, `limits.memory`, `pods`, `pvc`, `count/secrets` together. Keep the 1Gi limit: a first backfill is still +520 MB [O1] | High (30 to ~55-70 tenants, same box) | S |
| D2 | Apply `kubelet.yaml` (`systemReserved`) and the zram device already written under `deploy/hosted/node/` [O5] | Med (safety net for D1) | S |
| D3 | Roller: after the first tenant of a run converges, allow K more (3-5) in the same run, or a 2-minute tick [O6] | Med at scale | S-M |
| D4 | Prometheus: `metric_relabel_configs` drop `..._duration_seconds_bucket` (keep sum/count) or cut to 6 buckets; raise the agent's memory past ~50 tenants [O10] | Low | S |
| D5 | Mount the models PVC read-only at the daemon's cache path instead of copying 126 MB into every tenant volume [O7] | Low-Med | M |
| D6 | Gmail `users.watch` + Pub/Sub for hosted tenants, with the A7 backoff as the fallback poll (`docs/HOSTED.md` "Scaling path" step 2) [Y1] | High at scale | L |
| D7 | `docs/EMBED-SERVICE.md` when its section 9 trigger fires; D1 moves that trigger out | High at scale | L |
| D8 | Model tiering behind the eval: `sonnet-5-5` agent + `opus-5-5` review via the existing `review_model` knob; `haiku-5-5` at `low` for the notify and access lanes. The eval the current choice cites is not in the repo; check it in first [T7, T15] | Med (unverified) | S to try, M with the eval |
| D9 | Skip the fast notification lane for one-click `List-Unsubscribe` mail with no `surface` rule and no login-code hit [T8a] | Med | S |

### Software quality, while in there

- Delete the retired classifier: `stage1_llm.rs`, `stage2.rs`, `router.rs`,
  `extract/**` (~6.5k lines, no live callers) and the `Stage1Config` /
  `Stage2Config` caps that gate nothing; move the live caps' documentation to
  `AgentConfig` [T15].
- Delete `attention_updates` / `ranked_updates` (a 413 ms `ROW_NUMBER` window
  query with no production caller) [S15].
- Drop the dead `_rules` / `_known_contact_lookup` parameters and the
  per-batch `list_sender_rules` query that feeds them; share one `SyncEngine`
  HTTP client between the two engines [Y13].
- Check the triage eval harness and `evals/results/*` into the repo, or point
  at where they live, so the effort and model choices can be re-run [T15].
- Check the 50k-row planner bench in as a test that asserts the hot plans
  (the `INDEXED BY` pins can then be removed one by one where the plan holds).

## 3. What one tenant costs today

Daemon alone, idle, hosted defaults, per minute:

| | |
|---|---|
| Timer wakeups | ~26 (poll 12, scheduled-send 6, refine >= 2, metrics 2, reaper 1, pusher 1, calendar 1, opens 1) |
| Gmail HTTPS requests | 48 (per 5 s tick: `labels/INBOX`, `history.list` x2-3) |
| fsync'd WAL commits | ~24 (the `inbox_unread` upsert and the history-cursor upsert, both unconditional; `synchronous=FULL`) |
| Write-lock transactions that find nothing | ~120 (notification lane, 2 per second) |

Add one Mac: +18 HTTP requests (feed 200 rows + stats + sealed every 10 s),
each a bearer lookup, six full ranked feeds and six full-mailbox `stats()`
passes, plus on the Emails page one 100-row `/client/updates` per tick and
~40 `GET /client/thread/{id}` a minute of prefetch churn.

Per triage run at `opus-5-5` / `low`: ~6.6k cached prefix tokens ($0.0013),
3-5k uncached context ($0.012-0.02), 1.5-2.5k output ($0.03-0.05), about
$0.05-0.07 per run at ~1.3 turns; `xhigh` about 1.6x.

Hosted box today (4 vCPU / 16 GB): hard cap 25 tenants by `count/secrets`,
30 by the request quota, ~37 by memory arithmetic at 384Mi.

## 4. Verification before any of it ships

- **Store**: `EXPLAIN QUERY PLAN` on `stats()`, `human_inventory` (Standing),
  `external_message_allowed_conn`, `queue_dependent_refreshes`,
  `append_due_reminders` before and after A1/A3/B4, against a real store
  copied from a tenant PVC. Wall time of `SqliteStore::open` cold on that file.
- **Idle churn**: `squelchd_gmail_*` request counters and `strace -c -e fsync`
  (or `/proc/<pid>/io` write syscalls) on one tenant pod over an hour before
  and after A5-A8. Expect ~36 Gmail requests/min to ~1.
- **Door latency**: the `squelchd_http_request_duration_seconds` p95 that is
  already scraped, per route, before and after A10 and B1-B3. The hosted p95
  of 1.1 s named in `metrics.rs` is the number to move.
- **Triage**: `SELECT count(*) FROM agent_triage_jobs WHERE
  last_error='agent_timeout'` and the `model_turns` distribution in
  `agent_triage_runs.metadata_json` on a few tenants, to size T3 and T5;
  the ledger's cached vs uncached input split per run before and after A16
  and B11.
- **Client**: an `os_signpost` interval from app launch to the first sitrep
  row written, cold and warm, before and after C1; Instruments "Hangs" on a
  mid-scroll poll before and after A28; a network trace of one idle minute on
  the Emails page before and after A24/A25.
- **Hosted**: a week of `container_memory_working_set_bytes` per tenant with
  `squelchd_embedder_loaded` alongside, before D1.

## 5. Findings in detail

Each area's findings keep the audit's own ranking. References are
`file:line` at `29c4680`.

### S. Store (`squelch-core/src/store`)

Context: one `Mutex<Connection>` (`sqlite/mod.rs:166,254-258`); pragmas are
`busy_timeout` and `journal_mode=WAL` only (`sqlite/mod.rs:216-219`).
`foreign_keys = ON` really is applied per connection, because
`execute_batch(SCHEMA)` runs on the connection and `schema.sql:4` is a bare
pragma. 131 `prepare` sites against 12 `prepare_cached`.

- **S1. No `ANALYZE` or `PRAGMA optimize` ever runs.** [verified, measured]
  `migrate.rs` never analyzes; `rules.rs:7-14`, `ghosts.rs:462` and
  `agent_triage.rs:1034,2596` say so in comments and eight queries carry
  `INDEXED BY` to compensate. Bench, before and after a 150 ms `ANALYZE`:
  `external_message_allowed_conn` (`messages.rs:240-271`) 84 ms to 0.1 ms at
  closure 3, 457 ms to 0.2 ms at closure ~60 (the recursive step was
  `SEARCH d (account_id=?)` then `SCAN p`, every source row per node);
  `agent_sender_history` 64 ms to 0.0; `mark_thread_opened` and
  `set_attention_status` (`attention.rs:475-486,589-600`) flipped from an
  `idx_triage_status (account_id=?)` walk to the primary key; stats tier CTE
  1005 ms to 442 ms. High / S.
- **S2. The external-access CTE runs once per message inside N+1 loops.**
  [verified] `messages.rs:275-297` per sibling; `restrict_external_items` three
  times per item plus per source and sibling; `search.rs:288-319` per hit and
  sibling. An MCP `get_inbox_updates` of 20 items with 10-message threads is
  ~250 CTE evaluations under the mutex. Fix: one CTE seeded with every
  candidate id returning `(message_id, allowed)`, memoized in the
  `AccessCache` pattern `specialists.rs:553-681` already uses. High / M.
- **S3. `stats()` is a full-account scan.** [verified, measured]
  `attention.rs:13-31` (`SELECT m.*`, `json_array_length`, `json_extract`,
  a correlated `accounts` subquery per row), no date window
  (`attention.rs:768-771`), band counts with two correlated `EXISTS` per row
  (`:833-851`). Tier counts 1005 ms (442 after ANALYZE), bands 740 ms, plan
  `USE TEMP B-TREE FOR GROUP BY`. Polled every 10 s by the client and 30 s by
  Prometheus. `sealed_messages` (`messages.rs:639-648`) is the same shape at
  74 ms. High / M.
- **S4. `append_due_reminders` scans the account on every FYE / Standing
  read.** [verified, measured] `agent_triage.rs:2315,2326` (`reminded_at IS
  NOT NULL AND remind_at IS NULL`), no index; plan walks
  `idx_messages_from_nocase (account_id=?)` with a triage PK probe per row,
  233 ms at 50k, unchanged by ANALYZE. A partial index of the same shape as
  `idx_triage_remind_at` (`migrate.rs:339`) takes it to 0.0 ms. High / S.
- **S5. `queue_dependent_refreshes` materializes every dependency edge in the
  account on every changed ingest.** [verified, measured]
  `agent_triage.rs:811,819` (`WITH RECURSIVE edges ... UNION ...`), called
  inside the ingest transaction (`messages.rs:455-461`). Plan `MATERIALIZE
  edges`, `UNION USING TEMP B-TREE`, `AUTOMATIC COVERING INDEX`; 329 ms at
  50k, 412 ms after ANALYZE (structural). A 500-message burst is ~3 minutes
  of write-lock time. Recursing through `idx_agent_decision_source_reverse` /
  `idx_agent_attention_source_reverse` (`schema.sql:1988-1989`) measured 36 ms
  before ANALYZE and ~0 after. High / M.
- **S6. `synchronous=FULL` and autocommit loops.** [verified, measured] 200
  single-row `UPDATE`s: FULL 117 ms, NORMAL 3 ms, one transaction 1 ms.
  Loops that pay it: `upsert_message_vector` (`search.rs:663-688`,
  DELETE + INSERT = 2 commits per vector, so a backfill batch of 8 is 16
  fsyncs); `ensure_source_assessments` executed on a bare `&Connection` from
  `agent_read_thread`, `agent_search_mail`, `agent_sender_history`,
  `agent_read_message`, `snapshot_agent_sources` and
  `thread_view -> request_external_thread_access_conn`
  (`messages.rs:301-324,530-533`), up to ~10 fsyncs per *read*;
  `migrate::backfill_message_recipients` (its own comment admits it). Also
  missing: `journal_size_limit` (the WAL stays huge after
  `rebuild_fts_if_stale`'s whole-mailbox `INSERT ... SELECT`),
  `temp_store=MEMORY` (several hot plans use temp b-trees), `cache_size`
  (2 MB default against a 200 MB-1 GB file). Med-High / S.
- **S7. Every open, daemon and `squelch-mcp` alike, re-runs whole-table
  one-shot migrations.** [verified] `backfill_message_recipients`
  (`migrate.rs` ~1402-1441, `SCAN m` reading every overflow chain because
  `is_sent`/`to_addrs` sit after the bodies), `retire_records_destination`
  (~1573-1660, `json_each` per row), `retire_preference_retriage`,
  `clear_recoverable_no_body_markers`, `heal_own_mail_read_as_inbound`, three
  schema.sql tail repairs (`schema.sql:1802-1816`), ~60 `PRAGMA table_info`.
  ~0.4 s warm at 50k; cold is one full read of the messages table, seconds on
  a network volume. The FTS rebuild is correctly gated on `sqlite_master`
  text. Med / S.
- **S8. `agent_deliveries::reconcile` decodes every decision in the account
  inside the commit.** [verified SQL, plausible serde]
  `agent_deliveries.rs:233-290`: 208 ms of SQL plus ~50k
  `serde_json::from_str` per call, under the mutex and the write lock, on
  every commit whose decision names a delivery. Med / M.
- **S9. `human_inventory` decodes two JSON blobs per row with a two-query
  `thread_fye_override` N+1, and `early_limit` is off for Standing (the 10 s
  poll) and for tier filters.** [partly verified]
  `agent_triage.rs:4849-5060`, `handlers.rs:251`. Standing also runs
  `list_items(fye)` (150 ms at 50k) plus `rank_items` in Rust. The plain
  listing is excellent (0.6 ms per 50-row page). Med / M.
- **S10. Missing indexes shown by plans.** [verified] `send_trackers(account_id,
  message_id)`: `message_opens` (`tracking.rs:131-140`) plans `SCAN o` and
  `sent_listing`'s correlated count (`messages.rs:1419-1425`) is `SCAN st` per
  row. `agent_triage_runs` has no index and `agent_diagnostics` does `ORDER BY
  id DESC LIMIT 25` over a forever-growing table. `notification_assessments
  (message_id)` is an FK child with no index (matters only if `DELETE FROM
  messages` ever ships). Med / S.
- **S11. Unbounded tables with no retention.** [verified by grep] Never
  deleted: `events`, `audit_log`, `notify_decisions`, completed
  `agent_triage_jobs` (`idx_agent_jobs_retriage_progress` is a 7-column index
  over all of them and `retire_preference_retriage` scans them every boot),
  `agent_triage_runs` (`metadata_json` embeds the whole config per run,
  ~2-3 KB), `message_opens`, fired `triage_revisits`, `shred_log`,
  `notification_assessments`. Sweeps exist only for `scheduled_sends`,
  unclaimed `outbound_attachments` and pairing codes. Med / S.
- **S12. FTS5 is rewritten on every upsert, duplicates the body, is never
  merged.** [verified] `messages.rs:147-152` DELETE + INSERT on every
  `upsert_message_conn` including unchanged re-ingests; `schema.sql:1012-1016`
  is a regular table with `prefix='2 3'` (three indexes per write, body stored
  twice); no `'merge'`/`'optimize'` anywhere. Med / M.
- **S13. sqlite-vec KNN is a brute-force scan with `account_id` as a metadata
  column, not a partition key.** [plausible] `search.rs:421-431`,
  `schema.sql:1041-1045`. 100k vectors is ~154 MB read per search. Declare
  `account_id INTEGER PARTITION KEY`; consider int8/binary quantization if
  memory matters. Med / S-M.
- **S14. Hot statements recompiled on every call.** [plausible]
  `human_inventory` (`agent_triage.rs:4877`), `list_items`, `CLAIM_JOB_SQL`,
  `read_message`, `events_after`, the seven statements in `stats()`.
  ~50-300 µs each; free to fix; raise `set_prepared_statement_cache_capacity`
  past 16 if adopted broadly. Low-Med / S.
- **S15. A read pool would help, after S1-S5.** [plausible] WAL already allows
  N readers with one writer; candidates: `stats`, `human_inventory`, search,
  `thread_view_with_html`, SSE replay, agent-door listings. Non-SQL work under
  the lock is small but real: SHA-256 over full bodies for up to 1000 thread
  messages in `context()` inside the `BEGIN IMMEDIATE` commit
  (`agent_triage.rs:46-75,1588-1600`), serde decodes, ghost laundering. No
  HTTP or embedding under the lock. Low-Med / M-L.

### Y. Sync engine (`squelch-core/src/sync`)

- **Y1. Fixed 5 s poll: 3 HTTPS requests and 2 WAL commits per tick, no idle
  backoff.** [verified] `sync/mod.rs:1363-1451` (`sleep(interval)` at
  `:1410`), `config.rs:298`. Per tick: `refresh_inbox_unread`
  (`mod.rs:2748-2772`, a `labels/INBOX` GET then `set_inbox_unread` at
  `messages.rs:1236-1252` which always writes `fetched_at`); `history_walk`
  (`mod.rs:1595-1677`) with a label-filtered `history.list` for INBOX and
  SENT; `store_history_cursor` unconditionally at `:1670`; `reminder_pass`
  (`:2726-2746`, takes the write lock). 17,280 ticks a day is ~52k requests
  and ~35k fsyncs per idle daemon. The code already distrusts the label filter
  (`mod.rs:216-219`), so one unfiltered walk split client-side is safe. High / M.
- **Y2. Notification lane: fixed 1 s loop, two `BEGIN IMMEDIATE` claims per
  iteration, no wake.** [verified] `triage_worker.rs:657-745` (`:731`),
  `agent_config.rs:43` (`worker_poll_secs: 1`), `claim_agent_job`
  (`agent_triage.rs:1377-1424`: immediate transaction, `randomblob`, the
  `CLAIM_JOB_SQL` join). ~172,800 no-op write transactions a day per tenant,
  serialized against ingest and the doors. The refine lane next to it
  (`mod.rs:1453-1526`) already has `refine_wake` plus backoff to 30 s. High / S.
- **Y3. The vector drain scans the whole messages table every refine round.**
  [verified] `mod.rs:1511-1513` calls `backfill_missing_vectors` every round
  (1 s active, 30 s idle); `messages_missing_vectors` (`search.rs:1439-1465`)
  is `NOT EXISTS (SELECT 1 FROM message_vecs ...)` ordered by `received_at`,
  which in steady state never short-circuits: 50k vec0 probes every 30 s,
  under the mutex, on a worker thread. Med-High / S.
- **Y4. Every Gmail GET re-reads the credential store.** [verified]
  `mod.rs:1116-1147` `send_get -> creds.token()`; `credentials.rs:1116-1127`
  `-> spawn_blocking(valid_token_blocking)` `-> load_token_file` (`:970-996`):
  `age_identity_from_env` reads the identity file (`:585-595`), then the
  blob is read, decrypted and parsed. On the keyring backend it is a DBus
  round trip per request. `validate_or_refresh` (`:303-339`) only refreshes
  near expiry, so the I/O is pure overhead ~52k times a day. Med / S.
- **Y5. Catch-up re-fetches and rewrites the entire 30-day window.** [verified]
  `mod.rs:1758-1858`; the comment at `:1744-1752` records "spent half an hour
  in here". 20 quota units per message, so 4,500 messages is ~90k units
  (~19 min at the 80% pacer), and each re-ingest rewrites body and HTML, the
  FTS doc, the sender count, the SHA-256 and attachment blobs up to 25 MB.
  `subtract_ids` (`:357-378`) already has the in-memory shape of the fix.
  Med-High / M.
- **Y6. MIME parse, ammonia, flatten and the ~15-statement ingest transaction
  run on a tokio worker thread.** [verified] `mod.rs:2358-2520,2533-2578`
  call `ingest_one` inline from an async fn; `ingest.rs:946,1107-1112,
  1079-1084`; `messages.rs:795-1160`. Only embedding uses `spawn_blocking`
  (`:2609,2653`). `store/mod.rs:3-5` states the contract and
  `gcal/sync.rs:302-312` follows it. On a 2-core NAS a multi-MB newsletter
  stalls half the runtime; on hosted (see H1) all of it. Med / M.
- **Y7. Sent-contacts harvest: unbounded whole-mailbox serial sweep,
  all-or-nothing.** [verified] `mod.rs:1860-1996`: `list_message_ids(SENT,
  None)` with no window, one sequential `format=metadata` GET per id at 20
  units each, merged once at the end; the doc at `:1868-1869` says an
  interrupted pass restarts from scratch. 30k sent messages is 600k units and
  ~2 h; one error at message 29k discards everything. Runs 180 s after boot on
  a second engine (`squelchd.rs:2381-2403`). `backfill_recipients`
  (`:2199-2306`) polls `cc_addrs IS NULL` with no index and `skipped.contains`
  is O(n) per row. Med / M.
- **Y8. Redundant per-message body work at ingest.** [verified]
  `enqueue_agent_triage_lane` (`agent_triage.rs:607-616`) re-reads the body
  and SHA-256s it (`:83-106`); the FTS doc is always deleted and reinserted
  (`messages.rs:148-152`); `bump_sender_conn` (`senders.rs:47-60`) runs
  `COUNT(*)` over every message from that sender per ingest. Low-Med / S-M.
- **Y9. ammonia `Builder` rebuilt per message; per-tag allocations; no cap on
  stored bodies.** [verified] `sync/html.rs:27-105`; `ingest.rs:62,74,139,150`;
  `body` and `body_html` stored whole (`ingest.rs:1107-1112`,
  `messages.rs:96-118`), so a multi-MB marketing mail is stored three times
  (body, HTML, FTS) and copied again on every read. Low-Med / S.
- **Y10. `reqwest` built without `gzip` or `http2`.** [verified features,
  plausible ratio] `Cargo.toml:65`; `mod.rs:835-840`. `format=raw` is base64
  of mostly text and compresses 3-5x; a 5k-message backfill moves ~0.5 GB that
  could be ~150 MB. Low-Med / S.
- **Y11. Peak memory per in-flight message is ~2.75x raw, two in flight, plus
  25 MB attachment copies.** [verified copies, plausible sizes]
  `mod.rs:2522-2531,1052-1055,424-431,309-328` (`MAX_RAW_BYTES = 64 MB`),
  `config.rs:299` (`fetch_concurrency: 2`), `ingest.rs:904`. Two 50 MB
  attachment-heavy messages is 400+ MB transient on a 1Gi pod. Low / M.
- **Y12. Backoff ladders have no jitter.** [verified] `mod.rs:226-227,
  2812-2869,2883-2900`; `gmail_budget.rs:33`. A fleet re-enters together after
  any Gmail blip. Carriers (`poller.rs:137-157`) already spread. Low / S.
- **Y13. Dead work.** [verified] Two `SyncEngine`s each build a `reqwest`
  client and a throwaway `SyncMetrics` (`squelchd.rs:2310-2330,2381-2403`);
  `fetch_raw_and_ingest` loads `list_sender_rules` per batch (`mod.rs:2368`)
  into a parameter `ingest_with_rules` ignores (`ingest.rs:1256-1264`);
  `is_robot_address` re-lowercases lowercased input. Low / S.

### H. HTTP doors and runtime (`squelchd`, `squelch-api`, `squelch-mcp`)

- **H1. One tokio worker under the hosted CPU quota, and the sync engine,
  triage worker and MCP tools take the store mutex on it.** [verified
  pattern, plausible count] `squelchd.rs:421-426` builds
  `new_multi_thread()` with no `worker_threads`; tokio defaults to
  `available_parallelism()`, which honours cgroup `cpu.max`; the tenant limit
  is `1000m` (`15-warden-config.yaml:264`). `sync/mod.rs` has 20 direct store
  calls against 3 `spawn_blocking`; `triage_worker.rs` 24 against 0;
  `ingest.rs` 0; `squelch-mcp/src/server.rs` ~16 against 1 (`:1171`). When
  any `spawn_blocking` handler, embed or triage commit holds the mutex, the
  lone worker parks in `self.lock()` (`store/sqlite/mod.rs:296`) and nothing
  progresses: no accept, no auth, no SSE keep-alive, no timers. Verify with
  `cat /sys/fs/cgroup/cpu.max` in a tenant pod. High / M.
- **H2. (= Y1.)** Four Gmail requests and one fsync per 5 s per tenant; the
  daemon already knows whether anyone is listening
  (`event_tx.receiver_count()` in `events.rs`). High / M.
- **H3. The client's 10 s tick recomputes the whole mailbox twice, with no
  ETag, while an SSE channel is already open.** [verified]
  `/client/v2/feed` (`triage.rs` `feed -> agent_fye(usize::MAX)`,
  `agent_triage.rs:1922-1935`, `list_items` at `:2169+` a five-way join with
  `json_array_length` per row, then `rank_items` in Rust, then truncate to
  200); `/client/stats` (`handlers.rs:3541-3560 -> stats()`, see S3, plus
  seven more store calls). Both `no-store`; the only `ETag` in the tree is
  `/client/account/photo` (`handlers.rs:748-757`). The client already holds
  `GET /client/events` (`EventStream.swift`). High / M.
- **H4. `/metrics` re-runs the full-mailbox aggregates every 30 s under the
  mutex.** [verified] `squelchd.rs:1844-1898` (`stats`,
  `list_usage_by_category(LEDGER_ALL_DAYS)`, `triage_job_counts`,
  `retriage_progress`, ... in one `spawn_blocking`); the 1-permit semaphore
  caps concurrency, not frequency. Med / S.
- **H5. (= S6.)** `synchronous=FULL`. Med / S.
- **H6. `write_client()` builds a fresh `reqwest::Client` per action.**
  [verified] `handlers.rs:4371-4385 -> gmail_write.rs:862`. Twelve call sites
  (handlers x4, `scheduled_send.rs` x6 including per recovered and per fired
  row, `group_send.rs`, `sharing.rs`). Each send, archive, label or not-spam
  pays a TCP + TLS 1.3 handshake and a rustls config build; every other client
  in the tree is long-lived. Med / S.
- **H7. No response compression anywhere.** [verified]
  `squelch-api/Cargo.toml:35` (`tower-http` with `cors` only);
  `lib.rs:352-356`. `GET /client/thread/{id}` returns every message's
  sanitized HTML inline (`handlers.rs:589-630`); the feed returns 200 items
  with decision and attention JSON. `DefaultPredicate` already excludes
  `text/event-stream`. Med / S.
- **H8. MCP tools: N+1 store round trips per row on the runtime thread, and
  uncapped thread bodies.** [verified] `server.rs:715-752`
  (`external_agent_fye(..., 1000, ...)` then per item `agent_access_allowed`
  at `:730` and `external_thread_allowed` at `:580-585`: up to 2,000 extra
  mutex acquisitions per call); `get_thread` (`:769`) returns whole bodies
  (`messages.rs:543`); `search_mail` is bounded. Med / M.
- **H9. Scheduled-send ticks every 10 s with three store calls, one of them
  `BEGIN IMMEDIATE`, on an empty outbox.** [verified]
  `squelchd.rs:2480-2512`; `scheduled_send.rs:679-717`;
  `scheduled.rs:326,733`. Low-Med / S-M.
- **H10. Bearer check is one store round trip per request, by design.**
  [verified] `auth.rs:64-100`; `device_tokens.rs:71,92,415`. Correct
  (`token revoke` must bite on the next request); only noticeable through H1
  and H3. Leave it. Low.
- **H11. No request timeout, hyper's header-read timeout is inert, no global
  concurrency limit.** [verified, hyper default plausible] `axum::serve` at
  `squelchd.rs:2680` never sets a timer, so hyper-util's 30 s
  `h1_header_read_timeout` never fires; no `TimeoutLayer`, no load-shed on
  `/client/*`; only the per-route semaphores. A wedged store parks a blocking
  thread per request up to the 512 default. Low-Med / S.
- **H12. Startup.** [verified] `cmd_serve` (`squelchd.rs:2026-2527`): config
  parsed twice (`config.rs:1875-1898`); `migrate()` runs S7 on the main thread
  before the bind; 6-11 `reqwest::Client`s with their own rustls configs built
  synchronously; the sync engine and two one-shot `spawn_blocking` cleanups
  (`:2419,:2439`) take the mutex before the listener binds (`:2524`);
  `SquelchServer::new` calls `ensure_account` a second time
  (`server.rs:493`). Embedder init is correctly in the background (`:2600`).
  Low / S.
- **H13. SSE replay is N+1.** [verified] `events.rs:128-160`: one
  `spawn_blocking` for the 100-row batch, then one more per event for
  `notification_delivery_allowed`. Low / S.
- **H14. Calendar sync re-reads the credential store every 60 s when nothing
  is granted.** [plausible] `gcal/sync.rs:73-75`. Low / S.

### T. Triage, LLM and embedding (`squelch-core/src/triage`, `embed`)

What runs per message today (the Stage-1/Stage-2 classifier is retired,
`triage/mod.rs:1-5`): one bounded agent investigation (`agent.rs::run_agent`,
`prompts/agent-v1.txt` ~2.1k tokens, `max_model_turns=4`, `max_tool_calls=8`,
90 s wall clock, `agent_config.rs:38-41`) at `claude-opus-5-5` / `low`, with
`xhigh` for an account's first five days (`config.rs:1366-1369,1430-1444`);
a Haiku 4.5 fast notification call per eligible arrival
(`notify_lane.rs:327-337`); a Haiku 4.5 access call for sent and spam mail
and for every historical message the agent reads through a tool
(`access.rs`, `agent_triage.rs:969-1001`). Live caps: `agent.daily_run_cap`
1000, background 200, revisit 50, notify 1000.

- **T1. The 30-day backfill runs at onboarding `xhigh`; effort is chosen per
  account, not per job.** [verified] `triage_worker.rs:418-424` applies
  `effort_for_account` to every job, including the `"migration"` jobs queued
  for the whole window at account creation (`agent_triage.rs:1880-1920`).
  `config.rs:1309-1315` justifies `xhigh` as "the first impression" but cites
  an eval where `low` matched it on every metric. Up to 1,000 background runs
  at ~1.6x price: ~$30-40 extra per new tenant, and the backfill drains at
  half speed. High / S.
- **T2. No per-body cap on the target message.** [verified mechanism,
  plausible savings] `agent.rs:62-66` bounds `context.initial` at
  `max_context_bytes / 2` = 60,000 bytes and `fit()` (`:349-375`) spends it
  on the target body first. Siblings are excerpted (2 full + 6 x 400 chars,
  `agent_config.rs:79-81`) but a 55k-char newsletter rides nearly whole,
  ~14k tokens ($0.056) per turn. `condense.rs:4-11` (measured 2026-09-28):
  the top decile of calls carried half of all uncached input. The retired
  stages capped bodies at 6k/12k chars; the agent has no equivalent, and the
  `read_message` pager (`triage_worker.rs:59-70`) already exists for the
  tail. Med-High / S.
- **T3. One 90 s wall clock across all turns, timeouts retried up to six
  times, each attempt billed.** [plausible] `agent.rs:68-84` (one
  `tokio::time::timeout` around all turns; `:40-42` notes a timed-out request
  may be billed without usage); `triage_worker.rs:486-536,637-655`
  (`agent_timeout` is neither outage nor refusal, so `retry_agent` with
  `max_attempts=6`, no refund because `model_calls > 0`, budget re-reserved
  each time). `xhigh` with 8,000 thinking tokens across 2-4 turns can exceed
  90 s; those are exactly the long messages that already cost most. Measure
  first: `last_error='agent_timeout'` counts. Med-High / S.
- **T4. Full-thread loads (LIMIT 1000, full bodies, two correlated subqueries
  and a SHA-256 per row) 3-6 times per run, synchronously.** [verified]
  `agent_triage.rs:142-144,36-44,63,86-97`; call sites
  `triage_worker.rs:311,44-48,81,151-157,142`; the commit re-runs
  `context(&tx)` (`agent_triage.rs:1592`) and `read_message` per source
  (`:1595-1597`) under `BEGIN IMMEDIATE`; `snapshot_agent_sources` reads every
  id twice (`:1776-1784`). The notify lane loads the same full context to
  build a one-message push (`triage_worker.rs:688`). No `spawn_blocking` in
  `triage_worker`, `agent`, `access` or `notify_lane`, and all three lanes are
  one `select!` task (`sync/mod.rs:1337-1352`), so a long-thread read blocks
  the Gmail poll and the push lane. Med / M.
- **T5. Multi-turn runs re-send the whole context uncached every turn.**
  [verified] `agent.rs:112-116` builds `{context, investigation,
  must_finish}` as one string; `llm.rs:267-270` sends it as a single user
  block; the only `cache_control` is on the system block (`llm.rs:440-452`).
  Turn 2+ pays the full 3-15k context again at $4/MTok. Med / S.
- **T6. The output schema forces ~6 prose fields and every key on every
  Finish.** [plausible] `agent.rs:756-928` (strict mode needs every key);
  `config.rs:1300-1305` says output tokens were two thirds of the bill.
  Nullable `notification.*` and `external_access.reason` plus word caps on
  `summary`/`reason` should cut 20-40% of non-thinking output. Needs the
  replay eval. Med / S.
- **T7. Model tiering is a config-only experiment the code already supports.**
  [plausible] `agent_config.rs:13-15` (`model`, `review_model`);
  `agent.rs:106,215-222`. `sonnet-5-5` agent with `opus-5-5` review halves
  token prices if the replay holds; `haiku-5-5` at `low` for the notify and
  access lanes (currently Haiku 4.5, `config.rs:415-422`) is ~10x cheaper on
  two calls per message. The eval behind the current choice
  (`evals/results/2026-10-triage-model-comparison.md`, cited at
  `config.rs:1304`) is not in the repo. The user's call. Med / S-M.
- **T8. Every inbound message gets a full Opus run plus a Haiku fast-lane call;
  nothing deterministic short-circuits.** [verified] `messages.rs:1150-1160`
  enqueues every ingest; `List-Unsubscribe` is parsed and stored
  (`ingest.rs:448-461`) and unused for routing; `notify_lane.rs:74-98` filters
  only sent/spam/eligibility. Safe first step: skip the fast lane for
  one-click `List-Unsubscribe` mail with no `surface` rule and no login-code
  hit. Riskier: bulk mail at one turn with a smaller sibling window; the
  migration backfill through the Batches API at 50%. Med / S to M-L.
- **T9. One `429`/`5xx` on an agent step puts the whole account on a 5-minute
  cooldown, ignoring `retry-after`.** [verified] `agent.rs:125`
  (`max_tries: 1`); `llm.rs:676-684` (`retry-after` parsed only on the retry
  path); `triage_worker.rs:512-529` (`is_provider_outage("http_429")`,
  `outage_retry_secs=300`, half-open at concurrency 1). Right for
  401/402/403/404, wrong for 429/529. Med / S.
- **T10. `request_review` is offered to the model when no `review_model`
  exists.** [verified] `agent_config.rs:14,36`; `prompts/agent-v1.txt:1`;
  `agent.rs:215-226,948-951`. A billed turn spent learning the tool is absent,
  and one of four turns gone. Low-Med / S.
- **T11. Access jobs send uncondensed bodies up to 120 KB through Haiku and
  draw down the 200/day background budget.** [verified] `access.rs:36-39`;
  `triage_worker.rs:594-598,752-762`. Low-Med / S.
- **T12. (= Y2.)** Plus: a pass claims two jobs then `join_all`s them
  (`triage_worker.rs:271-300`), so a 90 s run idles the other slot. Low / S.
- **T13. (= Y3.)** Low / S.
- **T14. Embedder.** [verified] Reload sawtooth: `idle_unload_secs=600`
  (`config.rs:1077-1080`), so a mailbox with arrivals every 10-20 min reloads
  195 MB each time; `with_intra_threads` never set (`fastembed_impl.rs:70-74`),
  which is 1 under the hosted quota but a thread per core on a self-host box
  for a batch-of-1 ingest embed. Pin 1-2. Low / S.
- **T15. ~6.5k lines of retired classifier still compile; the eval is not in
  the repo.** [verified] `stage1_llm.rs` (1,475), `stage2.rs` (2,368),
  `router.rs` (411), `extract/**` (2,932): no live callers. `Stage1Config` /
  `Stage2Config` caps (`config.rs:1198-1245,1340-1368`) gate nothing. Low
  (maintainability) / M.

### C. Client model and launch (`passband/Sources/Passband/Model`, `App`)

Launch path as traced: `RootView.swift:137-146` `.task` ->
`AppStore.loadSettings()` (`:1411-1446`: account index and keychain off main)
-> `recoverSavedConnection` (`:1481-1518`): `connStatus = .loading`, then
`APIClient.probe` (`APIClient.swift:450-492`, `GET /client/v2/capabilities`),
and only then `.connected`; `RootView.swift:39-50` shows `LoadingGate` until
then; `.onChange(of: connStatus)` (`:150-173`) starts `SitrepPoller`
(`SitrepPoller.swift:152-268`: feed, stats and sealed in parallel), the SSE
feeds, and two detached refreshes. The first row paints after `store.sitrep =
next` (`:176`). The read model starts empty (`AppStore.swift:596`). iOS is the
same (`MobileRootView.swift:92-118`).

- **C1. No persisted read model; the probe runs serially ahead of the first
  pull.** [verified] Nothing in `Model/` writes `SitrepData`, `StoreStats`,
  `zones` or `mailPages` to disk (the only persisted fragment is the FYE
  headline in `UserDefaults`, `AppStore.swift:1173,1203-1209`). Every launch
  is keychain, then a probe RTT, then a feed RTT, then paint, worse under
  `CapabilityProbeRetry`. The capabilities only need to gate writes. High / M.
- **C2. The 10 s poll never pauses when inactive, hidden or closed.**
  [verified] `SitrepPoller.swift:16,74-86,97-108`; `PassbandApp.swift:164`
  keeps the process alive after the last window closes; nothing in `Model/`
  touches `occlusionState`, `didResignActive` or `scenePhase`. Each tick is
  three requests, every 45 s three to five more, and on the Emails page a
  100-row `/client/updates` per tick (`EmailsView.swift:950-974`). The
  `EventStream` (`EventStream.swift:284-316`) is already resident and could
  be the wake. High / S-M.
- **C3. Thread-body prefetch churn.** [verified] `ThreadPrefetch.swift:21`
  (`freshDefault = 60`), `:71-86` (refetch once `age >= fresh`), `:144-165`
  (regex pass on every put). Callers on a clock: `AppStore.swift:3128`
  (40 rows every poll, because `mailTTL = 5` at `:2898`), `:3423-3426`
  (18 zone threads every 45 s), `:3210` (sent page). The Mac board does it
  right (`SitrepView.swift:77,978`: `boardFresh = 300`, top 10). iOS
  (`MobileSitrepView.swift:189-192`) warms every standing thread on every
  `onAppear` with the 60 s default. ~40 `GET /client/thread/{id}` a minute
  idle on the Emails page; on the phone up to 200 fetches per return to the
  tab. High / S.
- **C4. Launch image/hero warm walks the whole FYE band and every reading
  sender, four wide.** [verified] `ImageWarmer.swift:55-78,95-106`;
  `HeroCache.swift:34,85-93,99-113`; triggered from `SitrepPoller.swift:253`
  and `AppStore.swift:2706`. Up to 200 thread fetches plus image downloads
  contend with the first click. Med-High / S.
- **C5. Assistant: no prompt caching, `get_thread` dumps every body uncapped,
  history never pruned.** [verified sizes, plausible savings]
  `Assistant.swift:376,470-477,830-848,1283,1369-1515`;
  `AgentTools.swift:261-285,289-315,398-400`; no `cache_control` in
  `Assistant/`; models `Prefs.swift:642-644`. Every turn of up to 8 re-sends
  the system prompt, 16 tool definitions and the full history uncached;
  `show_emails` snippets at 180 chars but `get_thread` returns whole bodies
  that then ride in history. The prefix is already cache-stable (volatile
  block at the end). Med-High / S-M.
- **C6. The 1000-row records feed is walked three times on the main actor
  every 45 s; `zones` is one observed property for five arrays.** [verified]
  `AppStore.swift:218-230,2635-2707` (the author's own note at `:2672-2674`);
  `WireTypes.swift:2587-2640`. Med / S.
- **C7. Emails page refetches and re-sorts 100 rows on main every tick; no
  conditional requests anywhere.** [verified] `EmailsView.swift:950-974`;
  `AppStore.swift:3024-3047,3078-3135`; `APIClient.swift:62`
  (`reloadIgnoringLocalCacheData`); `AgentFeed.ranked_at`
  (`WireTypes.swift:2513`) exists and is unused for skipping. Med / S client,
  M daemon (= H3).
- **C8. `headlineKey` rebuilt per board body evaluation.** [verified]
  `AppStore.swift:1185-1188`; `SitrepView.swift:173`. Low-Med / S.
- **C9. Synchronous file I/O on main at launch.** [verified]
  `Notifier.installSounds` (`Notifier.swift:59-80`), `Typo.registerBundledFonts`
  (`Typography.swift:25-33`). Low / S.
- **C10. Caches are count-bounded, not memory-pressure aware, and hold two
  copies of each body.** [verified] `ThreadPrefetch.swift:19,205-225`;
  `HeroCache.swift:44`; `WireTypes.swift:281-287`. Low-Med / S.
- **C11. Every activation re-identifies analytics and (iOS) re-registers push
  with every account.** [verified] `PushRegistration.swift:48-85,117-129`;
  `Analytics.swift:380-394`. Low / S.

### V. Client views (`passband/Sources/Passband/Views`, `PassbandiOS/Views`)

- **V1. The iOS WebKit frame pool is Mac-sized and nothing drains it under
  memory pressure.** [verified] `EmailWebCore.swift:1917` (`capacity = 6`),
  `:2045` (`prerenderedCapacity = 4`), `:2047` (`spareTarget = 2`), plus the
  measurer (`FrameMeasurer.swift:63-75`); `EmailWebViewiOS.swift:50-63`
  identical; no `#if os(iOS)` on any cap; no
  `didReceiveMemoryWarningNotification` handler anywhere. ~13 live WebContent
  processes on a phone. Med-High (iOS) / S.
- **V2. The reader and inbox bodies subscribe to the whole `stats` struct for
  one field.** [verified] `ThreadViewer.swift:842`
  (`store.stats?.account_email` inside the `ForEach`); `EmailsView.swift:620,
  700` (`store.stats?.spam`); `SitrepView.swift:307`. `SitrepPoller.swift:177`
  replaces the struct whole whenever any counter moves, which re-runs the
  reader's `content` and every loaded inbox row. Med / S.
- **V3. `isReadingMail` is an O(senders x items) scan run 3-4 times per
  `ThreadViewer` body.** [verified] `ThreadViewer.swift:1989-1993`, read at
  `:431,634,1226,1965`. Low-Med / S.
- **V4. Design tokens route every read through the Observation registrar.**
  [plausible] `Space.swift:41-43`, `Typography.swift:62-70` read
  `Prefs.shared.density`, a tracked property; ~30-50 registrar accesses per
  inbox row body. Low-Med / S-M.
- **V5. Search results re-highlight every mounted hit on every j/k.**
  [verified] `SidePanel.swift:314,330,695-700,786-798`. Low-Med / S.
- **V6. `ThreadViewer` is one 2,800-line struct with a ~270-line modifier
  chain and per-body allocations.** [verified structure] `:264-530`;
  `:323` (`messages.map(\.id)` per evaluation); `:386`; same in
  `EmailsView.swift:233-236`. Low-Med runtime, Med compile time / M.
- **V7. `MailRow` recomputes subject/gist five-plus times per body.**
  [verified] `MailRow.swift:42-55,60-79`; `SubjectText.swift:25-26,55`. Low / S.
- **V8. Rail navigation tears the inbox page down and rebuilds it.**
  [verified] `RootView.swift:659-668`. Not a fetch storm (data is in the
  store); a first-paint cost per visit and lost scroll. Low-Med / M.
- **V9. Continuous `TimelineView(.animation)` rings.** [verified]
  `ThreadViewer.swift:4413` (`FlowBorder`, 30 fps for the whole Space walk),
  `ActionLayer.swift:317`, `ThreadViewer.swift:4011` (`StackDoneHold`, no
  `minimumInterval`, so 120 Hz during a hold). Low / S.
- **V10. Per-row and whole-page periodic clocks.** [verified]
  `ScheduledRow.swift:84,126` (one 30 s `TimelineView` per row);
  `SitrepZones.swift:36`; `CalendarPage.swift:90`. Low / S.
- **V11. The style guess re-splits every message's quotes synchronously on
  open for participated threads.** [verified] `ThreadViewer.swift:1972-1982,
  2000-2006` (`Quotes.splitText`, not the memoized variant), inside `adopt`
  (`:2821-2826`) on main. Low-Med / S.
- **V12. The measuring pass re-measures every HTML message on every first
  open, every launch, with remote images on.** [verified]
  `FrameMeasurer.swift:100-160`; `FrameHeights` is in-memory only
  (`ThreadPrefetch.swift:243`). Low-Med / M.
- **V13. `Avatar` spawns a Task per row mount even for human senders.**
  [verified] `Avatar.swift:69,91-105`. Low / S.

### O. Hosted (`squelch-warden`, `squelch-control`, `squelch-relay`, `deploy/`)

What a tenant is: a Deployment, Service, Ingress, NetworkPolicy, 10Gi PVC and
up to four Secrets (`objects.rs:573-671`); `squelchd` at 100m/384Mi request,
1000m/1Gi limit (warden `config.rs:77-138`; `15-warden-config.yaml:263-266`);
the fp32 weights (126 MB) copied from the shared models PVC into the tenant's
volume by the init container (`objects.rs:419-444`); no sidecars; TCP
readiness every 3 s. The box: 4 vCPU / 16 GB, quota patched live to 30
(`PRODUCTION.md:104-109`).

- **O1. The memory request is sized to a resident ONNX session the daemon now
  unloads.** [verified code, plausible that `daemon-0.0.12` carries it]
  Warden `config.rs:88-121` calls 384Mi "today's truth and a stopgap" pending
  the daemon-side unload; `embed/lazy.rs:77-79` and `config.rs:1086`
  (`idle_unload_secs: 600`) plus `mem.rs:13-20` (`malloc_trim`) have landed,
  and the warden sets no override. An idle-unloaded tenant sits near
  40-130 MB. `PRODUCTION.md:140-143` already names the next step. High / S.
- **O2. (= Y1.)** 24 requests/min per tenant is 12 req/s at 30 and 40 at 100,
  and the only thing justifying a 100m CPU floor. Med / S, L for push.
- **O3. The relay read-receipt pixel does a synchronous, fsync'd SQLite
  `INSERT` under a global mutex on the tokio worker.** [verified]
  `squelch-relay/src/handlers.rs:151-170`; `opens.rs:79-85,92-97`
  (`Connection::open`, no pragmas at all, so DELETE-journal and
  `synchronous=FULL`); `record()` then runs `purge_if_due` and `trim_if_due`
  (a `count(*)` over up to 1M rows every 60 s, `opens.rs:38,47,209-215`)
  inside the request; the drain (`:140-143`) takes the same mutex; all on the
  same runtime as `/v1/push`. Med now, High the day a campaign's opens arrive
  in a burst / S-M.
- **O4. `count/secrets` binds at 25 tenants, before the 30 the memory quota
  allows.** [verified] Four Secrets per tenant (`objects.rs:573,605,627,658`);
  live `count/secrets: "100"` (`PRODUCTION.md:109`); the git manifest's `60`
  is 15. The 26th signup fails as the documented timeout with the label
  burned. Med / S.
- **O5. The hosted manifests in git no longer describe production.**
  [verified] `70-tenant-limits.yaml` (`requests.memory: 1920Mi`, five
  tenants) versus the live patch; `deploy/hosted/node/kubelet.yaml` and
  `zram-generator.conf` "not applied here yet" (`PRODUCTION.md:22-23`). Med
  (risk) / S.
- **O6. The fleet roller converges one tenant per 5-minute tick.** [verified]
  `90-warden-roller.yaml:9`; `provision.rs:2166-2190`. 30 tenants is 2.5 h
  per release, 100 is 8.3 h. Med at scale / S-M.
- **O7. 126 MB of fp32 weights copied into every tenant volume.** [plausible]
  `objects.rs:419-444,359-361`. Disk x N and seconds of I/O in the signup
  path. Mount the models PVC read-only at the cache path
  (`model_appears_cached`, `fastembed_impl.rs:368`). Do not re-propose int8:
  `fastembed_impl.rs:177-200` pins fp32 with measurements. Low-Med / M.
- **O8. The relay opens buffer is one global queue with destructive
  ack-by-cursor.** [verified, known and double-locked] `handlers.rs:187-205`;
  `15-warden-config.yaml:181-187`; `tracking.rs:165-181`. Hosted read receipts
  cannot ship without keying opens by tenant. Low now / M.
- **O9. Signup blocks up to 180 s on readiness and a quota refusal surfaces
  only as that timeout.** [verified] `provision.rs:826-836`;
  `cluster.rs:606-634`; `PRODUCTION.md:130-137`. Pre-check
  `ResourceQuota.status.used`. Low / S.
- **O10. Per-tenant Prometheus series sit just under the scrape cap.**
  [verified cap math] `metrics.rs:928-937` (10 buckets x 200 series = 2,600
  samples of the `sample_limit: 2800` in `80-monitoring.yaml`). Low / S.
- **O11. Pixel rate limit is per client IP, the wrong key behind Gmail's and
  Apple's image proxies.** [plausible] `squelch-relay/src/ratelimit.rs:50,
  62-67`. Low / S.
- **O12. Litestream restarts the whole replicator on every fleet change.**
  [plausible] Fine at 30. Low.
- **O13. The five service Dockerfiles rebuild the whole dependency graph on
  every push.** [verified] No cargo-chef or dependency layer in any of them.
  Low ($) / S.
- Nits: `squelch-control/src/oauth.rs:221,403,464` builds a fresh
  `reqwest::Client` per OAuth exchange; the blackbox `bifrost_llm` canary
  POSTs `max_tokens: 2000` every 15 min (~96 Opus calls a day of pure cost).

### B. Build, dependencies, CI

Baseline: 634 packages in `Cargo.lock`; `squelchd` links 595 crates, control
557, warden 313, relay 249, broker 128. 35 integration-test binaries (42k
lines) plus 11 unit-test binaries. `ci.yml` 10-16 min per run (8 runs on
2026-10-08 alone); the `release-daemon` squelchd image took 13.4, 18.9, 18.5
and 29.8 min over the last four tags; `ci-passband` 4-5 min.

- **B1. The squelchd image has no dependency layer and builds both arches on
  one runner.** [verified timings, plausible gains] `squelchd/Dockerfile:38-64`
  (`COPY` every crate, one `cargo build`); `release-daemon.yml:74-83`
  (`platforms: linux/amd64,linux/arm64`, default cache scope; `:121-122`
  notes shared scopes evict each other, warden/control got `scope=`,
  squelchd did not). ~20 min of runner time per release; incremental
  releases should be 5-8 min. `Dockerfile:59` forces `aarch64-linux-gnu-gcc`
  (bfd) for the cross link. High / M.
- **B2. CI per PR: 46 full-tree links one at a time, plus a 2-minute disk
  purge on every run.** [verified] `ci.yml:90-95` (purge, 2.0 min measured),
  `:118-122` (clippy 1.2 min warm, test 6.7 min warm, job 11.3 min).
  `squelch-api/tests` has 17 files including `client_api.rs` at 15,209 lines,
  which is also the repo's dominant history blob (19 of the 20 largest pack
  objects). Med-High / S (purge), M (merge binaries).
- **B3. Images build with a floating `rust:1` toolchain; `rust-toolchain.toml`
  never reaches the Docker context.** [verified] `squelchd/Dockerfile:14`,
  `Dockerfile.control:15`, `Dockerfile:8`, `Dockerfile.broker:10`,
  `Dockerfile.warden:13`; `rust-toolchain.toml:9` pins 1.94. CI tests a
  different compiler than what ships. Med / S.
- **B4. `keyring` default features pull zbus, a second async runtime, into
  every Linux daemon that uses the file backend.** [verified graph]
  `Cargo.toml:49`; `config.rs:162-170` (Linux default `File`);
  `squelchd/Dockerfile:88`. ~85 crates. Med (build time) / S.
- **B5. ONNX Runtime is statically linked at build time from a single vendor
  CDN.** [verified] `Cargo.toml:79` (`ort-download-binaries`); `ort-sys`
  fetches from `cdn.pyke.io` on every cold build; an offline build fails to
  link. Mirror the two Linux tarballs and set `ORT_LIB_LOCATION`. Med
  (release-pipeline risk) / M.
- **B6. Release profile: leave it.** [verified] `panic = "abort"` would break
  documented behaviour (`unsub_job.rs:272`, control `handlers.rs:2560`,
  `embed/lazy.rs:287ff` rely on a panicking task being isolated); fat LTO /
  `codegen-units = 1` cannot touch the C++ ORT archive that dominates the
  binary and would serialize codegen on a 123k-line crate. Low.
- **B7. Allocator: not supported by the evidence.** [verified]
  `mem.rs:12-25`: jemalloc was measured and "does NOT do this on its own";
  trim already recovers the arena. mimalloc is the one unmeasured candidate;
  the real win is `EMBED-SERVICE.md`. Low.
- **B8. `squelch-desktop` is a dead build-output stump.** [verified] 42 files,
  2.0 MB of `dist/` and generated schemas, no source, not a workspace member,
  retired 2026-07-30, still matches `ci.yml:32`'s `squelch-*/**` filter. Low / S.
- **B9. The Mac release binary ships unstripped with no dead-strip.**
  [verified] `build.sh:70,92` (`-O -whole-module-optimization`, no
  `-dead_strip`, no `strip`, no dSYM); the pbxproj Release block only governs
  the iOS archive; `project.yml:31` sets `GCC_OPTIMIZATION_LEVEL: 0` for
  Release (harmless today, a trap). Resources 1.4 MB. Low-Med (update size) / S.
- **B10. Duplicate crate versions.** [verified] 42 names at 2+ versions; ours:
  `zerocopy 0.7` (`Cargo.toml:81`, 0.8 already in the tree), `hashbrown 0.14`
  via `rusqlite 0.32`'s hashlink (0.37 drops it), `webpki-roots 0.26` via
  `tokio-tungstenite 0.29`. Low / S.
- **B11. `reqwest` `blocking` in the daemon for `squelchd auth` only; `cookies`
  pulls a second Public Suffix List.** [verified] `squelch-core/Cargo.toml:33-37`;
  `squelch-api/Cargo.toml:43`. Low / S-M.
- **B12. hf-hub's `ureq` is a second HTTP stack (~46 crates) for the one-time
  weights download.** [verified] Low / M.
- **B13-B14. `tokio` `full` and the big compiled tables (chrono-tz, psl,
  ammonia, mail-parser `full_encoding`) are all used and load-bearing.**
  Leave them.
- **B15. `release-daemon.yml` leaves the squelchd job on the default GHA cache
  scope.** [verified] `:82-83` versus `:121-124`. Low / S.

## 6. Already good, do not re-propose

Daemon store: WAL + `busy_timeout`; idempotent schema; one transaction per
ingest covering every side table (`messages.rs:101-464`); `BEGIN IMMEDIATE`
wherever a read precedes a write; partial and covering indexes with pinned
plans and regression tests (`idx_agent_jobs_pending_claim` claims in 0.0 ms
with 50k completed rows); FTS `CROSS JOIN` pin, capped diagnostic counts,
`prepare_cached` hydration; the `senders` directory maintained at ingest;
keyset paging with early `LIMIT` (0.6 ms per page); `append_event` drops the
lock before broadcasting; no HTTP or embedding under the mutex.

Sync: `GmailBudget` (one adaptive pacer per mailbox, priced per method,
`Retry-After` honoured, never add a second limiter); incremental
`history.list` with a single cursor and 404-to-catch-up; SPAM never polled;
the refine lane is event-driven with backoff; `poll_healthy` stops paid model
calls during Gmail outages; a bounded `FuturesOrdered` fetch window with
per-message ingest and a 64 MB raw bound; `malloc_trim` after big passes;
`heal_blank_bodies` is resumable (the harvest should copy it); push, carriers
and calendar pollers are jittered, bounded and shutdown-aware.

Runtime and doors: the port binds before the model loads; ONNX runs under
`spawn_blocking` with lazy unload and a `try_lock` reaper; metrics are atomics
with a closed label set, a series cap and no lock in `render`; JWKS is cached
with coalesced refresh; config is parsed once; SSE is broadcast-driven with
bounded channels and a 15 s keep-alive; every in-process map is bounded
(ratelimit, console limiter, rmcp session reaper); no local Chromium (the
unsubscribe browser is a remote Browserbase session, concurrency 2);
pagination clamped everywhere; typed JSON bodies; per-route semaphores on the
abusable unauthenticated paths; console HTML is `format!`-built with nothing
compiled per request.

Triage: prompt caching with a 1 h TTL on the system block, measured (48% miss
at 5 min, 31% of the bill); schema compaction asserted under 9.5 KB; one
native tool with `disable_parallel_tool_use`; condensed quoted tails and
URLs; byte-bounded context with a paged `read_message`; effort measured on a
393-email replay; budgets with atomic reservations and refunds only on
zero-call failures; config failures halt the pass; the notify fast lane is a
small model, 1,500 chars, 8 s, one attempt; lease tokens and a partial index
on the job queue; revisits are scheduled jobs, not sweeps; embedder batch 8
at 256 tokens chosen from measured peaks, spam never embedded.

Client: `@Observable @MainActor` store with compare-before-assign at every
poll write and zero-size watcher views so `lastRefresh` never invalidates the
shell; `APIClient` actor with one static decoder, decoding off main;
formatters and regexes static with a 20k date LRU; jittered backoff,
in-flight join, epoch fencing, TTL-joined refreshes; SSE with per-account
cursor and inactivity watchdog; lazy inbox with stable ids and the cursor
isolated so hover and j/k never re-run the page; sitrep zones equality-guarded
with a cached `AdaptiveGrid`; `MessageCard: Equatable`; `WebFramePool` LRU +
prerender + spares, warmed at boot, JavaScript off with a CSP, no content
rule list compile, `ResizeObserver` height with no JS polling, body prep off
main; Phosphor is an MTKView at 30 fps paused when the window is not key;
favicons through `AsyncMemo` with negative caching; settings screens mount per
visit; Release is `-O` whole-module with dead-code stripping; Swift 6 mode.

Hosted: APNs JWT cached 50 min, HTTP/2 client, a 64-permit semaphore; token
buckets that prune and cap; control on Postgres through deadpool with
long-lived upstream clients and the warden called only at signup, pair and
reconnect; the warden's kube client built once with label-selected lists and
bounded readiness polls; every tenant container bounded, read-only root,
`automountServiceAccountToken: false`; one host-level Litestream, not N
sidecars, age-encrypted; the models PVC and the Bifrost gateway shared; no
per-tenant model download or LLM cold start.

Build: thin LTO + `strip`; `line-tables-only` dev debug mirrored into the CI
cache key; rustls everywhere, no openssl; `default-features = false` with
each opt-in justified; multi-stage slim images, non-root, no compiler in the
final layer; arm64 cross-compiled without QEMU; the warden does not link
`squelch-core`; CI path filters and cancel-in-progress; `rust-lld` default on
x86_64 at 1.94.

## 7. Looks obvious, is wrong

- `panic = "abort"`: silently changes the daemon's documented recovery from a
  panicking task into a dead mailbox (B6).
- `lto = "fat"` / `codegen-units = 1`: the binary is mostly ORT's C++, which
  Rust LTO cannot touch; it would lengthen the release builds that are the
  bottleneck (B6).
- jemalloc: measured and rejected in `mem.rs`; mimalloc is untested, and the
  shared embed service removes the problem (B7).
- int8 or quantized embedding weights: `fastembed_impl.rs:177-200` pins fp32
  with numbers (O7).
- Narrowing `tokio` features, trimming chrono-tz or psl, dropping `ammonia` or
  `full_encoding`: all used, several load-bearing for correctness (B13-14).
- Caching the bearer lookup: `token revoke` must bite on the next request;
  the cost only shows through H1 and H3, which are the real fixes (H10).
- Another Gmail rate limiter: `GmailBudget` already is one.
- A Litestream sidecar per tenant: rejected in its README for credential
  blast-radius reasons.
- `railway up`: see `CLAUDE.md`.
