# Triage model comparison, October 2026

Tested 2026-10-01 (Claude models) and 2026-10-02 (open-weight models on
Fireworks) with `squelch-eval` (see `../README.md`) against one development
mailbox.

**Verdict.** Claude Opus 5.5 at effort `low` is the only change with no
measurable loss: 37% cheaper and twice as fast as the production config
(Opus 5.5 at `xhigh`). Claude Sonnet 5.5 is 69% cheaper but reads the
sealing rule more literally and buries more FYE items; it is one prompt
iteration away from a fair re-test. No open-weight model on Fireworks beats
Sonnet, and Kimi K3, the only one in the conversation, is not cheaper than
Opus at `low` as served today. Haiku 4.5, gpt-oss-120b and MiniMax M3 cannot
satisfy the output contract often enough to be usable at all.

Numbers below are weighted back to the mailbox's class mix, so a sampled
run reads as if it had covered the whole dataset.

## 1. Dataset

- Every stored agent decision since the 2026-09-20 cutover with exactly one
  applied triage run: 1,109 cases out of 1,158 decisions (44 messages were
  triaged more than once and their first decision is gone; 8 were mail the
  mailbox owner sent; 1 had no stored decision).
- Fidelity: 1,004 exact, 102 reconstructed (threads), 3 suspect (excluded).
- Gold distribution: 97 FYE items, 35 auth mails, 13 restricted, 53 pushes,
  331 with records, 678 Reading. The majority class ("promotional, Reading,
  nothing to do") covers about 60% of mail.
- Gold provenance by prompt version: v1 842, v3 142, v4 116, v5 8. Gold
  models: claude-opus-5 984, claude-opus-5-5 124.
- Sample used for every run: `--sample 150 --floor 100 --seed 1`, 393 cases.
  Every FYE, auth, restricted and correspondence case, 100 records cases and
  100 Reading cases; the rest weighted back to the population.
- Reference for the headline comparison: a fresh run of the production
  config (`claude-opus-5-5`, effort `xhigh`) on the same cases, scored ok
  on 391 of them. See section 6 for why not the stored gold.

## 2. Cost and behavior

Per email, from the provider's reported usage. "Projected" reprices each
call's cached prefix at the mailbox's measured 96.3% cache hit share, so a
short eval run that paid cold-cache writes reads like production.

| Run | Projected $ per 1k emails | Change vs production | Turns per email | Latency p50 / p95 | Uncached input | Output | Cache read | Cache hit share |
|---|---|---|---|---|---|---|---|---|
| Production ledger (125 gold runs, Opus 5.5 `xhigh`) | 36.99 | reference | 1.08 | - | 2,723 | 1,148 | 6,182 | 96% |
| Opus 5.5 `xhigh` (re-run) | 37.99 | +3% | 1.08 | 11.8 s / 27.6 s | 3,122 | 1,111 | 6,701 | eval-hot |
| Opus 5.5 `low` | 23.79 | -37% | 1.05 | 6.2 s / 15.1 s | 2,230 | 584 | 6,548 | eval-hot |
| Sonnet 5.5 `medium` | 12.04 | -68% | 1.02 | 4.0 s / 6.2 s | 2,208 | 546 | 6,237 | eval-hot |
| Sonnet 5.5 `low` | 11.94 | -69% | 1.04 | 4.0 s / 6.0 s | 2,181 | 539 | 6,436 | eval-hot |
| Haiku 4.5 | 20.53 | -46% | 3.94 | 20.7 s / 30.6 s | 7,210 | 2,070 | 17,371 | eval-hot |
| Kimi K3 `low` (Fireworks) | 27.30 | -28% | 1.35 | 13.5 s / 28.1 s | 6,422 | 535 | 23 | 0% |
| GLM 5.3 `low` (Fireworks US) | 14.75 | -61% | 2.20 | 21.3 s / 56.2 s | 2,864 | 1,061 | 3,261 | 57% |
| Qwen 3.8 Max `low` (Fireworks, 246 cases) | 33.90 | -11% | 2.78 | 39.2 s / 90 s | 7,940 | 3,003 | 8 | 0% |
| DeepSeek V4.1 Flash (Fireworks US) | 4.31 | -89% | 1.29 | 8.0 s / 33.0 s | 2,331 | 1,690 | 5,107 | 66% |
| MiniMax M3 (Fireworks) | 3.25 | -91% | 3.07 | 25.4 s / 38.1 s | 3,915 | 1,416 | 4,743 | 57% |
| GLM 5.3 Flash `low` (Fireworks US) | 1.75 | -95% | 1.79 | 8.8 s / 27.4 s | 4,922 | 856 | 70 | 1% |
| gpt-oss-120b `low` (Fireworks) | 1.93 | -95% | 3.38 | 6.6 s / 10.5 s | 6,510 | 1,119 | 10,906 | 65% |

Notes on cost:

- Output tokens are the largest line on the Opus bill (about two thirds at
  `xhigh`). Effort `low` halves them; that is the whole saving.
- Fireworks' automatic prompt cache never engaged on Kimi K3, Qwen 3.8 Max
  or GLM 5.3 Flash (0 to 1% cache reads) while GLM 5.3, DeepSeek and
  gpt-oss got 57 to 66%. With a working cache Kimi would land near $14 per
  1k. Cause unknown; it looked per-model.
- The cheap models' turn counts are repair turns: the agent spends them on
  decisions that fail validation (see section 5), and they are billed.
- "Fireworks US" means the US-only serverless endpoint
  (`us.api.fireworks.ai`, 1.5x the base price). GLM 5.3, GLM 5.3 Flash,
  DeepSeek V4.1 Flash and Kimi K3 have US-only routers; Qwen, MiniMax and
  gpt-oss do not and ran on the default endpoint, whose serving region
  Fireworks does not state. The default endpoint is acceptable for testing
  only; production would need the US routers.

## 3. Accuracy against today's Opus (`report --reference production@xhigh`)

391 cases. Failed runs count as "not shown", "not sealed", "not pushed",
which is what they would be in production.

### Headline misses

| Model | Failed cases | FYE buried (of 93) | False FYE | Credentials unsealed (of 15) | False pushes (49 real) | False Reading (95 real) | Harm per email |
|---|---|---|---|---|---|---|---|
| Opus 5.5 `low` | 0 | 4 | 14 | 1 | 11 | 4 | 0.34 |
| Sonnet 5.5 `medium` | 1 | 14 | 6 | 7 | 9 | 17 | 0.65 |
| Sonnet 5.5 `low` | 2 | 16 | 5 | 7 | 9 | 16 | 0.62 |
| Kimi K3 `low` | 0 | 16 | 18 | 6 | 24 | 16 | 0.86 |
| DeepSeek V4.1 Flash | 3 | 26 | 16 | 6 | 29 | 34 | 1.08 |
| GLM 5.3 Flash `low` | 14 | 29 | 34 | 8 | 73 | 137 | 2.86 |
| GLM 5.3 `low` | 51 | 19 | 55 | 7 | 55 | 194 | 2.92 |
| Qwen 3.8 Max `low` (246 cases) | 45 | 8 of 44 | 21 | 4 of 9 | 33 | 74 | 3.24 |
| MiniMax M3 | 162 | 54 | 29 | 8 | 49 | 63 | 6.03 |
| gpt-oss-120b `low` | 177 | 70 | 4 | 12 | 20 | 152 | 6.25 |
| Haiku 4.5 | 356 | 90 | 2 | 14 | 42 | - | 9.83 |

Harm is the eval's weighted cost per email (FYE miss 10 or 5, unsealed
credential 10, failure 10, false push 2, and so on; table in
`squelch-eval/src/score.rs`).

### Field agreement (weighted mean, 95% bootstrap CI in the reports)

| Metric | Opus `low` | Sonnet `medium` | Sonnet `low` | Kimi K3 | DeepSeek Flash | GLM 5.3 |
|---|---|---|---|---|---|---|
| fye | 0.979 | 0.968 | 0.969 | 0.948 | 0.936 | 0.770 |
| reading | 0.975 | 0.950 | 0.948 | - | 0.937 | - |
| kinds_exact | 0.913 | 0.837 | 0.844 | 0.718 | 0.787 | 0.438 |
| state | 0.912 | 0.856 | 0.850 | 0.818 | 0.812 | 0.390 |
| auth | 0.996 | 0.993 | 0.994 | 0.993 | 0.992 | 0.803 |
| restricted | 0.999 | 0.993 | 0.992 | 0.993 | 0.992 | 0.854 |
| push (threshold) | 0.987 | 0.980 | 0.982 | 0.963 | 0.953 | 0.765 |
| text_safe | 1.000 | 0.999 | 0.998 | 0.995 | 0.981 | 0.854 |
| factors | 0.978 | 0.962 | 0.960 | 0.943 | 0.939 | 0.782 |
| records | 0.937 | 0.919 | 0.913 | 0.895 | 0.838 | 0.583 |
| record_fields | 0.887 | 0.868 | 0.858 | 0.855 | 0.843 | 0.614 |
| summary_sim | 0.915 | 0.879 | 0.871 | 0.899 | 0.908 | 0.726 |
| push_text_sim | 0.907 | 0.827 | 0.809 | 0.870 | 0.868 | 0.709 |
| evidence | 0.969 | 0.962 | 0.960 | 0.970 | 0.967 | 0.832 |

Attention factor error (mean absolute, lower is better): Opus `low` 0.02 to
0.03 on all four; Sonnet 0.03 to 0.06; Kimi 0.04 to 0.07; DeepSeek 0.05 to
0.08; GLM 5.3 0.06 to 0.13; Haiku 0.17 to 0.30.

Text style: the Claude models wrote no em dashes; every open model did, in
4% to 15% of user-facing text (Qwen 14.5%). Length relative to the gold
text: Opus `low` 0.61, Sonnet 0.53 to 0.56, Kimi 0.77, DeepSeek 1.03.

## 4. Per-model notes

**Opus 5.5 at `low`.** No placement or security metric is worse than the
`xhigh` run within the paired confidence intervals; several are slightly
better. It shows 14 items `xhigh` does not and buries 4 it does. Text is a
third shorter and reasons diverge more (`reason_sim` -0.05). Half the
latency. One config change (`[stage2] effort = "low"`, or
`SQUELCH_STAGE2_EFFORT=low`).

**Sonnet 5.5 (`medium` and `low` behave alike).** Matches Opus on kinds,
Reading, auth detection and the push threshold. Two real gaps. It leaves
unsealed 7 of the 15 messages Opus restricts: all are non-account access
secrets (a short-term rental door code, patient survey PINs, insurance
claim document upload links, a photo gallery link); Sonnet reads "actionable
authentication credentials" literally and says these are not account
credentials. None is an OTP, reset link or sign-in link. The stored gold is
itself inconsistent on the claim links. It also buries more: six items shown
by gold, production and Opus `low` were hidden by both Sonnet runs,
including a returned ACH transaction and a TestFlight approval; one of its
"misses" is a marketing "past due" mail where Sonnet is arguably right.
Push and summary text is terser and scores lower on similarity. A prompt
line defining non-account access secrets, then a re-run, is the next step.

**Haiku 4.5.** 358 of 393 failed with `agent_turn_budget_exhausted`. Traced
through a logging proxy: it calls the tool and its judgment reads fine, but
its decisions fail structural validation (`access_requires_evidence`,
`invalid_step_shape`) and it cannot repair them in four turns from the terse
error codes. The retries make it cost more than Sonnet.

**Kimi K3 at `low` (Fireworks).** The only open-weight model near the
Claude tier: between Sonnet and DeepSeek on every axis, with double the
false pushes. Costs more than Opus `low` as served, entirely because the
prompt cache never hit. One turn on 297 of 393 cases, zero failures.

**DeepSeek V4.1 Flash (Fireworks US).** Cheapest usable model. Disciplined
(few false FYE) but buries a quarter of what Opus shows, pushes 29 false
alarms against 49 real, and tripped `text_safe` on about 2% of cases (URL
or digit run in user-facing text; not yet inspected).

**GLM 5.3 and GLM 5.3 Flash (Fireworks US).** At the default reasoning both
think to the 8,000-token output ceiling and time out (10 of 12 and 6 of 26
in the smoke test). With `reasoning_effort: low` they run, but GLM 5.3
still failed 51 cases (43 turn-budget, 8 timeouts), over-pushes five-fold
and files almost everything under Reading (194 false of 95 real). GLM 5.3
Flash is the cheapest model tested and the same shape, worse.

**Qwen 3.8 Max at `low` (Fireworks).** Slow (39 s median, 90 s p95 at the
timeout), 2.78 turns per email, 45 of 246 failed, hit its spend cap at case
246. Pushed 33 false against 14 real on the cases it finished.

**gpt-oss-120b and MiniMax M3 (Fireworks).** 177 and 162 failures: the
same output-contract failure as Haiku. Not usable with the current schema
and repair loop.

## 5. What the harness itself taught

- **Gold has prompt-era drift.** 842 of 1,109 gold rows came from the first
  agent prompt on Opus 5, which also filed correspondence under Reading (32
  of 55 FYE golds); prompts v3 and later almost never do (5 of 43). Every
  run on today's prompt, the production config included, "disagrees" with
  those rows. Scoring against a fresh production run (`--reference`) is the
  fair comparison; the gold-based report is still useful for the
  production row and for adjudication.
- **A constant answer scores 91% FYE agreement.** The null model
  ("promotional, Reading, nothing to do") agreed with gold on `show_in_fye`
  91% of the time while burying all 97 FYE items. Read recall and misses per
  class, never accuracy.
- **The step schema and evidence rules are a wall for smaller models.**
  Haiku, gpt-oss-120b, MiniMax and partly GLM lose most cases to validation,
  not judgment. If a smaller model is ever wanted, the repair messages
  (currently bare error codes) and the evidence requirements are the lever,
  not the model.
- **Open-weight reasoning models need `reasoning_effort`.** The OpenAI wire
  never carried one. `agent.openai_reasoning_effort` (off in production, on
  in the eval) forwards the candidate's effort; without it GLM 5.3 burns the
  whole output budget thinking.
- **Opus itself writes em dashes** in about 2% of summaries and push bodies
  despite the no-em-dash copy rule; the prompt never forbids them in output.
- **A near-empty email still bills about 670 uncached input tokens** of
  JSON envelope, including two SHA-256 fingerprints per message the model has
  no use for. Independent of model choice.

## 6. Method and limits

- Replay goes through the production `run_agent` with the production prompt
  (`agent-triage-v5`), schema and `AgentTriageConfig::default()`; only the
  model, effort and endpoint come from the candidate. Context is rebuilt as
  of each gold run (`evals/README.md`, "The as-of reconstruction").
- One rep per run. No self-agreement floor was measured (`run --reps 2` on
  the production config would give it), so differences of a few FYE items
  between Opus `low` and `xhigh` are within an unknown noise band.
- Free text is scored by cosine similarity of local bge-small embeddings
  against the reference wording. It measures "says what Opus said", not
  quality; read it relative to the production run and with `--texts`.
- Gold has no extracted login codes (35 auth mails, 0 codes), so login code
  extraction could not be evaluated; the Claude runs each extracted two to
  four, unverified.
- Tool reads during replay take the store's newest N results before the
  as-of filter, so a tool-using candidate can see fewer results than
  production did at the time. 97% of production runs use no tools.
- Prices: Anthropic list prices; Fireworks prices from
  docs.fireworks.ai/serverless/pricing on 2026-10-01 (US-only variants
  where used). Cache-write pricing assumes the 1-hour TTL.

## 7. Reproducing

Candidates are defined in `~/.local/share/squelch/evals/candidates.toml`
(not in the repo). The runs:

```sh
squelch-eval export
squelch-eval run --candidate production --sample 150 --floor 100 --seed 1 --max-usd 20 --yes
squelch-eval run --candidate <other> --sample 150 --floor 100 --seed 1 --max-usd <cap> --yes
squelch-eval report --baseline production@xhigh
squelch-eval report --reference production@xhigh --diff <other> --texts 15
```

The three saved reports (`report-vs-gold.md`, `report-vs-production.md`,
`report-fireworks-vs-production.md`) sit beside the dataset, outside the
repo, because they name senders and subjects.

## 8. Addendum, 2026-10-05: prompt v6 experiment (not shipped)

Branch `sealing-prompt` holds a prompt v6 that (a) widens the sealing rule
from "authentication credentials" to any secret or link that grants access
without a login, naming door and Wi-Fi codes, PINs, document upload links
and private file or gallery links, (b) spells out what a meaningful
development is for FYE, and (c) asks for user-facing text without em
dashes. Sonnet 5.5 `medium` and Opus 5.5 `low` were re-run on the same 393
cases and scored against the v5 `xhigh` reference.

| | Sonnet v5 | Sonnet v6 | Opus `low` v5 | Opus `low` v6 |
|---|---|---|---|---|
| Credentials unsealed (of 15) | 7 | 0 | 1 | 0 |
| Sealed that the reference left open | 0 | 6 | 0 | 38 |
| FYE buried (of 93) | 14 | 15 | 4 | 7 |
| False FYE | 6 | 10 | 14 | 15 |
| Push misses / false pushes (49 real) | 12 / 9 | 8 / 12 | 3 / 11 | 2 / 15 |
| Harm per email | 0.62 | 0.60 | 0.34 | 0.50 |
| Projected $ per 1k | 11.95 | 12.33 | 23.74 | 23.52 |

What it showed:

- The sealing line closed Sonnet's gap completely, so that gap was wording,
  not capability.
- The same line made Opus over-seal: 38 cases the reference leaves open,
  about 11% of mail when weighted, mostly receipts with tokenized invoice
  links, payment-update links, booking manage links and file-transfer
  links. Opus is following the rule as written. The rule is too broad, and
  it exposes that the sealing policy for login-free convenience links was
  never decided: the v5 reference seals claim upload links and a gallery
  link but not payment-update links, and the stored gold is inconsistent on
  the same sender.
- The FYE sentence did not move Sonnet (15 buried against 14).
- Em dashes were already at zero for both Claude models on this sample.

v6 should not ship as written. The next iteration needs a product decision
first: seal only secrets (codes, PINs, passwords of any kind, plus
password-reset and sign-in links), or also links that act without a login,
and if so which.

## 9. Open follow-ups

1. A second production rep for the noise floor.
2. Opus 5.5 at `medium`, untested.
3. Decide the sealing policy for login-free links (section 8), write
   prompt v7 to match, then re-run Sonnet 5.5, Opus `low` and Kimi K3.
4. Kimi K3 with a working prompt cache on Fireworks' US router, if a
   non-Anthropic fallback is wanted.
5. Repair messages that name the failing field and the fix, if a smaller
   model is ever to be tried again.
6. Drop the `source` fingerprints from the model-facing message JSON.
7. Record `effort` in run metadata so a mixed fleet can be told apart in
   the ledger.
