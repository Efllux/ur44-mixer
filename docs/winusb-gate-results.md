# WinUSB gate (G2) results

Results of `tools/winusb-gate` on Windows 10 LTSC. The UR44 is bound to in-box `winusb.sys` with
`driver/ur44-winusb.inf`: usbccgp on the parent and WinUSB on MI_00..MI_04. The 48 kHz measurement
log is in `tools/winusb-gate/results/gate-log.txt`. This file starts with the crash investigation.
**Verdict (section 7): NO-GO for a WinUSB + ASIO driver.** The last OUT option,
`ISO_NUM_FIXED_PACKETS`, does not exist in in-box winusb.sys.
**Verdict (section 8): GO with caveats for playback through VBoxUSB.sys** (Microsoft-catalog-signed,
installed by usbipd-win): 5 min of clean duplex at 48 kHz, no drift, 4.9 ms app round trip at a
4 ms queue.
**Update (section 9):** with late write and per-PC calibration, validated for 300 s through the
digital loopback: **1.78 ms at 48 kHz, 1.38 ms at 96 kHz, 1.57 ms at 192 kHz** (macOS: 1.55–1.71
ms). Tool and usage: [`tools/vboxusb-gate/README.md`](../tools/vboxusb-gate/README.md).

## Crashes on 2026-11-21/22 (dump analysis)

### Summary

- **Three real bugchecks, one bug.** The 0x139 at 23:11 UTC, the 0x139 at 23:13 UTC on
  2026-11-21 and the 0xD1 at 10:07:54 UTC on 2026-11-22 each have a WER 1001 event with a dump, a
  Kernel-Power 41 carrying the same stop code, and their own minidump. All three have **the same
  stack**:
  `USBXHCI!Isoch_CompleteTransfers` → `WinUSB!WinUSB_IsochTransferComplete+0x18e` →
  `WdfObjectDelete(URB)` → `ucx01000!Xrb_Free` → `Wdf01000!FxObject::ProcessDestroy`, which makes
  an indirect call through the corrupted pointer `0xC005000000000000`.
- **The user's power-off that night is a separate event.** It is the clean `User32 1074` "power
  off" at 23:56:23 UTC. Fast Startup turned it into a hybrid shutdown (hibernate), and the PC
  resumed at 09:46:51 UTC. That is why the 0xD1 dump shows 10:53 h of uptime, counted from the boot
  after the second 0x139, and why the 6008 event at 10:09 cites a stale "00:54:16" time.
- **What corrupts memory.** In-box Microsoft USB isoch code writes per-packet status
  `0xC0050000` = **USBD_STATUS_ISO_NOT_ACCESSED_LATE** for about 7 packets into a URB that has
  **one** packet. That overruns the URB allocation. The faulting module is in-box Microsoft code,
  not a third-party driver; rt640x64 and wintun are not on any stack.
- **Trigger.** A *late* isoch transfer that is **smaller than one frame** (fewer than 8 packets on
  this bInterval = 1 endpoint). The explicit-start-frame re-anchor is one way to make such a
  transfer late, but it is not required: both 0x139s happened in `stream` at 96 kHz, which never
  re-anchors.

### 1. Bugcheck records

| | 0x139 #1 | 0x139 #2 | 0xD1 |
|---|---|---|---|
| Crash time (dump) | 2026-11-21 23:11:24.95 UTC | 23:13:41.92 UTC | 2026-11-22 10:07:54.80 UTC |
| WER 1001 + dump | yes, report c2d8ff1c… | yes, report a52c23e9… | yes, report 69a11765… |
| Kernel-Power 41 | 23:12:16, BugcheckCode 313 | 23:14:12, 313 | 10:09:28, 209 |
| Minidump | `112226-4718-01.dmp` | `112226-5093-01.dmp` | `112226-6046-01.dmp` + `MEMORY.DMP` |
| Parameters | 0, 0, 0, **0xc0050000** | same | 0xffffe978c3e00000, 2, 0, 0xfffff800588722f9 |
| Bucket | `0x139_0_LEGACY_GS_VIOLATION_WinUSB!WinUSB_IsochTransferComplete` | same | `AV_WinUSB!WinUSB_IsochTransferComplete` |
| Current process | winusb-gate.exe | winusb-gate.exe | System (idle-loop DPC); winusb-gate.exe alive |
| Running | `stream` 96 kHz, at stream start | `stream` 96 kHz, reproduction, 87 s after boot | `probe-reanchor --every 64`, **134 ms** after the process started |

The 0x139 entries are real. The "stack-based buffer overrun" label is misleading. The bugcheck is
raised by `nt!guard_icall_bugcheck`, the kernel's CFG check on indirect calls, called from
`FxObject::ProcessDestroy`. The call target failed validation, and P4 carries its upper half
(0xc0050000). In the 0xD1, the same CFG/retpoline check at `fffff800'588722f9`
(`bt qword ptr [r11],r10`, using `nt!retpoline_image_bitmap`) faulted first, because
`target >> 16`, taken as an offset into the bitmap, landed on an unmapped page. So the outcome
differs only with that boot's layout.

For the 0xD1, the dump settles the timing question from the previous version of this file:
`cargo.exe` had already exited, the `--every 64` run of `winusb-gate.exe` was 134 ms old, and the
UR44 Mixer process was dead. The crash happened in the probe, not in the build.

Stack binaries: usbxhci.sys 10.0.19041.3031, ucx01000.sys 10.0.19041.1, **winusb.sys
10.0.19041.1030**, Wdf01000.sys 1.31.19041.3155, usbccgp.sys 10.0.19041.2546, on an Intel 8
Series/C220 xHCI (8C31). The UR44 is on a root port with no external hub.

### 2. The corrupted URB (from MEMORY.DMP)

The object being destroyed is a 0x1D0-byte nonpaged `WINU` pool block at `ffffbc09'34215100`. It
holds a WDF memory object (`…15120`) and, at `…151f0`, the URB:

- `Length 0x00a4, Function 0x000a` (URB_FUNCTION_ISOCH_TRANSFER) with `TransferBufferLength 0x6c`
  = 108 bytes. That is **NumberOfPackets = 1**, one of the probe's one-packet writes.
- The URB ends at `…15294`. From `…15290` onwards, the dword `c0050000` appears **every 12 bytes**,
  which is the stride of USBD_ISO_PACKET_DESCRIPTOR: at `…15290`, `…1529c`, `…152a8`, `…152b4`,
  `…152c0`, `…152cc` and `…152d8`. Packet 0's status is ISO_NOT_ACCESSED_LATE. Six more status
  writes land past the end of the URB. The last one reaches the header of the next pool block.
- Read as a qword, `…15298`, `…152b0` and `…152c8` are each `c0050000'00000000`. That is exactly
  the call target in `rax` at the fault. Memory after the URB that WDF/UCX later use as a function
  pointer was overwritten by packet statuses.

The pattern is "one status per microframe of a whole frame". It fits what the probes already
measured: the stack books a one-packet ASAP transfer as one whole frame. When that frame is late
(not accessed), the isoch completion fills in status for all 8 microframes, not just
NumberOfPackets. The status is written in the usbxhci completion path (`Isoch_CompleteTransfers`
runs just before `WdfRequestComplete`). WinUSB only shows up in the bucket because it frees the
damaged URB. That is an inference from the memory layout: the write itself happens earlier and is
not on the stack. But the module set is certain: Microsoft in-box usbxhci/ucx/winusb/Wdf. No
third-party code is involved.

### 3. Does this confirm the explicit-frame theory?

**Partly.** The theory had the mechanism right: frame-versus-microframe booking breaks down for
sub-frame transfers. The specific trigger was wrong.

- What is required: **a sub-frame isoch transfer** (here 1 packet) **that completes with
  late/not-accessed packets**.
- An explicit start frame inside already-booked frames is a reliable way to get there. The
  re-anchor collides with the pending one-packet ASAP chain, so packets go late. The 0xD1 run
  died 134 ms in.
- Without any re-anchor, the 96 kHz `stream` start produced the same crash twice. It uses sub-frame
  OUT writes (`out_pk` packets each) behind an 8-packet explicit-frame lead-in, and at 96 kHz the
  first transfers ran late.
- The 48 kHz `stream` runs and `probe-chain` survived because their sub-frame writes were either on
  time or **rejected at submission** (error 87 after write 1170). None of them completed late. That
  was luck, not safety.

### 4. Patterns, by evidence

| Pattern | Status |
|---|---|
| Control transfers, `desc`, `ur44ctl` on MI_04 | Safe |
| Isoch transfers of **whole frames** (packet count a multiple of 8, i.e. length a multiple of `MBPI × 8`), ASAP + ContinueStream, one explicit or ASAP start per pipe | Not affected by this bug: the status array covers every microframe the stack can write to. Ran without a crash; still keep the transfer count small and test off the main PC. |
| IN reads of 8/16 packets (48 kHz capture) | Whole frames, so the same as the row above. Ran dozens of times. |
| **Any isoch transfer of fewer than 8 packets** (one-packet OUT writes, `out_pk` < 8, odd-length writes) | **Unsafe.** It crashes the kernel as soon as one of them completes late. Late can come from a re-anchor, a slow start (96 kHz), CPU or DPC load, or a missed deadline. |
| Explicit start frame while transfers are pending on the same pipe | Unsafe in combination with sub-frame transfers (crash #3). With whole-frame transfers it is off-contract and untested. Don't do it. |
| 96/192 kHz `stream` as written | Unsafe (crashes #1 and #2), because it uses sub-frame writes. |

### 5. Is there still a credible WinUSB path for OUT?

The UR44 needs 5/6/7 audio frames per microframe packet at 44.1/48 kHz, steered by feedback.
WinUSB OUT writes have no per-packet length array. With whole-frame writes, WinUSB slices the
buffer into full `MaximumBytesPerInterval` packets, which gives 7 frames each: the wrong rate and
invalid packets. One packet per write is the only known way to size packets, and that is exactly
the pattern that corrupts kernel memory.

The one remaining lever is the **`ISO_NUM_FIXED_PACKETS` pipe policy** (0x22). The tool can
already set it (`--fixed-packets`), but no result was ever recorded. If it is set to 8, a
whole-frame ASAP write of `L` bytes might be split into 8 packets of about `L/8`.

- **48 kHz**: 8 × 6 frames = 864 bytes per frame works only if WinUSB divides evenly. Feedback
  correction (47 or 49 frames in a frame) is not divisible by 8, so it depends on how WinUSB places
  the remainder. If the remainder can't be placed on a whole-frame boundary (18 bytes), the device
  gets split audio frames.
- **44.1 kHz** (an average of 5.5125 frames per packet) needs uneven packets in every frame, so it
  has the same dependency.

This is the only credible WinUSB OUT path left, and it is weak. It is worth exactly one
measurement: what packet lengths reach the device for `L` = 864, 846 and 882 with
`ISO_NUM_FIXED_PACKETS = 8`. Do it on a **separate machine or a VM with USB passthrough**, with
whole-frame ASAP writes only, never on the PC the user works on. If WinUSB doesn't split on
18-byte audio-frame boundaries with the remainder placed usefully, G2 is **failed for OUT**. The
real path is then a KMDF client driver (per-packet lengths in `URB_FUNCTION_ISOCH_TRANSFER`), which
needs signing, or CC mode / Yamaha's driver.

IN (capture) with whole-frame reads stays usable for measurements.

**Result (section 7):** winusb.sys rejects the policy (error 87), so G2 is failed for OUT.

## 7. ISO_NUM_FIXED_PACKETS and the device clock (2026-11-22, on the main PC)

The user explicitly asked for this measurement on the main PC (not a separate machine). Safety
measures in the tool for this and every later run:

- **Guard in `winusb.rs`** (`guard_packets`, `guard_explicit`): every IN read and OUT write is
  checked *before submission*. The packet count must be a non-zero multiple of 8. For OUT it is
  computed as WinUSB builds it, `ceil(len / MaximumBytesPerInterval)`, not taken from any policy.
  An IN read must be exactly `packets × MBPI`. An explicit-frame read or write is only allowed as
  the first transfer on its buffer, never with transfers pending. A violation exits the process
  (code 3) without submitting. Consequence: `stream`, `probe-write`, `probe-chain` with sub-frame
  sizes and `probe-reanchor` now all stop at their first unsafe transfer. `stream`'s 864-byte
  OUT lead-in is 7 packets.
- New commands: `policy` (Get/SetPipePolicy only, no transfers) and `capture` (IN only, 8-packet
  ASAP reads, ≥ 8 ms in flight, stops at the first non-zero packet status or transfer error).
- UR44 Mixer must be closed for MI_01/MI_02. CreateFile fails with error 5 while it runs. It was
  restarted afterwards.

### Test 1: does WinUSB split whole-frame writes into 8 packets? No, the policy does not exist.

`winusb-gate policy` on winusb.sys 10.0.19041.1030 (48 kHz alt 1, both pipes MBPI 126, Interval 1):

| Policy | Get | Set 8 (1 / 2 / 4 byte value) |
|---|---|---|
| ISO_START_LATENCY 0x20 | error 87 | 87 / 87 / 87 |
| ISO_ALWAYS_START_ASAP 0x21 | error 87 | 87 / 87 / 87 |
| **ISO_NUM_FIXED_PACKETS 0x22** | **error 87** | **87 / 87 / 87** |
| MAXIMUM_TRANSFER_SIZE 0x08 (control) | 0x1FFFF8 | — |

0x20–0x22 are **libusbK** ISO pipe policies (libusbK `lusbk_shared.h`, "libusbK ISO pipe policy
types"). Microsoft's WinUSB pipe-policy list does not contain them, and in-box winusb.sys rejects
them. `--fixed-packets` in `stream` was therefore never in effect: the engine only printed the error.

So WinUSB still slices an OUT write into full 126-byte packets. 846, 864 and 882 bytes each become
**7 packets** (6 × 126 + 90/108/126). That is a sub-frame URB, the pattern that crashed this PC, so
the guard refuses it and nothing was submitted. No whole-frame OUT write can carry 48 kHz audio:
8 packets need 883–1008 bytes = 49.1–56 frames per ms, front-loaded at 7 frames per packet. USB ETW
tracing of the on-wire packet sizes was not needed, because there is no safe transfer whose split
could be traced.

Tests 2 (audio quality through MAIN → input 1) and 3 (round-trip latency) need a working OUT
stream and **could not be run safely**. No OUT isoch transfer was submitted in this session.

### Test 2 (partial): device clock, from capture only

`winusb-gate capture` (8-packet ASAP reads, 16 in flight = 16 ms), run as 2 s, 10 s, 60 s and 5 min.
The System event log was checked between runs: 0 errors or warnings apart from the usual DCOM/DNS.

| Run | Packets | Sizes in frames (5 / 6 / 7) | Frames per ms | vs USB frame clock | Stop |
|---|---|---|---|---|---|
| 2 s | 15 968 | 7 / 15 959 / 2 | 47.9975 | −52 ppm | none |
| 10 s | 79 968 | 32 / 79 928 / 8 | 47.9976 | −50 ppm | none |
| 60 s | 479 968 | 192 / 479 728 / 48 | 47.9976 | −50 ppm | none |
| 5 min | 2 399 976 | 1 018 / 2 398 659 / 299 | 47.9976 | −49.9 ppm | none |

- Every IN packet had status 0. There were no late or not-accessed packets and no empty packets
  after start, so whole-frame capture was clean for 5 min. The earlier 5-min `stream` runs saw ISO_NA
  and LATE statuses on IN, which came from the concurrent sub-frame OUT traffic.
- The UR44 clock runs **50 ppm slow** against the host's USB frame clock. It drops one frame every
  ~0.42 s, mostly as a single 5-frame packet; 299 of the 5-frame packets are paired with a 7.
  A host feeding a constant 48 frames per ms would gain one frame every **0.42 s** (about 2.4 per
  second, 719 in 5 min). Without feedback the device's buffer would overflow and slip or click
  on that scale. OUT sizes must follow the IN counts (implicit feedback), which need 5- and 6-frame
  packets inside one 1 ms write. WinUSB cannot express that: it would need per-packet lengths, which
  ISO_NUM_FIXED_PACKETS would not have given even if it existed (equal split only).

### Test 3: round trip

Not measured, because it needs OUT. For reference, the macOS driver does 1.55–1.71 ms. Even if OUT
worked, the safety requirement of ≥ 8 ms of transfers in flight (so that nothing completes late)
would put a WinUSB OUT path far above that.

### Verdict: NO-GO for WinUSB + ASIO

- OUT cannot be sized per packet: no per-packet length array, and the only policy that might have
  helped (0x22) is not implemented by winusb.sys (error 87).
- Every way to get 5/6/7-frame packets through WinUSB uses sub-frame URBs, which bugcheck Windows 10
  21H2 when one completes late (3 crashes).
- IN works: whole-frame capture is clean for 5 min, and the device clock is −50 ppm against the
  USB frame clock.
- Next options: a KMDF client driver that builds `URB_FUNCTION_ISOCH_TRANSFER` with its own packet
  offsets (needs signing, or test signing on a separate machine), or CC mode / Yamaha's driver.

### 6. How the dumps were analysed

Elevated, a one-off script copied `C:\Windows\MEMORY.DMP` and `C:\Windows\Minidump\*.dmp` to a
user folder and added the Debugging Tools from the installed SDK 10.0.26100
(`winsdksetup /features OptionId.WindowsDesktopDebuggers`). Then, unelevated:

```
cdb -z <dump> -y srv*C:\symbols*https://msdl.microsoft.com/download/symbols ^
    -c "!analyze -v; lmv m usb*; lmv m ucx*; lmv m winusb; lmv m wdf*; kb; q"
```

The URB evidence came from `dq ffffbc0934215100 L74`, `!pool` and `!process 0 0 winusb-gate.exe`
on MEMORY.DMP. The copies were deleted afterwards because they contain kernel memory. The originals
stay in `C:\Windows`. The next crash will overwrite `MEMORY.DMP` (`OverwriteExistingDebugFile`); the
minidumps are kept.

## 8. VBoxUSB gate (2026-11-22, on the main PC)

Tool: `tools/vboxusb-gate` (MIT). It talks to VBoxUSB.sys over its IOCTLs only. The struct
definitions are our own, written from the documented byte layout, and no GPL code is copied. Raw
output of every run is in `tools/vboxusb-gate/results/gate-log.txt`.

### Platform and install

- Secure Boot **off** (`UEFISecureBootEnabled=0`). VBS and Memory Integrity (HVCI) **off**:
  Win32_DeviceGuard shows nothing configured or running.
- One elevated run (`scripts/setup-elevated.ps1`, one UAC prompt) did the whole admin part:
  - installed **usbipd-win 5.3.0** (latest release; MSI Authenticode-valid, signed by the
    author through Certum)
  - ran `usbipd bind --force --hardware-id 0499:150e`
  - stopped the usbipd service
  - registered an on-demand restore task that runs with highest privileges as the user, so the
    unbind needed no second prompt; the task deletes itself afterwards
- Drivers in usbipd-win 5.3.0:
  - **VBoxUSB.sys 7.2.2.170484** (VirtualBox 7.2.2, not 7.2.22; 7.2.22 is only on usbipd-win
    `master`). Its catalog `VBoxUSB.cat` is signed by **Microsoft Windows Hardware Compatibility
    Publisher**.
  - VBoxUSBMon.sys (same version) carries an **Oracle** signature. It is not needed for this
    path. With Secure Boot on, whether it loads is untested here.
- Event log: one `VBoxUSBMon` event 4 ("internal error in its data structures") at the moment
  `usbipd list`/`bind` ran, before any streaming. It is harmless here, but noted.

### Capture and open

- `bind --force` installs VBoxUSB as the function driver of the **whole device**. It replaces
  usbccgp, so MI_00..MI_04 disappear and the UR44 Mixer app loses interface 4 while the device is
  bound. The app was closed for the test and restarted afterwards.
- VBoxUSBMon is not involved in a forced bind. The usbipd service only opens the device on
  `attach`, so it never held it. It was stopped anyway, which is not required.
- **A non-admin process can open the device.** The steps are:
  1. Enumerate device interface `{00873fdf-cafe-80ee-aa5e-00c04fb1720b}` and pick the
     `vid_0499&pid_150e` path.
  2. `CreateFile` with read/write and overlapped I/O.
  3. `GET_VERSION` (5.0), `GET_DEVICE`, `CLAIM_DEVICE`.
  4. `SET_CONFIG(1)`. VBoxUSB leaves the device unconfigured until it is told otherwise.
  5. Select alt 0, then alt 1, on interface 2 and then interface 1.
  6. UAC1 SET_CUR 48000 on 0x86 and 0x07, sent as control-message URBs. GET_CUR read back
     48000 on both.

  `GET_DEVICE` is mandatory: the driver only learns there that the device is high speed. Without
  it, the scheduler advances 8 frames per URB instead of 1.
- The claim is exclusive per file object, so one process owns the device and every interface.

### Driver behaviour (verified in the source of the shipped version)

- Isochronous `SEND_URB` uses `aIsoPkts[i].cb` and `.off` per packet, so **per-packet lengths
  work**. The limit is 8 packets per IOCTL, and a release build does not check it; the tool
  refuses anything but 8.
- The driver sets the StartFrame itself to `max(current frame + 2, pipe's next frame)` and
  advances it by one frame per URB on high-speed devices. If the queue runs dry it re-anchors
  silently: there is no error, just a gap. The tool detects this before it happens by counting
  queued URBs whose IOCTL has not completed yet. If fewer than 4 remain, the run stops.
- Per-packet status mapping: SUCCESS → 0, NOT_ACCESSED → 4, anything else (including
  ISO_NOT_ACCESSED_LATE) → 1. The tool stops on any non-zero status.
- Every transfer is a whole frame (8 microframes), so the sub-frame URB pattern behind the three
  WinUSB bugchecks (section 1) cannot occur.

### Runs

Safety rules were enforced in code throughout:
- exactly 8 packets per IN and OUT URB
- 48 kHz only
- at least 4 ms still queued after each resubmission
- stop at the first error or non-OK packet
- stop with a clean drain, never an abort

OUT packet i always carries the frame count of an IN packet, taken in order through a FIFO; an
empty IN packet would have mapped to 6 frames. Playback starts after 100 ms of stable capture. The
signal is a 1 kHz sine on outputs 1/2, recorded on input 1 over the MAIN → input 1 cable. The event
log was checked after every run and showed 0 errors and 0 crash markers each time.

| Run | Queue IN/OUT | IN / OUT URBs | IN frames per ms (ppm) | Min queued IN/OUT | Packet errors | Glitches | Phase span | Result |
|---|---|---|---|---|---|---|---|---|
| sine 2 s (gain too low, −47 dBFS) | 8/8 | 2 006 / 1 904 | 47.9975 (−52) | 8 / 8 | 0 | n/a (noise) | 0.0015 samples | ok |
| sine 2 s after gain fix (−34 dBFS) | 8/8 | 2 006 / 1 904 | 47.9975 (−52) | 8 / 8 | 0 | 0 | 0.0006 samples | ok |
| sine 10 s, amp 0.8 (−23.9 dBFS) | 8/8 | 10 006 / 9 904 | 47.9976 (−50) | 8 / 8 | 0 | 0 | 0.0002 samples | ok |
| sine 60 s | 8/8 | 60 006 / 59 904 | 47.9976 (−50) | 8 / 8 | 0 | 0 | 0.0010 samples | ok |
| **sine 5 min** | 8/8 | 300 007 / 299 905 | 47.9976 (−50.4) | 7 / 7 | 0 | **0** | **0.0014 samples** | ok |
| sine 60 s, minimum queue | 4/4 | 60 002 / 59 900 | 47.9976 (−50) | 4 / 4 | 0 | 0 | 0.0004 samples | ok |

- **Sine/drift verdict: clean.** In 5 min the glitch detector found nothing; the largest prediction
  error was 0.8 % of the tone, against a 2 % threshold. The 1 kHz phase, measured against the
  capture sample index in 2 988 blocks of 100 ms, spanned 0.0014 samples with no step over 2°.
  One lost or repeated sample anywhere in the loop would move it by 1 sample (7.5°). The amplitude
  stayed within 0.3 %.
- The IN packet mix over 5 min was 1 040 × 5, 2 398 702 × 6 and 314 × 7 frames, with no empty
  packets: the device is 50 ppm slow, as in section 7. Mirroring the IN sizes kept OUT exactly
  locked to it.
- The largest gap between two wakeups of the streaming thread was 1.68 ms, with the process at high
  priority and the thread time-critical.

### Latency (click on outputs 1/2, MAIN → input 1, 39 clicks per run, none missed)

| Queue IN/OUT | App round trip (OUT submit → IN completion), min / median / max | Wire round trip (OUT microframe → IN microframe) |
|---|---|---|
| 8/8 | 8.856 / 8.946 / 9.053 ms | 1.417 ms (68 samples) |
| 8/6 | 6.783 / 6.926 / 7.060 ms | 1.417 ms |
| 4/4 | **4.811 / 4.931 / 5.072 ms** | 1.417 ms |

- The **wire round trip is a constant 1.417 ms** (±0.02 ms) from the USB microframe that carries the
  click to the microframe that brings it back. It is the analog path: DAC, cable, ADC and the
  device's buffers. The macOS internal digital loopback is 29 samples, against our 68 through
  analog.
- The app round trip is the time from writing the click into an OUT URB to receiving the IN URB
  that contains it. In practice that is the OUT queue depth + 1.42 ms + about 0.5 ms. At the
  minimum queue the safety rules allow (4 ms) it is **4.9 ms**. That is about Yamaha's 4.4–4.5 ms
  (macOS DriverKit) and 3× the macOS driver's 1.55–1.71 ms. Closing the gap needs a queue of 1–2 ms
  per pipe, which is below this test's safety floor and untested. The driver re-anchors silently
  if the queue runs dry, so a smaller queue needs a real-time thread and a re-anchor-aware design.
- The OUT pipe's start frame was 102 ms after the IN pipe's: 100 ms of stable capture, then a 2 ms
  lead. This was derived from the median completion-time difference and was the same in every run.

### Restore

- `usbipd unbind` ran from the on-demand task, with no prompt. Windows reinstalled `oem18.inf`:
  usbccgp on the parent and WinUSB on MI_00..MI_04.
- The usbipd service was started again. The tasks and `C:\ProgramData\ur44-vboxgate` were removed.
- `ur44ctl version` → BOOT/MAIN V2.12, and the UR44 Mixer app was restarted.
- usbipd-win stays installed.

### Verdict: GO with caveats for a playback driver on VBoxUSB

Go:
- It needs no driver signing of our own: VBoxUSB.sys is Microsoft-catalog-signed and talked to
  over documented IOCTLs.
- Per-packet OUT sizing works, so implicit feedback is exact.
- Duplex at 48 kHz ran 5 min without a single error, late packet, glitch or drift.
- A non-admin process works.

Caveats:
1. **Exclusive whole-device ownership.** While bound, WinUSB/MI_04 is gone, so the mixer must go
   through the same owner: one process or service that owns the device and runs both the audio
   streams and the DSP mixer control (interface 4 control/bulk through `SEND_URB`). An ASIO DLL
   inside a DAW cannot be that owner if the mixer app has to run at the same time.
2. **Latency floor at the tested safety level: 4.9 ms app round trip.** Getting near macOS needs a
   1–2 ms queue, which is untested, and the driver re-anchors silently on underrun. That needs its
   own gated test series.
3. **Distribution.** The device must be bound with `usbipd bind --force`, which needs admin once.
   We depend on usbipd-win's driver package, or ship the unmodified Oracle binaries ourselves: GPL,
   so with a source offer. usbipd-win 5.3.0 ships VirtualBox 7.2.2. The behaviour must be
   re-checked when that driver changes.
4. Only tested with Secure Boot and HVCI off, at 48 kHz, on the main PC. 44.1/96/192 kHz, Secure
   Boot on and CPU cost (2 000 IOCTLs/s) were not measured.

## 9. Lower latency, higher rates and per-PC calibration (2026-11-22, evening)

The owner approved going below the 4 ms queue rule. The 8-packets-per-URB rule and stop-on-error
stayed in force. All runs below are in `tools/vboxusb-gate/results/gate-log.txt`. No crash, and no
System event-log error, in any run.

### Late write: the queue no longer sets the latency

VBoxUSB passes our locked user buffer to the USB stack without copying it. So an OUT URB can be
submitted with its final packet sizes (from the IN FIFO) and silent data, and the audio written
into it later. New options: `--write-lead W` writes into the URB at queue position W after each
OUT completion, and `--write-delay-us D` waits D µs before that write. The queue stays at 4 ms
(depth floor 3: VBoxUSB schedules at `max(now+2, next)`, so fewer than 3 queued always
re-anchors).

48 kHz, MAIN → input 1 over the analog cable, 4 ms queue:

| Write lead | App round trip median (min) | Write → URB completion, min |
|---|---|---|
| at submit (lead 3) | 4.89 ms | 3.0 ms |
| lead 2 | 3.93 ms | 2.7 ms |
| lead 1 | 2.90 ms (2.68) | 1.6 ms |
| lead 1 + 300 µs | 2.66 ms | 1.5 ms |
| lead 1 + 700 µs | 2.28 ms | 1.16 ms |
| lead 1 + 950 µs | 2.04 ms (1.98) | 0.97 ms |

- The xHCI reads each packet's data only shortly before its microframe. Writes landing up to
  about 0.95 ms before the URB completes still go out. Writes later than that went out as
  silence, which the sine detector sees as a dropout. The wire round trip stayed 1.417 ms
  throughout, so the clicks were never delayed, only lost.
- 5-min sine runs: lead 1 with no delay was **clean** (0 dropouts, writes 1.84–1.98 ms ahead,
  largest wakeup gap 1.12 ms). Lead 1 + 700 µs had 1 dropout, where a write landed 0.94 ms ahead.
  Lead 1 + 300 µs had 2, caused by a 1.87 ms wakeup hiccup.
- A tool bug was found and fixed. The per-URB timestamp vectors doubled at 2¹⁸ entries (262 s),
  and the 2 MB copy stalled the thread for about 1 ms. That gave a dropout at exactly the same
  sample in two runs. The vectors are now preallocated.
- MMCSS "Pro Audio" at critical priority made things worse: 41 dropouts and a 2.87 ms gap. MMCSS
  demotes a thread that spins (SystemResponsiveness). The spin-wait is a test device only; a real
  driver would wait on a timer.

### Higher sample rates (analog cable)

`--rate` selects alt 1 (44.1/48 kHz, 126-byte packets), alt 2 (88.2/96 kHz, 234 bytes, up to
13 frames) or alt 3 (176.4/192 kHz, 450 bytes, up to 25 frames). All read back correctly.

| Rate | Runs | Result | App round trip (late write, lead 1) | Wire |
|---|---|---|---|---|
| 96 kHz | 2 s / 10 s / 60 s sine, 8/8 queue | 0 errors, 0 dropouts, phase span 0.0004 samples, −51 ppm | **1.93 ms** (min 1.82) | 0.969 ms |
| 192 kHz | 2 s / 10 s / 60 s sine (1 kHz and 4 kHz), 8/8 queue | 0 errors, no phase jumps, −51 ppm | **1.90 ms** (min 1.78) | 0.693 ms |

- Right after a rate change the UR44 sends about 0.8 s of empty or short packets. Playback starts
  only after 100 ms of stable capture, so this does no harm.
- At 192 kHz over the analog cable, the sine detector's "glitches" were wideband ADC noise:
  worst error 3 % of the tone, against about 90 % for a real dropout, with constant phase and
  amplitude. The threshold is now 5 % and the test tone scales with the rate.

### Calibration mode (`vboxusb-gate calibrate`)

Every PC has a different floor, set by the USB controller's fetch point and the scheduling jitter.
`calibrate` finds the floor per rate on the machine it runs on:

1. It switches the UR44's internal **LOOPBACK** on (DSP parameter 100 `PatchLoopbackMode`). It
   goes through interface 4 bulk 0x03/0x84 over the VBoxUSB handle (`vboxusb-gate loopback
   on|off`), because the mixer app can't reach it while the device is captured. It then finds the
   returning capture channel (1/2 at −5 dBFS, very clean). No cable is needed.
2. Per rate it walks a ladder from safe to aggressive:
   - 8 ms at submit
   - 4 ms at submit
   - lead 2
   - lead 1
   - lead 1 + 200/400/600/800 µs

   Each step is screened with a sine (default 20 s), and the walk stops at the first error,
   dropout or phase jump.
3. It takes the last passing step minus `--margin` (default 1) and validates it (default 300 s,
   `--load` for all cores busy). If validation fails it backs off a step and validates again. A
   click run then measures the round trip.
4. LOOPBACK is restored and verified, also after Ctrl+C (console handler, clean drain). The
   result goes to `%LOCALAPPDATA%\ur44-vboxgate\calibration.json`.

"Stable" means zero errors, zero non-OK packets, no near-dry queue, and zero dropouts, slips and
phase jumps for the validation time. That is evidence for the tested time and load, not a proof.

Result on this PC (20 s screening, 300 s validation, margin 1, no added load; through the digital
loopback, so the converters are not included):

| Rate | Chosen setting | Round trip median / max | USB wire (DSP loopback) | Notes |
|---|---|---|---|---|
| 48 kHz | 4 ms queue, lead 1 + 200 µs | **1.78 / 1.79 ms** | 0.604 ms | screening passed up to +600 µs; +400 µs failed its 5-min validation (2 dropouts), so it backed off |
| 96 kHz | 4 ms queue, lead 1 + 600 µs | **1.38 / 1.39 ms** | 0.552 ms | all steps passed screening; validated one step below the top |
| 192 kHz | 4 ms queue, lead 1 + 400 µs | **1.57 / 1.60 ms** | 0.526 ms | re-run 2026-11-23 with re-checks (below); +600 µs failed its 5-min validation (2 dropouts), so it backed off |

- On the same digital-loopback method, macOS reaches 1.55–1.71 ms (section 7 reference). This PC
  is at **1.38 ms at 96 kHz, 1.57 ms at 192 kHz and 1.78 ms at 48 kHz**, in software only, with
  no driver of our own.
- 192 kHz, first run: the lead-2 step failed screening with 3 dropouts, and the old walk stopped
  at the first failure, so it settled on the 8 ms queue (7.86 / 8.01 ms). The calibration now
  re-checks a failed step once and stops only at a step that fails twice. In the re-run, steps 3,
  5 and 7 each failed once and passed on re-check. The first step-3 failure was a wake gap that let
  the OUT queue fall to 1 ms, not a late-write fault. The 300 s validation of step 6 failed, and
  step 5 then passed. So 192 kHz on this PC has sporadic one-off hiccups at every lead, not a sharp
  limit. The validated setting is less proven than the 48/96 kHz ones; a longer `--validate-secs`
  or `--load` run would add confidence.
- The macOS counterpart is `driver/tools/calibrate.sh`. It switches LOOPBACK with `ur44ctl`,
  sweeps the plug-in timing (`ur_hal_test -q -c`: margins, guard, scan period) and the client
  buffer, judges each step by the plug-in's error counters plus the client's sample time breaks,
  validates, and measures with `tools/roundtrip`. It is **untested**: it was written on Windows
  without a Mac. A review against `ur_hal_test.c` and the engine fixed two bugs before any run:
  `-c` without `-q` also recorded for 5 s, and the error count could miss errors because the
  engine resets its counters at every start (the script now reads the run's own statistics and
  detects a reset from `inputFrames`).

### State left on this PC

At the owner's request the UR44 stays bound to VBoxUSB, so the UR44 Mixer app and `ur44ctl`
cannot reach it through WinUSB. To go back, run `schtasks /run /tn UR44-VBoxGate-Restore`. That
unbinds the device, restores WinUSB on `oem18.inf`, and removes the task, with no admin prompt.
The usbipd service is stopped and starts again at boot; it does not touch a forced-bound device
unless something attaches it.
