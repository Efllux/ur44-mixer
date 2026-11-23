#!/bin/zsh
# Per-Mac calibration of the UR44 plug-in: the lowest-latency timing that is stable on THIS machine,
# per sample rate, measured through the UR44's internal LOOPBACK (Mix 1 -> computer inputs), so no
# cable is needed. Counterpart of `vboxusb-gate calibrate` on Windows.
#
# UNTESTED: written on Windows without access to a Mac. Read it before the first run, and try it
# with short times first:  tools/calibrate.sh --screen 5 --validate 20 --rates 48000
#
# Usage: tools/calibrate.sh [--rates 48000,96000,192000] [--screen 20] [--validate 300]
#                           [--margin 1] [--load] [--apply | --no-apply]
# Needs: UR44 in Yamaha mode, plug-in installed, the UR44 Mixer app quit (it holds interface 4),
#        ./build/ur_hal_test, ../tools/roundtrip (compiled roundtrip.swift) and ur44ctl (PATH or
#        ../target/release/ur44ctl).
#
# How it works (per rate):
#  1. Timing ladder, safe -> aggressive, at a fixed client buffer (32 frames x rate/48k). Each step
#     is screened for --screen seconds with ur_hal_test; it passes when the plug-in's error counters
#     (inputMissing, outputLate, resyncs, inErrors, outErrors, gapFrames, races) did not move.
#     The walk stops at the first failure.
#  2. With the chosen timing (last pass minus --margin steps), the client buffer ladder 64/32/16/8
#     (scaled by rate) is screened the same way.
#  3. The result is validated for --validate seconds (with every core busy if --load); a failure
#     backs off one step. Then tools/roundtrip measures the round trip.
# The plug-in configuration ('ucfg') is global, not per rate: with --apply (default) the script
# installs the most aggressive timing that passed at EVERY tested rate; --no-apply restores the
# original. LOOPBACK and (with --no-apply) the configuration are restored on exit, also on Ctrl+C.
set -u
cd "${0:a:h}/.."

RATES=(48000 96000 192000); SCREEN=20; VALIDATE=300; MARGIN=1; LOAD=0; APPLY=1
while (( $# )); do
    case $1 in
        --rates) RATES=(${(s:,:)2}); shift ;;
        --screen) SCREEN=$2; shift ;;
        --validate) VALIDATE=$2; shift ;;
        --margin) MARGIN=$2; shift ;;
        --load) LOAD=1 ;;
        --apply) APPLY=1 ;;
        --no-apply) APPLY=0 ;;
        *) echo "unknown option $1"; exit 2 ;;
    esac
    shift
done

HAL=./build/ur_hal_test
RT=../tools/roundtrip
CTL=$(command -v ur44ctl || echo ../target/release/ur44ctl)
for x in $HAL $RT $CTL; do [[ -x $x ]] || { echo "missing $x (build it first)"; exit 1; }; done
if pgrep -xq ur44-mixer; then echo "Quit the UR44 Mixer app first (it holds the mixer interface)."; exit 1; fi

# Timing ladder: requests,microframes,outMargin,inMargin,direct,guard,scanDelay,scanPeriodUs.
# Direct write+read (3) makes latency independent of the queue; the margins (frames at 44.1 kHz)
# and the guard/scan timing are the knobs. Step 2 is the hand-tuned default of 2026-11-07.
LADDER=(
    "2,512,8,8,3,4,1,250"
    "2,512,4,4,3,2,0,125"
    "2,512,0,2,3,1,0,125"
    "2,512,0,1,3,1,0,125"
    "2,512,0,0,3,1,0,125"
    "2,512,0,0,3,0,0,125"
)
BUFS48=(64 32 16 8)

COUNTERS='inputMissing|outputLate|resyncs|inErrors|outErrors|gapFrames|races'
# stats_of <ur_hal_test output>: "<inputFrames> <errors>", errors = the plug-in error counters plus
# the sample time breaks the client saw (only in the output of a streaming run).
stats_of() {
    print -r -- "$1" | awk -v re="^ *($COUNTERS) " '
        /^ *inputFrames / {f = $2}
        /sample time breaks in\/out:/ {split($NF, b, "/"); s += b[1] + b[2]}
        $0 ~ re {s += $2}
        END {printf "%d %d\n", f, s}'
}
current_config() {   # the 8 numbers of the active configuration, comma separated
    $HAL -q 2>&1 | awk '
        /^Configuration:/ {c=1; next} /^[^ ]/ {c=0}
        c && $1=="requests" {v[0]=$2} c && $1=="microframes" {v[1]=$2} c && $1=="outputMargin" {v[2]=$2}
        c && $1=="inputMargin" {v[3]=$2} c && $1=="direct" {v[4]=$2} c && $1=="guardMicroframes" {v[5]=$2}
        c && $1=="scanDelayMicroframes" {v[6]=$2} c && $1=="scanPeriodUs" {v[7]=$2}
        END {printf "%s,%s,%s,%s,%s,%s,%s,%s", v[0],v[1],v[2],v[3],v[4],v[5],v[6],v[7]}'
}
loopback_get() { $CTL get PatchLoopbackMode 0 2>/dev/null | awk -F'= ' '/=/{print $2+0}'; }

LOADPIDS=()
load_on()  { (( LOAD )) || return; for i in $(seq 1 $(sysctl -n hw.ncpu)); do yes >/dev/null & LOADPIDS+=($!); done; }
load_off() { (( ${#LOADPIDS[@]} )) && kill $LOADPIDS 2>/dev/null; LOADPIDS=(); }

ORIG_CFG=$(current_config)
$CTL version >/dev/null 2>&1   # wake the control link (on Windows the first message on a fresh link got lost)
ORIG_LB=$(loopback_get)
[[ -n $ORIG_LB ]] || { echo "ur44ctl cannot read PatchLoopbackMode (UR44 in Yamaha mode? mixer app quit?)"; exit 1; }
FINAL_CFG=$ORIG_CFG
restore() {
    load_off
    $HAL -q -c "$FINAL_CFG" >/dev/null 2>&1 && echo "plug-in configuration: $FINAL_CFG"
    $CTL set PatchLoopbackMode 0 $ORIG_LB >/dev/null 2>&1 && echo "LOOPBACK restored to $ORIG_LB" \
        || echo "WARNING: could not restore LOOPBACK; run: ur44ctl set PatchLoopbackMode 0 $ORIG_LB"
}
trap restore EXIT
trap 'echo; echo "interrupted"; exit 130' INT TERM

echo "original configuration $ORIG_CFG, LOOPBACK $ORIG_LB"
$CTL set PatchLoopbackMode 0 1 >/dev/null || { echo "could not switch LOOPBACK on"; exit 1; }
echo "LOOPBACK on"

# run <cfg> <rate> <buf> <secs> -> 0 if the run was clean; $LAST describes it.
# -q with -c: apply the configuration only (without -q, ur_hal_test would also record for 5 s).
run() {
    LAST="configuration $1 not applied"
    $HAL -q -c "$1" >/dev/null 2>&1 || return 1
    local b=(${=$(stats_of "$($HAL -q 2>&1)")})
    load_on
    local out; out=$($HAL -r $2 -b $3 -s $4 -o /dev/null 2>&1); local rc=$?
    load_off
    local a=(${=$(stats_of "$out")})
    # The engine resets its counters when it starts (ur_engine_start). If it kept running for another
    # client, they are cumulative: then inputFrames grew by about the whole run.
    local errs=${a[2]}
    (( a[1] >= b[1] + $4 * $2 / 2 )) && errs=$(( a[2] - b[2] ))
    LAST="exit $rc, $errs error(s)/break(s)"
    (( rc == 0 && errs == 0 ))
}
scale() { echo $(( $1 * $2 / 48000 < 1 ? 1 : $1 * $2 / 48000 )); }

typeset -A BEST_STEP BEST_BUF RT_MS
for rate in $RATES; do
    echo "\n=== $rate Hz"
    buf=$(scale 32 $rate)
    $RT UR44 $rate $buf 1 >/dev/null 2>&1   # settle after a rate change
    best=-1
    for i in {1..${#LADDER}}; do
        if LOAD=0 run ${LADDER[i]} $rate $buf $SCREEN; then
            echo "  timing step $((i-1)) pass  (${LADDER[i]})"; best=$i
        else
            echo "  timing step $((i-1)) FAIL (${LADDER[i]}): $LAST"; break
        fi
    done
    (( best > 0 )) || { echo "  not stable even with the safe timing"; continue; }
    pick=$(( best - MARGIN < 1 ? 1 : best - MARGIN ))
    # client buffer ladder at the picked timing
    bbest=0
    for b48 in $BUFS48; do
        b=$(scale $b48 $rate)
        if LOAD=0 run ${LADDER[pick]} $rate $b $SCREEN; then echo "  buffer $b pass"; bbest=$b48; else echo "  buffer $b FAIL: $LAST"; break; fi
    done
    (( bbest > 0 )) || { echo "  no buffer size passed"; continue; }
    # one buffer step of headroom too, then validate, backing off on failure
    bpick=$bbest
    (( MARGIN > 0 && bbest < ${BUFS48[1]} )) && bpick=$(( bbest * 2 ))
    while (( pick >= 1 )); do
        b=$(scale $bpick $rate)
        echo "  validating timing step $((pick-1)), buffer $b for $VALIDATE s$( (( LOAD )) && echo ' under load') ..."
        if run ${LADDER[pick]} $rate $b $VALIDATE; then break; fi
        echo "  validation FAILED ($LAST), backing off"; pick=$(( pick - 1 ))
    done
    (( pick >= 1 )) || { echo "  nothing passed validation"; continue; }
    line=$($RT UR44 $rate $b 4 2>&1 | grep 'APP ROUND TRIP')
    ms=$(echo $line | sed -E 's/.*= ([0-9.]+) ms.*/\1/')
    echo "  round trip $ms ms  ($line)"
    BEST_STEP[$rate]=$pick; BEST_BUF[$rate]=$b; RT_MS[$rate]=$ms
done

echo "\n=== calibration result"
printf "%7s  %-24s %7s %10s\n" rate timing buffer "round trip"
common=${#LADDER}
for rate in $RATES; do
    if [[ -n ${BEST_STEP[$rate]-} ]]; then
        printf "%7s  %-24s %7s %8s ms\n" $rate ${LADDER[${BEST_STEP[$rate]}]} ${BEST_BUF[$rate]} ${RT_MS[$rate]}
        (( BEST_STEP[$rate] < common )) && common=${BEST_STEP[$rate]}
    else
        printf "%7s  not stable\n" $rate
    fi
done
if (( APPLY && ${#BEST_STEP} )); then
    FINAL_CFG=${LADDER[common]}
    echo "applying the timing stable at every tested rate: $FINAL_CFG"
fi
OUT="$HOME/Library/Application Support/UR44/calibration.json"
mkdir -p "${OUT:h}"
{
    echo "{\"tool\": \"driver/tools/calibrate.sh\", \"validate_secs\": $VALIDATE, \"load\": $LOAD, \"applied\": \"$FINAL_CFG\", \"rates\": ["
    sep=""
    for rate in $RATES; do
        if [[ -n ${BEST_STEP[$rate]-} ]]; then
            echo "$sep  {\"rate\": $rate, \"stable\": true, \"timing\": \"${LADDER[${BEST_STEP[$rate]}]}\", \"buffer\": ${BEST_BUF[$rate]}, \"rt_ms\": ${RT_MS[$rate]:-0}}"
        else
            echo "$sep  {\"rate\": $rate, \"stable\": false}"
        fi
        sep=","
    done
    echo "]}"
} > "$OUT"
echo "saved $OUT"
