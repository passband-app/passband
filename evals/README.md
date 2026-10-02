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
                 [--sample N --seed S] [--fidelity exact|reconstructed|all]
                 [--case ID ...] [--effort E] [--max-usd X] [--yes]
squelch-eval report [RUN ...] [--dataset DIR] [--baseline RUN] [--diff RUN]
                    [--cache-hit F] [--md FILE] [--allow-repo-path] [--seed S]
squelch-eval adjudicate <case-id> --field F --verdict candidate|gold|either --run RUN
```

`--dataset` defaults to the newest dataset under the root. A typical loop:

1. `export`, then `sample --per-stratum 5` and read it.
2. `run --candidate production --sample 300 --seed 1 --max-usd 20 --yes`:
   the noise floor (the incumbent against its own past answers).
3. `run --candidate <cheaper> --sample 300 --seed 1 --max-usd 10 --yes`: the
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
absent when it does not apply. A failed run scores 0 on everything that
applies, and harm 10.

| metric | agrees when | applies |
|---|---|---|
| `fye` | `show_in_fye` matches | always |
| `reading` | Reading membership matches | always |
| `auth` | `is_auth()` matches | always |
| `auth_kinds` | the auth kind sets match | gold is auth |
| `restricted` | `external_access.restricted` matches | always |
| `push` | `is_auth or importance >= 50` matches | always |
| `kinds` | Jaccard of the kind sets | always |
| `kinds_exact` | the kind sets match | always |
| `state` | `attention.state` matches | always |
| `actions` | the multiset of unresolved action kinds matches | either side has one |
| `records` | the multiset of record kinds matches | gold has records |
| `record_fields` | share of key facts that match (bill amount and due, receipt amount, tracking number and status, event start, institution) | gold has records |
| `reading_kind` | matches | both Reading and gold has one |
| `revisit` | presence matches | always |
| `rank_delta` | lower is better: the difference in the model-set part of the FYE rank | both in FYE |

`harm` is one weighted cost per case: FYE miss 10 (needs_user) or 5, FYE
false alarm 2, restricted miss 10, false restriction 1, auth miss 5, push
miss 3, false push 2, Reading mismatch 1, record kinds 2, record facts up to
1, kinds up to 0.5, model failure 10. The table lives in `score.rs`.

## Reading the report

The first three lines say which dataset, what cache hit share the cost
projection assumes and where it came from, and that gold is one sample from
the incumbent model. Then:

- **Cost and behavior.** The `production` row is what the mailbox costs
  today: the gold runs on the current gold model, priced at that model.
  Observed cost is per scored email. Projected cost reprices each call's
  cached prefix at the production cache hit share, because a short eval run
  pays cold-cache writes that production mostly does not. Retries after
  infrastructure failures are shown as "wasted on retries" and included in
  the run total, but not in the per-email cost. A run with no recorded input
  tokens, or a model with no price, shows no cost rather than zero.
- **Agreement.** Weighted means with 95% bootstrap intervals over cases
  (reps averaged first). Sampled runs are weighted back to the population.
- **Directional.** Recall, precision and false-positive rate on each positive
  class, with raw miss and false-alarm counts. A buried FYE item and a
  credential left unrestricted come first. Failed runs count as negatives.
- **Versus baseline.** Paired differences on the cases both runs scored; `*`
  marks an interval that excludes zero.
- **By stratum and fidelity.** The headline metrics per group.
- **Disagreements** (`--diff RUN`). Every case where the run and gold differ
  on fye, restricted, auth, push, reading or records, highest harm first,
  with both reasons and the trace path. This is where to decide whether the
  candidate or the gold is wrong. `--md` writes the same text to a file; it
  names senders and subjects.

`report` always re-scores from the stored decisions. The `grade` written on
each row is a convenience for other tools, tagged with `scorer_version`.

## Adjudication

`adjudicate <case> --field F --verdict candidate|gold|either --run RUN`
records the run's value on that field. `report` then counts that value as
agreement for every run that gave it. For yes/no fields this replaces gold
for that case, so the metric, the directional counts and harm all follow. A
later `gold` ruling on the same value revokes it. `record_fields` cannot be
ruled on.

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
  paired, and adjudicate.
- The served model is not asserted: the transport does not surface the
  response's `model`.
- Tool reads take the store's newest N results (`read_thread`,
  `search_mail`, `read_sender_history`) before the as-of filter, so a
  candidate can see fewer results than production did at the time.
- Old gold rows came from earlier prompt versions; `gold_meta.prompt_version`
  says which.
- Runs use `AgentTriageConfig::default()`, not the daemon's live
  configuration. Only the model and effort come from the candidate.
- Thread attention is rebuilt from each message's current decision. A
  message triaged twice loses its first decision (those cases are suspect).
  Action ids are the ones production assigned where they can be recovered
  (from the committed attention for a thread's latest decision, and from the
  ids the next decision reuses for earlier ones); an action no later
  decision reuses keeps a stand-in id, `act-<message>-<index>`.
