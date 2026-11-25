# Sender research ("Who is this?")

When a stranger writes something that reads as genuinely personal ("I loved
your talk at X", "we met at Y"), the owner's first question is whether the
person is real. Sender research answers it on the contact card: one model call
with Anthropic's web search tool checks who the sender says they are, and the
card shows what it found.

Code: `crates/squelch-core/src/triage/research.rs` (the input, the call, the
output, the caps), `store/sqlite/sender_research.rs` (the table),
`sync/sender_research.rs` (the automatic round),
`crates/squelch-api/src/sender_research.rs` (the routes), and
the "Who is this?" section of `passband/Sources/Passband/Views/ContactCard.swift`.

## When it runs

- **Automatically**, for cold outreach, behind the hidden `research` tenant
  feature (`features::RESEARCH`, off by default; ticked per tenant on the
  admin page). When a triage decision on a FRESH ARRIVAL (the job's
  `arrival_eligible`; never a backfill or a revisit of old mail) commits with
  an `outreach` assessment that is `worth_research()` (shape `personal`,
  wanted `likely` or `unsure`, not `carried` from an earlier first contact;
  see `OutreachAssessment` in `triage/decision.rs`), the worker queues the
  sender. The refine lane runs at most one queued sender a round, the NEWEST
  first, gated like every other model spend there, under
  `[research] auto_daily_cap` (default 20) automatic runs per account per UTC
  day. A queued sender still waiting after three days is dropped unrun.
  Without the feature nothing is queued and the round does nothing.
- **On demand**, from the contact card's button, for any sender, feature or
  not: `POST /client/person/research?addr=`. It runs once and answers with the
  result; when an automatic run on that sender is already in flight it
  answers `running` at no cost and the card polls until it finishes. These
  count toward spend but never against the automatic cap; they have a runaway
  guard of their own, `on_demand_daily_cap` (default 50).

`GET /client/person/research?addr=` reads what is stored and never runs
anything.

## The cache

One row per (account, sender) in `sender_research`. A `done` result stands for
`cache_days` (default 90): inside that window neither trigger researches the
sender again, and the card shows the stored result. An `unavailable` or
`failed` result stands for a day against triage and for fifteen minutes
against the owner's click, so a transient failure does not cost the button for
a day. A run in flight blocks a second one (a second click pays nothing) until
it is fifteen minutes old. A queued row blocks a second automatic request but
not the owner, who asked now.

A row is marked `running` before its call, and every way out of a run records
an outcome on it: a store error after the claim finishes it `failed`
(`interrupted`) before the error is returned. A row a restart left `running`
is reported by `GET` as `none` once it is stale (as is a failure past its
retry window), so the card's spinner stops and the button comes back.

## What the call is told

Only structured fields about the sender, built by `ResearchSubject::new`: the
display name their mail carried, the address, its domain, and the role and
organization triage read from the pitch (`claimed_role`,
`claimed_organization`). Each is neutralized (`triage::text::neutralize`),
folded to one line and truncated, then sent as JSON inside an untrusted fence.

Never the email's body or subject, and never anything about the owner (their
name, address or company). A pitch is text its sender wrote, and a web search
is an action taken on the owner's behalf: nothing in the pitch beyond who it
claims to be from may shape what is searched. A test sends a pitch whose
subject and body carry canary words through the whole path and asserts none
reaches the request.

The owner's own address is never researched.

## What comes back

The model answers through a submission tool (`submit_sender_research`) rather
than a JSON body, because a request with a server tool cannot also ask for
strict structured output. The answer is a fixed shape:

- `who`: one short line on who the sender appears to be.
- `organization`: `name`, `website`, `what_it_does`, `size_hint`, each nullable.
- `checks`: each claim with `confirmed`, `contradicted` or `not_found`.
- `verdict`: `verified`, `plausible`, `unverified` or `suspicious`.
- `sources`: URLs.

`research::validate` holds it to its bounds whatever the model wrote: lengths,
at most eight checks and six sources, http(s) links only, and a source is kept
only when one of the call's own searches returned it (compared ignoring a
trailing slash). The organization's `website` is the model's to write, so it
is kept only when its host is the host of a URL a search returned, or the
sender's own domain (`www.` ignored); otherwise it is dropped.

`verified` is the model's word, and `validate` caps it at `plausible` unless
the call itself backs it: at least one search ran (`web_searches > 0`), at
least one source survived the filter, at least one check is `confirmed`, and
the sender's address is on the organization website's registrable domain
(`receipt_match::registrable_domain`; a freemail domain never matches). Even
then it means only that: the card labels it "org domain matches" and its help
text says it does not prove who wrote the mail.

## Trust boundaries

Everything research returns is UNTRUSTED. It is stored and shown to the human,
and nothing else:

- It is never read by triage and never placed in any prompt.
- It is never treated as an instruction.
- It never crosses to `/mcp`. Like the relationship ledger and the owner
  profile (docs/UNDERSTANDING.md §2.2), it is human door only;
  `crates/squelch-mcp/tests/research_never_crosses.rs` walks every agent door tool
  over a mailbox whose research carries canary words, and checks the agent
  door's source names none of research's store calls.

## Provider and gateway

Research runs the investigation model (the agent's, else stage 2's), qualified
for a gateway exactly as the profile pass is, with the web search tool
(`[research] web_search_tool`, `max_uses` `max_searches`, default 5).

The default tool is `web_search_20250305`, the basic one. Every model and
every platform a gateway may route to takes it, it is eligible for zero data
retention, and each search is a direct call whose results arrive as top-level
`web_search_tool_result` blocks. `web_search_20260209` (and later) adds
dynamic filtering: the model runs its searches from a code execution container
the API provisions. Older models and some platforms reject it, and it is not
ZDR eligible. It can still be configured, and the parser reads its results,
which arrive in the same `web_search_tool_result` blocks with a `caller`
field beside the code execution's own blocks; an error result (an object, not
a list) contributes no URLs. Either version can end a call with `pause_turn`
when the server-side loop pauses; it is not resumed (one bounded call is the
whole budget) and is recorded `failed`.

On the OpenAI wire there is no such tool: research is recorded `unavailable`
without a request. When the endpoint refuses the request (a 4xx such as a
gateway that does not pass the tool, or a key or model it does not admit), the
row is `unavailable` too, and the refusal is remembered for the rest of the UTC
day (the `__sender_research_refused__` budget sentinel): every later sender
that day, from either trigger, is recorded `unavailable` without a request and
without spending a cap unit. Nothing else fails either way.

## Spend

Usage is billed to the `research` category of the usage ledger, priced at the
investigation model's rates (`metrics::runs_investigation_model`), so it shows
on `/client/usage` beside the other lines. The provider also bills web searches
per search; the count is kept on each row (`web_searches`), not in the token
ledger. Hosted tenants call through the gateway, whose per-tenant budget covers
research like every other call.

## Config

`[research]` in the config file, no env var: `enabled`, `auto_daily_cap`,
`on_demand_daily_cap`, `cache_days`, `max_searches`, `web_search_tool`,
`effort` (default `low`). `enabled = false` turns off both triggers; the
`research` feature gates only the automatic one.

## Checking it live

The tests run against a loopback provider. A live check needs a configured
Anthropic key (or a gateway that passes the web search tool), a mailbox with
the `research` feature ticked, and one invented cold pitch: confirm the row
goes `done`, `web_searches` is nonzero, the sources are pages the search
returned, and `/client/usage` shows a `research` line.
