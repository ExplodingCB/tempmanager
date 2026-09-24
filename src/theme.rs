//! Colours and metrics. Kept in one place so the popup and the tray icon
//! never drift apart.

/// GDI wants 0x00BBGGRR, which is the reverse of how colours are usually
/// written, so build them from RGB components instead of hand-swapping.
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

pub const BG: u32 = rgb(0x1C, 0x1C, 0x1E);
pub const BG_PANEL: u32 = rgb(0x26, 0x26, 0x29);
pub const BORDER: u32 = rgb(0x3A, 0x3A, 0x3E);
pub const GRID: u32 = rgb(0x32, 0x32, 0x36);
pub const TEXT: u32 = rgb(0xEC, 0xEC, 0xEE);
pub const TEXT_DIM: u32 = rgb(0x8A, 0x8A, 0x92);

pub const OK: u32 = rgb(0x5A, 0xD1, 0x8B);
pub const WARN: u32 = rgb(0xE8, 0xB3, 0x39);
pub const HOT: u32 = rgb(0xE8, 0x5D, 0x55);

/// Per-channel series colours, cycled by channel index.
pub const SERIES: [u32; 6] = [
    rgb(0x6E, 0xA8, 0xFE), // blue
    rgb(0x5A, 0xD1, 0x8B), // green
    rgb(0xE8, 0xB3, 0x39), // amber
    rgb(0xC4, 0x8B, 0xF0), // violet
    rgb(0x5A, 0xD4, 0xD1), // teal
    rgb(0xF0, 0x8B, 0xB4), // pink
];

pub fn series_color(i: usize) -> u32 {
    SERIES[i % SERIES.len()]
}

/// Colour for a reading given its channel's thresholds.
pub fn status_color(deci_c: i16, warn: i16, hot: i16) -> u32 {
    if deci_c >= hot {
        HOT
    } else if deci_c >= warn {
        WARN
    } else {
        OK
    }
}
