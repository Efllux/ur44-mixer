# WinUSB gate (G2) results

Results of `tools/winusb-gate` on Windows 10 LTSC. The UR44 is bound to in-box `winusb.sys` with
`driver/ur44-winusb.inf`: usbccgp on the parent and WinUSB on MI_00..MI_04. The 48 kHz measurement
log is in `tools/winusb-gate/results/gate-log.txt`. This file starts with the crash investigation.
The other result sections are still to be written.

## Crash on 2026-11-22

### Summary

- **What happened.** At about 10:08 UTC the PC bugchecked with **0xD1
  DRIVER_IRQL_NOT_LESS_OR_EQUAL** and rebooted. The system came back up at 10:09:26 UTC. This was
  the third bugcheck in 11 hours while WinUSB isoch was in use on this machine. The two earlier
  ones were 0x139 at 23:12 and 23:14 UTC on 2026-11-21, during 96 kHz streaming.
- **Faulting module: not determined.** The kernel dump and minidump can only be read by an
  administrator, and the investigation ran without admin rights. The bugcheck parameters show a
  kernel-mode read of an unmapped kernel address at DISPATCH_LEVEL. They do not name a module. The
  commands to finish the analysis are in [Getting the faulting module](#getting-the-faulting-module).
- **What was running.** Only the `--every 64` run of `probe-reanchor` can have started. The 512,
  1024 and 4096 runs never did. The crash happened during the last seconds of `cargo build` or
  within the first seconds of the `--every 64` run. In that run, the first new kind of request,
  an explicit-frame write submitted while ASAP writes are pending, goes out about 8 ms after start.
- **Assessment.** It is very likely, but not proven, that the Windows USB stack is mishandling
  isoch OUT requests that fall outside the documented contract. The probable driver is usbxhci or
  UCX, with winusb as a possible alternative. A user-mode program should never be able to bugcheck
  the kernel, so this is a Windows bug whatever the inputs were. No public KB or fix for it was
  found.

### 1. Bugcheck record

From the System log and the WER report (Report Id `69a11765-a950-44e4-8aad-ead95fe07d84`):

| | |
|---|---|
| Stop code | `0x000000D1` DRIVER_IRQL_NOT_LESS_OR_EQUAL |
| P1, address referenced | `0xffffe978c3e00000` |
| P2, IRQL | `2` (DISPATCH_LEVEL) |
| P3, access | `0` (read) |
| P4, faulting IP | `0xfffff800588722f9` |
| Kernel-Power 41 | 11:09:28 local (10:09:28 UTC), BugcheckCode 209 (0xD1), same P1, not in sleep |
| Dump | `C:\Windows\MEMORY.DMP` (850,508,977 bytes, automatic memory dump) and `C:\Windows\Minidump\112226-6046-01.dmp` |
| WER bucket | `Kernel_d1_47452c5260ee36a38d3797a6fdb34be8b880` (not resolved; the report never reached Microsoft) |

How to read these parameters:

- **P1** is aligned to a page and to 2 MB. A read of the first byte of an unmapped region usually
  means code walked off the end of a buffer, a mapped MDL or a ring that ends exactly at that
  boundary.
- **P2 and P3**: the read happened at DISPATCH_LEVEL. That is where the USB transfer paths run:
  the usbxhci transfer-event DPC, UCX, and the WDF/winusb completion paths.
- **P4** lies in the `fffff800'5xxxxxxx` range, where kernel drivers are loaded. Driver bases are
  randomised on every boot, so the address cannot be matched to a module after the reboot.
  Without the dump it fits usbxhci.sys, ucx01000.sys, winusb.sys, Wdf01000.sys or ntoskrnl, and
  also unrelated drivers.

The other candidate is the network stack. NIC/VPN drivers are a common source of 0xD1. This PC
had network-diagnostics failures shortly before the crash, at 10:52–10:55 local, for `rt640x64.sys`
(Realtek, 2019) and `wintun.sys` 0.14 (installed 2026-11-16). That is why the dump has to be read
before the cause is called certain.

The two earlier crashes, for the record:

| Time (local) | Stop code | Parameters | Running at the time |
|---|---|---|---|
| 2026-11-22 00:12 | 0x139 KERNEL_SECURITY_CHECK_FAILURE | 0, 0, 0, 0xc0050000 | `stream` at 96 kHz, at stream start |
| 2026-11-22 00:14 | 0x139 | same | `stream` at 96 kHz, reproduction |

The 0x139 dumps were overwritten (`OverwriteExistingDebugFile = 1`). Only their WER report folders
remain: `Kernel_139_c9a0a7ac…`, two of them, also readable only by an administrator.

### 2. What the probe was submitting

The command started at 10:07:48.97 UTC:

```
cargo build --release 2>&1 | Select-String "error|Finished"
foreach ($e in 64,512,1024,4096) { winusb-gate.exe probe-reanchor --n 40000 --every $e }
```

**How the crash time was narrowed down.** The command's captured output file is 94 bytes long
and contains only NUL bytes. NTFS had journaled the file size, but the data pages had not yet been
written when the machine died. 94 bytes is exactly the blank line plus the 90-character compiler
warning `140 |     pub dropouts: u64, // stream breaks + …`. Select-String prints that warning
partway through the build, because it matches "error". The later rerun produced it at the same
place, about 10 s before `Finished`. None of the first build's outputs survived either: no
`winusb-gate.exe`, no fingerprint files. So the rerun at 10:10:30 compiled from scratch (10.35 s).
The `main.rs` edit saved at 10:07:47 did survive.

The crash therefore came a few seconds at most after the warning was printed. That puts it in the
last part of the build or the first seconds of the `--every 64` run, roughly 10:07:55–10:08:05
UTC. The 512, 1024 and 4096 runs never started. Nothing else touched the UR44 in that window. The
UR44 Mixer app had been killed at 10:06:24 UTC; it was restarted only by autostart after the
reboot. The last USB actions before the build were `probe-write` at 10:06:07 and an `ur44ctl dump`
at 10:06:33.

**What `probe-reanchor --every 64` submits.** This is the uncommitted `cmd_probe_reanchor` in
`tools/winusb-gate/src/main.rs`, with defaults `--rate 48000 --depth 24`.

- The interface is MI_01 (`GUID_MI01`). The probe sets alt 0, then alt 1 (6 ch × 24 bit), then
  SET_CUR 48000 on EP 0x07 OUT.
- One isoch buffer of `MaximumBytesPerInterval × 24 = 126 × 24 = 3024` bytes is registered. Slot
  `s` starts at offset `s × 126`.
- Every write is **108 bytes**, which is one microframe of audio: 6 frames × 18 bytes. That is one
  packet smaller than wMaxPacketSize (126). Writes go into slot `i % 24`.
- **24 writes are in flight** at all times, which is 3 ms of bus time. A new write `i + 24` is
  submitted only after write `i` has completed. The loop waits up to 1000 ms on the event, then
  calls `WinUsb_GetOverlappedResult(wait = TRUE)`. It keeps resubmitting after errors.
- `f0` is `WinUsb_GetCurrentFrameNumber` + 3 at the start.
- Write `i` is chosen like this:
  - `i % 64 == 0`: an **explicit-frame** `WinUsb_WriteIsochPipe` at frame `f0 + i/8`.
  - Otherwise: `WinUsb_WriteIsochPipeAsap(ContinueStream = TRUE)`.
- So 1 in 64 writes (625 of the 40,000) is explicit-frame. Write 0 is an explicit-frame start on
  an idle pipe. The **first explicit-frame write submitted while ASAP writes are pending** is
  write 64, submitted about 8 ms after start. It goes out when write 40 completes, so writes 41–63
  (23 ASAP transfers) are still pending.

**Why this pattern is new, and plausibly the trigger.** On this stack, an ASAP write of one packet
takes **one microframe** on the bus, but WinUSB books it as **one whole frame**. That is why a
plain ASAP chain fails with error 87 after write 1170: `N − N/8 = 1024` frames of booking lead.
After 64 writes, the pending ASAP transfers are booked up to about frame `f0 + 63`. On the bus,
though, they end around frame `f0 + 7`. The explicit write asks for frame `f0 + 8`. Measured
against the bus it is the next free frame. Measured against the software's own booking it lies
**about 55 frames inside frames already booked by pending transfers on the same endpoint**. That
is a start frame which is in range of the current frame but overlaps pending ASAP work. The
documentation never allows it, and none of the earlier runs ever submitted it. The safe 48 kHz
`stream` runs make one explicit-frame lead write on an idle pipe and then use ASAP only.

The probe also uses 108-byte writes. The documented contract for chained isoch writes is a
multiple of `MaximumBytesPerInterval × 8 / bInterval`, which here is 1008 bytes. So the stack's
frame and microframe bookkeeping was already off-contract before the re-anchor.

### 3. Host controller and port

| | |
|---|---|
| Machine | Shuttle SH87R (FH87 board, BIOS 2.01 of 2014-02-21), Core i5-4460 |
| OS | Windows 10 Enterprise LTSC 2021, build 19044.3324 (21H2) |
| xHCI | Intel 8 Series/C220 (Lynx Point) USB 3.0 xHCI, `PCI\VEN_8086&DEV_8C31&SUBSYS_40161297&REV_05`, PCI 0:20.0 |
| xHCI driver | in-box `USBXHCI` (usbxhci.inf) 10.0.19041.3031 |
| Stack binaries | usbxhci.sys 10.0.19041.3031, ucx01000.sys 10.0.19041.1, winusb.sys 10.0.19041.1, usbccgp.sys 10.0.19041.2546, usbhub3.sys 10.0.19041.3208, Wdf01000.sys 1.31.19041.3155 |
| UR44 port | **Port_#0003.Hub_#0003**, directly on the xHCI root hub (`USB\ROOT_HUB30\4&38a4f90c&0&0`), with no external hub. High speed. |

The board also has two EHCI controllers (8C26 and 8C2D, usbehci/usbport). The UR44 is not on them.

### 4. Current state (11:16 local, after the reboot)

- The UR44 parent `USB\VID_0499&PID_150E\5&164509E6&0&3` is **OK** with problem code **0**,
  service usbccgp, INF oem18.inf.
- MI_00, MI_01, MI_02, MI_03 and MI_04 (`…\6&14024FBC&0&000x`) are all **OK** with problem code
  **0**, service WINUSB, INF oem18.inf.
- The UR44 Mixer (`ur44-mixer.exe`, autostarted 10:10:04 UTC) has the WinUSB interface open. This
  is why the 10:10 rerun of the sweep failed with `CreateFile error 5` before it touched the
  device.
- There are no System errors or criticals since the boot at 10:09:26 UTC. The USB stack shows no
  device problem codes.

### 5. Assessment and how to continue

**Known Windows bug?** No public KB or fix was found for usbxhci or UCX bugchecking on a user-mode
WinUSB isoch pattern. The closest public report is a Microsoft Q&A thread about intermittent
verifier bugchecks (0x10D, 0xC9) in a KMDF isoch streaming driver, with usbxhci on the stack in
`Isoch_Stage_CompleteTD`. Microsoft's documentation describes these rules, which the probe broke
or came close to:

- An ASAP transfer goes in the frame right after the last pending one.
- ContinueStream fails the request if the stack cannot do that.
- An explicit start frame must lie within the ISO start-frame range and should be the current
  adjusted frame + about 5.
- Chained writes should be multiples of `MaximumBytesPerInterval × 8 / interval`.

The best explanation is a **stack bug that this off-contract input triggers**. usbxhci or UCX
appears to be computing ring or buffer positions from an explicit start frame that lies behind
frames already booked by pending ASAP TDs of one microframe each. P1 (a read starting exactly at a
2 MB boundary, at DISPATCH_LEVEL) fits that kind of overrun. This stays an inference until
`!analyze -v` names the module.

**Patterns, by evidence:**

| Pattern | Status |
|---|---|
| Control transfers, `desc`, `ur44ctl` on MI_04 | Safe (many runs) |
| 48 kHz `stream`: one explicit-frame lead write on an idle pipe, then ASAP continue only, IN 8/16-packet ASAP reads | Ran dozens of times without a crash. OUT is useless anyway: every write after #1170 fails with 87. |
| `probe-write` explicit-frame single writes on an idle pipe, 20 ms apart | Ran without a crash |
| One-packet ASAP chain (`probe-chain`) up to and past the error-87 wall | Ran without a crash |
| **Explicit-frame write while ASAP writes are pending on the same pipe** (`probe-reanchor`, any `--every`) | **Never run again.** Most likely cause of the 0xD1. |
| **96/192 kHz `stream`** (`run-gate.ps1 -Set high`) | **Never run again.** Two 0x139 reproductions. |
| Any explicit start frame that lies before or inside the stream's booked frames, or more than about 1000 frames ahead | Never run again (same bug class) |
| Isoch writes that are not a multiple of `MBPI × 8` in a ContinueStream chain | Off-contract. Treat as unsafe in combination with anything other than a plain ASAP chain. |

**Recommendation.** Stop the WinUSB isoch-OUT path for the UR44 on this machine. Treat G2 as
**failed for OUT**.

The reason is structural, not a matter of tuning. The UR44 needs a different number of frames in
each microframe packet: 5/6/7 at 44.1/48 kHz, driven by feedback. WinUSB only gets that packet
sizing by writing one packet per request, and on this stack that breaks the frame booking. Writing
whole-frame multiples of `MBPI × 8` would follow the contract, but WinUSB then packs every packet
full: 7 frames instead of 6. That gives a wrong rate and invalid packets for the device. The only
known workaround (re-anchoring with explicit frames) is what crashed the kernel.

The IN side (48 kHz capture) worked and can still be measured on its own if that data is useful.
Stay at 44.1/48 kHz only, and do not submit any explicit frame after the first one.

If OUT has to be pursued, the safe way to do it:

1. **Get the dump read first.** If the faulting module is not USB (for example wintun or
   rt640x64), the probe is cleared and the question can be reopened.
2. Run any further isoch-OUT test on a **separate machine or a VM with USB passthrough**, never on
   the PC the user works on. If it has to be this PC: save everything first, run one short test
   at a time, and run nothing in the same session as a build.
3. Keep requests within the contract: whole frames (`MBPI × 8 × k` bytes), ASAP with
   ContinueStream, one start per pipe. Measure what the device actually receives before trying
   variable sizing.
4. The real path to variable-size OUT packets is a **kernel-mode driver** (a KMDF USB client driver
   using `URB_FUNCTION_ISOCH_TRANSFER` with per-packet lengths and USBD_START_ISO_TRANSFER_ASAP).
   It needs signing, which was ruled out for this project. The alternatives are CC MODE or
   Yamaha's driver.

### Getting the faulting module

The dumps need admin rights. In an **elevated** PowerShell, after installing WinDbg (for example
`winget install Microsoft.WinDbg`, or the Windows SDK "Debugging Tools for Windows"):

```powershell
# copy the minidump somewhere readable first
Copy-Item C:\Windows\Minidump\112226-6046-01.dmp $env:USERPROFILE\Desktop\
cdb -z $env:USERPROFILE\Desktop\112226-6046-01.dmp -c ".symfix; .reload; !analyze -v; kv; lmv m usbxhci; lmv m ucx01000; lmv m winusb; q"
```

Report `MODULE_NAME`, `IMAGE_NAME`, `FAILURE_BUCKET_ID` and the stack from `kv`. Then update this
section. Copy `MEMORY.DMP` too if the minidump stack is truncated. The next crash will overwrite
it.
