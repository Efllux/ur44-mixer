//! winusb-gate: feasibility test G2 for a user-mode (WinUSB) UR44 driver on Windows.
//!
//!   winusb-gate desc
//!       dump the configuration descriptor (read through the parent hub; works with no driver bound)
//!   winusb-gate stream [--rate 48000] [--in-pk 8] [--in 4] [--out-pk 1] [--d 24] [--secs 30]
//!                      [--signal sine|click|silence] [--fb follow|nominal] [--early-probe]
//!                      [--fixed-packets N] [--start-lead 3] [--no-mmcss] [--quiet] [--log FILE]
//!       duplex stream through WinUSB (interfaces 1/2 must be bound to WinUSB, see driver/):
//!       IN transfers of --in-pk packets (microframes), --in of them in flight; OUT writes of
//!       --out-pk packets (1 = every packet sized exactly); playback for capture microframe m
//!       goes out in microframe m + d
//!   winusb-gate probe-write [--rate 48000] [--fixed-packets N]
//!       single OUT writes of several lengths at explicit frames, and ASAP chains; frames used
//!   winusb-gate probe-chain [--dir in|out] [--packets 1] [--bytes N] [--n 64] [--depth 16]
//!       timestamps of each completion in a chain of small ASAP transfers
mod analyze;
mod desc;
#[cfg(windows)]
mod engine;
#[cfg(windows)]
mod winusb;

use anyhow::{anyhow, bail, Result};

const VID: u16 = 0x0499;
const PID: u16 = 0x150e;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("desc") | None => cmd_desc(),
        #[cfg(windows)]
        Some("stream") => cmd_stream(&args[1..]),
        #[cfg(windows)]
        Some("probe-write") => cmd_probe_write(&args[1..]),
        #[cfg(windows)]
        Some("probe-chain") => cmd_probe_chain(&args[1..]),
        #[cfg(windows)]
        Some("probe-reanchor") => cmd_probe_reanchor(&args[1..]),
        #[cfg(windows)]
        Some("policy") => cmd_policy(&args[1..]),
        #[cfg(windows)]
        Some("capture") => cmd_capture(&args[1..]),
        Some(other) => bail!("unknown command {other}"),
    }
}

struct Opts<'a>(&'a [String]);
impl Opts<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().position(|a| a == name).and_then(|i| self.0.get(i + 1)).map(|s| s.as_str())
    }
    fn flag(&self, name: &str) -> bool {
        self.0.iter().any(|a| a == name)
    }
    fn num<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T> {
        match self.get(name) {
            Some(v) => v.parse().map_err(|_| anyhow!("bad value for {name}: {v}")),
            None => Ok(default),
        }
    }
}

#[cfg(windows)]
fn formats_for(rate: u32) -> Result<(desc::AltFormat, desc::AltFormat)> {
    let f = desc::hub::find(VID, PID)?;
    let all = desc::formats(&f.config);
    let pick = |iface: u8| {
        all.iter().find(|a| a.iface == iface && a.rates.contains(&rate)).cloned().ok_or_else(|| anyhow!("no alt setting of interface {iface} offers {rate} Hz"))
    };
    Ok((pick(2)?, pick(1)?))
}

#[cfg(windows)]
fn cmd_stream(a: &[String]) -> Result<()> {
    let o = Opts(a);
    let cfg = engine::Config {
        rate: o.num("--rate", 48000)?,
        in_pk: o.num("--in-pk", 8)?,
        k_in: o.num("--in", 4)?,
        out_pk: o.num("--out-pk", 1)?,
        d: o.num("--d", 24)?,
        secs: o.num("--secs", 30.0)?,
        signal: match o.get("--signal").unwrap_or("sine") {
            "sine" => engine::Signal::Sine,
            "click" => engine::Signal::Click,
            "silence" => engine::Signal::Silence,
            s => bail!("unknown signal {s}"),
        },
        fb: match o.get("--fb").unwrap_or("follow") {
            "follow" => engine::Feedback::Follow,
            "nominal" => engine::Feedback::Nominal,
            s => bail!("unknown feedback mode {s}"),
        },
        early_probe: o.flag("--early-probe"),
        fixed_packets: o.get("--fixed-packets").map(|v| v.parse()).transpose()?,
        start_lead: o.num("--start-lead", 3)?,
        mmcss: !o.flag("--no-mmcss"),
        quiet: o.flag("--quiet"),
        late_us: o.get("--late-us").map(|v| v.parse()).transpose()?,
    };
    let (fin, fout) = formats_for(cfg.rate)?;
    let r = engine::run(&cfg, &fin, &fout)?;
    if cfg.quiet {
        // Only the result block.
        if let Some(i) = r.text.find("--- result") {
            print!("{}", &r.text[i..]);
        }
    }
    if let Some(path) = o.get("--log") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "\n=== winusb-gate {} ===\n{}", a.join(" "), r.text)?;
    }
    Ok(())
}

#[cfg(windows)]
fn cmd_probe_write(a: &[String]) -> Result<()> {
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    let o = Opts(a);
    let rate: u32 = o.num("--rate", 48000)?;
    let (_fin, fout) = formats_for(rate)?;
    let out = winusb::Iface::open(&winusb::GUID_MI01)?;
    out.set_alt(0)?;
    out.set_alt(fout.alt)?;
    println!("rate read back {}", out.set_rate(fout.ep, rate)?);
    let p = out.query_pipe(fout.alt, 0)?;
    let mbpi = p.MaximumBytesPerInterval;
    println!("OUT pipe 0x{:02x}: MaximumBytesPerInterval {mbpi}, Interval {}", p.PipeId, p.Interval);
    for (name, pol) in [("ISO_START_LATENCY", winusb::ISO_START_LATENCY), ("ISO_ALWAYS_START_ASAP", winusb::ISO_ALWAYS_START_ASAP), ("ISO_NUM_FIXED_PACKETS", winusb::ISO_NUM_FIXED_PACKETS)] {
        let g = out.get_pipe_policy(p.PipeId, pol);
        let sets: Vec<String> = [&[8u8][..], &[8, 0], &[8, 0, 0, 0]]
            .iter()
            .map(|v| format!("{}B:{:?}", v.len(), out.set_pipe_policy_raw(p.PipeId, pol, v)))
            .collect();
        println!("policy {name} (0x{pol:x}) on OUT pipe: get {g:?}; set value 8 as {}", sets.join(" "));
    }
    if let Some(n) = o.get("--fixed-packets") {
        let n: u32 = n.parse()?;
        match out.set_pipe_policy_u32(p.PipeId, winusb::ISO_NUM_FIXED_PACKETS, n) {
            Ok(()) => println!("ISO_NUM_FIXED_PACKETS = {n}: accepted"),
            Err(e) => println!("ISO_NUM_FIXED_PACKETS = {n}: {e:#}"),
        }
    }
    let mut mem = vec![0u8; (mbpi * 8 * 8) as usize];
    let buf = out.register(p.PipeId, &mut mem)?;
    let fb = fout.frame_bytes();
    let nominal_ms = rate / 1000 * fb;
    let lens = [
        fb * (rate / 8000), // one microframe of audio
        mbpi,               // one full packet
        mbpi + 1,
        nominal_ms - fb,
        nominal_ms,         // one ms of audio
        mbpi * 7,
        mbpi * 8,           // eight full packets
        mbpi * 8 + fb,
        nominal_ms * 2,
        mbpi * 16,
        mbpi * 8 * 8,
    ];
    let mut ov = winusb::Ov::new();
    for &len in &lens {
        let Some((cur, _)) = out.frame() else { bail!("no frame number") };
        let mut frame = cur + 4;
        let start = frame;
        ov.reset();
        let r = buf.write_at(0, len, &mut frame, &ov.ov);
        match r {
            Ok(()) => {
                unsafe { WaitForSingleObject(ov.event(), 1000) };
                let res = out.overlapped_result(&ov.ov, true);
                println!(
                    "write {len:5} bytes ({:6.2} max packets): start frame {start}, next frame returned {frame} => {} frame(s); completion {:?}",
                    len as f64 / mbpi as f64, frame.wrapping_sub(start), res
                );
            }
            Err(e) => println!("write {len:5} bytes: submit failed, error {e}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // ASAP chains: N writes of the same length, ContinueStream = TRUE after the first. Time the
    // completions: if each write takes a whole frame, N writes take N ms.
    let mut qf = 0i64;
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut qf) };
    let now = || {
        let mut t = 0i64;
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut t) };
        t
    };
    for &(len, n) in &[(fb * (rate / 8000), 32usize), (mbpi, 32), (nominal_ms, 32), (mbpi * 8, 32)] {
        let mut ovs: Vec<winusb::Ov> = (0..n).map(|_| winusb::Ov::new()).collect();
        let mut submit = Vec::new();
        let f0 = out.frame().map(|f| f.0).unwrap_or(0);
        let t0 = now();
        for (i, o) in ovs.iter_mut().enumerate() {
            o.reset();
            let off = ((i % 8) as u32) * mbpi * 8;
            submit.push(buf.write_asap(off, len, i > 0, &o.ov).err());
        }
        let mut done_ms = Vec::new();
        let mut errs = Vec::new();
        for o in &ovs {
            unsafe { WaitForSingleObject(o.event(), 2000) };
            done_ms.push((now() - t0) as f64 * 1000.0 / qf as f64);
            if let Err(e) = out.overlapped_result(&o.ov, true) {
                errs.push(e);
            }
        }
        let f1 = out.frame().map(|f| f.0).unwrap_or(0);
        let fails: Vec<u32> = submit.iter().flatten().copied().collect();
        let span = done_ms.last().unwrap() - done_ms.first().unwrap();
        println!(
            "ASAP chain of {n} writes x {len} bytes: submit failures {fails:?}, completion errors {errs:?}; first->last completion {span:.2} ms ({:.3} ms per write); bus frames from submit to end {}",
            span / (n - 1) as f64, f1.wrapping_sub(f0)
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    drop(buf);
    let _ = out.set_alt(0);
    Ok(())
}

/// Chains of small ASAP transfers (ContinueStream) in one direction; logs each completion's QPC
/// time and the bus microframe at that moment, to see whether consecutive small transfers occupy
/// consecutive microframes and whether completions arrive per microframe or batched per frame.
#[cfg(windows)]
fn cmd_probe_chain(a: &[String]) -> Result<()> {
    use windows_sys::Win32::Devices::Usb::USBD_ISO_PACKET_DESCRIPTOR;
    use windows_sys::Win32::System::Threading::*;
    let o = Opts(a);
    let rate: u32 = o.num("--rate", 48000)?;
    let dir = o.get("--dir").unwrap_or("out");
    let len_packets: u32 = o.num("--packets", 1)?; // packets per transfer
    let bytes: Option<u32> = o.get("--bytes").map(|v| v.parse()).transpose()?;
    let n: usize = o.num("--n", 64)?;
    let depth: usize = o.num("--depth", 16)?; // transfers in flight
    let nocont = o.flag("--nocont"); // ContinueStream = FALSE for every transfer
    let (fin, fout) = formats_for(rate)?;
    let (f, guid) = if dir == "in" { (fin, winusb::GUID_MI02) } else { (fout, winusb::GUID_MI01) };
    let ifc = winusb::Iface::open(&guid)?;
    ifc.set_alt(0)?;
    ifc.set_alt(f.alt)?;
    println!("rate read back {}", ifc.set_rate(f.ep, rate)?);
    let p = ifc.query_pipe(f.alt, 0)?;
    let mbpi = p.MaximumBytesPerInterval;
    let len = bytes.unwrap_or(if dir == "in" { mbpi * len_packets } else { f.frame_bytes() * (rate / 8000) * len_packets });
    let slot = mbpi * len_packets.max(1) * 8;
    let mut mem = vec![0u8; (slot as usize) * depth];
    let buf = ifc.register(p.PipeId, &mut mem)?;
    let track = ifc.start_time_sync()?;
    let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut task = 0u32;
    let h = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut task) };
    if !h.is_null() {
        unsafe { AvSetMmThreadPriority(h, AVRT_PRIORITY_CRITICAL) };
    }
    let mut ovs: Vec<winusb::Ov> = (0..depth).map(|_| winusb::Ov::new()).collect();
    let mut descs: Vec<Vec<USBD_ISO_PACKET_DESCRIPTOR>> =
        (0..depth).map(|_| vec![USBD_ISO_PACKET_DESCRIPTOR { Offset: 0, Length: 0, Status: 0 }; len_packets.max(1) as usize]).collect();
    let now = || {
        let mut t = 0i64;
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut t) };
        t
    };
    let mut qf = 0i64;
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut qf) };
    let mut submit = |i: usize, ovs: &mut Vec<winusb::Ov>, descs: &mut Vec<Vec<USBD_ISO_PACKET_DESCRIPTOR>>| -> Result<(), u32> {
        let s = i % depth;
        ovs[s].reset();
        let off = s as u32 * slot;
        let cont = i > 0 && !nocont;
        if dir == "in" {
            buf.read_asap(off, len, cont, &mut descs[s], &ovs[s].ov)
        } else {
            buf.write_asap(off, len, cont, &ovs[s].ov)
        }
    };
    let mut rows = Vec::new();
    let mut fails = Vec::new();
    let mut submitted = 0usize;
    while submitted < depth.min(n) {
        if let Err(e) = submit(submitted, &mut ovs, &mut descs) {
            fails.push((submitted, e));
        }
        submitted += 1;
    }
    for i in 0..n {
        let s = i % depth;
        // Spin for precise completion time.
        while unsafe { WaitForSingleObject(ovs[s].event(), 0) } == windows_sys::Win32::Foundation::WAIT_TIMEOUT {
            std::hint::spin_loop();
        }
        let t = now();
        let info = ifc.frame_qpc(track, 0, 0);
        let res = ifc.overlapped_result(&ovs[s].ov, true);
        let (uf, cur) = info.map(|x| (x.CurrentUSBFrameNumber as u64 * 8 + x.CurrentHardwareMicroFrameNumber as u64, x.CurrentQueryPerformanceCounter)).unwrap_or((0, 0));
        let pk: Vec<String> = if dir == "in" { descs[s].iter().map(|d| format!("{}:{:x}", d.Length, d.Status)).collect() } else { vec![] };
        rows.push((t, uf, cur, res, pk));
        if submitted < n {
            if let Err(e) = submit(submitted, &mut ovs, &mut descs) {
                fails.push((submitted, e));
            }
            submitted += 1;
        }
    }
    let t0 = rows[0].0;
    let u0 = rows[0].1;
    if o.flag("--summary") {
        let first_err = rows.iter().position(|r| r.3.is_err());
        let nerr = rows.iter().filter(|r| r.3.is_err()).count();
        let span = (rows.last().unwrap().0 - t0) as f64 * 1000.0 / qf as f64;
        println!(
            "{dir} chain of {n} x {len} bytes, depth {depth}: submit failures {}, completion errors {nerr} (first at #{first_err:?}, {:?}), span {span:.1} ms, bus microframes {}",
            fails.len(), first_err.map(|i| rows[i].3), rows.last().unwrap().1 as i64 - u0 as i64
        );
        if let Some(i) = first_err {
            let t = (rows[i].0 - t0) as f64 * 1000.0 / qf as f64;
            println!("  first error at t = {t:.3} ms, bus microframe +{}", rows[i].1 as i64 - u0 as i64);
        }
        drop(buf);
        let _ = ifc.set_alt(0);
        return Ok(());
    }
    println!("{dir} chain: {n} transfers of {len} bytes, {depth} in flight; submit failures {fails:?}");
    println!("  i   t_ms    dt_us  bus_uframe(+rel)  result  packets");
    let mut prev = t0;
    for (i, (t, uf, _cur, res, pk)) in rows.iter().enumerate() {
        println!(
            "{i:3} {:7.3} {:7.1} {:>8}  {:?} {}",
            (*t - t0) as f64 * 1000.0 / qf as f64,
            (*t - prev) as f64 * 1e6 / qf as f64,
            *uf as i64 - u0 as i64,
            res.map(|_| ()).map_err(|e| e),
            pk.join(" ")
        );
        prev = *t;
    }
    let span = (rows.last().unwrap().0 - t0) as f64 * 1000.0 / qf as f64;
    println!("first->last completion {span:.3} ms for {} transfers = {:.4} ms per transfer", n - 1, span / (n - 1) as f64);
    drop(buf);
    let _ = ifc.set_alt(0);
    if !h.is_null() {
        unsafe { AvRevertMmThreadCharacteristics(h) };
    }
    Ok(())
}

/// Chain of one-packet OUT writes (one microframe each). A plain ASAP chain dies after 1170
/// writes (WinUSB books a whole frame per write: N - N/8 = 1024). Here every `--every` writes
/// (a multiple of 8, so on a frame boundary) the write is an explicit-frame WinUsb_WriteIsochPipe
/// at the frame the chain has reached, which re-anchors the stack's booking.
#[cfg(windows)]
fn cmd_probe_reanchor(a: &[String]) -> Result<()> {
    use windows_sys::Win32::System::Threading::*;
    let o = Opts(a);
    let rate: u32 = o.num("--rate", 48000)?;
    let n: usize = o.num("--n", 40000)?;
    let every: usize = o.num("--every", 512)?;
    let depth: usize = o.num("--depth", 24)?;
    if every % 8 != 0 {
        bail!("--every must be a multiple of 8");
    }
    let (_fin, fout) = formats_for(rate)?;
    let ifc = winusb::Iface::open(&winusb::GUID_MI01)?;
    ifc.set_alt(0)?;
    ifc.set_alt(fout.alt)?;
    println!("rate read back {}", ifc.set_rate(fout.ep, rate)?);
    let p = ifc.query_pipe(fout.alt, 0)?;
    let mbpi = p.MaximumBytesPerInterval;
    let len = fout.frame_bytes() * (rate / 8000);
    let mut mem = vec![0u8; (mbpi as usize) * depth];
    let buf = ifc.register(p.PipeId, &mut mem)?;
    let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut task = 0u32;
    let h = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut task) };
    let mut ovs: Vec<winusb::Ov> = (0..depth).map(|_| winusb::Ov::new()).collect();
    let (f0, _) = ifc.frame().ok_or_else(|| anyhow!("no frame"))?;
    let f0 = f0 + 3;
    let mut submit_fail = std::collections::BTreeMap::<u32, u64>::new();
    let mut done_err = std::collections::BTreeMap::<u32, u64>::new();
    let mut first_err: Option<usize> = None;
    let mut submit = |i: usize, ovs: &mut Vec<winusb::Ov>| {
        let s = i % depth;
        ovs[s].reset();
        let off = s as u32 * mbpi;
        let r = if i % every == 0 {
            let mut fr = f0 + (i / 8) as u32;
            buf.write_at(off, len, &mut fr, &ovs[s].ov)
        } else {
            buf.write_asap(off, len, true, &ovs[s].ov)
        };
        if let Err(e) = r {
            *submit_fail.entry(e).or_insert(0) += 1;
        }
    };
    for i in 0..depth.min(n) {
        submit(i, &mut ovs);
    }
    let t0 = std::time::Instant::now();
    for i in 0..n {
        let s = i % depth;
        unsafe { WaitForSingleObject(ovs[s].event(), 1000) };
        if let Err(e) = ifc.overlapped_result(&ovs[s].ov, true) {
            *done_err.entry(e).or_insert(0) += 1;
            first_err.get_or_insert(i);
        }
        if i + depth < n {
            submit(i + depth, &mut ovs);
        }
    }
    let el = t0.elapsed().as_secs_f64();
    let (f1, _) = ifc.frame().unwrap_or((0, 0));
    println!(
        "{n} one-packet writes, explicit-frame re-anchor every {every}: {el:.3} s ({:.4} ms per write, ideal 0.125), bus frames {} (ideal {}), submit failures {submit_fail:?}, completion errors {done_err:?}, first error at #{first_err:?}",
        el * 1000.0 / n as f64, f1.wrapping_sub(f0), n / 8
    );
    drop(buf);
    let _ = ifc.set_alt(0);
    if !h.is_null() {
        unsafe { AvRevertMmThreadCharacteristics(h) };
    }
    Ok(())
}

/// Capture-only clock measurement at 48 kHz. IN reads of `--pk` packets (multiple of 8), `--in`
/// in flight (at least 8 ms), ASAP start (no explicit frame), ContinueStream. No OUT transfers.
/// Stops at the first non-zero packet status or transfer error (late / not accessed).
/// Reports the device's frames per ms against the USB frame clock and how often a host feeding a
/// constant 48 frames per ms would slip.
#[cfg(windows)]
fn cmd_capture(a: &[String]) -> Result<()> {
    use windows_sys::Win32::Devices::Usb::USBD_ISO_PACKET_DESCRIPTOR;
    use windows_sys::Win32::System::Threading::*;
    let o = Opts(a);
    let secs: f64 = o.num("--secs", 2.0)?;
    let pk: usize = o.num("--pk", 8)?;
    let k: usize = o.num("--in", 16)?;
    if pk == 0 || pk % 8 != 0 || pk * k < 64 {
        bail!("--pk must be a multiple of 8 and --pk x --in >= 64 packets (8 ms)");
    }
    let rate = 48000u32;
    let (fin, _) = formats_for(rate)?;
    let ifc = winusb::Iface::open(&winusb::GUID_MI02)?;
    ifc.set_alt(0)?;
    ifc.set_alt(fin.alt)?;
    let rb = ifc.set_rate(fin.ep, rate)?;
    if rb != rate {
        bail!("rate read back {rb}");
    }
    let p = ifc.query_pipe(fin.alt, 0)?;
    let mbpi = p.MaximumBytesPerInterval as usize;
    let slot = pk * mbpi;
    let mut mem = vec![0u8; slot * k];
    let buf = ifc.register(p.PipeId, &mut mem)?;
    let base = mem.as_ptr();
    let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut task = 0u32;
    let h = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut task) };
    if !h.is_null() {
        unsafe { AvSetMmThreadPriority(h, AVRT_PRIORITY_CRITICAL) };
    }
    let mut ovs: Vec<winusb::Ov> = (0..k).map(|_| winusb::Ov::new()).collect();
    let mut descs: Vec<Vec<USBD_ISO_PACKET_DESCRIPTOR>> = (0..k).map(|_| vec![USBD_ISO_PACKET_DESCRIPTOR { Offset: 0, Length: 0, Status: 0 }; pk]).collect();
    let f_start = ifc.frame().map(|f| f.0).unwrap_or(0);
    for s in 0..k {
        ovs[s].reset();
        if let Err(e) = buf.read_asap((s * slot) as u32, slot as u32, s > 0, &mut descs[s], &ovs[s].ov) {
            bail!("submit {s} failed: {e}");
        }
    }
    let freq = {
        let mut f = 0i64;
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut f) };
        f as f64
    };
    let qpc = || {
        let mut t = 0i64;
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut t) };
        t
    };
    let t0 = qpc();
    let mut hist = std::collections::BTreeMap::<u32, u64>::new();
    let (mut frames, mut packets, mut xfers) = (0u64, 0u64, 0u64);
    let mut stop: Option<String> = None;
    let mut warm = 0u64; // packets before the first non-empty one (stream start)
    let mut started = false;
    // Running sum of (frames - 6) per microframe from the first data packet = device-vs-USB-clock offset.
    let mut acc: i64 = 0;
    let (mut acc_min, mut acc_max) = (0i64, 0i64);
    let mut odd_pos: Vec<u64> = Vec::new(); // packet index (after start) of each non-6 packet
    let mut peak = [0f32; 6];
    let mut i = 0usize;
    while (qpc() - t0) as f64 / freq < secs {
        let s = i % k;
        if unsafe { WaitForSingleObject(ovs[s].event(), 200) } != windows_sys::Win32::Foundation::WAIT_OBJECT_0 {
            stop = Some("IN read did not complete within 200 ms".into());
            break;
        }
        if let Err(e) = ifc.overlapped_result(&ovs[s].ov, true) {
            stop = Some(format!("transfer error {e} at transfer {xfers}"));
            break;
        }
        xfers += 1;
        for (j, d) in descs[s].iter().enumerate() {
            packets += 1;
            if d.Status != 0 {
                stop = Some(format!("packet status {:#x} at transfer {xfers} packet {j}", d.Status as u32));
                break;
            }
            let n = d.Length / 18;
            if !started {
                if n == 0 {
                    warm += 1;
                    continue;
                }
                started = true;
            }
            *hist.entry(n).or_insert(0) += 1;
            frames += n as u64;
            acc += n as i64 - 6;
            acc_min = acc_min.min(acc);
            acc_max = acc_max.max(acc);
            if n != 6 {
                odd_pos.push(packets - warm);
            }
            let pkt = unsafe { std::slice::from_raw_parts(base.add(s * slot + d.Offset as usize), d.Length as usize) };
            for f in pkt.chunks_exact(18) {
                for c in 0..6 {
                    let v = ((f[c * 3] as u32) << 8 | (f[c * 3 + 1] as u32) << 16 | (f[c * 3 + 2] as u32) << 24) as i32 >> 8;
                    peak[c] = peak[c].max((v as f32 / 8388608.0).abs());
                }
            }
        }
        if stop.is_some() {
            break;
        }
        ovs[s].reset();
        if let Err(e) = buf.read_asap((s * slot) as u32, slot as u32, true, &mut descs[s], &ovs[s].ov) {
            stop = Some(format!("resubmit failed: error {e}"));
            break;
        }
        i += 1;
    }
    let el = (qpc() - t0) as f64 / freq;
    // Drain: wait for every pending read (all whole-frame) before unregistering.
    for s in 0..k {
        unsafe { WaitForSingleObject(ovs[s].event(), 500) };
        let _ = ifc.overlapped_result(&ovs[s].ov, false);
    }
    let f_end = ifc.frame().map(|f| f.0).unwrap_or(0);
    drop(buf);
    let _ = ifc.set_alt(0);
    if !h.is_null() {
        unsafe { AvRevertMmThreadCharacteristics(h) };
    }
    let data_pk = packets - warm;
    let ppm = if data_pk > 0 { (frames as f64 / (data_pk as f64 * 6.0) - 1.0) * 1e6 } else { 0.0 };
    println!("capture 48 kHz: {el:.2} s, {xfers} transfers x {pk} packets, {k} in flight ({} ms), bus frames {}", pk * k / 8, f_end.wrapping_sub(f_start));
    println!("  stop reason: {}", stop.as_deref().unwrap_or("none (ran to the end)"));
    println!("  start: {warm} empty packets before the first data; then {data_pk} packets, {frames} frames");
    println!("  packet sizes (frames: count): {hist:?}");
    println!("  device clock vs USB frame clock: {:.4} frames/ms = {ppm:+.1} ppm", frames as f64 / data_pk.max(1) as f64 * 8.0);
    println!("  running offset sum(frames - 6) range [{acc_min}, {acc_max}], final {acc}");
    let gaps: Vec<u64> = odd_pos.windows(2).map(|w| w[1] - w[0]).collect();
    if !gaps.is_empty() {
        let mean = gaps.iter().sum::<u64>() as f64 / gaps.len() as f64 / 8.0;
        println!(
            "  non-6 packets: {}; spacing min {:.1} / mean {mean:.1} / max {:.1} ms",
            odd_pos.len(), *gaps.iter().min().unwrap() as f64 / 8.0, *gaps.iter().max().unwrap() as f64 / 8.0
        );
    }
    if ppm.abs() > 0.0 && data_pk > 0 {
        println!("  a constant 48 frames/ms feed drifts by one frame every {:.2} s", 1.0 / (ppm.abs() * 1e-6 * 48000.0));
    }
    println!("  input peaks (ch1..6): {:?}", peak.iter().map(|p| format!("{p:.4}")).collect::<Vec<_>>());
    if let Some(path) = o.get("--log") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "\n=== winusb-gate capture {} ({}) === {el:.2} s, stop: {}, warm {warm}, packets {data_pk}, frames {frames}, hist {hist:?}, {ppm:+.1} ppm, offset [{acc_min},{acc_max}] final {acc}, non-6 {}",
            a.join(" "), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), stop.as_deref().unwrap_or("none"), odd_pos.len())?;
    }
    Ok(())
}

/// Pipe-policy probe only: Get/SetPipePolicy for the ISO policies on the 48 kHz OUT and IN pipes.
/// Submits no isoch transfer at all.
#[cfg(windows)]
fn cmd_policy(a: &[String]) -> Result<()> {
    let o = Opts(a);
    let rate: u32 = o.num("--rate", 48000)?;
    if rate != 48000 {
        bail!("48 kHz only on this PC");
    }
    let (fin, fout) = formats_for(rate)?;
    for (name, guid, f) in [("OUT", winusb::GUID_MI01, fout), ("IN", winusb::GUID_MI02, fin)] {
        let ifc = winusb::Iface::open(&guid)?;
        ifc.set_alt(0)?;
        ifc.set_alt(f.alt)?;
        let p = ifc.query_pipe(f.alt, 0)?;
        println!("{name} pipe 0x{:02x} alt {}: MaximumBytesPerInterval {}, Interval {}, wMaxPacketSize {}", p.PipeId, f.alt, p.MaximumBytesPerInterval, p.Interval, p.MaximumPacketSize);
        for (pn, pol) in [("ISO_START_LATENCY", winusb::ISO_START_LATENCY), ("ISO_ALWAYS_START_ASAP", winusb::ISO_ALWAYS_START_ASAP), ("ISO_NUM_FIXED_PACKETS", winusb::ISO_NUM_FIXED_PACKETS)] {
            let before = ifc.get_pipe_policy(p.PipeId, pol);
            let sets: Vec<String> = [&[8u8][..], &[8, 0], &[8, 0, 0, 0]]
                .iter()
                .map(|v| format!("{}B:{:?}", v.len(), ifc.set_pipe_policy_raw(p.PipeId, pol, v)))
                .collect();
            let after = ifc.get_pipe_policy(p.PipeId, pol);
            println!("  {pn} (0x{pol:x}): get before {before:?}; set 8 as {}; get after {after:?}", sets.join(" "));
        }
        // Well-known WinUSB policies for comparison (MAXIMUM_TRANSFER_SIZE 0x08, RAW_IO 0x07).
        println!("  MAXIMUM_TRANSFER_SIZE get {:?}, RAW_IO get {:?}", ifc.get_pipe_policy(p.PipeId, 0x08), ifc.get_pipe_policy(p.PipeId, 0x07));
        let _ = ifc.set_alt(0);
    }
    Ok(())
}

#[cfg(windows)]
fn cmd_desc() -> Result<()> {
    let f = desc::hub::find(VID, PID)?;
    let d = &f.device;
    println!("hub {} port {}", f.hub_path, f.port);
    println!(
        "DEVICE bcdUSB={:x}.{:02x} class 0x{:02x}/0x{:02x}/0x{:02x} bMaxPacketSize0={} VID {:04x} PID {:04x} bcdDevice {:x}.{:02x} configs={} speed={} ({})",
        d[3], d[2], d[4], d[5], d[6], d[7],
        u16::from_le_bytes([d[8], d[9]]), u16::from_le_bytes([d[10], d[11]]), d[13], d[12], d[17],
        f.speed, ["low", "full", "high", "super"].get(f.speed as usize).unwrap_or(&"?")
    );
    let hex: Vec<String> = f.config.iter().map(|b| format!("{b:02x}")).collect();
    println!("{}", desc::dump(&f.config));
    println!("raw ({} bytes):", f.config.len());
    for chunk in hex.chunks(32) {
        println!("  {}", chunk.join(" "));
    }
    println!("\nstreaming formats:");
    for a in desc::formats(&f.config) {
        let fb = a.frame_bytes();
        println!(
            "  if {} alt {} ep 0x{:02x}: {} ch x {} bytes ({} bits), rates {:?}, wMaxPacketSize {} (x{}), bInterval {}, frame {} bytes, max frames/packet {}",
            a.iface, a.alt, a.ep, a.channels, a.subframe, a.bits, a.rates, a.max_packet, a.mult as u32 + 1, a.interval, fb,
            if fb > 0 { a.max_packet as u32 / fb } else { 0 }
        );
        for &r in &a.rates {
            let per_uframe = r as f64 / 8000.0;
            println!(
                "      {r} Hz: nominal {per_uframe:.4} frames/microframe = {:.1} bytes; max packet holds {} frames",
                per_uframe * fb as f64,
                if fb > 0 { a.max_packet as u32 / fb } else { 0 }
            );
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_desc() -> Result<()> {
    bail!("Windows only")
}
