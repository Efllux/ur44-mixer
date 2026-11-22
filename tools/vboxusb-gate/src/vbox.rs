//! User-mode access to a device captured by VBoxUSB.sys (Oracle, as shipped and catalog-signed in
//! usbipd-win). Only the documented IOCTL interface is used; the definitions below are our own,
//! written from the byte layout of the driver's user interface (METHOD_BUFFERED, 4-byte packing):
//!
//! | IOCTL (FILE_DEVICE_UNKNOWN, FILE_WRITE_ACCESS) | fn  | in / out bytes                    |
//! |-----------------------------------------------|-----|-----------------------------------|
//! | GET_DEVICE        | 0x603 | 8 / 8  (also latches the high-speed flag used for scheduling) |
//! | SEND_URB          | 0x607 | 104 / 104                                                     |
//! | SELECT_INTERFACE  | 0x609 | 2 / 0  (bInterfaceNumber, bAlternateSetting)                  |
//! | SET_CONFIG        | 0x60A | 1 / 0                                                         |
//! | CLAIM_DEVICE      | 0x60B | 2 / 2  (bInterfaceNumber, fClaimed)                           |
//! | RELEASE_DEVICE    | 0x60C | 2 / 0                                                         |
//! | GET_VERSION       | 0x60F | 0 / 8  (major, minor: u32)                                    |
//! | ABORT_ENDPOINT    | 0x610 | 1 / 0                                                         |
//!
//! URB block (104 bytes, x64): type u32 @0, ep u32 @4 (endpoint number without the direction bit),
//! dir u32 @8 (1 = IN, 2 = OUT), flags u32 @12, error u32 @16, len u64 @20, buf ptr @28,
//! packet count u32 @36, 8 packet descriptors @40 of { len u16, offset u16, status u32 }.
//! Transfer types: 1 = isochronous, 4 = control message (buffer = 8-byte setup + data).
//! Error/status codes: 0 OK, 1 stall (also any unmapped USBD status, e.g. ISO late), 2 DNR,
//! 3 CRC, 4 not accessed, 5 underrun, 6 overrun.
//!
//! Driver behaviour relevant for safety (from reading the driver's published source):
//! - One isochronous IOCTL carries at most 8 packets. There is no range check in a release
//!   build, so this module refuses anything but exactly 8 (one high-speed frame).
//! - The driver picks the start frame itself: max(current frame + 2, next frame of the pipe),
//!   and advances the pipe by one frame per URB on high-speed devices. If the queue runs dry it
//!   silently re-anchors. The caller has to detect that (see `live` in the engine).

use anyhow::{bail, Result};
use std::mem::size_of;
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

pub const GUID_CLASS_VBOXUSB: GUID = GUID {
    data1: 0x0087_3fdf,
    data2: 0xCAFE,
    data3: 0x80EE,
    data4: [0xaa, 0x5e, 0x00, 0xc0, 0x4f, 0xb1, 0x72, 0x0b],
};

const fn ctl(func: u32) -> u32 {
    (0x22 << 16) | (0x0002 << 14) | (func << 2)
}
pub const IOCTL_GET_DEVICE: u32 = ctl(0x603);
pub const IOCTL_SEND_URB: u32 = ctl(0x607);
pub const IOCTL_SELECT_INTERFACE: u32 = ctl(0x609);
pub const IOCTL_SET_CONFIG: u32 = ctl(0x60A);
pub const IOCTL_CLAIM_DEVICE: u32 = ctl(0x60B);
pub const IOCTL_RELEASE_DEVICE: u32 = ctl(0x60C);
pub const IOCTL_GET_VERSION: u32 = ctl(0x60F);
pub const IOCTL_ABORT_ENDPOINT: u32 = ctl(0x610);

pub const TYPE_ISOC: u32 = 1;
pub const TYPE_BULK: u32 = 2;
pub const TYPE_MSG: u32 = 4;
pub const DIR_SETUP: u32 = 0;
pub const DIR_IN: u32 = 1;
pub const DIR_OUT: u32 = 2;
pub const PKTS: usize = 8;

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
pub struct IsoPkt {
    pub len: u16,
    pub off: u16,
    pub status: u32,
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct Urb {
    pub kind: u32,
    pub ep: u32,
    pub dir: u32,
    pub flags: u32,
    pub error: u32,
    pub len: u64,
    pub buf: *mut u8,
    pub npkts: u32,
    pub pkts: [IsoPkt; PKTS],
}
const _: () = assert!(size_of::<Urb>() == 104);
const _: () = assert!(size_of::<IsoPkt>() == 8);

impl Urb {
    pub fn zeroed() -> Self {
        Self { kind: 0, ep: 0, dir: 0, flags: 0, error: 0, len: 0, buf: null_mut(), npkts: 0, pkts: [IsoPkt::default(); PKTS] }
    }
}

pub fn status_name(s: u32) -> &'static str {
    match s {
        0 => "ok",
        1 => "stall/other (late?)",
        2 => "device not responding",
        3 => "crc",
        4 => "not accessed",
        5 => "underrun",
        6 => "overrun",
        _ => "?",
    }
}

fn wide_to_string(w: &[u16]) -> String {
    let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..n])
}

/// Device interface paths of all present VBoxUSB devices.
pub fn list_paths() -> Result<Vec<String>> {
    let mut out = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(&GUID_CLASS_VBOXUSB, null(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set == INVALID_HANDLE_VALUE as _ {
            bail!("SetupDiGetClassDevs failed: {}", GetLastError());
        }
        let mut i = 0;
        loop {
            let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            ifd.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null(), &GUID_CLASS_VBOXUSB, i, &mut ifd) == 0 {
                break;
            }
            i += 1;
            let mut need = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut need, null_mut());
            if need == 0 {
                continue;
            }
            let mut buf = vec![0u64; (need as usize + 7) / 8];
            let det = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*det).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, det, need, null_mut(), null_mut()) != 0 {
                let p = std::ptr::addr_of!((*det).DevicePath) as *const u16;
                let len = (need as usize - 4) / 2;
                out.push(wide_to_string(std::slice::from_raw_parts(p, len)));
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    Ok(out)
}

pub struct Dev {
    pub h: HANDLE,
    ev: HANDLE,
    claimed: bool,
}

unsafe impl Send for Dev {}

impl Dev {
    /// Opens the captured UR44 (the VBoxUSB interface whose path names VID 0499 / PID 150E).
    pub fn open_ur44() -> Result<(Self, String)> {
        let paths = list_paths()?;
        let Some(path) = paths.iter().find(|p| p.to_ascii_lowercase().contains("vid_0499&pid_150e")) else {
            bail!("no VBoxUSB device with VID_0499&PID_150E (found {} VBoxUSB interfaces: {:?})", paths.len(), paths);
        };
        let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let h = CreateFileW(
                w.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                bail!("CreateFile({path}) failed: error {}", GetLastError());
            }
            let ev = CreateEventW(null(), 1, 0, null());
            Ok((Self { h, ev, claimed: false }, path.clone()))
        }
    }

    /// Synchronous IOCTL. Returns the number of output bytes.
    pub fn ioctl(&self, code: u32, inp: *const u8, n_in: usize, out: *mut u8, n_out: usize) -> Result<u32> {
        unsafe {
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = self.ev;
            let mut got = 0u32;
            let ok = DeviceIoControl(self.h, code, inp as _, n_in as u32, out as _, n_out as u32, &mut got, &mut ov);
            if ok == 0 {
                let e = GetLastError();
                if e != ERROR_IO_PENDING {
                    bail!("IOCTL {code:#x} failed: error {e}");
                }
                if GetOverlappedResult(self.h, &ov, &mut got, 1) == 0 {
                    bail!("IOCTL {code:#x} failed: error {}", GetLastError());
                }
            }
            Ok(got)
        }
    }

    pub fn version(&self) -> Result<(u32, u32)> {
        let mut v = [0u32; 2];
        self.ioctl(IOCTL_GET_VERSION, null(), 0, v.as_mut_ptr() as _, 8)?;
        Ok((v[0], v[1]))
    }

    /// Must precede streaming: the driver latches the device speed here, and the isochronous
    /// scheduler advances one frame per URB only when it knows the device is high speed.
    pub fn get_device(&self) -> Result<u64> {
        let mut v = 0u64;
        let p = &mut v as *mut u64 as *mut u8;
        self.ioctl(IOCTL_GET_DEVICE, p, 8, p, 8)?;
        Ok(v)
    }

    pub fn claim(&mut self) -> Result<()> {
        let mut b = [0u8; 2];
        self.ioctl(IOCTL_CLAIM_DEVICE, b.as_ptr(), 2, b.as_mut_ptr(), 2)?;
        if b[1] == 0 {
            bail!("CLAIM_DEVICE: not claimed (another process owns the device?)");
        }
        self.claimed = true;
        Ok(())
    }

    pub fn set_config(&self, v: u8) -> Result<()> {
        self.ioctl(IOCTL_SET_CONFIG, &v, 1, null_mut(), 0)?;
        Ok(())
    }

    pub fn select_interface(&self, ifc: u8, alt: u8) -> Result<()> {
        let b = [ifc, alt];
        self.ioctl(IOCTL_SELECT_INTERFACE, b.as_ptr(), 2, null_mut(), 0)?;
        Ok(())
    }

    /// Control transfer on the default pipe. `data` is the data stage (IN: filled, OUT: sent).
    pub fn control(&self, bm: u8, req: u8, val: u16, idx: u16, data: &mut [u8]) -> Result<usize> {
        let mut buf = vec![0u8; 8 + data.len()];
        buf[0] = bm;
        buf[1] = req;
        buf[2..4].copy_from_slice(&val.to_le_bytes());
        buf[4..6].copy_from_slice(&idx.to_le_bytes());
        buf[6..8].copy_from_slice(&(data.len() as u16).to_le_bytes());
        let din = bm & 0x80 != 0;
        if !din {
            buf[8..].copy_from_slice(data);
        }
        let mut u = Urb::zeroed();
        u.kind = TYPE_MSG;
        u.ep = 0;
        u.dir = if din { DIR_IN } else if data.is_empty() { DIR_SETUP } else { DIR_OUT };
        u.flags = 1;
        u.len = buf.len() as u64;
        u.buf = buf.as_mut_ptr();
        let p = &mut u as *mut Urb as *mut u8;
        self.ioctl(IOCTL_SEND_URB, p, 104, p, 104)?;
        let err = { u.error };
        if err != 0 {
            bail!("control {bm:#04x}/{req:#04x} wValue {val:#06x} wIndex {idx:#06x}: {}", status_name(err));
        }
        let n = ({ u.len } as usize).saturating_sub(8).min(data.len());
        if din {
            data[..n].copy_from_slice(&buf[8..8 + n]);
        }
        Ok(n)
    }

    /// Bulk transfer on endpoint address `ep_addr` (bit 7 = IN). Returns bytes transferred, or
    /// Ok(None) on timeout (the pipe is then aborted with ABORT_ENDPOINT and the IRP drained).
    pub fn bulk(&self, ep_addr: u8, buf: &mut [u8], timeout_ms: u32) -> Result<Option<usize>> {
        let din = ep_addr & 0x80 != 0;
        let mut u = Urb::zeroed();
        u.kind = TYPE_BULK;
        u.ep = (ep_addr & 0x0f) as u32;
        u.dir = if din { DIR_IN } else { DIR_OUT };
        u.flags = 1;
        u.len = buf.len() as u64;
        u.buf = buf.as_mut_ptr();
        unsafe {
            let ev = CreateEventW(null(), 1, 0, null());
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = ev;
            let p = &mut u as *mut Urb as *mut u8;
            let ok = DeviceIoControl(self.h, IOCTL_SEND_URB, p as _, 104, p as _, 104, null_mut(), &mut ov);
            if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                let e = GetLastError();
                CloseHandle(ev);
                bail!("bulk {ep_addr:#04x}: SEND_URB failed: error {e}");
            }
            let timed_out = WaitForSingleObject(ev, timeout_ms) == WAIT_TIMEOUT;
            if timed_out {
                let b = [ep_addr];
                let _ = self.ioctl(IOCTL_ABORT_ENDPOINT, b.as_ptr(), 1, null_mut(), 0);
            }
            let mut got = 0u32;
            let r = GetOverlappedResult(self.h, &ov, &mut got, 1);
            CloseHandle(ev);
            if timed_out {
                return Ok(None);
            }
            if r == 0 {
                bail!("bulk {ep_addr:#04x} failed: error {}", GetLastError());
            }
            let err = { u.error };
            if err != 0 {
                bail!("bulk {ep_addr:#04x}: {}", status_name(err));
            }
            Ok(Some({ u.len } as usize))
        }
    }

    /// Submit an isochronous URB asynchronously. Refuses anything but exactly 8 packets.
    /// Safety: `urb`, its buffer and `ov` must stay valid and unmoved until the IOCTL completes.
    pub unsafe fn submit_iso(&self, urb: *mut Urb, ov: *mut OVERLAPPED) -> Result<()> {
        if (*urb).npkts as usize != PKTS || (*urb).kind != TYPE_ISOC {
            eprintln!("SAFETY GUARD: isochronous URB with {} packets refused (must be {PKTS})", { (*urb).npkts });
            std::process::exit(3);
        }
        let ok = DeviceIoControl(self.h, IOCTL_SEND_URB, urb as _, 104, urb as _, 104, null_mut(), ov);
        if ok == 0 {
            let e = GetLastError();
            if e != ERROR_IO_PENDING {
                bail!("SEND_URB failed at submission: error {e}");
            }
        }
        Ok(())
    }

    pub unsafe fn result(&self, ov: *mut OVERLAPPED) -> Result<()> {
        let mut got = 0u32;
        if GetOverlappedResult(self.h, ov, &mut got, 1) == 0 {
            bail!("SEND_URB completed with error {}", GetLastError());
        }
        Ok(())
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        unsafe {
            if self.claimed {
                let b = [0u8; 2];
                let _ = self.ioctl(IOCTL_RELEASE_DEVICE, b.as_ptr(), 2, null_mut(), 0);
            }
            CloseHandle(self.ev);
            CloseHandle(self.h);
        }
    }
}
