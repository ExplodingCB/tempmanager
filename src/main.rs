//! tempmanager -- a tray temperature readout.
//!
//! Design notes on "not noticeable in the background":
//!   * One application thread; Windows and sensor DLLs may create others.
//!     A coalescable timer lets Windows batch background wakeups.
//!   * `GetMessageW` blocks between messages instead of busy-polling.
//!   * Sensor/history buffers and device paths are allocated at startup.
//!     Display formatting and GDI work happen when readings change.
//!   * The chart is only drawn while the flyout is on screen.

#![windows_subsystem = "windows"]

mod app;
mod config;
mod graph;
mod history;
mod popup;
mod sensors;
mod theme;
mod tray;

use std::sync::atomic::{AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetMessageW, GetWindowLongPtrW, IsWindowVisible, KillTimer, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, TrackPopupMenu,
    TranslateMessage, GWLP_USERDATA, HMENU, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG,
    TPM_BOTTOMALIGN, TPM_RIGHTALIGN, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_TIMER, WNDCLASSEXW,
    WS_OVERLAPPED,
};

use app::App;
use config::TraySource;

const TIMER_ID: usize = 1;

// Tray notification events. Under NOTIFYICON_VERSION_4 the shell delivers the
// raw button messages (WM_LBUTTONDOWN/UP, WM_RBUTTONDOWN/UP) *as well as* the
// NIN_* notifications, so a single left click arrives as three separate events.
// Acting on both NIN_SELECT and WM_LBUTTONUP toggles the flyout twice and it
// just blinks. Handle only the NIN_* notifications.
const NIN_SELECT: u32 = 0x0400;
const NIN_KEYSELECT: u32 = 0x0401;

const ID_HOTTEST: usize = 101;
const ID_CPU: usize = 102;
const ID_GPU: usize = 103;
const ID_FAHRENHEIT: usize = 104;
const ID_AUTOSTART: usize = 105;
const ID_EXIT: usize = 106;
const ID_ELEVATE: usize = 107;

/// The hidden owner window, so the popup can ask for the sample timer to be
/// rescheduled after the interval slider moves.
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
/// Registered "TaskbarCreated" message, for re-adding the icon if Explorer
/// restarts underneath us.
static TASKBAR_CREATED: AtomicIsize = AtomicIsize::new(0);
/// Module handle, kept so the flyout can be created lazily on first click.
static HINSTANCE: AtomicIsize = AtomicIsize::new(0);
/// Set once the flyout window class has been registered.
static POPUP_REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Create the flyout on demand and return its handle.
pub fn ensure_popup(app: &mut App) -> HWND {
    if !app.popup.is_null() {
        return app.popup;
    }
    let hinstance = HINSTANCE.load(Ordering::Relaxed) as windows_sys::Win32::Foundation::HMODULE;
    if !POPUP_REGISTERED.swap(true, Ordering::Relaxed) {
        popup::register(hinstance);
    }
    let app_ptr: *mut App = app;
    let owner = MAIN_HWND.load(Ordering::Relaxed) as HWND;
    app.popup = popup::create(hinstance, owner, app_ptr);
    app.popup
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Called from the popup when the interval changes.
pub fn request_reschedule(_from: HWND) {
    let h = MAIN_HWND.load(Ordering::Relaxed);
    if h != 0 {
        reschedule(h as HWND);
    }
}

/// True while the flyout is on screen, so sampling runs live.
static FAST_SAMPLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Switch between live and background sampling.
///
/// Nobody is watching a chart that is not on screen, so the fast rate only
/// applies while the flyout is open. The moment it closes we drop back to the
/// configured interval and the process goes quiet again.
pub fn set_fast_sampling(on: bool) {
    if FAST_SAMPLING.swap(on, Ordering::Relaxed) != on {
        let h = MAIN_HWND.load(Ordering::Relaxed);
        if h != 0 {
            reschedule(h as HWND);
        }
    }
}

/// Sampling interval in seconds for the current mode.
fn effective_interval_s(app: &App) -> u32 {
    if FAST_SAMPLING.load(Ordering::Relaxed) {
        // Never faster than the backend will tolerate: AMD's SDK degrades if
        // GetCPUParameters is called more than once a second.
        app.sensors.min_interval_s().max(1)
    } else {
        app.config.interval_s.max(app.sensors.min_interval_s())
    }
}

fn reschedule(hwnd: HWND) {
    let app_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    if app_ptr.is_null() {
        return;
    }
    let interval_s = effective_interval_s(unsafe { &*app_ptr });
    let ms = interval_s.saturating_mul(1000).max(1000);

    unsafe {
        KillTimer(hwnd, TIMER_ID);
        // A tolerance of 10% lets the kernel align our wakeup with timers that
        // are already scheduled, which is what keeps an idle machine idle.
        // While live-sampling the tolerance is dropped to keep the chart
        // ticking evenly -- there is no power win worth a visibly uneven trace
        // when the user is looking straight at it.
        let tolerance = if FAST_SAMPLING.load(Ordering::Relaxed) {
            0
        } else {
            (ms / 10).max(50)
        };
        if SetCoalescableTimer(hwnd, TIMER_ID, ms, None, tolerance) == 0 {
            SetTimer(hwnd, TIMER_ID, ms, None);
        }
    }
}

// windows-sys does not surface SetCoalescableTimer in every version, so bind
// it directly. It has been present since Windows 8.
#[link(name = "user32")]
extern "system" {
    fn SetCoalescableTimer(
        hwnd: HWND,
        n_id_event: usize,
        u_elapse: u32,
        lp_timer_func: Option<unsafe extern "system" fn(HWND, u32, usize, u32)>,
        u_tolerance_delay: u32,
    ) -> usize;
}

/// `--probe` takes one sample of every sensor and writes it to a text file,
/// then exits. The app is a GUI subsystem binary with no console attached, so
/// a file is the only way to get diagnostics out of it.
fn probe() {
    let mut app = App::new();
    app.sample();

    let mut out = String::new();
    out.push_str(&format!(
        "cpu backend: {}\n",
        app.sensors.cpu_status.message()
    ));
    if !app.sensors.cpu_source.is_empty() {
        out.push_str(&format!("cpu source : {}\n", app.sensors.cpu_source));
    }

    // Only diagnose when the backend did not come up. The SDK is a singleton,
    // so re-running its init sequence while a live instance exists always
    // fails, which would make a working setup look broken.
    if app.sensors.cpu_status != sensors::driver::DriverStatus::Ready {
        out.push_str(
            "
--- ryzen sdk diagnosis ---
",
        );
        out.push_str(&sensors::ryzen_sdk::diagnose());
        out.push_str(
            "---

",
        );
    }
    out.push_str(&format!(
        "elevated   : {}
",
        sensors::cpu::is_elevated()
    ));

    // Report the SDK's install state explicitly: "is it even there" is the
    // first question when the CPU row is missing.
    match sensors::ryzen_sdk::locate() {
        Some(loc) => {
            out.push_str(&format!("ryzen sdk  : found at {}\n", loc.root.display()));
            out.push_str(&format!(
                "  dll      : {}\n",
                loc.dll
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "not found".into())
            ));
        }
        None => out.push_str("ryzen sdk  : not installed\n"),
    }

    out.push_str(&format!("channels   : {}\n\n", app.sensors.channels.len()));
    for (i, c) in app.sensors.channels.iter().enumerate() {
        let v = app.latest.get(i).copied().unwrap_or(history::NO_VALUE);
        let kind = match c.kind {
            sensors::Kind::Cpu => "CPU",
            sensors::Kind::Gpu => "GPU",
            sensors::Kind::Storage => "DISK",
        };
        out.push_str(&format!(
            "[{kind:4}] {:<24} {}\n",
            c.label,
            graph::format_temp(v, false, true)
        ));
    }

    let path = std::env::current_exe()
        .map(|p| p.with_file_name("probe.txt"))
        .unwrap_or_else(|_| std::path::PathBuf::from("probe.txt"));
    let _ = std::fs::write(path, out);
}

/// `--shot` renders the flyout offscreen to a .bmp so the layout can be
/// inspected without a display. History is seeded with a synthetic sweep so
/// the chart has a curve to draw rather than a flat line.
fn shot() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };

    let mut app = App::new();
    app.sample();

    for step in 0..180u32 {
        for (i, h) in app.histories.iter_mut().enumerate() {
            let base = 400 + (i as i32) * 130;
            let wave = ((step as f32 * 0.09 + i as f32).sin() * 95.0) as i32;
            let ramp = (step as i32) / 3;
            h.push((base + wave + ramp) as i16);
        }
        let last = app.times.time_bounds(1).map(|(_, t)| t).unwrap_or(0);
        app.times
            .push(last + u64::from(app.config.interval_s) * 1000);
    }
    for (i, h) in app.histories.iter().enumerate() {
        if let Some(v) = h.range_recent(240) {
            app.latest[i] = v.1;
        }
    }

    let path = std::env::current_exe()
        .map(|p| p.with_file_name("popup.bmp"))
        .unwrap_or_else(|_| std::path::PathBuf::from("popup.bmp"));
    popup::render_to_file(hinstance, &mut app, &path.to_string_lossy());
}

fn main() {
    if std::env::args().any(|a| a == "--probe") {
        probe();
        return;
    }
    if std::env::args().any(|a| a == "--shot") {
        shot();
        return;
    }

    unsafe {
        // Per-monitor v2 so the flyout and tray icon stay crisp when dragged
        // between displays with different scaling.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };

    let class = wide("tempmanagerMain");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(main_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    unsafe { RegisterClassExW(&wc) };

    let title = wide("tempmanager");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return;
    }
    MAIN_HWND.store(hwnd as isize, Ordering::Relaxed);

    let msg_name = wide("TaskbarCreated");
    TASKBAR_CREATED.store(
        unsafe { RegisterWindowMessageW(msg_name.as_ptr()) } as isize,
        Ordering::Relaxed,
    );

    // App state outlives the message loop; the window procs borrow it.
    let mut app = Box::new(App::new());
    let app_ptr: *mut App = &mut *app;
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize) };

    // The flyout window is created on first click, not here. Creating it up
    // front drags in DWM composition resources and a swathe of GDI state for
    // a window that most sessions never open.
    HINSTANCE.store(hinstance as isize, Ordering::Relaxed);

    let mut tray = tray::Tray::new(hwnd);

    // Prime the display immediately rather than showing "--" until the first
    // interval elapses.
    app.sample();
    let (text, color, tip) = app.tray_display();
    tray.update(&text, color, &tip);
    app.tray_dirty = false;

    // Stash the tray so the window proc can reach it.
    TRAY.with(|t| *t.borrow_mut() = Some(tray));

    reschedule(hwnd);

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe {
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    TRAY.with(|t| t.borrow_mut().take());
}

thread_local! {
    static TRAY: std::cell::RefCell<Option<tray::Tray>> = const { std::cell::RefCell::new(None) };
}

fn refresh_tray(app: &mut App) {
    let (text, color, tip) = app.tray_display();
    TRAY.with(|t| {
        if let Some(tray) = t.borrow_mut().as_mut() {
            tray.update(&text, color, &tip);
        }
    });
    app.tray_dirty = false;
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Relaxed) as u32;
    if taskbar_created != 0 && msg == taskbar_created {
        TRAY.with(|t| {
            if let Some(tray) = t.borrow_mut().as_mut() {
                tray.readd();
            }
        });
        return 0;
    }

    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if app_ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let app = &mut *app_ptr;

    match msg {
        WM_TIMER if wparam == TIMER_ID => {
            if !app.sample() {
                return 0;
            }
            if app.tray_dirty {
                refresh_tray(app);
            }
            // Only repaint the flyout if it is actually on screen.
            if !app.popup.is_null() && IsWindowVisible(app.popup) != 0 {
                windows_sys::Win32::Graphics::Gdi::InvalidateRect(app.popup, std::ptr::null(), 0);
            }
            0
        }

        tray::WM_TRAY => {
            // With NOTIFYICON_VERSION_4 the event is in the low word of lparam
            // and the cursor position is packed into wparam.
            let event = (lparam & 0xFFFF) as u32;
            let pt = POINT {
                x: (wparam & 0xFFFF) as i16 as i32,
                y: ((wparam >> 16) & 0xFFFF) as i16 as i32,
            };
            match event {
                NIN_SELECT | NIN_KEYSELECT => {
                    ensure_popup(app);
                    if IsWindowVisible(app.popup) != 0 {
                        popup::hide(app.popup);
                    } else {
                        popup::show_at_cursor(app.popup, app, pt);
                    }
                }
                WM_CONTEXTMENU => show_menu(hwnd, app, pt),
                _ => {}
            }
            0
        }

        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            match id {
                ID_HOTTEST => app.config.tray_source = TraySource::HottestOverall,
                ID_CPU => app.config.tray_source = TraySource::Cpu,
                ID_GPU => app.config.tray_source = TraySource::Gpu,
                ID_FAHRENHEIT => app.config.fahrenheit = !app.config.fahrenheit,
                ID_AUTOSTART => {
                    app.config.autostart = !app.config.autostart;
                    set_autostart(app.config.autostart);
                }
                ID_ELEVATE => {
                    restart_elevated();
                    return 0;
                }
                ID_EXIT => {
                    PostQuitMessage(0);
                    return 0;
                }
                _ => return 0,
            }
            app.config.save();
            refresh_tray(app);
            if IsWindowVisible(app.popup) != 0 {
                windows_sys::Win32::Graphics::Gdi::InvalidateRect(app.popup, std::ptr::null(), 0);
            }
            0
        }

        WM_DESTROY => {
            KillTimer(hwnd, TIMER_ID);
            PostQuitMessage(0);
            0
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show_menu(hwnd: HWND, app: &App, pt: POINT) {
    let menu: HMENU = CreatePopupMenu();
    if menu.is_null() {
        return;
    }

    let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
    let src = app.config.tray_source;

    AppendMenuW(
        menu,
        MF_STRING | check(src == TraySource::HottestOverall),
        ID_HOTTEST,
        wide("Tray shows hottest").as_ptr(),
    );
    if app.sensors.first_of(sensors::Kind::Cpu).is_some() {
        AppendMenuW(
            menu,
            MF_STRING | check(src == TraySource::Cpu),
            ID_CPU,
            wide("Tray shows CPU").as_ptr(),
        );
    }
    if app.sensors.first_of(sensors::Kind::Gpu).is_some() {
        AppendMenuW(
            menu,
            MF_STRING | check(src == TraySource::Gpu),
            ID_GPU,
            wide("Tray shows GPU").as_ptr(),
        );
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(
        menu,
        MF_STRING | check(app.config.fahrenheit),
        ID_FAHRENHEIT,
        wide("Fahrenheit").as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING | check(app.config.autostart),
        ID_AUTOSTART,
        wide("Start with Windows").as_ptr(),
    );
    // Only worth offering when it would actually change what we can read.
    if !sensors::cpu::is_elevated()
        && app.sensors.cpu_status != sensors::driver::DriverStatus::Ready
    {
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(
            menu,
            MF_STRING,
            ID_ELEVATE,
            wide("Restart as administrator").as_ptr(),
        );
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, ID_EXIT, wide("Exit").as_ptr());

    // Required so the menu dismisses when the user clicks elsewhere.
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_RIGHTALIGN | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    DestroyMenu(menu);
}

/// Relaunch elevated and exit.
///
/// AMD's Platform.dll refuses to initialise for a non-elevated caller even
/// when its driver service is already running, so reading a Ryzen die
/// temperature means running with an elevated token. There is no way around
/// that from user mode.
fn restart_elevated() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let verb = wide("runas");
    let path = wide(&exe.to_string_lossy());
    let rc = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns >32 on success. If the user declined the UAC
    // prompt, stay running unelevated rather than quitting on them.
    if rc as isize > 32 {
        unsafe { PostQuitMessage(0) };
    }
}

/// Run a helper process without flashing a console window.
fn run_hidden(program: &str, args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

const TASK_NAME: &str = "tempmanager";

/// Autostart via a scheduled task running with highest privileges.
///
/// A Run-key entry cannot launch elevated, so it would start the app without
/// CPU temperatures every login. A logon task with `/rl highest` starts
/// elevated and, unlike the Run key, does so without a UAC prompt. Creating it
/// needs admin, so we fall back to the Run key when not elevated.
fn set_autostart_task(enabled: bool) -> bool {
    if !enabled {
        return run_hidden("schtasks", &["/delete", "/tn", TASK_NAME, "/f"]);
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let target = format!("\"{}\"", exe.to_string_lossy());
    run_hidden(
        "schtasks",
        &[
            "/create", "/tn", TASK_NAME, "/tr", &target, "/sc", "onlogon", "/rl", "highest", "/f",
        ],
    )
}

fn set_autostart(enabled: bool) {
    // Prefer the elevated task when we are in a position to create one.
    if sensors::cpu::is_elevated() && set_autostart_task(enabled) {
        // Drop any stale Run-key entry so the app cannot start twice.
        set_autostart_run_key(false);
        return;
    }
    set_autostart_run_key(enabled);
}

fn set_autostart_run_key(enabled: bool) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let subkey = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let name = wide("tempmanager");

    let mut key: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if rc != 0 || key.is_null() {
        return;
    }

    if enabled {
        // Quote the path so a directory with spaces still launches.
        let value = wide(&format!("\"{}\"", exe.to_string_lossy()));
        unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                (value.len() * 2) as u32,
            )
        };
    } else {
        unsafe { RegDeleteValueW(key, name.as_ptr()) };
    }
    unsafe { RegCloseKey(key) };
}
