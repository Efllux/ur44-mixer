//! Test signals and loopback analysis (one capture channel).
//!
//! Sine: outputs 1/2 play a sine of `freq` (default 1 kHz) as a function of the playback frame index.
//! Two independent detectors run on the capture channel:
//! - Predictor: e[n] = x[n] - 2cos(w) x[n-1] + x[n-2] is ~0 for a clean sine. A dropped, repeated or
//!   zeroed sample gives |e| up to ~A*w (0.13 A at 1 kHz), well above noise. Glitch = |e| > GLITCH_REL (0.05) x A;
//!   events closer than 64 samples are merged. It is weak right at the sine peaks, hence:
//! - Phase tracker: I/Q demodulation per 100 ms block against the capture sample index. While no
//!   sample is lost or repeated anywhere in the loop, playback frame p comes back at capture index
//!   p + L with constant L, so the phase is constant. One slipped sample moves it by 360*f/48000 deg
//!   (7.5 deg at 1 kHz). The phase span over the run is the drift, reported in samples.
//!
//! Click: a single-sample impulse on outputs 1/2 at the start of every 250th playback URB.

use std::f64::consts::TAU;

pub struct Sine {
    w: f64,
    pub amp: f32,
}

impl Sine {
    pub fn new(freq: f64, rate: f64, amp: f32) -> Self {
        Self { w: TAU * freq / rate, amp }
    }
    /// Sample for playback frame index `p` (phase computed from the index, never accumulated).
    pub fn at(&self, p: u64) -> f32 {
        let ph = (p as f64 * self.w) % TAU; // f64 keeps < 1e-6 rad error for > 1e9 frames
        (ph.sin() as f32) * self.amp
    }
}

/// A dropout scores ~1, a slipped sample ~w (0.13 at 1 kHz / 48 kHz); analog noise at 192 kHz~0.03.
pub const GLITCH_REL: f32 = 0.05;

pub struct SineAnalyzer {
    w: f64,
    c: f32,
    x1: f32,
    x2: f32,
    n: u64,
    pub start: u64, // capture index where analysis starts
    pub peak: f32,
    pub glitches: u64,
    pub first_glitches: Vec<u64>,
    last_evt: u64,
    pub max_err_rel: f32,
    // phase tracker
    block: u64,
    bi: f64,
    bq: f64,
    bn: u64,
    pub phases: Vec<(u64, f64, f64)>, // (block start index, unwrapped phase rad, amplitude)
    last_phase: Option<f64>,
    amp_ref: f32,
}

impl SineAnalyzer {
    pub fn new(freq: f64, rate: f64) -> Self {
        let w = TAU * freq / rate;
        Self {
            w,
            c: (2.0 * w.cos()) as f32,
            x1: 0.0,
            x2: 0.0,
            n: 0,
            start: u64::MAX,
            peak: 0.0,
            glitches: 0,
            first_glitches: Vec::new(),
            last_evt: 0,
            max_err_rel: 0.0,
            block: (rate / 10.0) as u64,
            bi: 0.0,
            bq: 0.0,
            bn: 0,
            phases: Vec::new(),
            last_phase: None,
            amp_ref: 0.0,
        }
    }

    /// Feed capture sample `x` at capture index `pos`.
    pub fn feed(&mut self, pos: u64, x: f32) {
        let e = x - self.c * self.x1 + self.x2;
        self.x2 = self.x1;
        self.x1 = x;
        self.n += 1;
        if pos < self.start || self.n < 3 {
            return;
        }
        // phase tracker
        let ph = (pos as f64 * self.w) % TAU;
        self.bi += x as f64 * ph.cos();
        self.bq += x as f64 * ph.sin();
        self.bn += 1;
        if self.bn == self.block {
            let amp = 2.0 * (self.bi * self.bi + self.bq * self.bq).sqrt() / self.bn as f64;
            let mut p = self.bq.atan2(self.bi);
            if let Some(lp) = self.last_phase {
                while p - lp > std::f64::consts::PI {
                    p -= TAU;
                }
                while p - lp < -std::f64::consts::PI {
                    p += TAU;
                }
            }
            self.last_phase = Some(p);
            self.phases.push((pos + 1 - self.bn, p, amp));
            if self.amp_ref == 0.0 {
                self.amp_ref = amp as f32;
            }
            self.bi = 0.0;
            self.bq = 0.0;
            self.bn = 0;
        }
        // predictor
        if x.abs() > self.peak {
            self.peak = x.abs();
        }
        let a = if self.amp_ref > 0.0 { self.amp_ref } else { self.peak };
        if a < 0.003 {
            return;
        }
        let rel = e.abs() / a;
        if rel > self.max_err_rel {
            self.max_err_rel = rel;
        }
        if rel > GLITCH_REL {
            if self.glitches == 0 || pos >= self.last_evt + 64 {
                self.glitches += 1;
                if self.first_glitches.len() < 20 {
                    self.first_glitches.push(pos);
                }
            }
            self.last_evt = pos;
        }
    }

    /// (phase span in samples, first-to-last drift in samples, phase jumps > 2 deg between blocks,
    /// min amplitude, max amplitude)
    pub fn phase_summary(&self) -> Option<(f64, f64, usize, f64, f64)> {
        if self.phases.len() < 2 {
            return None;
        }
        let to_samp = |r: f64| r / self.w;
        let ps: Vec<f64> = self.phases.iter().map(|p| p.1).collect();
        let min = ps.iter().cloned().fold(f64::MAX, f64::min);
        let max = ps.iter().cloned().fold(f64::MIN, f64::max);
        let drift = ps[ps.len() - 1] - ps[0];
        let jumps = ps.windows(2).filter(|w| (w[1] - w[0]).abs() > 2f64.to_radians()).count();
        let amin = self.phases.iter().map(|p| p.2).fold(f64::MAX, f64::min);
        let amax = self.phases.iter().map(|p| p.2).fold(f64::MIN, f64::max);
        Some((to_samp(max - min), to_samp(drift), jumps, amin, amax))
    }
}

pub fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}
