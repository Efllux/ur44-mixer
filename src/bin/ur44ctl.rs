//! ur44ctl — read-only command-line access to the UR44 mixer.
//!
//!   ur44ctl version
//!   ur44ctl get <id|name> [channel]
//!   ur44ctl set <name> <ch> <value>
//!   ur44ctl dump                                 # every known parameter, by name
//!   ur44ctl scan <from_id> <to_id> [channels]   # reads every id × channel, prints those that answer
//!   ur44ctl watch                                # prints every message the UR44 sends (turn knobs / use buttons)
//!   ur44ctl ports                                # lists MIDI ports and marks the UR44 control port

use anyhow::{Context, Result, bail};
use std::time::Duration;
use ur44::{Address, Message, Ur44, catalog, params};

fn num(s: Option<&String>, default: Option<u16>) -> Result<u16> {
    match s {
        Some(s) => s.parse().with_context(|| format!("not a number: {s}")),
        None => default.context("missing argument"),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    if cmd == "help" || cmd == "-h" || cmd == "--help" {
        println!("usage: ur44ctl version | get <id|name> [ch] | set <name> <ch> <value> | dump | scan <from> <to> [channels] | watch | ports");
        return Ok(());
    }
    if cmd == "ports" {
        // Diagnostics without touching the UR44: every MIDI port, and which one would be used.
        let (ins, outs) = ur44::device::midi_port_names();
        for (dir, names) in [("in ", ins), ("out", outs)] {
            if names.is_empty() {
                println!("{dir}  (no MIDI ports)");
            }
            for n in names {
                let mark = if ur44::device::is_control_port(&n) { "  <- UR44 control port" } else { "" };
                println!("{dir}  {n}{mark}");
            }
        }
        match ur44::device::usb_devices() {
            Ok(devs) if devs.is_empty() => println!("usb  (no Yamaha/Steinberg USB device visible to libusb)"),
            Ok(devs) => devs.iter().for_each(|d| println!("usb  {d}")),
            Err(e) => println!("usb  (libusb unavailable: {e})"),
        }
        return Ok(());
    }
    let mut dev = Ur44::connect()?;
    match cmd {
        "version" => {
            for (part, ver) in dev.version()? {
                println!("{part:<6} {ver}");
            }
        }
        "get" => {
            let key = args.get(1).context("usage: get <id|name> [ch]")?;
            let id = match key.parse::<u16>() {
                Ok(id) => id,
                Err(_) => params::by_name(key).with_context(|| format!("unknown parameter {key}"))?.id,
            };
            let addr = Address { id, channel: num(args.get(2), Some(0))? };
            let name = params::by_id(id).map(|p| p.name).unwrap_or("?");
            match dev.get(addr, Duration::from_millis(500))? {
                Some(v) => println!("{name} (id {}) ch {} = {}", addr.id, addr.channel, v as i32),
                None => println!("{name} (id {}) ch {}: no reply", addr.id, addr.channel),
            }
        }
        "set" => {
            let key = args.get(1).context("usage: set <name> <channel> <value>")?;
            let p = params::by_name(key).with_context(|| format!("unknown parameter {key}"))?;
            let channel = num(args.get(2), None)?;
            let value: i32 = args.get(3).context("missing value")?.parse().context("value must be a number")?;
            match catalog::access(p) {
                catalog::Access::ReadOnly => bail!("{} mirrors a hardware switch on the UR44 and can only be read", p.name),
                catalog::Access::Hidden => bail!("{} is not used on the UR44", p.name),
                catalog::Access::ReadWrite => {}
            }
            if !catalog::is_present(p, channel) {
                bail!("{} has channels 0..{}", p.name, p.channels - 1);
            }
            if value < p.min || value > p.max {
                bail!("{} accepts {}..{}", p.name, p.min, p.max);
            }
            let addr = Address { id: p.id, channel };
            let before = dev.get(addr, Duration::from_millis(500))?.map(|v| v as i32);
            let after = dev.set(addr, params::to_raw(value))? as i32;
            println!("{} ch {channel}: {} -> {after}", p.name, before.map_or("?".into(), |b| b.to_string()));
            if after != value {
                bail!("device reports {after}, expected {value}");
            }
        }
        "selftest-recall" => {
            // Non-destructive throughput test: read everything, write the same values back
            // at the mixer app's pacing, then read everything again and compare.
            let read_all = |dev: &mut Ur44| -> Result<ur44::logic::Values> {
                let mut v = ur44::logic::Values::new();
                for p in catalog::visible() {
                    for ch in (0..p.channels).filter(|&c| catalog::is_present(p, c)) {
                        let a = Address { id: p.id, channel: ch };
                        if let Some(x) = dev.get(a, Duration::from_millis(150))? {
                            v.insert(a, x as i32);
                        }
                    }
                }
                Ok(v)
            };
            let before = read_all(&mut dev)?;
            let writes = ur44::scene::recall(&ur44::scene::capture("selftest", &before));
            let t = std::time::Instant::now();
            for (i, (a, x)) in writes.iter().enumerate() {
                dev.send(&Message::ParamValue(*a, *x as u32))?;
                if i % 16 == 15 {
                    std::thread::sleep(Duration::from_millis(4));
                }
            }
            println!("sent {} writes in {:?}", writes.len(), t.elapsed());
            std::thread::sleep(Duration::from_millis(300));
            dev.drain();
            let after = read_all(&mut dev)?;
            let diffs: Vec<_> = before.iter().filter(|(a, x)| after.get(a) != Some(x)).collect();
            println!("read {} values before, {} after, {} differ", before.len(), after.len(), diffs.len());
            for (a, x) in diffs.iter().take(10) {
                println!("  {} ch {}: {} -> {:?}", params::by_id(a.id).map_or("?", |p| p.name), a.channel, x, after.get(a));
            }
        }
        "selftest-strip" => {
            // Uses channel strip 4 only if no input uses it (so nothing is audible),
            // loads a different Sweet Spot preset + morph, verifies, then restores.
            let s: u16 = 3;
            let mut cur = ur44::logic::Values::new();
            let mut touched: Vec<Address> = Vec::new();
            for p in params::PARAMS.iter() {
                let chans: Vec<u16> = match p.group {
                    params::Group::MorphTable => (s * 9..s * 9 + 9).collect(),
                    params::Group::ChannelStrip => vec![s],
                    params::Group::Input => (0..6).collect(),
                    _ => continue,
                };
                for ch in chans {
                    let a = Address { id: p.id, channel: ch };
                    if let Some(x) = dev.get(a, Duration::from_millis(150))? {
                        cur.insert(a, x as i32);
                        if p.group != params::Group::Input {
                            touched.push(a);
                        }
                    }
                }
            }
            if (0..6).any(|c| ur44::logic::insert_of(&cur, c).strip == s + 1) {
                bail!("strip 4 is in use on an input; not testing");
            }
            let presets = ur44::ssd::installed();
            let preset = presets.iter().find(|p| p.name.ends_with("08 MR Vocal")).context("preset not installed")?;
            let mut w = ur44::logic::load_sweet_spot(&cur, s, &preset.points);
            let mut after = cur.clone();
            for (a, x) in &w {
                after.insert(*a, *x);
            }
            w.extend(ur44::logic::edit(&after, "FxMorphValue", s, 52));
            let mut expect = cur.clone();
            for (a, x) in &w {
                expect.insert(*a, *x);
            }
            let (_, unconfirmed) = dev.write_verified(&w, 3)?;
            println!("  write_verified: {unconfirmed} unconfirmed");
            let mut bad = 0;
            for a in &touched {
                let got = dev.get(*a, Duration::from_millis(150))?.map(|x| x as i32);
                if got != expect.get(a).copied() {
                    bad += 1;
                    println!("    {} ch {}: expected {:?} got {:?}", params::by_id(a.id).map_or("?", |p| p.name), a.channel, expect.get(a), got);
                }
            }
            println!("loaded '{}' + morph 52 on strip 4: {} writes, {} mismatches", preset.name, w.len(), bad);
            let attack = expect[&ur44::logic::addr("FxCompAttack", s)];
            println!("  e.g. attack now {}", ur44::units::format("FxCompAttack", attack));
            // Restore
            let restore: Vec<(Address, i32)> = touched.iter().map(|a| (*a, cur[a])).collect();
            let (_, left) = dev.write_verified(&restore, 3)?;
            println!("  restore unconfirmed: {left}");
            let restored = touched.iter().filter(|a| dev.get(**a, Duration::from_millis(150)).ok().flatten().map(|x| x as i32) == cur.get(a).copied()).count();
            println!("restored {}/{} strip-4 values", restored, touched.len());
        }
        "test-insert" => {
            // 7 s channel strip on input 1 as Post patch, then 7 s as Pre patch, logging meters
            // (epoch ms) to stdout. Strip: heavy compression, total gain -12 dB. Restores everything.
            let ch: u16 = 0;
            let mut cur = ur44::logic::Values::new();
            for p in params::PARAMS.iter() {
                let chans: Vec<u16> = match p.group {
                    params::Group::ChannelStrip => (0..4).collect(),
                    params::Group::Input => (0..6).collect(),
                    params::Group::Patch | params::Group::System => vec![0],
                    _ => continue,
                };
                for c in chans {
                    let a = Address { id: p.id, channel: c };
                    if let Some(x) = dev.get(a, Duration::from_millis(150))? {
                        cur.insert(a, x as i32);
                    }
                }
            }
            let used: Vec<u16> = (1..6).map(|c| ur44::logic::insert_of(&cur, c).strip).filter(|s| *s > 0).collect();
            let strip = (1..=4u16).find(|s| !used.contains(s)).context("no free channel strip")?;
            let s = strip - 1;
            eprintln!("using channel strip {strip}");
            let a = ur44::logic::addr;
            let save: Vec<(Address, i32)> = [
                a("ChFxInsPrePatch", ch), a("ChFxInsPostPatch", ch), a("ChGtrFxInsPrePatch", ch), a("ChGtrFxInsPostPatch", ch),
                a("FxCompBypass", s), a("FxEqBypass", s), a("FxCompDrive", s), a("FxEqPeqGain", s),
            ]
            .iter()
            .map(|ad| (*ad, cur.get(ad).copied().unwrap_or(0)))
            .collect();
            let setup = vec![
                (a("ChGtrFxInsPrePatch", ch), 0), (a("ChGtrFxInsPostPatch", ch), 0), (a("ChFxInsPrePatch", ch), 0),
                (a("FxCompBypass", s), 0), (a("FxEqBypass", s), 0), (a("FxCompDrive", s), 200), (a("FxEqPeqGain", s), 60),
            ];
            dev.write_verified(&setup, 3)?;
            dev.send(&Message::Meter(true))?;
            let now_ms = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            let phases = [("POST", "ChFxInsPostPatch", "ChFxInsPrePatch"), ("PRE", "ChFxInsPrePatch", "ChFxInsPostPatch")];
            for (label, on, off) in phases {
                dev.send_paced(&[(a(off, ch), 0), (a(on, ch), i32::from(strip))])?;
                println!("PHASE\t{}\t{label}", now_ms());
                let t = std::time::Instant::now();
                let mut last_req = std::time::Instant::now();
                while t.elapsed() < Duration::from_secs(7) {
                    if let Some(raw) = dev.recv_timeout(Duration::from_millis(100)) {
                        if let Message::MeterFrame(m) = Message::decode(&raw) {
                            let pick: Vec<String> = [0usize, 1].iter().chain(&[20, 21, 22, 23, 24, 25, 26, 27]).chain(&[28 + 2 * s as usize, 29 + 2 * s as usize]).chain(&[36, 37, 38, 39, 40, 41, 42, 43]).map(|i| m[*i].to_string()).collect();
                            println!("M\t{}\t{}", now_ms(), pick.join("\t"));
                        }
                    }
                    if last_req.elapsed() > Duration::from_secs(3) {
                        dev.send(&Message::Meter(true))?;
                        last_req = std::time::Instant::now();
                    }
                }
            }
            dev.send(&Message::Meter(false))?;
            let (_, left) = dev.write_verified(&save, 3)?;
            println!("RESTORED\t{}\t{}", now_ms(), left);
        }
        "dump" => {
            println!("id\tname\tgroup\tch\twhere\tvalue\trange\tdefault\taccess");
            for p in catalog::visible() {
                for ch in (0..p.channels).filter(|&ch| catalog::is_present(p, ch)) {
                    let addr = Address { id: p.id, channel: ch };
                    let v = dev.get(addr, Duration::from_millis(150))?;
                    let shown = v.map(|v| (v as i32).to_string()).unwrap_or_else(|| "-".into());
                    let flag = match v { Some(v) if (v as i32) < p.min || (v as i32) > p.max => " (out of range)", _ => "" };
                    println!(
                        "{}\t{}\t{:?}\t{ch}\t{}\t{shown}{flag}\t{}..{}\t{}\t{:?}",
                        p.id, p.name, p.group, catalog::channel_label(p, ch), p.min, p.max, p.default, catalog::access(p)
                    );
                }
            }
        }
        "scan" => {
            let from = num(args.get(1), None)?;
            let to = num(args.get(2), None)?;
            let chans = num(args.get(3), Some(1))?;
            if to < from {
                bail!("<to> must be >= <from>");
            }
            println!("id\tch\tvalue\thex\tsigned");
            for id in from..=to {
                for ch in 0..chans {
                    let addr = Address { id, channel: ch };
                    if let Some(v) = dev.get(addr, Duration::from_millis(60))? {
                        println!("{id}\t{ch}\t{v}\t0x{v:X}\t{}", v as i32);
                    }
                }
            }
        }
        "watch" => {
            eprintln!("watching UR44 Port 2 — Ctrl-C to stop");
            loop {
                if let Some(raw) = dev.recv_timeout(Duration::from_secs(3600)) {
                    match Message::decode(&raw) {
                        Message::ParamValue(a, v) => println!("id {} ch {} = {v} (signed {})", a.id, a.channel, v as i32),
                        other => println!("{other:?}  [{}]", hex(&raw)),
                    }
                }
            }
        }
        other => bail!("unknown command {other}; try `ur44ctl help`"),
    }
    Ok(())
}
