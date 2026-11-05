# squelchd

The daemon. One process that hosts everything: the Gmail sync loop and a single HTTP server mounting both doors — the agent door (`/mcp`) and the human door (`/client/*`) — plus the owner's console at `/console`, which `/` redirects to. This crate sits at the top of the workspace: it depends on `squelch-core`, `squelch-mcp`, and `squelch-api`; nothing depends on it.

## Subcommands

```sh
squelchd auth              # one-time OAuth consent (requests gmail.readonly); token -> macOS keyring / 0600 file on Linux
squelchd auth --write      # BOTH credentials: write (gmail.modify + gmail.send), then read
squelchd auth --headless   # headless box: prints consent URL, binds loopback :8847
squelchd auth --export     # consent here, print a one-line blob to move elsewhere (--out FILE for 0600; --write for both slots)
squelchd auth --import     # store a blob from stdin, verified against Google first
squelchd auth --broker URL # consent via a relay — undeployable for self-host, see docs/BROKER.md
squelchd run               # sync loop only, no HTTP (back-compat)
squelchd serve [--bind A]  # the unified daemon: sync + both doors on one port
squelchd pair [--url BASE] # mint a pairing code + deep link for a new device
squelchd token issue --name NAME   # mint a device token, printed once
squelchd token list                # every token, revoked ones included; no secrets
squelchd token revoke ID           # kill one device, effective on its next request
```

`auth` mints the read credential the sync engine uses. `auth --write` runs two consent flows back to back — the write credential first, then the read one — so the two never drift apart after a renewal. They stay two tokens in two separate slots: the write one is reachable only by the human door's action handlers, and sync and triage never touch it. On a headless host, forward the consent port with `ssh -L 8847:127.0.0.1:8847 <host>` and open the printed URLs locally; both flows reuse that one port.

## Configuration

Config comes from `~/.config/squelch/config.toml` (override with `--config`) plus environment variables (a `.env` in the repo root is loaded via dotenvy):

| Variable | Meaning | Default |
|---|---|---|
| `SQUELCH_CLIENT_ID` / `SQUELCH_CLIENT_SECRET` | your Google Cloud "Desktop app" OAuth client | required for `auth` |
| `SQUELCH_ACCOUNT_EMAIL` | the Gmail account | required |
| `SQUELCH_API_TOKEN` | master bearer for `/client/*`; optional, see below | — |
| `SQUELCH_BIND` | bind address for `serve` | `127.0.0.1:8848` |
| `SQUELCH_DB_PATH` | SQLite path | `~/.local/share/squelch/squelch.db` |
| `SQUELCH_POLL_SECS` | Gmail poll interval | `45` |
| `SQUELCH_IDLE_POLL_SECS` | cap on the poll wait while the mailbox is idle: the wait doubles from `SQUELCH_POLL_SECS` up to this after quiet polls, and drops back on a change, a manual refresh, or a connected client. `0` is no backoff. Lengthening it trades idle Gmail requests for how late new mail can be noticed when no client is connected | `0` |
| `SQUELCH_FETCH_CONCURRENCY` | `messages.get` calls in flight while a window of mail is fetched (first backfill, catch-up, history walk); each is a raw body in memory, and the Gmail budget caps the gain past 2 | `2` |
| `SQUELCH_MCP_ALLOWED_HOSTS` | extra Host values when fronted by a proxy | — |
| `SQUELCH_MCP_OAUTH_ISSUER` | OAuth issuer for the agent door; set together with the next one | unset (agent door open) |
| `SQUELCH_MCP_PUBLIC_URL` | this daemon's public `/mcp` URL, the token audience | unset (agent door open) |
| `SQUELCH_MCP_OAUTH_JWKS_URL` | override for the issuer's signing keys | `<issuer>/oauth2/jwks` |
| `SQUELCH_METRICS_BIND` | second listener serving `GET /metrics` and `GET /healthz` | unset (no port opened) |
| `SQUELCH_EMBED_MODEL` | semantic-recall weights, same value as `[embed] model` | `Xenova/bge-small-en-v1.5` |
| `SQUELCH_CRED_BACKEND` | `keyring` (the macOS keychain; macOS only) or `file` | `keyring` on macOS, `file` elsewhere |
| `SQUELCH_CREDENTIALS_PATH` | where the `file` backend writes, mode 0600 | `~/.config/squelch/credentials.json` |

**Older mail (the archive).** Mail older than the backfill window is fetched once by a slow background walk and kept findable by search and openable in the reader, with no model call when it is stored. It is on by default, on every deployment, and has no environment variable; four config-file keys tune it. `[sync] archive_days` (default `365`, clamped to 7300, `0` turns it off) is how far back it reaches. `[sync] archive_min_free_mb` (default `2048`, `0` turns the check off) is the disk floor: the walk stores nothing while the filesystem the database lives on has less than this many MiB free, or while free space cannot be read, and it is checked before every batch. The floor in force is this number or a fifth of the whole filesystem (at most 5 GiB), whichever is larger (a volume several databases share needs a floor that is a share of it), and a walk the floor has stopped resumes only once free space is a tenth above it. `[sync] archive_max_db_mb` (default `4096`, `0` is no ceiling) is the limit on the database itself: the walk stores nothing while the database file plus its write-ahead log is this large. Live mail is never held by either. `[triage.agent] archive_access_daily_cap` (default `50`) is how many access checks a day agent searches and thread opens may buy on that mail. Archive attachments are kept as name and size only (no bytes), and the walk fetches at most 30 messages a minute. On `/metrics`, `squelchd_archive_walked_to_timestamp_seconds` and `squelchd_archive_covered_to_timestamp_seconds` are how far back and how far forward it has got, `squelchd_archive_held{reason=...}` is 1 for whatever is holding it off (`poll`, `window`, `embedder`, `disk`, `clock`, `quota_day`, `db_size`), and `squelchd_archive_skipped_messages_total` counts messages it stepped over (each is logged by Gmail id and error kind).

**Old bulk mail on the first load.** The first backfill (and a catch-up after a long outage) reads `backfill_days` of inbox, but list mail in it older than a week is not sent to the model: a message carrying a `List-Unsubscribe` header and dated more than `[sync] backfill_bulk_triage_days` (default `7`) ago is stored as an archive row, exactly like the older mail above (searchable, openable, no model call unless an agent search or thread open surfaces it). Mail without the header, and list mail a sender rule matches, is triaged at any age in the window. Live arrivals are never affected. Set the key to `backfill_days` or more to triage the whole window; there is no environment variable.

The listener defaults to loopback and never silently widens. To expose it beyond the machine, front it with a reverse proxy (e.g. `tailscale serve --bg 8848`) and set `SQUELCH_MCP_ALLOWED_HOSTS`.

The agent door has three modes, and the startup line says which one it is in. With neither `SQUELCH_MCP_OAUTH_ISSUER` nor `SQUELCH_MCP_PUBLIC_URL` set it is **open**: no sign-in, and the Host allow-list (loopback plus `SQUELCH_MCP_ALLOWED_HOSTS`) is its only guard, which is the self-host posture and why the advice above matters. With both set it requires **OAuth** bearer tokens from that issuer, bound to this mailbox's address, and adds the public URL's host to the allow-list itself. With only one set, a value that is not a valid URL, a public URL whose path is not exactly `/mcp` (no trailing slash, other path or query), or both set while the account email is unset or the `me@localhost` placeholder (tokens need a real mailbox to bind to), it is **misconfigured**: `/mcp` answers 503, the human door serves on, and the log names the setting at fault. The full model is in [docs/SECURITY.md](../docs/SECURITY.md) §8.

### The embedding model

`[embed] model` (or `SQUELCH_EMBED_MODEL`) is a fastembed `model_code`, and it must name exactly one build. Fastembed ships several families twice under one code, once in fp32 and once quantized, so a name that could mean either is now refused at startup instead of being resolved by whichever entry happened to come first. A refused name disables semantic recall for that run and logs one line; keyword search and triage are unaffected.

The default is the full code `Xenova/bge-small-en-v1.5`, and the short spellings `bge-small-en-v1.5`, `bgesmallenv15` and `bge_small_en_v15` are aliases for exactly that, so a config that already says one of them needs no edit. What does need an edit is a name that fits more than one build: a bare family name like `bge-base-en-v1.5` or `all-MiniLM-L12-v2`, and the codes fastembed shares outright, which are `Xenova/all-MiniLM-L12-v2`, `nomic-ai/nomic-embed-text-v1.5`, `mixedbread-ai/mxbai-embed-large-v1`, `Alibaba-NLP/gte-base-en-v1.5`, `Alibaba-NLP/gte-large-en-v1.5`, `onnx-community/embeddinggemma-300m-ONNX` and the five `snowflake-arctic-embed-*` codes. Those come in two shapes and the error tells them apart, because the fix differs. When the builds sit under DIFFERENT codes, as `bge-base-en-v1.5` does, either full code names one of them and the error lists both: `Xenova/bge-base-en-v1.5 (BGEBaseENV15)`, `Qdrant/bge-base-en-v1.5-onnx-Q (BGEBaseENV15Q)`. When they sit under ONE code no code can pick between them, and `all-MiniLM-L12-v2` is that shape despite looking like the first: both its builds are `Xenova/all-MiniLM-L12-v2`, so the error names that code, says a full model_code cannot pick between them, and lists the two fastembed variant names, `AllMiniLML12V2` and `AllMiniLML12V2Q`. Either way what you paste back into config is a copy out of the log.

### Metrics and readiness

`SQUELCH_METRICS_BIND` (e.g. `127.0.0.1:9464`) opens a second listener with exactly
two routes on it. `GET /metrics` is Prometheus text: sync timestamps, Gmail error
counts, LLM spend, store sizes. `GET /healthz` is one word out of two — `200` once
the sync engine is running and embedder init has settled, `503` before that and
again if either stops — because the doors bind before the daemon has finished
starting, so that a first-run model download cannot make them unreachable, which is
exactly why "the port accepts" answers a different question than "it is serving".

Both are **plaintext and unauthenticated by design**: a scrape credential sitting in
a Prometheus config is its own problem, and `/healthz` says nothing about the mailbox
behind it. Bind it to loopback or a private interface, never a public one. Unset, the
daemon opens no such port at all.

### The console

`serve` also hosts a small server-rendered console at `/console` for the person who
owns this daemon: what the mailbox is doing, and which devices may reach it. A
session there is not a new credential type — signing in claims a `squelchd pair`
code exactly as an app does, and the `sqd_` token it returns rides as an HttpOnly
cookie verified by the same check the bearer path runs. The browser shows up in
`token list` as `console` and `token revoke <id>` ends the session. No JavaScript, no
external asset, `default-src 'none'`.

### Carrier polling

Package tracking keeps working from mail alone, but the daemon can also ask the carriers directly. That is bring-your-own-credentials and off until you provide some: no credentials means no poller task and no carrier API is ever contacted.

```toml
[carriers]
poll_interval_hours = 6        # baseline cadence for an in-flight package
ofd_poll_interval_mins = 60    # tighter cadence once it is out for delivery
max_age_days = 45              # stop chasing a package this old
max_failures = 5               # consecutive permanent failures before retiring it

[carriers.ups]                 # SQUELCH_UPS_CLIENT_ID / _CLIENT_SECRET
client_id = "..."
client_secret = "..."

[carriers.fedex]               # SQUELCH_FEDEX_CLIENT_ID / _CLIENT_SECRET
client_id = "..."
client_secret = "..."

[carriers.usps]                # SQUELCH_USPS_CONSUMER_KEY / _CONSUMER_SECRET
consumer_key = "..."
consumer_secret = "..."

[carriers.dhl]                 # SQUELCH_DHL_API_KEY / SQUELCH_DHL_DAILY_CAP
api_key = "..."
daily_cap = 200
```

Each carrier is independent, and a credential pair set in the environment materializes a carrier the file never mentions, so a container needs no `config.toml`. The cadence knobs have `SQUELCH_CARRIERS_*` equivalents. Where to get each carrier's credentials, what the cadence actually does, and the metrics to alert on: [docs/SHIPMENTS.md](../docs/SHIPMENTS.md).

## Human-door credentials

`SQUELCH_API_TOKEN` is the master key and is never going away: it works with an empty token table, so it is the way back in after revoking every device. It is now optional, though. `serve` comes up without it and accepts **per-device tokens** instead:

```sh
squelchd pair                      # prints XXXX-XXXX + a passband://pair deep link, good for 10 minutes
squelchd token issue --name phone  # or mint one directly; the sqd_ token is printed ONCE, on stdout
squelchd token list                # id, name, created, last used, revoked
squelchd token revoke 3            # effective on the next request; nothing is cached
```

Each token is stored only as a hash, so a lost one is re-issued rather than recovered. Pairing codes are single-use, expire in ten minutes, burn after a few failed claims, and one mint supersedes the last. A daemon with no master token and no device tokens serves and 401s everything, which is exactly the state `squelchd pair` gets you out of.

The link `pair` prints names the address `serve` would bind (`SQUELCH_BIND`, else `127.0.0.1:8848`), so a daemon on a non-default port advertises the right one. Pass `--url` whenever the device is not this machine, and prefer an https front (`tailscale serve`): over plain http to another host, the code and the token it buys are readable in transit, which `pair` warns about.

## Run it

```sh
set -a; source .env; set +a
cargo run --bin squelchd -- auth
cargo run --bin squelchd -- serve
```

On macOS `cargo run` signs the binary with your Developer ID first (the runner
in `.cargo/config.toml`), so the keychain asks for "Always Allow" once per
credential and then stays quiet across rebuilds. A binary started straight from
`target/` skips the runner and is a stranger to the keychain again; start it
with `scripts/sign-run.sh target/debug/squelchd serve` instead.

See the [root README](../README.md) for the full setup walkthrough and [deploy/DEPLOY.md](../deploy/DEPLOY.md) for the Linux server deployment.
