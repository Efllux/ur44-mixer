//! UR44 DSP mixer access through the captured device: Yamaha SysEx framed as USB-MIDI packets on
//! interface 4 (bulk OUT 0x03 / bulk IN 0x84). Same protocol as the main crate's `protocol.rs` and
//! `usbmidi.rs` (MIT, this repository), reduced to parameter get/set and the version request.
//!
//! While VBoxUSB owns the device the mixer app cannot reach interface 4, so the calibration
//! switches the internal LOOPBACK (parameter 100 `PatchLoopbackMode`) through this module.

use crate::vbox::Dev;
use anyhow::{bail, Result};
use std::time::{Duration, Instant};

const EP_OUT: u8 = 0x03;
const EP_IN: u8 = 0x84;
pub const PARAM_LOOPBACK: u16 = 100;

fn frame_sysex(sx: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sx.len().div_ceil(3) * 4);
    let n = sx.chunks(3).count();
    for (i, c) in sx.chunks(3).enumerate() {
        // CIN 0x4: SysEx starts/continues; 0x5/0x6/0x7: SysEx ends with 1/2/3 bytes
        out.push(if i + 1 < n { 0x4 } else { [0, 0x5, 0x6, 0x7][c.len()] });
        for k in 0..3 {
            out.push(*c.get(k).unwrap_or(&0));
        }
    }
    out
}

#[derive(Default)]
struct Deframer {
    cur: Vec<u8>,
}

impl Deframer {
    fn feed(&mut self, data: &[u8], out: &mut Vec<Vec<u8>>) {
        for p in data.chunks_exact(4) {
            let (n, end) = match p[0] & 0x0F {
                0x4 => (3, false),
                0x5 => (1, true),
                0x6 => (2, true),
                0x7 => (3, true),
                _ => continue,
            };
            if p[1] == 0xF0 {
                self.cur.clear();
            }
            self.cur.extend_from_slice(&p[1..1 + n]);
            if end {
                out.push(std::mem::take(&mut self.cur));
            }
        }
    }
}

fn enc7(v: u32, bytes: usize, out: &mut Vec<u8>) {
    for i in (0..bytes).rev() {
        out.push(((v >> (7 * i)) & 0x7F) as u8);
    }
}
fn dec7(b: &[u8]) -> u32 {
    b.iter().fold(0, |a, &x| (a << 7) | (x & 0x7F) as u32)
}

fn param_msg(class: u8, id: u16, ch: u16, value: Option<u32>) -> Vec<u8> {
    let mut m = vec![0xF0, 0x43, class, 0x3E, 0x14, 0x01, 0x01];
    enc7(id as u32, 3, &mut m);
    enc7(ch as u32, 3, &mut m);
    if let Some(v) = value {
        enc7(v, 5, &mut m);
    }
    m.push(0xF7);
    m
}

pub struct Mixer<'a> {
    d: &'a Dev,
    de: Deframer,
}

impl<'a> Mixer<'a> {
    pub fn new(d: &'a Dev) -> Self {
        Self { d, de: Deframer::default() }
    }

    fn send(&self, sx: &[u8]) -> Result<()> {
        let mut f = frame_sysex(sx);
        match self.d.bulk(EP_OUT, &mut f, 500)? {
            Some(_) => Ok(()),
            None => bail!("mixer interface: write timed out"),
        }
    }

    /// Read messages until `pred` matches one or `timeout` passes.
    fn wait<T>(&mut self, timeout: Duration, mut pred: impl FnMut(&[u8]) -> Option<T>) -> Result<Option<T>> {
        let end = Instant::now() + timeout;
        let mut buf = [0u8; 512];
        while Instant::now() < end {
            let left = end.saturating_duration_since(Instant::now()).as_millis().max(1) as u32;
            let Some(n) = self.d.bulk(EP_IN, &mut buf, left)? else { break };
            let mut msgs = Vec::new();
            self.de.feed(&buf[..n], &mut msgs);
            for m in msgs {
                if let Some(v) = pred(&m) {
                    return Ok(Some(v));
                }
            }
        }
        Ok(None)
    }

    pub fn version(&mut self) -> Result<String> {
        for _ in 0..5 {
            self.send(&[0xF0, 0x43, 0x30, 0x3E, 0x14, 0x00, 0x70, 0x02, 0xF7])?;
            let r = self.wait(Duration::from_millis(300), |m| {
                (m.len() > 9 && m[2] & 0xF0 == 0x10 && m[5..8] == [0x00, 0x70, 0x02]).then(|| {
                    m[9..m.len() - 1]
                        .chunks(8)
                        .map(|c| String::from_utf8_lossy(c).trim_end_matches(['\0', ' ']).to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            })?;
            if let Some(v) = r {
                return Ok(v);
            }
        }
        bail!("mixer interface: no version reply")
    }

    pub fn get(&mut self, id: u16, ch: u16) -> Result<u32> {
        for _ in 0..3 {
            self.send(&param_msg(0x30, id, ch, None))?;
            let r = self.wait(Duration::from_millis(300), |m| {
                (m.len() == 19 && m[2] & 0xF0 == 0x10 && m[5..7] == [0x01, 0x01]
                    && dec7(&m[7..10]) == id as u32 && dec7(&m[10..13]) == ch as u32)
                    .then(|| dec7(&m[13..18]))
            })?;
            if let Some(v) = r {
                return Ok(v);
            }
        }
        bail!("mixer interface: no reply for parameter {id}/{ch}")
    }

    /// Write and confirm by read-back; retries because a fresh link can drop the first messages.
    pub fn set(&mut self, id: u16, ch: u16, v: u32) -> Result<u32> {
        let mut got = None;
        for _ in 0..5 {
            self.send(&param_msg(0x10, id, ch, Some(v)))?;
            std::thread::sleep(Duration::from_millis(10));
            let r = self.get(id, ch)?;
            if r == v {
                return Ok(r);
            }
            got = Some(r);
        }
        bail!("parameter {id}/{ch}: wrote {v}, device reports {got:?}")
    }
}
