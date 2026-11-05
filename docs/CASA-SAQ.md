# CASA Tier 2: questionnaire answers (draft)

Draft answers for the CASA Tier 2 self-assessment questionnaire, with the
evidence behind each one. The questionnaire is built on OWASP ASVS 4.0, so this
file follows ASVS chapters. When the lab sends its own form, map each item to
the chapter here by its ASVS number; the item IDs are the lab's, not ours.

Written 2026-11-05 from the code and docs at that commit. **Re-check every
answer against production before submitting** (CLAUDE.md, "Verifying against
production"). The docs here have been wrong before, and one claim in this
file's sources was (see V8).

Each chapter ends with the gaps an assessor is likely to raise, and the answer
or fix for each. The ones that need work before the assessment are collected
at the end.

Scope is the table in `docs/VERIFICATION.md`, "CASA Tier 2": control
(`signup.passband.app`), tenant vhosts (`*.passband.email`), the LLM gateway
(`bifrost.passband.app`), the push relay (`relay.squelch.sh`), and `carrier`.

## V1 Architecture and threat model

- **Threat model**: `docs/SECURITY.md`, per surface (invariant, enforcement
  point, what must not break), published at `passband.app/security`. Isolation
  limits are stated plainly in `deploy/hosted/SETUP.md`, "What this does and
  does not isolate".
- **Where Gmail data rests**: each hosted account's mail is a SQLite database on
  its own volume, served by its own daemon in its own pod on `carrier` (k3s,
  Hetzner). The Google refresh token is sealed with age (X25519) to a
  per-account key that only that account's pod holds. Control never holds the
  identity, only the public recipient, and keeps sealed ciphertext only while a
  reconnect is pending (`squelch-control/src/seal.rs`, `reconnect.rs`).
- **Where it transits**: pod to Gmail over https; pod to the LLM gateway over
  https, then to Anthropic; pod to the in-cluster embedder (plain http, see
  V9); the user's apps to their pod through Traefik TLS. The push relay sees
  no mail content.
- **Self-host** runs the same daemon on the user's own machine with their own
  Google Desktop client grant, which never touches our servers.

**Likely asked:** "Is there a trust-boundary diagram?" Not as one picture; it
is `docs/SECURITY.md` plus `deploy/aws/DESIGN.md` plus the SETUP.md isolation
section. The `/trust` page's data-flow section is the nearest single view.

## V2 Authentication

| Surface | How a caller proves who it is | Evidence |
|---|---|---|
| Signup and console SSO | Google OAuth with PKCE S256; `state` and verifier kept server-side, one-shot, 10-minute TTL, compared in constant time; `email_verified` required | `squelch-control/src/oauth.rs`, `sessions.rs` |
| Tenant `/client` (apps) | Device bearer token: `sqd_` + 256 bits from the OS RNG, stored only as SHA-256, compared in constant time | `squelch-core/src/store/sqlite/device_tokens.rs`, `squelch-api/src/auth.rs` |
| Pairing a new device | 8-character code (40 bits), single use, 10-minute TTL, burned after 5 wrong tries | `device_tokens.rs`, `squelch-api/src/pair.rs` |
| Tenant `/console` (browser) | Same device token in an HttpOnly cookie; sign-in by pairing code or Google SSO through control; 10 sign-ins/min per IP | `squelch-api/src/console.rs` |
| `/mcp` (AI agents) | WorkOS AuthKit RS256 JWT, verified locally: exact `iss`/`aud`, RSA ≥ 2048, `email` must equal the mailbox | `squelch-mcp/src/auth.rs`, `docs/SECURITY.md` §8 |
| Admin dashboard | Static token (≥ 32 chars), constant-time compare, own rate bucket (10/min/IP), routes absent when unconfigured | `squelch-control/src/admin.rs`, `ratelimit.rs` |
| Warden and relay APIs | Static bearer (≥ 32 chars), constant-time compare; warden also IP-allow-listed to control's NAT address | `squelch-warden/src/auth.rs`, `deploy/hosted/20-warden.yaml` |

Invite and share codes are 80 and 256 bits respectively, stored hashed. Failed
auth returns a bare 401 with no detail.

**Gaps:**
- *Admin login has no MFA and no per-person identity.* One operator holds it.
  Fix before the assessment if the lab marks it: put `/admin` behind the same
  Google sign-in control already runs, limited to the operator's address,
  instead of a pasted token.
- *`/client` bearer failures are not rate-limited per IP.* Answer: the tokens are
  256-bit random, so guessing is not a practical attack (ASVS 2.2.1's intent).
- *A stranger can burn a pairing code by guessing wrong 5 times.* Denial of
  pairing only, never access. Acceptable; say so.

## V3 Session management

- Control cookies are HMAC-SHA256 signed, `HttpOnly`, `Secure` on https, no
  `Domain`. Signup flow: `SameSite=Lax`, 10 minutes. Admin: `SameSite=Strict`,
  30 days, and bound to the admin token, so rotating it ends every session
  (`squelch-control/src/cookie.rs`).
- Console cookie: `HttpOnly; SameSite=Lax; Secure`. Logout revokes the token
  server-side, and any device can revoke any other. Revocation takes effect on
  the next request (no cache) and is audited (`console.rs`,
  `device_tokens.rs`).

**Gaps:**
- *Device tokens do not expire and nothing has an idle timeout* (ASVS 3.3.2).
  These are app credentials, like a mail app's, revocable per device from the
  console and the CLI. Expect the lab to accept that for native apps and ask
  for an idle timeout on the browser console. A 30-day idle expiry on console
  sessions is the smallest fix.
- *Agent grants are revoked on the WorkOS side, by the operator* (AuthKit refresh
  tokens). A user-facing revoke is owed (`docs/HOSTED.md`).

## V4 Access control

- **Tenant isolation**: one daemon and one database per account; every store
  call is scoped by the daemon's own `account_id`; a pod reaches nothing but
  DNS and public 443 (namespace default-deny plus a per-tenant NetworkPolicy
  that blocks private ranges); Pod Security `restricted`, non-root, read-only
  root filesystem, no capabilities, own user namespace, no service account
  token (`squelch-warden/src/objects.rs`, `deploy/hosted/30-tenants-default-deny.yaml`).
- **Two doors**: the human door (`/client`, `/console`) and the agent door
  (`/mcp`) share no auth, CORS or limits. Neither accepts the other's
  credential. The agent door never holds the write credential; drafting and
  sending through it are gated by the owner's setting, failing closed
  (`docs/SECURITY.md` §4, §4c).
- **Sealed mail** (login codes, resets) never reaches the assistant or the agent
  door; external reads go through one gate that fails closed on missing or
  stale assessments (`squelch-core/src/store/sqlite/messages.rs`).
- **CSRF**: admin uses `SameSite=Strict` plus an Origin / `Sec-Fetch-Site`
  check on every POST; console uses `SameSite=Lax` plus a guard on every
  non-safe method.

## V5 Validation, sanitization and encoding

- Mail HTML is sanitized at ingest with an allow-list (ammonia); URL schemes
  limited to http, https, mailto and cid (`squelch-core/src/sync/html.rs`,
  `docs/SECURITY.md` §1). The client renders it with JavaScript off, a strict
  CSP and navigation refused (§2).
- Every server-rendered page sends a CSP with `default-src 'none'` and
  `frame-ancestors 'none'` (`squelch-control/src/pages.rs`,
  `squelch-api/src/console.rs`). `passband.app` sends its own CSP
  (`deploy/aws/tofu/main/site.tf`).
- SQL is parameterized throughout. The only `format!`-built SQL interpolates
  literal table names, enums or test values (Semgrep pre-scan,
  `docs/VERIFICATION.md`).
- Outgoing mail is scanned for secrets (keys, PEM blocks, codes) before it
  leaves; a hit needs an explicit, audited override (`squelch-api/src/guard.rs`).

## V6 Cryptography

- Refresh tokens: age X25519 to a per-account recipient. The identity is
  generated by the warden from the OS RNG, written straight into the account's
  Kubernetes Secret, never logged, never escrowed (`squelch-warden/src/identity.rs`).
- Random values (tokens, codes, `state`) come from `getrandom`.
- TLS is rustls (ring) everywhere; no OpenSSL in `Cargo.lock`. Control reaches
  RDS with `verify-full` against the RDS CA.
- `deny.toml` ignores a few RustSec advisories, each with a written reason
  (for example `rsa`'s Marvin attack, used only for test keys). Have the
  reasons ready.

## V7 Errors and logging

- Clients get no internals: any unexpected error is an opaque 500
  (`squelch-api/src/error.rs`).
- Secrets stay out of logs: config `Debug` redacts them, token types have no
  `Debug`, seal errors carry no context, the embedder logs no text (tested by
  a canary), the guard logs only the kind of secret it found.
- Security events are logged without values: admin sign-in, cross-origin
  refusals, MCP and warden refusals as counts. A per-account `audit_log`
  records sends, guard overrides, token revokes and agent-access changes.
- AWS: CloudTrail (all regions, log-file validation, 365 days) and tripwire
  emails on every human secret read, shell session, console sign-in and IAM
  write (`deploy/aws/tofu/main/audit.tf`).
- Outages page through Grafana to Slack, including public endpoints and the
  LLM canary (`deploy/monitoring/grafana/provisioning/alerting/rules.yml`).

## V8 Data protection

- Mail responses are `Cache-Control: no-store` (messages, sealed reveals,
  uploads, every control and console page). Downloaded attachments are
  `private, max-age=3600`: never in a shared cache, and the native clients reuse
  inline images across renders. Defend that, or change it to `no-store` if the
  lab insists.
- **What reaches the LLM**: triage, translation and the hosted assistant send
  mail text through our gateway to Anthropic, which does not train on it. The
  triage model is what recognizes login codes, so it reads them once like any
  other message; once sealed they never reach the assistant or agents. Spam
  reaches no model. **Do not answer that sensitive mail never reaches a model.**
  An earlier line in `docs/VERIFICATION.md`, and the privacy policy until
  2026-11-05, said so, and it was not true after the triage rewrite.
- Gateway content logging is off, with 7-day log retention. That setting lives
  in Bifrost's own config store, not in IaC: screenshot it as evidence before
  the assessment (`docs/VERIFICATION.md`, Blockers).
- Backups: client-side age-encrypted Litestream to Cloudflare R2, 24-hour
  window.
- **Deletion**: by request to `braelyn@passband.app`, within 30 days, confirmed
  in writing (privacy policy). The admin purge deletes the pod, volume, sealed
  token, identity and control rows, and revokes the account's LLM keys. The
  user can end our access at once at `myaccount.google.com/permissions`.

**Gaps:**
- *Deletion is not self-service, and purge does not revoke the Google grant.*
  After purge the sealed token is unreadable (its key is gone), but Google
  still lists the app until the user removes it. Fix: have the daemon call
  Google's revoke endpoint as the first step of purge, while it still holds
  the key.
- *The purge leaves the R2 backup prefix* (it ages out in 24 hours) *and the
  PostHog person.* The erasure runbook covers both by hand
  (`deploy/hosted/PRODUCTION.md`).
- *The tenant volume has no disk-layer encryption.* The `/trust` page already
  says so. Mitigations: per-account sealed tokens, encrypted k3s Secrets,
  encrypted backups.

## V9 Communications

- Public TLS: ALB `ELBSecurityPolicy-TLS13-1-2-Res-2021-06` (TLS 1.2+, forward
  secrecy), port 80 redirects; CloudFront `TLSv1.2_2021`; Traefik with Let's
  Encrypt on carrier.
- HSTS (one year) and `nosniff` on every response from control, tenant daemons,
  the relay and the warden (`squelch-*/src/transport.rs`), and from
  `passband.app` (CloudFront).
- Internal: RDS `force_ssl` with `verify-full`; EFS transit encryption required;
  S3 policies deny non-TLS.

**Gaps:** ALB to Fargate tasks, Traefik to tenant pods, and pods to the embedder
are plain http on private networks (the embedder never receives sealed mail:
`docs/SECURITY.md` §4d). Carrier's Traefik gets a TLS 1.2 floor and an
ECDHE + AEAD suite list from `deploy/hosted/06-traefik-tls.yaml`, matching the
AWS side, once it is applied.

## V10 Malicious code and V14 configuration

- Dependencies: `cargo deny` on every Rust change and weekly; Dependabot for
  cargo, actions, Docker images and the site; gitleaks over full history on
  every push; ECR scan on push with immutable tags.
- Builds: base images pinned by digest, GitHub Actions pinned by SHA, `--locked`
  builds.
- Containers run as non-root (ECS entrypoints drop with `setpriv`; Kubernetes
  pods are non-root by policy).
- Startup refuses weak config: short admin, cookie or warden secrets fail fast.

**Gaps:** Kubernetes manifests use image tags, not digests. There is no
documented OS and k3s patch cadence for carrier, and Litestream is pinned at
an unmaintained version (the last with age encryption).

## V12 Files

- Uploads: 25 MiB cap enforced at the body limit, empty files refused, names
  and types cleaned, unclaimed uploads swept after a day.
- Downloads: only PDFs and safe image types keep their type; everything else is
  `application/octet-stream`, always `Content-Disposition: attachment` with a
  cleaned filename and `nosniff`. Another account's attachment is a 404.
- **Gap:** no malware scanning. Passband stores the user's own Gmail
  attachments and serves them only back to that user; it is not a file-sharing
  service.

## V13 API

- CORS: `/client` allows any origin because it is bearer-only and never reads a
  cookie, so a cross-origin page gains nothing it could not do with the token
  anyway. `/console` and pairing have no CORS. Control allows it on `/waitlist`
  only.
- Body limits: 4 KiB on pairing and console, 8 KiB on every control form, 2 MB
  default on `/client` except uploads.
- Timeouts: per-route response ceilings, 504 past them.
- Rate limits on every control route (per-IP buckets, forwarded headers
  trusted only from configured proxies) and on the warden.

## Before the assessment: what to fix or prepare

In priority order. The first four are code or config changes; the rest are
evidence to collect.

1. **Admin sign-in**: replace the pasted token with Google sign-in restricted to
   the operator's address, or be ready to explain the single-operator setup.
2. **Revoke the Google grant on purge** (V8).
3. **Console idle timeout** (V3).
4. `kubectl apply -f deploy/hosted/06-traefik-tls.yaml` on carrier, then the
   two `openssl` checks in that file (V9).
5. Screenshot Bifrost's logging settings (content logging off, 7 days).
6. Confirm `/mcp` OAuth is live on carrier (`docs/VERIFICATION.md`).
7. `tofu apply` so the `passband.app` CSP is live before the scan.
8. Turn on MFA everywhere the operator signs in (Google Cloud, GitHub, Hetzner,
   Cloudflare, WorkOS, Anthropic, Tailscale) and say so in the questionnaire.
   AWS already requires it.
9. Incident response: `docs/INCIDENT-RESPONSE.md`.
