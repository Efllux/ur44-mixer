//! vboxusb-gate: can the UR44 (Yamaha mode) stream playback through VBoxUSB.sys?
//!
//!   vboxusb-gate list                  VBoxUSB device interfaces
//!   vboxusb-gate probe                 open, claim, descriptors, set 48 kHz, read back; no streaming
//!   vboxusb-gate stream [options]      full duplex at 48 kHz, OUT sizes copied from IN (implicit fb)
//!     --secs N         run time (default 2)
//!     --mode sine|click|silence   (default sine)
//!     --freq HZ        sine frequency (default 1000)
//!     --amp A          output amplitude, 0..1 (default 0.25 = -12 dBFS)
//!     --in-depth N     IN URBs (1 ms each) in flight, >= 4 (default 8)
//!     --out-depth N    OUT URBs (1 ms each) in flight, >= 4 (default 8)
//!     --in-ch N        capture channel to analyse, 0-based (default 0 = input 1)
//!     --thr T          click detection threshold (default 0.02)
//!
//! Hard safety rules (enforced here, not by the caller): every isochronous URB, IN and OUT, has
//! exactly 8 packets (one high-speed frame); 48 kHz only; at least 4 URBs (4 ms) must still be
//! queued on each pipe after every resubmission; the run stops at the first transfer error and at
//! the first packet whose status is not OK (late / not accessed / anything else).

#[cfg(not(windows))]
compile_error!("Windows only");

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

const RATE: u32 = 48_000;
const EP_OUT: u8 = 0x07;
const EP_IN: u8 = 0x86;
const IF_OUT: u8 = 1;
const IF_IN: u8 = 2;
const ALT: u8 = 1;
const CH: usize = 6;
const FB: usize = 18; // bytes per frame
const MBPI: usize = 126;
const MIN_DEPTH: usize = 4;
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
    let mut v = [(RATE & 0xff) as u8, ((RATE >> 8) & 0xff) as u8, (RATE >> 16) as u8];
    d.control(0x22, 0x01, 0x0100, ep as u16, &mut v)?;
    let mut r = [0u8; 3];
    d.control(0xA2, 0x81, 0x0100, ep as u16, &mut r)?;
    Ok(r[0] as u32 | (r[1] as u32) << 8 | (r[2] as u32) << 16)
}

fn open_and_prepare(verbose: bool) -> Result<Dev> {
    let (mut d, path) = Dev::open_ur44()?;
    if verbose {
        println!("path={path}");
    }
    let (maj, min) = d.version()?;
    println!("vboxusb_version={maj}.{min}");
    if maj != 5 {
        bail!("unexpected VBoxUSB interface version {maj}.{min}");
    }
    d.get_device()?;
    d.claim()?;
    d.set_config(1).context("SET_CONFIG(1)")?;
    let eps = config_endpoints(&d)?;
    for (ifc, ep) in [(IF_OUT, EP_OUT), (IF_IN, EP_IN)] {
        let Some(e) = eps.iter().find(|e| e.ifc == ifc && e.alt == ALT && e.ep == ep) else {
            bail!("interface {ifc} alt {ALT} has no endpoint {ep:#04x}");
        };
        println!("desc if{} alt{} ep={:#04x} attr={:#04x} wMaxPacketSize={} bInterval={}", e.ifc, e.alt, e.ep, e.attr, e.mps, e.interval);
        if e.mps as usize != MBPI || e.interval != 1 || e.attr & 3 != 1 {
            bail!("endpoint {ep:#04x} is not isochronous / 126 bytes / interval 1");
        }
    }
    // Capture first, then playback, each through alt 0 (as the macOS engine does).
    d.select_interface(IF_IN, 0)?;
    d.select_interface(IF_IN, ALT)?;
    d.select_interface(IF_OUT, 0)?;
    d.select_interface(IF_OUT, ALT)?;
    let mut last = None;
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50 << (attempt - 1)));
        }
        match (set_rate(&d, EP_IN), set_rate(&d, EP_OUT)) {
            (Ok(a), Ok(b)) if a == RATE && b == RATE => {
                println!("rate_readback in={a} out={b}");
                return Ok(d);
            }
            r => last = Some(format!("{:?} / {:?}", r.0.map_err(|e| e.to_string()), r.1.map_err(|e| e.to_string()))),
        }
    }
    bail!("could not set 48 kHz: {}", last.unwrap_or_default())
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
    fn new(name: &'static str, ep: u8, dir: u32, depth: usize) -> Self {
        let xs = (0..depth)
            .map(|_| {
                let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
                ov.hEvent = unsafe { CreateEventW(null(), 1, 0, null()) };
                Xfer {
                    urb: Box::new(Urb::zeroed()),
                    data: vec![0u8; PKTS * MBPI],
                    ov,
                    seq: 0,
                    submit_qpc: 0,
                    in_done_at_submit: 0,
                    first_frame: 0,
                    counts: [0; 8],
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
            done_qpc: Vec::new(),
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
            if l > MBPI || l % FB != 0 {
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
}

struct Click {
    out_seq: u64,
    submit_qpc: i64,
    in_floor: u64,
    found: bool,
}

fn stream(o: &Opts) -> Result<bool> {
    if o.in_depth < MIN_DEPTH || o.out_depth < MIN_DEPTH {
        bail!("depth below {MIN_DEPTH} is not allowed");
    }
    let d = open_and_prepare(false)?;
    unsafe {
        SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
    let f = qpf();
    let ms = |a: i64, b: i64| (b - a) as f64 * 1000.0 / f;

    let mut pin = Pipe::new("IN", EP_IN, DIR_IN, o.in_depth);
    let mut pout = Pipe::new("OUT", EP_OUT, DIR_OUT, o.out_depth);
    let sine = sig::Sine::new(o.freq, RATE as f64, o.amp);
    let mut ana = sig::SineAnalyzer::new(o.freq, RATE as f64);

    let mut fifo: VecDeque<u8> = VecDeque::new(); // per-packet frame counts from IN
    let mut hist = [0u64; 8];
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
    let mut last_wake = 0i64;

    let t0 = qpc();
    let end = t0 + (o.secs * f) as i64;
    let in_lens = [MBPI; 8];
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
            let stopping = stop_reason.is_some() || now >= end;
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
                    if cb % FB != 0 || cb > MBPI {
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
                    for fr in 0..n {
                        let s = get24(&x.data[base + fr * FB + o.in_ch * 3..]);
                        let pos = in_frames;
                        in_frames += 1;
                        if o.mode == Mode::Sine {
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
                    fifo.push_back(if counts[k] == 0 { 6 } else { counts[k] });
                }
                stable = if all_nonempty { stable + 1 } else { 0 };
                pin.free.push(i);
                if !stopping {
                    let live = pin.live();
                    pin.min_live = pin.min_live.min(live + 1);
                    if live + 1 < MIN_DEPTH && in_urbs > o.in_depth as u64 {
                        stop_reason = Some(format!("IN queue fell to {} ms (< {MIN_DEPTH})", live + 1));
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
                        ana.start = in_frames + RATE as u64; // 1 s for the loop to settle
                    }
                    while let Some(i) = pout.free.pop() {
                        submit_out(&d, &mut pout, i, &mut fifo, &mut out_frames, o, &sine, in_urbs, &mut clicks)?;
                    }
                }
            } else {
                let i = pout.complete_oldest(&d)?;
                pout.free.push(i);
                if !stopping {
                    let live = pout.live();
                    pout.min_live = pout.min_live.min(live + 1);
                    if live + 1 < MIN_DEPTH {
                        stop_reason = Some(format!("OUT queue fell to {} ms (< {MIN_DEPTH})", live + 1));
                    } else if fifo.len() < PKTS {
                        stop_reason = Some("feedback FIFO empty".into());
                    } else {
                        let i = pout.free.pop().unwrap();
                        submit_out(&d, &mut pout, i, &mut fifo, &mut out_frames, o, &sine, in_urbs, &mut clicks)?;
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
    let elapsed = ms(t0, t1) / 1000.0;
    let in_rate = if in_urbs > 0 { in_frames as f64 / in_urbs as f64 } else { 0.0 };
    println!("elapsed_s={elapsed:.3} in_urbs={in_urbs} out_urbs={} in_frames={in_frames} out_frames={out_frames}", pout.next_seq);
    println!("in_hist_frames_per_packet 0..7 = {:?}", hist);
    println!("frames_per_ms={in_rate:.4} ppm_vs_usb={:.1}", (in_rate / 48.0 - 1.0) * 1e6);
    println!("empty_in_packets before_out={empty_before} after_out={empty_after}");
    println!(
        "min_live_ms in={} out={} max_wake_gap_ms={max_gap_ms:.2} out_started={out_started} out_start_in_frame={out_start_in_frame}",
        if pin.min_live == usize::MAX { 0 } else { pin.min_live },
        if pout.min_live == usize::MAX { 0 } else { pout.min_live }
    );
    // OUT frame index p and IN frame index q differ by a constant if no sample is lost.
    let mut deltas: Vec<f64> = pout
        .done_qpc
        .iter()
        .zip(pin.done_qpc.iter().skip(0))
        .map(|(o_, i_)| ms(*i_, *o_))
        .collect();
    let delta = sig::median(&mut deltas).map(|v| v.round());
    if let Some(dl) = delta {
        println!("start_frame_offset_out_minus_in_ms={dl} (median completion-time difference)");
    }
    if o.mode == Mode::Sine {
        println!("sine peak={:.4} ({:.1} dBFS) glitches={} max_err_rel={:.4} first_glitches={:?}",
            ana.peak, 20.0 * (ana.peak.max(1e-9) as f64).log10(), ana.glitches, ana.max_err_rel, ana.first_glitches);
        if let Some((span, drift, jumps, amin, amax)) = ana.phase_summary() {
            println!("phase blocks={} span_samples={span:.4} drift_first_to_last_samples={drift:.4} jumps_gt_2deg={jumps} amp_min={amin:.4} amp_max={amax:.4}", ana.phases.len());
        }
    }
    if o.mode == Mode::Click {
        let mut app: Vec<f64> = click_results.iter().map(|c| c.0).collect();
        let n = app.len();
        if n > 0 {
            let mn = app.iter().cloned().fold(f64::MAX, f64::min);
            let mx = app.iter().cloned().fold(f64::MIN, f64::max);
            let md = sig::median(&mut app).unwrap();
            println!("clicks found={n} missed={click_missed} app_rt_ms min={mn:.3} median={md:.3} max={mx:.3}");
            if let Some(dl) = delta {
                let mut wire: Vec<f64> = click_results.iter().map(|c| c.1 - dl).collect();
                let wmn = wire.iter().cloned().fold(f64::MAX, f64::min);
                let wmx = wire.iter().cloned().fold(f64::MIN, f64::max);
                let wmd = sig::median(&mut wire).unwrap();
                println!("wire_rt_ms (OUT microframe -> IN microframe) min={wmn:.3} median={wmd:.3} max={wmx:.3}");
            }
        } else {
            println!("clicks found=0 missed={click_missed}");
        }
    }
    if let Some(e) = &drain_err {
        println!("drain_error={e}");
    }
    match (&result, &stop_reason) {
        (Err(e), _) => {
            println!("STOP error: {e:#}");
            Ok(false)
        }
        (Ok(()), Some(r)) => {
            println!("STOP safety: {r}");
            Ok(false)
        }
        _ => {
            println!("RESULT ok");
            Ok(drain_err.is_none())
        }
    }
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
) -> Result<()> {
    let mut lens = [0usize; 8];
    let seq = p.next_seq;
    let click_here = o.mode == Mode::Click && seq >= 200 && seq % 250 == 0;
    let x = &mut p.xs[i];
    x.first_frame = *out_frames;
    x.in_done_at_submit = in_done;
    let mut off = 0;
    for k in 0..PKTS {
        let n = fifo.pop_front().unwrap_or(6) as usize;
        x.counts[k] = n as u8;
        lens[k] = n * FB;
        for fr in 0..n {
            let pos = *out_frames;
            *out_frames += 1;
            let v = match o.mode {
                Mode::Sine => sine.at(pos),
                Mode::Click => {
                    if click_here && k == 0 && fr == 0 { o.amp } else { 0.0 }
                }
                Mode::Silence => 0.0,
            };
            let fb = &mut x.data[off + fr * FB..off + (fr + 1) * FB];
            fb.fill(0);
            put24(&mut fb[0..3], v);
            put24(&mut fb[3..6], v);
        }
        off += lens[k];
    }
    p.submit(d, i, &lens)?;
    if click_here {
        clicks.push_back(Click { out_seq: seq, submit_qpc: p.xs[i].submit_qpc, in_floor: in_done, found: false });
    }
    Ok(())
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
    match args.first().map(|s| s.as_str()) {
        Some("list") => {
            for p in vbox::list_paths()? {
                println!("{p}");
            }
        }
        Some("probe") => {
            let d = open_and_prepare(true)?;
            idle_interfaces(&d);
            println!("RESULT ok");
        }
        Some("stream") => {
            let mode = match arg(&args, "--mode", "sine".to_string())?.as_str() {
                "sine" => Mode::Sine,
                "click" => Mode::Click,
                "silence" => Mode::Silence,
                m => bail!("unknown mode {m}"),
            };
            let o = Opts {
                secs: arg(&args, "--secs", 2.0)?,
                mode,
                freq: arg(&args, "--freq", 1000.0)?,
                amp: arg(&args, "--amp", if mode == Mode::Click { 0.5 } else { 0.25 })?,
                in_depth: arg(&args, "--in-depth", 8)?,
                out_depth: arg(&args, "--out-depth", 8)?,
                in_ch: arg(&args, "--in-ch", 0)?,
                thr: arg(&args, "--thr", 0.02)?,
            };
            if o.in_ch >= CH || o.amp > 0.9 || o.secs > 600.0 {
                bail!("option out of range");
            }
            println!(
                "stream mode={} secs={} freq={} amp={} in_depth={} out_depth={} in_ch={}",
                args.iter().position(|a| a == "--mode").map(|i| args[i + 1].as_str()).unwrap_or("sine"),
                o.secs, o.freq, o.amp, o.in_depth, o.out_depth, o.in_ch
            );
            if !stream(&o)? {
                std::process::exit(2);
            }
        }
        _ => {
            eprintln!("usage: vboxusb-gate list | probe | stream [--secs N] [--mode sine|click|silence] [--freq HZ] [--amp A] [--in-depth N] [--out-depth N] [--in-ch N] [--thr T]");
            std::process::exit(1);
        }
    }
    Ok(())
}
