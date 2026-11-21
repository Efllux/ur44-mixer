# Yamaha Steinberg USB Driver 4.0.4b (DriverKit) — measured 2026-11-06

UR44 FW 2.12, Yamaha mode (CC MODE off), MacBook Apple Silicon, macOS 26.6.2.
Method: tools/roundtrip.swift — one Core Audio IOProc writes a -30 dBFS click on Mix 1 L/R every 250 ms;
the UR44's internal LOOPBACK (MIX 1 → computer inputs) returns it; round trip = frames between writing
the click and reading it back in the same IOProc (what a DAW experiences). 3 runs × ~11 clicks per setting,
all identical.

## Outputs
1/2 Mix 1 L/R · 3/4 Mix 2 L/R · 5/6 **FX Bus L/R** (computer audio straight into REV-X). Inputs: 6.

## Round trip (app → UR44 → app)
| Rate | Buffer (frames) | Round trip | ms |
|---|---|---|---|
| 44.1 kHz | 32 | 238 | 5.40 |
| 48 kHz | 15 (driver minimum) | 216 | **4.50** |
| 48 kHz | 32 | 250 | 5.21 |
| 48 kHz | 64 | 314 | 6.54 |
| 96 kHz | 29 (driver minimum) | 425 | **4.43** |
| 96 kHz | 64 | 495 | 5.16 |
| 192 kHz | 128 | 985 | 5.13 |

Fit at 48 kHz: round trip ≈ 2 × buffer + 186 frames. Device-internal path (USB→DSP→loopback→USB,
from Core Audio timestamps): 29 frames @48 kHz, 53 @96 kHz.
Reported: 48 kHz in 35+84 / out 36+73; 96 kHz in 48+169 / out 48+145.

**Best: ≈ 4.4–4.5 ms.** Target for 2× better: ≤ 2.2 ms.

## CPU of the driver extension
Idle: ~0 ms/min. Streaming 48 kHz, 32-frame buffer: ~5.5 s CPU per minute (≈ 9 % of one core).

# Apple's built-in driver (CC mode) — measured 2026-11-07, same method

| Rate | Buffer | Round trip | ms | vs Yamaha same setting |
|---|---|---|---|---|
| 44.1 kHz | 32 | 172 | 3.90 | 5.40 → 1.38× faster |
| 48 kHz | 15 (minimum) | 147 | **3.06** | 4.50 → 1.47× |
| 48 kHz | 32 | 181 | 3.77 | 5.21 → 1.38× |
| 48 kHz | 64 | 245 | 5.10 | 6.54 → 1.28× |
| 96 kHz | 29 (minimum) | 283 | **2.95** | 4.43 → 1.50× |
| 96 kHz | 64 | 353 | 3.68 | 5.16 → 1.40× |
| 192 kHz | 128 | 697 | 3.63 | 5.13 → 1.41× |

Fit at 48 kHz: ≈ 2 × buffer + 117 frames (Yamaha: + 186). The first run after a sample-rate change
detected no clicks (the UR44 resets its internal routing on a rate change in CC mode); later runs
were identical. Apple's best ≈ 2.95 ms; 2× better than Yamaha's best (4.43 ms) needs ≤ 2.2 ms,
i.e. another ~25 % below Apple's driver.
