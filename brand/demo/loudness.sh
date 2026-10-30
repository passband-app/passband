#!/usr/bin/env bash
# Brings the score to -16 LUFS with ONE linear gain, capped so the true peak
# stays under -2 dBTP. Never ffmpeg's dynamic loudnorm: that one is a
# limiter, and it is what made the first kicks crunch.
#
#   ./loudness.sh in.wav out.wav
set -euo pipefail
stats=$(ffmpeg -hide_banner -i "$1" -af loudnorm=print_format=json -f null - 2>&1 | sed -n '/^{/,/^}/p')
gain=$(python3 -c '
import json, sys
m = json.loads(sys.argv[1])
i, tp = float(m["input_i"]), float(m["input_tp"])
print(f"{min(-16 - i, -2.0 - tp):.2f}")
' "$stats")
ffmpeg -y -loglevel error -i "$1" -af "volume=${gain}dB" -c:a pcm_s16le "$2"
echo "score gain ${gain} dB" >&2
