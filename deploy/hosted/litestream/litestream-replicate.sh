#!/bin/sh
#
# litestream-replicate.sh -- ExecStart of litestream.service. Loads the age
# IDENTITY into the replicator's environment, then execs `litestream replicate`.
#
# WHY THE REPLICATOR HOLDS THE PRIVATE KEY (issue #696). Litestream 0.3.13 keeps
# each replica's upload position in memory only. It loses it on every start AND
# on every failed sync (one R2 500 is enough), and recovers it by downloading
# the last WAL segment it uploaded and measuring it. That segment is age
# ciphertext. Without an identity, 0.3.13 hands the ciphertext straight to lz4,
# fails with `cannot determine replica position: lz4: bad magic number`, and
# that failure clears the position again: the tenant stops backing up for good,
# silently, until someone resets its generation.
#
# Holding the identity costs no boundary that existed. The key file sits on
# this box, and this process runs as root with CAP_DAC_OVERRIDE, so it could
# read the key file before this change. It also reads every tenant's plaintext
# database, which is all the key would unlock. What the old split did protect
# is kept: /etc/litestream/env (R2 creds plus the public recipient) still does
# not contain the key. Read the whole argument in README.md -> "Encryption".
#
# The key arrives through systemd's LoadCredential= (see litestream.service):
# PID 1 copies /etc/litestream/backup-age.key into a private, unswappable
# directory that only this unit can see, and names it in CREDENTIALS_DIRECTORY.
# There is no second copy of the key on disk.

set -eu

die() { printf 'litestream-replicate: FATAL: %s\n' "$*" >&2; exit 1; }

cred="${CREDENTIALS_DIRECTORY:-}/age-identity"
[ -n "${CREDENTIALS_DIRECTORY:-}" ] ||
  die "CREDENTIALS_DIRECTORY is unset: run me from litestream.service (LoadCredential=age-identity:...), not by hand"
[ -r "$cred" ] ||
  die "no age identity at ${cred}. Check LoadCredential= in litestream.service and /etc/litestream/backup-age.key"

# age-keygen writes comment lines (created at, public key) and one
# AGE-SECRET-KEY-1... line. Keep the key line only: the generated config splices
# this value into a YAML flow sequence, where a newline or a '#' would break it.
# Strip CR, tabs and spaces (a key pasted back from a password manager can
# carry them); newlines stay, so two keys are still counted as two.
identity="$(grep '^AGE-SECRET-KEY-1' "$cred" | tr -d '\r\t ' || true)"
[ -n "$identity" ] ||
  die "${cred} has no AGE-SECRET-KEY-1 line. Without the identity litestream cannot recover a replica position (issue #696), so it will not start"
[ "$(printf '%s\n' "$identity" | wc -l)" -eq 1 ] ||
  die "${cred} holds more than one identity; the config takes exactly one"

LITESTREAM_AGE_IDENTITY="$identity"
export LITESTREAM_AGE_IDENTITY
unset identity cred

# No -config: /etc/litestream.yml is litestream's default, and every ad-hoc CLI
# command reads the same file.
exec /usr/bin/litestream replicate "$@"
