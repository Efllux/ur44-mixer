//! Channel-strip maths ported from Yamaha's libChannelStrip_UR44 (decompiled with Ghidra):
//! `FILTER2PEQ::setCoefLowShelf_C / setCoefHiShelf_C / setCoefPEQ_C / power`,
//! `Channel_Strip::CalcThreData / GetCompCurve / filterPower`, `Table2*`, and the hardware
//! meter tables `Pjled_dB_Tbl / Pjled_GR_Tbl`. Used by the graphs and meters.

#[path = "dsp_tables.rs"]
mod tables;
pub use tables::*;

use std::f64::consts::PI;

/// Shelf/peak shape constants set in the Channel_Strip constructor
/// (Table2Option(0x20) = 0.32, Table2Option(0x46) = 0.70, 18 dB).
const SHELF_SHAPE: f64 = 0.32;
const PEQ_SHAPE: f64 = 0.70;
const SHAPE_DB: f64 = 18.0;

/// Biquad in Yamaha's form: H(z) = (b0 + b1 z⁻¹ + b2 z⁻²) / (1 − a1 z⁻¹ − a2 z⁻²).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

const FLAT: Biquad = Biquad { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

fn exp10(x: f64) -> f64 {
    10f64.powf(x)
}

fn above_nyquist_guard(fs: f64, f: f64) -> bool {
    fs < 44_100.0 && f > fs * 0.5 - 10.0
}

impl Biquad {
    /// FILTER2PEQ::setCoefLowShelf_C (first-order shelf).
    pub fn low_shelf(fs: f64, f: f64, gain_db: f64) -> Self {
        if above_nyquist_guard(fs, f) {
            return FLAT;
        }
        let g = exp10(gain_db.abs() / 20.0);
        let l = exp10(SHAPE_DB / 20.0);
        let t = (f * PI / fs).tan();
        let k = g.powf(1.0 - SHELF_SHAPE) * (l.powf(SHELF_SHAPE) * t).atan().tan();
        if gain_db >= 0.0 {
            let d = g + k;
            Biquad { b0: g * (k + 1.0) / d, b1: -g * (1.0 - k) / d, b2: 0.0, a1: (g - k) / d, a2: 0.0 }
        } else {
            let kg = k / g;
            let d = k + 1.0;
            Biquad { b0: (kg + 1.0) / d, b1: -(1.0 - kg) / d, b2: 0.0, a1: (1.0 - k) / d, a2: 0.0 }
        }
    }

    /// FILTER2PEQ::setCoefHiShelf_C (first-order shelf).
    pub fn high_shelf(fs: f64, f: f64, gain_db: f64) -> Self {
        if above_nyquist_guard(fs, f) {
            return FLAT;
        }
        let g = exp10(gain_db / 20.0);
        let l = exp10(SHAPE_DB / 20.0);
        let t = (f * PI / fs).tan();
        let w = (t * (1.0 / l).powf(SHELF_SHAPE)).atan().tan();
        if gain_db >= 0.0 {
            let k = g.powf(-(1.0 - SHELF_SHAPE)) * w;
            let d = g * k + 1.0;
            Biquad { b0: g * (k + 1.0) / d, b1: -g * (1.0 - k) / d, b2: 0.0, a1: (1.0 - g * k) / d, a2: 0.0 }
        } else {
            let k = g.powf(1.0 - SHELF_SHAPE) * w;
            let d = k + 1.0;
            Biquad { b0: (g + k) / d, b1: (k - g) / d, b2: 0.0, a1: (1.0 - k) / d, a2: 0.0 }
        }
    }

    /// FILTER2PEQ::setCoefPEQ_C, including Yamaha's high-frequency Q correction.
    pub fn peaking(fs: f64, f: f64, gain_db: f64, q: f64) -> Self {
        if above_nyquist_guard(fs, f) {
            return FLAT;
        }
        let mut q = q;
        // (log2 coefficient, offset, per-Hz slope, min frequency) per sample-rate band.
        let corr = if fs < 48_000.0 {
            (f > 2000.0).then_some((-0.0301, 1.25513, -4.817e-05))
        } else if fs < 88_200.0 {
            (f > 2000.0).then_some((-0.02143, 1.25686, -4.103e-05))
        } else if fs < 96_000.0 {
            (f > 4000.0).then_some((-0.0317, 1.29486, -2.462e-05))
        } else {
            (f > 4000.0).then_some((-0.02382, 1.21531, -2.004e-05))
        };
        if let Some((c, off, slope)) = corr {
            let factor = q.ln() * c * std::f64::consts::LOG2_E + off + f * slope;
            if factor <= 1.0 {
                q = (factor * q).max(0.1);
            }
        }
        let d3 = gain_db.abs() - SHAPE_DB;
        let u = if d3 == -100.0 { 0.0 } else { exp10(d3 / 20.0) };
        let inv_q = (1.0 / q) / u.powf(PEQ_SHAPE);
        let g = exp10(gain_db.abs() / 20.0);
        let t = (f * PI / fs).tan();
        let d8 = t * inv_q;
        let t2 = t * t;
        let d11 = t2 + 1.0;
        let (num_k, den_k, a2_k) = if gain_db >= 0.0 { (g * d8, d8, d8) } else { (d8, g * d8, g * d8) };
        let den = t2 + den_k + 1.0;
        let tm = t2 - 1.0;
        Biquad {
            b0: (num_k + d11) / den,
            b1: 2.0 * tm / den,
            b2: (d11 - num_k) / den,
            a1: -2.0 * tm / den,
            a2: (a2_k - d11) / den,
        }
    }

    /// FILTER2PEQ::power: |H(e^{jw})|² in dB.
    pub fn power_db(&self, f: f64, fs: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (c1, s1) = (w.cos(), -w.sin()); // z⁻¹ = cos w − j sin w
        let (c2, s2) = (c1 * c1 - s1 * s1, 2.0 * c1 * s1); // z⁻²
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = self.b1 * s1 + self.b2 * s2;
        let dr = 1.0 - self.a1 * c1 - self.a2 * c2;
        let di = -self.a1 * s1 - self.a2 * s2;
        let p = (nr * nr + ni * ni) / (dr * dr + di * di);
        if p <= 0.0 { -100.0 } else { 10.0 * p.log10() }
    }
}

pub fn freq_hz(raw: i32) -> f64 {
    FREQ_HZ[raw.clamp(0, 124) as usize]
}
pub fn q_of(raw: i32) -> f64 {
    Q[raw.clamp(0, 60) as usize]
}
pub fn gain_db(raw: i32) -> f64 {
    f64::from(raw) * 0.1 - 18.0
}

/// The three EQ bands of a channel strip, built from raw parameter values.
pub struct Eq {
    pub fs: f64,
    pub bands: [Biquad; 3],
}

impl Eq {
    pub fn new(fs: f64, low: (i32, i32), mid: (i32, i32, i32), high: (i32, i32)) -> Self {
        Eq {
            fs,
            bands: [
                Biquad::low_shelf(fs, freq_hz(low.0), gain_db(low.1)),
                Biquad::peaking(fs, freq_hz(mid.0), gain_db(mid.1), q_of(mid.2)),
                Biquad::high_shelf(fs, freq_hz(high.0), gain_db(high.1)),
            ],
        }
    }

    /// Channel_Strip::filterPower: band 0 = sum of all bands, 1..=3 = low, mid, high.
    pub fn response_db(&self, f: f64, band: usize) -> f64 {
        let f = f.min(self.fs * 0.48);
        match band {
            1..=3 => self.bands[band - 1].power_db(f, self.fs),
            _ => self.bands.iter().map(|b| b.power_db(f, self.fs)).sum(),
        }
    }
}

/// Compressor static curve (Channel_Strip::CalcThreData + GetCompCurve).
pub struct Comp {
    threshold: f64,
    r: f64,
    knee: i32,
}

impl Comp {
    /// From raw values: DRIVE (0..200), ratio (0..120), knee (0 soft, 1 medium, 2 hard), threshold offset (0..200).
    pub fn new(drive: i32, ratio: i32, knee: i32, thresh_ofst: i32) -> Self {
        let thre = -f64::from(drive) * 0.2; // Table2Threshold
        let off = f64::from(thresh_ofst) * 0.2 - 20.0; // Table2ThresholdOffset
        let mut t = thre + off;
        if off < 0.0 && thre >= -6.2 {
            t = thre * (thre + off) / -6.2;
        }
        let t = t.clamp(-54.0, 0.0);
        let ratio = RATIO[ratio.clamp(0, 120) as usize];
        Comp { threshold: t, r: 1.0 - 1.0 / ratio, knee }
    }

    pub fn threshold_db(&self) -> f64 {
        self.threshold
    }

    /// Output level (dB) for input level `x` (dB), without make-up gain — as dspMixFx draws it.
    pub fn curve(&self, x: f64) -> f64 {
        let mut d = (x - self.threshold).max(-30.0) / 20.0;
        let r = self.r;
        let exp = if self.threshold == 0.0 || !(0..=1).contains(&self.knee) {
            if d <= 0.0 { 0.0 } else { -r * d }
        } else if self.knee == 1 {
            if d <= -0.5 {
                0.0
            } else {
                if d <= 0.5 {
                    d = (d + 0.5) * (d + 0.5) * 0.5;
                }
                -r * d
            }
        } else if d > 1.3 {
            -r * d
        } else if d <= -1.3 {
            0.0
        } else {
            (d + 1.3) * (d + 1.3) * -0.19231 * r
        };
        x + 20.0 * exp
    }
}

/// Hardware level meter: raw value → segment index 0..=34 and Yamaha's label ("-oo" … "0", "OVER").
pub fn level_segment(raw: u32) -> (usize, &'static str) {
    let i = LEVEL_SEG.iter().position(|t| raw < u32::from(*t)).unwrap_or(LEVEL_SEG.len());
    (i, LEVEL_SEG_LABEL[i])
}

/// Hardware gain-reduction meter: raw value → segment index 0..=36 and label ("-24.0" … "0").
pub fn gr_segment(raw: u32) -> (usize, &'static str) {
    let i = GR_SEG.iter().position(|t| raw < u32::from(*t)).unwrap_or(GR_SEG.len());
    (i, GR_SEG_LABEL[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_reach_their_gain() {
        let fs = 48_000.0;
        let lo = Biquad::low_shelf(fs, 100.0, 6.0);
        assert!((lo.power_db(1.0, fs) - 6.0).abs() < 0.05);
        assert!(lo.power_db(20_000.0, fs).abs() < 0.3);
        let hi = Biquad::high_shelf(fs, 10_000.0, -6.0);
        assert!(hi.power_db(20.0, fs).abs() < 0.05);
        assert!((hi.power_db(23_999.0, fs) + 6.0).abs() < 0.3);
    }

    #[test]
    fn peaking_hits_gain_at_centre() {
        let fs = 48_000.0;
        let p = Biquad::peaking(fs, 1000.0, 6.0, 1.0);
        assert!((p.power_db(1000.0, fs) - 6.0).abs() < 0.1, "{}", p.power_db(1000.0, fs));
        assert!(p.power_db(30.0, fs).abs() < 0.2);
        let c = Biquad::peaking(fs, 1000.0, -9.0, 2.0);
        assert!((c.power_db(1000.0, fs) + 9.0).abs() < 0.1);
    }

    #[test]
    fn flat_eq_is_flat() {
        let e = Eq::new(48_000.0, (32, 180), (72, 180, 12), (112, 180));
        for f in [30.0, 300.0, 3000.0, 15_000.0] {
            assert!(e.response_db(f, 0).abs() < 1e-9);
        }
    }

    #[test]
    fn compressor_threshold_and_ratio() {
        // DRIVE 5.00 (raw 100), offset 0 dB (raw 100), 2.5:1 (raw 30), hard knee.
        let c = Comp::new(100, 30, 2, 100);
        assert!((c.threshold_db() + 20.0).abs() < 1e-9);
        assert!((c.curve(-40.0) + 40.0).abs() < 1e-9);
        assert!((c.curve(0.0) - (-20.0 + 20.0 / 2.5)).abs() < 1e-9);
    }

    #[test]
    fn meter_segments_match_yamaha_labels() {
        assert_eq!(level_segment(6124).1, "-20"); // -20 dBFS test tone, captured 2026-11-06
        assert_eq!(level_segment(4076).1, "-43"); // -40 dBFS tone sits just under the -40 segment
        assert_eq!(level_segment(9000).1, "OVER");
        assert_eq!(gr_segment(40959).1, "0");
        assert_eq!(gr_segment(8000).1, "-2.0");
    }
}

/// GetActualReverbTime (libRevHall_UR44): REV-X time in seconds for the reverb type
/// (0 Hall, 1 Room, 2 Plate), room size (0..=31) and time setting (0..=69).
pub fn reverb_time_s(rev_type: i32, room: i32, time: i32) -> f64 {
    let reference = match rev_type {
        1 => 19,
        2 => 15,
        _ => 30,
    };
    let d = reference - room;
    let factor = if d >= 1 { 0.965f64.powi(d) } else { 1.035f64.powi(-d) };
    let base = REVERB_TIME_TABLE[time.clamp(0, 69) as usize] as f32;
    ((f64::from(base * factor as f32) * 1000.0 + 0.5).floor()) * 0.001
}

/// Float2RevTimeStr: "0.781s", "2.51s", "28.9s".
pub fn reverb_time_label(s: f64) -> String {
    if s < 1.0 {
        format!("{s:.3} s")
    } else if s < 10.0 {
        format!("{s:.2} s")
    } else {
        format!("{s:.1} s")
    }
}

#[cfg(test)]
mod reverb_tests {
    use super::*;

    #[test]
    fn matches_manual() {
        // Room preset defaults (time 6, room 15) — the manual's screenshot shows "0.780".
        assert_eq!(reverb_time_label(reverb_time_s(1, 15, 6)), "0.780 s"); // Yamaha multiplies in f32, like this port
        // Hall range per manual: 0.103 s – 31.0 s.
        assert!((reverb_time_s(0, 0, 0) - 0.104).abs() < 0.002);
        assert!((reverb_time_s(0, 31, 69) - 31.05).abs() < 0.01);
    }
}
