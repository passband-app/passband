# Google OAuth verification

Passband requests one **restricted** Gmail scope, `gmail.modify` (and, optionally, the
Google Calendar pair, one sensitive and one not; see the table below). Until Google verifies the
project, consent is capped at 100 Google accounts, forever, and nothing else
moves that number. This is the record of what the submission says, what it is
blocked on, and what a reviewer will find if they look.

`docs/HOSTED.md` decided the shape and is not relitigated here: **one Cloud
project, one consent screen, two OAuth clients** (Desktop for self-host, Web for
hosted), therefore one verification and one security assessment.

## The three gates, in order

| Gate | What it is | Blocks |
|---|---|---|
| **1. Brand verification** | Automated + human check of app name, logo, home page, privacy policy, domain ownership. 2-3 business days. | Everything below |
| **2. Restricted scope verification** | Scope declarations, justifications, permitted application type, demo video. Human review. | Consent above 100 users |
| **3. CASA Tier 2** | Independent security assessment by an App Defense Alliance authorized lab: DAST scan against production plus a self-assessment questionnaire. ~$540-1,000, and the long pole. Repeats **every 12 months**. | Final approval |

Total realistic lead time from a clean submission: **4-12 weeks**.

### Why CASA is not optional for us

Google requires the assessment of any app that "has the ability to access data
from or through a third-party server." Self-host alone would very likely have
escaped it: a Desktop client, loopback consent, mail that never leaves the
user's disk. **Hosted is what triggers it.** Tenant pods on `carrier` hold the
mail, the control plane holds sealed refresh tokens, and `bifrost.passband.app`
carries message content in transit. One project means the Desktop client
inherits that obligation. That tradeoff was made deliberately; this is the bill.

## Blockers

Ordered by what stops the submission soonest.

- [ ] **Check the publishing status is In production, not Testing.** Google
      Auth Platform → Audience. In Testing, Google expires every refresh token
      seven days after consent, so every hosted user's mailbox goes dark a week
      after signup until they reconnect. That looks exactly like churn. In
      production but unverified, tokens last, and the cost is the "Google
      hasn't verified this app" interstitial at signup plus the 100-user cap.
      Verification removes both.
- [ ] **The consent screen still says "Squelch."** It must say **Passband** —
      the name on the home page, in the app, and on the App Store listing.
      A mismatch fails brand verification on the first automated pass.
      (`docs/HOSTED.md`, "Naming", has owed this since 2026-08-03.)
- [ ] **Verify `passband.app` in Google Search Console**, as an owner, from the
      same Google account that owns the Cloud project. Brand verification checks
      the top private domain of every URL on the consent screen.
- [ ] **Declare `https://passband.app/about` as the application home page** —
      not `https://passband.app`. The React homepage serves a 1,328-byte empty
      `#root` with no functional description and no privacy link, which is
      exactly what the automated home page check looks for. `/about` is static
      HTML (added 2026-08-25) and carries the description, the scope table, and
      the policy links without executing a bundle.
- [ ] **Confirm what Bifrost retains.** The gateway keeps its config and log
      stores as SQLite under `/app/data` on EFS (`bifrost_store = "efs"`,
      `deploy/aws/tofu/main/ecs.tf`). Bifrost's defaults log every request
      body, i.e. Gmail message content, to `/app/data/logs.db` for 365 days.
      On the AWS gateway content logging was turned off at cutover
      (`disable_content_logging: true`, `log_retention_days: 7`, 2026-10-26,
      `deploy/aws/CUTOVER.md`). The setting lives in Bifrost's config store,
      not in the task definition, so before the assessment re-check it under
      Config → Logs and keep the evidence. The Railway volume the gateway ran
      on before the move goes with the Railway project's deletion (CUTOVER.md
      Phase 10); confirm that has happened. **Do this before the assessment,
      not during.**
- [ ] **`/mcp` auth: confirm it is live.** Built (OAuth through WorkOS
      AuthKit, 2026-10-01), and the checked-in warden config sets
      `SQUELCH_WARDEN_MCP_OAUTH_ISSUER` (`deploy/hosted/15-warden-config.yaml`).
      Before the assessment, prove carrier runs that config: an unauthenticated
      `POST https://<label>.passband.email/mcp` answers `401` with a
      `www-authenticate: Bearer resource_metadata=...` header (the check is in
      `deploy/hosted/SETUP.md`, "Publishing the agent door"). Then tick this and
      record the date in `deploy/hosted/PRODUCTION.md`.

Cleared as of 2026-08-25:

- [x] Privacy policy rewritten to cover both tiers. The previous version claimed
      "we do not run servers that receive, store, or can access your email or
      your Google credentials," which stopped being true the day hosted shipped,
      and described LLM processing as bring-your-own-key only, which is false for
      hosted tenants routed through our own Anthropic account.
- [x] Terms of service extended to hosted accounts.
- [x] `/about` written and routed.

Cleared as of 2026-11-05:

- [x] HSTS (`max-age=31536000`) and `X-Content-Type-Options: nosniff` on every
      response from control and from tenant daemons
      (`squelch-control/src/transport.rs`, `squelch-api/src/transport.rs`).
      Neither service sent HSTS before, and a missing one is the first thing a
      DAST scan reports on each page. The daemon sends it only when Traefik
      says the request arrived over https, so self-host on loopback is
      unchanged. The marketing site already had it from CloudFront's
      `Managed-SecurityHeadersPolicy`.

## What gets submitted

### Permitted application type

**Email client.** Passband is a mail client for macOS and iOS; the Gmail
mailbox is the entire product surface. No other type on Google's list applies.

### Scope declarations

Declare exactly these on the Data Access page. Anything not on this list is a
finding.

| Scope | Class |
|---|---|
| `https://www.googleapis.com/auth/gmail.modify` | Restricted. **The only Gmail scope** (since 2026-10-27) |
| `https://www.googleapis.com/auth/userinfo.email` | Sensitive |
| `https://www.googleapis.com/auth/userinfo.profile` | Non-sensitive |
| `openid` | — |
| `https://www.googleapis.com/auth/calendar.events` | Sensitive (as the Cloud console classes it, 2026-10-27). **Optional**: requested at hosted signup and by the calendar grant |
| `https://www.googleapis.com/auth/calendar.calendarlist.readonly` | Non-sensitive (as the Cloud console classes it, 2026-10-27). **Optional**: requested at hosted signup and by the calendar grant |

`userinfo.profile` is requested and never required: it feeds the account's name
and photo in the client (`squelch-core/src/sync/profile.rs`), read through the
**People API**, which therefore has to be ENABLED on the Cloud project. Without
it the call answers 403 and the client shows an initial, the same as a grant
that never carried the scope.

The constants are pinned in `squelch-core/src/config.rs` and re-pinned in
`squelch-broker/src/validate.rs`; the signup floor is tested in
`squelch-control/tests/signup_flow.rs`. Keep this table and those constants in
agreement.

The two calendar scopes are never required. Hosted signup asks for them
beside the Gmail scopes (since 2026-10-27, even while the calendar feature is
off for that tenant), so they appear on the signup consent screen a reviewer
sees. Existing hosted tenants are asked at
`signup.passband.app/reconnect?calendar=1`, which asks for the Gmail
scopes again plus these two in one consent, and self-host by `squelchd auth
--calendar`. A user who unticks them at Google keeps a working mailbox and
nothing is installed. Every grant flow sends `include_granted_scopes=true`,
and a Gmail reconnect asks for the two calendar scopes again (optional, since
2026-10-29), so a repair restores calendar even when the grant it replaces was
revoked.
`docs/CALENDAR.md` §8 planned to file calendar as an amendment after the Gmail
submission. Now that signup asks for it, the reviewer sees these scopes on the
first consent screen, so they belong in the Gmail submission itself, with the
calendar feature shown in the demo video.

### Justifications

Paste these into "describe how you will use the restricted scopes and why more
limited scopes aren't sufficient." Every one of them names the narrower scope
Google will ask about and says why it does not work, because the rejection is
otherwise automatic.

**`gmail.modify`** (the only Gmail scope requested)

> Passband is an email client. It reads the user's messages to display them,
> thread them, and search them, and to run the triage that is the product's
> reason to exist: each message is scored so that the inbox holds only what
> needs attention and the rest is filed. Triage reads the message body, not just
> its headers, because sender and subject alone cannot distinguish a receipt
> from a bill, a newsletter from a personal note, or a routine notification from
> an account-security alert. The same body text is what the app extracts
> deadlines, package tracking numbers, and one-time verification codes from and
> surfaces as cards.
>
> Passband then writes back exactly the decisions the user or their own triage
> rules made: archive a message (remove the `INBOX` label), add or remove a
> user label, mark read or unread, and return a thread to the inbox at a time
> the user asked for. These are `users.messages.modify` calls, and no narrower
> scope permits them. And it sends the mail the user writes in it: replies,
> forwards, new messages, and invitations the user chooses to share from their
> own address, always user-initiated from the composer.
>
> One scope covers all three, so Passband requests only `gmail.modify`.
> Requesting `gmail.readonly` and `gmail.send` beside it would add nothing the
> grant does not already cover. `gmail.metadata` is not sufficient: it returns
> headers and labels with no body. `gmail.labels` governs label *definitions*
> and cannot apply a label to a message, so it cannot archive.
> `gmail.addons.current.message.*` is not applicable: Passband is a native
> macOS and iOS application, not a Gmail add-on.
>
> Passband deliberately does **not** request `https://mail.google.com/`, which
> would additionally grant permanent deletion and full IMAP/SMTP access. It
> never deletes a message permanently and has no code path that can. It does
> not request `gmail.compose` or any `gmail.settings` scope.

**`calendar.events`** (optional)

> Passband shows the user's own calendar beside what it finds in their mail:
> the events they already have, next to the flights, reservations and meetings
> their mail mentions, so they can see a day at a glance and know when they are
> free. It reads events with `events.list` on the primary calendar and on every
> calendar the user has chosen to show in Google Calendar.
>
> `calendar.events.owned` is not sufficient: it covers only calendars the user
> owns, and the calendars people actually live by are often ones they subscribe
> to and do not own (a partner's, a team's, a school's). An event the timeline
> cannot see on a shared calendar is a double booking it cannot warn about.
> `calendar.readonly` and `calendar.events.readonly` are not sufficient either:
> later releases let the user add an event Passband found in their mail to their
> calendar and answer an invitation from the app, and asking for a read scope
> now would mean a second consent screen for every user later. Passband
> deliberately does not request `https://www.googleapis.com/auth/calendar`,
> which would additionally grant creating, deleting and re-sharing whole
> calendars and changing their settings.

**`calendar.calendarlist.readonly`** (optional)

> Passband reads the list of calendars the user subscribes to, and which of them
> they have chosen to show, so it reads the same calendars Google Calendar
> displays and no others. It never changes the list. Without this scope only the
> primary calendar is read.

### Demo video

Unlisted on YouTube. Must be in English, and must show all four of: the consent
flow a real user sees, the **App Name** rendered correctly on the consent
screen, the **OAuth client ID visible in the browser address bar** during
consent, and each requested scope actually doing something in the app.

Shot list. Do not cut between the address bar and the consent screen — the
reviewer needs one continuous frame proving that client ID granted that consent.

1. **Home page.** `passband.app/about` in a browser. Read the first line aloud.
   Establishes what the app is and that the name matches.
2. **Hosted consent (Web client).** Start at `signup.passband.app`, click
   through to Google. Hold on the consent screen for a slow five seconds with
   the address bar in frame and legible: the URL must show `client_id=...` and
   the screen must show **Passband**. Scroll the consent screen so every
   permission is visible: the one Gmail permission (`gmail.modify`) and the two
   calendar ones. Approve.
3. **Self-host consent (Desktop client).** Run `squelchd auth` in a terminal,
   let it open the browser, and hold the same shot: address bar with the second
   client's `client_id`, consent screen showing **Passband**. Approve. This is
   the second client under the same project and the reviewer should see it
   exists rather than discover it.
4. **`gmail.modify` reading.** The app after first sync: the triaged inbox,
   open a thread and scroll it, run a search that returns results, show an
   extracted card (deadline or shipment) next to the message it came from.
5. **`gmail.modify` changing mail.** Archive a message and show it leave the inbox.
   Apply a label and show it on the message. Park a thread for later and show it
   return. Then show the same messages in **Gmail's own web interface** so the
   reviewer sees the change actually landed on Google's side.
6. **`gmail.modify` sending.** Compose a message, send it, and show it arrive in
   the recipient's mailbox and in the sender's Gmail Sent folder.
7. **Calendar.** Signup asks for it (since 2026-10-27), so it is in this
   submission: the signup consent screen showing the calendar permissions
   beside Gmail's, and the calendar page (feature on for the demo account)
   showing real events next to ones found in mail.
8. **Revocation.** `myaccount.google.com/permissions`, showing Passband listed
   and revocable. Not required, but it answers the question a reviewer is paid
   to ask.

### Documentation links

Up to three. Use:

1. `https://passband.app/privacy`
2. `https://github.com/passband-app/passband/blob/main/docs/SECURITY.md`
3. `https://passband.app/about`

## CASA Tier 2

Engage an App Defense Alliance authorized lab once scope verification is under
way; the assessment and Google's review can overlap, and the LOA date starts the
12-month clock, so do not start it early for nothing.

### Which route

Google's restricted-scope review emails a request for the assessment, naming
the tier. When it arrives:

- **Lab: TAC Security** (the lab Google points developers to, Tier 2 from
  about $540 per app per year as of 2025; check the current price). The lab
  runs the DAST scan itself, and you answer its questionnaire. This is the
  default: one invoice, and an assessor to argue scope with.
- **Self-scan** (`appdefensealliance.dev/casa`): run the CASA-configured scan
  yourself, fix the CWEs it flags, submit the results and the questionnaire
  in the CASA portal, and a lab validates them. Cheaper in money, dearer in
  operator time. Only worth it if the lab quote is unexpectedly large.

Either way the Letter of Validation goes to Google from the portal.

### Pre-scan before engaging the lab

Run the scan the lab will run, first, so its first report is clean. OWASP ZAP's
baseline scan is passive (it spiders and reads responses, no attack traffic),
so it is safe against production. From a laptop, not this repo's CI:

```sh
for t in https://signup.passband.app https://passband.app/about \
         https://<label>.passband.email https://relay.squelch.sh; do
  docker run --rm -t ghcr.io/zaproxy/zaproxy:stable zap-baseline.py -t "$t" -I
done
```

`bifrost.passband.app` answers only our own egress and `carrier`, so scan it
from carrier or note it to the assessor as not publicly reachable. Fix anything
Medium or above before the lab's scan, and write down the reason for every Low
that stays.

**Assessment scope is everything that touches Gmail data**, which is more than
the daemon:

| Surface | Why it is in scope |
|---|---|
| `signup.passband.app` (`squelch-control`) | Runs the OAuth exchange, seals the refresh token |
| `*.passband.email` tenant vhosts | Serve `/client`, `/console`, `/t` off the tenant's own mail |
| `bifrost.passband.app` | Message content in transit to Anthropic. Source-restricted: it answers only our own egress and `carrier`, so it is not publicly reachable |
| The push/read-receipt relay | Opaque tokens only, but it is our surface |
| `carrier` (Hetzner) and the k3s cluster | Where the mail and the sealed tokens rest |

Have ready before the DAST scan, because the questionnaire asks for all of it:
the threat model (`docs/SECURITY.md`), the tenant isolation story (namespace,
NetworkPolicy, per-tenant age identity), secret custody
(`deploy/hosted/PRODUCTION.md`, "Backups" and the custody table), the
encryption-at-rest posture (k3s `--secrets-encryption`, age-encrypted Litestream
to R2), and the deletion path.

## What a reviewer or assessor will poke at

Answers we should have written down before we are asked, not after.

- **Embedded client credentials in a public Docker image.** The self-host
  Desktop client's id and secret ship in `ghcr.io/passband-app/squelchd`. This is
  the sanctioned installed-app model — Google treats Desktop client secrets as
  non-confidential and it is what rclone and Thunderbird do — and
  `docs/BROKER.md` sets out why the alternatives are worse. Be ready to say so
  in one paragraph.
- **Message content reaching Anthropic.** Disclosed in the privacy policy for
  both tiers. The load-bearing facts: no training on the content,
  security-sensitive mail is detected deterministically and never sent to a
  model at all, and self-host requires the user's own key.
- **Why both a Desktop and a Web client.** A refresh token is bound to the
  client that minted it; routing self-host refreshes through our infra would put
  our uptime in the path of every hourly refresh and every token in our reach.
  `docs/HOSTED.md`, "OAuth architecture."
- **The 100-user cap.** Know the current consented-account count before
  submitting, and do not cross it while the review is open.

## Annual renewal

Restricted scope access lapses unless the app is re-verified and re-assessed
within 12 months of the assessor's Letter of Assessment. Google emails a
reminder; do not rely on it. Put the LOA date here when it arrives, and a
calendar reminder at LOA + 9 months, because a lapse is a fleet-wide outage that
looks like every tenant's token going bad at once.

**LOA date:** not yet issued.
