# squelch-api

The **human door**: the bearer-authenticated `/client/*` axum router used by squelch's own clients (Passband on the Mac and the phone, scripts you trust). This is the richer of the two doors and the only surface with write actions. This crate also carries the owner's console (`/console`, a second tree with its own auth shape — see below) and, on hosted, the assistant relay. Primarily a library — `squelchd serve` mounts the routers in the unified daemon; the `squelch-api` binary here is a thin dev harness for running the door standalone.

## Surface

Reads: updates (banded standing/new/open), threads and their attachments, hybrid search, sent mail, contacts, stats, the record views (receipts, banking, calendar, shipments), the calendar timeline (`/client/timeline`: ghosts derived from mail, with each end's source tier, the mail's own state apart from the user's mark (`mail_state`), whether the ghost's sender authenticated (`verified`), and `matched_event` when a real Google Calendar event duplicates it; beside them the synced Google Calendar `events` and the sync's `calendar` status; `/client/stats` advertises it as `timeline`, and `POST /client/timeline/refresh` starts a calendar sync, at most once per 30 seconds), the triage usage ledger (`/client/usage`: per-day token/cost history for both LLM triage stages, with each stage's model id), the triage budget config (`/client/triage-config`: the effective per-stage daily caps, where each came from, and trailing-14d spend averages), sender rules, the audit log, an SSE event stream with a replay cursor (`/client/events`, plus `/client/events/{id}` so a phone's notification extension can fetch one by id), and sealed-mail metadata with an explicit audited reveal.

Writes (the only mutation surface in the system): status lifecycle, archive, label, and send. Every action requires an explicit `confirm` flag, uses the separate write-slot Gmail credential (minted via `squelchd auth --write`; sync can only ever load the read slot), and lands in the audit log. Sends pass through an outbound secret guard: a matched body yields a 422 listing redacted match kinds, and only an explicit `override_guard` resend goes out.

Google Calendar writes, the user's own and one event at a time: `POST /client/calendar/events` (create, 201), `PATCH /client/calendar/events/{calendar_id}/{id}` (edit any subset; `location` and `description` take `null` to clear, 200) and `DELETE /client/calendar/events/{calendar_id}/{id}` (204). They go through the calendar token resolver the sync uses, send `sendUpdates=none` unless the request says `notify: true`, change the local row only after Google accepted the write (a refusal is a 502 that changes nothing), send the user's text as typed (less control and invisible characters), store and answer Google's own event in the timeline's shape, laundered like any synced event, and write one audit row per attempt (`calendar_event_create` / `_edit` / `_delete`, target `calendar:<calendar_id>/<event_id>`, never the title). Only on a calendar the user owns or writes to, and only for an event that is theirs (`editable` on the timeline). `GET /client/timeline` serves each event's `color`, `editable` and `description`, and `calendars: [{id, name, color, primary, writable, can_invite}]`. Human door only.

The event pane's guest suggestion: `GET /client/calendar/people?name=steven` answers `{"people": [{"addr", "name"}]}`, at most three, best first, `no-store`. Only people the user has written to (a `contacts` row with `sent_count > 0`), never the account's own address, and never somebody who has only written to the user. The typed words must each start the person's name word for word from the given name ("ste" and "steven n" find "Steven Ng"; "ng" and "even" do not), on the name the user's own Sent mail used, else the newest name on that address's readable mail, else the first token of the address's local part (then `name` is `null`). Two-way correspondents come first, by the older of the last mail each way; inbound mail that is spam, sealed or did not pass sender authentication does not count. An address with no stored name is not offered at all when any of its readable mail carries `List-Unsubscribe` or a list-shaped From name ("Steven Ng via Team", "Steven from Acme"). A name under two characters (empty included) is a 400, `{"error": "name is too short"}`; one longer than 64 characters or containing `@` answers the empty list. No model is involved. Behind the `calendar` feature like the routes above (404 while it is off). Human door only.

Guests, invited by Google: a create takes `"guests": ["a@b.c", ...]` (at most 50 plain addresses, no duplicates, never the owner) and an edit takes the same as the FULL new list (absent leaves it alone, `[]` removes everyone), only on the user's own calendar (`can_invite`) and, for an edit, only when the event is `guest_editable` (the user's own, organized by them, its stored list not truncated); otherwise 400. `"notify": true` on a create or edit, or `?notify=true` on a delete, is the ONLY thing that makes Google email the guests (`sendUpdates=all`); absent or false sends `none`, and any other value is a 400. Passband writes and sends no mail for this. A guest edit rereads the event's attendees from Google and sends the whole array back, kept guests exactly as Google holds them. The timeline serves per event `guests: [{email, name, response, organizer, self}]` (`response`: `accepted` / `declined` / `tentative` / `pending`), `guests_truncated` (Google holds more than 100) and `guest_editable`. Audit rows gain ` guests=<n> notified=<bool>`, never an address.

Local-only writes, which touch this database and never Gmail: sender rules, drafts (one per reply target, one per forwarded message, and any number of new-message drafts, each edited by id; `/client/stats` advertises this as `drafts_v2`; an older app's key-less save edits the newest plain new-message draft, possibly one a newer app has open elsewhere, so last writer wins but rows never pile up), the user's word on a calendar ghost (`PATCH /client/timeline/ghosts/{id}`: dismissed, going, or a corrected end), the user's correction of a correspondent's inferred timezone (`PUT /client/person/timezone` with `{addr, zone}`, `zone: null` handing it back to inference; the contact card at `GET /client/person` serves the result), device registration, a forced re-triage (`POST /client/retriage`, which outranks each pass's age cutoff for a day), and the unsubscribe plan — the server only parses `List-Unsubscribe` and hands back an http(s) URL, and the client is what confirms with the human and opens it.

The triage budget caps are also tunable here at runtime: `POST /client/triage-config` persists overrides for the Stage-1 global cap and the three Stage-2 caps (thread/sender/global), which the sync engine re-reads at the start of each triage pass — no restart, and no Gmail credential involved.

## The other two trees

`/console` is server-rendered HTML for the human who owns this daemon: device
management and a view of what the mailbox is doing, no JavaScript and no external
asset, under a `default-src 'none'` CSP. It invents no credential type — a session
is a device token in an HttpOnly cookie, claimed with a `squelchd pair` code and
verified by the same `verify_device_token` the bearer path calls, so a browser is
one more paired device and revocation, audit and TTL are all inherited. A hosted
tenant additionally gets a "Continue with Google" link, which appears only when
`SQUELCH_CONSOLE_SSO_URL` is set, because Google will not accept a redirect URI per
tenant subdomain and the hop has to go through the control plane.

`POST /client/assistant/messages` is the hosted assistant relay: the app posts its
Anthropic-shaped streaming request, the daemon forwards it to the gateway with a
credential the app never sees, and the SSE bytes stream back verbatim. Self-host
brings its own key in the app, and without a gateway to relay to the route does not
exist (404). The body is the user's own conversation and is treated like mail
content: never logged in either direction.

## Auth

Two kinds of bearer, checked in this order:

1. **`SQUELCH_API_TOKEN`**, the self-host master token, compared in constant time. First-class forever: it works with an empty token table and is the way back in after revoking the last device. Unset or blank means "not configured", which is a supported posture, not a refusal to serve.
2. **Per-device tokens** (`sqd_…`), issued by `squelchd token issue` or by a pairing claim. Stored only as a SHA-256 hash, verified with a point lookup on every request (no cache, so `squelchd token revoke <id>` bites on the very next request), and listed with `squelchd token list`.

A daemon with neither still serves; it 401s everything until a token exists. `POST /client/pair` is the ONE `/client/*` route outside the bearer layer — a device claiming a pairing code has no credential yet, by definition. Every pairing failure is an identical empty 401, and that route carries no CORS layer, so a web page cannot claim codes cross-origin.

CORS on the rest of `/client/*` is configured for webview / vite dev origins, but the token is the boundary — CORS is a courtesy.

## Dev binary

```sh
SQUELCH_API_TOKEN=... cargo run --bin squelch-api
```

| Variable | Meaning | Default |
|---|---|---|
| `SQUELCH_API_TOKEN` | master bearer token; optional, device tokens work without it | — |
| `SQUELCH_API_HTTP` | bind address | `127.0.0.1:8849` |
| `SQUELCH_DB_PATH` | SQLite path | shared XDG default |
| `SQUELCH_ACCOUNT_EMAIL` | account email | `me@localhost` |

Loopback only by default; front with a reverse proxy to expose it. In production use `squelchd serve` instead, which hosts this router and the agent door on one port.
