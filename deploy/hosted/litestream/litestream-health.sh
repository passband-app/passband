#!/usr/bin/env bash
#
# litestream-health.sh -- turn litestream's journal into metrics, per tenant.
#
# WHY. Litestream 0.3.13 can fail silently (issue #696): a tenant's replica
# sticks in an error loop while the unit stays "active" and nothing pages. 0.3.13 can serve Prometheus metrics, but none of them says
# "this replica is failing" (they are DB-side), and the agent scrapes pods, not
# host loopback. The journal does say it, one line per attempt. So this script
# reads the new journal lines every minute and writes, for node-exporter's
# textfile collector:
#
#   passband_litestream_last_upload_timestamp_seconds{tenant}
#       the last "wal segment written" or "snapshot written"
#   passband_litestream_last_error_timestamp_seconds{tenant}
#       the last replica error ("monitor error" / "sync error")
#   passband_litestream_errors_total{tenant}
#       replica errors seen since this state began (a counter)
#   passband_litestream_service_active          1 if litestream.service is active
#   passband_litestream_health_run_timestamp_seconds   this script's last run
#
# The alerts are in deploy/monitoring/grafana/provisioning/alerting/rules.yml
# (group `backups`). The key one: a tenant whose last error is newer than its
# last upload, still erroring, for ten minutes. A transient R2 500 clears on
# the next upload; a stuck replica never does.
#
# STATE. A journal cursor, so each run reads only new lines, and one line per
# tenant with the three numbers above. Both live in STATE_DIR (systemd's
# StateDirectory). Lose them and the first run re-reads the last 26 hours,
# which is longer than litestream's 24h retention snapshot interval, so every
# healthy tenant has an upload in that window.
#
#   sudo /usr/local/bin/litestream-health.sh            # normal (the timer)
#   sudo DRY_RUN=1 /usr/local/bin/litestream-health.sh  # print metrics, save nothing
#
# Tested off-box by feeding journal lines on stdin: JOURNAL_FILE=<file>.

set -euo pipefail

STATE_DIR="${STATE_DIR:-/var/lib/litestream-health}"
TEXTFILE_DIR="${TEXTFILE_DIR:-/var/lib/node-exporter/textfile}"
CONFIG_FILE="${CONFIG_FILE:-/etc/litestream.yml}"
SERVICE="${SERVICE:-litestream.service}"
DRY_RUN="${DRY_RUN:-0}"
JOURNAL_FILE="${JOURNAL_FILE:-}"
NOW="${NOW:-$(date +%s)}"

die() { printf 'litestream-health: FATAL: %s\n' "$*" >&2; exit 1; }

state="${STATE_DIR}/tenants"
cursor="${STATE_DIR}/cursor"
next_cursor="${STATE_DIR}/cursor.next"
[[ "$DRY_RUN" == "1" ]] || mkdir -p "$STATE_DIR" "$TEXTFILE_DIR"

# Configured tenants. Metrics are written for exactly these, so an erased
# tenant's series goes away and a new one appears before its first upload (at
# zero, which the staleness rule reads as "never uploaded").
[[ -f "$CONFIG_FILE" ]] || die "${CONFIG_FILE} does not exist"
configured="$(sed -nE 's|^  - path: .*_tenants_([a-z0-9-]+)-data/.*|\1|p' "$CONFIG_FILE" | LC_ALL=C sort -u)"

# New journal lines, `short-unix` so the timestamp is epoch seconds. With no
# cursor (first run, or lost state), the last 26 hours. DRY_RUN reads that
# window too and leaves the cursor alone.
read_journal() {
  if [[ -n "$JOURNAL_FILE" ]]; then
    cat "$JOURNAL_FILE"
  elif [[ "$DRY_RUN" == "1" ]]; then
    journalctl -u "$SERVICE" --since -26h --no-pager -o short-unix -q
  elif [[ -s "$cursor" ]]; then
    # journalctl advances the cursor file as it reads. Advance a copy, and
    # promote it only after the state is saved, so a failed run re-reads its
    # lines instead of losing them.
    cp -f "$cursor" "$next_cursor"
    journalctl -u "$SERVICE" --cursor-file="$next_cursor" --no-pager -o short-unix -q
  else
    # journalctl refuses --since together with --cursor-file ("Please specify
    # only one of ..."; seen on carrier, systemd 259), so the first run reads
    # the window with --show-cursor and saves the trailing cursor itself.
    journalctl -u "$SERVICE" --since -26h --no-pager -o short-unix -q --show-cursor |
      awk -v c="$next_cursor" '/^-- cursor: /{ sub(/^-- cursor: /, ""); printf "%s", $0 > c; next } { print }'
  fi
}

active=0
if [[ -z "$JOURNAL_FILE" ]] && systemctl is-active --quiet "$SERVICE"; then active=1; fi

# Fold the new lines into the saved state. awk input: the saved state first
# ("S <tenant> <upload> <error> <errors>"), then the configured tenants
# ("C <tenant>"), then the journal. Output: the new state, configured tenants
# only, sorted.
new_state="$(
  {
    # DRY_RUN reads the whole 26h window, so folding saved state in too
    # would count the same error lines twice.
    [[ -f "$state" && "$DRY_RUN" != "1" ]] && sed 's/^/S /' "$state"
    [[ -n "$configured" ]] && printf '%s\n' "$configured" | sed 's/^/C /'
    read_journal | sed 's/^/J /'
  } | awk '
    $1 == "S" { up[$2] = $3; err[$2] = $4; n[$2] = $5; next }
    $1 == "C" { want[$2] = 1; next }
    $1 == "J" {
      if (!match($0, /_tenants_[a-z0-9-]+-data\//)) next
      t = substr($0, RSTART + 9, RLENGTH - 15)
      ts = int($2)
      if ($0 ~ /msg="(wal segment written|snapshot written)"/) {
        if (ts > up[t]) up[t] = ts
      } else if ($0 ~ /msg="(monitor error|sync error)"/) {
        if (ts > err[t]) err[t] = ts
        n[t]++
      }
    }
    END {
      for (t in want) printf "%s %d %d %d\n", t, up[t] + 0, err[t] + 0, n[t] + 0
    }
  ' | LC_ALL=C sort
)"

render() {
  echo '# HELP passband_litestream_last_upload_timestamp_seconds Last WAL segment or snapshot litestream wrote for this tenant.'
  echo '# TYPE passband_litestream_last_upload_timestamp_seconds gauge'
  while read -r t up _err _n; do
    [[ -n "$t" ]] || continue; echo "passband_litestream_last_upload_timestamp_seconds{tenant=\"$t\"} $up"
  done <<< "$new_state"
  echo '# HELP passband_litestream_last_error_timestamp_seconds Last replica error litestream logged for this tenant.'
  echo '# TYPE passband_litestream_last_error_timestamp_seconds gauge'
  while read -r t _up err _n; do
    [[ -n "$t" ]] || continue; echo "passband_litestream_last_error_timestamp_seconds{tenant=\"$t\"} $err"
  done <<< "$new_state"
  echo '# HELP passband_litestream_errors_total Replica errors litestream logged for this tenant.'
  echo '# TYPE passband_litestream_errors_total counter'
  while read -r t _up _err n; do
    [[ -n "$t" ]] || continue; echo "passband_litestream_errors_total{tenant=\"$t\"} $n"
  done <<< "$new_state"
  echo '# HELP passband_litestream_service_active 1 if litestream.service is active.'
  echo '# TYPE passband_litestream_service_active gauge'
  echo "passband_litestream_service_active $active"
  echo '# HELP passband_litestream_health_run_timestamp_seconds When litestream-health.sh last ran.'
  echo '# TYPE passband_litestream_health_run_timestamp_seconds gauge'
  echo "passband_litestream_health_run_timestamp_seconds $NOW"
}

if [[ "$DRY_RUN" == "1" ]]; then
  render
  exit 0
fi

# Write-then-rename, both files: node-exporter must never read half a file,
# and a crash mid-run must not lose the state.
# Order: metrics, then state, then cursor. A run that fails anywhere before
# the cursor moves re-reads its lines next time; one that fails before the
# state moves has saved nothing, so nothing is counted twice.
out="${TEXTFILE_DIR}/litestream.prom"
render > "${out}.tmp"
chmod 0644 "${out}.tmp"
mv -f "${out}.tmp" "$out"
printf '%s\n' "$new_state" > "${state}.tmp"
mv -f "${state}.tmp" "$state"
if [[ -f "$next_cursor" ]]; then mv -f "$next_cursor" "$cursor"; fi
