# tempmanager accuracy and footprint review — 2026-09-12

The app has a sound, lightweight foundation. It does not need a rewrite: native
Win32, a blocking message loop, bounded buffers, lazy popup creation, and adaptive
sampling are sensible choices. The review did find correctness bugs and misleading
footprint claims. The fixes below are in the source and in `dist/tempmanager.exe`.
This is a measured improvement pass, not a claim of optimal performance or physical
sensor calibration.

The original source is preserved in [source-before-review.zip](source-before-review.zip).
A source comparison is in [source-changes.diff](source-changes.diff). The workspace
was not a Git repository when the review began. No runtime dependencies were added;
Rust's formatter and linter were installed for verification.

The main changes are:

| Area | Finding | Result |
|---|---|---|
| History timing | Requested timer intervals were stored as if they were measured elapsed time; the first interval was counted even with only one reading. | Store monotonic timestamps and use the difference between visible endpoints. Delayed ticks, rate changes, and sleep gaps retain their actual duration. |
| Chart spacing | Readings were evenly spaced even when sampled at different rates. | Position every reading according to its timestamp. |
| CCD identity | Skipping an unavailable CCD shifted the next CCD's temperature into its row. | Match each reading to the CCD ID recorded during discovery. Missing readings leave gaps. |
| Storage parsing | The local sensor entry omitted fields, had the wrong stride, and cast arbitrary byte buffers to aligned references. Returned sizes were insufficiently checked. | Use Windows SDK sizes/offsets, decode bytes safely, validate lengths/counts, and locate the primary sensor by ID. Zero/negative temperatures and unavailable values are handled separately. |
| Temperature formatting | Negative fractions could lose their sign; negative whole degrees and Fahrenheit tenths rounded incorrectly. | Round once at the requested precision and retain the sign. |
| Driver reads | A successful IOCTL was accepted even if its output was short. | Require the complete response for register reads. |
| Buffer and allocation costs | Retained 1,200 readings while drawing at most 240; allocated drive-path strings at every read; formatted unchanged tray values. | Keep 240 readings, precompute drive paths, use fixed sample slices, and skip unchanged tray formatting. For three sensors, history/timeline payload falls from 9,600 to 3,360 bytes. |
| GDI and tray updates | A font was deleted while selected; DIB pixel access lacked an explicit GDI flush; unchanged tooltips still triggered shell calls. | Restore the previous font, flush before direct DIB access, check resource creation, release offscreen windows, and cache tooltip contents. |
| NVML lifecycle | Missing exports could leave the DLL loaded; numeric conversions could wrap; DLL lookup used the general search path. | Own the library through all failure paths, check conversions, and use documented driver locations. |
| Tooltip behavior | Explorer restart lost the tooltip; version-4 notifications omitted the standard-tooltip flag. | Restore the cached tooltip and explicitly enable standard tooltips. |
| Documentation | Claimed no sample-path allocations, identical CPU backend semantics, and a very small working set as the footprint; setup instructions contradicted one another. | Describe actual behavior, sensor resolution, memory measures, setup status, and remaining limits. |

Storage layout was checked against Microsoft's
[temperature entry](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_temperature_info)
and [descriptor](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_temperature_data_descriptor)
definitions. The graphics changes follow the documented
[DeleteObject restrictions](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-deleteobject),
[DIB synchronization requirement](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-createdibsection),
and [notification flags](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-notifyicondataw).
NVIDIA's catalog had no strong match for this Windows NVML binding; its
[API reference](https://docs.nvidia.com/deploy/nvml-api/api/group__nvmlDeviceQueries.html)
was used for the temperature interface.

Measurements used release builds compiled with Rust 1.98.0, an isolated settings
directory, a five-second startup allowance, and the popup closed. Each process
polled the RTX 5080 and two drives. These sessions were unelevated, so the CPU
backend was unavailable. The original elevated app was left running. The script
sampled process counters once per second and stopped only the process it launched.

| Build / interval | Observed duration | CPU time accrued | Private memory at end | Resident memory at end |
|---|---:|---:|---:|---:|
| Original / 30 s | 65.87 s | 0.046875 s | 21.66 MiB | 11.55 MiB |
| Revised / 30 s | 66.00 s | 0.000000 s | 21.17 MiB | 33.96 MiB |
| Original / 1 s | 35.51 s | 0.109375 s | 21.71 MiB | 4.07 MiB |
| Revised / 1 s | 35.54 s | 0.093750 s | 21.21 MiB | 33.98 MiB |

The revised 1-second run averaged about 0.264% of one CPU core. These are short,
coarsely quantized measurements on a working desktop, not statistically controlled
benchmarks. A zero CPU-time delta means no increase was visible in that counter,
not zero work. An earlier revised 30-second run accrued 0.046875 seconds, the same
as the original. There is no basis for claiming a large CPU improvement.

Resident memory is higher because forced working-set trimming was removed.
[Working-set trimming](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-setprocessworkingsetsize)
evicts resident pages; it does not release the app's private committed allocations.
Avoiding repeated trimming also avoids forcing those pages to fault back in.
Windows can still reclaim pages under memory pressure. Both builds use roughly
21–22 MiB of private memory in these unelevated runs. The original running instance
was separately observed at about 2.9 MiB resident and 22.3 MiB private, illustrating
why the old working-set-only claim was incomplete.

The executable grew from 442,880 bytes (432.5 KiB) to 448,512 bytes (438 KiB).
Both still have one direct Rust dependency, `windows-sys`. Windows and sensor
libraries created additional threads: the measured total varied from three to six.

Raw results are in [before-30s.json](before-30s.json), [before-1s.json](before-1s.json),
[final-30s.json](final-30s.json), and [final-1s.json](final-1s.json).
The repeatable script is [measure.ps1](measure.ps1). Earlier development-build
measurements are also retained as `after-*.json` to show the observed variability.

Validation completed:

- `cargo test`: 14 unit tests passed, covering ring rollover, real timestamps,
  early queued ticks, mixed-cadence chart positions, CCD failure/reordering,
  malformed/unaligned storage responses, temperature rounding, Unicode labels,
  tooltip termination, and the installed AMD x64 parameter layout.
- The additional offscreen Windows test passed: 200 popup paints in about
  0.40 seconds in a debug build; GDI objects stayed at 5 before and after.
  This checks the revised code's steady resource use, not an assertion that every
  Windows version leaked resources in the original code.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` passed.
- A release build completed. The rendered popup was visually inspected;
  [popup-after.png](popup-after.png) contains synthetic layout-test values.
- The final [probe output](probe-final.txt) reports GPU 49 C and drives 50 C / 42 C.
  `nvidia-smi` reported 49 C just before the probe and 50 C just afterward. Both
  tools use NVML, so this corroborates the integration rather than independently
  calibrating the sensor. Windows' separate storage reliability query denied
  access in this unelevated session.

Remaining limits and follow-up priorities:

- **Live CPU validation is still needed.** The AMD SDK is installed, but Init
  returned false without elevation. Its `CPUParameters` structure is 192 bytes
  with temperature at offset 72, matching the installed headers and tests.
  Neither its live readings nor the direct SMU backend were independently
  compared in this session. The C++ vtable binding remains sensitive to SDK updates.
- **Sampling can block the UI.** Hardware calls run on the message-loop thread.
  This saves a worker thread but a slow driver could delay the popup. If stalls
  are observed, a single sampling worker should be evaluated against measured
  latency and memory use. No stall was demonstrated by these tests.
- **The WinRing0 fallback is not atomic across applications.** The index/data
  requests in `src/sensors/driver.rs` can interleave with other hardware monitors.
  The AMD SDK remains the preferred backend. This review did not install drivers
  or change Windows security settings.
- **Startup controls need a separate reliability fix.** In `src/main.rs`, saved
  autostart state can claim success after registration fails. Disabling from an
  unelevated instance can leave an existing elevated scheduled task enabled.
  No startup registration was changed during this review.
- **Coverage and interpretation are limited.** Discovery is fixed at startup;
  storage enumeration is capped at 16 indices. No Intel CPU or AMD/Intel GPU
  backends exist. Threshold colors are generic, GPU/drive readings have whole-
  degree resolution, and infrequent samples cannot capture every temperature spike.
- **Some Win32 maintenance work remains.** Popup placement uses the primary
  monitor's work area even when opened elsewhere, and raw callback state deserves
  a dedicated reentrancy review before expanding the UI.

Use the revised executable at `dist/tempmanager.exe` after exiting the currently
running copy. Run it elevated if CPU temperatures are needed with this SDK.
The normal release output and the running original process were not replaced.
The revised executable's SHA-256 is
`0D02A19235A864084497E935FFF36658CD056DE5AB91D2EA038D9BC7033AC13F`.
