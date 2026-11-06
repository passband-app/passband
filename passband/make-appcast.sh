#!/usr/bin/env bash
# Regenerate the Sparkle appcast from the released archives and stage it into
# passband-site (deploying the site publishes the feed).
#
# releases/ accumulates one zip per shipped version and is the generator's
# working set: entries exist for exactly the zips present. It is gitignored;
# losing it only loses old feed entries, not the updates themselves (those
# live on GitHub releases).
#
# EdDSA signing uses the private key in the login keychain (created by
# vendor/Sparkle/bin/generate_keys). No deltas for now: full zips are ~10 MB
# and delta filenames would defeat the /download/ redirect's tag mapping.

set -euo pipefail
cd "$(dirname "$0")"

./vendor/fetch-sparkle.sh
mkdir -p releases

# Stage the freshly built archive, if build-release.sh left one.
for zip in build/Passband-*.zip; do
  if [ -e "$zip" ]; then cp -f "$zip" releases/; fi
done

# Signing key: the login keychain by default (this machine), or a key file
# when SPARKLE_ED_KEY_FILE is set (CI, where the key arrives as a secret).
KEY_ARGS=()
if [ -n "${SPARKLE_ED_KEY_FILE:-}" ]; then
  KEY_ARGS=(--ed-key-file "$SPARKLE_ED_KEY_FILE")
fi

FEED=../passband-site/appcast.xml
NS='xmlns:passband="https://passband.app/xml-namespaces/appcast"'

# QUIET RELEASES. A release tagged with the message `quiet`
#   git tag -a passband-mac-0.0.21 -m quiet
# is a hotfix nobody is told about: its item gets <passband:quiet>, which the
# daemon reads to skip its release push (squelch-core/src/release_feed.rs) and
# the Mac reads to skip its "ready to install" banner (Updater.isQuiet). It
# installs exactly like any other release. Only an ANNOTATED tag whose message
# subject (first line) is `quiet` counts; a lightweight tag reads its commit's
# subject, so it never does.
#
# This edits the feed AFTER generate_appcast. Safe because the feed itself is
# not signed. If SURequireSignedFeed is ever turned on, the marks must move
# before signing, or every update fails verification.
#
# The marks come off before generate_appcast runs and go back on after, from
# the tags every time, so the generator only ever sees its own format and a
# mark can never outlive its tag.
perl -0pi -e 's{\s*<passband:quiet>[^<]*</passband:quiet>}{}g; s{ \Q'"$NS"'\E}{}g' "$FEED"

# (macOS bash 3.2: expanding an empty array under set -u is an error, hence
# the ${arr[@]+...} guard.)
./vendor/Sparkle/bin/generate_appcast releases \
  ${KEY_ARGS[@]+"${KEY_ARGS[@]}"} \
  --download-url-prefix "https://passband.app/download/" \
  --maximum-deltas 0 \
  -o "$FEED"

QUIET=$(git for-each-ref 'refs/tags/passband-mac-*' \
  --format='%(objecttype) %(refname:strip=2) %(contents:subject)' \
  | awk '$1 == "tag" && NF == 3 && $3 == "quiet" { sub(/^passband-mac-/, "", $2); print $2 }')
if [ -n "$QUIET" ]; then
  perl -0pi -e 's{<rss }{<rss '"$NS"' }' "$FEED"
  for v in $QUIET; do
    V="$v" perl -0pi -e 's{(<sparkle:shortVersionString>\Q$ENV{V}\E</sparkle:shortVersionString>)}{$1\n            <passband:quiet>yes</passband:quiet>}' "$FEED"
    echo "    quiet: $v (no release push, no banner)"
  done
fi

echo "==> ../passband-site/appcast.xml regenerated from $(ls releases/*.zip | wc -l | tr -d ' ') archive(s)"
echo "    commit and push passband-site to publish"
