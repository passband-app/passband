# Triage evals

`squelch-eval` replays the owner's own mail through the production triage
agent (`run_agent`) with a candidate model and compares each decision to
the one the incumbent model made at the time. The goal is a cheaper model
or a lower effort with no loss on the fields that matter.

Gold is every triage decision stored since the 2026-09-20 cutover. The
owner treats those as correct, and `adjudicate` lets them say where they
are not.

## Where the data lives, and why not here

A dataset is personal mail. It lives under `~/.local/share/squelch/evals/`
(override with `--root` or `SQUELCH_EVAL_ROOT`). Directories are 0700 and
files are 0600. `export` and `report --md` refuse any path inside a git
working tree, so nothing from a dataset can end up one `git add` from a
commit. Nothing under `evals/` in this repository is data.

```
~/.local/share/squelch/evals/
  candidates.toml
  <dataset>/
    snapshot.db          copy of the daemon db (no HTML bodies, no vectors)
    cases.jsonl          one case per line
    manifest.json        counts, skips, audit, measured cache hit share
    adjudications.jsonl  the owner's rulings
    sample-*.md          review samples
    runs/<candidate>[@effort]/
      run.json results.jsonl errors.jsonl traces/<id>_rep<k>.json
```

## Commands

```sh
squelch-eval export [--db PATH] [--out DIR] [--name NAME]
squelch-eval show <case-id> [--dataset DIR]
squelch-eval sample --per-stratum N [--seed S] [--dataset DIR]
squelch-eval init
squelch-eval run --candidate NAME [--dataset DIR] [--reps R] [--concurrency C]
                 [--sample N --seed S --floor F] [--fidelity exact|reconstructed|all]
                 [--case ID ...] [--effort E] [--max-usd X] [--yes]
squelch-eval report [RUN ...] [--dataset DIR] [--baseline RUN] [--diff RUN]
                    [--reference RUN] [--texts N] [--no-text] [--cache-hit F] [--md FILE]
                    [--allow-repo-path] [--seed S]
squelch-eval adjudicate <case-id> --field F --verdict candidate|gold|either --run RUN
```

`--dataset` defaults to the newest dataset under the root. A typical loop:

1. `export`, then `sample --per-stratum 5` and read it.
2. `run --candidate production --sample 150 --floor 100 --seed 1 --max-usd 20 --yes`:
   the noise floor (the incumbent against its own past answers).
3. `run --candidate <cheaper> --sample 150 --floor 100 --seed 1 --max-usd 10 --yes`: the
   same cases, because a sample is fixed by (dataset, N, seed).
4. `report --baseline production --diff <cheaper>`, then `adjudicate` the
   disagreements where gold is wrong, then `report` again.

## The paid-call gate, the budget, and progress

`run` prints `N cases x R reps on <model> via <host>` and makes no request
without `--yes`. Keys are never written to disk: `candidates.toml` names the
environment variable that holds each one.

`--max-usd X` caps the run directory's observed spend: every call on every
graded row and every failed attempt, at the candidate's prices, including
earlier sessions in the same directory, so a resume gets no fresh allowance.
Once the cap is reached no new case starts, the cases already in flight
finish and are recorded, and `run` exits non-zero; raise the cap and run the
same command to continue. A model with no price cannot take a budget.

While it runs, `run` prints a line to stderr every 25 finished cases and at
least every 30 seconds: done/total, ok, failed, infra errors, spend so far
(when priced), elapsed time and an estimate of what is left.

## What a case is

One inbound message with exactly one applied triage run that recorded its
usage. Messages triaged more than once, sent or own mail, and spam are
skipped and counted by reason in the manifest.

| field | meaning |
|---|---|
| `id`, `thread_id` | the message |
| `as_of` | T, the gold run's completion time |
| `context` | the agent's input, rebuilt as of T |
| `gold`, `gold_meta` | the stored decision; the run's model, prompt version, turns, tool calls and per-call usage |
| `fidelity`, `fidelity_notes` | how faithful the rebuild is, and why |
| `tags` | `tags[0]` is the stratum: the first of fye, auth, restricted, push, records, correspondence, reading, other |

## The as-of reconstruction

The context a gold run saw is not stored, so `export` rebuilds it as of T
with the real `load_agent_context` and `snapshot()`, then rewinds:

- siblings: only messages ingested by T;
- thread attention: replayed from the stored decisions (and their related
  updates) decided before T, never the target's own; its revision is the
  count of those; action ids are recovered as described under known limits;
- sender rules, contacts and corrections: only those dated by T;
- user state: done only if resolved by T, open if surfaced by T, else new;
  opened and reminder stamps only if provably set by T.

During `run`, every tool read goes through the same rewind. A read of a
message that did not exist at T, or `read_record` of the target, returns
`evidence_unavailable`, and every field that carries a decision is replaced
by its as-of value, so no tool can return the target's gold.

Fidelity:

- `exact`: a lone message, no prior attention, nothing dropped by the rewind.
- `reconstructed`: rebuilt from timestamps.
- `suspect`: a message in the thread was triaged more than once (its earlier
  decision is lost), or the rebuilt prompt's size disagrees with the tokens
  the gold run was billed for (outside 0.5x to 2x of its prompt version's
  median bytes per token). `run` skips suspect cases unless `--fidelity all`.

`export` also checks that each gold decision is valid against its own
rebuilt context, the same check a candidate's answer gets on its first turn
(evidence cites messages the context holds, revisions match, a reused action
id is one the context offers). A gold whose run used no tools saw nothing
beyond its initial context, so failing that check proves the rebuild wrong:
the case is marked suspect with the error. A gold whose run used tools may
cite what a tool fetched; it gets a note and keeps its fidelity. The manifest
and the export summary count both (`gold_validation`).

It also audits the dataset: duplicates, label balance, prompt sizes, cases
over the context budget, and a leak check that fails the export if any
case's context contains its gold summary or reason.

## Metrics

Programmatic, per field, no LLM judge. Each is in [0, 1] (1 agrees) or
absent when it does not apply. A failed run scores 0 on everything gold
makes applicable, and harm 10. The report groups them as below.

| section | metric | agrees when | applies |
|---|---|---|---|
| Placement | `fye` | `show_in_fye` matches | always |
| | `reading` | Reading membership matches | always |
| | `reading_kind` | matches | both Reading and gold has one |
| | `kinds` | Jaccard of the kind sets | always |
| | `kinds_exact` | the kind sets match | always |
| | `state` | `attention.state` matches | always |
| | `revisit` | presence matches | always |
| | `revisit_at` | the two times are within 24 hours | both have a revisit |
| | `related_updates` | the sets of target threads match | either side has one |
| | `rule_exceptions` | the sets of rule ids match | either side has one |
| Security and push | `auth` | `is_auth()` matches | always |
| | `auth_kinds` | the auth kind sets match | gold is auth |
| | `restricted` | `external_access.restricted` matches | always |
| | `push` | `is_auth or importance >= 50` matches | always |
| | `importance` | 1 minus the importance gap over 100 | always |
| | `login_code` | both have one, same code exactly, same service ignoring case | either side has one |
| | `text_safe` | no URL, no own login code, and (on auth or restricted mail) no 4 to 8 digit run from the email that gold never wrote, in any text the candidate wrote | always |
| Attention detail | `factors` | 1 minus the mean absolute error of the four factors | always |
| | `attention_at` | both have one and the times agree | either side has one |
| | `relevant_messages` | Jaccard of the relevant message ids | always |
| | `actions` | the multiset of unresolved action kinds matches | either side has one |
| | `action_fields` | share of facts per gold action that agree: paired at all, resolved, due, and the carried-forward id | gold has actions |
| | `rank_delta` | lower is better: the difference in the model-set part of the FYE rank | both in FYE |
| Records | `records` | the multiset of record kinds matches | gold has records |
| | `record_fields` | share of exact facts that agree on paired records (bill merchant, amount, currency, due, autopay; receipt merchant, amount, currency; delivery carrier, tracking number, status, merchant, order refs; event start; institution) | gold has records |
| Text | `summary_sim` | similarity of `summary` | text similarity on |
| | `attention_summary_sim` | similarity of `attention.summary` | text similarity on |
| | `push_text_sim` | mean similarity of push title and body | text similarity on |
| | `action_text_sim` | mean over gold actions of the paired description's similarity (0 when unpaired) | gold has actions |
| | `record_text_sim` | mean over gold record text fields (item, title, location, description) of the paired similarity (0 when missing) | gold has one |
| | `reason_sim` | mean similarity of the reasons: decision, access, notification, revisit, rule exceptions | text similarity on |
| Grounding | `evidence` | Jaccard of every message id cited anywhere in the decision | always |

Pairing: actions pair by the same non-null id first, then by kind
(preferring a matching due), then order. Records pair within a kind on the
fact that identifies them (tracking number, amount, event start and title,
institution), ties by order; an unpaired gold record scores 0 on all of its
facts. Times agree when two dates are equal, two timestamps are within an
hour, or a date matches a timestamp's UTC date; text that is neither agrees
only with identical text. Merchant and institution names agree after
lowercasing, dropping punctuation and a trailing `com`, `inc`, `llc`, `ltd`
or `co`, when equal or one contains the other.

`harm` is one weighted cost per case: FYE miss 10 (needs_user) or 5, FYE
false alarm 2, restricted miss 10, false restriction 1, auth miss 5, push
miss 3, false push 2, Reading mismatch 1, record kinds 2, record facts up to
1, kinds up to 0.5, a URL or credential in text 10, a login code
disagreement 3, a gold action left unpaired 2 each (at most 4), model
failure 10. The table lives in `score.rs`.

Also shown, not scored: each attention factor's mean absolute error, and
text style (the share of cases whose user-facing text has an em dash, with
gold's own rate on the `production` row, and the median length against
gold's).

### Text similarity

The `*_sim` metrics are the cosine of two embeddings from the daemon's own
on-box model (the same weights the daemon uses for search), 1.0 when the
texts are equal once whitespace is normalized. `report` loads the model only
if its weights are already on this machine; it never downloads them. Vectors
are cached at `<dataset>/embeddings.jsonl` (0600), keyed by model and text;
a cache from another model, or a corrupt one, is thrown away. With no
weights, or with `--no-text`, the text metrics are omitted and the header
says why.

Read these relative to the `production` run, not as absolutes: they measure
closeness to the incumbent's wording, and two good summaries can share few
words. `--diff RUN --texts N` lists the N rows whose user-facing text is
least like gold's, side by side, to judge by reading.

### What the agent emits and how it is scored

Every leaf of the decision schema has an entry here (from `COVERAGE` in
`score.rs`; the first matching row wins, `*` matches any prefix and a
trailing `.*` any field below). A test walks the live schema and fails when
a field appears that this table does not name, so a new field cannot go
unscored by accident.

| schema path | scored by |
|---|---|
| `related_updates[].expected_revision` | not scored: a concurrency guard the executor checks against the store, not a judgement |
| `related_updates[].attention.*` | not scored: another thread's attention; scored as that thread's own decision, and `related_updates` checks which threads are touched |
| `*.evidence[].location` | not scored: free-text pointers into a message; the cited message ids are scored by `evidence` |
| `*.timezone` | not scored: display metadata; the time itself is compared by the time rule |
| `*.interpreted_relative` | not scored: how the model read the date, not what it decided; the date is compared |
| `*.source_message_id` | `evidence` |
| `*.evidence[].message_id` | `evidence` |
| `kinds` | `kinds`, `kinds_exact` |
| `destinations` | `reading` |
| `reading_kind` | `reading_kind` |
| `summary` | `summary_sim`, `text_safe` |
| `reason` | `reason_sim`, `text_safe` |
| `auth.kinds` | `auth`, `auth_kinds`, `push` |
| `external_access.restricted` | `restricted` |
| `external_access.reason` | `reason_sim`, `text_safe` |
| `attention.show_in_fye` | `fye` |
| `attention.state` | `state` |
| `attention.actions[].id` | `action_fields` |
| `attention.actions[].kind` | `actions`, `action_fields` |
| `attention.actions[].description` | `action_text_sim`, `text_safe` |
| `attention.actions[].resolved` | `actions`, `action_fields` |
| `attention.actions[].due.value` | `action_fields` |
| `attention.summary` | `attention_summary_sim`, `text_safe` |
| `attention.factors.urgency` | `factors`, `rank_delta` |
| `attention.factors.action_need` | `factors`, `rank_delta` |
| `attention.factors.personal_relevance` | `factors`, `rank_delta` |
| `attention.factors.importance` | `factors`, `rank_delta` |
| `attention.factors.attention_at.value` | `attention_at` |
| `attention.relevant_message_ids` | `relevant_messages` |
| `records[bill].kind` | `records` |
| `records[bill].merchant` | `record_fields` |
| `records[bill].amount` | `record_fields` |
| `records[bill].currency` | `record_fields` |
| `records[bill].due.value` | `record_fields` |
| `records[bill].autopay` | `record_fields` |
| `records[receipt].kind` | `records` |
| `records[receipt].merchant` | `record_fields` |
| `records[receipt].amount` | `record_fields` |
| `records[receipt].currency` | `record_fields` |
| `records[delivery].kind` | `records` |
| `records[delivery].carrier` | `record_fields` |
| `records[delivery].tracking_number` | `record_fields` |
| `records[delivery].status` | `record_fields` |
| `records[delivery].item_name` | `record_text_sim` |
| `records[delivery].merchant` | `record_fields` |
| `records[delivery].order_refs` | `record_fields` |
| `records[event].kind` | `records` |
| `records[event].title` | `record_text_sim` |
| `records[event].start.value` | `record_fields` |
| `records[event].location` | `record_text_sim` |
| `records[financial_update].kind` | `records` |
| `records[financial_update].institution` | `record_fields` |
| `records[financial_update].description` | `record_text_sim` |
| `related_updates[].thread_id` | `related_updates` |
| `rule_exceptions[].rule_id` | `rule_exceptions` |
| `rule_exceptions[].reason` | `reason_sim` |
| `notification.importance` | `importance`, `push` |
| `notification.title` | `push_text_sim`, `text_safe` |
| `notification.body` | `push_text_sim`, `text_safe` |
| `notification.reason` | `reason_sim`, `text_safe` |
| `notification.login_code.service` | `login_code` |
| `notification.login_code.code` | `login_code`, `text_safe` |
| `revisit.at` | `revisit`, `revisit_at` |
| `revisit.reason` | `reason_sim` |

## Reading the report

The first four lines say which dataset, what cache hit share the cost
projection assumes and where it came from, that gold is one sample from the
incumbent model, and how text similarity was measured (or why not). Then:

- **Cost and behavior.** The `production` row is what the mailbox costs
  today: the gold runs on the current gold model, priced at that model.
  Observed cost is per scored email. Projected cost reprices each call's
  cached prefix at the production cache hit share, because a short eval run
  pays cold-cache writes that production mostly does not. Retries after
  infrastructure failures are shown as "wasted on retries" and included in
  the run total, but not in the per-email cost. A run with no recorded input
  tokens, or a model with no price, shows no cost rather than zero.
- **Agreement.** Weighted means with 95% bootstrap intervals over cases
  (reps averaged first), in the sections above. Sampled runs are weighted
  back to the population. Then the per-factor errors and text style.
- **Directional.** Recall, precision and false-positive rate on each positive
  class, with raw miss and false-alarm counts. A buried FYE item and a
  credential left unrestricted come first. Failed runs count as negatives.
- **Versus baseline.** Paired differences on the cases both runs scored; `*`
  marks an interval that excludes zero.
- **By stratum and fidelity.** The headline metrics per group.
- **Disagreements** (`--diff RUN`). Every case where the run and gold differ
  on fye, restricted, auth, push, reading or records, highest harm first,
  with both reasons, every other metric below 1 for that case, gold and
  candidate summary and push text, and the trace path. This is where to
  decide whether the candidate or the gold is wrong. `--texts N` adds the
  least similar user-facing text. `--md` writes the same text to a file; it
  names senders and subjects.

`report` always re-scores from the stored decisions. The `grade` written on
each row is a convenience for other tools, tagged with `scorer_version`.

## Gold or today's model as the reference

Gold is the decision the incumbent made when the mail arrived, under the
prompt of that day. Prompts changed: early prompt versions put
correspondence and FYE mail in Reading far more often than today's does, so
every run on today's prompt, the production configuration included,
"disagrees" with those golds. Paired deltas against a `production` run
cancel that drift in aggregate, but `--diff` against old gold is full of it.

`report --reference RUN` scores every other run against RUN's own decisions
instead (rep 0, rows that finished ok). Use it to ask "does the candidate
decide what today's model decides": run the `production` candidate once,
then `report --reference production`. Cases the reference did not score ok
are left out for every run, and the header counts them. The reference
itself appears only in the cost table. Adjudications are rulings about
gold, so they are ignored under `--reference`. Everything else (metrics,
directional rates, harm, strata, `--diff`, `--texts`, `--baseline`) reads
the same, with the reference standing in for gold.

The reference is one sample too: a second run of the same model would not
score 1.0 against it. Run it with `--reps 2` and the report adds a
self-agreement row, its rep 1 scored against its rep 0 on the cases both
finished. That is the true noise floor (same model, prompt and input); a
candidate within it is as close to today's model as today's model is to
itself. With one rep the report says how to get it.

Use gold when the question is "is the mail filed right", and to read and
adjudicate. Use `--reference production` when the question is "can a
cheaper model replace this one".

## Adjudication

`adjudicate <case> --field F --verdict candidate|gold|either --run RUN`
records the run's value on that field. `report` then counts that value as
agreement for every run that gave it. For yes/no fields this replaces gold
for that case, so the metric, the directional counts and harm all follow. A
later `gold` ruling on the same value revokes it. Rulable fields: fye,
reading, auth, auth_kinds, restricted, push, kinds (covers kinds_exact),
state, actions, records, reading_kind, revisit, login_code, related_updates
and attention_at. The graded facts (`record_fields`, `action_fields`), the
numeric ones and the text metrics cannot be ruled on.

## Adding a candidate

`squelch-eval init` writes `candidates.toml` with commented examples:

```toml
[candidate.other-provider]
provider = "openai"            # any OpenAI-compatible chat completions API
model = "vendor-model-name"
url = "https://api.example.com/v1/chat/completions"
api_key_env = "EXAMPLE_API_KEY"
input = 0.50                   # USD per million tokens
output = 2.00
cache_read = 0.05
```

First-party Claude models have built-in prices. Any other model needs
`input` and `output`; cache reads default to 0.1x input and cache writes to
1.25x (5 minute) and 2x (1 hour). Leave `effort` out for a model without
effort support: sending it is a 400, and a 400 stops the run, because every
case would fail the same way. Cached prompt tokens reported by an OpenAI
compatible provider are counted as cache reads.

Failures sort into two kinds. Model failures (turn, tool or review budget
exhausted, refusal, timeout, context too large) are graded results.
Transport errors, 429s and 5xx are retried up to three times into
`errors.jsonl` and never graded. `context_changed_during_run` means the
harness disagrees with itself; it is reported loudly as a bug.

## Known limits

- Gold is one sample from the incumbent model, so agreement rewards
  imitating it. Use the `production` candidate as a noise floor, compare
  paired, and adjudicate. The text metrics inherit this most: they measure
  wording, so read them against `production` and with `--texts`.
- The served model is not asserted: the transport does not surface the
  response's `model`.
- Tool reads take the store's newest N results (`read_thread`,
  `search_mail`, `read_sender_history`) before the as-of filter, so a
  candidate can see fewer results than production did at the time.
- Old gold rows came from earlier prompt versions; `gold_meta.prompt_version`
  says which.
- Runs use `AgentTriageConfig::default()`, not the daemon's live
  configuration. Only the model and effort come from the candidate, with one
  exception: a candidate on an OpenAI-compatible provider has its `effort`
  sent as `reasoning_effort` (`agent.openai_reasoning_effort`, off in
  production). Several open-weight models think to the token ceiling
  without it; a model that does not take the field answers 400, so leave
  `effort` out for those.
- Thread attention is rebuilt from each message's current decision. A
  message triaged twice loses its first decision (those cases are suspect).
  Action ids are the ones production assigned where they can be recovered
  (from the committed attention for a thread's latest decision, and from the
  ids the next decision reuses for earlier ones); an action no later
  decision reuses keeps a stand-in id, `act-<message>-<index>`.
