# Incident response

What to do when something touches user data, or might have. This is the plan
the CASA questionnaire asks for (`docs/VERIFICATION.md`), and the procedure
behind two public promises: breach notice within 72 hours
(`passband.app/trust`, "DPA") and a vulnerability-report acknowledgement within
72 hours (`passband.app/privacy`, `/.well-known/security.txt`).

Passband has one operator. Every step below names that person, and nothing
here assumes a second responder.

## What counts

An incident is any of:

- A security alert you cannot match to something you did yourself. The AWS
  tripwires (`deploy/aws/tofu/main/audit.tf`) mail on every Secrets Manager read
  or write by a human, every ECS Exec and SSM session, every console sign-in,
  any IAM write, and any attempt to stop CloudTrail. An unexplained one of these
  is an incident by definition (`deploy/hosted/PRODUCTION.md`).
- A credential that may have left our custody: a Google OAuth client secret, the
  Anthropic key, a warden or relay bearer, a tenant age identity, the backup age
  key, the R2 token, a cookie key, a WorkOS key, a GitHub or cloud console
  login.
- Mail or tokens of one account reaching another account, an operator, a log,
  or any party not in the subprocessor list.
- A vulnerability report that is credible on its face.
- A breach announced by a provider we use (the Railway env-var breach of
  2026-10-21 was one: `deploy/aws/CUTOVER.md`).

An outage alone (Grafana's `public-endpoint-down`, `llm-canary-failing`) is
not an incident unless it came from one of the above.

## The first hour

1. **Write down the time** you became aware. The 72-hour clocks start here.
   Keep a running log from now on: time, what you saw, what you did. A text
   file is fine; it becomes the record.
2. **Contain before you investigate.** Revoke or rotate whatever is in doubt
   (table below). Rotating a secret that turns out to be safe costs minutes;
   leaving a live one in an attacker's hands does not.
3. **Preserve evidence.** Do not delete logs, pods or volumes you might need.
   CloudTrail is in S3 with log-file validation for 365 days; CloudWatch groups
   keep 30 days by default (`log_retention_days`), so export what matters
   before it ages out.
4. **Work out the blast radius**: which accounts, which data (mail content,
   sealed tokens, account records), which window.

## Containment, by credential

| In doubt | Do this | Where it is described |
|---|---|---|
| Google Web or Desktop client secret | Create a new secret on the client in Google Cloud console, deploy it (control: `passband/control/...` in Secrets Manager; tenants via warden), then disable the old one. Refresh tokens survive a secret rotation | `docs/HOSTED.md`, "OAuth architecture" |
| A tenant's refresh token or age identity | Cancel the tenant's workload, revoke the grant (the user can at `myaccount.google.com/permissions`), purge, reconnect | `deploy/hosted/PRODUCTION.md`, erasure runbook |
| Anthropic key | Revoke in the Anthropic console, put a new one in `passband/bifrost/ANTHROPIC_API_KEY`, redeploy bifrost | `deploy/hosted/PRODUCTION.md` |
| A Bifrost virtual key | Revoke in the Bifrost UI; the warden mints a new one on reconcile | `deploy/hosted/SETUP.md` |
| Warden, relay or control bearer, cookie key | New value in Secrets Manager or the carrier Secret, redeploy the service. A new control cookie key or admin token ends every admin session | `squelch-control/src/cookie.rs` |
| Backup age key or R2 token | Rotate on carrier (`/etc/litestream/env`); old backups stay readable only with the old key, so keep it offline until they age out (24 h) | `deploy/hosted/PRODUCTION.md`, custody table |
| WorkOS | Rotate the API key; revoke agent grants on the WorkOS side | `docs/HOSTED.md` |
| AWS console or Identity Center login | Revoke sessions in Identity Center, rotate MFA, review CloudTrail for the session | `deploy/aws/CUTOVER.md` |
| carrier root | Treat every Secret in the cluster as exposed: rotate all of the above | `deploy/hosted/SETUP.md`, "What this does and does not isolate" |

## Notification

- **Affected users**: within 72 hours of becoming aware of a breach that
  affects their data (the DPA promise), by email from `braelyn@passband.app`:
  what happened, what data, the window, what we did, what they should do (for
  example, review `myaccount.google.com/permissions`).
- **Google**: an incident involving data obtained through Google APIs also has
  to be reported to Google under the API Services User Data Policy and the
  restricted-scope terms the project accepted. Check the current wording of
  that policy and the reporting route in the Cloud console at the time, rather
  than trusting this line.
- **Regulators**: where GDPR or a US state law applies to an affected user, its
  own deadline (72 hours under GDPR Art. 33). Take advice when in doubt.
- **Customers with a signed DPA**: whatever their DPA says, if stricter.

## Vulnerability reports

Reports arrive at `braelyn@passband.app` (listed in `security.txt`, the privacy
policy and `/trust`). Acknowledge within 72 hours. Reproduce, fix, deploy,
then tell the reporter. If the report shows user data was reached, it is an
incident: start at "The first hour".

## Afterwards

Within two weeks: a short written review in `docs/` (what happened, why, what
changed), and any doc that turned out to be wrong, corrected. CLAUDE.md asks
for this habit already ("Verifying against production"): the docs are only as
good as the last time someone checked them against the real system.

## Keep current

- `security.txt` carries an `Expires` date (RFC 9116). Renew it with the annual
  CASA reassessment.
- Re-read this file at that same time, against the subprocessor list and the
  secret custody tables it points to.
