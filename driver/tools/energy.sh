#!/bin/zsh
# Energy comparison of backup-scanner periods (needs sudo for powermetrics).
# Usage: sudo zsh driver/tools/energy.sh
set -u
D="${0:a:h}/.."
OWNER=${SUDO_USER:-$USER}
OUT=/tmp/ur44-energy.txt
CA=$(pgrep -x coreaudiod)
HOST=$(pgrep -f 'Core Audio Driver \(UR44.driver\)')
echo "coreaudiod $CA, UR44 host $HOST"
run() { sudo -u "$OWNER" "$D/build/ur_hal_test" "$@"; }
summary() {   # $1 label, $2 file. Task lines: name (may contain spaces), pid, CPU ms/s, ..., energy impact last.
  awk -v ca=$CA -v h=$HOST -v L=$1 '{
      for (i = 2; i < NF; i++) if ($i == ca || $i == h) {
        if ($i == ca) {c++; cc += $(i+1); ce += $NF} else {u++; uc += $(i+1); ue += $NF}
        break } }
    END { printf "%-6s coreaudiod: %.1f ms/s CPU, energy %.1f | UR44 host: %.1f ms/s CPU, energy %.1f\n",
          L, c?cc/c:0, c?ce/c:0, u?uc/u:0, u?ue/u:0 }' $2
}
for cfg in 125 4000; do
  run -c 6,512,0,2,3,1,0,$cfg >/dev/null 2>&1
  run -r 48000 -b 32 -s 75 >/dev/null 2>&1 &
  p=$!
  sleep 8
  powermetrics --samplers tasks --show-process-energy -i 5000 -n 12 > $OUT.$cfg 2>/dev/null
  wait $p
  sleep 5
done
run -c 6,512,0,2,3,1,0,125 >/dev/null 2>&1
echo "Per-second averages while streaming 48 kHz / 32 frames (60 s each):"
for cfg in 125 4000; do summary "${cfg}us" $OUT.$cfg; done
echo "Errors: $(run -q 2>&1 | awk '/inputMissing|outputLate/{printf "%s=%s ", $1, $2}')"
