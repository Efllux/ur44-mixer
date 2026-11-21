//! Duplex isochronous streaming over WinUSB, structured like a minimal ASIO driver.
//!
//! Finding that shapes this engine (see docs/winusb-gate-results.md): with ContinueStream = TRUE,
//! WinUsb_Read/WriteIsochPipeAsap continue the stream at the next *microframe*, and a transfer
//! occupies ceil(length / MaximumBytesPerInterval) microframes. So a write of one packet is one
//! microframe, and OUT packets can be sized individually by writing one packet per transfer.
//!
//! * capture: IN transfers of `in_pk` packets (microframes), `k_in` in flight;
//! * at each IN completion (the "callback") the app reads the n captured frames and writes n
//!   output frames, as OUT writes of `out_pk` packets each. Each write carries the frame counts of
//!   the same microframes of capture (implicit feedback). With out_pk = 1 every packet has the
//!   exact size; with out_pk > 1 WinUSB packs the write into full packets first (front-loaded);
//! * timing: both streams start at explicit frames (an 8-packet lead-in each), the OUT stream one
//!   frame earlier plus `d` packets of silence, so the playback for capture microframe m goes out
//!   in microframe m + d. Every packet's bus microframe is therefore known.
//!
//! Round trip is measured in the duplex time base: an output frame written at callback k, offset j,
//! has position in_pos(k) + j; a click found at input position q has round trip q - (in_pos(k) + j).
//! For an ASIO driver whose buffer equals one IN transfer this is 2 x buffer + overhead.

use crate::analyze::{self, ClickTester, SineAnalyzer, SineGen, OUT_CH};
use crate::desc::AltFormat;
use crate::winusb::*;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, VecDeque};
use windows_sys::Win32::Devices::Usb::USBD_ISO_PACKET_DESCRIPTOR;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::Threading::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Signal {
    Sine,
    Click,
    Silence,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Feedback {
    Follow,
    Nominal,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub rate: u32,
    pub in_pk: usize,
    pub k_in: usize,
    pub out_pk: usize,
    pub d: usize, // microframes from a capture microframe to its playback microframe
    pub secs: f64,
    pub signal: Signal,
    pub fb: Feedback,
    pub early_probe: bool,
    pub fixed_packets: Option<u32>,
    pub start_lead: u32,
    pub mmcss: bool,
    pub quiet: bool,
    pub late_us: Option<f64>, // late-write probe (signal silence)
}

const SENTINEL: [u8; 4] = [0x3C, 0xC3, 0xA5, 0x5A];
const LEAD_PK: usize = 8;

fn qpc() -> i64 {
    let mut t = 0i64;
    unsafe { QueryPerformanceCounter(&mut t) };
    t
}

fn qpf() -> f64 {
    let mut f = 0i64;
    unsafe { QueryPerformanceFrequency(&mut f) };
    f as f64
}

fn cpu_time_s() -> f64 {
    let mut c = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut e, mut k, mut u) = (c, c, c);
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 * 1e-7;
    f(k) + f(u)
}

#[inline]
fn get24(b: &[u8]) -> f32 {
    let v = ((b[0] as u32) << 8 | (b[1] as u32) << 16 | (b[2] as u32) << 24) as i32 >> 8;
    v as f32 / 8388608.0
}

#[inline]
fn put24(d: &mut [u8], v: f32) {
    let s = (v.clamp(-1.0, 1.0) * 8388607.0).round() as i32;
    d[0] = s as u8;
    d[1] = (s >> 8) as u8;
    d[2] = (s >> 16) as u8;
}

pub struct Hist {
    edges: Vec<f64>,
    counts: Vec<u64>,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub n: u64,
}

impl Hist {
    pub fn new(edges: &[f64]) -> Self {
        Self { edges: edges.to_vec(), counts: vec![0; edges.len() + 1], min: f64::MAX, max: f64::MIN, sum: 0.0, n: 0 }
    }
    pub fn add(&mut self, v: f64) {
        let b = self.edges.iter().position(|&e| v < e).unwrap_or(self.edges.len());
        self.counts[b] += 1;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += v;
        self.n += 1;
    }
    pub fn render(&self, unit: &str) -> String {
        if self.n == 0 {
            return "(no data)".into();
        }
        let mut s = format!("n={} min={:.0} mean={:.0} max={:.0} {unit} |", self.n, self.min, self.sum / self.n as f64, self.max);
        for (i, c) in self.counts.iter().enumerate() {
            if *c == 0 {
                continue;
            }
            let lo = if i == 0 { "-inf".to_string() } else { format!("{:.0}", self.edges[i - 1]) };
            let hi = if i == self.edges.len() { "inf".to_string() } else { format!("{:.0}", self.edges[i]) };
            s += &format!(" [{lo},{hi}):{c}");
        }
        s
    }
}

pub struct Report {
    pub text: String,
    pub glitches: u64,
    pub dropouts: u64, // stream breaks + packet errors + empty packets after stable
    pub rtl_frames: Option<(i64, f64, i64)>,
}

#[derive(Default)]
struct Stats {
    in_transfers: u64,
    in_packets: u64,
    in_frames: u64,
    in_pkt_status: BTreeMap<i32, u64>,
    in_xfer_err: BTreeMap<u32, u64>,
    in_submit_err: BTreeMap<u32, u64>,
    in_restarts: u64,
    in_empty_after_stable: u64,
    in_count_hist: BTreeMap<u32, u64>,
    out_writes: u64,
    out_frames: u64,
    out_submit_err: BTreeMap<u32, u64>,
    out_xfer_err: BTreeMap<u32, u64>,
    out_restarts: u64,
    out_clamped: u64,
    out_wait_timeouts: u64,
    in_wait_timeouts: u64,
    err_by_micro: [u64; 8],
    ok_by_micro: [u64; 8],
}

fn bump<K: Ord>(m: &mut BTreeMap<K, u64>, k: K) {
    *m.entry(k).or_insert(0) += 1;
}

struct InFlight {
    slot: usize,       // ring slot, or usize::MAX for the lead-in region
    packets: usize,
    start_micro: i64,  // bus microframe of the first packet (-1 = unknown)
}

/// Linear microframe -> QPC model, re-anchored every 50 ms.
struct Clock {
    track: Option<HANDLE>,
    anchor_micro: i64,
    anchor_qpc: i64,
    ticks_per_micro: f64,
    next_refresh: i64,
}

impl Clock {
    fn refresh(&mut self, ifc: &Iface, now: i64, freq: f64) {
        if now < self.next_refresh {
            return;
        }
        self.next_refresh = now + (0.05 * freq) as i64;
        let Some(t) = self.track else { return };
        // Ask for the QPC of the current frame's microframe 0.
        if let Some(info) = ifc.frame_qpc(t, 0, 0) {
            let f = info.CurrentUSBFrameNumber;
            if let Some(i2) = ifc.frame_qpc(t, f, 0) {
                self.anchor_micro = f as i64 * 8;
                self.anchor_qpc = i2.QueryPerformanceCounterAtInputFrameOrMicroFrame;
            }
        }
    }
    fn qpc_of(&self, micro: i64) -> Option<i64> {
        if self.track.is_none() || self.anchor_qpc == 0 {
            return None;
        }
        Some(self.anchor_qpc + ((micro - self.anchor_micro) as f64 * self.ticks_per_micro) as i64)
    }
}

pub fn run(cfg: &Config, fin: &AltFormat, fout: &AltFormat) -> Result<Report> {
    let alt = fin.alt;
    let fb_in = fin.frame_bytes() as usize;
    let fb_out = fout.frame_bytes() as usize;
    if fb_in != 18 || fb_out != 18 {
        bail!("unexpected frame size");
    }
    if cfg.in_pk % cfg.out_pk != 0 || LEAD_PK % cfg.out_pk != 0 {
        bail!("--in-pk and 8 must be multiples of --out-pk");
    }
    if cfg.d % cfg.out_pk != 0 && std::env::var_os("UR_ALLOW_SHORT_PREFILL").is_none() {
        // Short writes in the prefill made the playback timing wander (results doc, d sweep).
        bail!("--d must be a multiple of --out-pk (set UR_ALLOW_SHORT_PREFILL to override)");
    }
    let freq = qpf();
    let us = |dt: i64| dt as f64 * 1e6 / freq;
    let mut log = String::new();
    let quiet = cfg.quiet;
    macro_rules! say {
        ($($a:tt)*) => {{ let l = format!($($a)*); if !quiet { eprintln!("{l}"); } log += &l; log.push('\n'); }};
    }

    // ---- open and configure ----
    let ifin = Iface::open(&GUID_MI02).context("opening interface 2 (capture, MI_02)")?;
    let ifout = Iface::open(&GUID_MI01).context("opening interface 1 (playback, MI_01)")?;
    ifin.set_alt(0)?;
    ifin.set_alt(alt)?;
    ifout.set_alt(0)?;
    ifout.set_alt(alt)?;
    let mut rate_ok = false;
    let mut last_err = String::new();
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50 << (attempt - 1)));
        }
        match ifin.set_rate(fin.ep, cfg.rate).and_then(|ri| Ok((ri, ifout.set_rate(fout.ep, cfg.rate)?))) {
            Ok((ri, ro)) if ri == cfg.rate && ro == cfg.rate => {
                rate_ok = true;
                say!("rate: SET_CUR {} Hz on 0x{:02x} and 0x{:02x}, GET_CUR reads back {ri} / {ro}", cfg.rate, fin.ep, fout.ep);
                break;
            }
            Ok((ri, ro)) => last_err = format!("read back {ri} / {ro}"),
            Err(e) => last_err = format!("{e:#}"),
        }
    }
    if !rate_ok {
        bail!("cannot set the rate: {last_err}");
    }
    let pin = ifin.query_pipe(alt, 0)?;
    let pout = ifout.query_pipe(alt, 0)?;
    say!(
        "pipes: IN 0x{:02x} MaxBytesPerInterval {} Interval {} | OUT 0x{:02x} MaxBytesPerInterval {} Interval {}",
        pin.PipeId, pin.MaximumBytesPerInterval, pin.Interval, pout.PipeId, pout.MaximumBytesPerInterval, pout.Interval
    );
    let mbpi_in = pin.MaximumBytesPerInterval as usize;
    let mbpi_out = pout.MaximumBytesPerInterval as usize;
    let max_frames_out = (mbpi_out / fb_out) as u32;
    if let Some(n) = cfg.fixed_packets {
        match ifout.set_pipe_policy_u32(pout.PipeId, ISO_NUM_FIXED_PACKETS, n) {
            Ok(()) => say!("OUT pipe policy ISO_NUM_FIXED_PACKETS = {n}: accepted"),
            Err(e) => say!("OUT pipe policy ISO_NUM_FIXED_PACKETS = {n}: {e:#}"),
        }
    }

    // ---- buffers: a ring of slots per direction plus one 8-packet lead-in region ----
    let in_slot_len = cfg.in_pk * mbpi_in;
    let out_slot_len = cfg.out_pk * mbpi_out;
    let k_out = (cfg.d + cfg.in_pk) / cfg.out_pk + LEAD_PK / cfg.out_pk + 4;
    let in_lead_off = cfg.k_in * in_slot_len;
    let out_lead_off = k_out * out_slot_len;
    let mut in_mem = vec![0u8; in_lead_off + LEAD_PK * mbpi_in];
    let mut out_mem = vec![0u8; out_lead_off + LEAD_PK * mbpi_out];
    let in_buf = ifin.register(pin.PipeId, &mut in_mem)?;
    let out_buf = ifout.register(pout.PipeId, &mut out_mem)?;
    let in_base = in_mem.as_mut_ptr();
    let out_base = out_mem.as_mut_ptr();
    let mut in_desc: Vec<Vec<USBD_ISO_PACKET_DESCRIPTOR>> =
        (0..cfg.k_in).map(|_| vec![USBD_ISO_PACKET_DESCRIPTOR { Offset: 0, Length: 0, Status: 0 }; cfg.in_pk]).collect();
    let mut lead_desc = vec![USBD_ISO_PACKET_DESCRIPTOR { Offset: 0, Length: 0, Status: 0 }; LEAD_PK];
    let mut in_ov: Vec<Ov> = (0..cfg.k_in).map(|_| Ov::new()).collect();
    let mut in_lead_ov = Ov::new();
    let mut out_ov: Vec<Ov> = (0..k_out).map(|_| Ov::new()).collect();
    let mut out_lead_ov = Ov::new();
    let mut clock = Clock { track: ifin.start_time_sync().ok(), anchor_micro: 0, anchor_qpc: 0, ticks_per_micro: freq / 8000.0, next_refresh: 0 };
    if clock.track.is_none() {
        say!("warning: StartTrackingForTimeSync failed; no frame-to-QPC mapping");
    }
    clock.refresh(&ifin, qpc(), freq);

    // ---- real-time priority ----
    let mut mmcss_task = 0u32;
    let mmcss = if cfg.mmcss {
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let h = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut mmcss_task) };
        if h.is_null() {
            say!("warning: MMCSS Pro Audio refused: {}", last_error());
        } else {
            unsafe { AvSetMmThreadPriority(h, AVRT_PRIORITY_CRITICAL) };
        }
        h
    } else {
        std::ptr::null_mut()
    };

    // ---- state ----
    let mut st = Stats::default();
    let mut nom_acc = 0u32;
    let rate = cfg.rate;
    let nominal = move |acc: &mut u32| -> u32 {
        *acc += rate;
        let n = *acc / 8000;
        *acc %= 8000;
        n
    };
    let mut sine = SineGen::new(cfg.rate);
    let mut clicks = ClickTester::new(cfg.rate);
    let mut sana = SineAnalyzer::new(cfg.rate);
    let mut in_pos: u64 = 0;
    let mut consec = 0u32;
    let mut stable = false;
    let mut lag_hist = Hist::new(&[0.0, 50.0, 100.0, 150.0, 200.0, 300.0, 500.0, 750.0, 1000.0, 2000.0, 5000.0]);
    let mut lead_hist = Hist::new(&[0.0, 62.0, 125.0, 250.0, 375.0, 500.0, 750.0, 1000.0, 1500.0, 2000.0, 4000.0]);
    let mut early_hist = Hist::new(&[0.0, 125.0, 250.0, 375.0, 500.0, 750.0, 1000.0, 1500.0, 2000.0]);
    let (mut early_seen, mut early_missed) = (0u64, 0u64);
    let mut cb_count = 0u64;
    let mut late_inj: VecDeque<i64> = VecDeque::new();
    let mut late_actual_us: Vec<f64> = Vec::new();
    let mut late_wire: Vec<i64> = Vec::new();
    let mut late_last_det: u64 = 0;
    let mut early_rel_sum = [0f64; 8];
    let mut early_rel_n = [0u64; 8];
    let mut pend_hist = Hist::new(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0]);
    let mut wire_lat: Vec<i64> = Vec::new();
    let mut click_out_micro: VecDeque<(u64, i64)> = VecDeque::new();

    let mut in_q: VecDeque<InFlight> = VecDeque::new();
    let mut out_q: VecDeque<InFlight> = VecDeque::new();
    let mut in_next_micro: i64 = -1; // microframe where the next ASAP IN transfer starts
    let mut out_next_micro: i64 = -1;
    let mut in_slot_next = 0usize;
    let mut out_slot_next = 0usize;

    // Submit one IN transfer into the next ring slot (ASAP, ContinueStream).
    macro_rules! submit_in_ring {
        () => {{
            let s = in_slot_next;
            in_slot_next = (in_slot_next + 1) % cfg.k_in;
            in_ov[s].reset();
            if cfg.early_probe {
                let sl = unsafe { std::slice::from_raw_parts_mut(in_base.add(s * in_slot_len), in_slot_len) };
                for (i, b) in sl.iter_mut().enumerate() {
                    *b = SENTINEL[i & 3];
                }
            }
            match in_buf.read_asap((s * in_slot_len) as u32, in_slot_len as u32, true, &mut in_desc[s], &in_ov[s].ov) {
                Ok(()) => {
                    in_q.push_back(InFlight { slot: s, packets: cfg.in_pk, start_micro: in_next_micro });
                    if in_next_micro >= 0 {
                        in_next_micro += cfg.in_pk as i64;
                    }
                    true
                }
                Err(e) => {
                    bump(&mut st.in_submit_err, e);
                    false
                }
            }
        }};
    }
    // Lead-in IN transfer (8 packets) at an explicit frame.
    macro_rules! submit_in_lead {
        ($frame:expr) => {{
            let mut fr: u32 = $frame;
            let start = fr;
            in_lead_ov.reset();
            match in_buf.read_at(in_lead_off as u32, (LEAD_PK * mbpi_in) as u32, &mut fr, &mut lead_desc, &in_lead_ov.ov) {
                Ok(()) => {
                    in_q.push_back(InFlight { slot: usize::MAX, packets: LEAD_PK, start_micro: start as i64 * 8 });
                    in_next_micro = start as i64 * 8 + LEAD_PK as i64;
                    true
                }
                Err(e) => {
                    bump(&mut st.in_submit_err, e);
                    false
                }
            }
        }};
    }

    // Write `counts` packets' worth of frames into an OUT region and submit.
    let mut write_frames = |region_off: usize, counts: &[u32], gen: &mut dyn FnMut(&mut [f32; OUT_CH]), st: &mut Stats| -> usize {
        let mut frames = 0usize;
        for &c in counts {
            if c > max_frames_out {
                st.out_clamped += 1;
            }
            frames += c.min(max_frames_out) as usize;
        }
        let dst = unsafe { std::slice::from_raw_parts_mut(out_base.add(region_off), frames * fb_out) };
        let mut fr = [0f32; OUT_CH];
        for f in 0..frames {
            gen(&mut fr);
            for c in 0..OUT_CH {
                put24(&mut dst[f * fb_out + c * 3..], fr[c]);
            }
        }
        frames
    };

    // ---- start ----
    let Some((cur, _)) = ifin.frame() else { bail!("no frame number") };
    let f_in = cur + cfg.start_lead;
    // OUT lead-in: frame f_in - 1, silence of nominal size, then d packets of silence.
    {
        let counts: Vec<u32> = (0..LEAD_PK).map(|_| nominal(&mut nom_acc)).collect();
        let n = write_frames(out_lead_off, &counts, &mut |f| *f = [0.0; OUT_CH], &mut st);
        let mut fr = f_in - 1;
        out_lead_ov.reset();
        let r = if std::env::var_os("UR_ASAP_START").is_some() {
            out_buf.write_asap(out_lead_off as u32, (n * fb_out) as u32, false, &out_lead_ov.ov)
        } else {
            out_buf.write_at(out_lead_off as u32, (n * fb_out) as u32, &mut fr, &out_lead_ov.ov)
        };
        if let Err(e) = r {
            bail!("cannot start playback at frame {}: error {e}", f_in - 1);
        }
        out_q.push_back(InFlight { slot: usize::MAX, packets: LEAD_PK, start_micro: (f_in as i64 - 1) * 8 });
        out_next_micro = f_in as i64 * 8;
        st.out_writes += 1;
        st.out_frames += n as u64;
    }
    let mut pre = 0usize;
    while pre < cfg.d {
        let s = out_slot_next;
        out_slot_next = (out_slot_next + 1) % k_out;
        // Whole writes first, then single packets for the rest (a few short writes only shift the
        // stack's frame booking by a few frames; see the results doc).
        let pk = if cfg.d - pre >= cfg.out_pk { cfg.out_pk } else { 1 };
        let counts: Vec<u32> = (0..pk).map(|_| nominal(&mut nom_acc)).collect();
        let n = write_frames(s * out_slot_len, &counts, &mut |f| *f = [0.0; OUT_CH], &mut st);
        out_ov[s].reset();
        if let Err(e) = out_buf.write_asap((s * out_slot_len) as u32, (n * fb_out) as u32, true, &out_ov[s].ov) {
            bail!("cannot prefill playback: error {e}");
        }
        out_q.push_back(InFlight { slot: s, packets: pk, start_micro: out_next_micro });
        out_next_micro += pk as i64;
        st.out_writes += 1;
        st.out_frames += n as u64;
        pre += pk;
    }
    if !submit_in_lead!(f_in) {
        bail!("cannot start capture at frame {f_in}: {:?}", st.in_submit_err);
    }
    for _ in 0..cfg.k_in {
        if !submit_in_ring!() {
            bail!("cannot queue capture: {:?}", st.in_submit_err);
        }
    }
    say!(
        "streaming: {} Hz alt {alt}, IN {} x {} packets, OUT writes of {} packets, playback offset d = {} microframes ({:.3} ms), signal {:?}, feedback {:?}, MMCSS {}",
        cfg.rate, cfg.k_in, cfg.in_pk, cfg.out_pk, cfg.d, cfg.d as f64 / 8.0, cfg.signal, cfg.fb, if mmcss.is_null() { "off" } else { "Pro Audio" }
    );

    let t_start = qpc();
    let cpu0 = cpu_time_s();
    let t_end = t_start + (cfg.secs * freq) as i64;
    let mut next_report = t_start + (30.0 * freq) as i64;
    let mut first_desc_logged = false;
    let mut counts_buf: Vec<u32> = Vec::with_capacity(LEAD_PK.max(cfg.in_pk));
    let mut abort = false;

    while qpc() < t_end && !abort {
        let Some(head) = in_q.front() else { break };
        let (slot, packets, start_micro) = (head.slot, head.packets, head.start_micro);
        let (ov_ref, region_off) = if slot == usize::MAX { (&in_lead_ov, in_lead_off) } else { (&in_ov[slot], slot * in_slot_len) };
        let ev = ov_ref.event();
        if cfg.early_probe && slot != usize::MAX {
            let base = unsafe { in_base.add(region_off) };
            let mut seen_t = vec![0i64; packets];
            let t0 = qpc();
            while unsafe { WaitForSingleObject(ev, 0) } == WAIT_TIMEOUT {
                for j in 0..packets {
                    if seen_t[j] == 0 {
                        let o = j * mbpi_in;
                        let p = unsafe { std::slice::from_raw_parts(base.add(o), 4) };
                        if (0..4).any(|k| p[k] != SENTINEL[(o + k) & 3]) {
                            seen_t[j] = qpc();
                        }
                    }
                }
                if us(qpc() - t0) > 200_000.0 {
                    break;
                }
                std::hint::spin_loop();
            }
            let t_done = qpc();
            for j in 0..packets.min(8) {
                if seen_t[j] != 0 {
                    early_rel_sum[j] += us(seen_t[j] - t_done);
                    early_rel_n[j] += 1;
                }
            }
            for j in 0..packets {
                if start_micro >= 0 {
                    if let Some(end) = clock.qpc_of(start_micro + j as i64 + 1) {
                        if seen_t[j] != 0 {
                            early_seen += 1;
                            early_hist.add(us(seen_t[j] - end));
                        } else {
                            early_missed += 1;
                        }
                    }
                }
            }
        } else if unsafe { WaitForSingleObject(ev, 200) } == WAIT_TIMEOUT {
            st.in_wait_timeouts += 1;
            if st.in_wait_timeouts > 10 {
                say!("capture stopped completing; abort");
                abort = true;
            }
            continue;
        }
        let t_cb = qpc();
        clock.refresh(&ifin, t_cb, freq);
        let res = ifin.overlapped_result(&ov_ref.ov, true);
        in_q.pop_front();
        st.in_transfers += 1;
        if start_micro >= 0 {
            if let Some(t) = clock.qpc_of(start_micro + packets as i64) {
                lag_hist.add(us(t_cb - t));
            }
        }
        if let Err(e) = res {
            bump(&mut st.in_xfer_err, e);
        }
        let descs: &[USBD_ISO_PACKET_DESCRIPTOR] = if slot == usize::MAX { &lead_desc } else { &in_desc[slot] };
        if !first_desc_logged && slot != usize::MAX {
            first_desc_logged = true;
            let d: Vec<String> = descs.iter().take(8).map(|d| format!("{}+{}:{:x}", d.Offset, d.Length, d.Status)).collect();
            say!("first ring IN descriptors (offset+length:status): {}", d.join(" "));
        }

        // ---- read capture ----
        counts_buf.clear();
        let in_pos_cb = in_pos;
        let base = unsafe { in_base.add(region_off) };
        for (j, d) in descs.iter().enumerate() {
            st.in_packets += 1;
            let mut frames = 0u32;
            if d.Status != 0 {
                bump(&mut st.in_pkt_status, d.Status);
            } else {
                frames = d.Length / fb_in as u32;
            }
            bump(&mut st.in_count_hist, frames);
            if !stable {
                consec = if frames > 0 { consec + 1 } else { 0 };
                if consec >= 800 {
                    stable = true;
                    say!("capture stable at input position {in_pos} (t = {:.2} s)", us(qpc() - t_start) / 1e6);
                }
            } else if frames == 0 {
                st.in_empty_after_stable += 1;
            }
            let off = (d.Offset as usize).min(packets * mbpi_in);
            let pkt = unsafe { std::slice::from_raw_parts(base.add(off), (frames as usize * fb_in).min(packets * mbpi_in - off)) };
            let micro_abs = if start_micro >= 0 { start_micro + j as i64 } else { -1 };
            for f in 0..frames as usize {
                let mut x = [0f32; OUT_CH];
                for c in 0..OUT_CH {
                    x[c] = get24(&pkt[f * fb_in + c * 3..]);
                }
                match cfg.signal {
                    Signal::Sine => sana.frame(in_pos, &x),
                    Signal::Click => {
                        if let Some(p) = clicks.input(in_pos, &x) {
                            while let Some(&(cp, _)) = click_out_micro.front() {
                                if cp < p {
                                    click_out_micro.pop_front();
                                } else {
                                    break;
                                }
                            }
                            if let Some(&(cp, om)) = click_out_micro.front() {
                                if cp == p && om >= 0 && micro_abs >= 0 {
                                    wire_lat.push(micro_abs - om);
                                    click_out_micro.pop_front();
                                }
                            }
                        }
                    }
                    Signal::Silence => {
                        if cfg.late_us.is_some() && x[0].abs() > 0.02 && in_pos > late_last_det + cfg.rate as u64 / 10 {
                            late_last_det = in_pos;
                            while late_inj.len() > 1 && micro_abs - late_inj[1] >= 0 {
                                late_inj.pop_front();
                            }
                            if let Some(&m) = late_inj.front() {
                                if micro_abs >= m && micro_abs - m < 160 {
                                    late_wire.push(micro_abs - m);
                                    late_inj.pop_front();
                                }
                            }
                        }
                    }
                }
                in_pos += 1;
            }
            st.in_frames += frames as u64;
            counts_buf.push(frames);
        }

        // ---- keep capture queued ----
        while in_q.len() < cfg.k_in {
            if !submit_in_ring!() {
                // Stream broken: restart with a lead-in at an explicit frame (if the lead-in region is free).
                if in_q.iter().any(|f| f.slot == usize::MAX) {
                    break;
                }
                let Some((cur, _)) = ifin.frame() else { break };
                if submit_in_lead!(cur + 2) {
                    st.in_restarts += 1;
                } else {
                    say!("cannot restart capture; abort");
                    abort = true;
                    break;
                }
            }
        }

        // ---- playback for this callback ----
        let mut written = 0u32;
        let mut first_write: Option<(usize, i64)> = None;
        for w in 0..packets / cfg.out_pk {
            // Bound the OUT queue (should not block in steady state).
            while out_q.len() >= k_out {
                let h = out_q.pop_front().unwrap();
                let o = if h.slot == usize::MAX { &out_lead_ov } else { &out_ov[h.slot] };
                if unsafe { WaitForSingleObject(o.event(), 100) } == WAIT_TIMEOUT {
                    st.out_wait_timeouts += 1;
                }
                if let Err(e) = ifout.overlapped_result(&o.ov, true) {
                    bump(&mut st.out_xfer_err, e);
                }
            }
            // Reap completed writes at the head without blocking.
            while let Some(h) = out_q.front() {
                let o = if h.slot == usize::MAX { &out_lead_ov } else { &out_ov[h.slot] };
                if unsafe { WaitForSingleObject(o.event(), 0) } != WAIT_OBJECT_0 {
                    break;
                }
                let r = ifout.overlapped_result(&o.ov, false);
                if h.start_micro >= 0 {
                    let k = (h.start_micro % 8) as usize;
                    if r.is_err() { st.err_by_micro[k] += 1 } else { st.ok_by_micro[k] += 1 }
                }
                if let Err(e) = r {
                    if st.out_xfer_err.values().sum::<u64>() < 5 {
                        say!(
                            "debug: OUT write error {e}: slot {} start_micro {} Internal(NTSTATUS) {:#x} InternalHigh {}",
                            h.slot, h.start_micro, o.ov.Internal, o.ov.InternalHigh
                        );
                    }
                    bump(&mut st.out_xfer_err, e);
                }
                out_q.pop_front();
            }
            if w == 0 {
                // OUT writes still pending on the bus: the real playback queue (WinUSB may pack a write
                // into fewer microframes than it books, see the results doc).
                pend_hist.add(out_q.len() as f64);
            }
            let counts: Vec<u32> = match cfg.fb {
                Feedback::Follow => counts_buf[w * cfg.out_pk..(w + 1) * cfg.out_pk]
                    .iter()
                    .map(|&c| if c == 0 { nominal(&mut nom_acc) } else { c })
                    .collect(),
                Feedback::Nominal => (0..cfg.out_pk).map(|_| nominal(&mut nom_acc)).collect(),
            };
            let s = out_slot_next;
            out_slot_next = (out_slot_next + 1) % k_out;
            let base_written = written;
            let mut click_offsets: Vec<u32> = Vec::new();
            let mut idx = 0u32;
            let n = {
                let mut gen = |fr: &mut [f32; OUT_CH]| {
                    let pos = in_pos_cb + (base_written + idx) as u64;
                    match cfg.signal {
                        Signal::Sine => *fr = [sine.next(); OUT_CH],
                        Signal::Click => {
                            let v = if stable { clicks.out(pos) } else { 0.0 };
                            if v != 0.0 {
                                click_offsets.push(idx);
                            }
                            *fr = [0.0; OUT_CH];
                            fr[0] = v;
                            fr[1] = v;
                        }
                        Signal::Silence => *fr = [0.0; OUT_CH],
                    }
                    idx += 1;
                };
                write_frames(s * out_slot_len, &counts, &mut gen, &mut st)
            };
            out_ov[s].reset();
            let t_sub = qpc();
            let mut start_micro = out_next_micro;
            match out_buf.write_asap((s * out_slot_len) as u32, (n * fb_out) as u32, true, &out_ov[s].ov) {
                Ok(()) => {
                    out_next_micro += cfg.out_pk as i64;
                }
                Err(e) => {
                    bump(&mut st.out_submit_err, e);
                    // Restart the playback stream ASAP (bus position unknown until the next lead-in).
                    out_ov[s].reset();
                    if out_buf.write_asap((s * out_slot_len) as u32, (n * fb_out) as u32, false, &out_ov[s].ov).is_ok() {
                        st.out_restarts += 1;
                        start_micro = -1;
                        out_next_micro = -1;
                    } else {
                        say!("cannot restart playback; abort");
                        abort = true;
                        break;
                    }
                }
            }
            if start_micro >= 0 {
                if let Some(t) = clock.qpc_of(start_micro) {
                    lead_hist.add(us(t - t_sub));
                }
            }
            for &k in &click_offsets {
                let m = if start_micro >= 0 { start_micro + (k / max_frames_out) as i64 } else { -1 };
                click_out_micro.push_back((in_pos_cb + (base_written + k) as u64, m));
            }
            if w == 0 {
                first_write = Some((s, start_micro));
            }
            out_q.push_back(InFlight { slot: s, packets: cfg.out_pk, start_micro });
            st.out_writes += 1;
            st.out_frames += n as u64;
            written += idx;
        }

        // ---- late-write probe: put a click into an already submitted write shortly before it goes out ----
        cb_count += 1;
        if let (Some(late), true, Some((s, m))) = (cfg.late_us, stable, first_write.take()) {
            if cb_count % 250 == 0 && m >= 0 {
                if let Some(t_bus) = clock.qpc_of(m) {
                    let target = t_bus - (late * freq / 1e6) as i64;
                    let spun_from = qpc();
                    while qpc() < target {
                        std::hint::spin_loop();
                    }
                    let t_w = qpc();
                    let p = unsafe { out_base.add(s * out_slot_len) };
                    let s24 = (0.1f32 * 8388607.0) as i32;
                    for c in 0..2usize {
                        for b in 0..3usize {
                            unsafe { std::ptr::write_volatile(p.add(c * 3 + b), (s24 >> (8 * b)) as u8) };
                        }
                    }
                    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
                    late_inj.push_back(m);
                    late_actual_us.push(us(t_bus - t_w));
                    let _ = spun_from;
                }
            }
        }

        if qpc() >= next_report {
            next_report += (30.0 * freq) as i64;
            let g: u64 = sana.chans.iter().map(|c| c.glitches).sum();
            say!(
                "  t={:4.0}s pkts {} status errs {} empty {} restarts in/out {}/{} out submit errs {} glitches {} lag max {:.0} us lead min {:.0} us",
                us(qpc() - t_start) / 1e6, st.in_packets, st.in_pkt_status.values().sum::<u64>(), st.in_empty_after_stable,
                st.in_restarts, st.out_restarts, st.out_submit_err.values().sum::<u64>(), g, lag_hist.max, lead_hist.min
            );
        }
    }
    let elapsed = us(qpc() - t_start) / 1e6;
    let cpu = cpu_time_s() - cpu0;

    // ---- stop: let all transfers complete (an abort can mute the outputs), then alt 0 ----
    while let Some(h) = in_q.pop_front() {
        let o = if h.slot == usize::MAX { &in_lead_ov } else { &in_ov[h.slot] };
        unsafe { WaitForSingleObject(o.event(), 500) };
        let _ = ifin.overlapped_result(&o.ov, false);
    }
    while let Some(h) = out_q.pop_front() {
        let o = if h.slot == usize::MAX { &out_lead_ov } else { &out_ov[h.slot] };
        unsafe { WaitForSingleObject(o.event(), 500) };
        let _ = ifout.overlapped_result(&o.ov, false);
    }
    if !mmcss.is_null() {
        unsafe { AvRevertMmThreadCharacteristics(mmcss) };
    }
    drop(in_buf);
    drop(out_buf);
    let _ = ifin.set_alt(0);
    let _ = ifout.set_alt(0);
    let _ = (&mut in_mem, &mut out_mem);

    // ---- report ----
    let ms_per_frame = 1000.0 / cfg.rate as f64;
    say!("--- result: {:.1} s, process CPU {:.2} s ({:.1}% of one core) ---", elapsed, cpu, cpu / elapsed * 100.0);
    say!(
        "IN : {} transfers, {} packets, {} frames ({:.3} frames/ms), packet status errors {:?}, transfer errors {:?}, submit errors {:?}, restarts {}, empty packets after stable {}, wait timeouts {}",
        st.in_transfers, st.in_packets, st.in_frames, st.in_frames as f64 / elapsed / 1000.0, st.in_pkt_status, st.in_xfer_err,
        st.in_submit_err, st.in_restarts, st.in_empty_after_stable, st.in_wait_timeouts
    );
    say!("IN packet sizes (frames: count): {:?}", st.in_count_hist);
    say!(
        "OUT: {} writes, {} frames (IN-OUT {}), submit errors {:?}, transfer errors {:?}, restarts {}, clamped packets {}, wait timeouts {}",
        st.out_writes, st.out_frames, st.in_frames as i64 - st.out_frames as i64, st.out_submit_err, st.out_xfer_err,
        st.out_restarts, st.out_clamped, st.out_wait_timeouts
    );
    say!("OUT completion by start microframe (mod 8): ok {:?} error {:?}", st.ok_by_micro, st.err_by_micro);
    say!("OUT writes pending at each callback (after reaping completed ones): {}", pend_hist.render("writes"));
    say!("callback lag (completion seen minus end of the IN transfer's last microframe): {}", lag_hist.render("us"));
    say!("OUT lead (bus time of the write's first microframe minus submit time): {}", lead_hist.render("us"));
    if cfg.early_probe {
        say!("early IN visibility (data in buffer minus end of its microframe): seen {early_seen}, not before completion {early_missed}: {}", early_hist.render("us"));
        let rel: Vec<String> = (0..8).map(|j| format!("p{j}:{:.0}", early_rel_sum[j] / early_rel_n[j].max(1) as f64)).collect();
        say!("early IN visibility relative to the transfer's completion event (mean us per packet index): {}", rel.join(" "));
    }
    let mut glitches = 0u64;
    let mut rtl = None;
    match cfg.signal {
        Signal::Sine => {
            for (ch, c) in sana.chans.iter().enumerate() {
                if c.peak > 0.005 {
                    say!("  in{}: peak {:.3}, glitches {}, max |e|/A {:.4}, first at input frames {:?}", ch + 1, c.peak, c.glitches, c.max_err_rel, c.first_glitches);
                }
            }
            glitches = sana.chans.iter().filter(|c| c.peak > 0.005).map(|c| c.glitches).sum();
            let live = sana.chans.iter().filter(|c| c.peak > 0.005).count();
            say!("sine analysis: {live} channels with signal, {glitches} glitch events in total");
        }
        Signal::Click => {
            say!("clicks emitted {}, never detected {}", clicks.emitted, clicks.missed);
            for (ch, v) in clicks.lat.iter().enumerate() {
                if let Some((mn, mean, mx)) = analyze::stats(v) {
                    say!(
                        "  in{}: {} detections, round trip min {} / mean {:.1} / max {} frames = {:.3} / {:.3} / {:.3} ms",
                        ch + 1, v.len(), mn, mean, mx, mn as f64 * ms_per_frame, mean * ms_per_frame, mx as f64 * ms_per_frame
                    );
                    if rtl.is_none() {
                        rtl = Some((mn, mean, mx));
                    }
                }
            }
            if let Some((mn, mean, mx)) = analyze::stats(&wire_lat) {
                say!("  device path (click's OUT packet microframe -> its capture packet microframe): min {mn} / mean {mean:.2} / max {mx} microframes = {:.3} ms mean", mean * 0.125);
            }
        }
        Signal::Silence => {
            if let Some(late) = cfg.late_us {
                let n = late_actual_us.len();
                let mean_lead = late_actual_us.iter().sum::<f64>() / n.max(1) as f64;
                let min_lead = late_actual_us.iter().copied().fold(f64::MAX, f64::min);
                say!(
                    "late write: {n} clicks written into submitted OUT buffers {late:.0} us before their booked microframe (actual lead mean {mean_lead:.0} / min {min_lead:.0} us); {} came back",
                    late_wire.len()
                );
                if let Some((mn, mean, mx)) = analyze::stats(&late_wire) {
                    say!("  booked OUT microframe -> capture microframe: min {mn} / mean {mean:.2} / max {mx} microframes");
                }
            }
        }
    }
    let dropouts = st.in_restarts + st.out_restarts + st.in_pkt_status.values().sum::<u64>() + st.in_empty_after_stable;
    Ok(Report { text: log, glitches, dropouts, rtl_frames: rtl })
}
