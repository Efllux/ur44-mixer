# UR44 Mixer

A modern replacement for Steinberg's **dspMixFx UR44**, written in Rust. It controls the
UR44's built-in DSP mixer and effects **without any Yamaha/Steinberg driver**: the UR44 runs
in class-compliant (CC) mode on Apple's own driver, so the Mac stays on Full Security.

## Use

1. Set **CC MODE** on the back of the UR44 *before* switching the UR44 on.
2. Open **UR44 Mixer** from `~/Applications`.

UR44 Mixer runs in the background (like a driver's helper): it starts at login without a window,
keeps the connection to the UR44, and restores your last mixer settings when the UR44 is switched on.
Closing the window leaves it running. Settings → Background chooses *Menu bar* (fader icon with
status, Open, Recall Scene, Settings, Quit) or *Invisible* (open UR44 Mixer from Applications /
Spotlight to show the window), and turns start-at-login and restore on or off.

Bundle layout: `UR44 Mixer.app` is a tiny launcher (`ur44-open`) that asks the agent
`Contents/Helpers/UR44 Mixer Agent.app` (`ur44-mixer`) to show its window over a socket in
`~/Library/Application Support/UR44 Mixer/`, or starts it. The login item is
`~/Library/LaunchAgents/local.ur44.mixer.agent.plist`.

| Page | What it does |
|---|---|
| Mixer | 6 inputs, DAW and REV-X returns, MIX 1/2 master; mute, solo, pan, link + fader link, HPF, phase, REV-X send, effect insert (MON.FX / INS.FX), meters |
| Ch Strip | 4 Sweet Spot Morphing Channel Strips: Sweet Spot presets, MORPH, drive, total gain, compressor, EQ, output meter |
| Guitar Amp | Guitar Amp Classics (Clean / Crunch / Lead / Drive) |
| REV-X | Hall / Room / Plate with every parameter |
| Scenes | Store / recall / delete; files in `~/Library/Application Support/UR44 Mixer/Scenes` |
| Settings | Sample rate, Input 5/6 level, HPF frequency, Loopback, PHONES 2 source |

Sweet Spot presets are read from Yamaha's installed files in
`/Library/Application Support/Yamaha/SSMD`.

## Build

```sh
cargo test
./tools/bundle.sh          # builds ~/Applications/UR44 Mixer.app
```

`ur44ctl` is a command-line tool: `version`, `get`, `set`, `dump`, `watch`, `scan`, and two
non-destructive hardware self-tests (`selftest-recall`, `selftest-strip`).

## How it works

* `src/protocol.rs` — Yamaha SysEx on MIDI "Port 2": `F0 43 {10|30} 3E 14 …`, 7-bit packed numbers,
  meter stream (`02 02 32 7F` to start; 44 values, `dB = v/102.4 − 80`, calibrated with test tones).
* `src/params.rs` — the 177 parameters (names, ranges, defaults) from MemAPI_UR44's item table.
* `src/logic.rs` — dspMixFx's host-side rules re-implemented from the decompiled MemAPI
  (`smd_SetPrmRel*`): stereo link mirroring, solo flag, morph interpolation, REV-X/amp type
  defaults, effect insert budget (4 DSP units; amp = 2).
* `src/units.rs` — Yamaha's own display strings (dB, ms, Hz…) from MemAPI's string table.
* `tools/` — table generators, Ghidra script, probe tools.

## Exactness

The graphs, meters and value displays use Yamaha's own maths, decompiled from the dspMixFx
editor plug-ins (`libChannelStrip_UR44`, `libRevHall_UR44`) and ported in `src/dsp.rs`:

* EQ curve: `FILTER2PEQ::setCoefLowShelf_C / setCoefHiShelf_C / setCoefPEQ_C / power` and
  `Channel_Strip::filterPower` (first-order shelves, Yamaha's high-frequency Q correction), real
  frequency / Q tables, at the UR44's current sample rate.
* Compressor curve: `Channel_Strip::CalcThreData` + `GetCompCurve` (DRIVE → threshold −0.2 dB/step,
  soft ±26 dB / medium ±10 dB / hard knee), drawn without make-up gain like dspMixFx.
* Meters: Yamaha's hardware meter tables `Pjled_dB_Tbl` / `Pjled_GR_Tbl` (8192 = 0 dBFS).
* REV-X time: `GetActualReverbTime` (depends on type and room size); matches the manual's
  "0.780" for the Room preset and the 0.103–31 s Hall range.

## Known gaps / unverified

* MON.FX = Post patch, INS.FX = Pre patch: verified 2026-11-06 by recording a sung signal.
* Gain-reduction meter slot 20 + 2·strip verified for strip 1 only; idle strips 3/4 report 3903 (shown as "–").
* The REV-X graph is a 2-D decay per band; dspMixFx draws a 3-D envelope.
* "Air" on the guitar amps is shown as a number (0–2).
