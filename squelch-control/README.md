# squelch-control: the Passband hosted signup control plane

One invite code plus one Google consent becomes one provisioned tenant daemon.
This crate is the web half of the hosted tier described in
[`docs/HOSTED.md`](../docs/HOSTED.md); the cluster half is `squelch-warden`,
which runs in-cluster and turns these two calls into a tenant's pod.

```
browser ── GET /  ──► signup form (invite code + address)
        ── POST /signup ──► validate, open session, 303 to Google
        ── Google consent ──►
        ── GET /oauth/callback ──► exchange, CREATE tenant (learn its recipient),
                                   SEAL to it, INSTALL credential, success page
```

Where the code comes from, when the waitlist is configured (the trio in the
table below; unset, none of it is mounted and both URLs are a 404):

```
browser  ── GET  /  ──────────► "No invite code? Join the waitlist",
                                linking to <WAITLIST_ORIGIN>/waitlist
site     ── POST /waitlist ──► one users row per address (with the name it gave),
                                same 200 for a duplicate
operator ── GET  /admin ──────► token login, then the list
         ── POST /admin/approve ──► mint an invite, email it through Resend
         ── POST /admin/invite ───► the same, for an address that never joined
         ── POST /admin/send ─────► revoke that code, mint and mail a fresh one
```

The form asks for a name as well as an address, and the name is stored beside it
and shown under it on the board — context for the approve decision, never an
identity: nothing is matched, joined, or mailed on it. It is written by the
INSERT only, so a second submission of an address already on the list cannot
rewrite the name the operator sees beside it, and a row that arrived before the
field existed simply has none.

A direct invite is recorded as a `users` row that starts out approved, so one
table answers "have we invited them" however they arrived and the re-send button
works on both. Typing an address that is already waiting approves the row it has
rather than making a second one.

`users` is the whole funnel in one table: waiting, invited, signed up. Only
pending and approved are STORED, because those two are what the one-click-one-
invite guards swing on; everything after is read off the stamps a signup writes,
so there is no second copy to disagree with the first. A code minted from the
CLI names nobody, so that row appears at consumption instead, keyed on the
Google account that redeemed it. An invite is a bearer code, so the address it
was mailed to and the account that signs up are two different facts and the
board shows both when they differ.

The emailed link is `/?invite=CODE`, and `GET /` fills the form in from it. That
reverses this crate's first rule, which was that a code never appears in a URL:
the query string reaches the edge's access log, the recipient's history, and any
proxy between them, and a live code sits in all three until it is redeemed or
lapses. It is a deliberate trade for the click, taken 2026-08-14, bounded by the
code being single use. A parameter not shaped like a code renders an empty field.

The "join the waitlist" link closes the loop from the other end: a person with
no code could not get one from the page that demands it. It is built from
`SQUELCH_CONTROL_WAITLIST_ORIGIN` (the same origin the CORS answer names, so the
link and the form it leads to cannot drift apart) and is rendered on every signup
page: prefilled or not, refused or not. Making it conditional on either would
turn its presence into an answer about the code in the URL. With the waitlist
unconfigured the form links nowhere.

The admin POSTs take a `SameSite=Strict` session cookie AND a same-origin
`Origin`/`Sec-Fetch-Site`, because "same site" includes every sibling
`passband.app` name and a page on one of those could otherwise press these
buttons. An `Origin` of `null` names nothing, so it is decided by
`Sec-Fetch-Site`, which no page can write.

## The agent door: WorkOS AuthKit login (`/agent/auth`, `/agent/confirm`)

Agents reach a hosted tenant's `/mcp` with an OAuth access token issued by
**WorkOS AuthKit**, running in "Standalone Connect" mode: AuthKit is the
authorization server and does the OAuth dance with the agent, but it delegates
the LOGIN to this service. This route is that login:

```
agent    ── OAuth authorize ──► AuthKit
browser  ── GET /agent/auth?external_auth_id=<opaque> ──► 302 to Google (identity only)
         ── Google consent ──►
         ── GET /oauth/callback ──► mailbox Google names → its ACTIVE tenant here
                                    park the result, 303 to /agent/confirm (fresh cookie)
         ── GET /agent/confirm ──► OUR confirmation page (a read; refresh is safe)
         ── POST /agent/confirm ──► POST https://api.workos.com/authkit/oauth2/complete
                                    303 to the redirect_uri WorkOS answers with
         ── AuthKit consent ──► token for the agent
```

- **One input.** `external_auth_id` is the only parameter read, and it must be
  1 to 128 ASCII letters, digits, `-` or `_`; anything else is a 400 refusal page
  and opens no session. No return URL, no tenant, no address. It is opaque to
  us: parked in the server-side session (never in the cookie), sent to WorkOS,
  and never logged.
- **Not an oracle.** Every well-formed id gets the same 302 to Google. Nothing
  is looked up until Google has authenticated somebody, so whether a mailbox
  exists is a question only its owner can ask.
- **A page of ours asks first.** The `/agent/auth` link can be mailed to a
  victim by whoever started the authorization, Google's chooser asks a
  returning user nothing, and AuthKit's consent screen names the agent with
  whatever name its builder registered. So the callback does NOT call WorkOS:
  it parks the verified result (the id, the tenant, the stored address, the
  verified address) server-side under a FRESH session id and cookie, for five
  minutes, one-shot, inside the same bounded session table, and answers a 303
  to `GET /agent/confirm`, a stable address with no query string (Google's
  code does not stay in the address bar). That page names the mailbox: an AI
  agent is asking to read this mail, continue only if you started this
  yourself on this device, close it if somebody sent you the link, and the
  next screen's agent name is chosen by whoever built it. Unframeable,
  uncacheable, no script, no CORS grant.
- **`GET /agent/confirm` is a read.** It renders from the parked session
  without consuming it and sets no cookie, so a refresh, Back/Forward, or a
  second tab shows the same page with the same one CSRF token. It answers only
  to the flow cookie naming a live confirmation; anything else is the refusal
  page. A refresh of the old callback URL (stale code, new cookie) is sent back
  to this page without touching the parked session or Google: the callback
  peeks at the session before taking it, and declines to take the two
  agent-flow kinds it never finishes. Every other kind is still taken first.
- **Confirmations are not charged to the login pool.** Only a Google-verified
  owner of an active tenant can create one, so a stranger spraying
  `/agent/auth` cannot crowd them out. They are bounded by the whole session
  table, and per client by the `/agent/auth` bucket (each needs its own
  pre-Google session and a Google round trip) and a five minute life.
- **`POST /agent/confirm`** is the answer. It requires a same-origin
  `Origin`/`Sec-Fetch-Site` (the admin POSTs' check), the flow cookie, an
  atomic one-shot take of a confirmation session (any other kind is refused),
  and the form's CSRF token (constant time). Continue completes at WorkOS;
  Cancel discards the state and says nothing was connected. A double click is
  covered: while the first press is waiting on WorkOS it leaves an in-flight
  placeholder under its session id (15 seconds at most), and a second Continue
  from the same browser (same signed cookie, same token) that lands then waits
  for it, polling the table, and gets the first press's answer; for 10 seconds
  after, a done or failed marker answers the same way. The same 303, or the
  same class of failure page, and never a second WorkOS call. A wrong token or
  a Cancel in that window is refused without disturbing the first press, and
  after the markers expire a replay is refused. The success answer does not clear the flow cookie, so that second
  press can still be matched. Both methods ride the callback's rate-limit
  bucket, as the second half of a consent already given, and a refresh must
  not meet a 429.
  Its page's CSP widens `form-action` to `https:`, because Chrome applies
  `form-action` to the redirect a form submission gets and the AuthKit host is
  not configured here.
- **WorkOS is called last.** Only once the Google-verified mailbox matches an
  active tenant and its owner has pressed Continue. A Google account with no
  mailbox here gets a plain "no Passband mailbox" page, nothing is parked, and
  WorkOS hears nothing. The completion sends
  `{"external_auth_id", "user": {"id": <verified address, lowercased>, "email":
  <the address as the tenant record stores it>}}` with `Authorization: Bearer
  <key>` and a 10 second budget. That `email` becomes the token's `email` claim,
  which is what a tenant daemon holds against its own account before serving
  anything.
- **The redirect is checked.** The browser is sent (303, `no-store`,
  `no-referrer`) to WorkOS's `redirect_uri` only if it is an absolute `https`
  URL with a host and no userinfo (`http` too, on a plain-http local run).
  401/403, 429, any 5xx, a timeout, unreadable JSON, or a URL that fails that
  check is one "try again" page. Every other non-2xx status (in practice a 4xx
  such as a spent or expired `external_auth_id`, but also a 3xx, which the
  client never follows) gets "this connection request has expired, start
  again from your agent", since a retry here can never work. Neither repeats
  anything WorkOS said.
- `/agent/auth` shares the login hops' rate-limit bucket and counts against
  the session table's login ceiling.

**Setup.** Set `SQUELCH_CONTROL_WORKOS_API_KEY` in the `control` service's
Railway variables; unset, neither `/agent/auth` nor `/agent/confirm` is mounted
(404). In the WorkOS
dashboard, under *Connect* → *Configuration*, set the **Login URI** to
`https://<control origin>/agent/auth` (today `https://signup.passband.app/agent/auth`).
The `control` service deploys itself from `main`: merge the change, set the
variable, and let it redeploy. Do not `railway up`. The WorkOS API origin is a
pinned constant (`https://api.workos.com`), not a variable, for the reason the
Google and Resend ones are.

## Provisioning is two calls

Wire v2 splits provisioning so that no single key opens two mailboxes:

1. `POST /v1/tenants {label, account_email}` → `201 {recipient}`. The warden
   mints an age identity **for that tenant**, writes it into that tenant's
   Kubernetes Secret, and answers with the public half. The tenant is now
   `pending`: no credential, nothing running.
2. `PUT /v1/tenants/{label}/credentials {cred_read_ciphertext}` → `200`
   `{pair_code, pair_url, deep_link}`. The warden installs the sealed credential,
   applies the workload, and mints the first pairing code.

A failure between the two leaves a **pending** tenant. That is retriable by
design: the invite is not spent, the address is held for that Google account, and
a retry walks call 1 again and gets the **same** recipient back. `POST /signup`
therefore lets a pending label through its availability check; the warden decides
whether the mailbox coming back is the one that reserved it (a different account
gets a 409, which this crate reports as "already taken").

## What this process holds, and what it cannot do

| Holds | Does not hold |
|---|---|
| the confidential **web** OAuth client id + secret | any age **identity** (private key) |
| one tenant's age **recipient** (a public key), for the length of one signup | any long-lived recipient, and no key shared between tenants |
| tenant labels, mailbox addresses, invite **hashes** | invite codes, pairing codes, tokens or ciphertext at rest |

A plaintext refresh token exists in this process's memory for the length of one
callback: between the token exchange returning and the age encryption finishing.
It is never written to the store, never logged, and never rendered. What leaves
is ASCII armor addressed to **that one tenant**, which only that tenant's daemon
can open (its pod mounts the identity Secret and reads it via
`SQUELCH_CRED_AGE_IDENTITY`).

The plaintext inside the armor is rendered by
`squelch_core::credentials::credentials_file_plaintext`, so the on-disk shape the
daemon reads is decided in exactly one place. A bare `StoredToken` would encrypt
and decrypt fine and then deserialize into an empty slot map, which surfaces as
"no stored credentials" on a box nobody can easily debug.

## One consent, both slots

Signup asks for `gmail.readonly`, `gmail.modify`, and `gmail.send` **in one
consent** (squelch-core's `GMAIL_READONLY_SCOPE` + `WRITE_SCOPES`, never spelled
out here), and seals the resulting grant into **both** credential slots: `email`
for the Read side and `email#write` for the Write side. Hosted Passband ships
compose, archive, and label, and the daemon's action path loads the Write slot,
which nothing else on a hosted box can fill: there is no second consent screen a
tenant can reach from inside the app.

The scope check after the exchange is therefore a floor over all three. Google
unions grants across a Cloud project, so a token may report **more** than was
asked for and that passes; **less** does not. A partial consent (a box unchecked
on Google's screen) provisions nothing at all: the callback stops before call 1,
hands the invite code back, and says all three permissions are needed. A tenant
sealed on a partial grant would look fine until the first archive or send, with
no way back to Google from the app.

The corollary worth stating plainly: **a full compromise of this Railway service
yields no mailbox.** It yields the ability to provision new tenants and to seal
things nobody can open, plus a list of labels and addresses. The refresh tokens
are in the cluster, each encrypted to a key that is not here and that opens
nothing else.

## Environment

Everything is validated at startup and a bad value is a refusal to boot.

| Variable | Required | Meaning |
|---|---|---|
| `SQUELCH_CONTROL_BIND` | no | Listener. Default `127.0.0.1:8852`; the container entrypoint widens it to `0.0.0.0:$PORT`. |
| `SQUELCH_CONTROL_PUBLIC_URL` | **yes** | This service's externally visible origin, e.g. `https://signup.passband.app`. Deliberately NOT on the tenant base domain: wildcard subdomains there mean "a tenant" and nothing else. The Google `redirect_uri` is this plus `/oauth/callback`. |
| `SQUELCH_CONTROL_BASE_DOMAIN` | **yes** | The hosted base domain, e.g. `passband.email`. Tenant URLs are `https://<label>.<base domain>`. Never hardcoded. |
| `SQUELCH_CONTROL_CLIENT_ID` | **yes** | The confidential **web** OAuth client id (not the desktop client the self-hosted daemon uses). |
| `SQUELCH_CONTROL_CLIENT_SECRET` | **yes** | Its secret. |
| `SQUELCH_CONTROL_COOKIE_KEY` | **yes** | HMAC key for the signup cookie. base64 or hex, at least 32 bytes decoded. `openssl rand -base64 48`. |
| `SQUELCH_CONTROL_WARDEN_URL` | **yes** | The warden's base URL, e.g. `https://warden.passband.app` (product domain, same reasoning as the public URL). |
| `SQUELCH_CONTROL_WARDEN_TOKEN` | **yes** | Bearer presented to the warden. Must match `SQUELCH_WARDEN_TOKEN` in the cluster. |
| `SQUELCH_CONTROL_DATABASE_URL` | **yes** | The control store's Postgres URL. Falls back to `DATABASE_URL`, so a Railway service that references the Postgres service the default way needs no second variable — set it to `${{Postgres.DATABASE_URL}}` (the **private** URL; the store speaks no TLS, which private networking is for). No default: a database URL cannot be guessed. |
| `SQUELCH_CONTROL_BIFROST_URL` | pair | The Bifrost LLM gateway's governance origin, `https://` only. With the token below it is all-or-nothing: both set mints a per-tenant virtual key at signup, neither set provisions keyless tenants, anything partial (including a budget or model list on their own) refuses to boot. |
| `SQUELCH_CONTROL_BIFROST_ADMIN_TOKEN` | pair | The gateway admin's `username:password`, sent as HTTP Basic on every governance call (a session bearer expires after 30 days and does not belong here). Exactly one `:` between two nonempty halves, at least 32 characters total. It can mint unbounded LLM spend; treat it like the warden bearer. |
| `SQUELCH_CONTROL_LLM_BUDGET_USD` | no | Monthly USD budget stamped on each minted triage key. Default `5.00`. Only meaningful with the gateway pair set; set alone it refuses to boot. |
| `SQUELCH_CONTROL_LLM_MODELS` | no | Comma-separated model allow-list stamped on each minted triage key. Each entry is letters, digits, `-`, `_`, `.`, optionally provider-qualified as `anthropic/claude-opus-5` — the gateway matches this list against the model string the daemon actually sent, and a daemon sends the prefix only once rolled, so the default carries **both** spellings of every id. Never empty (the gateway treats an empty list as deny-all). Only meaningful with the gateway pair set; set alone it refuses to boot. |
| `SQUELCH_CONTROL_ASSISTANT_BUDGET_USD` | no | Monthly USD budget stamped on each minted assistant key (`tenant-<label>-assistant`, the tenant's second key). Default `10.00`. Only meaningful with the gateway pair set; set alone it refuses to boot. |
| `SQUELCH_CONTROL_ASSISTANT_MODELS` | no | Comma-separated model allow-list stamped on each minted assistant key. Same grammar and same both-spellings default as the triage list above. Never empty. Only meaningful with the gateway pair set; set alone it refuses to boot. |
| `SQUELCH_CONTROL_ADMIN_TOKEN` | trio | The operator's password for `/admin`, where waitlist rows are approved. At least 32 characters: `openssl rand -base64 32`. With the two below it is all-or-nothing: all three set mounts the waitlist and admin routes, none set leaves them unmounted (a 404, not a 403), anything partial refuses to boot. |
| `SQUELCH_CONTROL_RESEND_API_KEY` | trio | Resend sending key, used for the one call that mails an approved applicant their invite. Printable ASCII, no spaces. Mint it sending-only and domain-restricted. |
| `SQUELCH_CONTROL_INVITE_FROM` | trio | The `From:` invites are sent as, e.g. `Passband <invites@passband.app>`. Must be an address on a domain **verified at Resend** or every send is refused. |
| `SQUELCH_CONTROL_WAITLIST_ORIGIN` | no | The one browser origin allowed to post the public waitlist form, echoed as `Access-Control-Allow-Origin` on that route only, and the origin the signup page's "join the waitlist" link is built from (`<origin>/waitlist`). Default `https://passband.app`. Only meaningful with the trio above set; set alone it refuses to boot. |
| `SQUELCH_CONTROL_WORKOS_API_KEY` | no | The WorkOS API key (`sk_live_...`), sent as a bearer on the one AuthKit completion call `/agent/auth` makes. Set: the agent door is mounted. Unset: `/agent/auth` is a 404. Printable ASCII, no spaces. A SECRET: it acts on the whole WorkOS environment. Set it in the `control` service's Railway variables. |
| `SQUELCH_CONTROL_TRUSTED_PROXY_HOPS` | no | How many proxies write `X-Forwarded-For` in front of this listener. `0` (default) meters the TCP peer, which behind a platform edge means one shared rate-limit bucket. Set `1` on Railway. |
| `SQUELCH_CONTROL_ALERT_WEBHOOK_URL` | no | A Slack incoming webhook (https only). When set, control posts there when a signup ends half-finished, hits an already-set-up mailbox, or provisions a tenant it then cannot record, and on every finished signup. Label and VK ids only, never the mailbox. Fire and forget: Slack being down never slows or fails a signup. A SECRET: the URL is the whole credential. Unset = log lines only. |
| `SQUELCH_CONTROL_LOG` | no | `tracing` filter. Default `info`. |

There is deliberately **no age recipient variable**. It existed in v1, when one
key opened every tenant; under v2 the recipient arrives per signup in the answer
to the first provisioning call and this process never holds one between requests.
An old `SQUELCH_CONTROL_AGE_RECIPIENT` left in a deployment's environment is now
ignored, so delete it: a variable that looks load-bearing and is not is worse
than no variable at all.

The Google endpoints are **pinned constants**, deliberately not environment
variables: those requests carry the client secret, and "which host do we send
the secret to" must not be a deploy-time typo. Resend's and WorkOS's API origins
are pinned for the same reason; tests point them at stubs by building the config
struct directly.

## CLI

```sh
squelch-control serve                     # the signup site
squelch-control invite issue --count 5    # mint codes; printed ONCE, on stdout
squelch-control invite issue --ttl 7      # ...good for a week instead of 30 days
squelch-control invite list               # ids and status. Never codes or hashes
squelch-control invite revoke <id>        # revoke an unused code
squelch-control tenants                   # what has been provisioned
squelch-control llm mint <label>          # mint + install a Bifrost virtual key (also rotates)
squelch-control llm revoke <label>        # revoke the recorded key and forget it
squelch-control llm sync                  # converge the gateway's model config onto this one
squelch-control llm sync --check          # ...report only. Exits 1 when anything has drifted
squelch-control drift                     # what has been changed on every tenant's workload
squelch-control drift <label>             # ...on one. Exits 1 when anything has drifted
squelch-control reconcile <label>         # converge ONE tenant back onto the warden's render
squelch-control import-sqlite <path>      # one-time cutover: copy the legacy SQLite store into
                                          # Postgres, ids preserved. Refuses a non-empty target
```

Invite codes are Crockford `XXXX-XXXX-XXXX-XXXX` (80 bits), stored only as a
lowercase hex SHA-256, single use, and good for 30 days unless `--ttl` says
otherwise. A lost code is re-issued, never recovered. Codes minted before the
space grew are still accepted and expire 30 days after they were issued.

They are longer than the daemon's pairing codes on purpose. A pairing code is
read aloud, lives for minutes, and only works against a daemon the guesser must
already be able to reach; an invite is pasted out of an email, sits in a table
for weeks, and one hit against a public form is a free tenant.

A code is **held** from the moment the form is posted until the signup finishes
or fails, so one code cannot open two signups at once. The hold names the
session, lapses with it, and is handed back immediately when a signup fails, so
a retry does not have to wait it out.

The invite commands talk to the store directly and need none of the serving
configuration, so codes can be minted on a box with no OAuth client in its
environment.

`drift` and `reconcile` need the **warden pair** and, for a fleet-wide `drift`,
the store; no Bifrost trio, no OAuth client, no cookie key.

The warden writes every tenant object by server-side apply, which owns **fields,
not objects**. A `kubectl set env` against a tenant therefore takes ownership of
that field, and no apply the warden makes afterwards will report it or take it
back — it converges around the edit forever, and the edit detonates whenever
something unrelated makes it fatal. `drift` is how that gets seen first: it asks
the warden which other field managers own part of a tenant's Deployment and what
an apply of today's render would move. The report is data on **stdout**; the
exit code is the finding (`0` clean, `1` drifted), so a periodic run needs
nobody to read it.

`reconcile` is the repair, and it takes exactly one label on purpose. Taking a
field back from another manager means deleting the Deployment and applying it
fresh, so it rolls that tenant's pod and their mailbox is offline until it
returns. Converge one, verify it, then walk the list; a command that swept the
fleet from here would turn one bad render into a fleet-wide outage at the speed
an operator can press return.

The fleet IS swept, in the cluster, by `squelch-warden roll`
(`deploy/hosted/90-warden-roller.yaml`), and it earns that by doing what an
operator pressing return cannot be relied on to do: one tenant at a time, each
rollout finished before the next is touched, a halt on the first tenant that
does not come back, and no repair at all on a tenant where the repair means
deleting a live workload — which is precisely the case this command exists for.

That sweep is the **Deployment** and nothing else, because that is all a drift
report covers. `reconcile` re-applies all five of a tenant's objects, so a change
to a tenant's Service, Ingress, NetworkPolicy or PVC reaches an existing tenant
through this command and through no timer.

The `llm` commands need the Bifrost trio, the warden pair, and the store — but
still no OAuth client or cookie key. `llm mint` backfills a tenant that signed
up while Bifrost was down (signup is fail-soft about the key: an outage costs
the tenant its key, never the signup), and rotates one that has a key already;
rotation prints the old key's id, which stays live in Bifrost until revoked
there. Key **values** are never printed, stored, or logged — only ids are.

`llm sync` is the exception to that paragraph twice over: it needs **neither
the store nor the warden**, only the Bifrost trio, because it reads and writes
the gateway's own model configuration and nothing else. The operator most
likely to run it is one whose fleet cannot reach its model, and making that
person assemble a Postgres URL and a warden bearer first is how a small outage
becomes an unreadable one.

What it converges is the **provider key's `models` list**, which is the second
place a model has to be allow-listed and the one nothing owned. A virtual key's
`allowed_models` is matched against the id the daemon sent
(`anthropic/claude-opus-5`); the provider key's list is matched after the
provider prefix is resolved away (`claude-opus-5`). A model in the first and
not the second answers `400 no keys found that support model`, which is how
every hosted tenant ran heuristic-only triage for four days in August 2026
while each tenant's virtual key, the warden's config and the daemon all named
the model correctly. Run it after **every** change to the fleet's model, and
run `--check` to ask whether they agree right now.

Virtual keys are **reported, never rewritten**: converging one here would leave
the pod holding whatever the warden last installed, so the fix it names is `llm
mint <label>`, which rotates both ends. The report also flags any allow-list
entry with leading or trailing whitespace, on managed and unmanaged keys alike,
because the gateway matches exactly and an entry typed as `claude-opus-5 `
renders as correct everywhere a human would look while matching nothing.

## Tests need a real Postgres

The store is Postgres, so its tests refuse to run against anything else — a
fake would test the fake. Each test creates (and later reaps) its own
throwaway schema, so one local instance serves the whole parallel suite:

```sh
docker run -d --name squelch-pg -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres:16
export SQUELCH_TEST_PG_URL=postgres://postgres:postgres@localhost:5432/postgres
cargo test -p squelch-control
```

Unset, the tests panic with this same remedy rather than skipping: a suite
that can be silently skipped is a suite CI could silently skip too.

## Deploying to Railway

A **separate Railway service** off this same repo, alongside the APNs relay
(root `Dockerfile`) and the consent broker (`Dockerfile.broker`). The one thing
that decides which of the three a service builds is its **config-as-code file**,
so that is step 1:

1. New service from the repo. Set its **Config-as-code file path** to
   `railway.control.toml`, which pins `dockerfilePath = "Dockerfile.control"`
   along with the health check and restart policy.

   Do not rely on `RAILWAY_DOCKERFILE_PATH` for this. Left at the default,
   a service inherits the root `railway.toml` — which belongs to the relay and
   pins `dockerfilePath = "Dockerfile"` — and **config-as-code outranks the
   service variable**. This service's first deploys built and shipped the relay
   image with the variable set correctly, which is a green deploy serving the
   wrong binary and no error anywhere. Setting the variable as well is harmless;
   it is just not what makes this work.
2. Add a Railway Postgres service to the project and set
   `SQUELCH_CONTROL_DATABASE_URL = ${{Postgres.DATABASE_URL}}` on this service.
   (The `/data` volume is only a transition artifact: it holds the pre-Postgres
   SQLite file until `import-sqlite` has run once, then it is retired.)
3. Set every required variable from the table above.
4. Add the custom domain for the signup surface (`signup.passband.app` — the
   product domain, never the tenant base domain) and point a CNAME at the
   Railway target. The same hostname must be registered as an authorized
   redirect URI (`https://signup.passband.app/oauth/callback`) on the web
   OAuth client in Google Cloud Console.
5. Health check: `GET /healthz`.

DNS for the rest of the hosted tier (`*.<base domain>` tenant wildcard and
`warden.passband.app` pointing at the k3s node) is in `deploy/hosted/SETUP.md`;
what the live deployment actually looks like — box, domains, secret inventory —
is `deploy/hosted/PRODUCTION.md`.

## Notes on the surface

- No JavaScript, no external assets, `default-src 'none'` CSP, `no-store`, and
  `X-Frame-Options: DENY` on every page. The Passband masthead in the top left
  is subject to that rather than an exception to it: the mark is an inline
  `<svg>` (markup, not a fetch, so `img-src` stays `'none'`) and the wordmark's
  serif is whatever the reader's platform has, because there is no webfont here
  either. It is not a link; the one honest destination would be the marketing
  site, and that is a door out of a signup, not a masthead.
- Per-route rate limits: the form is metered like a page, `POST /signup` is
  tight (it is the one route where a stranger can guess at a secret), and
  `/oauth/callback` is the most generous, because refusing it destroys a consent
  the user has already granted.
- Every invite failure is one message. Wrong, spent, revoked, and malformed are
  indistinguishable from the outside.
- `/mcp` does not exist here. On a tenant it is published only when the warden
  is configured for agent OAuth, and then the daemon accepts nothing but an
  AuthKit token whose `email` is its own mailbox; `/agent/auth` above is this
  service's whole part in that.
