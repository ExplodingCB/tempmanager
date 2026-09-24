//! The flyout: current readings, the history chart, and the interval slider.
//!
//! It is a single borderless top-level window that is created once and then
//! shown and hidden. Recreating it per click would mean re-registering the
//! class and re-measuring DPI every time, for no benefit.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, GdiFlush,
    InvalidateRect, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_BOLD, FW_NORMAL, FW_SEMIBOLD, HDC, HGDIOBJ, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    PS_SOLID, SRCCOPY, TRANSPARENT, VARIABLE_PITCH,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, LoadCursorW,
    RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SystemParametersInfoW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW,
    SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WA_INACTIVE,
    WM_ACTIVATE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
    WM_PRINTCLIENT, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::app::{App, STEPS};
use crate::graph::{self, Series};
use crate::history::NO_VALUE;
use crate::theme;

pub const CLASS_NAME: &str = "tempmanagerPopup";

// Layout at 96 DPI; every value is scaled by the window's actual DPI.
const PAD: i32 = 14;
const WIDTH: i32 = 344;
const ROW_H: i32 = 25;
const GRAPH_H: i32 = 128;
const SLIDER_H: i32 = 38;
const HEADER_H: i32 = 22;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct Metrics {
    scale: f32,
}

impl Metrics {
    fn for_window(hwnd: HWND) -> Self {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        Self {
            scale: if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 },
        }
    }
    fn s(&self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }
}

pub fn window_size(app: &App, hwnd: HWND) -> (i32, i32) {
    let m = Metrics::for_window(hwnd);
    let rows = app.sensors.channels.len().max(1) as i32;
    let h = PAD + HEADER_H + rows * ROW_H + 10 + GRAPH_H + 4 + SLIDER_H + PAD;
    (m.s(WIDTH), m.s(h))
}

pub fn register(hinstance: windows_sys::Win32::Foundation::HMODULE) {
    let class = wide(CLASS_NAME);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(popup_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: std::ptr::null_mut(),
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    unsafe { RegisterClassExW(&wc) };
}

pub fn create(
    hinstance: windows_sys::Win32::Foundation::HMODULE,
    owner: HWND,
    app: *mut App,
) -> HWND {
    let class = wide(CLASS_NAME);
    let title = wide("tempmanager");
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            // Owned by the tray window: keeps the flyout above its owner and
            // gives it the activation behaviour a popup is expected to have.
            owner,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return hwnd;
    }
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);

        // Match the shell's own flyouts rather than looking like a 2005 dialog.
        let pref = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &pref as *const _ as *const c_void,
            4,
        );
        let border: COLORREF = theme::BORDER;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            &border as *const _ as *const c_void,
            4,
        );
    }
    hwnd
}

/// Show the flyout near the cursor, kept inside the work area.
pub fn show_at_cursor(hwnd: HWND, app: &App, cursor: POINT) {
    // Park the window on the cursor's monitor *before* measuring. Layout is
    // DPI-scaled and GetDpiForWindow reports the monitor the window currently
    // sits on, so sizing it while it is still at the origin would use the
    // primary monitor's scaling -- wrong whenever the tray is on a second
    // display with different scaling.
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            cursor.x,
            cursor.y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }

    let (w, h) = window_size(app, hwnd);

    let mut work = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as *mut c_void, 0) };

    let margin = 8;
    let mut x = cursor.x - w / 2;
    let mut y = cursor.y - h - margin;

    // The taskbar is usually at the bottom, but respect wherever it actually is.
    if y < work.top {
        y = cursor.y + margin;
    }
    x = x.clamp(
        work.left + margin,
        (work.right - w - margin).max(work.left + margin),
    );
    y = y.clamp(
        work.top + margin,
        (work.bottom - h - margin).max(work.top + margin),
    );

    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        ShowWindow(hwnd, SW_SHOWNA);
        // Take focus so the flyout dismisses itself on the next outside click.
        SetForegroundWindow(hwnd);
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
    // Someone is looking at the chart now: sample live.
    crate::set_fast_sampling(true);
}

pub fn hide(hwnd: HWND) {
    unsafe { ShowWindow(hwnd, SW_HIDE) };
    // Back to the configured background cadence.
    crate::set_fast_sampling(false);
}

/// Track rectangle for the interval slider, in client coordinates.
fn slider_track(app: &App, m: &Metrics, client: &RECT) -> RECT {
    let _ = app;
    let bottom = client.bottom - m.s(PAD);
    let cy = bottom - m.s(SLIDER_H) / 2;
    RECT {
        left: client.left + m.s(PAD + 54),
        top: cy - m.s(3),
        right: client.right - m.s(PAD + 46),
        bottom: cy + m.s(3),
    }
}

fn step_from_x(track: &RECT, x: i32) -> usize {
    let w = (track.right - track.left).max(1);
    let t = ((x - track.left).clamp(0, w) as f32) / w as f32;
    ((t * (STEPS.len() - 1) as f32).round() as usize).min(STEPS.len() - 1)
}

fn x_from_step(track: &RECT, idx: usize) -> i32 {
    let w = track.right - track.left;
    track.left + (w * idx as i32) / (STEPS.len() - 1) as i32
}

unsafe extern "system" fn popup_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if app_ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let app = &mut *app_ptr;

    match msg {
        // We repaint every pixel in WM_PAINT; letting the system erase first
        // just causes a visible flash.
        WM_ERASEBKGND => 1,

        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint(hwnd, hdc, app);
            EndPaint(hwnd, &ps);
            0
        }

        // Render into a caller-supplied DC. Without this, anything that
        // captures the window through PrintWindow -- screenshot tools, the
        // Snipping Tool, screen recorders -- gets a black rectangle, because
        // we only ever draw in response to WM_PAINT.
        WM_PRINTCLIENT => {
            paint(hwnd, wparam as HDC, app);
            0
        }

        WM_ACTIVATE => {
            if (wparam & 0xFFFF) as u32 == WA_INACTIVE {
                hide(hwnd);
            }
            0
        }

        WM_LBUTTONDOWN => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let mut client = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
            let m = Metrics::for_window(hwnd);
            let track = slider_track(app, &m, &client);

            // Generous vertical hit band: the visual track is only a few
            // pixels tall, which is far too small to grab reliably.
            if y >= track.top - m.s(12) && y <= track.bottom + m.s(12) {
                app.dragging = true;
                SetCapture(hwnd);
                let idx = step_from_x(&track, x);
                if app.set_step(idx) {
                    crate::request_reschedule(hwnd);
                }
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            0
        }

        WM_MOUSEMOVE => {
            if app.dragging {
                let x = (lparam & 0xFFFF) as i16 as i32;
                let mut client = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
                let m = Metrics::for_window(hwnd);
                let track = slider_track(app, &m, &client);
                let idx = step_from_x(&track, x);
                if app.set_step(idx) {
                    crate::request_reschedule(hwnd);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }

        WM_LBUTTONUP => {
            if app.dragging {
                app.dragging = false;
                ReleaseCapture();
                app.config.save();
            }
            0
        }

        WM_DESTROY => 0,

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Render the flyout to a .bmp without ever showing it. Used by `--shot` to
/// check layout without needing a human to look at the screen.
pub fn render_to_file(
    hinstance: windows_sys::Win32::Foundation::HMODULE,
    app: &mut App,
    path: &str,
) {
    unsafe {
        register(hinstance);
        let hwnd = create(hinstance, std::ptr::null_mut(), app as *mut App);
        if hwnd.is_null() {
            return;
        }
        let (w, h) = window_size(app, hwnd);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            w,
            h,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );

        let mut client = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        let (cw, chh) = (client.right, client.bottom);

        let screen = windows_sys::Win32::Graphics::Gdi::GetDC(std::ptr::null_mut());
        let dc = CreateCompatibleDC(screen);
        windows_sys::Win32::Graphics::Gdi::ReleaseDC(std::ptr::null_mut(), screen);
        if dc.is_null() {
            DestroyWindow(hwnd);
            return;
        }

        let mut info: windows_sys::Win32::Graphics::Gdi::BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER {
            biSize: std::mem::size_of::<windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER>()
                as u32,
            biWidth: cw,
            biHeight: chh, // bottom-up, which is what a .bmp file wants
            biPlanes: 1,
            biBitCount: 32,
            biCompression: windows_sys::Win32::Graphics::Gdi::BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = windows_sys::Win32::Graphics::Gdi::CreateDIBSection(
            dc,
            &info,
            windows_sys::Win32::Graphics::Gdi::DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if dib.is_null() || bits.is_null() {
            if !dib.is_null() {
                DeleteObject(dib as HGDIOBJ);
            }
            DeleteDC(dc);
            DestroyWindow(hwnd);
            return;
        }
        let old = SelectObject(dc, dib as HGDIOBJ);

        paint(hwnd, dc, app);
        GdiFlush();

        let count = (cw * chh) as usize;
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, count);
        for p in px.iter_mut() {
            *p |= 0xFF00_0000; // opaque, so image viewers do not show it blank
        }

        let stride = (cw * 4) as usize;
        let data_len = stride * chh as usize;
        let mut file = Vec::with_capacity(54 + data_len);
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&((54 + data_len) as u32).to_le_bytes());
        file.extend_from_slice(&0u32.to_le_bytes());
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&cw.to_le_bytes());
        file.extend_from_slice(&chh.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&0u32.to_le_bytes());
        file.extend_from_slice(&(data_len as u32).to_le_bytes());
        file.extend_from_slice(&[0u8; 16]);
        file.extend_from_slice(std::slice::from_raw_parts(bits as *const u8, data_len));
        let _ = std::fs::write(path, file);

        SelectObject(dc, old);
        DeleteObject(dib as HGDIOBJ);
        DeleteDC(dc);
        DestroyWindow(hwnd);
    }
}

unsafe fn paint(hwnd: HWND, target: HDC, app: &App) {
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
    let (cw, ch) = (client.right, client.bottom);
    if cw <= 0 || ch <= 0 {
        return;
    }

    let m = Metrics::for_window(hwnd);

    // Double buffer: the chart and the rows would otherwise tear visibly.
    let mem = CreateCompatibleDC(target);
    if mem.is_null() {
        return;
    }
    let bmp = CreateCompatibleBitmap(target, cw, ch);
    if bmp.is_null() {
        DeleteDC(mem);
        return;
    }
    let old_bmp = SelectObject(mem, bmp as HGDIOBJ);

    let bg = CreateSolidBrush(theme::BG);
    FillRect(mem, &client, bg);
    DeleteObject(bg as HGDIOBJ);

    SetBkMode(mem, TRANSPARENT as i32);

    let font_label = make_font(&m, 12, FW_NORMAL as i32);
    let font_value = make_font(&m, 13, FW_SEMIBOLD as i32);
    let font_head = make_font(&m, 12, FW_BOLD as i32);
    let font_small = make_font(&m, 11, FW_NORMAL as i32);

    // ---- header -----------------------------------------------------------
    let mut y = m.s(PAD);
    let old_font = SelectObject(mem, font_head as HGDIOBJ);
    SetTextColor(mem, theme::TEXT);
    draw_text(
        mem,
        "Temperatures",
        &RECT {
            left: m.s(PAD),
            top: y,
            right: cw - m.s(PAD),
            bottom: y + m.s(HEADER_H),
        },
        DT_LEFT,
    );

    // Surface why the CPU is missing instead of silently omitting the row.
    if app.sensors.cpu_status != crate::sensors::driver::DriverStatus::Ready {
        SelectObject(mem, font_small as HGDIOBJ);
        SetTextColor(mem, theme::WARN);
        draw_text(
            mem,
            app.sensors.cpu_status.message(),
            &RECT {
                left: m.s(PAD),
                top: y,
                right: cw - m.s(PAD),
                bottom: y + m.s(HEADER_H),
            },
            DT_RIGHT,
        );
    }
    y += m.s(HEADER_H);

    // ---- one row per channel ----------------------------------------------
    for (i, chan) in app.sensors.channels.iter().enumerate() {
        let v = app.latest.get(i).copied().unwrap_or(NO_VALUE);
        let row = RECT {
            left: m.s(PAD),
            top: y,
            right: cw - m.s(PAD),
            bottom: y + m.s(ROW_H),
        };

        // Series swatch, matching the trace colour in the chart below.
        let dot = app.series_color(i);
        let brush = CreateSolidBrush(dot);
        let pen = CreatePen(PS_SOLID, 1, dot);
        let ob = SelectObject(mem, brush as HGDIOBJ);
        let op = SelectObject(mem, pen as HGDIOBJ);
        let cy = (row.top + row.bottom) / 2;
        let r = m.s(4);
        Ellipse(mem, row.left, cy - r, row.left + 2 * r, cy + r);
        SelectObject(mem, ob);
        SelectObject(mem, op);
        DeleteObject(brush as HGDIOBJ);
        DeleteObject(pen as HGDIOBJ);

        SelectObject(mem, font_label as HGDIOBJ);
        SetTextColor(mem, theme::TEXT);
        let label_rect = RECT {
            left: row.left + m.s(16),
            ..row
        };
        draw_text(mem, &chan.label, &label_rect, DT_LEFT);

        SelectObject(mem, font_value as HGDIOBJ);
        SetTextColor(
            mem,
            if v == NO_VALUE {
                theme::TEXT_DIM
            } else {
                theme::status_color(v, chan.warn, chan.hot)
            },
        );
        draw_text(
            mem,
            &graph::format_temp(v, app.config.fahrenheit, true),
            &row,
            DT_RIGHT,
        );

        y += m.s(ROW_H);
    }

    // ---- chart -------------------------------------------------------------
    y += m.s(10);
    let plot = RECT {
        left: m.s(PAD),
        top: y,
        right: cw - m.s(PAD),
        bottom: y + m.s(GRAPH_H),
    };

    let panel = CreateSolidBrush(theme::BG_PANEL);
    FillRect(mem, &plot, panel);
    DeleteObject(panel as HGDIOBJ);

    let window = app.graph_window();
    let series: Vec<Series> = app
        .histories
        .iter()
        .enumerate()
        .map(|(i, h)| Series {
            history: h,
            color: app.series_color(i),
        })
        .collect();
    graph::draw(
        mem,
        &plot,
        &series,
        window,
        app.config.fahrenheit,
        &app.times,
    );

    y = plot.bottom + m.s(4);

    // ---- interval slider ---------------------------------------------------
    let track = slider_track(app, &m, &client);
    let idx = app.step_index();
    let thumb_x = x_from_step(&track, idx);
    let cy = (track.top + track.bottom) / 2;

    SelectObject(mem, font_small as HGDIOBJ);
    SetTextColor(mem, theme::TEXT_DIM);
    draw_text(
        mem,
        "Every",
        &RECT {
            left: m.s(PAD),
            top: cy - m.s(9),
            right: track.left,
            bottom: cy + m.s(9),
        },
        DT_LEFT,
    );

    // Inactive part of the track.
    let rest = CreateSolidBrush(theme::BORDER);
    FillRect(mem, &track, rest);
    DeleteObject(rest as HGDIOBJ);

    // Filled part up to the thumb.
    let filled = RECT {
        right: thumb_x,
        ..track
    };
    let fill = CreateSolidBrush(theme::SERIES[0]);
    FillRect(mem, &filled, fill);
    DeleteObject(fill as HGDIOBJ);

    let tr = m.s(7);
    let thumb_brush = CreateSolidBrush(theme::TEXT);
    let thumb_pen = CreatePen(PS_SOLID, 1, theme::TEXT);
    let ob = SelectObject(mem, thumb_brush as HGDIOBJ);
    let op = SelectObject(mem, thumb_pen as HGDIOBJ);
    Ellipse(mem, thumb_x - tr, cy - tr, thumb_x + tr, cy + tr);
    SelectObject(mem, ob);
    SelectObject(mem, op);
    DeleteObject(thumb_brush as HGDIOBJ);
    DeleteObject(thumb_pen as HGDIOBJ);

    SetTextColor(mem, theme::TEXT);
    let secs = app.config.interval_s;
    let label = if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{secs}s")
    };
    draw_text(
        mem,
        &label,
        &RECT {
            left: track.right + m.s(8),
            top: cy - m.s(9),
            right: cw - m.s(PAD),
            bottom: cy + m.s(9),
        },
        DT_RIGHT,
    );

    let _ = y;

    BitBlt(target, 0, 0, cw, ch, mem, 0, 0, SRCCOPY);

    // DeleteObject cannot release a font while it is selected into this DC.
    SelectObject(mem, old_font);
    for f in [font_label, font_value, font_head, font_small] {
        DeleteObject(f as HGDIOBJ);
    }
    SelectObject(mem, old_bmp);
    DeleteObject(bmp as HGDIOBJ);
    DeleteDC(mem);

    // Silence unused warnings for helpers kept for layout tweaking.
}

unsafe fn make_font(m: &Metrics, pt: i32, weight: i32) -> windows_sys::Win32::Graphics::Gdi::HFONT {
    CreateFontW(
        -m.s(pt),
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        ANTIALIASED_QUALITY as u32,
        (VARIABLE_PITCH | FF_DONTCARE) as u32,
        wide("Segoe UI").as_ptr(),
    )
}

unsafe fn draw_text(hdc: HDC, text: &str, rect: &RECT, align: u32) {
    let w = wide(text);
    let mut r = *rect;
    DrawTextW(
        hdc,
        w.as_ptr(),
        (w.len() - 1) as i32,
        &mut r,
        align | DT_SINGLELINE | DT_VCENTER,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use crate::sensors::{Channel, Kind, Sensors};

    #[test]
    #[ignore = "creates an offscreen Win32 window; run explicitly on Windows"]
    fn repeated_paint_releases_gdi_objects() {
        use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetGuiResources};

        let mut history = History::new();
        let mut times = History::<u64>::new();
        for i in 0..240 {
            history.push(500 + (i % 100) as i16);
            times.push(i * 1000);
        }
        let mut app = App {
            config: crate::config::Config::default(),
            sensors: Sensors::test_fixture(vec![Channel {
                label: "Test CPU".into(),
                kind: Kind::Cpu,
                warn: 750,
                hot: 900,
            }]),
            histories: vec![history],
            latest: vec![650],
            tray_dirty: false,
            times,
            popup: std::ptr::null_mut(),
            dragging: false,
        };
        unsafe {
            let instance =
                windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
            register(instance);
            let hwnd = create(instance, std::ptr::null_mut(), &mut app);
            assert!(!hwnd.is_null());
            let (w, h) = window_size(&app, hwnd);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                w,
                h,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            let screen = GetDC(std::ptr::null_mut());
            let dc = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            ReleaseDC(std::ptr::null_mut(), screen);
            assert!(!dc.is_null() && !bitmap.is_null());
            let old = SelectObject(dc, bitmap as HGDIOBJ);
            paint(hwnd, dc, &app);
            GdiFlush();
            let before = GetGuiResources(GetCurrentProcess(), 0);
            let start = std::time::Instant::now();
            for _ in 0..200 {
                paint(hwnd, dc, &app);
            }
            GdiFlush();
            let after = GetGuiResources(GetCurrentProcess(), 0);
            let elapsed = start.elapsed();
            SelectObject(dc, old);
            DeleteObject(bitmap as HGDIOBJ);
            DeleteDC(dc);
            DestroyWindow(hwnd);
            eprintln!("200 paints: {elapsed:?}; GDI objects {before} -> {after}");
            assert_eq!(after, before, "GDI objects grew over repeated paints");
        }
    }
}
