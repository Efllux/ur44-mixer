//! Background device thread. Owns the MIDI connection, loads all parameters, applies
//! writes (coalesced so dragging a fader doesn't flood the UR44), keeps the meter stream
//! running and polls state that can change on the hardware itself.

use iced::futures::{SinkExt, Stream, channel::mpsc, executor::block_on};
use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};
use ur44::{Address, Message, Ur44, catalog, params};

#[derive(Debug, Clone)]
pub enum Event {
    /// The worker is running; commands go to this sender.
    Ready(std_mpsc::Sender<Command>),
    Connected { firmware: String },
    Loading { done: usize, total: usize },
    Values(Vec<(Address, i32)>),
    Value(Address, i32),
    Meters(Vec<u32>),
    /// About once a second while connected (used for saving state).
    Tick,
    Disconnected(String),
}

#[derive(Debug, Clone)]
pub enum Command {
    Set(Vec<(Address, i32)>),
    /// Meter stream on/off (off while no window is open).
    Meters(bool),
}

pub fn run() -> impl Stream<Item = Event> {
    iced::stream::channel(1024, |mut out: mpsc::Sender<Event>| async move {
        let (tx, rx) = std_mpsc::channel();
        let _ = out.send(Event::Ready(tx)).await;
        std::thread::spawn(move || worker(rx, out));
        std::future::pending::<()>().await;
    })
}

fn emit(out: &mut mpsc::Sender<Event>, e: Event) -> bool {
    block_on(out.send(e)).is_ok()
}

/// Parameters that can change without the app touching them.
fn polled() -> Vec<Address> {
    let mut v = Vec::new();
    for name in ["HaP48VOnf", "SysFsCode"] {
        let p = params::by_name(name).expect("known parameter");
        for ch in (0..p.channels).filter(|&c| catalog::is_present(p, c)) {
            v.push(Address { id: p.id, channel: ch });
        }
    }
    v
}

/// Hardware-status poll interval with a window open / in the background.
const POLL_ACTIVE: Duration = Duration::from_millis(1000);
const POLL_IDLE: Duration = Duration::from_secs(4);

fn worker(rx: std_mpsc::Receiver<Command>, mut out: mpsc::Sender<Event>) {
    let poll_list = polled();
    // Meters only stream while a window is open (the app sends Meters(true) then).
    let mut meters_on = false;
    loop {
        let mut dev = match Ur44::connect() {
            Ok(d) => d,
            Err(e) => {
                if !emit(&mut out, Event::Disconnected(e.to_string())) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
                for cmd in rx.try_iter() {
                    if let Command::Meters(on) = cmd {
                        meters_on = on; // remembered; writes while offline are dropped
                    }
                }
                continue;
            }
        };

        let firmware = dev
            .version()
            .map(|v| v.iter().map(|(k, ver)| format!("{k} {ver}")).collect::<Vec<_>>().join(" · "))
            .unwrap_or_default();
        eprintln!("ur44-mixer: connected ({firmware})");
        if !emit(&mut out, Event::Connected { firmware }) {
            return;
        }

        // Initial load of every visible parameter.
        let addrs: Vec<Address> = catalog::visible()
            .flat_map(|p| (0..p.channels).filter(move |&c| catalog::is_present(p, c)).map(move |c| Address { id: p.id, channel: c }))
            .collect();
        let total = addrs.len();
        let mut values = Vec::with_capacity(total);
        for (i, a) in addrs.iter().enumerate() {
            if let Ok(Some(v)) = dev.get(*a, Duration::from_millis(150)) {
                values.push((*a, v as i32));
            }
            if i % 50 == 0 && !emit(&mut out, Event::Loading { done: i, total }) {
                return;
            }
        }
        eprintln!("ur44-mixer: loaded {} of {} parameters", values.len(), total);
        if !emit(&mut out, Event::Values(values)) {
            return;
        }

        let _ = dev.send(&Message::Meter(meters_on));
        let mut last_meter_req = Instant::now();
        let mut last_poll = Instant::now();
        let mut last_meter_rx = Instant::now();
        let mut known: HashMap<Address, i32> = HashMap::new();
        let mut misses = 0;
        'connected: loop {
            // With meters on (window open) drain the 20 Hz meter stream promptly; otherwise sleep
            // until the next status poll — commands still wake us immediately.
            let poll_every = if meters_on { POLL_ACTIVE } else { POLL_IDLE };
            let wait = if meters_on {
                Duration::from_millis(10)
            } else {
                poll_every.saturating_sub(last_poll.elapsed()).max(Duration::from_millis(1))
            };
            match rx.recv_timeout(wait) {
                Ok(first) => {
                    // Coalesce: keep only the latest value per address, preserving order.
                    let mut pending: Vec<(Address, i32)> = Vec::new();
                    for cmd in std::iter::once(first).chain(rx.try_iter()) {
                        match cmd {
                            Command::Set(batch) => {
                                for (a, v) in batch {
                                    match pending.iter_mut().find(|(pa, _)| *pa == a) {
                                        Some(slot) => slot.1 = v,
                                        None => pending.push((a, v)),
                                    }
                                }
                            }
                            Command::Meters(on) => {
                                meters_on = on;
                                let _ = dev.send(&Message::Meter(on));
                            }
                        }
                    }
                    if pending.len() > 24 {
                        // Large batch (scene recall, Sweet Spot load): paced, read back, re-sent
                        // until the UR44 confirms every value; report what it actually holds.
                        match dev.write_verified(&pending, 3) {
                            Ok((actual, left)) => {
                                if left > 0 {
                                    eprintln!("ur44-mixer: {left} values not confirmed by the UR44");
                                }
                                if !emit(&mut out, Event::Values(actual)) {
                                    return;
                                }
                            }
                            Err(_) => break 'connected,
                        }
                    } else if dev.send_paced(&pending).is_err() {
                        break 'connected;
                    }
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = dev.send(&Message::Meter(false));
                    return; // app closed
                }
            }

            for raw in dev.drain() {
                match Message::decode(&raw) {
                    Message::ParamValue(a, v) => {
                        known.insert(a, v as i32);
                        if !emit(&mut out, Event::Value(a, v as i32)) {
                            return;
                        }
                    }
                    Message::MeterFrame(m) => {
                        last_meter_rx = Instant::now();
                        if !emit(&mut out, Event::Meters(m)) {
                            return;
                        }
                    }
                    _ => {}
                }
            }

            // Keep the meter stream alive.
            if meters_on && last_meter_req.elapsed() >= Duration::from_secs(3) {
                last_meter_req = Instant::now();
                let _ = dev.send(&Message::Meter(true));
            }

            if last_poll.elapsed() >= poll_every {
                last_poll = Instant::now();
                let mut answered = meters_on && last_meter_rx.elapsed() < Duration::from_secs(2);
                for a in &poll_list {
                    if let Ok(Some(v)) = dev.get(*a, Duration::from_millis(150)) {
                        answered = true;
                        let v = v as i32;
                        if known.insert(*a, v) != Some(v) && !emit(&mut out, Event::Value(*a, v)) {
                            return;
                        }
                    }
                }
                misses = if answered { 0 } else { misses + 1 };
                if !emit(&mut out, Event::Tick) {
                    return;
                }
                if misses >= 3 {
                    break 'connected;
                }
            }
        }
        if !emit(&mut out, Event::Disconnected("UR44 stopped responding".into())) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
