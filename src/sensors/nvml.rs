//! NVIDIA GPU temperature via NVML.
//!
//! `nvml.dll` ships with the display driver, in System32 or NVIDIA's NVSMI
//! directory. No additional kernel driver or admin rights are needed. We resolve
//! it with LoadLibrary/GetProcAddress rather than link-time import so the app
//! still starts cleanly on a machine with no NVIDIA GPU.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

type NvmlDevice = *mut c_void;

// NVML returns 0 (NVML_SUCCESS) or a nonzero error code.
const NVML_SUCCESS: i32 = 0;
const NVML_TEMPERATURE_GPU: u32 = 0;

type FnInit = unsafe extern "C" fn() -> i32;
type FnShutdown = unsafe extern "C" fn() -> i32;
type FnDeviceGetCount = unsafe extern "C" fn(*mut u32) -> i32;
type FnDeviceGetHandle = unsafe extern "C" fn(u32, *mut NvmlDevice) -> i32;
type FnDeviceGetTemp = unsafe extern "C" fn(NvmlDevice, u32, *mut u32) -> i32;
type FnDeviceGetName = unsafe extern "C" fn(NvmlDevice, *mut u8, u32) -> i32;

struct Library(HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.0) };
    }
}

pub struct Nvml {
    _module: Library,
    shutdown: FnShutdown,
    get_temp: FnDeviceGetTemp,
    /// (handle, friendly name) for every GPU found at init.
    devices: Vec<(NvmlDevice, String)>,
}

macro_rules! sym {
    ($module:expr, $name:literal, $ty:ty) => {{
        let p = unsafe { GetProcAddress($module, concat!($name, "\0").as_ptr()) };
        match p {
            Some(p) => {
                Some(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(p) })
            }
            None => None,
        }
    }};
}

impl Nvml {
    pub fn open() -> Option<Self> {
        let name: Vec<u16> = "nvml.dll\0".encode_utf16().collect();
        let mut module = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            // Standard (non-DCH) drivers use NVIDIA's NVSMI directory.
            let root =
                std::env::var_os("ProgramW6432").or_else(|| std::env::var_os("ProgramFiles"))?;
            let path = std::path::PathBuf::from(root).join(r"NVIDIA Corporation\NVSMI\nvml.dll");
            let path: Vec<u16> = path
                .as_os_str()
                .to_string_lossy()
                .encode_utf16()
                .chain(Some(0))
                .collect();
            module = unsafe {
                LoadLibraryExW(
                    path.as_ptr(),
                    std::ptr::null_mut(),
                    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
                )
            };
        }
        if module.is_null() {
            return None;
        }
        // Own the DLL before resolving symbols so every early return unloads it.
        let library = Library(module);

        // The _v2 entry points are the modern ABI; fall back to the legacy
        // names on very old drivers.
        let init =
            sym!(module, "nvmlInit_v2", FnInit).or_else(|| sym!(module, "nvmlInit", FnInit))?;
        let shutdown = sym!(module, "nvmlShutdown", FnShutdown)?;
        let get_count = sym!(module, "nvmlDeviceGetCount_v2", FnDeviceGetCount)
            .or_else(|| sym!(module, "nvmlDeviceGetCount", FnDeviceGetCount))?;
        let get_handle = sym!(module, "nvmlDeviceGetHandleByIndex_v2", FnDeviceGetHandle)
            .or_else(|| sym!(module, "nvmlDeviceGetHandleByIndex", FnDeviceGetHandle))?;
        let get_temp = sym!(module, "nvmlDeviceGetTemperature", FnDeviceGetTemp)?;
        let get_name = sym!(module, "nvmlDeviceGetName", FnDeviceGetName);

        if unsafe { init() } != NVML_SUCCESS {
            return None;
        }

        let mut count: u32 = 0;
        if unsafe { get_count(&mut count) } != NVML_SUCCESS || count == 0 {
            unsafe { shutdown() };
            return None;
        }

        let mut devices = Vec::with_capacity(count as usize);
        for i in 0..count {
            let mut dev: NvmlDevice = std::ptr::null_mut();
            if unsafe { get_handle(i, &mut dev) } != NVML_SUCCESS {
                continue;
            }
            let name = get_name
                .and_then(|f| {
                    let mut buf = [0u8; 96];
                    (unsafe { f(dev, buf.as_mut_ptr(), buf.len() as u32) } == NVML_SUCCESS).then(
                        || {
                            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                            String::from_utf8_lossy(&buf[..end]).into_owned()
                        },
                    )
                })
                .unwrap_or_else(|| format!("GPU {i}"));
            devices.push((dev, shorten(&name)));
        }

        if devices.is_empty() {
            unsafe { shutdown() };
            return None;
        }

        Some(Self {
            _module: library,
            shutdown,
            get_temp,
            devices,
        })
    }

    pub fn device_names(&self) -> impl Iterator<Item = &str> {
        self.devices.iter().map(|(_, n)| n.as_str())
    }

    /// Core temperature in tenths of a degree C (sensor precision is 1 C).
    pub fn read(&self, idx: usize) -> Option<i16> {
        let (dev, _) = self.devices.get(idx)?;
        let mut t: u32 = 0;
        if unsafe { (self.get_temp)(*dev, NVML_TEMPERATURE_GPU, &mut t) } != NVML_SUCCESS {
            return None;
        }
        i16::try_from(t).ok()?.checked_mul(10)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
        }
    }
}

/// "NVIDIA GeForce RTX 5080" -> "RTX 5080" so it fits the popup's label column.
fn shorten(name: &str) -> String {
    name.trim()
        .trim_start_matches("NVIDIA ")
        .trim_start_matches("GeForce ")
        .to_string()
}
