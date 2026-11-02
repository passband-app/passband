#!/usr/bin/env bash
#
# litestream-replicate.sh -- ExecStart for litestream.service. Loads the backup
# age IDENTITY into the environment and execs `litestream replicate`.
#
# WHY THE REPLICATOR HOLDS THE IDENTITY (issue #696). Litestream 0.3.13 works
# out where a replica left off by DOWNLOADING AND DECOMPRESSING the last WAL
# segment it uploaded (Replica.calcPos in replica.go). That segment is age
# ciphertext. Without the identity the decompress fails with
#
#   cannot determine replica position: lz4: bad magic number
#
# about once a second, and that tenant uploads nothing, silently, until someone
# starts a fresh generation by hand. calcPos runs:
#
#   - on every start, for every tenant with an existing generation -- and the
#     config timer restarts litestream on every signup and erasure; and
#   - after ANY failed sync, because Replica.Sync clears its position on error.
#     One transient R2 error at 3am was enough to break an idle tenant with no
#     restart at all.
#
# So "the replicator only holds the public recipient" was never a working
# configuration on 0.3.13: it fails the first time anything goes wrong. And it
# protected nothing: this process runs as root with CAP_DAC_OVERRIDE on the box
# where the key file sits, and it already reads every tenant's live database.
# See README.md -> "Encryption".
#
# HOW THE KEY ARRIVES. systemd copies /etc/litestream/backup-age.key into this
# unit's private credentials directory (LoadCredential= in litestream.service)
# and refuses to start the unit if the file is missing. The key lives in this
# process's environment because litestream 0.3.13 only takes identities as
# config strings, and the config refers to ${LITESTREAM_AGE_IDENTITY} by name.
# /proc/<pid>/environ is readable by root only (and ProtectProc=invisible hides
# the process from everyone else). It is never written to disk or the journal.

set -euo pipefail

LITESTREAM_BIN="${LITESTREAM_BIN:-/usr/bin/litestream}"
CRED_NAME="${CRED_NAME:-backup-age-identity}"

die() { printf 'litestream-replicate: %s\n' "$*" >&2; exit 1; }

[[ -n "${CREDENTIALS_DIRECTORY:-}" ]] ||
  die "CREDENTIALS_DIRECTORY is unset. Run this from litestream.service, which sets LoadCredential=${CRED_NAME}:/etc/litestream/backup-age.key."

key_file="${CREDENTIALS_DIRECTORY}/${CRED_NAME}"
[[ -r "$key_file" ]] || die "${key_file} is missing or unreadable."

# The key file is age-keygen output: '# created' / '# public key' comments and
# one AGE-SECRET-KEY-1... line. Keep the key lines only.
identity="$(grep -v '^#' "$key_file" | tr -d '[:space:]')"

# Fail here, by name, rather than as litestream's "no secret keys found" or --
# worse -- a config that expands to an empty identity list and starts.
[[ "$identity" == AGE-SECRET-KEY-1* ]] ||
  die "${key_file} does not hold an age identity (expected AGE-SECRET-KEY-1...). Check /etc/litestream/backup-age.key."

export LITESTREAM_AGE_IDENTITY="$identity"
unset identity

exec "$LITESTREAM_BIN" replicate "$@"
