# Resource measurements, 2026-11-07

Development notes. Apple Silicon, macOS 26.6.2,
UR44 in Yamaha mode. No sudo, plug-in installation, coreaudiod restart or push.

## Changes retained

- Add frame-sized client calls and streaming-only process CPU accounting to
  `ur_engine_test`; `-N` avoids allocating/storing/writing recordings. Fix its
  scratch output buffer, which previously had room for four of six channels.
- Replace the mixer's 200 ms USB read timeout with a pending asynchronous bulk
  read. Drop interrupts the event loop, cancels the read and drains its callback
  before releasing interface 4. Read-back and three reconnect/cancel cycles pass;
  cancellation takes 3.1–3.3 ms. The whole-device libusb open remains unchanged.
- Change the engine queue from 8 × 128 to 2 × 512 microframes: still 128 ms total,
  with four times fewer USB requests/completions. Direct read/write, margins and
  scanner timing remain unchanged. Startup took about 279 ms versus 256 ms.

## Measurement method

Engine CPU is user + system time from `getrusage(RUSAGE_SELF)` during the streaming
loop, normalized to one minute. Each comparison lasts 35 s. `/usr/bin/time -l`
provides an independent whole-process measurement including startup and drain.
The process includes the client harness, not Core Audio/HAL. `-b` selects call
cadence and block size; the harness retains its existing 512-frame output
lookahead. Reported latency is the engine's lead/delay, not a measured round trip.

```
/usr/bin/time -l driver/build/ur_engine_test -r 48000 -b 32 -N -s 35
/usr/bin/time -l driver/build/ur_engine_test -r 48000 -b 15 -N -s 35
```

The before binary used the same enhanced harness and the original engine. It was
saved before engine changes. Engine tests ran sequentially, never concurrently.

| Measurement | Before | After |
|---|---:|---:|
| 48 kHz / 32, process CPU s/min | 1.495 | 1.312 |
| 48 kHz / 15, process CPU s/min | 1.666 | 1.394 |
| Output lead / input delay at 48 kHz, frames | 6 / 15 | 6 / 15 |
| Mixer background CPU s/min, 30 s interval | 0.069 | 0.025 |
| Mixer package-idle wakeups/s, 30 s interval | 0.10 | 0.27 |
| Mixer interrupt wakeups/s, 30 s interval | 0.63 | 0.70 |
| Mixer RSS (`ps`, KiB) | 87,232 | 84,272 |
| Mixer physical footprint, MiB | 19.0 | 20.2 |

The 32-frame queue-only result was 0.179385 user + 0.586238 system = 0.765623 CPU s
in 35.000 s (1.312496 s/min); whole process was 0.18 user + 0.60 system over 35.85 s.
The before result was 0.193429 + 0.678516 = 0.871945 CPU s (1.494763 s/min).
The final 15-frame result was 0.203216 user + 0.610001 system = 0.813217 CPU s
in 35.000 s (1.394086 s/min); whole process was 0.20 user + 0.62 system over 35.41 s.
All four required fault counters were zero in the before and after comparisons.

Agent measurements used `top -l 2 -s 20 -pid PID -stats pid,cpu,idlew,mem` and `ps`,
plus 30 s deltas of `proc_pid_rusage`. Its CPU fields on this machine use Mach
absolute-time units (timebase 125/3), verified against `getrusage` with a busy loop.
`top` IDLEW is cumulative in these runs: the interval change is what matters.
Old-agent `top` showed 3 → 5 over 20 s and 0.1% CPU; the installed new agent showed
8 → 16 over 20 s and 0.0% CPU. The supplied 10.6 wakeups/s reference did not
reproduce; these runs do not establish a 10× wakeup reduction. RSS differences
are small and the physical footprint did not improve; no memory optimization is
claimed. The agent was restored in background mode after each hardware test.

## Reliability

- Idle: 180 s at 48 kHz / 32 frames. inputMissing 0, outputLate 0, USB errors
  in/out 0/0, resyncs 0, gap frames 0, empty packets 0, direct-write races 0.
  CPU 3.872512 s / 180 s = 1.290837 s/min. Lead/delay 6/15 frames.
- Loaded: 180 s at 48 kHz / 15 frames, ten `yes` processes (one per
  `sysctl -n hw.ncpu`), all verified at nice level 10. inputMissing 0,
  outputLate 0, USB errors in/out 0/0, resyncs 0, gap frames 0, empty packets 0,
  direct-write races 0. CPU 3.971518 s / 180 s = 1.323839 s/min. Lead/delay 6/15.
  Maximum capture/playback callback lag 5.54/5.42 ms. All workers terminated in
  a Python `finally` block; the harness and a separate final `pgrep -x yes`
  both confirmed empty. There was one full-load run.

`cargo test` passes all 28 library tests and all binary/doc test targets.
`driver/build.sh` builds the universal plug-in/tools without warnings.
`cargo build --release` passes; its pre-existing `block v0.1.6` future-compatibility
warning remains. The final agent is installed and running with `--background`.
LOOPBACK read-back was 0; the saved restore state also contains 0. Raw command
logs are retained locally in `/tmp/ur44-resources` (not committed).

## Exploratory experiments discarded

- Scanning on client reads/timestamps instead of the 8 kHz thread: zero faults in
  short runs, fewer instructions/wakeups, but worse measured CPU (2.35 s/min at
  32 frames and 2.43 at 15). A control with the original engine was 1.55 s/min.
- A USB-clock-aligned 250 µs scanner: 2.87 s/min, despite fewer wakeups. Discarded.
- Skipping conversion of an empty playback ring: only 1.31 → 1.29 s/min, within
  observed variability. Discarded to keep the patch small.

The ≤0.55 CPU s/min target is not met by the standalone process. Exact attribution
excluding the client harness was not measured. New plug-in CPU, idle CPU and HAL
behavior cannot be verified until the owner installs it. These engine numbers
must not be presented as a like-for-like comparison with Yamaha's dext process
or the earlier whole-coreaudiod measurements.

## Owner installation

The mixer agent is already built, bundled, installed and restarted in background
mode. Only `driver/build/UR44.driver` needs installation over
`/Library/Audio/Plug-Ins/HAL/UR44.driver`, followed by the owner's usual Core Audio
reload. Nothing privileged was performed here.

A persisted old timing configuration overrides the new defaults. With audio
stopped, apply this after installation:

```
driver/build/ur_hal_test -c 2,512,0,2,3,1,0,125
driver/build/ur_hal_test -q
```

Confirm outputLeadFrames 6 and inputDelayFrames 9 at 48 kHz (15 before the on-demand scan). Recheck HAL streaming
at 32/15 frames and plug-in idle CPU. The larger requests were hardware-tested at
48 kHz; other supported rates and macOS versions need validation. The queue keeps
the same total depth but now has only two requests, making the loaded-run result
particularly relevant.

## On-demand capture scan (commit 67295e0, branch driver/on-demand-scan)

The 8 kHz real-time scanner was the largest cost. Capture is now scanned when the IO
thread reads input or the clock; a backup thread (scanPeriodUs, now 4000 µs) scans
only when nobody has. Engine harness, 48 kHz / 32-frame real-time client, marginal
per second of streaming:

| | before | after |
|---|---|---|
| cycles/s | 94 M | 40 M |
| instructions/s | 140 M | 73 M |
| context switches / 20 s | 194 k | 36 k |
| input delay @48k | 15 frames | 9 frames |

CPU-seconds rose (1.3 → 2.15 s/min) although cycles fell 2.3×: without the 8 kHz
real-time thread the cores stay at a low clock, so the same kernel work (USB
enqueue calls) takes longer in wall-clock CPU time. Cycles and instructions are the
frequency-independent measure; energy follows cycles. Reliability: 3 min idle and
3 min all-cores load at 48k/15, 60 s at 96k/29 and 192k/58: zero missing/late.

Known, pre-existing (same on the old build): the first stream after switching to
192 kHz reports ~800 empty packets and nearly all output late; the next start is
clean. Starting right after a rate change could fail with -10 (rate request
refused); the engine now retries for up to 750 ms.

### Correction after installation (2026-11-07)

In coreaudiod, on-demand scanning by the HAL IO thread costs more, not less (top, 48k/32,
30 s each, alternating): backup period 125 µs → 1.7 % CPU; 250, 1000 or 4000 µs → 5.3–5.5 %.
The harness does not reproduce this; the cause is unknown (coreaudiod cannot be sampled
without root). Applied config: 2,512,0,2,3,1,0,125 (input delay 9 frames, zero errors).
Next: count scans per source in the 'usta' stats to find why the IO-thread path is expensive.

## Core Audio CPU investigation — `codex/coreaudiod-cpu` (2026-11-07)

Based on `driver/on-demand-scan` at `acb2fd3`. All changes and builds are local;
no sudo, install, merge or push. Raw logs, experimental binaries and the sample
are in `/tmp/ur44-coreaudiod-cpu`. Existing `driver/recordings/` was left alone.

### What is consuming the CPU

The original engine reproduces the earlier profile at 48 kHz / 32: with a
4000 µs backup period, 2.014 CPU s/min, 42.406 M whole-process cycles per streaming
second (`time -l`, includes startup/drain), and 7.545 / 8.546 ms/s inside capture /
playback enqueue respectively. Completion-body inclusive times were 8.942 /
9.746 ms/s. Do not add enqueue times to completion times: they are nested.

An 8 s, 1 ms `sample` of the unmodified engine found 93 of 116 sampled USB
completion stacks inside the enqueue Mach trap, reached through
`IOConnectCallAsyncMethod`. The public transaction-list entry point is a short
wrapper around the internal versioned frame-list method. This is evidence of
kernel/user-client submission work, not a demonstrated user-space transaction
conversion or autorelease hotspot. Without privileged kernel profiling we cannot
separate kernel transaction construction, descriptor mapping and page wiring.
Enqueue counters measure elapsed call time, including any blocking/preemption;
they must not be equated exactly with CPU time.

Controlled alternatives (20 s, 4000 µs backup, 6/9-frame lead/delay, zero faults):

| Submission | Whole-process cycles / streaming s¹ | Streaming CPU s/min | Enqueue in/out, ms/s |
|---|---:|---:|---:|
| Original transaction list | 42.406 M | 2.014 | 7.545 / 8.546 |
| Frame list, reusable ordinary allocation | 43.844 M | 1.781 | 6.311 / 7.394 |
| Transaction list allocated with `ioDataWithCapacity` | 43.022 M | 2.095 | 7.849 / 8.980 |
| Frame list allocated with `ioDataWithCapacity` | 42.598 M | 2.117 | 8.002 / 9.163 |

¹ `time -l` cycles divided by 20 s; startup/drain are included, so these are
approximate rates. The final harness also reports streaming-only cycles and
instructions using `proc_pid_rusage(RUSAGE_INFO_V4)`.

The frame-list API did not reduce instructions/cycles or establish a reproducible
CPU benefit, so none of these experimental paths is shipped. Lists were reused,
not allocated per completion. The only documented transfer option is `None`.
Apple's current SDK recommends `dataWithCapacity:options:error:` with
`IOUSBHostObjectDataOptionsKernelUserShared` for isochronous lists. **That selector
is absent on this machine**, despite macOS reporting 26.6.2 and the SDK marking it
26.4+. An isolated experiment raised `NSInvalidArgumentException` before queuing
I/O. No call to that unavailable API is in the delivered build. Reference:
[Apple IOUSBHostObjectDataOptions](https://developer.apple.com/documentation/iousbhost/iousbhostobjectdataoptions).

### The plug-in has its own host process

On this machine the plug-in runs as `_coreaudiod` in a separate
`Core Audio Driver (UR44.driver)` process, in addition to `/usr/sbin/coreaudiod`.
The earlier 1.7% measurement covered only the latter. `top -pid PID -pid HOSTPID`
accepts repeated `-pid` arguments; a comma-separated PID list is invalid.
Two steady 8 s samples, installed old plug-in, 48k/32, otherwise idle:

| Backup period | coreaudiod CPU | UR44 host CPU | Combined CPU s/min (approx.) | Engine lead/delay |
|---|---:|---:|---:|---:|
| 125 µs | 1.7% / 1.7% | 2.2% / 2.3% | 2.34–2.40 | 6 / 9 frames |
| 4000 µs | 5.4% / 5.3% | 3.9% / 3.6% | 5.34–5.58 | 6 / 9 frames |

Both 22 s HAL tests had zero inputMissing/outputLate, USB errors, resyncs, gaps,
races and sample-time breaks. The 125 µs configuration was restored in `finally`.
`proc_pid_rusage` on these protected processes returns `Operation not permitted`;
no privileged profiling was attempted. The fresh build exports `usta.hostPid` to
identify whichever process actually hosts the plug-in after reinstall.

The scanner-period change affects both processes, not just scanner execution.
Clock frequency / scheduling is a plausible contributor: the harness does much
less instruction/cycle work with a 4000 µs backup yet uses more CPU-seconds, and
the same enqueue call takes roughly three times longer. That does not prove the
cause of the Core Audio increase. The installed plug-in has no profiling counters;
its zero-timestamp call rate and spin retries still require the new build.

### Changes retained and before/after

- Binary-search the first playback transaction for each client block instead of
  walking up to 511 preceding transactions. Subsequent frames advance linearly.
- Skip the input scan when the whole requested range is already published.
- Make statistics a timestamp snapshot; reading stats no longer performs audio
  scanning or increments `zeroCall`. This also removes the harness's artificial
  second timestamp scan every client cycle.
- Restore the fresh-install scanner default to the measured known-good 125 µs.
  Queue depth, guard, scan delay, margins and timestamp period remain unchanged.
- Keep all profiling counters; wire `halCycle` to actual input operations and add
  `readReady`, `refRetry`, `zeroRetry`. Retry counts are loop iterations, not calls.
  Count retries locally and publish once, avoiding an atomic operation per spin.
- Fix `ur_hal_test`'s 64-entry stack arrays: `CFDictionaryGetKeysAndValues` writes
  every dictionary key regardless of a local count clamp. The larger stats
  dictionary now uses allocated key storage (60 keys in this build). ASan with 100 keys passes.
- Harness `-p` selects backup period; streaming-only cycles/instructions are
  printed alongside CPU-seconds. `-a` selects the output-end target (default
  512). At `-b 32 -a 64` the next write normally begins about one buffer ahead;
  `-a 32` places it at the guard and causes late frames in **both** old/new engines.
  This exploratory failure is not an engine latency improvement or regression.

Same enhanced harness, old/new engine, 25 s each, 48k/32, 125 µs backup, `-N -a 64`:

| Metric | Before (`acb2fd3` engine) | After |
|---|---:|---:|
| Streaming cycles/s | 96.129 M | 106.768 M |
| Streaming instructions/s | 143.569 M | 140.804 M |
| Streaming process CPU s/min | 1.3245 | 1.4754 |
| `readCall` elapsed µs/s | 78.4 | 29.6 |
| `writeCall` elapsed µs/s | 361.8 | 257.4 |
| Enqueue in/out elapsed ms/s | 2.115 / 3.789 | 2.036 / 3.660 |
| Engine lead/delay, frames | 6 / 9 | 6 / 9 |
| Missing / late / USB in/out errors | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 |

The small input/write paths improve, but **no reduction of total CPU is
established**; the paired total CPU/cycles result is worse. A separate 4000 µs
comparison of the earlier patch (including the discarded scan-deadline cache)
was 43.316 → 42.604 M cycles/s, 75.020 → 71.638 M instructions/s, but 1.931 →
2.054 CPU s/min. That cache was removed. Do not sell instruction savings as
meeting a CPU-seconds target or compare the harness directly with Yamaha's dext.
The small changes above leave the USB submission and periodic wakeup costs intact.
Profiler totals include engine priming; harness CPU/cycles exclude priming and drain.

### Reliability investigation

The first final-build test at `2,512,0,2,3,1,0,125`, 48k/32, `-N -a 64`:

| 180 s streaming test | Cycles/s | CPU s/min | Lead/delay | Missing / late / USB errors in/out |
|---|---:|---:|---:|---:|
| Otherwise idle | 107.436 M | 1.4872 | 6 / 9 | 0 / 0 / 0 / 0 |
| Ten nice-10 workers | 85.683 M | 1.3761 | 6 / 9 | 16,038 / 5,009,456 / 128 / 360 |

Idle also had zero resyncs, gaps, empties and races (max callback lag 17.07 /
16.34 ms). The load failure followed a 230.80 ms capture callback stall and
140.24 ms playback stall: two 64 ms requests cannot keep transfers scheduled
through that delay. There were 2 resyncs, 9,072 gap frames, 128 empty packets,
and zero write races. All ten workers were terminated in `finally`; a separate
`pgrep -x yes` was empty. The machine also had unrelated app/updater activity;
none of those processes was stopped. This result must not be hidden by earlier
short clean runs, or attributed specifically to the small read/write patch.

The deeper queue was evaluated independently: six 512-microframe requests
provide 384 ms of queued USB transfers, with 320 ms available to resubmit each
completed request. Direct read/write still advertises 6/9-frame lead/delay; the
USB queue is not the streaming safety offset. Completion rate remains 15.625
requests/s per direction. Additional memory, map checks and startup priming are
the costs. Final results and selected configuration follow below.


### Selected build: six requests, unchanged streaming latency

`0259e45` makes `6,512,0,2,3,1,0,125` the fresh-build default. The installed
plug-in remains at the owner's original `2,512,0,2,3,1,0,125` pending reinstall.
The deeper queue addresses the observed callback-stall failure; it does not
claim to solve the CPU target.

| 180 s streaming test, final engine at six requests | Cycles/s | Instructions/s | CPU s/min | Lead/delay | Missing / late / USB in/out errors |
|---|---:|---:|---:|---:|---:|
| No artificial load | 107.345 M | 141.696 M | 1.4958 | 6 / 9 | 0 / 0 / 0 / 0 |
| Ten nice-10 workers | 74.677 M | 161.154 M | 1.2180 | 6 / 9 | 0 / 0 / 0 / 0 |

Both runs also had zero resyncs, gap frames, empty packets, write races and
backwards timestamps. Max callback lag in the loaded run was 13.56 / 13.38 ms.
All ten workers (one per `hw.ncpu`) were verified at nice 10, stopped in
`finally`, and `pgrep -x yes` was verified empty afterward.

The six-request queue's 180 s no-load result is essentially unchanged from the
same final engine at two requests: 107.436 → 107.345 M cycles/s and 1.4872 →
1.4958 CPU s/min. Startup was 276 → 533 ms, peak RSS 10,092,544 → 10,813,440
bytes (about +0.69 MiB), and peak physical footprint 5,472,616 → 6,144,384 bytes.
A clean stop can also drain more queued USB data (up to 384 ms, previously 128).

A separate 22 s HAL test of the **installed old plug-in**, six requests, 48k/32,
with the same ten workers still running, had zero missing/late/errors, resyncs,
gaps, empties, races and input/output sample-time breaks. Two steady `top`
samples showed coreaudiod 1.6% / 1.8%, UR44 host 2.0% / 2.1% (combined about
2.16–2.34 CPU s/min). This is a configuration check, not a post-install CPU
measurement of the new code. The planned 4000 µs loaded comparison was dropped
after the two-request reliability failure; no loaded 4000 µs result is claimed.

Hardware loopback, installed plug-in, 48 kHz / 32 frames, no added load:

| USB queue | App round trip | Clicks | Min / max | HAL safety input/output |
|---|---:|---:|---:|---:|
| 2 × 512 | 108 samples = 2.25 ms | 11 | 108 / 108 samples | 9 / 6 frames |
| 6 × 512 | 108 samples = 2.25 ms | 11 | 108 / 108 samples | 9 / 6 frames |

The internal device component was 29 samples in both measurements. These are
actual loopback measurements, not just reported latency values. LOOPBACK was
then set to 0 and read back as 0. The mixer agent was restarted with
`--background`. Installed timing was restored to `2,512,0,2,3,1,0,125` and read
back. An inactive HAL `top` sample showed 0.0% for both processes. New plug-in
idle CPU and HAL-thread behavior still need the owner to reinstall it.

### Target and handoff

**The ≤0.55 CPU s/min target is not met.** The best clean final-engine 180 s result
observed here was 1.218 s/min under load; no-added-load streaming was 1.496
s/min. These are measured process results including the harness, not a proven
hard lower bound for the driver alone. Most process CPU is system time. USB
submission dominates sampled active completion stacks, and the 8 kHz backup
plus client timer wakeups remain. The tested public submission variants did
not remove that cost. Achieving the target with this backend is not established;
claiming it is fundamentally impossible would require better kernel attribution
and post-install host measurements. A working kernel-shared isochronous-list
allocator or a different USB submission backend remains a possible future path.

Local commits implementing this build:

- `a3edaec` — trim redundant IO work and expose host profiling.
- `0259e45` — absorb USB callback stalls with six queued requests.

`driver/build.sh` builds the universal arm64/x86_64 plug-in and tools; the ad-hoc
bundle signature verifies with `codesign --verify --strict`. The rebuilt default
is also smoke-tested directly with the hardware. Only 48 kHz was validated for
the deeper queue; other supported rates and older macOS versions need checks.
No Rust code changed, so no unrelated Rust rebuild/test was run. Raw measurement
logs are archived in `driver/build/coreaudiod-cpu-evidence.tar.gz`.

Owner commands, with all audio clients stopped (these privileged commands were
**not** run by the agent):

```sh
cd ~/Developer/ur44
sudo ditto driver/build/UR44.driver /Library/Audio/Plug-Ins/HAL/UR44.driver
sudo chown -R root:wheel /Library/Audio/Plug-Ins/HAL/UR44.driver
sudo launchctl kickstart -k system/com.apple.audio.coreaudiod
driver/build/ur_hal_test -r 48000 -c 6,512,0,2,3,1,0,125 -q
```

Confirm lead/delay 6/9, requests 6, scanner period 125, `hostPid` and the new
`prof.*` keys. Persisted timing overrides compiled defaults, so the configuration
command matters. `-q` also prevents `-c` from starting the tool's default 5 s
recording. Then measure the actual new hosted driver:

```sh
driver/build/ur_hal_test -r 48000 -b 32 -s 30 -o /tmp/ur44-cpu-check.wav
driver/build/ur_hal_test -q
```

The HAL test prints the profiling totals after stopping; they remain available
through `-q` until the next stream starts. Compare `zeroCall` / `readCall` /
`writeCall` / `halCycle` counts, scan source counts, `readReady`, retry counts,
and enqueue elapsed time. Inclusive timers overlap; do not sum all sections.
Repeat the three-minute HAL idle/load reliability check after reinstall, because
the new code has only been run in the standalone engine here. To return to the
owner's original timing, stop audio and use:

```sh
driver/build/ur_hal_test -c 2,512,0,2,3,1,0,125 -q
```

## Energy, installed plug-in (2026-11-07, powermetrics via driver/tools/energy.sh)

48 kHz / 32 frames, 60 s each, 6 × 512 requests. powermetrics "energy impact" per process:

| backup scanner | coreaudiod CPU ms/s | coreaudiod energy | UR44 host CPU ms/s | UR44 host energy | total energy |
|---|---:|---:|---:|---:|---:|
| 125 µs | 17.4 | 71.0 | 23.2 | 76.8 | 147.8 |
| 4000 µs | 81.3 | 66.6 | 43.3 | 17.9 | 84.5 |

CPU time is misleading here: without 8000 wakeups/s the work runs on efficiency cores at low
clock, so it takes more CPU time but ~43 % less energy (UR44 host 4.3× less). 4000 µs passed
3 min at 48k/15 with all cores loaded (zero missing/late/errors), so it is the new default
and the applied configuration: 6,512,0,2,3,1,0,4000.
