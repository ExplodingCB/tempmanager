//! The tray icon itself: a temperature rendered straight into a DIB and
//! handed to the shell as an HICON.
//!
//! The icon is rebuilt only when the displayed number or its colour actually
//! changes, so a machine sitting at a steady temperature does no GDI work at
//! all between samples.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
    DrawTextW, GdiFlush, GetDC, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT, DT_CENTER, DT_NOCLIP, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_BOLD, HBITMAP, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT, VARIABLE_PITCH,
};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON, SM_CYSMICON,
};

use crate::theme;

/// Message the shell posts back to us for tray interactions.
pub const WM_TRAY: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1;

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    /// Last rendered (text, colour), to skip redundant icon rebuilds.
    last: Option<(String, u32)>,
    tip: [u16; 128],
    size: i32,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        // Use the shell's small-icon metric so we render at native resolution
        // on high-DPI displays instead of letting the shell upscale a 16px icon.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }
            .max(unsafe { GetSystemMetrics(SM_CYSMICON) })
            .max(16);

        let mut tray = Self {
            hwnd,
            icon: std::ptr::null_mut(),
            last: None,
            tip: [0; 128],
            size,
        };
        tray.icon = render_icon("--", theme::TEXT_DIM, size);

        let mut data = tray.base_data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = tray.icon;
        copy_tip(&mut data.szTip, "tempmanager - starting");
        tray.tip = data.szTip;
        unsafe { Shell_NotifyIconW(NIM_ADD, &data) };

        // Version 4 gives us proper per-message cursor coordinates in lParam.
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };

        tray
    }

    fn base_data(&self) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = 1;
        data
    }

    /// Update the icon and tooltip. `text` is what gets painted (e.g. "62"),
    /// `tip` is the multi-line hover text.
    pub fn update(&mut self, text: &str, color: u32, tip: &str) {
        let changed = self
            .last
            .as_ref()
            .map(|(t, c)| t != text || *c != color)
            .unwrap_or(true);

        let mut data = self.base_data();
        data.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy_tip(&mut data.szTip, tip);
        if !changed && data.szTip == self.tip {
            return;
        }

        if changed {
            let icon = render_icon(text, color, self.size);
            if !icon.is_null() {
                let old = self.icon;
                self.icon = icon;
                data.uFlags |= NIF_ICON;
                data.hIcon = icon;
                let updated = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0;
                if !old.is_null() {
                    unsafe { DestroyIcon(old) };
                }
                if updated {
                    self.last = Some((text.to_string(), color));
                    self.tip = data.szTip;
                }
                return;
            }
        }

        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0 {
            self.tip = data.szTip;
        }
    }

    /// Re-add the icon after an Explorer restart.
    pub fn readd(&mut self) {
        let mut data = self.base_data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = self.icon;
        data.szTip = self.tip;
        unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let data = self.base_data();
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &data);
            if !self.icon.is_null() {
                DestroyIcon(self.icon);
            }
        }
    }
}

fn copy_tip(dst: &mut [u16; 128], s: &str) {
    dst.fill(0);
    for (slot, unit) in dst[..127].iter_mut().zip(s.encode_utf16()) {
        *slot = unit;
    }
    // Never leave a truncated high surrogate at the end of the tooltip.
    if (0xD800..=0xDBFF).contains(&dst[126]) {
        dst[126] = 0;
    }
}

/// Paint `text` into a 32-bit ARGB DIB and wrap it as an icon.
///
/// GDI text drawing does not write an alpha channel, so we draw white glyphs
/// on black and then reinterpret luminance as coverage. That gives properly
/// antialiased edges against whatever taskbar colour the user has, which is
/// the thing that makes a text tray icon look native rather than pasted on.
fn render_icon(text: &str, color: u32, size: i32) -> HICON {
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let dc = CreateCompatibleDC(screen);
        ReleaseDC(std::ptr::null_mut(), screen);
        if dc.is_null() {
            return std::ptr::null_mut();
        }

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size, // negative: top-down, so row 0 is the top
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };

        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if dib.is_null() || bits.is_null() {
            if !dib.is_null() {
                DeleteObject(dib as HGDIOBJ);
            }
            DeleteDC(dc);
            return std::ptr::null_mut();
        }

        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        pixels.fill(0x0000_0000);

        let old_bmp = SelectObject(dc, dib as HGDIOBJ);

        // Pick the largest point size whose rendered extent still fits. Two
        // digits get most of the icon; three digits shrink to stay legible.
        let wtext = wide(text);
        let mut font_px = size;
        let mut chosen = std::ptr::null_mut();
        loop {
            let f = CreateFontW(
                -font_px,
                0,
                0,
                0,
                FW_BOLD as i32,
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
            if f.is_null() {
                break;
            }
            let old = SelectObject(dc, f as HGDIOBJ);
            let mut r = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            DrawTextW(
                dc,
                wtext.as_ptr(),
                (wtext.len() - 1) as i32,
                &mut r,
                DT_CALCRECT | DT_SINGLELINE | DT_NOCLIP,
            );
            SelectObject(dc, old);

            if (r.right - r.left) <= size && (r.bottom - r.top) <= size {
                chosen = f;
                break;
            }
            DeleteObject(f as HGDIOBJ);
            font_px -= 1;
            if font_px < 6 {
                break;
            }
        }

        if !chosen.is_null() {
            let old_font = SelectObject(dc, chosen as HGDIOBJ);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, 0x00FF_FFFF); // white; becomes the alpha channel
            let mut r = RECT {
                left: 0,
                top: 0,
                right: size,
                bottom: size,
            };
            DrawTextW(
                dc,
                wtext.as_ptr(),
                (wtext.len() - 1) as i32,
                &mut r,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP,
            );
            SelectObject(dc, old_font);
            DeleteObject(chosen as HGDIOBJ);
        }

        // Complete batched GDI writes before reading the DIB's memory.
        GdiFlush();
        // Luminance -> alpha, then premultiply the requested colour.
        let (cr, cg, cb) = (
            (color & 0xFF),
            ((color >> 8) & 0xFF),
            ((color >> 16) & 0xFF),
        );
        for px in pixels.iter_mut() {
            let v = *px;
            let a = ((v & 0xFF).max((v >> 8) & 0xFF)).max((v >> 16) & 0xFF);
            if a == 0 {
                *px = 0;
            } else {
                // Icons are composited premultiplied.
                let r = cr * a / 255;
                let g = cg * a / 255;
                let b = cb * a / 255;
                *px = (a << 24) | (r << 16) | (g << 8) | b;
            }
        }

        SelectObject(dc, old_bmp);

        // A 32bpp colour bitmap carries its own alpha, so the mask is unused;
        // it still has to exist and be the right size.
        let mask: HBITMAP = CreateBitmap(size, size, 1, 1, std::ptr::null());

        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: dib,
        };
        let icon = CreateIconIndirect(&icon_info);

        DeleteObject(mask as HGDIOBJ);
        DeleteObject(dib as HGDIOBJ);
        DeleteDC(dc);
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_terminates_and_clears_old_content_without_splitting_unicode() {
        let mut tip = [0; 128];
        copy_tip(&mut tip, &format!("{}🌡", "a".repeat(126)));
        assert_eq!(tip[126], 0);
        assert_eq!(tip[127], 0);
        copy_tip(&mut tip, "GPU: 42°");
        assert!(tip[8..].iter().all(|v| *v == 0));
    }
}
