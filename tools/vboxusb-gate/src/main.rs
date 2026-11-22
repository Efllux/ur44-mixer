//! vboxusb-gate: can the UR44 (Yamaha mode) stream playback through VBoxUSB.sys?
//!
//!   vboxusb-gate list                  VBoxUSB device interfaces
//!   vboxusb-gate probe                 open, claim, descriptors, set 48 kHz, read back; no streaming
//!   vboxusb-gate stream [options]      full duplex at 48 kHz, OUT sizes copied from IN (implicit fb)
//!     --rate HZ        44100/48000 (alt 1), 88200/96000 (alt 2), 176400/192000 (alt 3); default 48000
//!     --mode sine|click|silence   (default sine)
//!     --freq HZ        sine frequency (default 1000)
//!     --amp A          output amplitude, 0..1 (default 0.25 = -12 dBFS)
//!     --in-depth N     IN URBs (1 ms each) in flight, >= 4 (default 8)
//!     --out-depth N    OUT URBs (1 ms each) in flight, >= 4 (default 8)
//!     --in-ch N        capture channel to analyse, 0-based (default 0 = input 1)
//!     --thr T          click detection threshold (default 0.02)
//!     --min-queue N    stop when fewer than N URBs remain queued (default 4, floor 3)
//!     --mmcss          streaming thread in MMCSS "Pro Audio" at critical priority
//!     --write-delay-us D  late write: wait D us after the wakeup before writing (default 0)
//!     --write-lead W   late write: submit OUT URBs silent (final sizes), write the audio into
//!                      the URB at queue position W (1..depth-1) after each OUT completion. The
//!                      buffer is used in place by VBoxUSB (locked, not copied), so this tests
//!                      how late the controller still picks up data. Default 0 = write at submit.
//!
//! Hard safety rules (enforced here, not by the caller): every isochronous URB, IN and OUT, has
//! exactly 8 packets (one high-speed frame); 48 kHz only; at least 4 URBs (4 ms) must still be
//! queued on each pipe after every resubmission; the run stops at the first transfer error and at
//! the first packet whose status is not OK (late / not accessed / anything else).

#[cfg(not(windows))]
compile_error!("Windows only");

mod calib;
mod mixer;
mod sig;
mod vbox;

use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::ptr::null;
use vbox::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Performance::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::OVERLAPPED;

const EP_OUT: u8 = 0x07;
const EP_IN: u8 = 0x86;
const IF_OUT: u8 = 1;
const IF_IN: u8 = 2;
const CH: usize = 6;
const FB: usize = 18; // bytes per frame
// Stream format, set once at startup (--rate) and from the endpoint descriptor (packet size).
static RATE_V: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(48_000);
static MBPI_V: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(126);
fn rate() -> u32 {
    RATE_V.load(std::sync::atomic::Ordering::Relaxed)
}
fn mbpi() -> usize {
    MBPI_V.load(std::sync::atomic::Ordering::Relaxed)
}
/// UR44: alt 1 = 44.1/48 kHz, alt 2 = 88.2/96 kHz, alt 3 = 176.4/192 kHz.
fn alt() -> u8 {
    match rate() {
        0..=48_000 => 1,
        48_001..=96_000 => 2,
        _ => 3,
    }
}
/// Frames for a playback packet whose capture packet was empty.
fn nominal() -> u8 {
    (rate() / 8000) as u8
}
const MIN_DEPTH: usize = 3; // VBoxUSB schedules at max(now+2, next): fewer than 3 queued always re-anchors
const STABLE_URBS: u64 = 100; // 100 ms of non-empty capture before playback starts

fn qpc() -> i64 {
    let mut v = 0;
    unsafe { QueryPerformanceCounter(&mut v) };
    v
}
fn qpf() -> f64 {
    let mut v = 0;
    unsafe { QueryPerformanceFrequency(&mut v) };
    v as f64
}

fn get24(b: &[u8]) -> f32 {
    let v = (((b[2] as i32) << 24) | ((b[1] as i32) << 16) | ((b[0] as i32) << 8)) >> 8;
    v as f32 / 8_388_608.0
}
fn put24(d: &mut [u8], v: f32) {
    let s = (v.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
    d[0] = s as u8;
    d[1] = (s >> 8) as u8;
    d[2] = (s >> 16) as u8;
}

// ---------------------------------------------------------------- setup

struct EpInfo {
    ifc: u8,
    alt: u8,
    ep: u8,
    attr: u8,
    mps: u16,
    interval: u8,
}

fn config_endpoints(d: &Dev) -> Result<Vec<EpInfo>> {
    let mut hdr = [0u8; 9];
    d.control(0x80, 6, 0x0200, 0, &mut hdr).context("GET_DESCRIPTOR(config, 9)")?;
    let total = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
    let mut cfg = vec![0u8; total];
    let n = d.control(0x80, 6, 0x0200, 0, &mut cfg).context("GET_DESCRIPTOR(config)")?;
    let mut out = Vec::new();
    let (mut i, mut ifc, mut alt) = (0usize, 0u8, 0u8);
    while i + 2 <= n {
        let len = cfg[i] as usize;
        if len < 2 || i + len > n {
            break;
        }
        match cfg[i + 1] {
            4 if len >= 9 => {
                ifc = cfg[i + 2];
                alt = cfg[i + 3];
            }
            5 if len >= 7 => out.push(EpInfo {
                ifc,
                alt,
                ep: cfg[i + 2],
                attr: cfg[i + 3],
                mps: u16::from_le_bytes([cfg[i + 4], cfg[i + 5]]),
                interval: cfg[i + 6],
            }),
            _ => {}
        }
        i += len;
    }
    Ok(out)
}

fn set_rate(d: &Dev, ep: u8) -> Result<u32> {
    let mut v = [(rate() & 0xff) as u8, ((rate() >> 8) & 0xff) as u8, (rate() >> 16) as u8];
    d.control(0x22, 0x01, 0x0100, ep as u16, &mut v)?;
    let mut r = [0u8; 3];
    d.control(0xA2, 0x81, 0x0100, ep as u16, &mut r)?;
    Ok(r[0] as u32 | (r[1] as u32) << 8 | (r[2] as u32) << 16)
}

/// Open, claim and configure the captured UR44 (all interfaces at alt 0). Enough for the mixer.
pub fn open_claimed(verbose: bool, quiet: bool) -> Result<Dev> {
    let (mut d, path) = Dev::open_ur44()?;
    if verbose {
        println!("path={path}");
    }
    let (maj, min) = d.version()?;
    if !quiet {
        println!("vboxusb_version={maj}.{min}");
    }
    if maj != 5 {
        bail!("unexpected VBoxUSB interface version {maj}.{min}");
    }
    d.get_device()?;
    d.claim()?;
    d.set_config(1).context("SET_CONFIG(1)")?;
    Ok(d)
}

fn open_and_prepare(verbose: bool, quiet: bool) -> Result<Dev> {
    let d = open_claimed(verbose, quiet)?;
    let eps = config_endpoints(&d)?;
    let a = alt();
    let mut sizes = Vec::new();
    for (ifc, ep) in [(IF_OUT, EP_OUT), (IF_IN, EP_IN)] {
        let Some(e) = eps.iter().find(|e| e.ifc == ifc && e.alt == a && e.ep == ep) else {
            bail!("interface {ifc} alt {a} has no endpoint {ep:#04x}");
        };
        if !quiet {
            println!("desc if{} alt{} ep={:#04x} attr={:#04x} wMaxPacketSize={} bInterval={}", e.ifc, e.alt, e.ep, e.attr, e.mps, e.interval);
        }
        // one transaction per microframe, whole frames, room for nominal + 1 frames
        let mps = (e.mps & 0x7ff) as usize;
        if e.mps >> 11 != 0 || e.interval != 1 || e.attr & 3 != 1 || mps % FB != 0 || mps / FB <= nominal() as usize {
            bail!("endpoint {ep:#04x}: unexpected format for {} Hz", rate());
        }
        sizes.push(mps);
    }
    if sizes[0] != sizes[1] {
        bail!("IN and OUT packet sizes differ: {sizes:?}");
    }
    MBPI_V.store(sizes[0], std::sync::atomic::Ordering::Relaxed);
    // Capture first, then playback, each through alt 0 (as the macOS engine does).
    d.select_interface(IF_IN, 0)?;
    d.select_interface(IF_IN, alt())?;
    d.select_interface(IF_OUT, 0)?;
    d.select_interface(IF_OUT, alt())?;
    let mut last = None;
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50 << (attempt - 1)));
        }
        match (set_rate(&d, EP_IN), set_rate(&d, EP_OUT)) {
            (Ok(a), Ok(b)) if a == rate() && b == rate() => {
                if !quiet {
                    println!("rate_readback in={a} out={b}");
                }
                return Ok(d);
            }
            r => last = Some(format!("{:?} / {:?}", r.0.map_err(|e| e.to_string()), r.1.map_err(|e| e.to_string()))),
        }
    }
    bail!("could not set {} Hz: {}", rate(), last.unwrap_or_default())
}

fn idle_interfaces(d: &Dev) {
    let _ = d.select_interface(IF_OUT, 0);
    let _ = d.select_interface(IF_IN, 0);
}

// ---------------------------------------------------------------- streaming

struct Xfer {
    urb: Box<Urb>,
    data: Vec<u8>,
    ov: Box<OVERLAPPED>,
    seq: u64,
    submit_qpc: i64,
    in_done_at_submit: u64, // IN URBs completed when this OUT URB was submitted
    first_frame: u64,       // OUT: playback frame index of the first frame
    counts: [u8; 8],
    filled: bool,   // OUT: audio written (late-write mode writes after submission)
    write_qpc: i64, // OUT: when the audio was written
}

struct Pipe {
    name: &'static str,
    ep: u32,
    dir: u32,
    xs: Vec<Xfer>,
    pending: VecDeque<usize>,
    free: Vec<usize>,
    next_seq: u64,
    done_qpc: Vec<i64>,
    min_live: usize,
}

impl Pipe {
    fn new(name: &'static str, ep: u8, dir: u32, depth: usize, cap: usize) -> Self {
        let xs = (0..depth)
            .map(|_| {
                let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
                ov.hEvent = unsafe { CreateEventW(null(), 1, 0, null()) };
                Xfer {
                    urb: Box::new(Urb::zeroed()),
                    data: vec![0u8; PKTS * mbpi()],
                    ov,
                    seq: 0,
                    submit_qpc: 0,
                    in_done_at_submit: 0,
                    first_frame: 0,
                    counts: [0; 8],
                    filled: false,
                    write_qpc: 0,
                }
            })
            .collect();
        Self {
            name,
            ep: (ep & 0x0f) as u32,
            dir,
            xs,
            pending: VecDeque::new(),
            free: (0..depth).rev().collect(),
            next_seq: 0,
            // preallocated: a Vec doubling (2^18 entries = 262 s) stalls the thread ~1 ms
            done_qpc: Vec::with_capacity(cap),
            min_live: usize::MAX,
        }
    }

    /// Number of pending URBs whose IOCTL has not completed yet (= frames still scheduled).
    fn live(&self) -> usize {
        self.pending
            .iter()
            .filter(|&&i| unsafe { WaitForSingleObject(self.xs[i].ov.hEvent, 0) } != WAIT_OBJECT_0)
            .count()
    }

    /// Fill URB `i` with packet lengths (bytes) and submit it.
    fn submit(&mut self, d: &Dev, i: usize, lens: &[usize; 8]) -> Result<()> {
        let x = &mut self.xs[i];
        let mut u = Urb::zeroed();
        u.kind = TYPE_ISOC;
        u.ep = self.ep;
        u.dir = self.dir;
        u.flags = 1;
        let mut off = 0usize;
        for (k, &l) in lens.iter().enumerate() {
            if l > mbpi() || l % FB != 0 {
                bail!("bad packet length {l}");
            }
            u.pkts[k] = IsoPkt { len: l as u16, off: off as u16, status: 0 };
            off += l;
        }
        u.len = off as u64;
        u.buf = x.data.as_mut_ptr();
        u.npkts = PKTS as u32;
        *x.urb = u;
        x.seq = self.next_seq;
        self.next_seq += 1;
        x.submit_qpc = qpc();
        unsafe { d.submit_iso(&mut *x.urb, &mut *x.ov)? };
        self.pending.push_back(i);
        Ok(())
    }

    fn oldest_event(&self) -> Option<HANDLE> {
        self.pending.front().map(|&i| self.xs[i].ov.hEvent)
    }

    /// Complete the oldest URB: check every status. Returns its index.
    fn complete_oldest(&mut self, d: &Dev) -> Result<usize> {
        let i = self.pending.pop_front().unwrap();
        let seq = self.xs[i].seq;
        let r = unsafe { d.result(&mut *self.xs[i].ov) };
        r.with_context(|| format!("{} URB #{seq}", self.name))?;
        self.done_qpc.push(qpc());
        let u = *self.xs[i].urb;
        let err = { u.error };
        if err != 0 {
            bail!("{} URB #{seq}: error {err} ({})", self.name, status_name(err));
        }
        for k in 0..PKTS {
            let p = u.pkts[k];
            let st = { p.status };
            if st != 0 {
                bail!("{} URB #{seq} packet {k}: status {st} ({})", self.name, status_name(st));
            }
        }
        Ok(i)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Sine,
    Click,
    Silence,
}

struct Opts {
    secs: f64,
    mode: Mode,
    freq: f64,
    amp: f32,
    in_depth: usize,
    out_depth: usize,
    in_ch: usize,
    thr: f32,
    /// 0: audio written at submission. W >= 1: URBs are submitted silent (sizes final) and the
    /// audio is written into the URB at queue position W (0 = the one now on the wire) after each
    /// OUT completion, i.e. roughly W ms before it is sent. The queue depth is unchanged.
    write_lead: usize,
    /// stop when fewer than this many URBs are still queued after a resubmission (default 4)
    min_q: usize,
    /// late write: spin this long after the OUT completion wakeup before writing (probe the fetch point)
    write_delay_us: f64,
    /// register the streaming thread with MMCSS "Pro Audio" at critical priority
    mmcss: bool,
    /// suppress the per-run report (calibration prints its own summary)
    quiet: bool,
}

/// Outcome of one stream run, for the calibration.
#[derive(Default, Clone)]
pub struct Report {
    pub ok: bool,
    pub stop: Option<String>,
    pub glitches: u64,
    pub max_err_rel: f32,
    pub phase_jumps: usize,
    pub clicks_found: usize,
    pub clicks_missed: u64,
    pub app_rt_med: f64,
    pub app_rt_max: f64,
    pub wire_rt_med: f64,
    pub write_margin_min: f64,
    pub max_gap_ms: f64,
    pub ch_peak: [f32; CH],
    pub ppm: f64,
}

/// Set by Ctrl+C / console close: every run stops cleanly and the calibration restores LOOPBACK.
pub static ABORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct Click {
    out_seq: u64,
    submit_qpc: i64,
    in_floor: u64,
    found: bool,
}

fn stream(o: &Opts) -> Result<Report> {
    if o.min_q < MIN_DEPTH || o.in_depth < o.min_q || o.out_depth < o.min_q {
        bail!("queue depth below --min-queue {} (floor {MIN_DEPTH}) is not allowed", o.min_q);
    }
    let d = open_and_prepare(false, o.quiet)?;
    unsafe {
        SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        if o.mmcss {
            let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
            let mut idx = 0u32;
            let h = AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut idx);
            let ok = !h.is_null() && AvSetMmThreadPriority(h, AVRT_PRIORITY_CRITICAL) != 0;
            println!("mmcss Pro Audio critical: {}", if ok { "on" } else { "FAILED" });
        }
    }
    let f = qpf();
    let ms = |a: i64, b: i64| (b - a) as f64 * 1000.0 / f;

    let cap = (o.secs * 1000.0) as usize + 10_000;
    let mut pin = Pipe::new("IN", EP_IN, DIR_IN, o.in_depth, cap);
    let mut pout = Pipe::new("OUT", EP_OUT, DIR_OUT, o.out_depth, cap);
    let sine = sig::Sine::new(o.freq, rate() as f64, o.amp);
    let mut ana = sig::SineAnalyzer::new(o.freq, rate() as f64);

    let mut fifo: VecDeque<u8> = VecDeque::new(); // per-packet frame counts from IN
    let mut hist = [0u64; 32];
    let mut in_frames = 0u64; // capture frames received
    let mut in_urbs = 0u64;
    let mut empty_before = 0u64;
    let mut empty_after = 0u64;
    let mut stable = 0u64;
    let mut out_started = false;
    let mut out_frames = 0u64;
    let mut out_start_in_frame = 0u64;
    let mut stop_reason: Option<String> = None;
    let mut clicks: VecDeque<Click> = VecDeque::new();
    let mut click_results: Vec<(f64, f64, u64)> = Vec::new(); // (app rt ms, wire ms raw (w/o delta), click seq)
    let mut click_missed = 0u64;
    let mut max_gap_ms = 0f64;
    let mut write_margin: Vec<f64> = Vec::with_capacity(cap); // late write: ms from writing to completion
    let mut last_wake = 0i64;
    let mut ch_peak = [0f32; CH];

    let t0 = qpc();
    let end = t0 + (o.secs * f) as i64;
    let in_lens = [mbpi(); 8];
    while let Some(i) = pin.free.pop() {
        pin.submit(&d, i, &in_lens)?;
    }

    let result: Result<()> = (|| {
        loop {
            let mut evs: Vec<HANDLE> = Vec::new();
            if let Some(e) = pin.oldest_event() {
                evs.push(e);
            }
            if let Some(e) = pout.oldest_event() {
                evs.push(e);
            }
            if evs.is_empty() {
                break;
            }
            let w = unsafe { WaitForMultipleObjects(evs.len() as u32, evs.as_ptr(), 0, 100) };
            if w == WAIT_TIMEOUT {
                bail!("no completion for 100 ms (stalled)");
            }
            let now = qpc();
            if last_wake != 0 {
                max_gap_ms = max_gap_ms.max(ms(last_wake, now));
            }
            last_wake = now;
            let stopping = stop_reason.is_some() || now >= end || ABORT.load(std::sync::atomic::Ordering::Relaxed);
            let idx = (w - WAIT_OBJECT_0) as usize;
            let is_in = pin.oldest_event() == Some(evs[idx]);

            if is_in {
                let i = pin.complete_oldest(&d)?;
                let j = in_urbs;
                in_urbs += 1;
                let x = &pin.xs[i];
                let u = *x.urb;
                let mut all_nonempty = true;
                let mut counts = [0u8; 8];
                for k in 0..PKTS {
                    let p = u.pkts[k];
                    let cb = p.len as usize;
                    if cb % FB != 0 || cb > mbpi() {
                        bail!("IN URB #{j} packet {k}: length {cb} not a whole number of frames");
                    }
                    let n = cb / FB;
                    counts[k] = n as u8;
                    hist[n] += 1;
                    if n == 0 {
                        all_nonempty = false;
                        if out_started { empty_after += 1 } else { empty_before += 1 }
                    }
                    let base = p.off as usize;
                    let meter = out_started && !stopping && in_frames > out_start_in_frame + rate() as u64 / 2;
                    for fr in 0..n {
                        if meter {
                            for (ch, pk) in ch_peak.iter_mut().enumerate() {
                                let v = get24(&x.data[base + fr * FB + ch * 3..]).abs();
                                if v > *pk {
                                    *pk = v;
                                }
                            }
                        }
                        let s = get24(&x.data[base + fr * FB + o.in_ch * 3..]);
                        let pos = in_frames;
                        in_frames += 1;
                        // stop analysing once stopping: playback ends before capture does
                        if o.mode == Mode::Sine && !stopping {
                            ana.feed(pos, s);
                        }
                        if o.mode == Mode::Click {
                            if let Some(c) = clicks.front_mut() {
                                // IN URBs completed before the OUT URB was submitted cannot hold it.
                                if !c.found && s.abs() > o.thr && j >= c.in_floor {
                                    c.found = true;
                                    let app = ms(c.submit_qpc, now);
                                    // wire time in ms since IN URB 0 minus ms since OUT URB 0
                                    let tin = j as f64 + (k as f64 + fr as f64 / n as f64) / 8.0;
                                    click_results.push((app, tin - c.out_seq as f64, c.out_seq));
                                }
                            }
                        }
                    }
                }
                // retire clicks that were not found within 50 ms
                while let Some(c) = clicks.front() {
                    if c.found {
                        clicks.pop_front();
                    } else if ms(c.submit_qpc, now) > 50.0 {
                        click_missed += 1;
                        clicks.pop_front();
                    } else {
                        break;
                    }
                }
                for k in 0..PKTS {
                    fifo.push_back(if counts[k] == 0 { nominal() } else { counts[k] });
                }
                stable = if all_nonempty { stable + 1 } else { 0 };
                pin.free.push(i);
                if !stopping {
                    let live = pin.live();
                    pin.min_live = pin.min_live.min(live + 1);
                    if live + 1 < o.min_q && in_urbs > o.in_depth as u64 {
                        stop_reason = Some(format!("IN queue fell to {} ms (< {})", live + 1, o.min_q));
                    } else {
                        let i = pin.free.pop().unwrap();
                        pin.submit(&d, i, &in_lens)?;
                    }
                }
                // start playback after 100 ms of stable capture
                if !out_started && !stopping && stable >= STABLE_URBS {
                    let keep = (o.out_depth + 2) * PKTS;
                    while fifo.len() > keep {
                        fifo.pop_front();
                    }
                    out_started = true;
                    out_start_in_frame = in_frames;
                    if o.mode == Mode::Sine {
                        ana.start = in_frames + rate() as u64; // 1 s for the loop to settle
                    }
                    while let Some(i) = pout.free.pop() {
                        submit_out(&d, &mut pout, i, &mut fifo, &mut out_frames, o, &sine, in_urbs, &mut clicks, true)?;
                    }
                }
            } else {
                let i = pout.complete_oldest(&d)?;
                pout.free.push(i);
                if pout.xs[i].filled {
                    if o.write_lead > 0 {
                        write_margin.push(ms(pout.xs[i].write_qpc, now));
                    }
                } else if !stopping {
                    stop_reason = Some(format!("OUT URB #{} was sent before its audio was written", pout.xs[i].seq));
                }
                if stopping {
                    // no more late writes will come: write everything still queued now
                    let todo: Vec<usize> = pout.pending.iter().copied().filter(|&j| !pout.xs[j].filled).collect();
                    for j in todo {
                        fill_out(&mut pout, j, o, &sine, in_urbs, &mut clicks);
                    }
                }
                if !stopping {
                    let live = pout.live();
                    pout.min_live = pout.min_live.min(live + 1);
                    if live + 1 < o.min_q {
                        stop_reason = Some(format!("OUT queue fell to {} ms (< {})", live + 1, o.min_q));
                    } else if fifo.len() < PKTS {
                        stop_reason = Some("feedback FIFO empty".into());
                    } else {
                        let i = pout.free.pop().unwrap();
                        submit_out(&d, &mut pout, i, &mut fifo, &mut out_frames, o, &sine, in_urbs, &mut clicks, o.write_lead == 0)?;
                        // late write: the URB at queue position W gets its audio now
                        if o.write_lead > 0 {
                            if let Some(&j) = pout.pending.get(o.write_lead) {
                                if !pout.xs[j].filled {
                                    let until = now + (o.write_delay_us * f / 1e6) as i64;
                                    while qpc() < until {
                                        std::hint::spin_loop();
                                    }
                                    fill_out(&mut pout, j, o, &sine, in_urbs, &mut clicks);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    })();

    // Drain: never abort, let everything complete (an abort can mute the UR44 outputs).
    let mut drain_err = None;
    for p in [&mut pin, &mut pout] {
        while let Some(i) = p.pending.pop_front() {
            if let Err(e) = unsafe { d.result(&mut *p.xs[i].ov) } {
                drain_err.get_or_insert(e.to_string());
            }
        }
    }
    idle_interfaces(&d);
    let t1 = qpc();
    drop(d);

    // ---- report
    let mut rep = Report { max_gap_ms, ch_peak, ..Default::default() };
    let q = o.quiet;
    macro_rules! out {
        ($($t:tt)*) => { if !q { println!($($t)*); } };
    }
    let elapsed = ms(t0, t1) / 1000.0;
    let in_rate = if in_urbs > 0 { in_frames as f64 / in_urbs as f64 } else { 0.0 };
    rep.ppm = (in_rate / (rate() as f64 / 1000.0) - 1.0) * 1e6;
    out!("elapsed_s={elapsed:.3} in_urbs={in_urbs} out_urbs={} in_frames={in_frames} out_frames={out_frames}", pout.next_seq);
    let top = hist.iter().rposition(|&v| v > 0).unwrap_or(0);
    out!("in_hist_frames_per_packet 0..{top} = {:?}", &hist[..=top]);
    out!("frames_per_ms={in_rate:.4} ppm_vs_usb={:.1}", rep.ppm);
    out!("empty_in_packets before_out={empty_before} after_out={empty_after}");
    out!(
        "min_live_ms in={} out={} max_wake_gap_ms={max_gap_ms:.2} out_started={out_started} out_start_in_frame={out_start_in_frame}",
        if pin.min_live == usize::MAX { 0 } else { pin.min_live },
        if pout.min_live == usize::MAX { 0 } else { pout.min_live }
    );
    out!("capture_peaks_dbfs {:?}", ch_peak.map(|p| (20.0 * (p.max(1e-9) as f64).log10() * 10.0).round() / 10.0));
    // OUT frame index p and IN frame index q differ by a constant if no sample is lost.
    let mut deltas: Vec<f64> = pout
        .done_qpc
        .iter()
        .zip(pin.done_qpc.iter().skip(0))
        .map(|(o_, i_)| ms(*i_, *o_))
        .collect();
    let delta = sig::median(&mut deltas).map(|v| v.round());
    if let Some(dl) = delta {
        out!("start_frame_offset_out_minus_in_ms={dl} (median completion-time difference)");
    }
    if o.mode == Mode::Sine {
        rep.glitches = ana.glitches;
        rep.max_err_rel = ana.max_err_rel;
        out!("sine peak={:.4} ({:.1} dBFS) glitches={} max_err_rel={:.4} first_glitches={:?}",
            ana.peak, 20.0 * (ana.peak.max(1e-9) as f64).log10(), ana.glitches, ana.max_err_rel, ana.first_glitches);
        if let Some((span, drift, jumps, amin, amax)) = ana.phase_summary() {
            rep.phase_jumps = jumps;
            out!("phase blocks={} span_samples={span:.4} drift_first_to_last_samples={drift:.4} jumps_gt_2deg={jumps} amp_min={amin:.4} amp_max={amax:.4}", ana.phases.len());
        }
    }
    if o.mode == Mode::Click {
        let mut app: Vec<f64> = click_results.iter().map(|c| c.0).collect();
        let n = app.len();
        rep.clicks_found = n;
        rep.clicks_missed = click_missed;
        if n > 0 {
            let mn = app.iter().cloned().fold(f64::MAX, f64::min);
            let mx = app.iter().cloned().fold(f64::MIN, f64::max);
            let md = sig::median(&mut app).unwrap();
            rep.app_rt_med = md;
            rep.app_rt_max = mx;
            out!("clicks found={n} missed={click_missed} app_rt_ms min={mn:.3} median={md:.3} max={mx:.3}");
            if let Some(dl) = delta {
                let mut wire: Vec<f64> = click_results.iter().map(|c| c.1 - dl).collect();
                let wmn = wire.iter().cloned().fold(f64::MAX, f64::min);
                let wmx = wire.iter().cloned().fold(f64::MIN, f64::max);
                let wmd = sig::median(&mut wire).unwrap();
                rep.wire_rt_med = wmd;
                out!("wire_rt_ms (OUT microframe -> IN microframe) min={wmn:.3} median={wmd:.3} max={wmx:.3}");
            }
        } else {
            out!("clicks found=0 missed={click_missed}");
        }
    }
    if !write_margin.is_empty() {
        let mn = write_margin.iter().cloned().fold(f64::MAX, f64::min);
        let md = sig::median(&mut write_margin).unwrap();
        rep.write_margin_min = mn;
        out!("late_write lead_pos={} write_to_completion_ms min={mn:.3} median={md:.3} (completion ~= end of its frame)", o.write_lead);
    }
    if let Some(e) = &drain_err {
        out!("drain_error={e}");
    }
    match (&result, &stop_reason) {
        (Err(e), _) => {
            out!("STOP error: {e:#}");
            rep.stop = Some(format!("error: {e:#}"));
        }
        (Ok(()), Some(r)) => {
            out!("STOP safety: {r}");
            rep.stop = Some(format!("safety: {r}"));
        }
        _ if ABORT.load(std::sync::atomic::Ordering::Relaxed) => {
            out!("STOP aborted by user");
            rep.stop = Some("aborted by user".into());
        }
        _ => {
            out!("RESULT ok");
            rep.ok = drain_err.is_none();
        }
    }
    Ok(rep)
}

#[allow(clippy::too_many_arguments)]
fn submit_out(
    d: &Dev,
    p: &mut Pipe,
    i: usize,
    fifo: &mut VecDeque<u8>,
    out_frames: &mut u64,
    o: &Opts,
    sine: &sig::Sine,
    in_done: u64,
    clicks: &mut VecDeque<Click>,
    fill_now: bool,
) -> Result<()> {
    let mut lens = [0usize; 8];
    let seq = p.next_seq;
    {
        let x = &mut p.xs[i];
        x.first_frame = *out_frames;
        x.in_done_at_submit = in_done;
        x.seq = seq;
        x.filled = false;
        for k in 0..PKTS {
            let n = fifo.pop_front().unwrap_or(nominal()) as usize;
            x.counts[k] = n as u8;
            lens[k] = n * FB;
            *out_frames += n as u64;
        }
        x.data.fill(0);
    }
    if fill_now {
        fill_out(p, i, o, sine, in_done, clicks);
    }
    p.submit(d, i, &lens)
}

/// Write the audio of OUT URB `i` (sizes already fixed). Clicks are timed from this write.
fn fill_out(p: &mut Pipe, i: usize, o: &Opts, sine: &sig::Sine, in_done: u64, clicks: &mut VecDeque<Click>) {
    let x = &mut p.xs[i];
    let seq = x.seq;
    let click_here = o.mode == Mode::Click && seq >= 200 && seq % 250 == 0;
    let mut off = 0;
    let mut pos = x.first_frame;
    for k in 0..PKTS {
        let n = x.counts[k] as usize;
        for fr in 0..n {
            let v = match o.mode {
                Mode::Sine => sine.at(pos),
                Mode::Click => {
                    if click_here && k == 0 && fr == 0 { o.amp } else { 0.0 }
                }
                Mode::Silence => 0.0,
            };
            pos += 1;
            let fb = &mut x.data[off + fr * FB..off + (fr + 1) * FB];
            put24(&mut fb[0..3], v);
            put24(&mut fb[3..6], v);
        }
        off += n * FB;
    }
    x.filled = true;
    x.write_qpc = qpc();
    if click_here {
        clicks.push_back(Click { out_seq: seq, submit_qpc: x.write_qpc, in_floor: in_done, found: false });
    }
}

fn arg<T: std::str::FromStr>(args: &[String], name: &str, def: T) -> Result<T> {
    match args.iter().position(|a| a == name) {
        Some(i) => args
            .get(i + 1)
            .and_then(|v| v.parse().ok())
            .with_context(|| format!("bad value for {name}")),
        None => Ok(def),
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r: u32 = arg(&args, "--rate", 48_000)?;
    if ![44_100, 48_000, 88_200, 96_000, 176_400, 192_000].contains(&r) {
        bail!("unsupported rate {r}");
    }
    RATE_V.store(r, std::sync::atomic::Ordering::Relaxed);
    match args.first().map(|s| s.as_str()) {
        Some("list") => {
            for p in vbox::list_paths()? {
                println!("{p}");
            }
        }
        Some("probe") => {
            let d = open_and_prepare(true, false)?;
            idle_interfaces(&d);
            println!("RESULT ok");
        }
        Some("loopback") => {
            let d = open_claimed(false, true)?;
            let mut m = mixer::Mixer::new(&d);
            println!("firmware: {}", m.version()?);
            match args.get(1).map(|s| s.as_str()) {
                Some("on") => println!("loopback={}", m.set(mixer::PARAM_LOOPBACK, 0, 1)?),
                Some("off") => println!("loopback={}", m.set(mixer::PARAM_LOOPBACK, 0, 0)?),
                _ => println!("loopback={}", m.get(mixer::PARAM_LOOPBACK, 0)?),
            }
        }
        Some("calibrate") => {
            install_ctrl_handler();
            calib::run(&args)?;
        }
        Some("stream") => {
            let mode = match arg(&args, "--mode", "sine".to_string())?.as_str() {
                "sine" => Mode::Sine,
                "click" => Mode::Click,
                "silence" => Mode::Silence,
                m => bail!("unknown mode {m}"),
            };
            install_ctrl_handler();
            let o = Opts {
                secs: arg(&args, "--secs", 2.0)?,
                mode,
                // default tone scales with the rate so the detector keeps the same sensitivity
                freq: arg(&args, "--freq", 1000.0 * rate() as f64 / 48_000.0)?,
                amp: arg(&args, "--amp", if mode == Mode::Click { 0.5 } else { 0.25 })?,
                in_depth: arg(&args, "--in-depth", 8)?,
                out_depth: arg(&args, "--out-depth", 8)?,
                in_ch: arg(&args, "--in-ch", 0)?,
                thr: arg(&args, "--thr", 0.02)?,
                write_lead: arg(&args, "--write-lead", 0)?,
                min_q: arg(&args, "--min-queue", 4)?,
                write_delay_us: arg(&args, "--write-delay-us", 0.0)?,
                mmcss: args.iter().any(|a| a == "--mmcss"),
                quiet: false,
            };
            if o.write_lead >= o.out_depth {
                bail!("--write-lead must be below --out-depth");
            }
            if o.in_ch >= CH || o.amp > 0.9 || o.secs > 600.0 {
                bail!("option out of range");
            }
            println!(
                "stream rate={} mode={} secs={} freq={} amp={} in_depth={} out_depth={} in_ch={}",
                rate(),
                args.iter().position(|a| a == "--mode").map(|i| args[i + 1].as_str()).unwrap_or("sine"),
                o.secs, o.freq, o.amp, o.in_depth, o.out_depth, o.in_ch
            );
            if !stream(&o)?.ok {
                std::process::exit(2);
            }
        }
        _ => {
            eprintln!("usage: vboxusb-gate list | probe | loopback [on|off] | calibrate [options] | stream [options]  (see the header of main.rs)");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Ctrl+C / Ctrl+Break / console close: stop the current run cleanly (queued transfers drain,
/// the calibration restores LOOPBACK) instead of killing the process mid-stream.
fn install_ctrl_handler() {
    unsafe extern "system" fn handler(_t: u32) -> BOOL {
        ABORT.store(true, std::sync::atomic::Ordering::Relaxed);
        1
    }
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1);
    }
}
