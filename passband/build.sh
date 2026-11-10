#!/usr/bin/env bash
# Build Passband.app straight from the command line with swiftc — no xcodebuild,
# no IDE. This is the canonical build for this repo because the Xcode CLI on
# this machine intermittently fails to load its simulator plug-ins, which
# xcodebuild treats as fatal even for a macOS-only target. `swiftc` itself is
# unaffected, so we drive it directly and assemble the bundle ourselves.
#
#   ./build.sh          debug build
#   ./build.sh release  optimized build
#   ./build.sh preview  faster unoptimized build for visual iteration
#   ./build.sh run      build, then launch the app
#
# The .xcodeproj (via `xcodegen generate`) is still checked in and is the
# preferred path when xcodebuild is healthy; both produce the same bundle.

set -euo pipefail

# Resolved before the cd below, so a relative path means the caller's one.
if [ -n "${MAC_PUSH_PROFILE:-}" ]; then
  MAC_PUSH_PROFILE="$(cd "$(dirname "$MAC_PUSH_PROFILE")" && pwd)/$(basename "$MAC_PUSH_PROFILE")"
fi

cd "$(dirname "$0")"

# shellcheck source=signing.sh
. ./signing.sh

MODE="${1:-debug}"
APP="build/Passband.app"
SRC_DIR="Sources/Passband"
RES_DIR="$SRC_DIR/Resources"

# Vendored prebuilt Sparkle (no package resolution in this build; see
# vendor/fetch-sparkle.sh). Fetched on demand so a fresh clone just builds.
SPARKLE_DIR="vendor/Sparkle"
./vendor/fetch-sparkle.sh

# Versioning. ./VERSION is the single source of truth for the user-facing
# version; keep project.yml's MARKETING_VERSION in step with it. The build
# number is the commit count, which is monotonic across the whole history —
# macOS and every updater require CFBundleVersion to only ever increase. Both
# are env-overridable so a release can be cut from a dirty or exported tree.
MARKETING_VERSION="${MARKETING_VERSION:-$(cat VERSION)}"
BUILD_NUMBER="${BUILD_NUMBER:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}"

# Which branch this build came from, shown next to the sitrep label so a desk
# with three dev builds open can tell which one is testing which feature. Only
# dev builds carry it: a release build is by definition what shipped, and
# `main` is the default so it is not worth a label. Detached HEAD ("HEAD") is
# treated as no branch. Overridable so a build cut from an exported tree can
# name itself.
BUILD_BRANCH="${BUILD_BRANCH-$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)}"
case "$MODE:$BUILD_BRANCH" in
  release:*|*:main|*:HEAD) BUILD_BRANCH="" ;;
esac

SWIFT_FLAGS=(
  -target arm64-apple-macosx26.0
  -swift-version 6
  -strict-concurrency=minimal
  -framework AppKit
  -framework SwiftUI
  -framework Charts
  -framework WebKit
  -framework Security
  -framework UniformTypeIdentifiers
  -framework QuickLookUI
  -framework UserNotifications
  -framework ServiceManagement
  -F "$SPARKLE_DIR"
  -framework Sparkle
  -Xlinker -rpath -Xlinker @executable_path/../Frameworks
)

case "$MODE" in
  # -g so there are symbols to keep in a dSYM (the binary is stripped of them
  # below); dead_strip so code nothing reaches is not linked at all.
  release) SWIFT_FLAGS+=(-O -whole-module-optimization -g -Xlinker -dead_strip) ;;
  preview) SWIFT_FLAGS+=(-Onone -whole-module-optimization) ;;
  *)       SWIFT_FLAGS+=(-Onone -g) ;;
esac

# The shipped build, and only it, reports analytics (see Analytics.client).
# HARDENED=1 is set by build-release.sh alone, so a local `./build.sh release`
# for a perf check stays as silent as a debug one.
if [ "${HARDENED:-0}" = 1 ]; then SWIFT_FLAGS+=(-D PASSBAND_RELEASE); fi

echo "==> compiling ($MODE) v$MARKETING_VERSION build $BUILD_NUMBER"${BUILD_BRANCH:+ [$BUILD_BRANCH]}
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# Every .swift under Sources/Passband, excluding the resources folder.
# (`mapfile` is bash 4+; macOS ships bash 3.2, so read the list the portable way.)
SOURCES=()
while IFS= read -r file; do
  SOURCES+=("$file")
done < <(find "$SRC_DIR" -name '*.swift' -not -path "$RES_DIR/*" | sort)
echo "    ${#SOURCES[@]} source files"

xcrun swiftc "${SWIFT_FLAGS[@]}" -o "$APP/Contents/MacOS/Passband" "${SOURCES[@]}"

# THE SYMBOLS LEAVE THE BUNDLE. With -g the driver writes Passband.dSYM next
# to the binary, inside Contents/MacOS, where it would ship ~55 MB of debug
# symbols to every install. It goes to build/Passband.app.dSYM
# (build-release.sh packages it), and the binary is stripped of local symbols
# before it is signed. The dSYM keeps the binary's UUID, which is what
# symbolicates a crash from the shipped build.
if [ "$MODE" = release ]; then
  DSYM="build/Passband.app.dSYM"
  echo "==> splitting symbols into $DSYM"
  rm -rf "$DSYM"
  mv "$APP/Contents/MacOS/Passband.dSYM" "$DSYM"
  xcrun strip -x "$APP/Contents/MacOS/Passband"
  # A -g compile has been seen to leave its module beside this script
  # (Passband-1.swiftmodule and friends, with absolute paths inside). Nothing
  # reads them; they must not end up in a commit.
  rm -f Passband-[0-9]*.swiftmodule Passband-[0-9]*.swiftdoc \
    Passband-[0-9]*.swiftsourceinfo Passband-[0-9]*.abi.json
fi

echo "==> assembling bundle"
# Info.plist with the build-setting placeholders resolved.
sed -e 's/\$(EXECUTABLE_NAME)/Passband/g' \
    -e 's/\$(PRODUCT_BUNDLE_IDENTIFIER)/app.passband.client/g' \
    -e 's/\$(PRODUCT_NAME)/Passband/g' \
    -e "s/\\\$(MARKETING_VERSION)/$MARKETING_VERSION/g" \
    -e "s/\\\$(CURRENT_PROJECT_VERSION)/$BUILD_NUMBER/g" \
    -e "s|\\\$(PASSBAND_BUILD_BRANCH)|$BUILD_BRANCH|g" \
    -e 's/\$(MACOSX_DEPLOYMENT_TARGET)/26.0/g' \
    "$SRC_DIR/Info.plist" > "$APP/Contents/Info.plist"

cp -R "$RES_DIR/." "$APP/Contents/Resources/" 2>/dev/null || true

# App icon. ./Passband.icon is the only icon source in the repo; actool compiles
# it to the Assets.car that CFBundleIconName points at — macOS 26 shrinks any
# bundle without one onto a plain squircle as an unadopted legacy icon — and
# also emits the flattened Passband.icns fallback that CFBundleIconFile names.
# actool ships with full Xcode, not the CommandLineTools; a machine without it
# just builds an iconless bundle instead of failing.
#
# The same call compiles AgentLogos.xcassets (the agents' own marks in Settings >
# Agents) into that Assets.car; without actool the pane falls back to SF
# Symbols. Every path here is ABSOLUTE: actool hands them to a shared daemon
# that resolves a relative one against its own working directory, which can be
# another checkout entirely (seen 2026-10-05: a relative input resolved into a
# different worktree, and a build came out with an Assets.car missing these).
if ! xcrun actool "$PWD/Passband.icon" "$PWD/Sources/Passband/AgentLogos.xcassets" \
    --compile "$PWD/$APP/Contents/Resources" \
    --platform macosx --minimum-deployment-target 26.0 \
    --app-icon Passband \
    --output-partial-info-plist "$PWD/build/actool-partial.plist" \
    --output-format human-readable-text >/dev/null; then
  echo "    WARNING: actool failed; bundle has no app icon" >&2
fi

printf 'APPL????' > "$APP/Contents/PkgInfo"

# Embed Sparkle. ditto (not cp) so the framework's symlink structure survives —
# codesign rejects a framework whose Versions/Current is a real directory.
mkdir -p "$APP/Contents/Frameworks"
ditto "$SPARKLE_DIR/Sparkle.framework" "$APP/Contents/Frameworks/Sparkle.framework"

# Sign with the Developer ID when the machine has one, ad-hoc otherwise.
#
# This is what stops the keychain re-prompting on every dev build. Keychain
# ACLs match against a bundle's designated requirement, and an ad-hoc
# signature's requirement is a cdhash of the build itself — it changes on every
# recompile, so each build looks like a brand new app and has to ask again. A
# Developer ID requirement is keyed to the team and never changes, so the ACL
# keeps matching for the life of the certificate. Ad-hoc remains the fallback
# so the build still works on a machine with no certificate.
ENTITLEMENTS="$SRC_DIR/Passband.entitlements"
if [ -z "${SIGN_ID:-}" ]; then
  SIGN_ID="$(detect_sign_identity)"
  SIGN_ID="${SIGN_ID:--}"
fi

CODESIGN_FLAGS=(--force)
SIGN_NOTE=""
if [ "${HARDENED:-0}" = "1" ]; then
  # Hardened runtime is mandatory for notarization; the secure timestamp keeps
  # the signature valid after the signing certificate expires.
  CODESIGN_FLAGS+=(--options runtime --timestamp)
  SIGN_NOTE=" (hardened runtime)"
else
  # Local builds need get-task-allow so a debugger can attach. Notarization
  # rejects that entitlement outright, so it is injected into a throwaway copy
  # and can never reach a release build.
  ENTITLEMENTS="build/Passband.debug.entitlements"
  cp "$SRC_DIR/Passband.entitlements" "$ENTITLEMENTS"
  /usr/libexec/PlistBuddy -c \
    'Add :com.apple.security.get-task-allow bool true' "$ENTITLEMENTS" >/dev/null
fi

# APNs, for the release announcement (Sources/Passband/Lib/MacPush.swift).
# Push is a PROFILE-GATED entitlement: a Developer ID app may claim
# aps-environment only with a provisioning profile embedded that grants it, and
# macOS KILLS AT LAUNCH an app that claims it without one. So the profile and
# the entitlement go in together or not at all. MAC_PUSH_PROFILE names a
# Developer ID provisioning profile for app.passband.client with Push
# Notifications on; without it the app is signed exactly as it always was and
# never asks for a token. Every check below fails the build rather than ship an
# app that will not open.
if [ -n "${MAC_PUSH_PROFILE:-}" ]; then
  PROFILE_PLIST="build/push-profile.plist"
  security cms -D -i "$MAC_PUSH_PROFILE" > "$PROFILE_PLIST"
  # `|| true`: a missing key is an empty answer for the checks below to
  # name, not a silent `set -e` exit.
  pb() { /usr/libexec/PlistBuddy -c "Print :$1" "$2" 2>/dev/null || true; }
  PUSH_TEAM=$(pb 'TeamIdentifier:0' "$PROFILE_PLIST")
  PUSH_APPID=$(pb 'Entitlements:com.apple.application-identifier' "$PROFILE_PLIST")
  PUSH_APS=$(pb 'Entitlements:com.apple.developer.aps-environment' "$PROFILE_PLIST")
  BUNDLE_ID=$(pb CFBundleIdentifier "$APP/Contents/Info.plist")
  [ -n "$PUSH_TEAM" ] && [ "$PUSH_APPID" = "$PUSH_TEAM.$BUNDLE_ID" ] || {
    echo "error: MAC_PUSH_PROFILE is for '$PUSH_APPID', not $PUSH_TEAM.$BUNDLE_ID" >&2; exit 1; }
  case "$PUSH_APS" in
    production|development) ;;
    *) echo "error: MAC_PUSH_PROFILE grants no aps-environment (Push Notifications off on the App ID?)" >&2; exit 1 ;;
  esac
  case "$SIGN_ID" in
    *"($PUSH_TEAM)"*) ;;
    -) echo "error: MAC_PUSH_PROFILE needs a Developer ID signature; an ad-hoc build with it is killed at launch" >&2; exit 1 ;;
    *) echo "error: signing as '$SIGN_ID', but MAC_PUSH_PROFILE belongs to team $PUSH_TEAM" >&2; exit 1 ;;
  esac
  EXPIRES=$(pb ExpirationDate "$PROFILE_PLIST")
  if [ -n "$EXPIRES" ] && [ "$(date -j -f '%a %b %d %T %Z %Y' "$EXPIRES" +%s 2>/dev/null || echo 0)" -lt "$(date +%s)" ]; then
    echo "error: MAC_PUSH_PROFILE expired on $EXPIRES" >&2; exit 1
  fi
  cp "$MAC_PUSH_PROFILE" "$APP/Contents/embedded.provisionprofile"
  PUSH_ENTITLEMENTS="build/Passband.push.entitlements"
  cp "$ENTITLEMENTS" "$PUSH_ENTITLEMENTS"
  /usr/libexec/PlistBuddy -c "Add :com.apple.developer.aps-environment string $PUSH_APS" "$PUSH_ENTITLEMENTS" >/dev/null
  /usr/libexec/PlistBuddy -c "Add :com.apple.application-identifier string $PUSH_APPID" "$PUSH_ENTITLEMENTS" >/dev/null
  /usr/libexec/PlistBuddy -c "Add :com.apple.developer.team-identifier string $PUSH_TEAM" "$PUSH_ENTITLEMENTS" >/dev/null
  ENTITLEMENTS="$PUSH_ENTITLEMENTS"
  SIGN_NOTE="$SIGN_NOTE (push: $PUSH_APS)"
else
  rm -f "$APP/Contents/embedded.provisionprofile"
fi

echo "==> signing as $SIGN_ID$SIGN_NOTE"
# Sparkle's nested executables first, inside-out: the hardened runtime enforces
# library validation, so a release app may only load code signed by its own
# team — Sparkle's upstream signature does not qualify. The Downloader XPC
# keeps its sandbox entitlements via --preserve-metadata.
SPARKLE_B="$APP/Contents/Frameworks/Sparkle.framework/Versions/B"
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" \
  --preserve-metadata=entitlements "$SPARKLE_B/XPCServices/Downloader.xpc"
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" "$SPARKLE_B/XPCServices/Installer.xpc"
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" "$SPARKLE_B/Autoupdate"
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" "$SPARKLE_B/Updater.app"
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" \
  "$APP/Contents/Frameworks/Sparkle.framework"
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" --entitlements "$ENTITLEMENTS" "$APP"

echo "==> built $APP"

if [ "$MODE" = "run" ]; then
  echo "==> launching"
  open "$APP"
fi
