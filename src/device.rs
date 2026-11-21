//! Connection to the UR44's DSP mixer. Two links carry the same SysEx messages:
//!   - CC mode: the class-compliant MIDI "Port 2" (Apple's MIDI driver), via CoreMIDI;
//!   - Yamaha mode (CC MODE off, our own audio driver): USB interface 4 directly (bulk 0x03 out /
//!     0x84 in, USB-MIDI framing). Interfaces 1/2 (audio) belong to the Core Audio plug-in.

use crate::protocol::{Address, Message};
use crate::usbmidi;
use anyhow::{Context, Result, anyhow, bail};
use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

const PORT_MATCH: &str = "UR44";
const CONTROL_PORT_SUFFIX: &str = "Port 2";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(150);

pub struct Ur44 {
    link: Link,
    rx: Receiver<Vec<u8>>,
}

enum Link {
    Midi { out: MidiOutputConnection, _in: MidiInputConnection<()> },
    Usb(UsbLink),
}

/// Which link a connection uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    MidiPort2,
    UsbInterface4,
}

const VID: u16 = 0x0499;
const PID: u16 = 0x150e;
const MIXER_IF: u8 = 4;
const EP_OUT: u8 = 0x03;
const EP_IN: u8 = 0x84;

struct UsbLink {
    handle: Arc<rusb::DeviceHandle<rusb::Context>>,
    stop: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Drop for UsbLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the event loop so it cancels the outstanding read before releasing interface 4.
        use rusb::UsbContext;
        self.handle.context().interrupt_handle_events();
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
        let _ = self.handle.release_interface(MIXER_IF);
    }
}

impl UsbLink {
    /// Open interface 4 of a UR44 in Yamaha mode (device class 255).
    fn open(tx: Sender<Vec<u8>>) -> Result<Self> {
        use rusb::UsbContext;
        let ctx = rusb::Context::new()?;
        let dev = ctx
            .devices()?
            .iter()
            .find(|d| {
                d.device_descriptor()
                    .map(|dd| dd.vendor_id() == VID && dd.product_id() == PID && dd.class_code() == 0xFF)
                    .unwrap_or(false)
            })
            .context("UR44 in Yamaha mode not found")?;
        // Windows: libusb can only open interfaces bound to WinUSB (or libusbK/UsbDk). Binding
        // one would replace Yamaha's driver, so this normally fails there: say what to do instead.
        #[cfg(windows)]
        const HOW: &str = " (Windows: direct USB access to the mixer needs a WinUSB driver, which \
            would replace Yamaha's driver. Switch CC MODE on and power-cycle the UR44 to use MIDI instead)";
        #[cfg(not(windows))]
        const HOW: &str = "";
        let h = dev.open().with_context(|| format!("opening the UR44{HOW}"))?;
        if h.active_configuration().unwrap_or(0) == 0 {
            // Normally the audio plug-in configures the device; do it if nothing has.
            let _ = h.set_active_configuration(1);
        }
        h.claim_interface(MIXER_IF).with_context(|| format!("claiming the mixer interface (4){HOW}"))?;
        let handle = Arc::new(h);
        let stop = Arc::new(AtomicBool::new(false));
        let (h2, stop2) = (handle.clone(), stop.clone());
        let reader = std::thread::Builder::new().name("ur44-usb-mixer".into()).spawn(move || {
            usb_reader(h2, stop2, tx);
        })?;
        Ok(UsbLink { handle, stop, reader: Some(reader) })
    }

    fn send(&self, sx: &[u8]) -> Result<()> {
        let framed = usbmidi::frame_sysex(sx);
        self.handle.write_bulk(EP_OUT, &framed, Duration::from_millis(500)).map(|_| ()).map_err(|e| anyhow!("{e}"))
    }
}

// libusb's synchronous read is an async transfer plus an event loop. Keep that read pending
// without a timeout, and explicitly cancel it on drop instead of waking twice every 200 ms.
fn usb_reader(handle: Arc<rusb::DeviceHandle<rusb::Context>>, stop: Arc<AtomicBool>, tx: Sender<Vec<u8>>) {
    use rusb::{UsbContext, ffi};
    struct Completion {
        done: AtomicBool,
        context: rusb::Context,
    }
    extern "system" fn complete(transfer: *mut ffi::libusb_transfer) {
        // SAFETY: user_data points to the boxed Completion below until the submitted transfer
        // has completed. A synchronous writer may dispatch this callback on another thread.
        let state = unsafe { &*((*transfer).user_data.cast::<Completion>()) };
        let context = state.context.clone();
        // Do not access state after publishing completion: the reader may free it immediately.
        state.done.store(true, Ordering::Release);
        context.interrupt_handle_events();
    }

    let state = Box::new(Completion { done: AtomicBool::new(false), context: handle.context().clone() });
    let mut buf = [0u8; 512];
    let mut deframer = usbmidi::Deframer::default();
    // SAFETY: the handle, buffer and boxed completion stay alive until the transfer's final
    // callback, including after cancellation. Only this thread submits/cancels/frees it.
    unsafe {
        let transfer = ffi::libusb_alloc_transfer(0);
        if transfer.is_null() {
            return;
        }
        ffi::libusb_fill_bulk_transfer(
            transfer, handle.as_raw(), EP_IN, buf.as_mut_ptr(), buf.len() as i32,
            complete, (&*state as *const Completion).cast_mut().cast(), 0,
        );
        while !stop.load(Ordering::Acquire) {
            state.done.store(false, Ordering::Relaxed);
            if ffi::libusb_submit_transfer(transfer) != 0 {
                break;
            }
            let mut cancelled = false;
            while !state.done.load(Ordering::Acquire) {
                if stop.load(Ordering::Acquire) && !cancelled {
                    ffi::libusb_cancel_transfer(transfer);
                    cancelled = true;
                }
                // Interrupted event handling is harmless. Always drain the callback before
                // freeing the transfer, even on unplug or cancellation.
                if let Err(e) = state.context.handle_events(None) {
                    if e != rusb::Error::Interrupted && !cancelled {
                        stop.store(true, Ordering::Release);
                        ffi::libusb_cancel_transfer(transfer);
                        cancelled = true;
                    }
                }
            }
            if stop.load(Ordering::Acquire) || (*transfer).status != ffi::constants::LIBUSB_TRANSFER_COMPLETED {
                break;
            }
            let n = (*transfer).actual_length as usize;
            deframer.feed(&buf[..n], |m| { let _ = tx.send(m); });
        }
        ffi::libusb_free_transfer(transfer);
    }
}

/// Is this MIDI port the UR44's second port (the one that carries the DSP mixer)?
/// Names differ per OS and driver:
///   macOS (CoreMIDI):                 "Steinberg UR44 Port 2"
///   Windows class driver (CC mode):   "MIDIIN2 (Steinberg UR44)" / "MIDIOUT2 (Steinberg UR44)"
///   Windows, Yamaha Steinberg driver: "Steinberg UR44-2"
///   Linux (ALSA):                     "Steinberg UR44:Steinberg UR44 MIDI 2 20:1"
/// A leading "2- " is Windows' marker for a second device with the same name, not port 2.
pub fn is_control_port(name: &str) -> bool {
    let n = name.trim().to_ascii_uppercase();
    if !n.contains(PORT_MATCH) {
        return false;
    }
    let head = n.split_whitespace().next().unwrap_or("");
    n.ends_with(&CONTROL_PORT_SUFFIX.to_ascii_uppercase())
        || head == "MIDIIN2"
        || head == "MIDIOUT2"
        || n.ends_with("-2")
        || n.contains("MIDI 2 ")
}

/// Names of all MIDI input and output ports, for diagnostics.
pub fn midi_port_names() -> (Vec<String>, Vec<String>) {
    fn names<T: midir::MidiIO>(io: &T) -> Vec<String> {
        io.ports().iter().filter_map(|p| io.port_name(p).ok()).collect()
    }
    let ins = MidiInput::new("ur44-list").map(|i| names(&i)).unwrap_or_default();
    let outs = MidiOutput::new("ur44-list").map(|o| names(&o)).unwrap_or_default();
    (ins, outs)
}

fn find_port<T: midir::MidiIO>(io: &T) -> Result<T::Port> {
    let ports = io.ports();
    ports
        .iter()
        .find(|p| io.port_name(p).map(|n| is_control_port(&n)).unwrap_or(false))
        .cloned()
        .ok_or_else(|| {
            let names: Vec<String> = ports.iter().filter_map(|p| io.port_name(p).ok()).collect();
            let seen = if names.is_empty() { "no MIDI ports at all".to_string() } else { format!("ports seen: {}", names.join(", ")) };
            anyhow!("UR44 control port not found ({seen}). Is the UR44 powered on in CC mode?")
        })
}

impl Ur44 {
    /// Connect over MIDI Port 2 (CC mode) or, failing that, USB interface 4 (Yamaha mode).
    pub fn connect() -> Result<Self> {
        match Self::connect_midi() {
            Ok(d) => Ok(d),
            Err(midi_err) => Self::connect_usb().map_err(|usb_err| {
                anyhow!("UR44 not reachable. CC mode (MIDI Port 2): {midi_err}. Yamaha mode (USB): {usb_err}")
            }),
        }
    }

    pub fn link_kind(&self) -> LinkKind {
        match self.link {
            Link::Midi { .. } => LinkKind::MidiPort2,
            Link::Usb(_) => LinkKind::UsbInterface4,
        }
    }

    pub fn connect_usb() -> Result<Self> {
        let (tx, rx) = channel();
        let link = UsbLink::open(tx)?;
        let mut dev = Self { link: Link::Usb(link), rx };
        dev.handshake()?;
        Ok(dev)
    }

    fn handshake(&mut self) -> Result<()> {
        // A fresh connection can drop the first messages: retry a version request.
        for _ in 0..10 {
            if self.version().is_ok() {
                self.drain();
                return Ok(());
            }
        }
        bail!("UR44 control link found but the mixer does not answer")
    }

    pub fn connect_midi() -> Result<Self> {
        let mut input = MidiInput::new("ur44-in")?;
        input.ignore(midir::Ignore::None); // we need SysEx
        let output = MidiOutput::new("ur44-out")?;
        let in_port = find_port(&input)?;
        let out_port = find_port(&output)?;

        let (tx, rx) = channel();
        let conn_in = input
            .connect(&in_port, "ur44-ctl", move |_ts, msg, _| { let _ = tx.send(msg.to_vec()); }, ())
            .map_err(|e| anyhow!("{e}"))
            .context("opening UR44 input")?;
        let conn_out = output
            .connect(&out_port, "ur44-ctl")
            .map_err(|e| anyhow!("{e}"))
            .context("opening UR44 output")?;
        let mut dev = Self { link: Link::Midi { out: conn_out, _in: conn_in }, rx };
        dev.handshake()?;
        Ok(dev)
    }

    pub fn send(&mut self, msg: &Message) -> Result<()> {
        let bytes = msg.encode();
        match &mut self.link {
            Link::Midi { out, .. } => out.send(&bytes).map_err(|e| anyhow!("{e}")),
            Link::Usb(u) => u.send(&bytes),
        }
    }

    /// Wait for the first incoming message satisfying `pred`, discarding others.
    pub fn wait_for<T>(&self, timeout: Duration, mut pred: impl FnMut(Message) -> Option<T>) -> Result<T> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(raw) => {
                    if let Some(v) = pred(Message::decode(&raw)) {
                        return Ok(v);
                    }
                }
                Err(RecvTimeoutError::Timeout) => bail!("no reply from UR44 within {timeout:?}"),
                Err(RecvTimeoutError::Disconnected) => bail!("UR44 disconnected"),
            }
        }
    }

    pub fn version(&mut self) -> Result<Vec<(String, String)>> {
        self.send(&Message::VersionRequest)?;
        self.wait_for(DEFAULT_TIMEOUT, |m| match m {
            Message::VersionReply(v) => Some(v),
            _ => None,
        })
    }

    /// Read one parameter. Returns `Ok(None)` if the device does not answer (likely no such parameter).
    /// Tries up to 3 times: CoreMIDI occasionally drops a SysEx reply under load.
    pub fn get(&mut self, addr: Address, timeout: Duration) -> Result<Option<u32>> {
        for _ in 0..3 {
            self.send(&Message::ParamRequest(addr))?;
            if let Ok(v) = self.wait_for(timeout, |m| match m {
                Message::ParamValue(a, v) if a == addr => Some(v),
                _ => None,
            }) {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// Write a parameter and confirm by reading it back. Returns the value the device reports.
    /// Range checking is the caller's job (see `params::ParamDef`).
    pub fn set(&mut self, addr: Address, value: u32) -> Result<u32> {
        self.send(&Message::ParamValue(addr, value))?;
        // The UR44 does not acknowledge writes; read back to confirm.
        std::thread::sleep(Duration::from_millis(5));
        self.get(addr, DEFAULT_TIMEOUT)?
            .ok_or_else(|| anyhow!("UR44 did not answer the read-back for id {} ch {}", addr.id, addr.channel))
    }

    /// Send a batch of writes at a pace the UR44 keeps up with (bursts of 16, 4 ms apart).
    pub fn send_paced(&mut self, writes: &[(Address, i32)]) -> Result<()> {
        for (i, (a, v)) in writes.iter().enumerate() {
            self.send(&Message::ParamValue(*a, *v as u32))?;
            if i % 16 == 15 {
                std::thread::sleep(Duration::from_millis(4));
            }
        }
        Ok(())
    }

    /// Send writes, read every one back and re-send what didn't stick (up to `passes` rounds).
    /// Returns the values the device reports afterwards and the number still wrong.
    pub fn write_verified(&mut self, writes: &[(Address, i32)], passes: usize) -> Result<(Vec<(Address, i32)>, usize)> {
        // Coalesce duplicates (last value wins) so verification compares against the final value.
        let mut todo: Vec<(Address, i32)> = Vec::with_capacity(writes.len());
        for (a, v) in writes {
            match todo.iter_mut().find(|(t, _)| t == a) {
                Some(slot) => slot.1 = *v,
                None => todo.push((*a, *v)),
            }
        }
        let mut actual: Vec<(Address, i32)> = Vec::new();
        for _ in 0..passes.max(1) {
            self.send_paced(&todo)?;
            std::thread::sleep(Duration::from_millis(60));
            let mut retry = Vec::new();
            for (a, v) in &todo {
                match self.get(*a, Duration::from_millis(150))? {
                    Some(got) if got as i32 == *v => actual.push((*a, *v)),
                    Some(got) => {
                        actual.push((*a, got as i32));
                        retry.push((*a, *v));
                    }
                    None => retry.push((*a, *v)),
                }
            }
            if retry.is_empty() {
                return Ok((actual, 0));
            }
            actual.retain(|(a, _)| !retry.iter().any(|(r, _)| r == a));
            todo = retry;
        }
        let n = todo.len();
        Ok((actual, n))
    }

    /// Drain and return any raw messages received so far (e.g. unsolicited changes).
    pub fn drain(&self) -> Vec<Vec<u8>> {
        self.rx.try_iter().collect()
    }

    pub fn recv_timeout(&self, t: Duration) -> Option<Vec<u8>> {
        self.rx.recv_timeout(t).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::is_control_port;

    #[test]
    fn control_port_names() {
        for yes in [
            "Steinberg UR44 Port 2",
            "MIDIIN2 (Steinberg UR44)",
            "MIDIOUT2 (Steinberg UR44)",
            "MIDIIN2 (2- Steinberg UR44)",
            "Steinberg UR44-2",
            "Steinberg UR44:Steinberg UR44 MIDI 2 20:1",
        ] {
            assert!(is_control_port(yes), "{yes}");
        }
        for no in [
            "Steinberg UR44 Port 1",
            "Steinberg UR44",
            "2- Steinberg UR44",
            "Steinberg UR44-1",
            "MIDIIN2 (Other Device)",
            "Steinberg UR44:Steinberg UR44 MIDI 1 20:0",
            "Microsoft GS Wavetable Synth",
        ] {
            assert!(!is_control_port(no), "{no}");
        }
    }
}
