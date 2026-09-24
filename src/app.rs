//! Shared application state.
//!
//! One instance lives for the process lifetime and is reached from both window
//! procedures through GWLP_USERDATA. Everything is single-threaded: there is no
//! background sampling thread, because a thread that wakes up on a timer costs
//! more than letting the existing message loop do the work.

use crate::config::{Config, TraySource, MAX_INTERVAL_S, MIN_INTERVAL_S};
use crate::graph;
use crate::history::{History, CAPACITY, NO_VALUE};
use crate::sensors::{driver::DriverStatus, Kind, Sensors};
use crate::theme;

/// Discrete slider stops, in seconds. Discrete beats a continuous 1..300
/// range: the useful values are not evenly spread, and every stop lands on a
/// number worth choosing.
pub const STEPS: [u32; 13] = [1, 2, 5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300];

pub struct App {
    pub config: Config,
    pub sensors: Sensors,
    /// One ring buffer per channel, index-aligned with `sensors.channels`.
    pub histories: Vec<History>,
    /// Scratch for the most recent sample, reused every tick.
    pub latest: Vec<i16>,
    pub tray_dirty: bool,
    /// Monotonic timestamps in milliseconds, aligned with every channel.
    /// GetTickCount64 includes sleep and is unaffected by wall-clock changes.
    pub times: History<u64>,
    pub popup: windows_sys::Win32::Foundation::HWND,
    /// True while the user is dragging the interval slider.
    pub dragging: bool,
}

impl App {
    pub fn new() -> Self {
        let config = Config::load();
        let sensors = Sensors::probe();
        let n = sensors.channels.len();
        Self {
            config,
            sensors,
            histories: (0..n).map(|_| History::new()).collect(),
            times: History::new(),
            latest: vec![NO_VALUE; n],
            tray_dirty: true,
            popup: std::ptr::null_mut(),
            dragging: false,
        }
    }

    /// Take one reading from every sensor and append it to the history.
    pub fn sample(&mut self) -> bool {
        let now = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
        self.sample_at(now)
    }

    fn sample_at(&mut self, now: u64) -> bool {
        // A queued WM_TIMER can survive a timer reset. Enforce the backend's
        // minimum gap at the actual read, as well as when scheduling timers.
        if let Some((_, last)) = self.times.time_bounds(1) {
            if now.saturating_sub(last) < u64::from(self.sensors.min_interval_s().max(1)) * 1000 {
                return false;
            }
        }
        self.tray_dirty |= self.sensors.sample_into(&mut self.latest);
        for (i, v) in self.latest.iter().enumerate() {
            if let Some(h) = self.histories.get_mut(i) {
                h.push(*v);
            }
        }
        self.times.push(now);
        true
    }

    /// Which channel the tray icon is currently showing.
    pub fn tray_channel(&self) -> Option<usize> {
        match self.config.tray_source {
            TraySource::HottestOverall => self.sensors.hottest(&self.latest),
            TraySource::Cpu => self.sensors.first_of(Kind::Cpu),
            TraySource::Gpu => self.sensors.first_of(Kind::Gpu),
        }
    }

    /// (icon text, icon colour, tooltip) for the current state.
    pub fn tray_display(&self) -> (String, u32, String) {
        let Some(idx) = self.tray_channel() else {
            let tip = match self.sensors.cpu_status {
                DriverStatus::Ready => "tempmanager - no sensors found".to_string(),
                s => format!("tempmanager - {}", s.message()),
            };
            return ("--".to_string(), theme::TEXT_DIM, tip);
        };

        let v = self.latest.get(idx).copied().unwrap_or(NO_VALUE);
        let ch = &self.sensors.channels[idx];
        let color = if v == NO_VALUE {
            theme::TEXT_DIM
        } else {
            theme::status_color(v, ch.warn, ch.hot)
        };

        // The tooltip carries every channel, so hovering answers the question
        // without needing to open the popup at all.
        let mut tip = String::with_capacity(64);
        for (i, c) in self.sensors.channels.iter().enumerate() {
            if i > 0 {
                tip.push('\n');
            }
            let val = self.latest.get(i).copied().unwrap_or(NO_VALUE);
            tip.push_str(&format!(
                "{}: {}",
                c.label,
                graph::format_temp(val, self.config.fahrenheit, true)
            ));
        }
        if tip.is_empty() {
            tip.push_str("tempmanager");
        }

        (graph::format_tray(v, self.config.fahrenheit), color, tip)
    }

    /// Index into STEPS for the configured interval, snapping to the nearest.
    pub fn step_index(&self) -> usize {
        STEPS
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| (**s as i32 - self.config.interval_s as i32).abs())
            .map(|(i, _)| i)
            .unwrap_or(6)
    }

    pub fn set_step(&mut self, i: usize) -> bool {
        let v = STEPS[i.min(STEPS.len() - 1)].clamp(MIN_INTERVAL_S, MAX_INTERVAL_S);
        if v == self.config.interval_s {
            return false;
        }
        self.config.interval_s = v;
        true
    }

    pub fn graph_window(&self) -> usize {
        CAPACITY.min(240)
    }

    pub fn series_color(&self, i: usize) -> u32 {
        theme::series_color(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_record_actual_time_and_reject_queued_early_ticks() {
        let mut app = App {
            config: Config::default(),
            sensors: Sensors::test_fixture(Vec::new()),
            histories: Vec::new(),
            latest: Vec::new(),
            tray_dirty: false,
            times: History::new(),
            popup: std::ptr::null_mut(),
            dragging: false,
        };
        assert!(app.sample_at(10_000));
        assert!(!app.sample_at(10_999));
        assert!(app.sample_at(40_500));
        app.config.interval_s = 1;
        assert!(app.sample_at(41_600));
        // A resume after a long sleep must not be recorded as a one-second gap.
        assert!(app.sample_at(86_441_600));
        assert_eq!(
            app.times.iter_recent(4).collect::<Vec<_>>(),
            [10_000, 40_500, 41_600, 86_441_600]
        );
    }
}
