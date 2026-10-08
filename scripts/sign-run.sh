#!/bin/sh
# Cargo runner for macOS (wired up in .cargo/config.toml): signs a freshly built
# squelchd with the Developer ID, then runs it. Also usable directly on a binary
# cargo already built:
#
#   scripts/sign-run.sh target/debug/squelchd serve
#
# This is what stops the keychain asking for the login password on every daemon
# start. Keychain ACLs match against a binary's designated requirement, and the
# linker's ad-hoc signature has a cdhash of the build itself as its requirement:
# it changes on every recompile, so each build looks like a brand new program
# and has to ask again, once per credential slot. A Developer ID requirement is
# keyed to the team and the identifier and never changes, so one "Always Allow"
# keeps matching for the life of the certificate. Same reasoning as the app's
# signing step in passband/build.sh.
#
# Only squelchd is signed: it is the one binary that reads the keyring, and
# signing every test binary would tax `cargo test` for nothing. With no
# Developer ID on the machine the binary runs as built, prompts and all.
# SQUELCH_DEV_SIGN=0 skips signing.

set -eu

BIN="$1"
SCRIPTS="$(cd "$(dirname "$0")" && pwd)"

if [ "$(basename "$BIN")" = "squelchd" ] && [ "${SQUELCH_DEV_SIGN:-1}" != "0" ] \
  && codesign -dv "$BIN" 2>&1 | grep -q '^TeamIdentifier=not set'; then
  # shellcheck source=../passband/signing.sh
  . "$SCRIPTS/../passband/signing.sh"
  SIGN_ID="${SIGN_ID:-$(detect_sign_identity)}"
  if [ -n "$SIGN_ID" ]; then
    # The identifier is pinned because it is half of the requirement, and the
    # default is the file name. get-task-allow keeps a debugger able to attach.
    # No timestamp: that is a network round trip a dev build has no use for.
    OUT="$(codesign --force --timestamp=none --sign "$SIGN_ID" \
      --identifier app.passband.squelchd-dev \
      --entitlements "$SCRIPTS/squelchd-dev.entitlements" "$BIN" 2>&1)" \
      || echo "sign-run: signing failed, running $BIN as built: $OUT" >&2
  fi
fi

exec "$@"
