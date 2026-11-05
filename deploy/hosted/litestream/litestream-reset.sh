#!/usr/bin/env bash
#
# litestream-reset.sh -- start every tenant on a fresh litestream generation.
#
# WHEN. A tenant's replica is stuck and will not recover on its own: the
# `litestream-replica-stuck` alert, or `journalctl -u litestream` repeating one
# error per tenant about once a second. Before issue #696 was fixed the stuck
# error was `cannot determine replica position: lz4: bad magic number`; with
# the identity loaded it should not recur, so read the error before you reach
# for this.
#
# WHAT. Stop litestream, move every per-tenant bookkeeping directory in
# /var/lib/litestream aside to a fresh /var/lib/litestream.reset-<timestamp>/,
# recreate the empty directories, start litestream. Every tenant then begins a
# new generation and uploads a full snapshot. Nothing in R2 is deleted by this
# script. Litestream's own retention (24h) removes the old generations from R2
# about a day later, so the point-in-time window restarts from now.
#
# WHY ALL OF THEM, NEVER A SUBSET. A reset restarts the process, and a restart
# makes every OTHER tenant recompute its position too. With the bug present,
# resetting three broken tenants broke the six healthy ones (2026-11-02).
#
# WHAT IT FIXES OVER THE HAND-TYPED VERSION. The service is started again
# whatever happens after the stop (an EXIT trap), so a failed move never leaves
# backups off. Each run gets its own aside directory, so running it twice is
# safe (the hand-typed `mv` reused a name, failed, and left the unit down). A
# lock stops two runs overlapping. It waits for each tenant's first snapshot,
# and exits non-zero naming any tenant that did not get one.
#
#   sudo /usr/local/bin/litestream-reset.sh             # does it
#   sudo DRY_RUN=1 /usr/local/bin/litestream-reset.sh   # says what it would move

set -euo pipefail

META_ROOT="${META_ROOT:-/var/lib/litestream}"
SERVICE="${SERVICE:-litestream.service}"
CONFIG_FILE="${CONFIG_FILE:-/etc/litestream.yml}"
WAIT_SECONDS="${WAIT_SECONDS:-600}"
DRY_RUN="${DRY_RUN:-0}"

log() { printf 'litestream-reset: %s\n' "$*" >&2; }
die() { printf 'litestream-reset: FATAL: %s\n' "$*" >&2; exit 1; }

[[ "${EUID:-$(id -u)}" -eq 0 ]] || die "must run as root"
[[ -d "$META_ROOT" ]] || die "${META_ROOT} does not exist"
[[ -f "$CONFIG_FILE" ]] || die "${CONFIG_FILE} does not exist; run litestream-sync-config.sh first"

# The tenants litestream is configured for. The config lists one meta-path per
# database, which is also the directory this script moves.
mapfile -t labels < <(sed -n "s|^    meta-path: ${META_ROOT}/||p" "$CONFIG_FILE")
(( ${#labels[@]} > 0 )) || die "${CONFIG_FILE} has no meta-path entries; nothing to reset"

shopt -s nullglob
entries=("$META_ROOT"/*)
shopt -u nullglob

if [[ "$DRY_RUN" == "1" ]]; then
  log "DRY_RUN=1: would stop ${SERVICE}, move ${#entries[@]} entr(y/ies) out of ${META_ROOT}, and wait for ${#labels[@]} snapshot(s):"
  printf '  %s\n' "${labels[@]}" >&2
  exit 0
fi

exec 9>/run/litestream-reset.lock
flock -n 9 || die "another litestream-reset is running"

aside="$(mktemp -d "${META_ROOT}.reset-$(date -u +%Y%m%dT%H%M%SZ).XXXX")"
chmod 0700 "$aside"

started=0
start_service() {
  (( started )) && return 0
  started=1
  systemctl start "$SERVICE" || log "WARNING: systemctl start ${SERVICE} failed. Check: systemctl status ${SERVICE}"
}
trap start_service EXIT

log "stopping ${SERVICE}"
systemctl stop "$SERVICE"

if (( ${#entries[@]} > 0 )); then
  mv -- "${entries[@]}" "$aside"/
fi
for label in "${labels[@]}"; do
  install -d -m 0700 -o root -g root "${META_ROOT}/${label}"
done
log "moved ${#entries[@]} entr(y/ies) to ${aside}"

since="$(date -u '+%Y-%m-%d %H:%M:%S')"
start_service
trap - EXIT
log "started ${SERVICE}; waiting up to ${WAIT_SECONDS}s for ${#labels[@]} snapshot(s)"

# A tenant counts once litestream logs "snapshot written" for its database.
# Its db= path ends in _tenants_<label>-data/<file>, see litestream-sync-config.sh.
deadline=$(( SECONDS + WAIT_SECONDS ))
pending=("${labels[@]}")
while (( ${#pending[@]} > 0 && SECONDS < deadline )); do
  sleep 5
  written="$(journalctl -u "$SERVICE" --since "$since" --no-pager -o cat 2>/dev/null |
    grep 'msg="snapshot written"' | sed -nE 's|.*_tenants_([a-z0-9-]+)-data/.*|\1|p' | sort -u || true)"
  still=()
  for label in "${pending[@]}"; do
    grep -qxF "$label" <<< "$written" || still+=("$label")
  done
  pending=("${still[@]+"${still[@]}"}")
done

if (( ${#pending[@]} > 0 )); then
  log "no snapshot within ${WAIT_SECONDS}s for: ${pending[*]}"
  log "  journalctl -u ${SERVICE} --since '${since}' | grep -E '$(IFS='|'; echo "${pending[*]}")'"
  exit 1
fi

log "every tenant has a fresh snapshot (${#labels[@]}). Old bookkeeping: ${aside}"
log "  It is safe to delete once you are satisfied: R2 retention drops the old generations within about a day."
