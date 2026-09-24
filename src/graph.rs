//! The history chart.
//!
//! Drawn with plain GDI at 2x into an offscreen bitmap and then halftone-
//! shrunk onto the target. GDI has no line antialiasing of its own, and
//! supersampling costs a fraction of a millisecond at this size -- far
//! cheaper than initialising Direct2D or GDI+ for one small chart, and it
//! only ever runs while the popup is actually visible.

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, FillRect, LineTo, MoveToEx, SelectObject, SetBkMode,
    SetStretchBltMode, SetTextColor, StretchBlt, ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_NORMAL,
    HALFTONE, HDC, HGDIOBJ, OUT_DEFAULT_PRECIS, PS_SOLID, SRCCOPY, TRANSPARENT, VARIABLE_PITCH,
};

use crate::history::{History, NO_VALUE};
use crate::theme;

/// Supersampling factor. 2 is the sweet spot: visibly smoother diagonals,
/// 4x the fill cost of a chart that is only a few hundred pixels wide.
const SS: i32 = 2;

/// Width in pixels reserved on the right for the axis labels.
const AXIS_W: i32 = 34;

pub struct Series<'a> {
    pub history: &'a History,
    pub color: u32,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Round a deci-Celsius span out to tidy gridline values.
fn nice_bounds(lo: i16, hi: i16) -> (i32, i32) {
    let (mut lo, mut hi) = (lo as i32, hi as i32);
    // Always show at least a 10 C window so a flat idle trace does not turn
    // into a noise-amplifying full-scale zigzag.
    if hi - lo < 100 {
        let mid = (lo + hi) / 2;
        lo = mid - 50;
        hi = mid + 50;
    }
    let pad = ((hi - lo) / 8).max(20);
    lo -= pad;
    hi += pad;
    // Snap to whole 5 C steps.
    lo = (lo.div_euclid(50)) * 50;
    hi = (hi.div_euclid(50) + 1) * 50;
    (lo, hi)
}

pub fn draw(
    dest: HDC,
    area: &RECT,
    series: &[Series],
    window: usize,
    fahrenheit: bool,
    times: &History<u64>,
) {
    let (first, last) = times.time_bounds(window).unwrap_or((0, 0));
    let seconds_span = last.saturating_sub(first) / 1000;
    let w = area.right - area.left;
    let h = area.bottom - area.top;
    if w <= 0 || h <= 0 {
        return;
    }

    unsafe {
        let mem = CreateCompatibleDC(dest);
        if mem.is_null() {
            return;
        }
        let bmp = CreateCompatibleBitmap(dest, w * SS, h * SS);
        if bmp.is_null() {
            DeleteDC(mem);
            return;
        }
        let old_bmp = SelectObject(mem, bmp as HGDIOBJ);

        let full = RECT {
            left: 0,
            top: 0,
            right: w * SS,
            bottom: h * SS,
        };
        let bg = CreateSolidBrush(theme::BG_PANEL);
        FillRect(mem, &full, bg);
        DeleteObject(bg as HGDIOBJ);

        // Plot area, leaving room for the value axis on the right.
        let plot = RECT {
            left: 2 * SS,
            top: 4 * SS,
            right: (w - AXIS_W) * SS,
            bottom: (h - 12) * SS,
        };

        // ---- vertical range across every visible series -------------------
        let mut lo = i16::MAX;
        let mut hi = i16::MIN;
        let mut any = false;
        for s in series {
            if let Some((a, b)) = s.history.range_recent(window) {
                any = true;
                lo = lo.min(a);
                hi = hi.max(b);
            }
        }

        if !any {
            SelectObject(mem, old_bmp);
            DeleteObject(bmp as HGDIOBJ);
            DeleteDC(mem);
            return;
        }

        let (y_lo, y_hi) = nice_bounds(lo, hi);
        let span = (y_hi - y_lo).max(1);
        let plot_h = plot.bottom - plot.top;
        let plot_w = plot.right - plot.left;

        let y_of = |deci: i32| -> i32 { plot.bottom - ((deci - y_lo) * plot_h) / span };

        // ---- gridlines and axis labels -------------------------------------
        let font = CreateFontW(
            -9 * SS,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            ANTIALIASED_QUALITY as u32,
            (VARIABLE_PITCH | FF_DONTCARE) as u32,
            wide("Segoe UI").as_ptr(),
        );
        let old_font = SelectObject(mem, font as HGDIOBJ);
        SetBkMode(mem, TRANSPARENT as i32);

        let grid_pen = CreatePen(PS_SOLID, SS, theme::GRID);
        let old_pen = SelectObject(mem, grid_pen as HGDIOBJ);

        // Four gridlines is enough structure to read values off without the
        // chart turning into graph paper.
        for k in 0..=4 {
            let v = y_lo + span * k / 4;
            let y = y_of(v);
            MoveToEx(mem, plot.left, y, std::ptr::null_mut());
            LineTo(mem, plot.right, y);

            SetTextColor(mem, theme::TEXT_DIM);
            let label = format_temp(v as i16, fahrenheit, false);
            let wl = wide(&label);
            let mut lr = RECT {
                left: plot.right + 4 * SS,
                top: y - 7 * SS,
                right: w * SS,
                bottom: y + 7 * SS,
            };
            DrawTextW(
                mem,
                wl.as_ptr(),
                (wl.len() - 1) as i32,
                &mut lr,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );
        }
        SelectObject(mem, old_pen);
        DeleteObject(grid_pen as HGDIOBJ);

        // ---- the traces ----------------------------------------------------
        for s in series {
            let pen = CreatePen(PS_SOLID, 3, s.color);
            let old = SelectObject(mem, pen as HGDIOBJ);

            let mut pen_down = false;
            for (v, time) in s.history.iter_recent(window).zip(times.iter_recent(window)) {
                if v == NO_VALUE {
                    // Break the line so a dropped sample reads as a gap
                    // rather than a straight interpolation across it.
                    pen_down = false;
                    continue;
                }
                let x = plot.left + time_x(time, first, last, plot_w);
                let y = y_of(v as i32).clamp(plot.top, plot.bottom);
                if pen_down {
                    LineTo(mem, x, y);
                } else {
                    MoveToEx(mem, x, y, std::ptr::null_mut());
                    pen_down = true;
                }
            }

            SelectObject(mem, old);
            DeleteObject(pen as HGDIOBJ);
        }

        // ---- time axis caption --------------------------------------------
        SetTextColor(mem, theme::TEXT_DIM);
        let cap = if seconds_span < 90 {
            format!("last {seconds_span} s")
        } else if seconds_span < 7200 {
            format!("last {} min", seconds_span / 60)
        } else {
            format!("last {} h", seconds_span / 3600)
        };
        let wc = wide(&cap);
        let mut cr = RECT {
            left: plot.left,
            top: plot.bottom + SS,
            right: plot.right,
            bottom: h * SS,
        };
        DrawTextW(
            mem,
            wc.as_ptr(),
            (wc.len() - 1) as i32,
            &mut cr,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );

        let wn = wide("now");
        let mut nr = cr;
        DrawTextW(
            mem,
            wn.as_ptr(),
            (wn.len() - 1) as i32,
            &mut nr,
            DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
        );

        SelectObject(mem, old_font);
        DeleteObject(font as HGDIOBJ);

        // ---- resolve down onto the target ----------------------------------
        SetStretchBltMode(dest, HALFTONE);
        StretchBlt(
            dest,
            area.left,
            area.top,
            w,
            h,
            mem,
            0,
            0,
            w * SS,
            h * SS,
            SRCCOPY,
        );

        SelectObject(mem, old_bmp);
        DeleteObject(bmp as HGDIOBJ);
        DeleteDC(mem);
    }
}

/// Milliseconds determine spacing, including timer delays and rate changes.
fn time_x(time: u64, first: u64, last: u64, width: i32) -> i32 {
    if last <= first {
        return width;
    }
    ((time.saturating_sub(first).min(last - first) * width as u64) / (last - first)) as i32
}

fn round_div(value: i32, divisor: i32) -> i32 {
    (value + value.signum() * (divisor / 2)) / divisor
}

fn display_value(deci_c: i16, fahrenheit: bool, decimals: bool) -> i32 {
    // Round only once, at the displayed precision. Rounding tenths of F
    // before rounding whole degrees would turn 47.48 F into 48 F.
    if fahrenheit {
        round_div(i32::from(deci_c) * 9 + 1600, if decimals { 5 } else { 50 })
    } else {
        round_div(i32::from(deci_c), if decimals { 1 } else { 10 })
    }
}

/// Format a deci-Celsius reading for display.
pub fn format_temp(deci_c: i16, fahrenheit: bool, decimals: bool) -> String {
    if deci_c == NO_VALUE {
        return "--".to_string();
    }
    let value = display_value(deci_c, fahrenheit, decimals);
    if decimals {
        let sign = if value < 0 { "-" } else { "" };
        format!("{sign}{}.{}\u{00B0}", value.abs() / 10, value.abs() % 10)
    } else {
        format!("{value}\u{00B0}")
    }
}

/// Unsuffixed integer form, for the tray icon where every pixel counts.
pub fn format_tray(deci_c: i16, fahrenheit: bool) -> String {
    if deci_c == NO_VALUE {
        return "--".to_string();
    }
    display_value(deci_c, fahrenheit, false).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_spaces_mixed_cadences_by_elapsed_time() {
        assert_eq!(time_x(30_000, 0, 32_000, 320), 300);
        assert_eq!(time_x(31_000, 0, 32_000, 320), 310);
        assert_eq!(time_x(32_000, 0, 32_000, 320), 320);
        assert_eq!(time_x(5, 5, 5, 320), 320);
        assert_eq!(time_x(3_600_000, 0, 3_601_000, 320), 319);
    }

    #[test]
    fn negative_temperatures_keep_their_sign_and_round_correctly() {
        assert_eq!(format_temp(-4, false, true), "-0.4°");
        assert_eq!(format_temp(-55, false, false), "-6°");
        assert_eq!(format_tray(-55, false), "-6");
        assert_eq!(format_temp(-400, true, true), "-40.0°");
    }

    #[test]
    fn fahrenheit_rounds_at_the_requested_precision() {
        assert_eq!(format_temp(1, true, true), "32.2°");
        assert_eq!(format_temp(86, true, true), "47.5°");
        assert_eq!(format_tray(86, true), "47");
        assert_eq!(format_temp(1000, true, true), "212.0°");
        assert_eq!(format_temp(NO_VALUE, true, true), "--");
    }
}
