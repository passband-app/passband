# Auth mail: handle each kind for what it is

Status: building, 2026-10-26. Branch `auth-kinds`, on top of PR #554 (prompt
v11, security incidents are not auth).

## Why

Every auth kind used to get the same treatment: sealed, filed on the Auth
page, kept out of FYE, and (for alerts, resets and sign-in links) a
"That was me / Not me" card for 7 days, with the verdict kept in one Mac's
UserDefaults. A survey of 65 real auth messages (Aug 21 to Oct 7) found that
shape fits about a quarter of them:

| Group | Count | Needs |
|---|---|---|
| Codes and links you asked for | 8 | The code, fast. Worthless in 15 minutes. |
| Sign-in receipts ("you shared Google Account data with X") | 14 | Nothing. Proof of what you just did. |
| New sign-in or device | 16 | A question only when nothing explains it. |
| Security setting changed (recovery phone, 2FA phone, SSH key, token) | 8 | More weight than a sign-in: this is what an attacker does to stay in. |
| App granted access | 4 | Say what was granted, to whom. |
| Card added to a wallet | 2 | Mostly a receipt; fraud if not you. |
| Finish setup ("verify your email") | 5 | A to-do. |
| Not auth at all (fraud checks, SIM changes, app permission requests, privacy reminders) | 8 | Somewhere else. |

Two patterns: **pairs** (a sign-in alert lands within a minute of the code or
Google receipt that explains it: 5 of the 16) and **bursts** (a new phone
produces ~9 new-device alerts across services in a week).

## Decisions (owner, 2026-10-26)

1. New auth kinds, not a severity field.
2. Sign-in receipts stay visible on the Auth page, with no question card.
3. Critical account changes sit in FYE until the owner rules "That was me".
4. Verdicts move to the server.

## Contract

### Kinds

`AuthKind` offered to the model: `otp`, `password_reset`, `sign_in_link`,
`verification`, `login_alert`, `account_change`, `app_grant`,
`signin_receipt`. `security_alert` still deserializes (stored rows) but is no
longer offered in the schema.

- `login_alert`: a sign-in or a new/remembered device on the owner's account.
- `account_change`: a security setting on the owner's account changed:
  recovery email or phone, 2FA method or phone, password, account email,
  phone number or SIM, SSH key, access token, passkey, trusted device, card
  added to a wallet, account locked or unlocked.
- `app_grant`: a third-party app was granted access to the account (OAuth app
  authorized, "X was granted access to your Google Account").
- `signin_receipt`: a notice that the owner used one account to sign in to
  another service ("You shared some Google Account data with X").

`AuthAssessment.critical: bool` (serde default false). True only with
`account_change`, for takeover-class changes: recovery email or phone changed
or removed, phone number or SIM moved or activated, 2FA turned off or moved to
a new device, password changed, account email changed. Normalized to false
when `account_change` is absent (normalize, do not fail the run).

Not auth (prompt re-routes, no auth kinds unless a code or link of its own):
- Fraud and transaction confirmations: `account_service`, FYE, `needs_user`,
  high urgency.
- App permission requests ("X is requesting updated permissions"):
  `account_service`, FYE, `needs_user`, a review action.
- Privacy reminders (location sharing and the like): `account_service`,
  informational.
- Security incidents: as v11.

`PROMPT_VERSION` = `agent-triage-v13`.

### The kind a client sees

One kind per message, picked by priority across `auth.kinds` (not
`kinds[0]`): `otp` > `sign_in_link` (wire `magic_link`) > `password_reset` >
`verification` > `account_change` > `app_grant` > `login_alert` >
`security_alert` > `signin_receipt`. Defined once, used by every place that
reads a sealed kind.

### Explained alerts (pairing)

Revised after security review (2026-10-26): the first version let an attacker
manufacture the mail that "explains" their own alert (anyone can trigger a
reset or a signup verification; a consent screen produces both a grant and a
receipt from google.com; a forged receipt subject named any service).

Only a NON-critical `login_alert` can be explained. `account_change`,
`app_grant` and `security_alert` never are. A login alert M is explained by E
when ALL hold:

- E is either
  - an `otp` or `sign_in_link` message the owner revealed (an audited
    reveal row for it, dated no later than 60 seconds after M arrived), from
    the same service as M; or
  - Google's own sign-in receipt: sender exactly `noreply-accounts@google.com`
    and subject exactly "You shared some Google Account data with <App>",
    where <App> (and only <App>) names M's service. Google hosts mail other
    people write (Docs shares, Calendar, Forms), so the domain alone proves
    nothing. Apple and Microsoft receipts are not used (none in the survey,
    templates unverified);
- E passed sender authentication (`messages.auth_pass = 1`);
- E arrived before M, or at most 60 seconds after it, and at most 5 minutes
  before it;
- E has no `not_mine` verdict, and M has no verdict at all (a ruled alert is
  never explained);
- E is judged by its priority-picked kind (an alert that also lists a code
  kind does not become an explainer).

Same service = same registrable domain under the public suffix list (the
`psl` crate already in Cargo.lock), so `*.firebaseapp.com`, `*.myshopify.com`
and other shared hosts are separate services. Relay addresses never match.
Alerts never explain alerts. Critical changes are never explained.
Residual risk, accepted: an attacker-triggered code the owner reveals just
before the attacker's sign-in still explains it.
Computed at read time in Rust, reading done auth rows only within the window
of an open alert.

### Verdicts

Table `auth_verdicts(account_id, message_id, verdict CHECK IN ('mine','not_mine'),
decided_at, PRIMARY KEY(account_id, message_id))`. Human door only:
`PUT /client/sealed/{id}/verdict` with `{"verdict": "mine" | "not_mine" | null}`
(null clears). 404 for a message that is not auth mail. Every verdict write
leaves an audit row, like a reveal. The shredder never trashes an unruled
critical message or one ruled `not_mine` (it is evidence).

`GET /client/sealed` rows gain `critical`, `verdict`, `explained_by`
(message id or null).

### FYE membership for auth mail

Replaces the blanket exclusion. Auth mail is in FYE when the owner pinned it
or a reminder fired (unchanged), OR its verdict is `not_mine`, OR it is
critical with no verdict, unless its sender explicitly failed
authentication (`auth_pass = 0`; NULL still counts). A `mine` verdict takes it out (an explicit pin
still wins). FYE is thread-level: a thread qualifies when ANY of its not-done
auth messages does. Archiving one auth message never clears an unruled
critical message, or a `not_mine` message, in the same thread. The FYE row
shows the qualifying auth message but keeps the thread's own reminder,
snooze and activity time. Not done, not archived, as for all FYE. One predicate shared by
the FYE list and `placement`; a test pins that they agree. The FYE headline
still never uses auth mail. The agent door's access rules are unchanged:
restricted mail never reaches it, wherever it is placed.

### Clients (Mac and iOS)

- New kinds with labels "Account change", "App access", "Sign-in receipt"
  and symbols. `AuthLabels.swift` is shared with the iOS notification
  extension.
- Verdicts read from `/client/sealed`, written with the PUT (optimistic,
  rolled back on failure). If the daemon has no verdict route yet, fall back
  to the local ledger. Once it does, upload local verdicts for listed
  messages without a server verdict, then clear the local ledger.
- Question cards for `login_alert`, `account_change`, `app_grant`,
  `security_alert`, `password_reset`, `magic_link`; never for codes,
  verifications or receipts; never for an explained alert (the row shows a
  quiet "explained by your <service> code" note instead). Open window 7 days;
  14 for `account_change`, `app_grant`, `security_alert`; no window for
  critical.
- Copy: reset and sign-in link "I asked / I did not"; app grant
  "I allowed it / I didn't"; otherwise "That was me / Not me".
- Critical cards sort first and read as critical.
- Bursts: 3 or more open, non-critical `login_alert` cards (sign-ins only,
  never account changes or grants) from distinct services within 72 hours
  collapse into one card ("New device? 5 services saw
  a new sign-in") with "All me" and "Review each". Critical cards never group.
- "Not me" records `not_mine` and opens the reveal; the server puts it in FYE.
- An auth item in FYE shows its summary and a lock, never prefetches a body,
  and opens the reveal panel. Auth is detected from the feed row's own
  decision (`auth.kinds`), not only from the sealed list, so a stale or empty
  sealed list cannot turn it into a normal row. A notification tap on auth
  mail opens the reveal too. NOTE: the human thread view does NOT 404 sealed
  threads (it serves them, by design, and a test pins it). Everywhere else
  (search, All Mail, a thread that mixes a code with correspondence) keeps
  opening the normal reader exactly as before this feature.

### Rollout

Clients tolerate both daemon versions. The daemon half reaches tenants at the
next daemon roll; stored decisions keep their old kinds until re-triaged.
