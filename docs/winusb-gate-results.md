# WinUSB gate (G2) results

Results of `tools/winusb-gate` on Windows 10 LTSC. The UR44 is bound to in-box `winusb.sys` with
`driver/ur44-winusb.inf`: usbccgp on the parent and WinUSB on MI_00..MI_04. The 48 kHz measurement
log is in `tools/winusb-gate/results/gate-log.txt`. This file starts with the crash investigation.
The other result sections are still to be written.

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
