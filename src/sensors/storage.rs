//! NVMe / SATA drive temperatures.
//!
//! `IOCTL_STORAGE_QUERY_PROPERTY` with `StorageDeviceTemperatureProperty` is
//! serviced by the Windows storage stack, so this needs no kernel driver and
//! no elevation -- the handle is opened with zero desired access, which is
//! enough for a property query.
//!
//! The Windows SDK types supply the exact descriptor offsets and entry sizes;
//! response bytes are decoded without creating potentially unaligned references.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{
    STORAGE_TEMPERATURE_DATA_DESCRIPTOR, STORAGE_TEMPERATURE_INFO,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

// CTL_CODE(IOCTL_STORAGE_BASE 0x2D, 0x0500, METHOD_BUFFERED, FILE_ANY_ACCESS)
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;

const STORAGE_DEVICE_PROPERTY: u32 = 0;
const STORAGE_DEVICE_TEMPERATURE_PROPERTY: u32 = 52;
const PROPERTY_STANDARD_QUERY: u32 = 0;

/// Highest \\.\PhysicalDriveN index we bother probing at startup.
const MAX_DRIVES: u32 = 16;

#[repr(C)]
struct StoragePropertyQuery {
    property_id: u32,
    query_type: u32,
    additional_parameters: [u8; 4],
}

pub struct Drive {
    /// Allocate/encode the device path once, while keeping handles short-lived.
    path: Vec<u16>,
    pub label: String,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_drive(path: &[u16]) -> Option<HANDLE> {
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            0, // property queries need no read/write access, so no elevation
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(h)
}

fn query(h: HANDLE, property_id: u32, out: &mut [u8]) -> Option<u32> {
    let q = StoragePropertyQuery {
        property_id,
        query_type: PROPERTY_STANDARD_QUERY,
        additional_parameters: [0; 4],
    };
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &q as *const _ as *const c_void,
            std::mem::size_of::<StoragePropertyQuery>() as u32,
            out.as_mut_ptr() as *mut c_void,
            out.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(returned)
}

/// Pull the product ID out of a STORAGE_DEVICE_DESCRIPTOR blob.
fn product_name(buf: &[u8], returned: u32) -> Option<String> {
    let buf = buf.get(..returned as usize)?;
    if returned < 36 {
        return None;
    }
    let product_id_offset = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]) as usize;
    if product_id_offset == 0 || product_id_offset >= returned as usize {
        return None;
    }
    let tail = &buf[product_id_offset..returned as usize];
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    let name = String::from_utf8_lossy(&tail[..end]).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Probe every physical drive once at startup and keep the ones that actually
/// report a temperature, so the sample path never touches a dead handle.
pub fn detect() -> Vec<Drive> {
    let mut drives = Vec::new();
    for index in 0..MAX_DRIVES {
        let path = wide(&format!("\\\\.\\PhysicalDrive{index}"));
        let Some(h) = open_drive(&path) else { continue };

        let mut temp_buf = [0u8; 512];
        let supports_temp = query(h, STORAGE_DEVICE_TEMPERATURE_PROPERTY, &mut temp_buf)
            .map(|n| parse_temperature(&temp_buf, n).is_some())
            .unwrap_or(false);

        let label = if supports_temp {
            let mut desc_buf = [0u8; 1024];
            query(h, STORAGE_DEVICE_PROPERTY, &mut desc_buf)
                .and_then(|n| product_name(&desc_buf, n))
                .unwrap_or_else(|| format!("Drive {index}"))
        } else {
            String::new()
        };

        unsafe { CloseHandle(h) };

        if supports_temp {
            drives.push(Drive {
                path,
                label: shorten(&label),
            });
        }
    }
    drives
}

fn parse_temperature(buf: &[u8], returned: u32) -> Option<i16> {
    let buf = buf.get(..returned as usize)?;
    let header = std::mem::offset_of!(STORAGE_TEMPERATURE_DATA_DESCRIPTOR, TemperatureInfo);
    let entry = std::mem::size_of::<STORAGE_TEMPERATURE_INFO>();
    if buf.len() < header {
        return None;
    }
    let version = u32::from_le_bytes(buf[0..4].try_into().ok()?) as usize;
    let size = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
    let count = u16::from_le_bytes(buf[12..14].try_into().ok()?) as usize;
    if version < header + entry || size > buf.len() || size < header + count * entry || count == 0 {
        return None;
    }
    // Select the composite/primary sensor by its ID, not its position.
    // The entry is 16 bytes, including the reserved fields omitted in the
    // old hand-written struct. Windows temperatures are signed Celsius.
    for info in buf[header..header + count * entry].chunks_exact(entry) {
        let index = u16::from_le_bytes([info[0], info[1]]);
        if index == 0 {
            let c = i16::from_le_bytes([info[2], info[3]]);
            // Checked conversion also rejects the unavailable value 0x8000.
            return c.checked_mul(10);
        }
    }
    None
}

impl Drive {
    /// Current temperature in tenths of a degree Celsius.
    ///
    /// The handle is opened per read rather than held open for the process
    /// lifetime, so the app never keeps a reference that would block a drive
    /// from sleeping or being safely removed.
    pub fn read(&self) -> Option<i16> {
        let h = open_drive(&self.path)?;
        let mut buf = [0u8; 512];
        let result = query(h, STORAGE_DEVICE_TEMPERATURE_PROPERTY, &mut buf)
            .and_then(|n| parse_temperature(&buf, n));
        unsafe { CloseHandle(h) };
        result
    }
}

/// Trim vendor noise so labels fit the popup's column.
fn shorten(name: &str) -> String {
    let s = name.trim();
    if s.chars().count() <= 22 {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(19).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(entries: &[(u16, i16)]) -> Vec<u8> {
        // Independent wire fixture from winioctl.h: 24-byte header + 16 per sensor.
        let mut b = vec![0; 24 + 16 * entries.len()];
        let size = b.len() as u32;
        b[0..4].copy_from_slice(&40u32.to_le_bytes());
        b[4..8].copy_from_slice(&size.to_le_bytes());
        b[12..14].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        for (i, (id, temp)) in entries.iter().enumerate() {
            b[24 + i * 16..26 + i * 16].copy_from_slice(&id.to_le_bytes());
            b[26 + i * 16..28 + i * 16].copy_from_slice(&temp.to_le_bytes());
        }
        b
    }

    #[test]
    fn reads_composite_by_id_with_correct_stride_and_unaligned_input() {
        let b = descriptor(&[(1, 78), (0, 42)]);
        let mut unaligned = vec![0];
        unaligned.extend_from_slice(&b);
        assert_eq!(
            parse_temperature(&unaligned[1..], b.len() as u32),
            Some(420)
        );
    }

    #[test]
    fn rejects_truncated_and_inconsistent_driver_responses() {
        let b = descriptor(&[(0, 42)]);
        for n in 0..b.len() {
            assert_eq!(parse_temperature(&b, n as u32), None);
        }
        assert_eq!(parse_temperature(&b, 1000), None);
        let mut bad = b.clone();
        bad[12..14].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(parse_temperature(&bad, bad.len() as u32), None);
        assert_eq!(product_name(&[0; 36], 1000), None);
    }

    #[test]
    fn handles_zero_negative_and_unavailable_temperatures() {
        for (temp, expected) in [
            (0, Some(0)),
            (-5, Some(-50)),
            (i16::MIN, None),
            (i16::MAX, None),
        ] {
            let b = descriptor(&[(0, temp)]);
            assert_eq!(parse_temperature(&b, b.len() as u32), expected);
        }
    }

    #[test]
    fn label_truncation_preserves_unicode() {
        assert_eq!(shorten(&"界".repeat(25)), format!("{}...", "界".repeat(19)));
    }
}
