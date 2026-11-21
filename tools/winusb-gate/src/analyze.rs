//! Signal generation and analysis for the loopback tests.
//!
//! Sine mode: every output channel plays a sine at 0.0612345 x rate (2939 Hz at 48 kHz). On each capture
//! channel a two-tap predictor e[n] = x[n] - 2cos(w) x[n-1] + x[n-2] is exactly zero for a clean
//! sine; a dropped, repeated or zeroed sample gives |e| of at least ~A*w^2/2 = 0.077*A, far above
//! the noise floor. Glitch = |e| > 0.02*A, events closer than 64 samples are merged.
//!
//! Click mode: a single-sample impulse on outputs 1/2 every 250 ms; the first capture sample above
//! the threshold after it gives the round trip in the duplex time base (see engine.rs).

pub const OUT_CH: usize = 6;

/// Test tone: 0.0612345 x rate (2939 Hz at 48 kHz). Not a rational fraction of the rate with a small
/// denominator, so a device that replays an old buffer produces phase jumps the detector sees.
pub const W: f64 = std::f64::consts::TAU * 0.0612345;

pub struct SineGen {
    phase: f64,
    step: f64,
    pub amp: f32,
}

impl SineGen {
    pub fn new(rate: u32) -> Self {
        Self { phase: 0.0, step: W, amp: 0.25 + 0.0 * rate as f32 }
    }
    pub fn next(&mut self) -> f32 {
        let v = (self.phase.sin() as f32) * self.amp;
        self.phase += self.step;
        if self.phase > std::f64::consts::TAU {
            self.phase -= std::f64::consts::TAU;
        }
        v
    }
}

#[derive(Default, Clone)]
pub struct ChanStats {
    pub peak: f32,
    pub glitches: u64,
    pub max_err_rel: f32,
    pub first_glitches: Vec<u64>, // input sample positions of the first glitch events
}

pub struct SineAnalyzer {
    c: f32,
    x1: [f32; OUT_CH],
    x2: [f32; OUT_CH],
    amp: [f32; OUT_CH],     // running peak estimate (decays slowly)
    last_evt: [u64; OUT_CH],
    pub chans: Vec<ChanStats>,
    pub warmup: u64,
    pub n: u64,
}

impl SineAnalyzer {
    pub fn new(rate: u32) -> Self {
        Self {
            c: (2.0 * W.cos()) as f32,
            x1: [0.0; OUT_CH],
            x2: [0.0; OUT_CH],
            amp: [0.0; OUT_CH],
            last_evt: [0; OUT_CH],
            chans: vec![ChanStats::default(); OUT_CH],
            warmup: rate as u64, // ignore the first second (DSP and stream settle)
            n: 0,
        }
    }

    /// Feed one capture frame. `pos` = input stream position of the frame.
    pub fn frame(&mut self, pos: u64, x: &[f32; OUT_CH]) {
        for ch in 0..OUT_CH {
            let v = x[ch];
            let a = &mut self.amp[ch];
            *a = if v.abs() > *a { v.abs() } else { *a * 0.99999 };
            let e = v - self.c * self.x1[ch] + self.x2[ch];
            self.x2[ch] = self.x1[ch];
            self.x1[ch] = v;
            if self.n < self.warmup || *a < 0.005 {
                continue;
            }
            let st = &mut self.chans[ch];
            if v.abs() > st.peak {
                st.peak = v.abs();
            }
            let rel = e.abs() / *a;
            if rel > st.max_err_rel {
                st.max_err_rel = rel;
            }
            if rel > 0.02 {
                if pos >= self.last_evt[ch] + 64 || st.glitches == 0 {
                    st.glitches += 1;
                    if st.first_glitches.len() < 20 {
                        st.first_glitches.push(pos);
                    }
                }
                self.last_evt[ch] = pos;
            }
        }
        self.n += 1;
    }
}

pub struct ClickTester {
    period: u64,
    next_click: u64,
    pending: std::collections::VecDeque<(u64, bool)>, // duplex position of each click, found?
    pub lat: Vec<Vec<i64>>,                           // per input channel, round trips in frames
    last_match: Vec<u64>,                             // per channel: click already matched
    pub emitted: u64,
    pub missed: u64,
    max_wait: u64,
    pub amp: f32,
    pub thr: f32,
}

impl ClickTester {
    pub fn new(rate: u32) -> Self {
        Self {
            period: rate as u64 / 4,
            next_click: rate as u64, // first click after 1 s
            pending: Default::default(),
            lat: vec![Vec::new(); OUT_CH],
            last_match: vec![u64::MAX; OUT_CH],
            emitted: 0,
            missed: 0,
            max_wait: rate as u64 / 5,
            amp: 0.1,
            thr: 0.02,
        }
    }

    /// Output sample for duplex position `pos` (channels 1/2 get the click).
    pub fn out(&mut self, pos: u64) -> f32 {
        if pos >= self.next_click {
            self.pending.push_back((pos, false));
            self.emitted += 1;
            self.next_click += self.period;
            self.amp
        } else {
            0.0
        }
    }

    /// Returns the click position when this sample is the first detection of a click.
    pub fn input(&mut self, pos: u64, x: &[f32; OUT_CH]) -> Option<u64> {
        let mut first = None;
        while let Some(&(p, found)) = self.pending.front() {
            if pos > p + self.max_wait {
                self.pending.pop_front();
                if !found {
                    self.missed += 1;
                }
            } else {
                break;
            }
        }
        for ch in 0..OUT_CH {
            if x[ch].abs() <= self.thr {
                continue;
            }
            // The most recent click not newer than this sample.
            if let Some(c) = self.pending.iter_mut().rev().find(|c| c.0 <= pos) {
                if pos - c.0 < self.max_wait && self.last_match[ch] != c.0 {
                    self.last_match[ch] = c.0;
                    self.lat[ch].push((pos - c.0) as i64);
                    if !c.1 {
                        first = Some(c.0);
                    }
                    c.1 = true;
                }
            }
        }
        first
    }
}

pub fn stats(v: &[i64]) -> Option<(i64, f64, i64)> {
    if v.is_empty() {
        return None;
    }
    let min = *v.iter().min().unwrap();
    let max = *v.iter().max().unwrap();
    let mean = v.iter().sum::<i64>() as f64 / v.len() as f64;
    Some((min, mean, max))
}
