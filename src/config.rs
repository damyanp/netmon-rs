//! App configuration and target list. On first run it seeds generic defaults
//! (auto-detected gateway + a couple of internet hosts) and persists everything
//! — ping interval, display window, and targets — to `settings.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Hide the console window spawned by `ipconfig` (CREATE_NO_WINDOW).
const NO_WINDOW: u32 = 0x0800_0000;

/// A ping target. `ip` self-heals from the ARP table when `mac` is set. Named
/// `host` in the config file since it may be a hostname rather than an IP.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub name: String,
    #[serde(rename = "host")]
    pub ip: String,
    #[serde(default)]
    pub mac: Option<String>,
}

impl Target {
    pub fn new(name: &str, ip: &str, mac: Option<&str>) -> Self {
        Self {
            name: name.into(),
            ip: ip.into(),
            mac: mac.map(Into::into),
        }
    }
}

/// The set of display-window options (in minutes) the UI offers.
pub const WINDOW_MINS: [i64; 5] = [10, 30, 60, 180, 360];

pub struct Config {
    pub interval_ms: u32,
    pub window_mins: i64,
    pub timeout_ms: u32,
    pub history_max_age_ms: i64,
    pub history_max_samples: usize,
    pub mac_resolve_ms: u64,
    pub targets: Vec<Target>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            window_mins: 10,
            timeout_ms: 1500,
            history_max_age_ms: 6 * 60 * 60 * 1000,
            history_max_samples: 60_000,
            mac_resolve_ms: 30_000,
            targets: default_targets(),
        }
    }
}

/// Generic starter targets for a fresh install: the machine's own gateway plus
/// two well-known internet endpoints. No hardcoded MAC.
fn default_targets() -> Vec<Target> {
    let gateway = detect_gateway().unwrap_or_else(|| "192.168.1.1".into());
    vec![
        Target::new("Gateway", &gateway, None),
        Target::new("Internet (8.8.8.8)", "8.8.8.8", None),
        Target::new("MSFT NCSI", "www.msftconnecttest.com", None),
    ]
}

/// On-disk settings schema (v2). Fields default individually so a v1 file
/// (`{"intervalMs":N}`) still parses and migrates cleanly.
#[derive(Serialize, Deserialize)]
struct Settings {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(rename = "intervalMs", default = "default_interval")]
    interval_ms: u32,
    #[serde(rename = "windowMins", default = "default_window")]
    window_mins: i64,
    #[serde(default)]
    targets: Vec<Target>,
}

fn default_version() -> u32 {
    2
}
fn default_interval() -> u32 {
    1000
}
fn default_window() -> i64 {
    10
}

impl Config {
    /// Load defaults, then apply persisted settings. Migrates old files and, on
    /// first run, writes out the seeded defaults so they're stable thereafter.
    pub fn load() -> Self {
        let mut cfg = Config::default();
        match std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
        {
            Some(s) => {
                cfg.interval_ms = clamp_interval(s.interval_ms);
                cfg.window_mins = clamp_window(s.window_mins);
                if !s.targets.is_empty() {
                    cfg.targets = s.targets;
                }
            }
            None => {
                // First run (or an unreadable file): persist the seeded defaults.
                save_settings(cfg.interval_ms, cfg.window_mins, &cfg.targets);
            }
        }
        cfg
    }
}

/// Clamp the ping interval to the same 1s..60s range as the Node app.
pub fn clamp_interval(ms: u32) -> u32 {
    ms.clamp(1000, 60_000)
}

/// Snap a window value to one of the offered options, defaulting to 10 min.
pub fn clamp_window(mins: i64) -> i64 {
    if WINDOW_MINS.contains(&mins) { mins } else { 10 }
}

/// Persist all settings to `settings.json`.
pub fn save_settings(interval_ms: u32, window_mins: i64, targets: &[Target]) {
    let settings = Settings {
        version: 2,
        interval_ms,
        window_mins,
        targets: targets.to_vec(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&settings) {
        let _ = std::fs::write(settings_path(), json);
    }
}

/// Best-effort default-gateway lookup by parsing `ipconfig` output.
fn detect_gateway() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("ipconfig")
        .creation_flags(NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if line.contains("Default Gateway")
            && let Some(value) = line.rsplit(':').next()
        {
            let value = value.trim();
            if is_ipv4(value) {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn is_ipv4(s: &str) -> bool {
    let mut parts = 0;
    for octet in s.split('.') {
        parts += 1;
        if octet.is_empty() || octet.parse::<u8>().is_err() {
            return false;
        }
    }
    parts == 4
}

/// Directory next to the executable, so data files sit beside the app.
fn data_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn history_path() -> PathBuf {
    data_dir().join("history.json")
}
