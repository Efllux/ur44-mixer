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
