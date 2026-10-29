# Hidden features

A hidden feature is one the build has and most people cannot see yet. An
operator turns it on per tenant from the admin site; the tenant's daemon and
apps follow within a few minutes, with no restart and no release.

A flag is a key and nothing else: on or off, per tenant. No percentages, no
variants, no payloads. A flag that needs a value is configuration.

## How a flag travels

```
/admin/features  ->  control plane (Postgres)  ->  GET /tenant/features
                                                        |  daemon, every 5 min
                                                        v
                                         Features handle in squelchd
                                                        |
                                    /client/stats  "features": [...]
                                                        v
                                        Mac and iOS: store.hasFeature(.x)
```

- **Control plane.** `tenant_features` holds one row per (tenant, feature) that
  is on; `feature_defaults` holds the features that are on for everyone. The
  admin page at `/admin/features` is a grid of tenants against features plus an
  "Everyone" row, behind the same admin token as the waitlist.
- **Daemon.** Asks `GET /tenant/features` with its control token
  (`SQUELCH_CONTROL_TOKEN`, the same one invites are minted with) at boot and
  every five minutes. Nothing is pushed and no pod is rolled.
- **Apps.** Read the list off `/client/stats`, which they already poll.

Off is the answer to every doubt. No control plane, an old daemon, an old
control plane, a key nobody recognises: all read as "not enabled".

| The control plane says | The daemon does |
|---|---|
| a list | replaces what it held |
| nothing (down, deploying, 404, 5xx) | keeps what it last heard, asks again in a minute |
| 401 (token revoked, tenant cancelled) | clears what was granted |

Two consequences worth knowing:

- **A tenant with no control token has no hidden features.** The token is
  installed at signup; a tenant from before that needs `squelch-control share
  mint <label>`. The admin page marks those rows "no control token"; a box
  ticked there is stored and goes live when the token is minted. `share revoke` turns their hidden features off along with
  sharing, because it is the same credential.
- **A pod that restarts while the control plane is down comes up with nothing
  on** until the control plane answers. The list is held in memory.

## Adding a flag

1. **Name it** in `squelch-core/src/features.rs`: a `pub const` for the key and
   an entry in `KNOWN` with one sentence for the operator. The admin page is
   drawn from `KNOWN`, so this is required even for a flag only the apps read.

   ```rust
   pub const RENOTIFY: &str = "renotify";

   pub const KNOWN: &[Feature] = &[Feature {
       key: RENOTIFY,
       summary: "Smart re-notify: a second banner for mail that still needs handling, when the \
                 Mac comes online or a meeting ends.",
   }];
   ```

2. **Gate the daemon** wherever it matters. In an API handler:

   ```rust
   if !state.features().enabled(features::RENOTIFY) {
       return Err(ApiError::not_found());
   }
   ```

   Outside `squelch-api`, clone the handle (`api_state.features().clone()`) into
   the subsystem at boot. Ask at the moment of the decision; the answer can
   change between two calls, which is the point.

3. **Gate the apps.** Add the key to `HiddenFeature` (an extension next to the
   feature's own code is fine) and check it where the UI would appear:

   ```swift
   extension HiddenFeature { static let renotify = HiddenFeature(key: "renotify") }

   if store.hasFeature(.renotify) { RenotifySettingsRow() }
   ```

4. **Ship.** The control plane deploys from `main` and the box appears on
   `/admin/features`. The daemon and apps need their usual release for the
   gated code to exist at all; the flag only decides who sees it.

A key is a lowercase letter followed by lowercase letters, digits and
underscores, at most 48 characters.

## Seeing a hidden feature in development

Pin it on the daemon you are pointed at:

```sh
SQUELCH_FEATURES=renotify,agent_send squelchd serve
```

Pinned keys are on regardless of any control plane, and the app learns them the
same way a tenant's app does. This is also how a self-host turns one on. No
hosted pod sets the variable, so it is not a way around the admin page.

## Launching

Tick "Everyone" to turn a feature on for the whole fleet, new signups included.
When it has settled, delete the flag: the `KNOWN` entry, the constant, and every
check on it, in the daemon and the apps, and add the key to `RETIRED` in
`squelch-core/src/features.rs` (a test there fails if a `KNOWN` key is ever a
retired one). Leftover rows in the two tables are harmless and saving the admin
page does not touch them. (With no users yet, a launch can skip the "Everyone"
step and go straight to deleting the flag, as `calendar` did.)

**Never reuse a key.** Those leftover rows are still served to daemons, so a
retired key given to a new feature arrives already switched on for whoever had
the old one, which after an "Everyone" launch is the whole fleet.

## Retired keys

These were hidden features and have launched. Do not give their keys to anything
new. Any `tenant_features` or `feature_defaults` rows for them are leftovers:
still stored, still served on `GET /tenant/features` and in `/client/stats`
`features`, and read by nothing.

| Key | What it was | Retired |
|---|---|---|
| `calendar` | The Calendar page, the timeline, and the Google Calendar connection (docs/CALENDAR.md §11) | 2026-10, launched for everyone |

## What this is not

Not a security boundary on the client. The app hides what is not listed, and a
person with a debugger can unhide it; anything that must not happen for a
tenant has to be refused by the daemon too (step 2).
