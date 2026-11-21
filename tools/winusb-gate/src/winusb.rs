//! Thin safe-ish wrappers over winusb.dll for the UR44 interfaces bound by driver/ur44-winusb.inf.

use anyhow::{bail, Context, Result};
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::Usb::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::OVERLAPPED;

// Device interface GUIDs from ur44-winusb.inf.
pub const GUID_MI01: GUID = GUID::from_u128(0x770BC14B_1B5F_4694_AF40_DFB1E8E9A178);
pub const GUID_MI02: GUID = GUID::from_u128(0x07471BC0_6198_450E_8F47_C5D9D5888688);
#[allow(dead_code)]
pub const GUID_MI04: GUID = GUID::from_u128(0xF903F222_BECA_4D27_A931_319E0AF0FCCE);

// Pipe policies missing from windows-sys (winusbio.h).
#[allow(dead_code)]
pub const ISO_START_LATENCY: u32 = 0x20;
#[allow(dead_code)]
pub const ISO_ALWAYS_START_ASAP: u32 = 0x21;
#[allow(dead_code)]
pub const ISO_NUM_FIXED_PACKETS: u32 = 0x22;

#[link(name = "winusb")]
extern "system" {
    fn WinUsb_ReadIsochPipeAsap(
        buffer: *const core::ffi::c_void,
        offset: u32,
        length: u32,
        continue_stream: BOOL,
        number_of_packets: u32,
        descriptors: *mut USBD_ISO_PACKET_DESCRIPTOR,
        overlapped: *const OVERLAPPED,
    ) -> BOOL;
    fn WinUsb_WriteIsochPipeAsap(
        buffer: *const core::ffi::c_void,
        offset: u32,
        length: u32,
        continue_stream: BOOL,
        overlapped: *const OVERLAPPED,
    ) -> BOOL;
}

pub fn last_error() -> u32 {
    unsafe { GetLastError() }
}

/// Path of the first present device interface with this GUID.
pub fn interface_path(guid: &GUID) -> Result<Vec<u16>> {
    unsafe {
        let set = SetupDiGetClassDevsW(guid, null(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set == INVALID_HANDLE_VALUE as _ {
            bail!("SetupDiGetClassDevs failed: {}", last_error());
        }
        let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
        ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        if SetupDiEnumDeviceInterfaces(set, null(), guid, 0, &mut ifd) == 0 {
            SetupDiDestroyDeviceInfoList(set);
            bail!("no device interface present (is ur44-winusb.inf installed and the UR44 on?)");
        }
        let mut need = 0u32;
        SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut need, null_mut());
        let mut buf = vec![0u8; need as usize + 8];
        let det = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        (*det).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        let ok = SetupDiGetDeviceInterfaceDetailW(set, &ifd, det, need, null_mut(), null_mut());
        SetupDiDestroyDeviceInfoList(set);
        if ok == 0 {
            bail!("SetupDiGetDeviceInterfaceDetail failed: {}", last_error());
        }
        let p = (*det).DevicePath.as_ptr();
        let mut v = Vec::new();
        let mut k = 0;
        while *p.add(k) != 0 {
            v.push(*p.add(k));
            k += 1;
        }
        v.push(0);
        Ok(v)
    }
}

pub struct Iface {
    file: HANDLE,
    pub wu: WINUSB_INTERFACE_HANDLE,
}

unsafe impl Send for Iface {}

impl Iface {
    pub fn open(guid: &GUID) -> Result<Self> {
        let path = interface_path(guid)?;
        let file = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if file == INVALID_HANDLE_VALUE {
            bail!("CreateFile failed: error {} (5 = in use by another program)", last_error());
        }
        let mut wu: WINUSB_INTERFACE_HANDLE = null_mut();
        if unsafe { WinUsb_Initialize(file, &mut wu) } == 0 {
            let e = last_error();
            unsafe { CloseHandle(file) };
            bail!("WinUsb_Initialize failed: {e}");
        }
        Ok(Self { file, wu })
    }

    pub fn set_alt(&self, alt: u8) -> Result<()> {
        if unsafe { WinUsb_SetCurrentAlternateSetting(self.wu, alt) } == 0 {
            bail!("SetCurrentAlternateSetting({alt}) failed: {}", last_error());
        }
        Ok(())
    }

    pub fn query_pipe(&self, alt: u8, index: u8) -> Result<WINUSB_PIPE_INFORMATION_EX> {
        let mut pi: WINUSB_PIPE_INFORMATION_EX = unsafe { std::mem::zeroed() };
        if unsafe { WinUsb_QueryPipeEx(self.wu, alt, index, &mut pi) } == 0 {
            bail!("QueryPipeEx(alt {alt}) failed: {}", last_error());
        }
        Ok(pi)
    }

    pub fn control(&self, rt: u8, req: u8, value: u16, index: u16, data: &mut [u8]) -> Result<usize> {
        let sp = WINUSB_SETUP_PACKET { RequestType: rt, Request: req, Value: value, Index: index, Length: data.len() as u16 };
        let mut done = 0u32;
        if unsafe { WinUsb_ControlTransfer(self.wu, sp, data.as_mut_ptr(), data.len() as u32, &mut done, null()) } == 0 {
            bail!("control transfer {rt:02x}/{req:02x} failed: {}", last_error());
        }
        Ok(done as usize)
    }

    /// UAC1 sampling frequency control on an endpoint: SET_CUR, then GET_CUR.
    pub fn set_rate(&self, ep: u8, rate: u32) -> Result<u32> {
        let mut b = [(rate & 0xff) as u8, (rate >> 8) as u8, (rate >> 16) as u8];
        self.control(0x22, 0x01, 0x0100, ep as u16, &mut b).context("SET_CUR sampling frequency")?;
        self.get_rate(ep)
    }

    pub fn get_rate(&self, ep: u8) -> Result<u32> {
        let mut r = [0u8; 3];
        let n = self.control(0xA2, 0x81, 0x0100, ep as u16, &mut r).context("GET_CUR sampling frequency")?;
        if n != 3 {
            bail!("GET_CUR returned {n} bytes");
        }
        Ok(r[0] as u32 | (r[1] as u32) << 8 | (r[2] as u32) << 16)
    }

    pub fn set_pipe_policy_u32(&self, pipe: u8, policy: u32, v: u32) -> Result<()> {
        if unsafe { WinUsb_SetPipePolicy(self.wu, pipe, policy, 4, &v as *const u32 as _) } == 0 {
            bail!("SetPipePolicy({policy:#x}) failed: {}", last_error());
        }
        Ok(())
    }

    /// Raw policy read: (bytes returned, value bytes) or the error code.
    pub fn get_pipe_policy(&self, pipe: u8, policy: u32) -> Result<Vec<u8>, u32> {
        let mut buf = [0u8; 16];
        let mut len = buf.len() as u32;
        if unsafe { WinUsb_GetPipePolicy(self.wu, pipe, policy, &mut len, buf.as_mut_ptr() as _) } == 0 {
            return Err(last_error());
        }
        Ok(buf[..len as usize].to_vec())
    }

    pub fn set_pipe_policy_raw(&self, pipe: u8, policy: u32, v: &[u8]) -> Result<(), u32> {
        if unsafe { WinUsb_SetPipePolicy(self.wu, pipe, policy, v.len() as u32, v.as_ptr() as _) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    pub fn register(&self, pipe: u8, buf: &mut [u8]) -> Result<IsochBuffer> {
        let mut h = null_mut();
        if unsafe { WinUsb_RegisterIsochBuffer(self.wu, pipe, buf.as_mut_ptr(), buf.len() as u32, &mut h) } == 0 {
            bail!("RegisterIsochBuffer failed: {}", last_error());
        }
        Ok(IsochBuffer { h })
    }

    /// Current 1 ms frame number and its QPC time.
    pub fn frame(&self) -> Option<(u32, i64)> {
        let mut f = 0u32;
        let mut t = 0i64;
        (unsafe { WinUsb_GetCurrentFrameNumber(self.wu, &mut f, &mut t) } != 0).then_some((f, t))
    }

    pub fn start_time_sync(&self) -> Result<HANDLE> {
        let mut info = USB_START_TRACKING_FOR_TIME_SYNC_INFORMATION { TimeTrackingHandle: null_mut(), IsStartupDelayTolerable: 1 };
        if unsafe { WinUsb_StartTrackingForTimeSync(self.wu, &mut info) } == 0 {
            bail!("StartTrackingForTimeSync failed: {}", last_error());
        }
        Ok(info.TimeTrackingHandle)
    }

    /// (frame, microframe, QPC of that microframe, QPC now, current usb frame, current hw microframe, accuracy µs).
    pub fn frame_qpc(&self, tracking: HANDLE, frame: u32, micro: u32) -> Option<USB_FRAME_NUMBER_AND_QPC_FOR_TIME_SYNC_INFORMATION> {
        let mut info: USB_FRAME_NUMBER_AND_QPC_FOR_TIME_SYNC_INFORMATION = unsafe { std::mem::zeroed() };
        info.TimeTrackingHandle = tracking;
        info.InputFrameNumber = frame;
        info.InputMicroFrameNumber = micro;
        (unsafe { WinUsb_GetCurrentFrameNumberAndQpc(self.wu, &mut info) } != 0).then_some(info)
    }

    pub fn overlapped_result(&self, ov: &OVERLAPPED, wait: bool) -> Result<u32, u32> {
        let mut n = 0u32;
        if unsafe { WinUsb_GetOverlappedResult(self.wu, ov, &mut n, wait as BOOL) } == 0 {
            Err(last_error())
        } else {
            Ok(n)
        }
    }

    pub fn abort(&self, pipe: u8) {
        unsafe { WinUsb_AbortPipe(self.wu, pipe) };
    }
}

impl Drop for Iface {
    fn drop(&mut self) {
        unsafe {
            WinUsb_Free(self.wu);
            CloseHandle(self.file);
        }
    }
}

pub struct IsochBuffer {
    h: *mut core::ffi::c_void,
}

impl IsochBuffer {
    /// Returns Ok(()) when the transfer is pending, Err(code) otherwise.
    pub fn read_asap(&self, off: u32, len: u32, cont: bool, desc: &mut [USBD_ISO_PACKET_DESCRIPTOR], ov: &OVERLAPPED) -> Result<(), u32> {
        let ok = unsafe { WinUsb_ReadIsochPipeAsap(self.h, off, len, cont as BOOL, desc.len() as u32, desc.as_mut_ptr(), ov) };
        let e = last_error();
        if ok != 0 || e == ERROR_IO_PENDING {
            Ok(())
        } else {
            Err(e)
        }
    }

    pub fn write_asap(&self, off: u32, len: u32, cont: bool, ov: &OVERLAPPED) -> Result<(), u32> {
        let ok = unsafe { WinUsb_WriteIsochPipeAsap(self.h, off, len, cont as BOOL, ov) };
        let e = last_error();
        if ok != 0 || e == ERROR_IO_PENDING {
            Ok(())
        } else {
            Err(e)
        }
    }

    /// Explicit-frame write: `frame` is the start frame; on return it holds the frame after the transfer.
    pub fn write_at(&self, off: u32, len: u32, frame: &mut u32, ov: &OVERLAPPED) -> Result<(), u32> {
        let ok = unsafe { WinUsb_WriteIsochPipe(self.h, off, len, frame, ov) };
        let e = last_error();
        if ok != 0 || e == ERROR_IO_PENDING {
            Ok(())
        } else {
            Err(e)
        }
    }

    pub fn read_at(&self, off: u32, len: u32, frame: &mut u32, desc: &mut [USBD_ISO_PACKET_DESCRIPTOR], ov: &OVERLAPPED) -> Result<(), u32> {
        let ok = unsafe { WinUsb_ReadIsochPipe(self.h, off, len, frame, desc.len() as u32, desc.as_mut_ptr(), ov) };
        let e = last_error();
        if ok != 0 || e == ERROR_IO_PENDING {
            Ok(())
        } else {
            Err(e)
        }
    }
}

impl Drop for IsochBuffer {
    fn drop(&mut self) {
        unsafe { WinUsb_UnregisterIsochBuffer(self.h) };
    }
}

/// Manual-reset event + OVERLAPPED.
pub struct Ov {
    pub ov: Box<OVERLAPPED>,
}

impl Ov {
    pub fn new() -> Self {
        let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        ov.hEvent = unsafe { windows_sys::Win32::System::Threading::CreateEventW(null(), 1, 0, null()) };
        Self { ov }
    }
    pub fn reset(&mut self) {
        let ev = self.ov.hEvent;
        *self.ov = unsafe { std::mem::zeroed() };
        self.ov.hEvent = ev;
        unsafe { windows_sys::Win32::System::Threading::ResetEvent(ev) };
    }
    pub fn event(&self) -> HANDLE {
        self.ov.hEvent
    }
}

impl Drop for Ov {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.ov.hEvent) };
    }
}
