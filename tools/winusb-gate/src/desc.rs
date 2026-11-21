//! Configuration descriptor: read it through the parent hub (works with no driver bound) and
//! parse the parts the audio engine needs.

use std::fmt::Write as _;

#[derive(Debug, Clone, Default)]
pub struct AltFormat {
    pub iface: u8,
    pub alt: u8,
    pub channels: u8,
    pub subframe: u8,
    pub bits: u8,
    pub rates: Vec<u32>,
    pub ep: u8,
    pub ep_attr: u8,
    pub max_packet: u16, // bytes per transaction (bits 0..10)
    pub mult: u8,        // additional transactions per microframe (bits 11..12)
    pub interval: u8,
}

impl AltFormat {
    pub fn frame_bytes(&self) -> u32 {
        self.channels as u32 * self.subframe as u32
    }
}

/// All audio streaming formats (interface, alt) with an isochronous endpoint.
pub fn formats(cd: &[u8]) -> Vec<AltFormat> {
    let mut out = Vec::new();
    let mut cur: Option<AltFormat> = None;
    let mut p = 0usize;
    while p + 2 <= cd.len() {
        let len = cd[p] as usize;
        if len < 2 || p + len > cd.len() {
            break;
        }
        let d = &cd[p..p + len];
        match d[1] {
            4 if len >= 9 => {
                if let Some(f) = cur.take() {
                    if f.ep != 0 {
                        out.push(f);
                    }
                }
                cur = Some(AltFormat { iface: d[2], alt: d[3], ..Default::default() });
            }
            0x24 if len >= 8 && d[2] == 0x02 && d[3] == 0x01 => {
                if let Some(f) = cur.as_mut() {
                    f.channels = d[4];
                    f.subframe = d[5];
                    f.bits = d[6];
                    let n = d[7] as usize;
                    for i in 0..n {
                        let o = 8 + 3 * i;
                        if o + 2 < len {
                            f.rates.push(d[o] as u32 | (d[o + 1] as u32) << 8 | (d[o + 2] as u32) << 16);
                        }
                    }
                }
            }
            5 if len >= 7 => {
                if let Some(f) = cur.as_mut() {
                    if d[3] & 3 == 1 {
                        let w = u16::from_le_bytes([d[4], d[5]]);
                        f.ep = d[2];
                        f.ep_attr = d[3];
                        f.max_packet = w & 0x7ff;
                        f.mult = ((w >> 11) & 3) as u8;
                        f.interval = d[6];
                    }
                }
            }
            _ => {}
        }
        p += len;
    }
    if let Some(f) = cur.take() {
        if f.ep != 0 {
            out.push(f);
        }
    }
    out
}

/// Human-readable dump of every descriptor in a configuration descriptor.
pub fn dump(cd: &[u8]) -> String {
    let mut s = String::new();
    let mut p = 0usize;
    let mut iface_class = 0u8;
    let mut iface_sub = 0u8;
    while p + 2 <= cd.len() {
        let len = cd[p] as usize;
        if len < 2 || p + len > cd.len() {
            let _ = writeln!(s, "  !! malformed descriptor at offset {p}");
            break;
        }
        let d = &cd[p..p + len];
        let hex: Vec<String> = d.iter().map(|b| format!("{b:02x}")).collect();
        match d[1] {
            2 => {
                let _ = writeln!(
                    s,
                    "CONFIG wTotalLength={} bNumInterfaces={} bConfigurationValue={} bmAttributes=0x{:02x} MaxPower={}mA",
                    u16::from_le_bytes([d[2], d[3]]), d[4], d[5], d[7], d[8] as u32 * 2
                );
            }
            4 => {
                iface_class = d[5];
                iface_sub = d[6];
                let _ = writeln!(
                    s,
                    "\n  INTERFACE {} alt {}: {} endpoints, class 0x{:02x} subclass 0x{:02x} protocol 0x{:02x} iInterface {}",
                    d[2], d[3], d[4], d[5], d[6], d[7], d[8]
                );
            }
            5 => {
                let w = u16::from_le_bytes([d[4], d[5]]);
                let ty = ["control", "isochronous", "bulk", "interrupt"][(d[3] & 3) as usize];
                let sync = ["none", "async", "adaptive", "sync"][((d[3] >> 2) & 3) as usize];
                let usage = ["data", "feedback", "implicit-fb data", "reserved"][((d[3] >> 4) & 3) as usize];
                let _ = write!(
                    s,
                    "    ENDPOINT 0x{:02x} ({}) {} sync={} usage={} wMaxPacketSize=0x{:04x} ({} bytes x {} per uframe) bInterval={}",
                    d[2], if d[2] & 0x80 != 0 { "IN" } else { "OUT" }, ty, sync, usage, w, w & 0x7ff, ((w >> 11) & 3) + 1, d[6]
                );
                if len >= 9 {
                    let _ = write!(s, " bRefresh={} bSynchAddress=0x{:02x}", d[7], d[8]);
                }
                let _ = writeln!(s);
            }
            0x24 => {
                let _ = write!(s, "    CS_INTERFACE(class 0x{iface_class:02x}/0x{iface_sub:02x}) subtype 0x{:02x}", d[2]);
                if d[2] == 0x01 && len >= 7 && iface_class == 0xff {
                    let _ = write!(s, " AS_GENERAL? bTerminalLink={} bDelay={} wFormatTag=0x{:04x}", d[3], d[4], u16::from_le_bytes([d[5], d[6]]));
                }
                if d[2] == 0x02 && len >= 8 && d[3] == 0x01 {
                    let n = d[7] as usize;
                    let rates: Vec<String> = (0..n)
                        .filter(|i| 8 + 3 * i + 2 < len)
                        .map(|i| {
                            let o = 8 + 3 * i;
                            (d[o] as u32 | (d[o + 1] as u32) << 8 | (d[o + 2] as u32) << 16).to_string()
                        })
                        .collect();
                    let _ = write!(
                        s,
                        " FORMAT_TYPE_I channels={} subframe={} bytes bits={} rates=[{}]",
                        d[4], d[5], d[6], rates.join(", ")
                    );
                }
                let _ = writeln!(s, "  [{}]", hex.join(" "));
            }
            0x25 => {
                let _ = writeln!(s, "    CS_ENDPOINT [{}]", hex.join(" "));
            }
            0x0b => {
                let _ = writeln!(s, "  IAD first={} count={} class 0x{:02x}", d[2], d[3], d[4]);
            }
            _ => {
                let _ = writeln!(s, "    desc type 0x{:02x} [{}]", d[1], hex.join(" "));
            }
        }
        p += len;
    }
    s
}

#[cfg(windows)]
pub mod hub {
    //! Read the device and configuration descriptors from the hub the UR44 is attached to.
    use anyhow::{bail, Result};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
    use windows_sys::Win32::Devices::Usb::*;
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    pub struct Found {
        pub hub_path: String,
        pub port: u32,
        pub device: [u8; 18],
        pub speed: u8,
        pub config: Vec<u8>,
    }

    fn wide_to_string(w: &[u16]) -> String {
        let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..n])
    }

    /// Paths of all present USB hub interfaces.
    pub fn hub_paths() -> Vec<Vec<u16>> {
        let mut out = Vec::new();
        unsafe {
            let set = SetupDiGetClassDevsW(&GUID_DEVINTERFACE_USB_HUB, null(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
            if set == INVALID_HANDLE_VALUE as _ {
                return out;
            }
            let mut i = 0;
            loop {
                let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
                ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
                if SetupDiEnumDeviceInterfaces(set, null(), &GUID_DEVINTERFACE_USB_HUB, i, &mut ifd) == 0 {
                    break;
                }
                i += 1;
                let mut need = 0u32;
                SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut need, null_mut());
                let mut buf = vec![0u8; need as usize + 8];
                let det = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
                (*det).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                if SetupDiGetDeviceInterfaceDetailW(set, &ifd, det, need, null_mut(), null_mut()) == 0 {
                    continue;
                }
                let p = (*det).DevicePath.as_ptr();
                let mut v = Vec::new();
                let mut k = 0;
                while *p.add(k) != 0 {
                    v.push(*p.add(k));
                    k += 1;
                }
                v.push(0);
                out.push(v);
            }
            SetupDiDestroyDeviceInfoList(set);
        }
        out
    }

    fn get_descriptor(h: HANDLE, port: u32, ty: u8, index: u8, len: u16) -> Option<Vec<u8>> {
        // USB_DESCRIPTOR_REQUEST (packed): ConnectionIndex u32, setup packet (8 bytes), data.
        let mut buf = vec![0u8; 12 + len as usize];
        buf[0..4].copy_from_slice(&port.to_le_bytes());
        buf[4] = 0x80;
        buf[5] = 6;
        buf[6..8].copy_from_slice(&(((ty as u16) << 8) | index as u16).to_le_bytes());
        buf[8..10].copy_from_slice(&0u16.to_le_bytes());
        buf[10..12].copy_from_slice(&len.to_le_bytes());
        let mut ret = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                h,
                IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION,
                buf.as_ptr() as _,
                buf.len() as u32,
                buf.as_mut_ptr() as _,
                buf.len() as u32,
                &mut ret,
                null_mut(),
            )
        };
        if ok == 0 || ret < 12 {
            return None;
        }
        Some(buf[12..ret as usize].to_vec())
    }

    pub fn find(vid: u16, pid: u16) -> Result<Found> {
        for path in hub_paths() {
            let h = unsafe {
                CreateFileW(path.as_ptr(), GENERIC_WRITE, FILE_SHARE_WRITE, null(), OPEN_EXISTING, 0, null_mut())
            };
            if h == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut info: USB_NODE_INFORMATION = unsafe { std::mem::zeroed() };
            let mut ret = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    h,
                    IOCTL_USB_GET_NODE_INFORMATION,
                    &info as *const _ as _,
                    std::mem::size_of::<USB_NODE_INFORMATION>() as u32,
                    &mut info as *mut _ as _,
                    std::mem::size_of::<USB_NODE_INFORMATION>() as u32,
                    &mut ret,
                    null_mut(),
                )
            };
            let ports = if ok != 0 { (unsafe { info.u.HubInformation.HubDescriptor.bNumberOfPorts }) as u32 } else { 0 };
            for port in 1..=ports {
                let mut ci: USB_NODE_CONNECTION_INFORMATION_EX = unsafe { std::mem::zeroed() };
                ci.ConnectionIndex = port;
                let sz = std::mem::size_of::<USB_NODE_CONNECTION_INFORMATION_EX>() as u32;
                let ok = unsafe {
                    DeviceIoControl(h, IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX, &ci as *const _ as _, sz, &mut ci as *mut _ as _, sz, &mut ret, null_mut())
                };
                if ok == 0 {
                    continue;
                }
                let dd = ci.DeviceDescriptor;
                if dd.idVendor != vid || dd.idProduct != pid {
                    continue;
                }
                let device = get_descriptor(h, port, 1, 0, 18).unwrap_or_default();
                let head = get_descriptor(h, port, 2, 0, 9);
                let Some(head) = head else { bail!("UR44 found on {} port {port}, but the config descriptor read failed", wide_to_string(&path)) };
                let total = u16::from_le_bytes([head[2], head[3]]);
                let config = get_descriptor(h, port, 2, 0, total).unwrap_or_default();
                unsafe { CloseHandle(h) };
                let mut d = [0u8; 18];
                d[..device.len().min(18)].copy_from_slice(&device[..device.len().min(18)]);
                return Ok(Found { hub_path: wide_to_string(&path), port, device: d, speed: ci.Speed, config });
            }
            unsafe { CloseHandle(h) };
        }
        bail!("no {vid:04x}:{pid:04x} found on any hub")
    }
}
