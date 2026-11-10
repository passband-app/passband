# Auto-translate

Mail written in a language other than English is translated to English
automatically. The reader shows the English with a header saying it was
translated, and one click shows the original. Both versions are indexed for
keyword search and semantic search.

Design decisions:

- Model: **`claude-sonnet-5`**. It is already on every tenant's Bifrost
  allow-list (`DEFAULT_LLM_MODELS` in `squelch-control/src/config.rs`), so
  shipping needs no `llm sync`/`mint`.
- Rendering: **HTML text-node translation.** The visible text segments are
  pulled out of the already-sanitized `body_html`, translated as a numbered
  array, and put back as escaped text. The model never sees or produces
  markup: inline formatting inside a sentence (`Bon<b>jour</b>`) reaches it
  as numbered markers (`Bon⟦1⟧jour⟦/1⟧`), and an answer whose markers do not
  round-trip keeps its text and loses only that formatting.
- Scope: **all live incoming mail, eagerly. Archive-tier mail is translated
  only when the reader displays it** (see Pipeline). No backfill of mail
  that is already stored.
- No new env vars and no warden knobs. The feature is ON by default and takes
  effect when the daemon version rolls out. A config-file section with
  defaults is fine.

## Invariants

1. **`messages.body`, `messages.subject` and `messages.body_html` are never
   rewritten.** A translation lives only in `message_translations`.
   **The served translation IS part of the agent triage `content_snapshot`**
   (`store/sqlite/agent_triage.rs`), hashed in only when one exists, so mail
   without a translation keeps the snapshot it always had. The agent door
   serves the translation beside the body, so the access assessment has to
   have read it: a translation that lands (or is dropped) after triage
   re-triages the message once and resets its external access to `pending`
   until then. See Pipeline for why that is normally zero extra runs.
2. **Every fact is carried over.** The prompt demands a complete, faithful
   translation: no summarizing, no omissions, no additions, and numbers,
   amounts, dates, codes, names, URLs and addresses kept exactly. The code then
   verifies this (`translate/facts.rs`):
   - The output array has the same length as the input array, one translation
     per segment.
   - A TWO-WAY fact check over each batch. Both sides are normalized first
     (fullwidth forms and every script's decimal digits become ASCII). Every
     fact has a set of keys, the forms a faithful translation may write it
     in, and two facts are the same fact when they share one:
     - a number is its digits, separators and leading zeros dropped
       (`1.234,56` matches `1,234.56`, `01` matches `1`);
     - a number with a magnitude unit is its VALUE: `3万` is `30000`,
       `12만 5천` is `125000`, `1,5 Mio.` is `1500000`, and so are
       `千`/`천`, `百`/`백`, `億`/`억`, `Mrd.`, `Million(en)`, `Milliarde(n)`,
       `тыс.`, `млн`, `млрд`, `lakh`, `crore`, `mil`, and on the English side
       `thousand`, `million`, `billion`, `k`. A source magnitude is matched
       only by its value (`30,000`, or a translated magnitude such as `30
       thousand`, `1.5 million`, `2 billion`, which carries it): its bare
       digits alone (`3 yen` for `3万円`, `Amount: 2` for `2 milliards`)
       dropped the magnitude, so the fact is missing and the row ends
       `incomplete`. The bare digits still explain an addition. A compound
       amount whose value overflows has no derived value and is checked as
       plain numbers;
     - a time is its (hour, minute): `18 Uhr`, `18h`, `14 h 30`, `14h30`,
       `14.30 Uhr`, `14時30分`, `午後3時` match `18:00`, `6 PM`, `2:30 p.m.`,
       with an hour of 12 or less and no a.m./p.m. read as either half of the
       day. A source CJK CLOCK time (`18時`, `午後3時`, `3시`) is not
       satisfied by its bare digits (`on the 18th`); a Western `18 Uhr`,
       `3 heures` or `18h` still is, because `heures` and `h` are durations
       too. `3時間` / `3시간` (three hours) are plain numbers, and a `点`
       time keeps its digits (`点` is as often a count: `商品3点`);
     - a dotted, slashed or ISO date (`21.10.2026`, `21/10/2026`,
       `2026-10-21`) is its day, month and year, never one joined number, and
       a month (as a CJK `10月`/`10월` is) may be written as the English month
       name;
     - a URL, an email address (case-insensitively) and a code-like token
       (5 or more ASCII letters and digits with at least one of each,
       compared case-insensitively) are kept verbatim.
   - MISSING facts are what a maximum matching of source facts to
     translation facts over shared keys leaves unmatched. Facts with the
     same key set are interchangeable, so the matching runs over groups with
     their sizes as capacities: a batch of 3,000 `1`s against 1,500 takes
     milliseconds, not seconds.
   - ADDED facts are translation facts that share no key with anything the
     source says or implies. What it implies counts: kanji numerals (`三千`
     is 3000, `二〇二六` is 2026), Japanese era years (`令和8年` is 2026,
     `平成` counts from 1988), every magnitude's value, and a phone number
     the translation only prefixed with a country code. A number the source
     repeats is not new.
   - On a missing fact or a REJECT-CLASS addition the call is retried once,
     naming them. Reject-class is an added URL, email address, all-caps
     code, or number of seven digits or more (a phone number's length, its
     groups taken together: `+1 555 0100` is one number). If the retry still
     adds one, the row is terminal `rejected`.
   - Any other added number (4 to 6 digits, a magnitude, a time) is most
     likely a number word the source spelled out (`deux mille`, `dix-huit
     heures`), which the check does not try to parse in every language. It
     buys no retry (the retry would forbid the faithful digits): the row is
     served `incomplete` with the note `added numbers: …` naming them (at
     most eight). Numbers of 1 to 3 digits are let through: a translation
     writes `3` for `trois`.
   - If facts are still missing after the retry, the row is stored with
     `status='incomplete'`, its `missing_facts` naming them, and the client
     warns about it.
   - The regression corpus (`facts/tests.rs`,
     `faithful_idioms_are_never_rejected`) is 59 faithful translations of
     everyday idioms: none is rejected, and the only six `incomplete` ones
     are number words and a Roman numeral. CJK prices, dates and times and
     German dates and times all pass outright.
3. **Email text is untrusted.** Segments are fenced the same way the other
   prompts fence email content (`triage::text`). Translated strings are
   inserted as escaped text nodes, never parsed as HTML. The reassembled
   document goes through `sanitize_email_html` again.
4. **Spam never reaches a model**, as everywhere else. **The account's own
   mail is never translated**, because the user wrote it: not only stored
   sent mail but anything passing the same authorship test the human thread
   view uses (`own_mail_sql!`: `is_sent`, or From is the account address),
   which covers self-addressed notes and group echoes. Checked at ingest and
   again at every claim. Archive-tier rows spend nothing
   until the reader displaying the thread asks for them. Reading the thread
   is not asking: the client prefetches threads through the same GET.
5. **External doors (MCP, agent HTTP) see a translation exactly when they may
   see the body.** The translation is joined inside the same guarded query.
   It is never served through a path the body could not take.
6. Spend respects `refine_can_spend()` and is billed to usage category
   **`translate`** in `stage2_usage`. It is priced at the actual Sonnet 5
   rates (`[translate].price_in_per_mtok`/`price_out_per_mtok`, 2.0/10.0) in
   `metrics.rs` `estimate_cost_usd` and the `/client/usage` estimator.
   Without that it would fall through to the Opus arm. Translation spends
   the tenant's TRIAGE key (the same `stage2.resolve_llm()`), so it has a
   dollar cap of its own (Pipeline, "Daily budget").

## Detection

- Detection runs locally with `whatlang`. It is a pure function over the
  original subject and body text, run at ingest. English mail creates no row
  and costs nothing.
- Detection needs at least 40 letters of text (12 in a non-Latin script,
  where English is not a candidate). The question asked is "is this English",
  not whatlang's `is_reliable()`, which rejected short personal mail (a
  two-sentence German note scored 0.61) because close neighbours such as
  Spanish and Portuguese compete for the top spot. whatlang's top guess must be
  non-English and must beat English with confidence of at least 0.6 on a
  detection restricted to the two of them. The MIME `Content-Language` header
  is a tiebreaker only, when English is still in play.
- A false positive corrects itself: the translator reports `source_language`,
  and a message it reads as English is closed as `not_needed` and served
  nowhere. When it names a different language than whatlang did, its answer
  is stored.
- Mixed mail, such as an English reply over a foreign quote, is judged on the
  text with quoted lines (`>`) removed.
- When the language is not English, ingest inserts a
  `message_translations` row with `status='pending'`, inside the same
  transaction. Archive rows get `status='deferred'`, and so does a
  backfilled message while the day's translate budget is spent.

## Languages I read

- The owner picks languages they read in Settings > Mail. The choice is
  stored per account in `app_settings` under `translate.read_languages` as a
  JSON array of ISO 639-1 codes. The store drops unknown codes and `en`. A
  value that does not parse reads as an empty list, so a bad value means
  translating too much, never too little.
- Mail detected in a language the owner reads still gets a row, inserted as
  `not_needed`, which serves nothing and spends nothing. The row exists so the
  choice can be undone.
- A row the translator reads as English is recorded with
  `source_lang='en'`. That keeps the two kinds of `not_needed` row
  distinguishable: one in a real language means the owner reads that
  language.
- Changing the set reconciles the queue in the same transaction:
  - Adding a language closes its unserved rows (`pending`, `failed`,
    `deferred`) as `not_needed`. That releases any triage hold, and a result
    still in flight writes nothing because its lease no longer matches.
  - Removing a language brings its `not_needed` rows back as `deferred`, with
    attempts reset, so that mail is translated when its thread is opened and
    not before.
  - Served translations (`done`, `incomplete`) are left alone. They are paid
    for and still offer the original.
- `GET /client/translate-config` returns `{active, read_languages, available:
  [{code, name}]}`. `available` lists every language the detector knows except
  English. `POST` with `{read_languages}` replaces the set and returns the
  same view.

## Storage (`schema.sql`, `CREATE TABLE IF NOT EXISTS`)

```sql
message_translations(
  message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
  account_id INTEGER NOT NULL,
  source_lang TEXT NOT NULL,            -- ISO 639-1, e.g. 'fr'
  status TEXT NOT NULL CHECK (status IN ('deferred','pending','done','incomplete','failed','not_needed','rejected')),
  subject TEXT, body TEXT, body_html TEXT, snippet TEXT,
  missing_facts TEXT,                   -- JSON array when incomplete
  model TEXT, prompt_version INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0,
  lease TEXT,                           -- the claim token, NULL when not claimed
  triage_wait_until INTEGER,            -- how long triage holds the message back
  releases INTEGER NOT NULL DEFAULT 0,  -- consecutive releases without a verdict
  first_released_at INTEGER,            -- unix ms of the run's first release (or
                                        -- last charge); NULL after any verdict
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)
```

- `body` is `segment::html_to_block_text(translated body_html)` when there
  is HTML: block elements keep their lines and paragraphs, quoted history is
  prefixed `> `, hidden text and code are dropped. (Ingest's `html_to_text`
  folds a whole mail into one line.) Otherwise it is the translated plain
  text, with paragraphs as segments.
- `snippet` is the first 200 characters of the translated body, whitespace
  collapsed.
- `missing_facts` is a JSON array: the source facts still missing, then
  the notes `added numbers: …` and `only the first part was translated`
  when they apply. Any entry makes the row `incomplete`.
- **A claim is a lease with a token.** It counts an attempt, stamps a fresh
  random `lease`, and every write of the result matches on it (`status =
  'pending' AND lease = ?`); every terminal write clears it. A heal that
  drops and re-queues the row, or a pass that re-takes an expired lease, owns
  the row from then on: a late result writes nothing, and the pass embeds no
  vector for a result that wrote nothing.
- `failed` rows (refusals, malformed answers) are retried with backoff (10
  minutes, doubling, capped at 12 hours) and at most 5 attempts. They never
  loop forever.
- **Nothing is left `pending` for good.** A lease that expired on the last
  attempt is settled `failed` by the next claim. A row whose message has
  since become spam, sealed or the account's own mail is closed (terminal
  `failed`, text cleared, a served translation dropped from the FTS columns
  and the vector table) AT THE EVENT that changed its verdict: the agent
  decision that seals it (`commit_agent_decision`, and a correction that
  rewrites a decision) and the ingest of the row. Spam and own mail move the
  agent snapshot with it; a seal does not (Sealed mail). The claim keeps
  only a cheap sweep of the rows it could claim (`pending` and retryable
  `failed`, over the claim index) for anything an event missed. It never
  scans the `deferred` or served backlog: that scan cost about 110 ms a
  claim at 100k deferred and 20k done rows, under the store lock, twice a
  minute; the claim now costs about 0.1 ms there.
- A failure that is not about the message is not charged to it: a
  configuration failure (400/401/402/403/404, a model missing from the
  gateway's allow-list) and a provider outage (5xx, 429, 529, a timeout or
  transport error) RELEASE the row, which goes back to exactly the status,
  `updated_at` (so a `failed` row keeps its backoff) and attempt count the
  claim found. The translate pass then cools down on its own clock,
  `triage.agent.outage_retry_secs` for a configuration failure and 120
  seconds for an outage, without touching triage's own circuit.
- **Unless the failure is the message's own.** Only a MESSAGE-SHAPED
  failure can be: a timeout or transport error on this row's call, or a 400
  its content draws. When another row of the same round got an answer, the
  gateway works, and such a row is charged an ordinary `failed` with backoff
  (no cooldown). And a row released twice in a row (`releases`, reset by any
  verdict) is charged on its third (`failed`, the attempt spent, backoff
  from now), but ONLY if something has answered the account since the
  row's first release (`first_released_at`), as two in-memory stamps
  decide. A TIMEOUT needs `gateway_answer_ms`: any translation call that
  reached an outcome other than a configuration failure or an outage, or a
  successful agent triage run or access assessment (the translator uses the
  same `[stage2]` gateway and key). A 400 needs `translate_answer_ms`: only
  the translate model's own answers count, because a 400 can be about the
  model (Bifrost unable to resolve a provider for it, a parameter it
  refuses), and triage succeeding on another model says nothing about
  that; a sustained model 400 beside a healthy triage spends no attempts.
  Otherwise the row keeps being released with no attempt spent. So a
  message whose batch times out every time on a healthy gateway runs out
  of its five attempts, even when it is the only translation queued,
  instead of being claimed and released forever (each abandoned generation
  is billed by the provider and recorded as a zero-token call). A total
  outage of any length (the 4-day allow-list outage of August 2026, say)
  leaves every row `pending` with its attempts untouched. A daemon restart forgets the last answer, which only delays a
  charge.
- **A capacity failure is never the message's.** A 429, 529, 503 or any
  other 5xx, and a key, budget or model failure (401, 402, 403, 404), always
  releases and cools down, however many other rows of the round succeeded,
  and the release cap never charges it: partial 429s beside successes are
  the normal shape of a rate limit on a key shared with triage.
- Configuration failures are counted in
  `squelchd_translate_config_failures_total`, once per row per run of
  releases. They are NOT in `squelchd_llm_config_failures_total`, which
  counts triage and notify only, so a translate model missing from an
  allow-list never reads as "triage is stalled" on the dashboard.
- `rejected` is terminal: the engine refused its own output (it could not
  verify that the translation carries the original). Never served, never
  retried.
- A translation the engine cut short (an over-long message, only its head
  translated) is stored `incomplete` with the note "only the first part was
  translated" among its `missing_facts`.

## Sealed mail

- Sealed means what the Auth page means (`sealed_messages` in
  `store/sqlite/messages.rs`): an agent decision naming an auth kind
  (`decision_json` `$.auth.kinds`), or a legacy `triage.sensitivity =
  'sealed'` row with no agent state. `triage.sensitivity` alone is a retired
  column and seals nothing; a person's restriction (`external_access`
  corrected) is an external-door matter and does not stop translation, since
  the translation is gated exactly like the body.
- Sealed mail is never claimed, is never queued by a re-sighting, and a row
  queued before the seal is closed at the next claim, its served text, FTS
  columns and vector dropped.
- **Accepted: foreign auth mail IS translated before triage seals it, then
  dropped.** Newly arrived live mail is translated first and triaged on the
  English (Pipeline), so every foreign OTP, password reset or login alert
  costs one translation, and its English sits in `message_translations`,
  the FTS `tbody` column and the translation vector until the decision that
  seals it lands. That decision closes the row in its own transaction and
  re-triages nothing: sealing is terminal, the stored snapshot is brought in
  line in place (no new revision, no job). That is within what
  docs/SECURITY.md §4 allows internal models to read; the translation is
  gated exactly like the body at both doors, and the original body is
  already in FTS and the vectors.

## Search

- **Keyword search.** `messages_fts` gains the columns `tsubject` and
  `tbody`. `rebuild_fts_if_stale` picks up the column change. Its refill
  SELECT and the test fixtures LEFT JOIN `message_translations`.
  `upsert_message_conn` writes the translation if one exists. When a
  translation lands, the FTS row is rewritten. The `bm25` weights extend to
  4.0 for `tsubject` and 1.0 for `tbody`. Snippet and highlight take the
  window from whichever body column matched.
- **Semantic search.** A sibling vec0 table, `message_translation_vecs`, holds
  the translated text embedded with `message_embed_text(tsubject, tbody)`.
  The KNN legs (human and external) take the better distance across both
  tables, per message. The original vector stays as it is.
- Archive-tier rows are searchable only in the original until the reader
  has asked for them and the translation has landed.

## Pipeline

- The **translate pass runs in its own lane** (`translate_lane`, beside the
  refine lane in `run_lanes`), so one long message never holds up triage of
  everything that arrives after it. The refine lane pokes it at the top of
  every round; it runs rounds back to back while it has work, and pokes the
  refine lane whenever translations land. A round claims at most 4 rows.
- **Translate, then triage.** Agent triage holds back the triage or access
  job of a message whose translation row is `pending` (claimed or not), for
  at most 10 minutes from when the row was queued (`triage_wait_until`).
  ONLY NEWLY ARRIVED LIVE MAIL holds (incremental sync, and an archive row
  the owner promotes): a backfill (onboarding) is triaged on the original at
  once and re-triaged when the English lands, so a foreign-heavy mailbox's
  first impression does not wait on a translation backlog. Nothing holds
  while the day's translate budget is spent.
  Normally the translation lands first, its snapshot change supersedes the
  waiting job with one at the new revision, and the message is triaged
  ONCE, on the English. A translation that lands after triage ran (the wait
  ran out: translator down, over its cap, cooling down) changes the
  snapshot and re-triages exactly once.
- No loops: `deferred`, `failed`, `not_needed` and `rejected` rows never hold
  triage back and never change the snapshot, and English mail has no row.
- Triage gets the translation as an extra field in the agent context
  (`translated_subject` and `translated_body` on the message). Its body is
  NOT replaced.
- **Daily budget:** `[translate].daily_budget_usd`, default **$0.50** per
  account per UTC day: at Sonnet 5's prices, roughly a hundred short
  messages a day, or twenty of 5,000 characters. Translation shares the
  tenant's triage key and its monthly gateway budget, and this keeps a
  foreign backlog from spending it. Each message's cost is ESTIMATED before
  its call (`translate::estimate_tokens`, about one token per dense-script
  character and a third per Latin one each way, plus the system prompt per
  call) and goes only if it fits what is left beside the round's others; a
  row that does not fit is handed back untouched. The day's spend is
  SETTLED from the usage the provider reported on the `translate` ledger
  line, priced at the translate prices. Over it, live rows stay `pending`
  until tomorrow without holding triage, and a backfilled message is queued
  `deferred` (translated when its reader opens it). The once-a-day log line
  says the budget is spent. No `app_settings` override, no env var.
- Every call is on the usage ledger, including one that reported no usage
  (a timeout), which is counted as a call with no tokens.
- An archive row seen again by live sync stays exactly as it was,
  translation included (the archive tier's own rule, ingest step 00). The
  owner's promotion of an archive row (`promote_archive_conn`) makes it live,
  and its `deferred` translation becomes `pending` like live mail's.
- **No translator, no promise.** With translation off or no LLM configured,
  ingest queues nothing, and `GET /client/thread/{id}` does not serve a
  `pending` or `deferred` row (nothing would ever finish it); the POST
  promotes nothing. A translation that already landed is still served.
- `GET /client/thread/{id}` promotes NOTHING. The Passband client
  prefetches threads through it (`ThreadPrefetch`: search results, list
  hovers, the sitrep's preload), so a promoting GET would buy translations
  for archive mail nobody opened. It serves `deferred` rows as `deferred`.
- `POST /client/thread/{thread_id}/translate` is the ask. It promotes that
  thread's `deferred` rows to `pending` and, when any moved, wakes the
  refine lane. Human door only, scoped to the active account like the GET.
  Idempotent: only `deferred` rows move, so a repeat promotes and pokes
  nothing. It answers `200 {"promoted": n}`; an unknown thread is
  `{"promoted": 0}`, not an error.
- The client calls it from `ThreadViewer` only, once per open, when the
  thread on screen has a `deferred` message. It runs off the loaded thread
  whichever way it arrived, prefetch cache included, because a warmed thread
  opens without a request. `ThreadPrefetch` never calls it.
- Long mail is split into batches of about 6k WEIGHTED source characters
  per call, whole segments at a time; a segment longer than a batch is split
  at sentence ends and rejoined. A CJK, kana, Hangul, Thai, Lao, Khmer or
  Myanmar character weighs 3, because what a call costs in time is the
  English it writes, so a CJK batch is about 2,000 characters and its answer
  fits the 120-second request timeout. `LlmRequest` gains an optional
  `max_tokens` (the batch's weight plus 4,000, at least the shared default,
  at most 32,000).
- A message is translated up to 60,000 weighted characters
  (`MAX_SOURCE_CHARS`, so 20,000 CJK characters). Past it the head is
  translated, the tail is kept in the original, and the row is `incomplete`
  with the note "only the first part was translated".
- **What is not sent at all** (`translate/segment.rs`), kept in place as
  written: hidden text (`display:none`, `visibility:hidden`, `font-size:0`,
  `opacity:0`, `mso-hide:all`, `max-height:0` or `height:0` with
  `overflow:hidden`, inline or from a one-class rule at the top level of the
  document's `<style>`; a class an `@media` block names stays visible, as
  does one a complex top-level selector (`p .x`, `.a > .x`, `.x:hover`)
  names in a rule that shows it (`display:block`, `visibility:visible`, a
  non-zero `font-size` or `opacity`; `.zz .x{color:red}` changes nothing), and so
  does an element whose inline style shows it again on every property the
  class hides it with, unless the class rule was `!important` and the
  inline one is not), and quoted history: `<blockquote>`, Outlook's reply
  header (`div#divRplyFwdMsg`, or an `<hr>` followed by a `From:`/`De :`/
  `Von:` header block that also carries a `Sent:`/`Envoyé :`/`Gesendet:`/
  `Date:` line, so a sender's own `<hr><p>De: la banque</p>` does not hide
  the rest of its mail) and everything after it, plain `>` lines with their "On ...
  wrote:" line, and everything from a plain "-----Original Message-----"
  (`Message d'origine`, `Ursprüngliche Nachricht` and the other Outlook
  languages) or an underscore rule followed by a `From:` line. When that
  leaves nothing (a forward), the quote IS the mail and is translated.
  Known gaps: text hidden by colour (white on white), a 1px font, a
  class hidden only through a complex selector, or positioning is still
  sent.
- A batch the model calls English (an English signature block, a quoted
  English reply) keeps its own text; a message whose every batch is English
  is `not_needed`.
- **Thinking off.** Translation is not a reasoning task. On models that take
  `thinking: {"type": "disabled"}` the call sends it (Claude Sonnet 5, Opus
  5, Opus 4.8/4.7/4.6, Sonnet 4.6, Haiku 4.5, Haiku 5.5; gateway-qualified,
  platform and dated ids included) and no `effort`. Every other model is
  sent no `thinking` field, because the explicit switch is a 400 on Claude
  Sonnet 5.5, Opus 5.5 and the Fable family. So pointing `translate.model`
  at a model outside that list KEEPS THINKING ON (adaptive, billed at the
  output price and spent against the batch's output ceiling and timeout):
  Sonnet 5.5 turns it off only with `between_tools`, which this client does
  not send.

## Wire

- `ClientMessage.translation: Option<ClientTranslation>`, with
  `skip_serializing_if = "Option::is_none"`.
- `ClientTranslation` has these fields:
  - `source_lang` (ISO code)
  - `source_language` (English name, e.g. "French")
  - `status`: `deferred`, `pending`, `done` or `incomplete`
  - `subject`
  - `content`
  - `html` (optional)
  - `missing_facts` (optional)
- `deferred` (archive tier, not yet asked for) is served as itself, with
  `source_lang`/`source_language` set and empty `subject`/`content` and no
  `html`, so the client knows a translation is available. It reads as
  `pending` once the reader's POST has promoted it.
- `deferred` and `pending` both carry no text. Only `done` and `incomplete`
  carry the translation.
- `failed`, `not_needed` and `rejected` are omitted, so the client shows the
  original and nothing else.
- MCP `SanitizedMessage` gains an optional `translation { source_language,
  subject, content, status }`, joined inside `thread_view`. The agent doors
  see served statuses only (`done`, `incomplete`), never `deferred` or
  `pending`, and have no way to ask for a translation.

## Reader (Mac and iOS share `ThreadViewer.swift`)

- A message with a `translation` renders the translation by default:
  translated subject and translated HTML, or translated plain text.
- A quiet strip above the body says, for example, "Translated from French"
  and has a "Show original" toggle that switches to "Show translation". It is
  modeled on the `imageBar` in `EmailWebCore.swift` and on the outlined
  informational style of `SpamNotice`.
- The toggle is per-message `@State`. The frame cache key includes which
  version is showing.
- `pending`: the original shows, with "Translating from French…". The
  thread refetches on a bounded backoff until the status leaves `pending`.
- `deferred` displays exactly like `pending`. The reader that shows it is
  the one that asks (`POST .../translate`), then refetches in place and runs
  the same backoff. The Swift status decodes `deferred` as itself; an
  unknown status still falls back to `done`, which only shows with a body.
- `incomplete`: the strip adds a note that the translation may have missed
  something and to check the original.
- The `ProductGuide.swift` topic for the reader gains a line about it. No em
  or en dashes appear in any copy.
