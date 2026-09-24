//! Sensor discovery and sampling.
//!
//! Every source is probed exactly once at startup and the resulting channel
//! list is then fixed for the process lifetime. That keeps the sample path
//! allocation-free and lets the history ring buffers map to channels by index
//! instead of by name lookup.

pub mod amd;
pub mod cpu;
pub mod driver;
pub mod nvml;
pub mod ryzen_sdk;
pub mod storage;

use crate::history::NO_VALUE;
use cpu::CpuBackend;
use driver::DriverStatus;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cpu,
    Gpu,
    Storage,
}

pub struct Channel {
    pub label: String,
    pub kind: Kind,
    /// Tenths of a degree C at which the reading turns amber, then red.
    pub warn: i16,
    pub hot: i16,
}

pub struct Sensors {
    pub channels: Vec<Channel>,
    pub cpu_status: DriverStatus,
    /// Which backend won, for the flyout's status line. Empty when none did.
    pub cpu_source: &'static str,
    cpu: Option<Box<dyn CpuBackend>>,
    nvml: Option<nvml::Nvml>,
    drives: Vec<storage::Drive>,
    /// Reused between samples so reading CCDs allocates nothing.
    ccd_scratch: Vec<(u32, i16)>,
    /// Stable CCD identities, even if one read temporarily fails.
    ccd_ids: Vec<u32>,
    /// Number of leading channels that come from the CPU.
    cpu_channels: usize,
    gpu_channels: usize,
}

impl Sensors {
    #[cfg(test)]
    pub(crate) fn test_fixture(channels: Vec<Channel>) -> Self {
        Self {
            channels,
            cpu_status: DriverStatus::Ready,
            cpu_source: "test",
            cpu: None,
            nvml: None,
            drives: Vec::new(),
            ccd_scratch: Vec::with_capacity(16),
            ccd_ids: Vec::new(),
            cpu_channels: 0,
            gpu_channels: 0,
        }
    }

    pub fn probe() -> Self {
        let mut channels = Vec::new();

        // --- CPU ----------------------------------------------------------
        let (mut cpu, cpu_status, cpu_source) = match cpu::open() {
            Ok(backend) => {
                let name = backend.source_name();
                (Some(backend), DriverStatus::Ready, name)
            }
            Err(status) => (None, status, ""),
        };

        let mut ccd_scratch = Vec::with_capacity(16);
        let mut ccd_ids = Vec::new();
        let mut cpu_channels = 0;

        if let Some(backend) = cpu.as_mut() {
            channels.push(Channel {
                label: backend.label(),
                kind: Kind::Cpu,
                warn: 750,
                hot: 900,
            });
            cpu_channels += 1;

            // Only expose per-CCD tiles for parts that actually have more than
            // one; on a single-CCD chip they would just duplicate the package.
            backend.read_ccds(&mut ccd_scratch);
            if ccd_scratch.len() > 1 {
                for (i, _) in &ccd_scratch {
                    ccd_ids.push(*i);
                    channels.push(Channel {
                        label: format!("CCD{i}"),
                        kind: Kind::Cpu,
                        warn: 750,
                        hot: 900,
                    });
                    cpu_channels += 1;
                }
            }
        }

        // --- GPU (user-mode, no driver) -----------------------------------
        let nvml = nvml::Nvml::open();
        let mut gpu_channels = 0;
        if let Some(n) = nvml.as_ref() {
            for name in n.device_names() {
                channels.push(Channel {
                    label: name.to_string(),
                    kind: Kind::Gpu,
                    warn: 700,
                    hot: 840,
                });
                gpu_channels += 1;
            }
        }

        // --- Drives (user-mode, no driver) --------------------------------
        let drives = storage::detect();
        for d in &drives {
            channels.push(Channel {
                label: d.label.clone(),
                kind: Kind::Storage,
                warn: 600,
                hot: 700,
            });
        }

        Self {
            channels,
            cpu_status,
            cpu_source,
            cpu,
            nvml,
            drives,
            ccd_scratch,
            ccd_ids,
            cpu_channels,
            gpu_channels,
        }
    }

    /// Fill `out` with one reading per channel, in channel order.
    /// Failed reads become `NO_VALUE` so the graph shows a gap rather than a
    /// stale value held over from the previous sample.
    /// Returns whether any displayed value changed. The caller owns a fixed
    /// slice, so no read can grow its allocation.
    pub fn sample_into(&mut self, out: &mut [i16]) -> bool {
        assert_eq!(out.len(), self.channels.len());
        let mut slot = 0;
        let mut changed = false;
        let mut put = |value| {
            changed |= out[slot] != value;
            out[slot] = value;
            slot += 1;
        };

        if let Some(backend) = self.cpu.as_mut() {
            put(backend.read_package().unwrap_or(NO_VALUE));

            if self.cpu_channels > 1 {
                backend.read_ccds(&mut self.ccd_scratch);
                // cpu_channels is 1 package + N CCDs fixed at probe time.
                for id in &self.ccd_ids {
                    let v = self
                        .ccd_scratch
                        .iter()
                        .find(|(i, _)| i == id)
                        .map(|(_, t)| *t)
                        .unwrap_or(NO_VALUE);
                    put(v);
                }
            }
        }

        if let Some(n) = self.nvml.as_ref() {
            for i in 0..self.gpu_channels {
                put(n.read(i).unwrap_or(NO_VALUE));
            }
        }

        for d in &self.drives {
            put(d.read().unwrap_or(NO_VALUE));
        }

        debug_assert_eq!(slot, self.channels.len());
        changed
    }

    /// Shortest sampling interval any live backend is willing to sustain.
    pub fn min_interval_s(&self) -> u32 {
        let ms = self.cpu.as_ref().map(|c| c.min_interval_ms()).unwrap_or(0);
        ms.div_ceil(1000)
    }

    /// Index of the channel the tray icon should track for "hottest".
    pub fn hottest(&self, latest: &[i16]) -> Option<usize> {
        latest
            .iter()
            .enumerate()
            .filter(|(_, v)| **v != NO_VALUE)
            .max_by_key(|(_, v)| **v)
            .map(|(i, _)| i)
    }

    pub fn first_of(&self, kind: Kind) -> Option<usize> {
        self.channels.iter().position(|c| c.kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IntermittentCpu {
        sample: usize,
    }

    impl CpuBackend for IntermittentCpu {
        fn label(&self) -> String {
            "test CPU".into()
        }
        fn source_name(&self) -> &'static str {
            "test"
        }
        fn read_package(&mut self) -> Option<i16> {
            Some(800)
        }
        fn read_ccds(&mut self, out: &mut Vec<(u32, i16)>) {
            out.clear();
            match self.sample {
                0 => out.extend_from_slice(&[(0, 510), (2, 720)]),
                1 => out.push((2, 730)),
                _ => out.extend_from_slice(&[(2, 740), (0, 520)]),
            }
            self.sample += 1;
        }
    }

    #[test]
    fn failed_and_reordered_ccds_never_change_channel_identity() {
        let channels = ["package", "CCD0", "CCD2"]
            .into_iter()
            .map(|label| Channel {
                label: label.into(),
                kind: Kind::Cpu,
                warn: 750,
                hot: 900,
            })
            .collect();
        let mut sensors = Sensors::test_fixture(channels);
        sensors.cpu = Some(Box::new(IntermittentCpu { sample: 0 }));
        sensors.cpu_channels = 3;
        sensors.ccd_ids = vec![0, 2];
        let mut out = vec![NO_VALUE; 3];
        sensors.sample_into(&mut out);
        assert_eq!(out, [800, 510, 720]);
        sensors.sample_into(&mut out);
        assert_eq!(out, [800, NO_VALUE, 730]);
        sensors.sample_into(&mut out);
        assert_eq!(out, [800, 520, 740]);
    }
}
