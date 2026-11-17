#!/usr/bin/env bash
# Build a distributable Passband.app: Developer ID signature, hardened runtime,
# Apple notarization, stapled ticket. The result opens by double-click on any
# Mac; an ad-hoc build from plain ./build.sh does not.
#
#   ./build-release.sh              sign, notarize, staple, package zip + dmg
#   ./build-release.sh --no-notary  sign and package only (fast; still
#                                   Gatekeeper-warned)
#
# One-time setup — store an app-specific password from appleid.apple.com under
# a keychain profile that this script reads by name:
#
#   xcrun notarytool store-credentials passband-notary \
#     --apple-id <your apple id> --team-id <your team id>
#
# Overridable: SIGN_ID (defaults to the Developer ID Application cert found in
# the keychain) and NOTARY_PROFILE (defaults to passband-notary).

set -euo pipefail

cd "$(dirname "$0")"

NOTARIZE=1
[ "${1:-}" = "--no-notary" ] && NOTARIZE=0

APP="build/Passband.app"
NOTARY_PROFILE="${NOTARY_PROFILE:-passband-notary}"

# ---------------------------------------------------------------- identity ---
# Fall back to whatever Developer ID Application cert is in the keychain, so a
# second machine or a second developer needs no edits here. Unlike build.sh
# there is no ad-hoc fallback: an unsigned release is never what anyone wants.
. ./signing.sh
SIGN_ID="${SIGN_ID:-$(detect_sign_identity)}"

if [ -z "$SIGN_ID" ]; then
  echo "error: no 'Developer ID Application' identity in the keychain." >&2
  echo "       Create one at developer.apple.com (Certificates > +) and" >&2
  echo "       download it, or set SIGN_ID to an identity name." >&2
  exit 1
fi

# --------------------------------------------------------------- build/sign ---
# HARDENED turns on the runtime and the secure timestamp, both required by the
# notary service and both absent from local builds so debugging still works.
SIGN_ID="$SIGN_ID" HARDENED=1 ./build.sh release

VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")
BUILD_NUM=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist")
STEM="Passband-$VERSION"

echo "==> verifying signature"
codesign --verify --strict --verbose=2 "$APP"

# ---------------------------------------------------------------- notarize ---
# notarize <file>: submit a zip or dmg and wait; on rejection print Apple's
# log and fail the build.
notarize() {
  local file="$1" log="build/notarytool-submit.txt" status id
  echo "==> submitting $(basename "$file") to Apple (typically 2-15 min)"
  set +e
  xcrun notarytool submit "$file" \
    --keychain-profile "$NOTARY_PROFILE" \
    --wait 2>&1 | tee "$log"
  status=${PIPESTATUS[0]}
  set -e
  if [ "$status" -ne 0 ] || grep -q 'status: Invalid' "$log"; then
    echo "==> notarization failed; fetching the rejection log" >&2
    id=$(grep -m1 -Eo '[0-9a-f]{8}-([0-9a-f]{4}-){3}[0-9a-f]{12}' "$log" || true)
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
    fi
    exit 1
  fi
}

if [ "$NOTARIZE" -eq 1 ]; then
  # Notarization takes a container, never a bare bundle. This zip is a
  # throwaway used only for the upload; the shipping archives are built after
  # stapling, so the app inside each carries its own ticket.
  ZIP="build/$STEM-notarize.zip"
  rm -f "$ZIP"
  ditto -c -k --keepParent "$APP" "$ZIP"
  notarize "$ZIP"
  rm -f "$ZIP"
  # The ticket is stapled into the .app so a first launch works offline.
  # stapler cannot write into a zip, which is why the archives come after.
  echo "==> stapling ticket"
  xcrun stapler staple "$APP"
fi

# ----------------------------------------------------------------- package ---
# Two archives of the same stapled app. The zip is what Sparkle and the
# Homebrew cask fetch; the dmg is what a person downloads from the site: open
# it, drag Passband onto the Applications alias beside it.
ARCHIVE="build/$STEM.zip"
echo "==> packaging $ARCHIVE"
rm -f "$ARCHIVE"
ditto -c -k --keepParent "$APP" "$ARCHIVE"

DMG="build/$STEM.dmg"
echo "==> packaging $DMG"
DMG_STAGE="build/dmg-stage"
rm -rf "$DMG_STAGE" "$DMG"
mkdir -p "$DMG_STAGE"
ditto "$APP" "$DMG_STAGE/Passband.app"
ln -s /Applications "$DMG_STAGE/Applications"
# HFS+ rather than APFS so it mounts on every macOS the app supports; ULFO
# (lzfse) is the smallest format those versions all read.
hdiutil create -quiet -volname "Passband" -srcfolder "$DMG_STAGE" \
  -fs HFS+ -format ULFO -ov "$DMG"
rm -rf "$DMG_STAGE"
# A downloaded dmg is itself checked by Gatekeeper, so it is signed and
# notarized in its own right, not only the app inside it.
codesign --sign "$SIGN_ID" --timestamp "$DMG"

# The symbols for exactly this binary, kept beside the archive. Named so no
# `Passband-*.zip` glob (make-appcast.sh, the release workflow's feed) can
# take it for an app archive.
SYMBOLS="build/symbols-$STEM.dSYM.zip"
rm -f "$SYMBOLS"
ditto -c -k --keepParent build/Passband.app.dSYM "$SYMBOLS"
echo "==> symbols: $SYMBOLS"

if [ "$NOTARIZE" -eq 0 ]; then
  echo "==> signed but NOT notarized: Gatekeeper will still warn on other Macs"
  exit 0
fi

notarize "$DMG"
echo "==> stapling dmg"
xcrun stapler staple "$DMG"

echo "==> verifying Gatekeeper acceptance"
spctl -a -vvv -t exec "$APP"
spctl -a -vvv -t open --context context:primary-signature "$DMG"

echo
echo "==> $STEM (build $BUILD_NUM) ready: $ARCHIVE, $DMG"
