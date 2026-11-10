# Incident response

What Passband does when something touches user data, or might have. It backs
two public promises: breach notice within 72 hours (`passband.app/trust`,
"DPA") and a vulnerability-report acknowledgement within 72 hours
(`passband.app/privacy`, `/.well-known/security.txt`).

## Reporting a vulnerability

Send reports to `braelyn@passband.app` (also listed in `security.txt`, the
privacy policy and `/trust`). We acknowledge within 72 hours, reproduce, fix,
deploy, and then tell the reporter. If a report shows user data was reached, it
is handled as an incident.

## What counts as an incident

- A security alert that no operator action explains.
- A credential that may have left our custody (OAuth client secrets, API keys,
  service bearers, encryption identities, console logins).
- Mail or tokens of one account reaching another account, an operator, a log,
  or any party not in the subprocessor list.
- A credible vulnerability report.
- A breach announced by a provider we use.

An outage alone is not an incident unless it came from one of the above.

## Response

1. **Record the time** of awareness. The 72-hour clocks start here. Keep a
   running log of what was seen and done.
2. **Contain first.** Revoke or rotate whatever is in doubt. Rotating a secret
   that turns out to be safe costs minutes; leaving a live one exposed does not.
3. **Preserve evidence.** Keep logs and volumes that may be needed, and export
   anything that would age out of retention.
4. **Establish the blast radius**: which accounts, which data, which window.

## Notification

- **Affected users**: within 72 hours of becoming aware of a breach that
  affects their data, by email from `braelyn@passband.app`: what happened, what
  data, the window, what we did, and what they should do (for example, review
  `myaccount.google.com/permissions`).
- **Google**: incidents involving data obtained through Google APIs are
  reported to Google as its API Services User Data Policy requires.
- **Regulators**: where GDPR or a US state law applies, within its deadline
  (72 hours under GDPR Art. 33).
- **Customers with a signed DPA**: whatever their DPA says, if stricter.

## Afterwards

Within two weeks of an incident, a short written review (what happened, why,
what changed), and any documentation that turned out to be wrong is corrected.

## Keep current

`security.txt` carries an `Expires` date (RFC 9116); it is renewed, and this
file re-read, once a year.
