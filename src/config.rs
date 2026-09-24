//! Persisted settings. Deliberately hand-rolled `key=value` parsing so we do
//! not pull in serde + a format crate for six fields.

use std::fs;
use std::path::PathBuf;

pub const MIN_INTERVAL_S: u32 = 1;
pub const MAX_INTERVAL_S: u32 = 300;

#[derive(Clone, Copy)]
pub struct Config {
    /// Seconds between sensor samples.
    pub interval_s: u32,
    /// Which sensor's value is painted into the tray icon.
    pub tray_source: TraySource,
    /// Show degrees as Fahrenheit in the UI (samples are always stored in C).
    pub fahrenheit: bool,
    /// Start with Windows (writes the Run key on toggle).
    pub autostart: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TraySource {
    HottestOverall,
    Cpu,
    Gpu,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_s: 30,
            tray_source: TraySource::HottestOverall,
            fahrenheit: false,
            autostart: false,
        }
    }
}

pub fn config_path() -> PathBuf {
    let mut p = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    p.push("tempmanager");
    let _ = fs::create_dir_all(&p);
    p.push("config.ini");
    p
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config::default();
        let Ok(text) = fs::read_to_string(config_path()) else {
            return c;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "interval_s" => {
                    if let Ok(n) = v.parse::<u32>() {
                        c.interval_s = n.clamp(MIN_INTERVAL_S, MAX_INTERVAL_S);
                    }
                }
                "tray_source" => {
                    c.tray_source = match v {
                        "cpu" => TraySource::Cpu,
                        "gpu" => TraySource::Gpu,
                        _ => TraySource::HottestOverall,
                    }
                }
                "fahrenheit" => c.fahrenheit = v == "1" || v == "true",
                "autostart" => c.autostart = v == "1" || v == "true",
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let src = match self.tray_source {
            TraySource::Cpu => "cpu",
            TraySource::Gpu => "gpu",
            TraySource::HottestOverall => "hottest",
        };
        let body = format!(
            "# tempmanager settings\ninterval_s={}\ntray_source={}\nfahrenheit={}\nautostart={}\n",
            self.interval_s, src, self.fahrenheit as u8, self.autostart as u8
        );
        let _ = fs::write(config_path(), body);
    }
}
