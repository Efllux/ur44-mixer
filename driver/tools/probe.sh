#!/bin/zsh
# Quick probe: latency at one rate/buffer + 60 s idle reliability for a timing config.
# Usage: tools/probe.sh <cfg> <rate> <buffer> [seconds]   (LOOPBACK on, agent quit)
set -u
cd "${0:a:h}/.."
./build/ur_hal_test -c "$1" >/dev/null 2>&1 || { echo "config $1 rejected"; exit 1; }
../tools/roundtrip UR44 $2 $3 1 >/dev/null 2>&1
L=$(../tools/roundtrip UR44 $2 $3 3 | sed 's/.*APP ROUND TRIP //' | cut -d'|' -f1)
./build/ur_hal_test -r $2 -b $3 -s ${4:-60} >/dev/null 2>&1
S=$(./build/ur_hal_test -q 2>&1 | awk '/inputMissing|outputLate|inErrors|resyncs/{printf "%s=%s ", $1, $2}')
echo "$1 @ $2/$3: $L| $S"
