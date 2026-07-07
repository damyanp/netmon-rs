//! Rolling latency history, persisted to `history.json` in a format compatible
//! with the original Node `server.js` (`{ "samples": [ { "t", "v": {name: ms|null} } ] }`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One measurement round. `v` maps target name -> latency in ms, or `None` for a
/// dropped packet.
#[derive(Clone, Serialize, Deserialize)]
pub struct Sample {
    pub t: i64,
    pub v: BTreeMap<String, Option<u32>>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct History {
    pub samples: Vec<Sample>,
}

impl History {
    pub fn save(&self, path: &Path) {
        if let Ok(json) = serde_json::to_string(self) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn push(&mut self, sample: Sample) {
        self.samples.push(sample);
    }

    /// Drop samples older than `max_age_ms` and cap the total count.
    pub fn prune(&mut self, now_ms: i64, max_age_ms: i64, max_samples: usize) {
        let cutoff = now_ms - max_age_ms;
        let first_keep = self.samples.iter().position(|s| s.t >= cutoff).unwrap_or(self.samples.len());
        if first_keep > 0 {
            self.samples.drain(..first_keep);
        }
        if self.samples.len() > max_samples {
            let excess = self.samples.len() - max_samples;
            self.samples.drain(..excess);
        }
    }
}
