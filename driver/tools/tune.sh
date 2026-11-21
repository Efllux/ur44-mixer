#!/bin/zsh
# Latency/reliability check of the installed UR44 plug-in for one timing configuration.
# Usage: tools/tune.sh requests,microframes,outMargin,inMargin,direct,guard,scanDelay,scanPeriodUs
# Needs: UR44 in Yamaha mode, plug-in installed, LOOPBACK on (MIX 1 -> inputs), the mixer app quit.
set -u
cd "${0:a:h}/.."
CFG=$1
RT=../tools/roundtrip
./build/ur_hal_test -c "$CFG" >/dev/null 2>&1 || { echo "config $CFG rejected"; exit 1; }
LEAD=$(./build/ur_hal_test -q 2>&1 | awk '/outputLeadFrames|inputDelayFrames/{printf "%s=%s ", $1, $2}')
echo "== $CFG  ($LEAD)"
for cfg in "48000 16" "48000 32" "96000 32"; do
    set -- $(echo $cfg)
    $RT UR44 $1 $2 1 >/dev/null 2>&1          # settle after a rate change
    $RT UR44 $1 $2 3 | sed 's/Steinberg UR44 @ //' | cut -d'|' -f1,2
done
# Reliability: 20 s at 48 kHz / 16 frames, idle and with all cores loaded.
for load in idle loaded; do
    pids=()
    if [[ $load == loaded ]]; then
        for i in $(seq 1 $(sysctl -n hw.ncpu)); do yes >/dev/null & pids+=($!); done
    fi
    ./build/ur_hal_test -r 48000 -b 16 -s 20 >/dev/null 2>&1
    [[ ${#pids[@]} -gt 0 ]] && kill $pids 2>/dev/null
    ./build/ur_hal_test -q 2>&1 | awk -v L=$load '/inputMissing|outputLate|resyncs|inErrors|outErrors|gapFrames/{printf "%s=%s ", $1, $2} END{print "  [" L "]"}'
done
