//! CPU temperature backends.
//!
//! There is more than one way to get a die temperature out of a Ryzen, and
//! each exposes a backend-specific package reading:
//!
//!   * `WinRing0Backend` talks to the SMU directly through a generic ring-0
//!     driver. Self-contained, but the driver is on Microsoft's
//!     vulnerable-driver blocklist, so it needs Memory Integrity turned off.
//!   * `RyzenSdkBackend` goes through AMD's own WHQL-signed driver, which
//!     loads happily under HVCI, at the cost of depending on AMD software
//!     being installed.
//!
//! Both return Celsius, but the SDK's temperature and the SMU Tdie reading
//! need not have identical sampling/averaging semantics.

use super::driver::{Driver, DriverStatus};

pub trait CpuBackend {
    /// Human-readable processor name for the flyout row.
    fn label(&self) -> String;

    /// Package temperature (Tctl/Tdie) in tenths of a degree Celsius.
    fn read_package(&mut self) -> Option<i16>;

    /// Per-CCD temperatures, appended as (ccd_index, deci_c).
    ///
    /// Backends that cannot break the package down leave `out` empty; callers
    /// treat that as "one channel only" rather than an error.
    fn read_ccds(&mut self, out: &mut Vec<(u32, i16)>) {
        out.clear();
    }

    /// Which backend this is, for the flyout's status line.
    fn source_name(&self) -> &'static str;

    /// Minimum sensible gap between reads, in milliseconds. AMD's SDK
    /// documents that querying faster than once a second loads the SMU and
    /// degrades the readings; the direct register path has no such limit.
    fn min_interval_ms(&self) -> u32 {
        0
    }
}

/// Open the best available CPU backend.
///
/// AMD's SDK is preferred when present: it is the only option that works
/// without asking the user to weaken Memory Integrity.
pub fn open() -> Result<Box<dyn CpuBackend>, DriverStatus> {
    if let Some(sdk) = super::ryzen_sdk::open() {
        return Ok(sdk);
    }

    // The SDK is installed but would not initialise. Its Platform.dll refuses
    // to start for a non-elevated caller even when AMD's driver service is
    // already running, which is the same restriction Ryzen Master itself has.
    // Say so, rather than falling through to a WinRing0 message about a driver
    // the user does not need.
    if super::ryzen_sdk::locate().is_some() && !is_elevated() {
        return Err(DriverStatus::NeedsElevation);
    }

    let Some(layout) = super::amd::detect() else {
        return Err(DriverStatus::Unsupported);
    };

    match Driver::open() {
        Ok(driver) => Ok(Box::new(WinRing0Backend { layout, driver })),
        Err(status) => Err(status),
    }
}

/// Whether this process is running with an elevated token.
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Direct SMU register access over a generic ring-0 driver.
pub struct WinRing0Backend {
    layout: super::amd::CpuLayout,
    driver: Driver,
}

impl CpuBackend for WinRing0Backend {
    fn label(&self) -> String {
        super::amd::short_brand(&self.layout.brand)
    }

    fn read_package(&mut self) -> Option<i16> {
        self.layout.read_package(&self.driver)
    }

    fn read_ccds(&mut self, out: &mut Vec<(u32, i16)>) {
        self.layout.read_ccds(&self.driver, out);
    }

    fn source_name(&self) -> &'static str {
        "SMU (WinRing0)"
    }
}
