#!/usr/bin/env bash
# Renders the demo film.
#
#   ./brand/demo/build.sh [out.mp4]              the whole film, with its score
#   ./brand/demo/build.sh --stills DIR 3 12.5    single frames at those seconds
#
# The score is made first in both modes, because the film's analyzer is an FFT
# of it: a change to the sound is a change to the picture, so there is no
# audio-only remux.
#
# Everything runs on a virtual clock and a seeded random, so two renders are
# the same film frame for frame.
set -euo pipefail
cd "$(dirname "$0")"
site=../../passband-site

[[ -d node_modules ]] || bun install >/dev/null
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

bun build film.ts --outfile "$tmp/film.js" >/dev/null
cp film.html "$site/mark.svg" "$site/newsreader-var.woff2" "$tmp/"
bun run score.ts "$tmp/raw.wav"
./loudness.sh "$tmp/raw.wav" "$tmp/score.wav"

if [[ "${1:-}" == "--stills" ]]; then
  shift
  out=$1; shift
  bun run render.ts "$tmp" stills "$out" "$@"
  echo "wrote stills to $out"
else
  out=${1:-passband-demo.mp4}
  bun run render.ts "$tmp" video "$out" "$tmp/score.wav"
  echo "wrote $out"
fi
