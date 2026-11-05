# Litestream: tenant mailboxes to Cloudflare R2, age-encrypted

Host-level streaming backup for hosted Passband. One systemd service on
`carrier` replicates every tenant's SQLite database to R2 continuously, sealed
to an age keypair we hold, so that losing the block volume costs minutes of mail
rather than every tenant's index — and so that the copy in R2 is ciphertext to
everyone including Cloudflare.

Install and first-run: `../SETUP.md` → **"Backups: Litestream to R2"**. This file
is the shape of the thing and the restore commands.

## Pinned to litestream 0.3.13, on purpose

**0.3.x is the last line with client-side age encryption.** 0.5.x removed it and
refuses to start on a config that asks for it (`age encryption is not currently
supported, if you need encryption please revert back to Litestream v0.3.x`). We
would rather run the older line than hand Cloudflare a readable copy of every
tenant's mail, so: 0.3.13, and do not "upgrade" it without reading this
paragraph and the trade below.

What the pin costs, stated plainly: 0.3.13 shipped in October 2023 and gets no
fixes. If a bug in it eats a backup we find out the way everyone else does. The
mitigation is the restore drill in `../SETUP.md`, run on a schedule rather than
after a disaster.

The two lines also disagree about config schema, in a way that fails **silently**
and is the single most important thing to know before editing anything here:

| | 0.3.x (us) | 0.5.x |
|---|---|---|
| replicas | `replicas:` — a **list** | `replica:` — one mapping |
| encryption | `age: {identities, recipients}` on the replica | removed |
| bookkeeping | `meta-path:` per db | different mechanism |
| CLI | `generations`, `snapshots`, `wal` | `ltx`, `status` |

Feed 0.3.13 a 0.5-shaped config and it parses without a word of complaint,
lists the database, and attaches **zero replicas**. A box that looks healthy and
backs up nothing. Verified, not assumed — see the footer.

## What is in here

| File | Installs to | What it is |
|---|---|---|
| `litestream.service` | `/etc/systemd/system/` | the replicator. Overrides the .deb's unit at `/usr/lib/systemd/system/` |
| `litestream-replicate.sh` | `/usr/local/bin/` (0755) | its ExecStart: loads the age identity, refuses to start without it, execs `litestream replicate` |
| `litestream-health.sh` | `/usr/local/bin/` (0755) | reads litestream's journal, writes per-tenant metrics for node-exporter's textfile collector |
| `litestream-health.service`, `.timer` | `/etc/systemd/system/` | runs it every minute |
| `litestream-reset.sh` | `/usr/local/bin/` (0755) | the lever: every tenant onto a fresh generation, safely |
| `litestream-sync-config.sh` | `/usr/local/bin/` (0755) | renders `/etc/litestream.yml` from the tenant PVCs that exist now |
| `litestream-config.service` | `/etc/systemd/system/` | oneshot wrapper for the script |
| `litestream-config.timer` | `/etc/systemd/system/` | runs it every 2 minutes |
| `env.example` | `/etc/litestream/env` (0600, root) | R2 endpoint, bucket, key id, secret, **age recipient** |

Not in here, and not in git: `/etc/litestream/backup-age.key`, the age identity.
See "The identity" below.

## Why one process on the host, and not a sidecar per pod

The obvious design is a litestream sidecar in each tenant pod, next to the
daemon that owns the database. It is the wrong one here, for one reason that
outweighs the rest:

**A sidecar needs the object-storage write credential inside the tenant pod.**
Every tenant pod would hold a key that can write to the backup bucket. S3-style
credentials are not meaningfully scopable to "your own prefix, and only append",
so one tenant who gets code execution in their own pod could delete or overwrite
*every other tenant's* backups. That converts a single-tenant compromise into a
fleet-wide loss of the exact thing backups exist for, and it does it silently.

Host-level keeps the credential with root, which already owns the disk every one
of those databases sits on. It grants a tenant pod nothing it did not already
have. The rest of the ledger, honestly:

- **Cost.** One process, one credential, one set of logs, instead of N of each.
- **Cost.** The sidecar dies with its pod, which is the correct blast radius; the
  host process is a single point of failure for the whole fleet's replication.
  That is a monitoring problem, and a cheaper one than credential distribution.
- **Cost.** litestream is a SQLite client: it checkpoints the WAL, so it holds
  **write** access to every tenant's database. Root on the node could do that
  anyway (see SETUP.md → "Root on the node reads everything"), so this widens no
  boundary — but it is not a read-only observer and nobody should think it is.
- **Cost.** Tenants appear dynamically and litestream's config lists explicit
  paths, so the host design needs the generator below. A sidecar would not.

## How it finds tenants

Nothing tells the host about a signup. The warden creates a PVC named
`<label>-data` in namespace `tenants`, k3s local-path materialises it as a
directory on the block volume, and the daemon creates its SQLite inside:

```
/mnt/tenant-data/pvc-<pv-uid>_tenants_<label>-data/squelch.db
                 └──────────── local-path ────────┘ └ SQUELCH_DB_PATH minus /data
```

`litestream-sync-config.sh` globs that pattern every two minutes, parses `<label>`
back out of the directory name, re-validates it against the warden's own rule
(`[a-z0-9-]{3,30}`, no leading or trailing hyphen), and renders one `dbs:` entry
per tenant. It writes nothing and restarts nothing when the set has not changed.

It replicates to a **stable, human-typeable key**:

```
s3://<bucket>/tenants/<label>/store.db
```

Generating the config, rather than pointing litestream at a directory, is what
buys that key. A path derived from the directory name would bake a Kubernetes
**PV uid** into it; delete and recreate a PVC and that uid changes, so the
tenant's backup path silently moves and the old one orphans.

Generation also buys `meta-path` per database — which is a real 0.3.13 config
key, verified, not a 0.5-ism. Litestream's default is a hidden
`.squelch.db-litestream` directory **beside the database**, i.e. inside the
volume the tenant's own pod mounts read-write. `meta-path` puts it at
`/var/lib/litestream/<label>` instead, root-owned 0700, where the tenant cannot
reach the bookkeeping that describes their own backup.

Refusals worth knowing before you meet them, all loud, all exit non-zero:

- `/etc/litestream/env` missing, not root-owned, group/world readable, or with an
  empty value — including an empty `LITESTREAM_AGE_RECIPIENT`.
- `LITESTREAM_R2_ENDPOINT` spelled with a `https://` scheme. One canonical form:
  the bare host. The message tells you the `sed` that fixes it.
- Two PVC directories claiming the same label. They would replicate to one key
  and corrupt each other; a human picks which is the mailbox.
- Zero tenants found while the current config replicates some. That is an
  unmounted volume, not an empty box. `FORCE=1` overrides, once you are sure.

## Encryption: the replicator holds both halves, the env file holds neither secret

Every snapshot and every WAL segment is age-encrypted before it leaves the box.
The R2 objects begin `age-encryption.org/v1`; a leaked bucket, or a subpoenaed
Cloudflare, yields ciphertext.

- **`/etc/litestream/env` holds `LITESTREAM_AGE_RECIPIENT`** — the public key —
  next to the R2 credentials. Not the identity.
- **`/etc/litestream/backup-age.key` holds the identity** — the private key,
  root:root 0600, the only copy on disk. `litestream.service` loads it with
  systemd's `LoadCredential=`, `litestream-replicate.sh` puts it in the
  replicator's environment as `LITESTREAM_AGE_IDENTITY`, and the generated
  config references it as `identities: [${LITESTREAM_AGE_IDENTITY}]`.

### Why the replicator holds the identity (issue #696)

This file used to say the opposite: the replicator held only the recipient, so
the always-running process "could not open a single backup it writes". That
broke backups four times between 2026-09-26 and 2026-11-04, each time silently.
In litestream 0.3.13 (`replica.go`):

- `Replica.Sync` keeps the replica's upload position **in memory only**. It is
  empty on start, and a deferred reset empties it again after **any** failed
  sync, a single R2 500 included.
- With no position, `calcPos` downloads the last WAL segment it uploaded and
  measures it through lz4. It decrypts first only if `identities:` is set.
  Without them it reads age ciphertext as lz4, fails with `cannot determine
  replica position: lz4: bad magic number`, and that failure empties the
  position again. The tenant never uploads again.
- So: a restart broke every tenant whose generation had a WAL segment (the
  config timer restarts on every signup and erasure), and a transient R2 error
  broke one tenant at a time (2026-11-02 03:06 and 2026-11-04 14:43, no
  restart either time). Starting a fresh generation "fixed" it only because a
  generation with no WAL segment yet needs nothing read back.

And the split protected nothing. The key file is on this box; the replicator
runs as root with `CAP_DAC_OVERRIDE`, so it could always read the key file; and it
reads every tenant's live database, which is everything the key unlocks.
What the split did buy is kept: the identity is not in `/etc/litestream/env`,
so that file and `/etc/litestream.yml` stay safe to paste into a ticket.

The guards, so a keyless replicator cannot come back quietly:

- `litestream-replicate.sh` refuses to start without an `AGE-SECRET-KEY-1`
  line, and `LoadCredential=` fails the unit outright if the key file is
  missing. Either one shows as the `litestream-down` alert.
- `litestream-sync-config.sh` will not install a new config or restart the
  service while the installed unit lacks `LoadCredential=age-identity:`.
- The `identities:` list is a YAML flow list on purpose: in a shell that has
  not exported the key it expands to `[]`, so the metadata-only CLI commands
  (`databases`, `generations`, `snapshots`, `wal`) work without the key, and
  only a restore needs it.

> ### THE IDENTITY IS THE WHOLE BACKUP
>
> **Lose `/etc/litestream/backup-age.key` and every byte in R2 is permanently
> unreadable.** Not by us, not by Cloudflare, not by anyone. There is no escrow
> and there is no recovery.
>
> **Put it in the password manager the day you generate it** — before the first
> backup exists — beside the tenant identity Secrets dump. Those two are the only
> things on this box that no amount of re-syncing can rebuild.
>
> A key that lives only on the machine it protects is not a backup key.

If the identity is ever exposed: mint a new pair, put the new identity in
`backup-age.key` and the new recipient in `LITESTREAM_AGE_RECIPIENT`, then run
`litestream-reset.sh`, **not** a plain restart. The current generations are
sealed to the old key, and reading their last WAL segment back with the new one
is the #696 loop. Keep the old identity forever, because everything written
before the swap is still sealed to it.

## Restore, the short version

Full drills, including the ordering that keeps a fresh box from overwriting your
backups, are in `../SETUP.md`. The shape of it:

```sh
# Credentials for the CLI, exactly as the daemon gets them.
set -a; . /etc/litestream/env; set +a

# The identity, for as long as this shell lives. /etc/litestream.yml already
# references it by name; this gives the name a value.
export LITESTREAM_AGE_IDENTITY="$(grep '^AGE-SECRET-KEY-1' /etc/litestream/backup-age.key)"

# Look, without touching anything: restore a tenant to a scratch path.
DB=/mnt/tenant-data/pvc-<uid>_tenants_alice-data/squelch.db
litestream restore -o /root/scratch-alice.db "$DB"
sqlite3 /root/scratch-alice.db 'pragma integrity_check; select count(*) from messages;'
```

**If you forget the identity, the error does not say "identity".** It says:

```
cannot restore snapshot: lz4: bad magic number
```

That is litestream trying to decompress age ciphertext. It means
`LITESTREAM_AGE_IDENTITY` was empty in that shell. It does **not** mean the
backup is corrupt. (The same words in the *replicator's* journal, prefixed
`cannot determine replica position:`, are issue #696; see above.)

What actually reached R2, per tenant — 0.3.x spells this `generations` /
`snapshots` / `wal`, not `ltx`:

```sh
litestream generations "$DB"   # lag and time range
litestream snapshots   "$DB"
litestream wal         "$DB"
```

Those three only list object metadata, so they need the R2 credentials and not
the identity. `unset LITESTREAM_AGE_IDENTITY` when you are done.

## Day-to-day

```sh
systemctl status litestream litestream-config.timer
journalctl -u litestream -f
litestream databases                  # what the config covers, and that it has replicas
systemctl start litestream-config     # pick up a new tenant right now
DRY_RUN=1 VERBOSE=1 /usr/local/bin/litestream-sync-config.sh   # explain itself
cat /var/lib/node-exporter/textfile/litestream.prom            # per-tenant health, as the alerts see it
DRY_RUN=1 /usr/local/bin/litestream-health.sh                  # the same, from the last 26h of journal
```

### Alerts

Grafana group `backups` (`deploy/monitoring/grafana/provisioning/alerting/rules.yml`)
reads what `litestream-health.sh` writes:

| Alert | Fires when |
|---|---|
| `litestream-replica-stuck` | a tenant's last error is newer than its last upload and it errored in the last 5 min, for 10 min |
| `litestream-tenant-stale` | nothing uploaded for a tenant in 26 h (0.3.13 snapshots even an idle tenant at least every ~25 h) |
| `litestream-down` | `litestream.service` not active for 5 min |
| `litestream-health-missing` | the exporter's file is missing or older than 10 min while carrier is reporting |

### When a replica is stuck anyway

Read the error first (`journalctl -u litestream --since -10min`). If it will
not recover:

```sh
sudo DRY_RUN=1 /usr/local/bin/litestream-reset.sh   # what it would move
sudo /usr/local/bin/litestream-reset.sh
```

It stops litestream, moves **every** tenant's bookkeeping aside to a new
`/var/lib/litestream.reset-<time>.XXXX/`, starts it again (even if something
failed in between), and waits for a fresh snapshot per tenant. Every tenant,
never a subset: the restart is shared. Nothing in R2 is deleted by it;
litestream's 24 h retention removes the old generations about a day later.

`litestream databases` is the cheap schema check and worth reading carefully:
the **replicas column must say `s3`**. A blank replicas column is the silent
0.5-shaped-config failure at the top of this file.

There is no `litestream status` on 0.3.x — that is a 0.5 command. Local
replication state comes from `journalctl -u litestream`, and remote state from
`litestream generations`.

One gotcha the .deb hands you: `/etc/litestream.yml` is a dpkg **conffile**, and
this script rewrites it. Upgrading or reinstalling litestream will therefore
prompt about a locally modified config file. Answer "keep the local version"
(`dpkg -i --force-confold`); the timer would overwrite the package's copy within
two minutes anyway.

---

**Verified against litestream v0.3.13 on 2026-08-11**, using the real
`litestream-v0.3.13-darwin-arm64` binary from the upstream release, driven by
`litestream-sync-config.sh` itself against a scratch tenant layout:

- the generated config is accepted by `litestream databases -config`, which
  builds the replica and parses the age recipient — a schema check, not a
  syntax one — and reports `s3` for every tenant;
- the same render with `replicas:` collapsed to 0.5's `replica:` parses cleanly
  and reports **no** replicas;
- a full round trip: replicate → the on-disk snapshot begins
  `age-encryption.org/v1` → restore with `identities:` → `pragma
  integrity_check` = `ok`, row count intact; restore without them fails with
  `lz4: bad magic number`;
- `meta-path` keeps every byte of bookkeeping out of the database's directory;
- an empty recipient is `no recipients found`, an empty identity is
  `no secret keys found`, and a malformed one is `malformed recipient at line 1`
  — all hard failures, none of them silent plaintext;
- endpoints with and without an `https://` scheme both reach the same URL, so
  the generator's refusal is a house rule for the runbooks' sake, not litestream's.

**2026-11-04 (issue #696):** the position bug above was read out of the
v0.3.13 source (`replica.go`: `Sync`, `calcPos`), and both triggers matched
carrier's journal: a restart, and an R2 `InternalError` with no restart.
`litestream-health.sh` was run with `DRY_RUN=1` against carrier's real
journal and reported the stuck tenants correctly.

**Not verified:** `systemd-analyze verify` on the units in this directory. That
was written on macOS, which has no systemd. Run it once on the box:
`systemd-analyze verify /etc/systemd/system/litestream*.{service,timer}`.
