//! AMD Ryzen Master Monitoring SDK backend.
//!
//! Goes through AMD's own WHQL-signed driver, so unlike the WinRing0 path this
//! works with Memory Integrity enabled.
//!
//! The SDK is a C++ virtual-interface API. `GetPlatform` is declared
//! `extern "C"` so it resolves by plain name, but everything after it is a
//! virtual call, which means reproducing MSVC's vtable slot order:
//!
//! ```cpp
//! class IPlatform {                        // no virtual destructor
//!   virtual bool Init(const char*, bool) = 0;              // slot 0
//!   virtual bool UnInit(void) = 0;                         // slot 1
//!   virtual IDeviceManager& GetIDeviceManager(void) = 0;   // slot 2
//! };
//!
//! class IDevice {
//!   virtual bool Init(unsigned long) = 0;                  // slot 0
//!   virtual bool UnInit(void) = 0;                         // slot 1
//!   virtual const wchar_t* GetName(void) = 0;              // slot 2
//!   virtual const wchar_t* GetDescription(void) = 0;       // slot 3
//!   virtual const wchar_t* GetVendor(void) = 0;            // slot 4
//!   virtual const wchar_t* GetRole(void) = 0;              // slot 5
//!   virtual const wchar_t* GetClassName(void) = 0;         // slot 6
//!   virtual AOD_DEVICE_TYPE GetType(void) = 0;             // slot 7
//!   virtual unsigned long GetIndex(void) = 0;              // slot 8
//!   virtual ~IDevice() {}                                  // slot 9
//! };
//!
//! class ICPUEx : public IDevice {   // overrides reuse slots 2..8
//!   ... GetL1DataCache 10, GetL1InstructionCache 11, GetL2Cache 12,
//!       GetL3Cache 13, GetCoreCount 14, GetCorePark 15, GetPackage 16,
//!       GetCPUParameters 17, GetChipsetName 18, GetFamily 19,
//!       GetStepping 20, GetModel 21
//! };
//! ```
//!
//! Two slot indices are deliberately *not* trusted on faith:
//!
//!   * `IDeviceManager::GetDevice` is overloaded, and MSVC emits consecutive
//!     overloads in reverse declaration order. Rather than depend on that, we
//!     call the candidate slots with `(dtCPU, 0)` -- and since `dtCPU == 0`,
//!     both overloads do the same thing with those arguments, so either
//!     ordering yields device 0.
//!   * Before reading a temperature we verify the object really is the CPU
//!     (`GetType() == dtCPU`) and that `GetCoreCount` returns success with a
//!     plausible count. If the slot map were wrong, those checks fail and we
//!     report the backend as unavailable instead of returning garbage.

use super::cpu::CpuBackend;
use std::ffi::c_void;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

// --- AOD_DEVICE_TYPE ------------------------------------------------------
const DT_CPU: i32 = 0;

// --- AOD_STATUS_CODE ------------------------------------------------------
const B_SUCCESS: i32 = 0;

// --- vtable slot indices --------------------------------------------------
const IPLATFORM_INIT: usize = 0;
const IPLATFORM_UNINIT: usize = 1;
const IPLATFORM_GET_DEVICE_MANAGER: usize = 2;

/// Candidate slots for the two `GetDevice` overloads.
const IDEVICEMANAGER_GET_DEVICE_A: usize = 2;
const IDEVICEMANAGER_GET_DEVICE_B: usize = 3;

const IDEVICE_GET_NAME: usize = 2;
const IDEVICE_GET_TYPE: usize = 7;
const ICPUEX_GET_CORE_COUNT: usize = 14;
const ICPUEX_GET_CPU_PARAMETERS: usize = 17;

/// Upper bound on cores we will ask the SDK to fill in. The arrays are
/// caller-allocated; oversizing them costs a few KB once and removes any
/// chance of the SDK writing past the end.
const MAX_CORES: usize = 256;

#[repr(C)]
struct EffectiveFreqData {
    u_length: u32,
    d_freq: *mut f64,
    d_state: *mut f64,
    d_current_freq: *mut f64,
    d_current_temp: *mut f64,
}

/// Mirrors `CPUParameters` from IDevice.h field for field. `repr(C)` gives the
/// same padding MSVC does: `e_mode` is 4 bytes followed by 4 bytes of padding,
/// because `EffectiveFreqData` is 8-byte aligned.
#[repr(C)]
struct CpuParameters {
    e_mode: u32,
    st_freq_data: EffectiveFreqData,
    d_peak_core_voltage: f64,
    d_peak_core_voltage_1: f64,
    d_soc_voltage: f64,
    d_temperature: f64,
    d_avg_core_voltage: f64,
    d_avg_core_voltage_1: f64,
    d_peak_speed: f64,
    f_ppt_limit: f32,
    f_ppt_value: f32,
    f_tdc_limit_vdd: f32,
    f_tdc_value_vdd: f32,
    f_tdc_value_vdd_1: f32,
    f_edc_limit_vdd: f32,
    f_edc_value_vdd: f32,
    f_edc_value_vdd_1: f32,
    f_chtc_limit: f32,
    f_fclk_p0_freq: f32,
    f_cclk_fmax: f32,
    f_tdc_limit_soc: f32,
    f_tdc_value_soc: f32,
    f_edc_limit_soc: f32,
    f_edc_value_soc: f32,
    f_vddcr_vdd_power: f32,
    f_vddcr_soc_power: f32,
    f_tdc_limit_ccd: f32,
    f_tdc_value_ccd: f32,
    f_edc_limit_ccd: f32,
    f_edc_value_ccd: f32,
}

type FnGetPlatform = unsafe extern "C" fn() -> *mut c_void;
type FnInitPlatform = unsafe extern "system" fn(*mut c_void, *const u8, bool) -> bool;
type FnUnInit = unsafe extern "system" fn(*mut c_void) -> bool;
type FnGetDeviceManager = unsafe extern "system" fn(*mut c_void) -> *mut c_void;
type FnGetDevice = unsafe extern "system" fn(*mut c_void, i32, u32) -> *mut c_void;
type FnGetType = unsafe extern "system" fn(*mut c_void) -> i32;
type FnGetName = unsafe extern "system" fn(*mut c_void) -> *const u16;
type FnGetCoreCount = unsafe extern "system" fn(*mut c_void, *mut u32) -> i32;
type FnGetCpuParameters = unsafe extern "system" fn(*mut c_void, *mut CpuParameters) -> i32;

/// Fetch entry `index` from the object's vtable.
///
/// # Safety
/// `obj` must be a live pointer to a C++ object with a vtable pointer at
/// offset 0, and `index` must be within that vtable.
unsafe fn vslot(obj: *mut c_void, index: usize) -> *const c_void {
    let vtbl = *(obj as *const *const *const c_void);
    *vtbl.add(index)
}

const SEARCH_ROOTS: [&str; 3] = [
    r"C:\Program Files\AMD\RyzenMasterMonitoringSDK",
    r"C:\Program Files (x86)\AMD\RyzenMasterMonitoringSDK",
    r"C:\Program Files\AMD\RyzenMaster",
];

pub struct SdkLocation {
    pub root: PathBuf,
    pub dll: Option<PathBuf>,
}

/// Look for an installed SDK. Reported by `--probe` so the setup state is
/// visible without guesswork.
pub fn locate() -> Option<SdkLocation> {
    for root in SEARCH_ROOTS {
        let root = PathBuf::from(root);
        if !root.exists() {
            continue;
        }
        let dll = ["bin", "."]
            .iter()
            .map(|sub| root.join(sub).join("Platform.dll"))
            .find(|p| p.exists());
        return Some(SdkLocation { root, dll });
    }
    None
}

pub struct RyzenSdk {
    lib: HMODULE,
    platform: *mut c_void,
    cpu: *mut c_void,
    brand: String,
    /// Caller-allocated arrays that `EffectiveFreqData` points into. These must
    /// outlive every `GetCPUParameters` call, so they live here rather than on
    /// the stack of the read.
    freq: Vec<f64>,
    state: Vec<f64>,
    cur_freq: Vec<f64>,
    cur_temp: Vec<f64>,
    core_count: u32,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn wide_to_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < 256 && *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}

/// Append a step result to the diagnostic log, if one is being kept.
macro_rules! step {
    ($log:expr, $($arg:tt)*) => {
        if let Some(l) = $log.as_deref_mut() {
            l.push_str(&format!($($arg)*));
            l.push('\n');
        }
    };
}

impl RyzenSdk {
    fn open_inner(mut log: Option<&mut String>) -> Option<Self> {
        let Some(loc) = locate() else {
            step!(log, "locate: SDK not found");
            return None;
        };
        let Some(dll_path) = loc.dll else {
            step!(
                log,
                "locate: Platform.dll missing under {}",
                loc.root.display()
            );
            return None;
        };
        step!(log, "locate: {}", dll_path.display());

        // LOAD_WITH_ALTERED_SEARCH_PATH makes Platform.dll's own directory the
        // search root, so its Device.dll and Qt dependencies resolve without
        // touching the process-wide DLL search path.
        let wpath = wide(&dll_path.to_string_lossy());
        let lib = unsafe {
            LoadLibraryExW(
                wpath.as_ptr(),
                std::ptr::null_mut(),
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        };
        if lib.is_null() {
            step!(log, "LoadLibraryEx: FAILED (err {})", unsafe {
                windows_sys::Win32::Foundation::GetLastError()
            });
            return None;
        }
        step!(log, "LoadLibraryEx: ok");

        let get_platform: FnGetPlatform = unsafe {
            match GetProcAddress(lib, c"GetPlatform".as_ptr() as *const u8) {
                Some(p) => {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, FnGetPlatform>(p)
                }
                None => {
                    step!(log, "GetProcAddress(GetPlatform): FAILED");
                    FreeLibrary(lib);
                    return None;
                }
            }
        };

        unsafe {
            let platform = get_platform();
            step!(log, "GetPlatform: {:p}", platform);
            if platform.is_null() {
                FreeLibrary(lib);
                return None;
            }

            // Init(pszBoardVendor = NULL, bUseCPUOnly = false), matching the
            // defaults AMD's own sample relies on. Defaults in C++
            // are applied at the call site, so both arguments must be passed
            // explicitly.
            let init: FnInitPlatform = std::mem::transmute(vslot(platform, IPLATFORM_INIT));
            let ok = init(platform, std::ptr::null(), false);
            step!(log, "IPlatform::Init -> {}", ok);
            if !ok {
                FreeLibrary(lib);
                return None;
            }

            let get_dm: FnGetDeviceManager =
                std::mem::transmute(vslot(platform, IPLATFORM_GET_DEVICE_MANAGER));
            let dm = get_dm(platform);
            step!(log, "GetIDeviceManager: {:p}", dm);
            if dm.is_null() {
                Self::teardown(platform, lib);
                return None;
            }

            // Try both candidate GetDevice slots. With (dtCPU, 0) the two
            // overloads are indistinguishable, so whichever slot holds which
            // signature, index 0 of the CPU type is what comes back.
            let mut cpu = std::ptr::null_mut();
            for slot in [IDEVICEMANAGER_GET_DEVICE_A, IDEVICEMANAGER_GET_DEVICE_B] {
                let get_device: FnGetDevice = std::mem::transmute(vslot(dm, slot));
                let candidate = get_device(dm, DT_CPU, 0);
                if candidate.is_null() {
                    continue;
                }
                // Confirm we are holding a CPU device before trusting it.
                let get_type: FnGetType = std::mem::transmute(vslot(candidate, IDEVICE_GET_TYPE));
                let ty = get_type(candidate);
                step!(log, "slot {}: device {:p} type {}", slot, candidate, ty);
                if ty == DT_CPU {
                    cpu = candidate;
                    break;
                }
            }
            if cpu.is_null() {
                Self::teardown(platform, lib);
                return None;
            }

            // Second slot-map check: a working GetCoreCount proves the ICPUEx
            // half of the vtable lines up, not just the IDevice half.
            let get_core_count: FnGetCoreCount =
                std::mem::transmute(vslot(cpu, ICPUEX_GET_CORE_COUNT));
            let mut cores: u32 = 0;
            let rc = get_core_count(cpu, &mut cores);
            step!(log, "GetCoreCount -> rc {} cores {}", rc, cores);
            if rc != B_SUCCESS || cores == 0 || cores as usize > MAX_CORES {
                Self::teardown(platform, lib);
                return None;
            }

            let get_name: FnGetName = std::mem::transmute(vslot(cpu, IDEVICE_GET_NAME));
            let brand = wide_to_string(get_name(cpu));

            Some(Self {
                lib,
                platform,
                cpu,
                brand: super::amd::short_brand(&brand),
                freq: vec![0.0; MAX_CORES],
                state: vec![0.0; MAX_CORES],
                cur_freq: vec![0.0; MAX_CORES],
                cur_temp: vec![0.0; MAX_CORES],
                core_count: cores,
            })
        }
    }

    unsafe fn teardown(platform: *mut c_void, lib: HMODULE) {
        let uninit: FnUnInit = std::mem::transmute(vslot(platform, IPLATFORM_UNINIT));
        uninit(platform);
        FreeLibrary(lib);
    }

    fn query(&mut self) -> Option<CpuParameters> {
        let mut params: CpuParameters = unsafe { std::mem::zeroed() };
        params.st_freq_data = EffectiveFreqData {
            u_length: self.core_count,
            d_freq: self.freq.as_mut_ptr(),
            d_state: self.state.as_mut_ptr(),
            d_current_freq: self.cur_freq.as_mut_ptr(),
            d_current_temp: self.cur_temp.as_mut_ptr(),
        };

        let rc = unsafe {
            let f: FnGetCpuParameters =
                std::mem::transmute(vslot(self.cpu, ICPUEX_GET_CPU_PARAMETERS));
            f(self.cpu, &mut params)
        };
        (rc == B_SUCCESS).then_some(params)
    }
}

impl Drop for RyzenSdk {
    fn drop(&mut self) {
        unsafe { Self::teardown(self.platform, self.lib) };
    }
}

impl CpuBackend for RyzenSdk {
    fn label(&self) -> String {
        if self.brand.is_empty() {
            "CPU".to_string()
        } else {
            self.brand.clone()
        }
    }

    fn read_package(&mut self) -> Option<i16> {
        let p = self.query()?;
        let deci = (p.d_temperature * 10.0).round();
        // Same sanity window the register path uses.
        (deci > -490.0 && deci < 2069.0).then_some(deci as i16)
    }

    fn source_name(&self) -> &'static str {
        "AMD Ryzen Master SDK"
    }

    fn min_interval_ms(&self) -> u32 {
        // AMD documents that GetCPUParameters should not be called more than
        // once a second; faster polling loads the SMU and skews the readings.
        1000
    }
}

/// Open the SDK backend, or `None` if the SDK is absent or does not respond.
pub fn open() -> Option<Box<dyn CpuBackend>> {
    RyzenSdk::open_inner(None).map(|s| Box::new(s) as Box<dyn CpuBackend>)
}

/// Walk the same initialisation sequence, recording the outcome of each step.
///
/// "The SDK did not come up" is not an actionable diagnosis -- this says which
/// call refused, which is usually the difference between "needs elevation" and
/// "the vtable map is wrong".
pub fn diagnose() -> String {
    let mut log = String::new();
    match RyzenSdk::open_inner(Some(&mut log)) {
        Some(mut sdk) => {
            log.push_str(&format!("brand: {}\n", sdk.label()));
            match sdk.query() {
                Some(p) => {
                    log.push_str(&format!(
                        "GetCPUParameters: ok\n  temperature {:.2} C\n  peak speed {:.0} MHz\n  PPT {:.1}/{:.1} W\n",
                        p.d_temperature, p.d_peak_speed, p.f_ppt_value, p.f_ppt_limit
                    ));
                    // Per-core temperatures land in the caller-supplied array.
                    let n = (sdk.core_count as usize).min(sdk.cur_temp.len());
                    let cores: Vec<String> = sdk.cur_temp[..n]
                        .iter()
                        .map(|t| format!("{t:.1}"))
                        .collect();
                    log.push_str(&format!("  per-core temp: {}\n", cores.join(", ")));
                }
                None => log.push_str("GetCPUParameters: FAILED\n"),
            }
        }
        None => log.push_str("result: backend unavailable\n"),
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_layout_matches_installed_amd_sdk_x64_headers() {
        assert_eq!(std::mem::size_of::<EffectiveFreqData>(), 40);
        assert_eq!(std::mem::size_of::<CpuParameters>(), 192);
        assert_eq!(std::mem::offset_of!(CpuParameters, st_freq_data), 8);
        assert_eq!(std::mem::offset_of!(CpuParameters, d_temperature), 72);
        assert_eq!(std::mem::offset_of!(CpuParameters, f_ppt_limit), 104);
        assert_eq!(std::mem::offset_of!(CpuParameters, f_edc_value_ccd), 184);
    }
}
