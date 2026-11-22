//! `calibrate`: find, per sample rate, the lowest-latency streaming setting that is stable on THIS
//! PC, using the UR44's internal LOOPBACK (Mix 1 -> computer inputs) so no cable is needed.
//!
//!   vboxusb-gate calibrate [--rates 48000,96000,192000] [--screen-secs 20] [--validate-secs 300]
//!                          [--margin 1] [--load] [--out FILE]
//!
//! For each rate it walks a ladder of settings from safe to aggressive (queue depth, late-write
//! lead, write delay). Each step is screened with a sine run; the walk stops at the first step
//! with any error, dropout or phase jump. The chosen setting is the last passing step minus
//! `--margin` steps (default 1: one step of headroom), and it must then pass a long validation
//! run (`--validate-secs`, optionally with every CPU core busy: `--load`). A failed validation
//! moves one step back and validates again. Finally a click run measures the round trip.
//!
//! "Stable" means: zero transfer errors, zero non-OK packets, the queue never near dry, zero
//! dropouts/slips in the sine and zero phase jumps over the validation time. That is evidence for
//! the tested duration and load, not a proof; use a longer --validate-secs for more confidence.
//! LOOPBACK is switched on at the start and restored to its previous value at the end, also after
//! Ctrl+C.

use crate::mixer::{Mixer, PARAM_LOOPBACK};
use crate::{arg, open_claimed, rate, stream, Mode, Opts, Report, ABORT, RATE_V};
use anyhow::{bail, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy)]
struct Step {
    out_depth: usize,
    min_q: usize,
    write_lead: usize,
    delay_us: f64,
    label: &'static str,
}

/// Safe -> aggressive. Latency falls with every step (measured on the development PC:
/// 8.9, 4.9, 3.9, 2.9, 2.7, 2.5, 2.3, 2.1 ms at 48 kHz over the analog cable).
const LADDER: &[Step] = &[
    Step { out_depth: 8, min_q: 4, write_lead: 0, delay_us: 0.0, label: "8 ms queue, write at submit" },
    Step { out_depth: 4, min_q: 3, write_lead: 0, delay_us: 0.0, label: "4 ms queue, write at submit" },
    Step { out_depth: 4, min_q: 3, write_lead: 2, delay_us: 0.0, label: "4 ms queue, late write ~2 ms ahead" },
    Step { out_depth: 4, min_q: 3, write_lead: 1, delay_us: 0.0, label: "4 ms queue, late write ~1 ms ahead" },
    Step { out_depth: 4, min_q: 3, write_lead: 1, delay_us: 200.0, label: "late write ~0.8 ms ahead" },
    Step { out_depth: 4, min_q: 3, write_lead: 1, delay_us: 400.0, label: "late write ~0.6 ms ahead" },
    Step { out_depth: 4, min_q: 3, write_lead: 1, delay_us: 600.0, label: "late write ~0.4 ms ahead" },
    Step { out_depth: 4, min_q: 3, write_lead: 1, delay_us: 800.0, label: "late write ~0.2 ms ahead" },
];

fn opts(step: &Step, mode: Mode, secs: f64, in_ch: usize, thr: f32) -> Opts {
    Opts {
        secs,
        mode,
        freq: 1000.0 * rate() as f64 / 48_000.0,
        amp: 0.5,
        in_depth: 8,
        out_depth: step.out_depth,
        in_ch,
        thr,
        write_lead: step.write_lead,
        min_q: step.min_q,
        write_delay_us: step.delay_us,
        mmcss: false,
        quiet: true,
    }
}

fn verdict(r: &Report, in_ch: usize) -> Result<(), String> {
    if let Some(s) = &r.stop {
        return Err(s.clone());
    }
    if !r.ok {
        return Err("drain error".into());
    }
    if r.ch_peak[in_ch] < 0.01 {
        return Err(format!("no signal on capture channel {} (loopback off?)", in_ch + 1));
    }
    if r.glitches > 0 {
        return Err(format!("{} dropout(s)/slip(s), worst error {:.0} % of the tone", r.glitches, r.max_err_rel * 100.0));
    }
    if r.phase_jumps > 0 {
        return Err(format!("{} phase jump(s)", r.phase_jumps));
    }
    Ok(())
}

/// Busy threads on every core for the duration of a run (`--load`).
struct Load {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}
impl Load {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let threads = (0..n)
            .map(|_| {
                let s = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 1u64;
                    while !s.load(Ordering::Relaxed) {
                        for _ in 0..10_000 {
                            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                        }
                        std::hint::black_box(x);
                    }
                })
            })
            .collect();
        Self { stop, threads }
    }
}
impl Drop for Load {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Switches LOOPBACK on and restores the previous value when dropped (also after Ctrl+C).
struct LoopbackGuard {
    orig: u32,
}
impl LoopbackGuard {
    fn on() -> Result<Self> {
        let d = open_claimed(false, true)?;
        let mut m = Mixer::new(&d);
        println!("UR44 firmware: {}", m.version()?);
        let orig = m.get(PARAM_LOOPBACK, 0)?;
        let now = m.set(PARAM_LOOPBACK, 0, 1)?;
        if now != 1 {
            bail!("could not switch LOOPBACK on (reads {now})");
        }
        println!("LOOPBACK on (was {orig})");
        Ok(Self { orig })
    }
}
impl Drop for LoopbackGuard {
    fn drop(&mut self) {
        let r = open_claimed(false, true).and_then(|d| {
            let mut m = Mixer::new(&d);
            m.version()?; // handshake: a fresh link can drop the first messages
            m.set(PARAM_LOOPBACK, 0, self.orig)
        });
        match r {
            Ok(v) => println!("LOOPBACK restored to {v}"),
            Err(e) => eprintln!("WARNING: could not restore LOOPBACK ({e:#}); run `vboxusb-gate loopback off`"),
        }
    }
}

struct RateResult {
    rate: u32,
    chosen: Option<(usize, Report)>, // ladder index, click report
    best_screen: Option<usize>,
    note: String,
}

fn aborted() -> bool {
    ABORT.load(Ordering::Relaxed)
}

pub fn run(args: &[String]) -> Result<()> {
    let rates: Vec<u32> = arg(args, "--rates", "48000,96000,192000".to_string())?
        .split(',')
        .map(|s| s.trim().parse())
        .collect::<Result<_, _>>()?;
    let screen: f64 = arg(args, "--screen-secs", 20.0)?;
    let validate: f64 = arg(args, "--validate-secs", 300.0)?;
    let margin: usize = arg(args, "--margin", 1)?;
    let load = args.iter().any(|a| a == "--load");
    let out_file: String = arg(args, "--out", default_out())?;
    for r in &rates {
        if ![44_100, 48_000, 88_200, 96_000, 176_400, 192_000].contains(r) {
            bail!("unsupported rate {r}");
        }
    }
    let est = rates.len() as f64 * (LADDER.len() as f64 * (screen + 2.0) + validate + 15.0) / 60.0;
    println!("calibration: rates {rates:?}, screen {screen} s/step, validate {validate} s{}, margin {margin} step(s); worst case ~{est:.0} min. Ctrl+C stops cleanly.",
        if load { " under full CPU load" } else { "" });

    let guard = LoopbackGuard::on()?;

    // Which capture channel carries the loopback: play a sine, take the loudest channel.
    RATE_V.store(rates[0], Ordering::Relaxed);
    let probe = stream(&opts(&LADDER[0], Mode::Sine, 3.0, 0, 0.0))?;
    if let Some(s) = &probe.stop {
        bail!("loopback probe run failed: {s}");
    }
    let (in_ch, pk) = probe.ch_peak.iter().enumerate().fold((0, 0f32), |a, (i, &p)| if p > a.1 { (i, p) } else { a });
    println!("capture peaks (dBFS): {:?}", probe.ch_peak.map(|p| (20.0 * (p.max(1e-9) as f64).log10()).round()));
    if pk < 0.01 {
        bail!("the loopback returns no signal on any capture channel");
    }
    println!("loopback returns on capture channel {} at {:.1} dBFS", in_ch + 1, 20.0 * (pk as f64).log10());
    let thr = (pk * 0.3).max(0.005);

    let mut results = Vec::new();
    for &r in &rates {
        if aborted() {
            break;
        }
        RATE_V.store(r, Ordering::Relaxed);
        println!("\n=== {} Hz", r);
        let mut res = RateResult { rate: r, chosen: None, best_screen: None, note: String::new() };
        // screening walk
        for (i, st) in LADDER.iter().enumerate() {
            if aborted() {
                break;
            }
            let rep = stream(&opts(st, Mode::Sine, screen, in_ch, 0.0))?;
            match verdict(&rep, in_ch) {
                Ok(()) => {
                    println!("  step {i} pass  ({}; wake gap max {:.2} ms{})", st.label, rep.max_gap_ms,
                        if st.write_lead > 0 { format!(", write margin min {:.2} ms", rep.write_margin_min) } else { String::new() });
                    res.best_screen = Some(i);
                }
                Err(e) => {
                    println!("  step {i} FAIL ({}): {e}", st.label);
                    break;
                }
            }
        }
        let Some(best) = res.best_screen else {
            res.note = "even the safe setting failed".into();
            results.push(res);
            continue;
        };
        // validation, backing off on failure
        let mut idx = best.saturating_sub(margin) as isize;
        while idx >= 0 && !aborted() {
            let st = &LADDER[idx as usize];
            println!("  validating step {idx} ({}) for {validate} s{} ...", st.label, if load { " under load" } else { "" });
            let rep = {
                let _l = load.then(Load::start);
                stream(&opts(st, Mode::Sine, validate, in_ch, 0.0))?
            };
            match verdict(&rep, in_ch) {
                Ok(()) => {
                    println!("  validated (wake gap max {:.2} ms, clock {:+.1} ppm)", rep.max_gap_ms, rep.ppm);
                    let click = stream(&opts(st, Mode::Click, 10.0, in_ch, thr))?;
                    if click.stop.is_some() || click.clicks_found == 0 || click.clicks_missed > 0 {
                        println!("  latency run failed: {:?}, {} found / {} missed", click.stop, click.clicks_found, click.clicks_missed);
                        idx -= 1;
                        continue;
                    }
                    println!("  round trip {:.2} ms median / {:.2} ms max (USB wire {:.3} ms)", click.app_rt_med, click.app_rt_max, click.wire_rt_med);
                    res.chosen = Some((idx as usize, click));
                    break;
                }
                Err(e) => {
                    println!("  validation FAILED: {e}; backing off one step");
                    idx -= 1;
                }
            }
        }
        if res.chosen.is_none() && res.note.is_empty() {
            res.note = if aborted() { "aborted".into() } else { "no setting passed validation".into() };
        }
        results.push(res);
    }

    drop(guard); // restore LOOPBACK before printing the summary

    println!("\n=== calibration result ({})", if aborted() { "ABORTED, partial" } else { "complete" });
    println!("{:>7}  {:<40} {:>10} {:>10} {:>9}", "rate", "setting", "RT median", "RT max", "headroom");
    let mut json = String::from("{\n  \"tool\": \"vboxusb-gate calibrate\",\n");
    json += &format!("  \"validate_secs\": {validate},\n  \"load\": {load},\n  \"margin_steps\": {margin},\n  \"loopback_channel\": {},\n  \"rates\": [\n", in_ch + 1);
    for (k, r) in results.iter().enumerate() {
        match &r.chosen {
            Some((i, c)) => {
                let st = &LADDER[*i];
                let head = r.best_screen.map(|b| b - i).unwrap_or(0);
                println!("{:>7}  {:<40} {:>8.2}ms {:>8.2}ms {:>6} st", r.rate, st.label, c.app_rt_med, c.app_rt_max, head);
                json += &format!(
                    "    {{\"rate\": {}, \"stable\": true, \"step\": {}, \"out_depth\": {}, \"min_queue\": {}, \"write_lead\": {}, \"write_delay_us\": {}, \"rt_median_ms\": {:.3}, \"rt_max_ms\": {:.3}, \"wire_ms\": {:.3}, \"headroom_steps\": {}}}",
                    r.rate, i, st.out_depth, st.min_q, st.write_lead, st.delay_us, c.app_rt_med, c.app_rt_max, c.wire_rt_med, head
                );
            }
            None => {
                println!("{:>7}  not stable: {}", r.rate, r.note);
                json += &format!("    {{\"rate\": {}, \"stable\": false, \"note\": \"{}\"}}", r.rate, r.note.replace('"', "'"));
            }
        }
        json += if k + 1 < results.len() { ",\n" } else { "\n" };
    }
    json += "  ]\n}\n";
    if let Some(dir) = std::path::Path::new(&out_file).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&out_file, json)?;
    println!("saved {out_file}");
    Ok(())
}

fn default_out() -> String {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    format!("{base}\\ur44-vboxgate\\calibration.json")
}
