//! Background network monitor: pings each target on an interval, self-heals
//! target IPs by MAC via the ARP table, and appends results to shared history.
//! Runs on a worker thread; the UI only reads the shared state.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{self, Config, Target};
use crate::history::{History, Sample};

/// Hide the console window spawned by `ping`/`arp` (CREATE_NO_WINDOW).
const NO_WINDOW: u32 = 0x0800_0000;

/// State shared between the worker thread and the UI thread.
pub struct AppState {
    pub history: History,
    pub interval_ms: u32,
    pub targets: Vec<Target>,
    /// Bumped whenever a new sample lands, so the UI can cheaply detect changes.
    pub revision: u64,
}

pub type Shared = Arc<Mutex<AppState>>;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Build the shared state. History starts empty each launch — stale samples
/// from a previous run would otherwise show up as bogus packet loss (their
/// target names no longer match the current config). The old file is removed.
pub fn init_shared(cfg: &Config) -> Shared {
    let _ = std::fs::remove_file(config::history_path());
    Arc::new(Mutex::new(AppState {
        history: History::default(),
        interval_ms: cfg.interval_ms,
        targets: cfg.targets.clone(),
        revision: 0,
    }))
}

/// Wipe all collected history in place (used by the settings "Clear data"
/// button). Bumps the revision so the UI redraws.
pub fn clear_history(shared: &Shared) {
    {
        let mut st = shared.lock().unwrap();
        st.history.samples.clear();
        st.revision = st.revision.wrapping_add(1);
    }
    let _ = std::fs::remove_file(config::history_path());
}

/// Spawn the monitor worker. It runs for the lifetime of the process.
pub fn spawn(shared: Shared, cfg: Config) {
    thread::spawn(move || worker(shared, cfg));
}

fn worker(shared: Shared, cfg: Config) {
    // `None` means "never resolved yet", so the first loop iteration resolves
    // immediately. Avoids `Instant - Duration`, which panics on short uptimes.
    let mut last_resolve: Option<Instant> = None;
    let mut last_save = Instant::now();

    loop {
        // Snapshot the mutable bits under the lock, then release it for the
        // slow ping/arp work.
        let (mut targets, interval_ms) = {
            let st = shared.lock().unwrap();
            (st.targets.clone(), st.interval_ms)
        };

        if last_resolve.is_none_or(|t| t.elapsed() >= Duration::from_millis(cfg.mac_resolve_ms)) {
            resolve_macs(&mut targets, cfg.timeout_ms);
            last_resolve = Some(Instant::now());
            // Push any healed IPs back into shared state (matched by name).
            let mut st = shared.lock().unwrap();
            for t in &targets {
                if let Some(cur) = st.targets.iter_mut().find(|x| x.name == t.name) {
                    cur.ip = t.ip.clone();
                }
            }
        }

        let sample = take_sample(&targets, cfg.timeout_ms);
        {
            let mut st = shared.lock().unwrap();
            st.history.push(sample);
            st.history
                .prune(now_ms(), cfg.history_max_age_ms, cfg.history_max_samples);
            st.revision = st.revision.wrapping_add(1);
        }
        // Debounced persistence (~every 2s).
        if last_save.elapsed() >= Duration::from_secs(2) {
            let st = shared.lock().unwrap();
            st.history.save(&config::history_path());
            drop(st);
            last_save = Instant::now();
        }

        thread::sleep(Duration::from_millis(interval_ms as u64));
    }
}

/// Ping every target in parallel and collect one `Sample`.
fn take_sample(targets: &[Target], timeout_ms: u32) -> Sample {
    let handles: Vec<_> = targets
        .iter()
        .map(|t| {
            let ip = t.ip.clone();
            let name = t.name.clone();
            thread::spawn(move || (name, ping(&ip, timeout_ms)))
        })
        .collect();

    let mut v = std::collections::BTreeMap::new();
    for h in handles {
        if let Ok((name, latency)) = h.join() {
            v.insert(name, latency);
        }
    }
    Sample { t: now_ms(), v }
}

/// Ping one host once. Returns the latency in ms, or `None` on drop/timeout.
fn ping(ip: &str, timeout_ms: u32) -> Option<u32> {
    let out = Command::new("ping")
        .args(["-n", "1", "-w", &timeout_ms.to_string(), ip])
        .creation_flags(NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // A real reply has both a `time=NNms` (or `time<1ms`) and a `TTL=` field.
    if !text.contains("TTL=") && !text.contains("ttl=") {
        return None;
    }
    parse_latency(&text)
}

/// Extract the latency from a `time=12ms` / `time<1ms` fragment.
fn parse_latency(text: &str) -> Option<u32> {
    let idx = text.find("time").or_else(|| text.find("TIME"))?;
    let rest = &text[idx + 4..];
    let rest = rest.strip_prefix('=').or_else(|| rest.strip_prefix('<'))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        // `time<1ms` with the `<` consumed leaves the digit; empty means no match.
        return None;
    }
    digits.parse().ok()
}

fn norm_mac(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Format a MAC as canonical uppercase dash-separated pairs (e.g. `0C-EF-15-...`).
fn fmt_mac(s: &str) -> String {
    let hex = norm_mac(s).to_uppercase();
    hex.as_bytes()
        .chunks(2)
        .filter_map(|c| std::str::from_utf8(c).ok())
        .collect::<Vec<_>>()
        .join("-")
}

/// Look up the MAC for an IP via the ARP table, priming it with a ping first.
/// Only works for hosts on the local subnet (ARP is link-local).
pub fn resolve_mac_for_ip(ip: &str, timeout_ms: u32) -> Option<String> {
    if !is_ipv4(ip) {
        return None;
    }
    let _ = Command::new("ping")
        .args(["-n", "1", "-w", &timeout_ms.to_string(), ip])
        .creation_flags(NO_WINDOW)
        .output();
    let out = Command::new("arp")
        .arg("-a")
        .creation_flags(NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if let (Some(found_ip), Some(mac)) = (tokens.first(), tokens.get(1))
            && *found_ip == ip
            && is_mac(mac)
        {
            return Some(fmt_mac(mac));
        }
    }
    None
}

/// Read the OS ARP table into a map of normalized-MAC -> IP.
fn read_arp_table() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(out) = Command::new("arp")
        .arg("-a")
        .creation_flags(NO_WINDOW)
        .output()
    else {
        return map;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if let (Some(ip), Some(mac)) = (tokens.first(), tokens.get(1))
            && is_ipv4(ip)
            && is_mac(mac)
        {
            map.insert(norm_mac(mac), (*ip).to_string());
        }
    }
    map
}

fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok())
}

fn is_mac(s: &str) -> bool {
    let seps = s.contains('-') || s.contains(':');
    seps && norm_mac(s).len() == 12
}

/// Ping every address in the local /24 to repopulate the ARP table.
fn sweep_subnet(sample_ip: &str) {
    let Some(base) = sample_ip.rsplit_once('.').map(|(b, _)| b) else {
        return;
    };
    let mut handles = Vec::new();
    for i in 1..=254 {
        let target = format!("{base}.{i}");
        handles.push(thread::spawn(move || {
            let _ = Command::new("ping")
                .args(["-n", "1", "-w", "200", &target])
                .creation_flags(NO_WINDOW)
                .output();
        }));
    }
    for h in handles {
        let _ = h.join();
    }
}

/// Keep MAC-pinned targets pointed at the right IP.
fn resolve_macs(targets: &mut [Target], _timeout_ms: u32) {
    if !targets.iter().any(|t| t.mac.is_some()) {
        return;
    }
    let mut arp = read_arp_table();

    let missing = targets
        .iter()
        .filter_map(|t| t.mac.as_deref())
        .any(|m| !arp.contains_key(&norm_mac(m)));
    if missing {
        if let Some(lan) = targets.iter().find(|t| t.ip.starts_with("192.168.")) {
            let ip = lan.ip.clone();
            sweep_subnet(&ip);
            thread::sleep(Duration::from_millis(2500));
            arp = read_arp_table();
        }
    }

    for t in targets.iter_mut() {
        if let Some(mac) = &t.mac
            && let Some(found) = arp.get(&norm_mac(mac))
            && found != &t.ip
        {
            t.ip = found.clone();
        }
    }
}
