#!/usr/bin/env bash
# THE IMAP TEST BED: a throwaway Dovecot in Docker for the IMAP source's
# `#[ignore]` integration tests (issue #575). Dev only; nothing ships from here.
#
#   scripts/imap-dev.sh start    # run Dovecot (idempotent), mint its TLS cert
#   scripts/imap-dev.sh seed     # APPEND the fixture messages (once per start)
#   scripts/imap-dev.sh status   # is it up, and what is in INBOX
#   scripts/imap-dev.sh stop     # remove the container AND its mail volume
#
# Reaching it (127.0.0.1 only, never 0.0.0.0):
#   plaintext / STARTTLS  127.0.0.1:10143
#   implicit TLS          127.0.0.1:10993   (cert for localhost + 127.0.0.1,
#                                            CA at scripts/imap-dev/.state/ca.crt)
#   SMTP submission, STARTTLS   127.0.0.1:10587
#   SMTP submission, implicit   127.0.0.1:10465   (same cert, same CA)
#   user  squelch@dev.test   password  squelch-dev-password
#   ANY other @dev.test user logs in with the same password (Dovecot's static
#   passdb), and mail submitted to one is delivered into its INBOX: a send to
#   self round-trips, and a second user (bcc@dev.test) can be a Bcc target.
#
# Submission is Dovecot's own `submission` service (TLS + AUTH, the same
# passdb), relaying to a Postfix container (squelch-imap-dev-smtp, built from
# scripts/imap-dev/smtp, no published port) that delivers back into Dovecot
# over LMTP on the private network squelch-imap-dev-net. Nothing auto-saves a
# Sent copy, on purpose. Postfix bounces anything not @dev.test.
#
# The password is a fixed dev value on purpose: the server is loopback-only,
# holds only the fixture mail below, and the tests need to know it.
set -euo pipefail

IMAGE="dovecot/dovecot:2.4.4"
NAME="squelch-imap-dev"
VOLUME="squelch-imap-dev-vmail"
PLAIN_PORT=10143
TLS_PORT=10993
SUBMISSION_PORT=10587
SUBMISSIONS_PORT=10465
NETWORK="squelch-imap-dev-net"
SMTP_NAME="squelch-imap-dev-smtp"
SMTP_IMAGE="squelch-imap-dev-smtp:1"
USER_NAME="squelch@dev.test"
PASSWORD="squelch-dev-password"

HERE="$(cd "$(dirname "$0")" && pwd)/imap-dev"
STATE="$HERE/.state"

mint_tls() {
  [[ -s "$STATE/tls/server.crt" ]] && return 0
  mkdir -p "$STATE/tls"
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -subj "/CN=squelch imap-dev CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    -keyout "$STATE/ca.key" -out "$STATE/ca.crt" 2>/dev/null
  openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
    -keyout "$STATE/tls/server.key" -out "$STATE/server.csr" 2>/dev/null
  printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid\n' \
    > "$STATE/server.ext"
  openssl x509 -req -in "$STATE/server.csr" -CA "$STATE/ca.crt" -CAkey "$STATE/ca.key" \
    -CAcreateserial -days 3650 -extfile "$STATE/server.ext" \
    -out "$STATE/tls/server.crt" 2>/dev/null
  # The container runs as an unprivileged user that has to read the key.
  chmod 644 "$STATE/tls/server.key"
}

running_one() { [[ "$(docker inspect -f '{{.State.Running}}' "$1" 2>/dev/null)" == "true" ]]; }
# Up means BOTH halves. A Dovecot started before the SMTP half existed (no
# 10587 mapping) counts as down, so `start` recreates it (the mail volume
# survives that).
running() {
  running_one "$NAME" && running_one "$SMTP_NAME" &&
    docker port "$NAME" 31587/tcp 2>/dev/null | grep -q "127.0.0.1:$SUBMISSION_PORT"
}

wait_ready() {
  for _ in $(seq 1 60); do
    if python3 -I - "$PLAIN_PORT" <<'PY' 2>/dev/null; then return 0; fi
import socket, sys
s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=2)
sys.exit(0 if s.recv(64).startswith(b"* OK") else 1)
PY
    sleep 1
  done
  echo "dovecot did not answer on 127.0.0.1:$PLAIN_PORT" >&2
  docker logs --tail 40 "$NAME" >&2 || true
  return 1
}

wait_smtp_ready() {
  for _ in $(seq 1 60); do
    if python3 -I - "$SUBMISSION_PORT" <<'PY' 2>/dev/null; then return 0; fi
import socket, sys
s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=2)
sys.exit(0 if s.recv(64).startswith(b"220") else 1)
PY
    sleep 1
  done
  echo "submission did not answer on 127.0.0.1:$SUBMISSION_PORT" >&2
  docker logs --tail 40 "$NAME" >&2 || true
  return 1
}

start() {
  if running; then echo "already running"; return 0; fi
  docker rm -f "$NAME" "$SMTP_NAME" >/dev/null 2>&1 || true
  mint_tls
  docker network inspect "$NETWORK" >/dev/null 2>&1 ||
    docker network create "$NETWORK" >/dev/null
  # Rebuilt on every start; docker's cache makes that cheap.
  docker build -q -t "$SMTP_IMAGE" "$HERE/smtp" >/dev/null
  docker run -d --name "$SMTP_NAME" --network "$NETWORK" "$SMTP_IMAGE" >/dev/null
  docker run -d --name "$NAME" --network "$NETWORK" \
    -p "127.0.0.1:$PLAIN_PORT:31143" -p "127.0.0.1:$TLS_PORT:31993" \
    -p "127.0.0.1:$SUBMISSION_PORT:31587" -p "127.0.0.1:$SUBMISSIONS_PORT:31465" \
    -e USER_PASSWORD="$PASSWORD" \
    -v "$VOLUME:/srv/vmail" \
    -v "$HERE/zz-squelch-dev.conf:/etc/dovecot/conf.d/zz-squelch-dev.conf:ro" \
    -v "$STATE/tls:/etc/squelch-dev/tls:ro" \
    "$IMAGE" >/dev/null
  wait_ready
  wait_smtp_ready
  echo "dovecot up: imap 127.0.0.1:$PLAIN_PORT (plain/STARTTLS), imaps 127.0.0.1:$TLS_PORT;"
  echo "  submission 127.0.0.1:$SUBMISSION_PORT (STARTTLS), 127.0.0.1:$SUBMISSIONS_PORT (TLS); user $USER_NAME"
}

seed() {
  running || { echo "not running; scripts/imap-dev.sh start first" >&2; exit 1; }
  python3 -I "$HERE/seed.py" "$PLAIN_PORT" "$USER_NAME" "$PASSWORD"
}

status() {
  if ! running; then echo "stopped"; return 1; fi
  echo "running ($IMAGE + $SMTP_IMAGE)"
  docker exec "$NAME" /dovecot/bin/doveadm mailbox status -u "$USER_NAME" \
    "messages unseen uidvalidity uidnext highestmodseq" '*'
}

stop() {
  docker rm -f "$NAME" "$SMTP_NAME" >/dev/null 2>&1 || true
  docker volume rm "$VOLUME" >/dev/null 2>&1 || true
  docker network rm "$NETWORK" >/dev/null 2>&1 || true
  echo "stopped (container and mail volume removed)"
}

case "${1:-}" in
  start) start ;;
  seed) seed ;;
  status) status ;;
  stop) stop ;;
  *) echo "usage: $0 start|seed|status|stop" >&2; exit 2 ;;
esac
