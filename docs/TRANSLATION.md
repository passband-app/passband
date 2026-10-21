# Auto-translate

Mail written in a language other than English is translated to English
automatically. The reader shows the English with a header saying it was
translated, and one click shows the original. Both versions are indexed for
keyword search and semantic search.

Decisions locked with the user (2026-10-21):

- Model: **`claude-sonnet-5`**. It is already on every tenant's Bifrost
  allow-list (`DEFAULT_LLM_MODELS` in `squelch-control/src/config.rs`), so
  shipping needs no `llm sync`/`mint`.
- Rendering: **HTML text-node translation.** The visible text segments are
  pulled out of the already-sanitized `body_html`, translated as a numbered
  array, and put back. The model never sees or produces markup.
- Scope: **all live incoming mail, eagerly. Archive-tier mail is translated
  only when the reader displays it** (see Pipeline). No backfill of mail
  that is already stored.
- No new env vars and no warden knobs. The feature is ON by default and takes
  effect when the daemon version rolls out. A config-file section with
  defaults is fine.

## Invariants

1. **`messages.body`, `messages.subject` and `messages.body_html` are never
   rewritten.** `body` and `subject` are hashed into the agent triage
   `content_snapshot` (`store/sqlite/agent_triage.rs`). Changing them
   re-triages the message and resets external access to `pending`. A
   translation lives only in `message_translations`, and it is NOT part of
   `content_snapshot`.
2. **Every fact is carried over.** The prompt demands a complete, faithful
   translation: no summarizing, no omissions, no additions, and numbers,
   amounts, dates, codes, names, URLs and addresses kept exactly. The code then
   verifies this:
   - The output array has the same length as the input array, one translation
     per segment.
   - A fact check compares the source and the translation for every digit run
     (separators inside a number are ignored, so `1.234,56` matches
     `1,234.56`), every URL, every email address, and every token of 5 or more
     uppercase letters or digits that looks like a code.
   - On a failure the call is retried once with the missing items named.
   - If it still fails, the row is stored with `status='incomplete'` and the
     client warns about it.
3. **Email text is untrusted.** Segments are fenced the same way the other
   prompts fence email content (`triage::text`). Translated strings are
   inserted as escaped text nodes, never parsed as HTML. The reassembled
   document goes through `sanitize_email_html` again.
4. **Spam never reaches a model**, as everywhere else. **Sent mail is never
   translated**, because the user wrote it. Archive-tier rows spend nothing
   until the reader displaying the thread asks for them. Reading the thread
   is not asking: the client prefetches threads through the same GET.
5. **External doors (MCP, agent HTTP) see a translation exactly when they may
   see the body.** The translation is joined inside the same guarded query.
   It is never served through a path the body could not take.
6. Spend respects `refine_can_spend()` and is billed to usage category
   **`translate`** in `stage2_usage`. It is priced at the actual Sonnet 5
   rates in `metrics.rs` `estimate_cost_usd` and the `/client/usage`
   estimator. Without that it would fall through to the Opus arm.

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
  transaction. Archive rows get `status='deferred'`.

## Storage (`schema.sql`, `CREATE TABLE IF NOT EXISTS`)

```sql
message_translations(
  message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
  account_id INTEGER NOT NULL,
  source_lang TEXT NOT NULL,            -- ISO 639-1, e.g. 'fr'
  status TEXT NOT NULL CHECK (status IN ('deferred','pending','done','incomplete','failed','not_needed')),
  subject TEXT, body TEXT, body_html TEXT, snippet TEXT,
  missing_facts TEXT,                   -- JSON array when incomplete
  model TEXT, prompt_version INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)
```

- `body` is `html_to_text(translated body_html)` when there is HTML.
  Otherwise it is the translated plain text, with paragraphs as segments.
- `snippet` is the first 200 characters of the translated body.
- `failed` rows (transport or refusal errors) are retried with backoff and a
  bounded attempt count. They never loop forever.
- A configuration failure (401/402/403/404, a model missing from the
  gateway's allow-list) is not a fact about the message: the row is released
  with its attempt handed back, and the translate pass cools down for
  `triage.agent.outage_retry_secs` without touching triage's own circuit.
- Mail a person sealed (`triage.sensitivity = 'sealed'`) is never claimed.

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

- The **translate pass** runs in the refine lane before `agent_triage_pass`,
  so triage usually sees the English.
- Triage gets the translation as an extra field in the agent context
  (`translated_subject` and `translated_body` on the message). Its body is
  NOT replaced, and the translation is not hashed into the snapshot.
- Triage does NOT wait for a translation. If the translation is late, triage
  runs on the original, as it does today.
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
- Long mail is split into batches of about 6k source characters per call.
  `LlmRequest` gains an optional `max_tokens`.

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
- `failed` and `not_needed` are omitted, so the client shows the original and
  nothing else.
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
