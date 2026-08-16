//! Throughput sampling for the bandwidth area drawn behind the latency lines.
//!
//! Reads the byte counters of whichever interface currently carries the default
//! route (`GetBestInterface` + `GetIfEntry2`) on a fixed 1 s tick, and turns
//! consecutive readings into a bits-per-second rate. It runs on its own thread
//! rather than inside the ping loop, whose interval varies from 50 ms to 10 s.

use std::thread;
use std::time::{Duration, Instant};

use windows::iphlpapi::GetBestInterface;
use windows::netioapi::{GetIfEntry2, MIB_IF_ROW2};
use windows::ntddndis::IPAddr;
use windows::winerror::NO_ERROR;

use crate::monitor::{Shared, now_ms};

/// How often the interface counters are read. Also the resolution of the area
/// chart, so keep it well under the shortest display window (1 minute).
const SAMPLE_MS: u64 = 1_000;

/// One throughput reading, in bits per second in each direction.
#[derive(Clone, Copy)]
pub struct BandwidthSample {
    pub t: i64,
    pub rx_bps: f64,
    pub tx_bps: f64,
}

#[derive(Default)]
pub struct BandwidthHistory {
    pub samples: Vec<BandwidthSample>,
}

impl BandwidthHistory {
    pub fn push(&mut self, sample: BandwidthSample) {
        self.samples.push(sample);
    }

    /// The most recent reading, if any.
    pub fn latest(&self) -> Option<BandwidthSample> {
        self.samples.last().copied()
    }

    /// Drop samples older than `max_age_ms` and cap the total count.
    pub fn prune(&mut self, now_ms: i64, max_age_ms: i64, max_samples: usize) {
        let cutoff = now_ms - max_age_ms;
        let first_keep = self
            .samples
            .iter()
            .position(|s| s.t >= cutoff)
            .unwrap_or(self.samples.len());
        if first_keep > 0 {
            self.samples.drain(..first_keep);
        }
        if self.samples.len() > max_samples {
            let excess = self.samples.len() - max_samples;
            self.samples.drain(..excess);
        }
    }
}

/// Raw counters for one interface at one instant.
struct Counters {
    index: u32,
    in_octets: u64,
    out_octets: u64,
    at: Instant,
}

/// Spawn the sampler. It runs for the lifetime of the process.
pub fn spawn(shared: Shared, max_age_ms: i64, max_samples: usize) {
    thread::spawn(move || worker(shared, max_age_ms, max_samples));
}

fn worker(shared: Shared, max_age_ms: i64, max_samples: usize) {
    let mut prev: Option<Counters> = None;
    loop {
        let started = Instant::now();
        let current = read_counters();

        if let (Some(p), Some(c)) = (prev.as_ref(), current.as_ref()) {
            let dt = c.at.duration_since(p.at).as_secs_f64();
            // A different interface means the default route moved, so the two
            // readings aren't comparable. `saturating_sub` covers the counter
            // resetting (adapter reconnect) the same way: no traffic, not a
            // wildly negative rate.
            if p.index == c.index && dt > 0.1 {
                let rate = |now: u64, before: u64| now.saturating_sub(before) as f64 * 8.0 / dt;
                let sample = BandwidthSample {
                    t: now_ms(),
                    rx_bps: rate(c.in_octets, p.in_octets),
                    tx_bps: rate(c.out_octets, p.out_octets),
                };
                let mut st = shared.lock().unwrap();
                st.bandwidth.push(sample);
                st.bandwidth.prune(sample.t, max_age_ms, max_samples);
                st.revision = st.revision.wrapping_add(1);
            }
        }

        prev = current;
        thread::sleep(Duration::from_millis(SAMPLE_MS).saturating_sub(started.elapsed()));
    }
}

/// Counters for the interface carrying the default route, or `None` if it can't
/// be determined right now (e.g. the link is down).
fn read_counters() -> Option<Counters> {
    // Any routable address resolves to the default-route interface; 8.8.8.8 is
    // only used as a lookup key, nothing is sent to it.
    let dest = IPAddr(u32::from_ne_bytes([8, 8, 8, 8]));
    let mut index = 0u32;
    if unsafe { GetBestInterface(dest, &mut index) } != NO_ERROR {
        return None;
    }

    let mut row = MIB_IF_ROW2 {
        InterfaceIndex: windows::ifdef::NET_IFINDEX(index),
        ..Default::default()
    };
    if unsafe { GetIfEntry2(&mut row) }.0 != 0 {
        return None;
    }

    Some(Counters {
        index,
        in_octets: row.InOctets,
        out_octets: row.OutOctets,
        at: Instant::now(),
    })
}

/// Human-readable bit rate, e.g. `12.3 Mb/s`.
pub fn format_bps(bps: f64) -> String {
    const UNITS: [(f64, &str); 3] = [(1e9, "Gb/s"), (1e6, "Mb/s"), (1e3, "kb/s")];
    for (scale, unit) in UNITS {
        if bps >= scale {
            let value = bps / scale;
            let decimals = if value < 10.0 { 1 } else { 0 };
            return format!("{value:.decimals$} {unit}");
        }
    }
    format!("{} b/s", bps.round() as i64)
}

#[cfg(test)]
mod tests {
    use super::{BandwidthHistory, BandwidthSample, format_bps};

    #[test]
    fn formats_rates_by_magnitude() {
        assert_eq!(format_bps(0.0), "0 b/s");
        assert_eq!(format_bps(950.0), "950 b/s");
        assert_eq!(format_bps(1_500.0), "1.5 kb/s");
        assert_eq!(format_bps(12_300_000.0), "12 Mb/s");
        assert_eq!(format_bps(2_500_000_000.0), "2.5 Gb/s");
    }

    #[test]
    fn prunes_by_age_and_count() {
        let mut history = BandwidthHistory::default();
        for t in 0..10 {
            history.push(BandwidthSample {
                t: t * 1_000,
                rx_bps: 0.0,
                tx_bps: 0.0,
            });
        }
        history.prune(9_000, 5_000, 100);
        assert_eq!(history.samples.first().map(|s| s.t), Some(4_000));
        history.prune(9_000, 5_000, 2);
        assert_eq!(history.samples.len(), 2);
        assert_eq!(history.latest().map(|s| s.t), Some(9_000));
    }
}
