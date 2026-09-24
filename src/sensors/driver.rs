//! Ring-0 PCI config-space access via a WinRing0-compatible kernel driver.
//!
//! Why this exists: on AMD Zen there is no user-mode path to the die
//! temperature. The value lives behind the SMU's System Management Network,
//! reachable only by writing an address to PCI config offset 0x60 of device
//! 00:00.0 and reading the result back from 0x64 -- and PCI config space needs
//! `in`/`out` on ports 0xCF8/0xCFC, which is ring-0 only. HWMonitor, HWiNFO
//! and LibreHardwareMonitor all ship a kernel driver for exactly this reason.
//!
//! This module is only the *client*. It never downloads anything: it either
//! attaches to a WinRing0 service that is already running, or registers one
//! from a `.sys` the user has deliberately placed next to the executable.
//! See SETUP.md.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_SERVICE_EXISTS, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, CreateServiceW, OpenSCManagerW, OpenServiceW, StartServiceW,
    SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL,
    SERVICE_KERNEL_DRIVER,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

const ERROR_SERVICE_ALREADY_RUNNING: u32 = 1056;

const SERVICE_NAME: &str = "WinRing0_1_2_0";
const DEVICE_PATH: &str = "\\\\.\\WinRing0_1_2_0";

// CTL_CODE(40000, function, METHOD_BUFFERED, access)
//   = (40000 << 16) | (access << 14) | (function << 2)
const IOCTL_READ_PCI_CONFIG: u32 = 0x9C40_6184; // fn 0x861, FILE_READ_ACCESS
const IOCTL_WRITE_PCI_CONFIG: u32 = 0x9C40_A188; // fn 0x862, FILE_WRITE_ACCESS

#[repr(C)]
struct ReadPciInput {
    pci_address: u32,
    pci_offset: u32,
}

#[repr(C)]
struct WritePciInput {
    pci_address: u32,
    pci_offset: u32,
    value: u32,
}

/// Why the CPU sensor is unavailable, so the UI can say something useful
/// instead of showing a blank tile.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DriverStatus {
    Ready,
    /// No driver service running and no .sys found to install from.
    NotInstalled,
    /// A .sys is present but the service could not be created (not elevated).
    NeedsElevation,
    /// CPU family this decoder has not been verified against, or not AMD.
    Unsupported,
    /// Service exists but the device would not open. On a machine with Memory
    /// Integrity enabled, this is what a blocklisted driver looks like.
    Blocked,
}

impl DriverStatus {
    pub fn message(self) -> &'static str {
        match self {
            DriverStatus::Ready => "ready",
            DriverStatus::NotInstalled => "driver not installed - see SETUP.md",
            DriverStatus::NeedsElevation => "run as administrator for CPU temps",
            DriverStatus::Blocked => "CPU driver unavailable - see SETUP.md",
            DriverStatus::Unsupported => "no supported CPU sensor",
        }
    }
}

pub struct Driver {
    handle: HANDLE,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn sys_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("WinRing0x64.sys");
    candidate.exists().then_some(candidate)
}

impl Driver {
    /// Get a usable handle, installing/starting the service if necessary.
    pub fn open() -> Result<Self, DriverStatus> {
        // Fast path: something already started the driver for us.
        if let Some(h) = Self::open_device() {
            return Ok(Self { handle: h });
        }

        let Some(sys) = sys_path() else {
            return Err(DriverStatus::NotInstalled);
        };

        Self::install_and_start(&sys)?;

        match Self::open_device() {
            Some(h) => Ok(Self { handle: h }),
            // Service registered and reported started, but the device node is
            // absent: the kernel refused to load the image.
            None => Err(DriverStatus::Blocked),
        }
    }

    fn open_device() -> Option<HANDLE> {
        let path = wide(DEVICE_PATH);
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(h)
    }

    fn install_and_start(sys: &Path) -> Result<(), DriverStatus> {
        let scm =
            unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS) };
        if scm.is_null() {
            // Opening the SCM for write requires elevation.
            return Err(DriverStatus::NeedsElevation);
        }

        let name = wide(SERVICE_NAME);
        let bin = wide(&sys.to_string_lossy());

        let mut svc = unsafe {
            CreateServiceW(
                scm,
                name.as_ptr(),
                name.as_ptr(),
                SERVICE_ALL_ACCESS,
                SERVICE_KERNEL_DRIVER,
                SERVICE_DEMAND_START,
                SERVICE_ERROR_NORMAL,
                bin.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };

        if svc.is_null() {
            if unsafe { GetLastError() } == ERROR_SERVICE_EXISTS {
                svc = unsafe { OpenServiceW(scm, name.as_ptr(), SERVICE_ALL_ACCESS) };
            }
            if svc.is_null() {
                unsafe { CloseServiceHandle(scm) };
                return Err(DriverStatus::NeedsElevation);
            }
        }

        let started = unsafe { StartServiceW(svc, 0, std::ptr::null()) };
        let start_err = unsafe { GetLastError() };
        unsafe {
            CloseServiceHandle(svc);
            CloseServiceHandle(scm);
        }

        if started == 0 && start_err != ERROR_SERVICE_ALREADY_RUNNING {
            return Err(DriverStatus::Blocked);
        }
        Ok(())
    }

    fn ioctl(
        &self,
        code: u32,
        input: *const c_void,
        in_len: u32,
        out: *mut c_void,
        out_len: u32,
    ) -> bool {
        let mut returned: u32 = 0;
        unsafe {
            DeviceIoControl(
                self.handle,
                code,
                input,
                in_len,
                out,
                out_len,
                &mut returned,
                std::ptr::null_mut(),
            ) != 0
                && (out_len == 0 || returned == out_len)
        }
    }

    fn read_pci_dword(&self, pci_address: u32, offset: u32) -> Option<u32> {
        let input = ReadPciInput {
            pci_address,
            pci_offset: offset,
        };
        let mut value: u32 = 0;
        self.ioctl(
            IOCTL_READ_PCI_CONFIG,
            &input as *const _ as *const c_void,
            std::mem::size_of::<ReadPciInput>() as u32,
            &mut value as *mut _ as *mut c_void,
            4,
        )
        .then_some(value)
    }

    fn write_pci_dword(&self, pci_address: u32, offset: u32, value: u32) -> bool {
        let input = WritePciInput {
            pci_address,
            pci_offset: offset,
            value,
        };
        self.ioctl(
            IOCTL_WRITE_PCI_CONFIG,
            &input as *const _ as *const c_void,
            std::mem::size_of::<WritePciInput>() as u32,
            std::ptr::null_mut(),
            0,
        )
    }

    /// Read one 32-bit SMN register.
    ///
    /// Two config-space accesses against device 00:00.0: latch the address
    /// into the index register, then read the data register back.
    pub fn read_smn(&self, address: u32) -> Option<u32> {
        const ROOT_COMPLEX: u32 = 0; // bus 0, device 0, function 0
        const SMN_INDEX: u32 = 0x60;
        const SMN_DATA: u32 = 0x64;

        if !self.write_pci_dword(ROOT_COMPLEX, SMN_INDEX, address) {
            return None;
        }
        self.read_pci_dword(ROOT_COMPLEX, SMN_DATA)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // Leave the service registered: stopping and removing it on every exit
        // would demand elevation on every run. Just release our handle.
        unsafe { CloseHandle(self.handle) };
    }
}
