# vboxusb-gate

Test tool: UR44 audio on Windows through the Microsoft-catalog-signed `VBoxUSB.sys` that
usbipd-win installs, with no driver of our own. It talks to the driver over its IOCTLs only
(struct layouts written from the documented interface; no VirtualBox code). MIT.

This is a feasibility and calibration tool, not a driver: no DAW or Windows app can use the
UR44 through it. Results and the full test history are in
[`docs/winusb-gate-results.md`](../../docs/winusb-gate-results.md), sections 8 and 9.

## Results on the development PC (digital loopback, 300 s validated)

| Rate | Round trip | Setting |
|---|---|---|
| 48 kHz | 1.78 ms | 4 ms queue, late write ~0.8 ms ahead |
| 96 kHz | 1.38 ms | 4 ms queue, late write ~0.4 ms ahead |
| 192 kHz | 1.57 ms | 4 ms queue, late write ~0.6 ms ahead (sporadic one-off hiccups; less proven) |

macOS reaches 1.55–1.71 ms with the same method.

## Setup (once, needs admin)

1. Install [usbipd-win](https://github.com/dorssel/usbipd-win) (tested: 5.3.0, VBoxUSB 7.2.2).
2. `usbipd bind --force --hardware-id 0499:150e`, then replug the UR44 or restart it.
   VBoxUSB now owns the **whole** device: the UR44 Mixer app and `ur44ctl` cannot reach it until
   you unbind (`usbipd unbind --hardware-id 0499:150e`).

`scripts/setup-elevated.ps1` does both in one elevated run and registers an on-demand task
`UR44-VBoxGate-Restore` that undoes the bind without another admin prompt
(`schtasks /run /tn UR44-VBoxGate-Restore`; `scripts/restore-elevated.ps1` is what it runs).

## Build

```powershell
cargo build --release   # target\release\vboxusb-gate.exe
```

## Commands

```text
vboxusb-gate list                  device paths of the VBoxUSB interface
vboxusb-gate probe                 open and claim, select the streaming interfaces, set the rate, idle
vboxusb-gate loopback [on|off]     read or switch the UR44's internal LOOPBACK (mixer param 100)
vboxusb-gate calibrate [options]   find the lowest stable latency per rate on this PC
vboxusb-gate stream [options]      one playback/capture run with live checks
```

**calibrate**: `--rates 48000,96000,192000 --screen-secs 20 --validate-secs 300 --margin 1
[--load] [--out FILE]`. It switches LOOPBACK on (no cable needed), walks a ladder from safe to
aggressive settings (a failed step is re-checked once; the walk stops at a step that fails twice),
backs off `--margin` steps, validates for `--validate-secs` (with every core busy under `--load`)
and measures the round trip with clicks. LOOPBACK is restored at the end, also after Ctrl+C. The
result goes to `%LOCALAPPDATA%\ur44-vboxgate\calibration.json` unless `--out` is given. All three
rates take about 25 min.

**stream** (defaults shown): `--rate 48000 --mode sine|click|silence --secs 2 --out-depth 8
--in-depth 8 --min-queue 4 --write-lead 0 --write-delay-us 0 --in-ch 0 --thr 0.02`, plus `--freq`
(1 kHz scaled with the rate) and `--amp` (0.25 sine, 0.5 click; at most 0.9). Runs are capped at
600 s.
`--write-lead N` fills each OUT transfer N transfers before it is sent (late write, lower latency);
`--write-delay-us` waits that long before the write. Set your monitor volume low: `sine` and
`click` go to MAIN OUT.

## Safety rules built in

- Every isochronous transfer has exactly 8 packets (1 ms); anything else exits the process.
- A run stops at the first transfer error, late or not-accessed packet, empty feedback, or a queue
  below `--min-queue` (floor 3: the driver re-anchors silently when a queue runs dry).
- Queued transfers always drain at the end; nothing is aborted mid-stream (an abort can mute the
  UR44 outputs). Ctrl+C stops cleanly.

## Scripts

| Script | Use |
|---|---|
| `scripts/setup-elevated.ps1` | install usbipd-win, force-bind the UR44, register the restore task (admin) |
| `scripts/restore-elevated.ps1` | unbind, restart the usbipd service, remove the tasks (run by the task) |
| `scripts/launch-setup.ps1` | start the setup with one UAC prompt |
| `scripts/run-one.ps1 -Name X -- <args>` | one logged run, plus an event-log check, into `results/gate-log.txt` |
| `scripts/check-events.ps1 -Since <time>` | System/Application errors and crash markers since a time |

## Known limits

- One owner: while bound, only one process can use the UR44 (audio and mixer together).
- Tested only with Secure Boot and HVCI off, on one PC; 44.1/88.2/176.4 kHz not tested.
- Depends on usbipd-win's VBoxUSB build; re-check when it changes.
- Once (2026-11-23), a `stream` started right after `loopback` stopped on its very first IN
  transfer with "not accessed"; the immediate rerun was clean. The run stops safely in that case
  (stop-on-first-error), so just run it again.
