#!/usr/bin/env bash
# Two native clients in a call; a spare connection for A opens and closes
# mid-call. Prints what B hears of A, second by second.
set -euo pipefail
T=${T:-target/debug/chatter-testclient.exe}
R=$1; OUT=${2:-recordings/spare}
rm -rf "$OUT"; mkdir -p "$OUT"
$T voice native_a --room "$R" --tone-hz 440 --wav "${WAV:-recordings/tone-continuous.wav}" --seconds 24 --out "$OUT/a" > "$OUT/a.log" 2>&1 &
sleep 1
$T voice native_b --room "$R" --seconds 22 --out "$OUT/b" > "$OUT/b.log" 2>&1 &
sleep 8
$T connect native_a --seconds 2 > "$OUT/spare.log" 2>&1
echo "spare connection closed at $(date +%T)"
wait
grep -E "slot [0-9]+ \(" "$OUT/b.log" | sed -E 's/^\[([^ ]+) .*slot ([0-9]+) \((.*)\): (.*)$/\1 slot \2 \3 \4/' | cut -c12-
