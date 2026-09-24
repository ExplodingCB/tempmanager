//! AMD Zen die temperature via the SMU thermal registers.
//!
//! Register layout and the conversion arithmetic follow the Linux `k10temp`
//! driver (drivers/hwmon/k10temp.c), cross-checked against
//! LibreHardwareMonitor's `Amd17Cpu.cs`. Both agree; k10temp's formulation is
//! used here because it validates the CCD register explicitly rather than
//! folding the valid bit into a magic constant.

use super::driver::Driver;

/// Tctl control register. Same address on every Zen generation so far.
const ZEN_REPORTED_TEMP_CTRL_BASE: u32 = 0x0005_9800;

const ZEN_CUR_TEMP_SHIFT: u32 = 21;
const ZEN_CUR_TEMP_RANGE_SEL_MASK: u32 = 1 << 19;
const ZEN_CUR_TEMP_TJ_SEL_MASK: u32 = 0x0003_0000;

/// Per-CCD temperature registers live at BASE + offset + index*4.
const ZEN_CCD_TEMP_VALID: u32 = 1 << 11;
const ZEN_CCD_TEMP_MASK: u32 = 0x7FF;

pub struct CpuLayout {
    /// Offset from ZEN_REPORTED_TEMP_CTRL_BASE to the first CCD register.
    ccd_offset: u32,
    /// How many CCD slots to probe.
    ccd_count: u32,
    /// Fixed Tctl->Tdie correction for the handful of parts that need one.
    /// Zero on everything Zen 3 and newer.
    tctl_offset_deci_c: i16,
    pub brand: String,
}

/// Read family/model out of CPUID leaf 1 and pick the right register layout.
///
/// Returns `None` on non-AMD or on a family this decoder has not been
/// verified against, rather than guessing at register offsets.
pub fn detect() -> Option<CpuLayout> {
    let (family, model, brand) = cpuid_identity()?;

    // Offsets straight from k10temp's per-family table.
    let (ccd_offset, ccd_count) = match (family, model) {
        (0x17 | 0x18, 0x01 | 0x08 | 0x11 | 0x18) => (0x154, 4),
        (0x17 | 0x18, 0x31 | 0x47 | 0x60 | 0x68 | 0x71) => (0x154, 8),
        (0x17 | 0x18, 0xa0..=0xaf) => (0x300, 8),
        (0x19, 0x00..=0x01 | 0x08 | 0x21 | 0x50..=0x5f) => (0x154, 8),
        (0x19, 0x40..=0x4f) => (0x300, 8),
        (0x19, 0x60..=0x7f) => (0x308, 8),
        // Zen 5. Granite Ridge (Ryzen 9000, incl. the 9800X3D) is model 0x44.
        (0x1a, 0x00..=0x2f) => (0x1F0, 16),
        (0x1a, 0x40..=0x4f) => (0x308, 8),
        _ => return None,
    };

    // Zen 1 / Zen+ reported Tctl with a built-in offset above Tdie.
    let tctl_offset_deci_c =
        if brand.contains("1600X") || brand.contains("1700X") || brand.contains("1800X") {
            -200
        } else if brand.contains("Threadripper 19") || brand.contains("Threadripper 29") {
            -270
        } else if brand.contains("2700X") {
            -100
        } else {
            0
        };

    Some(CpuLayout {
        ccd_offset,
        ccd_count,
        tctl_offset_deci_c,
        brand,
    })
}

impl CpuLayout {
    /// Package temperature (Tctl, corrected to Tdie where a correction
    /// applies), in tenths of a degree Celsius.
    pub fn read_package(&self, drv: &Driver) -> Option<i16> {
        let regval = drv.read_smn(ZEN_REPORTED_TEMP_CTRL_BASE)?;

        // Guard against an all-ones read, which is what config space returns
        // when the access silently failed.
        if regval == u32::MAX {
            return None;
        }

        // raw units are 0.125 C; work in millidegrees then round to tenths.
        let mut milli = ((regval >> ZEN_CUR_TEMP_SHIFT) * 125) as i32;
        if (regval & ZEN_CUR_TEMP_RANGE_SEL_MASK) != 0
            || (regval & ZEN_CUR_TEMP_TJ_SEL_MASK) == ZEN_CUR_TEMP_TJ_SEL_MASK
        {
            milli -= 49_000;
        }

        let deci = round_milli_to_deci(milli) + self.tctl_offset_deci_c;
        plausible(deci)
    }

    /// Per-CCD (Tdie) temperatures. Slots that report invalid are skipped, so
    /// a single-CCD part like the 9800X3D yields exactly one entry.
    pub fn read_ccds(&self, drv: &Driver, out: &mut Vec<(u32, i16)>) {
        out.clear();
        for i in 0..self.ccd_count {
            let addr = ZEN_REPORTED_TEMP_CTRL_BASE + self.ccd_offset + i * 4;
            let Some(regval) = drv.read_smn(addr) else {
                continue;
            };
            if regval == u32::MAX || (regval & ZEN_CCD_TEMP_VALID) == 0 {
                continue;
            }
            let milli = ((regval & ZEN_CCD_TEMP_MASK) * 125) as i32 - 49_000;
            if let Some(deci) = plausible(round_milli_to_deci(milli)) {
                out.push((i, deci));
            }
        }
    }
}

fn round_milli_to_deci(milli: i32) -> i16 {
    // Round half away from zero so 26.75 C reads 26.8, not 26.7.
    let r = if milli >= 0 {
        (milli + 50) / 100
    } else {
        (milli - 50) / 100
    };
    r as i16
}

/// Reject readings outside the range the silicon can actually report, so a
/// bad access shows as "no reading" rather than a wild number on the graph.
fn plausible(deci_c: i16) -> Option<i16> {
    (-490..=2069).contains(&deci_c).then_some(deci_c)
}

/// (family, model, brand string) from CPUID, or None if the CPU is not AMD.
fn cpuid_identity() -> Option<(u32, u32, String)> {
    use std::arch::x86_64::__cpuid;

    // Leaf 0: vendor string in EBX, EDX, ECX -> "AuthenticAMD".
    let vendor = __cpuid(0);
    let mut v = [0u8; 12];
    v[0..4].copy_from_slice(&vendor.ebx.to_le_bytes());
    v[4..8].copy_from_slice(&vendor.edx.to_le_bytes());
    v[8..12].copy_from_slice(&vendor.ecx.to_le_bytes());
    if &v != b"AuthenticAMD" {
        return None;
    }

    // Leaf 1 EAX: base family 11:8, extended family 27:20,
    //             base model 7:4,  extended model 19:16.
    let one = __cpuid(1);
    let eax = one.eax;
    let base_family = (eax >> 8) & 0xF;
    let base_model = (eax >> 4) & 0xF;
    let family = if base_family == 0xF {
        base_family + ((eax >> 20) & 0xFF)
    } else {
        base_family
    };
    let model = if base_family == 0xF || base_family == 0x6 {
        (((eax >> 16) & 0xF) << 4) | base_model
    } else {
        base_model
    };

    Some((family, model, brand_string()))
}

/// Extended leaves 0x80000002..4 hold the 48-byte marketing name.
fn brand_string() -> String {
    use std::arch::x86_64::__cpuid;
    let mut bytes = [0u8; 48];
    for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004]
        .iter()
        .enumerate()
    {
        let r = __cpuid(*leaf);
        let base = i * 16;
        bytes[base..base + 4].copy_from_slice(&r.eax.to_le_bytes());
        bytes[base + 4..base + 8].copy_from_slice(&r.ebx.to_le_bytes());
        bytes[base + 8..base + 12].copy_from_slice(&r.ecx.to_le_bytes());
        bytes[base + 12..base + 16].copy_from_slice(&r.edx.to_le_bytes());
    }
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// "AMD Ryzen 7 9800X3D 8-Core Processor" -> "Ryzen 7 9800X3D"
pub fn short_brand(brand: &str) -> String {
    let s = brand.trim_start_matches("AMD ");
    match s.find(" 8-Core").or_else(|| s.find("-Core")) {
        Some(_) => s
            .split_whitespace()
            .take_while(|w| !w.ends_with("-Core") && *w != "Processor")
            .collect::<Vec<_>>()
            .join(" "),
        None => s.to_string(),
    }
}
