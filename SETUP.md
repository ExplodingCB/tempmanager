# Enabling CPU temperature

GPU and supported storage temperatures do not require elevation. Ryzen CPU
temperature requires a supported hardware-monitoring interface and driver.

## Preferred path: AMD Ryzen Master Monitoring SDK

The CPU backend is implemented in `src/sensors/ryzen_sdk.rs`. It loads AMD's
`Platform.dll`, obtains the CPU device, and reads `CPUParameters.dTemperature`.
The Rust x64 structure layout was checked against the installed AMD headers
in the September 2026 review, with regression assertions for its size and offsets.
It remains a version-sensitive C++ ABI binding, not a stable C API.

Install a compatible SDK from
[AMD's SDK page](https://www.amd.com/en/developer/ryzen-master-monitoring-sdk.html).
The app searches these locations for `bin/Platform.dll` or `Platform.dll`:

- `C:\Program Files\AMD\RyzenMasterMonitoringSDK`
- `C:\Program Files (x86)\AMD\RyzenMasterMonitoringSDK`
- `C:\Program Files\AMD\RyzenMaster`

This backend uses AMD's own driver and does not require the app to disable
Memory Integrity. Compatibility still depends on the SDK, CPU, and Windows setup.
The app does not install the SDK or change Windows security settings.

On the SDK installed on this machine, initialization requires an elevated
process even when the driver service is already running. The tray menu's
**Restart as administrator** command can request that elevation. Alternatively,
launch the executable as administrator. Without elevation the other sensors
continue to work.

Run `tempmanager.exe --probe` and inspect `probe.txt` next to that executable.
A working CPU path reports `cpu backend: ready`, its source, and a CPU reading.
If initialization fails, the file includes the SDK location and failing step.
The September 2026 review session was unelevated: SDK presence and ABI layout
were verified, but fresh CPU temperatures could not be validated in that session.

## Starting elevated at login

Toggling **Start with Windows** while elevated asks Windows to create a logon
scheduled task named `tempmanager` with highest privileges. When unelevated,
the app instead writes a per-user Run-key entry, which cannot itself elevate.

The existing startup implementation has two limitations: it does not surface
all registration errors, and disabling from an unelevated process can leave a
previously created elevated task in place. Manage that task from an elevated
instance or Task Scheduler when needed. Startup registrations were not changed
during the code review.

## Legacy direct-register fallback

When the SDK cannot be used, supported AMD CPUs can use
`src/sensors/amd.rs` with the client in `src/sensors/driver.rs`. The client first
tries the existing `WinRing0_1_2_0` device. If it is unavailable and a
`WinRing0x64.sys` is placed next to the executable, it attempts to register and
start that driver service. This can require administrator rights.

WinRing0 has known security and Windows compatibility limitations. The review
did not install it, disable Memory Integrity, or validate this backend against
live hardware. A driver-open failure does not by itself prove that Memory
Integrity caused the failure; the UI now reports a generic unavailable status.

The decoder follows the AMD family/model mapping and arithmetic documented in
the [Linux k10temp source](https://github.com/torvalds/linux/blob/master/drivers/hwmon/k10temp.c).
Direct SMU access uses a pair of PCI requests; it is not atomic with competing
hardware-monitoring programs. Prefer the AMD SDK where available.

## Interpreting readings

The SDK package temperature and direct SMU Tdie value can use different
averaging. CCD values describe individual dies. Compare matching sensors and
sampling intervals when checking against another monitor. Color thresholds in
this app are generic display choices, not the CPU manufacturer's exact limits.
