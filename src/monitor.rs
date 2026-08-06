//! Background network monitor: pings each target on an interval, self-heals
//! target IPs by MAC via the ARP table, and appends results to shared history.
//! Runs on a worker thread; the UI only reads the shared state.

use std::collections::{HashMap, HashSet};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{self, Config, Target};
use crate::history::{History, Sample};
use crate::notification;

/// Hide the console window spawned by `ping`/`arp` (CREATE_NO_WINDOW).
const NO_WINDOW: u32 = 0x0800_0000;

/// Intervals auto mode walks between, fastest first. Idle sits on the last
/// rung; a drop snaps straight back to the first.
pub const AUTO_LADDER: [u32; 4] = [1_000, 2_000, 5_000, 10_000];

/// Consecutive loss-free rounds needed before auto mode steps one rung slower.
pub const AUTO_BACKOFF_RUN: u32 = 10;

/// Floor on the gap between two consecutive pings, so a long target list can't
/// turn into a burst.
const MIN_PING_GAP_MS: u32 = 50;

/// Auto-interval state machine. Any drop on any target drops us to the fastest
/// rung immediately; recovery is gradual, one rung per clean round-robin run,
/// so a flapping link keeps sampling fast instead of oscillating.
pub struct AutoInterval {
    step: usize,
    clean_run: u32,
}

impl Default for AutoInterval {
    fn default() -> Self {
        Self {
            step: AUTO_LADDER.len() - 1,
            clean_run: 0,
        }
    }
}

impl AutoInterval {
    /// The interval each target is currently pinged at.
    pub fn interval_ms(&self) -> u32 {
        AUTO_LADDER[self.step]
    }

    /// A packet was dropped: snap to the fastest rung right away.
    pub fn on_loss(&mut self) {
        self.step = 0;
        self.clean_run = 0;
    }

    /// One full pass over every target completed with no drops.
    pub fn on_clean_round(&mut self) {
        self.clean_run += 1;
        if self.clean_run >= AUTO_BACKOFF_RUN {
            self.clean_run = 0;
            self.step = (self.step + 1).min(AUTO_LADDER.len() - 1);
        }
    }

    /// Clean rounds still needed before the next step down in speed, or `None`
    /// once we're already at the slowest rung.
    pub fn clean_needed(&self) -> Option<u32> {
        (self.step + 1 < AUTO_LADDER.len()).then(|| AUTO_BACKOFF_RUN - self.clean_run)
    }
}

/// State shared between the worker thread and the UI thread.
pub struct AppState {
    pub history: History,
    /// Whether the monitor picks its own interval (see [`AutoInterval`]).
    pub auto_interval: bool,
    /// The manual interval, used when `auto_interval` is off.
    pub interval_ms: u32,
    /// The interval actually in force right now. Equals `interval_ms` unless
    /// auto mode has sped things up.
    pub current_interval_ms: u32,
    /// In auto mode, how many more clean samples are needed before slowing down
    /// a rung. `None` when already at the slowest rung (or auto is off).
    pub auto_clean_needed: Option<u32>,
    pub window_mins: i64,
    pub packet_loss_alert_threshold: u32,
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
        auto_interval: cfg.auto_interval,
        interval_ms: cfg.interval_ms,
        current_interval_ms: if cfg.auto_interval {
            AUTO_LADDER[AUTO_LADDER.len() - 1]
        } else {
            cfg.interval_ms
        },
        auto_clean_needed: None,
        window_mins: cfg.window_mins,
        packet_loss_alert_threshold: cfg.packet_loss_alert_threshold,
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
    if let Err(e) = unsafe { windows::ro::RoInitialize(windows::ro::RO_INIT_MULTITHREADED) }.ok() {
        eprintln!("failed to initialize Windows Runtime on monitor thread: {e}");
    }

    // `None` means "never resolved yet", so the first loop iteration resolves
    // immediately. Avoids `Instant - Duration`, which panics on short uptimes.
    let mut last_resolve: Option<Instant> = None;
    let mut last_save = Instant::now();
    let mut alerted_targets = HashSet::new();
    let mut auto = AutoInterval::default();
    // Round-robin cursor and whether the current pass has seen any drop.
    let mut next_target = 0usize;
    let mut round_had_loss = false;

    loop {
        // Snapshot the mutable bits under the lock, then release it for the
        // slow ping/arp work.
        let (mut targets, interval_ms, auto_enabled) = {
            let st = shared.lock().unwrap();
            (st.targets.clone(), st.interval_ms, st.auto_interval)
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

        if targets.is_empty() {
            next_target = 0;
            round_had_loss = false;
            thread::sleep(Duration::from_millis(interval_ms as u64));
            continue;
        }

        // Ping one target per iteration so the load is spread evenly across the
        // interval instead of arriving in a burst. The target list can change
        // between iterations, so wrap the cursor defensively.
        next_target %= targets.len();
        let target = &targets[next_target];
        let started = Instant::now();
        let latency = ping(&target.ip, cfg.timeout_ms);
        let sample = Sample {
            t: now_ms(),
            v: std::iter::once((target.name.clone(), latency)).collect(),
        };

        // Auto mode reacts to a drop immediately, but only slows back down on a
        // full clean pass over every target.
        if latency.is_none() {
            round_had_loss = true;
            auto.on_loss();
        }
        next_target += 1;
        if next_target >= targets.len() {
            next_target = 0;
            if !round_had_loss {
                auto.on_clean_round();
            }
            round_had_loss = false;
        }

        let round_ms = if auto_enabled {
            auto.interval_ms()
        } else {
            auto = AutoInterval::default();
            interval_ms
        };
        let alerts = {
            let mut st = shared.lock().unwrap();
            st.current_interval_ms = round_ms;
            st.auto_clean_needed = auto_enabled.then(|| auto.clean_needed()).flatten();
            st.history.push(sample);
            st.history
                .prune(now_ms(), cfg.history_max_age_ms, cfg.history_max_samples);
            st.revision = st.revision.wrapping_add(1);
            evaluate_alerts(
                &st.history,
                &st.targets,
                st.window_mins,
                st.packet_loss_alert_threshold,
                &mut alerted_targets,
                now_ms(),
            )
        };
        if !alerts.is_empty()
            && let Err(e) = notification::show_packet_loss_alert(&alerts)
        {
            eprintln!("failed to show packet-loss notification: {e}");
        }
        // Debounced persistence (~every 2s).
        if last_save.elapsed() >= Duration::from_secs(2) {
            let st = shared.lock().unwrap();
            st.history.save(&config::history_path());
            drop(st);
            last_save = Instant::now();
        }

        // Space the pings evenly: each target still gets one ping per
        // `round_ms`, but consecutive pings are a fraction of that apart.
        // Time already spent waiting on this ping counts toward the gap.
        let gap =
            Duration::from_millis((round_ms / targets.len() as u32).max(MIN_PING_GAP_MS) as u64);
        thread::sleep(gap.saturating_sub(started.elapsed()));
    }
}

/// Cap on the time credit (ms) a single sample can earn, so one long gap — the
/// app was asleep, or a target was just added — can't swamp the window.
const MAX_SAMPLE_WEIGHT_MS: i64 = 60_000;

/// Credit given to a sample with no predecessor to measure a gap against.
const DEFAULT_SAMPLE_WEIGHT_MS: i64 = 1_000;

/// Packet loss for one target over the window ending now, as
/// `(samples measured, loss %)`.
///
/// Loss is weighted by the time each sample stands for (the gap since the
/// previous sample) rather than by raw sample count. With a variable interval
/// that matters: an outage sampled at 1 s would otherwise contribute ten times
/// as many samples per second as the healthy 10 s stretches around it and
/// wildly overstate the loss.
pub fn target_loss(history: &History, target_name: &str, cutoff: i64) -> (usize, u32) {
    let mut measured = 0usize;
    let mut total_weight = 0i64;
    let mut dropped_weight = 0i64;

    for (i, sample) in history.samples.iter().enumerate() {
        if sample.t < cutoff {
            continue;
        }
        let Some(latency) = sample.v.get(target_name) else {
            continue;
        };
        measured += 1;
        let weight = i
            .checked_sub(1)
            .and_then(|prev| history.samples.get(prev))
            .map(|prev| sample.t - prev.t)
            .unwrap_or(DEFAULT_SAMPLE_WEIGHT_MS)
            .clamp(1, MAX_SAMPLE_WEIGHT_MS);
        total_weight += weight;
        if latency.is_none() {
            dropped_weight += weight;
        }
    }

    let loss = if total_weight > 0 {
        (dropped_weight * 100 / total_weight) as u32
    } else {
        0
    };
    (measured, loss)
}

fn evaluate_alerts(
    history: &History,
    targets: &[Target],
    window_mins: i64,
    threshold: u32,
    alerted_targets: &mut HashSet<String>,
    now: i64,
) -> Vec<(String, u32)> {
    let current_names: HashSet<&str> = targets.iter().map(|target| target.name.as_str()).collect();
    alerted_targets.retain(|name| current_names.contains(name.as_str()));

    let cutoff = now - window_mins * 60_000;
    let mut newly_alerting = Vec::new();
    for target in targets {
        let (measured, loss) = target_loss(history, &target.name, cutoff);
        let above = measured >= 10 && loss > threshold;
        if above {
            if alerted_targets.insert(target.name.clone()) {
                newly_alerting.push((target.name.clone(), loss));
            }
        } else {
            alerted_targets.remove(&target.name);
        }
    }
    newly_alerting
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
    if missing && let Some(lan) = targets.iter().find(|t| t.ip.starts_with("192.168.")) {
        let ip = lan.ip.clone();
        sweep_subnet(&ip);
        thread::sleep(Duration::from_millis(2500));
        arp = read_arp_table();
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

#[cfg(test)]
mod tests {
    use super::{AUTO_BACKOFF_RUN, AUTO_LADDER, AutoInterval, evaluate_alerts, target_loss};
    use crate::config::Target;
    use crate::history::{History, Sample};
    use std::collections::{BTreeMap, HashSet};

    /// `drops` failures followed by `replies` successes, one second apart,
    /// starting at `now`.
    fn history(name: &str, drops: usize, replies: usize, now: i64) -> History {
        let mut samples = Vec::new();
        for i in 0..(drops + replies) {
            let mut values = BTreeMap::new();
            values.insert(name.to_string(), (i >= drops).then_some(10));
            samples.push(Sample {
                t: now + i as i64 * 1_000,
                v: values,
            });
        }
        History { samples }
    }

    #[test]
    fn calculates_loss_for_measured_samples() {
        let history = history("Router", 2, 8, 1_000);
        assert_eq!(target_loss(&history, "Router", 0), (10, 20));
        assert_eq!(target_loss(&history, "Other", 0), (0, 0));
    }

    /// A minute of health sampled every 10 s next to a minute of loss sampled
    /// every second is ~50% loss — by raw sample count it would read as 90%.
    #[test]
    fn weights_loss_by_elapsed_time_not_sample_count() {
        let mut samples = Vec::new();
        let mut push = |t: i64, ok: bool| {
            let mut values = BTreeMap::new();
            values.insert("Router".to_string(), ok.then_some(10));
            samples.push(Sample { t, v: values });
        };
        for i in 0..=6 {
            push(i * 10_000, true);
        }
        for i in 61..=120 {
            push(i * 1_000, false);
        }
        let history = History { samples };
        let (measured, loss) = target_loss(&history, "Router", 0);
        assert_eq!(measured, 67);
        // 60 s dropped against 60 s healthy plus the leading sample's default.
        assert_eq!(loss, 49);
    }

    #[test]
    fn a_slow_healthy_sample_outweighs_a_fast_dropped_one() {
        let mut samples = Vec::new();
        let mut push = |t: i64, ok: bool| {
            let mut values = BTreeMap::new();
            values.insert("Router".to_string(), ok.then_some(10));
            samples.push(Sample { t, v: values });
        };
        push(0, true);
        push(1_000, false); // 1 s of loss
        push(11_000, true); // 10 s of health
        let history = History { samples };
        let (measured, loss) = target_loss(&history, "Router", 0);
        assert_eq!(measured, 3);
        // 1 s dropped out of 1 s + 10 s + the leading sample's 1 s default.
        assert_eq!(loss, 8);
    }

    #[test]
    fn auto_snaps_to_the_fastest_rung_on_any_loss() {
        let mut auto = AutoInterval::default();
        assert_eq!(auto.interval_ms(), AUTO_LADDER[AUTO_LADDER.len() - 1]);
        auto.on_loss();
        assert_eq!(auto.interval_ms(), AUTO_LADDER[0]);
        assert_eq!(auto.clean_needed(), Some(AUTO_BACKOFF_RUN));
    }

    #[test]
    fn auto_backs_off_one_rung_per_clean_run() {
        let mut auto = AutoInterval::default();
        auto.on_loss();
        for rung in 1..AUTO_LADDER.len() {
            for _ in 0..AUTO_BACKOFF_RUN - 1 {
                auto.on_clean_round();
                assert_eq!(auto.interval_ms(), AUTO_LADDER[rung - 1]);
            }
            auto.on_clean_round();
            assert_eq!(auto.interval_ms(), AUTO_LADDER[rung]);
        }
        // Already slowest: stays there and stops advertising a countdown.
        auto.on_clean_round();
        assert_eq!(auto.interval_ms(), AUTO_LADDER[AUTO_LADDER.len() - 1]);
        assert_eq!(auto.clean_needed(), None);
    }

    #[test]
    fn auto_restarts_the_clean_run_after_a_relapse() {
        let mut auto = AutoInterval::default();
        auto.on_loss();
        for _ in 0..AUTO_BACKOFF_RUN - 1 {
            auto.on_clean_round();
        }
        auto.on_loss();
        assert_eq!(auto.interval_ms(), AUTO_LADDER[0]);
        assert_eq!(auto.clean_needed(), Some(AUTO_BACKOFF_RUN));
    }

    #[test]
    fn waits_for_ten_samples_and_uses_strict_threshold() {
        let target = Target::new("Router", "192.168.1.1", None);
        let mut alerted = HashSet::new();
        assert!(
            evaluate_alerts(
                &history("Router", 9, 0, 1_000),
                std::slice::from_ref(&target),
                10,
                15,
                &mut alerted,
                1_000,
            )
            .is_empty()
        );
        assert!(
            evaluate_alerts(
                &history("Router", 3, 17, 1_000),
                std::slice::from_ref(&target),
                10,
                15,
                &mut alerted,
                1_000,
            )
            .is_empty()
        );
    }

    #[test]
    fn alerts_once_then_rearms_after_recovery() {
        let target = Target::new("Router", "192.168.1.1", None);
        let mut alerted = HashSet::new();
        let unhealthy = history("Router", 2, 8, 1_000);
        assert_eq!(
            evaluate_alerts(
                &unhealthy,
                std::slice::from_ref(&target),
                10,
                15,
                &mut alerted,
                1_000,
            ),
            vec![("Router".to_string(), 20)]
        );
        assert!(
            evaluate_alerts(
                &unhealthy,
                std::slice::from_ref(&target),
                10,
                15,
                &mut alerted,
                1_000,
            )
            .is_empty()
        );

        let healthy = history("Router", 0, 10, 1_000);
        assert!(
            evaluate_alerts(
                &healthy,
                std::slice::from_ref(&target),
                10,
                15,
                &mut alerted,
                1_000,
            )
            .is_empty()
        );
        assert_eq!(
            evaluate_alerts(
                &unhealthy,
                std::slice::from_ref(&target),
                10,
                15,
                &mut alerted,
                1_000,
            ),
            vec![("Router".to_string(), 20)]
        );
    }

    #[test]
    fn combines_crossings_and_forgets_removed_targets() {
        let targets = vec![
            Target::new("Router", "192.168.1.1", None),
            Target::new("Internet", "8.8.8.8", None),
        ];
        let mut combined = History::default();
        for i in 0..10 {
            let mut values = BTreeMap::new();
            values.insert("Router".to_string(), (i >= 2).then_some(10));
            values.insert("Internet".to_string(), (i >= 3).then_some(10));
            combined.samples.push(Sample {
                t: 1_000 - (9 - i) * 1_000,
                v: values,
            });
        }
        let mut alerted = HashSet::new();
        assert_eq!(
            evaluate_alerts(&combined, &targets, 10, 15, &mut alerted, 1_000),
            vec![("Router".to_string(), 20), ("Internet".to_string(), 30),]
        );

        evaluate_alerts(&combined, &targets[1..], 10, 15, &mut alerted, 1_000);
        assert!(!alerted.contains("Router"));
    }
}
