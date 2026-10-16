//! One scrape of an inference server, and the values derived from consecutive scrapes.

use serde::{Deserialize, Serialize};

/// A single scrape of the server's metrics.
///
/// `running`, `waiting` and `generation_tokens_total` are required because every
/// built-in rule needs them. `kv_usage` and `preemptions_total` are optional:
/// servers and versions differ, and a rule that needs a missing signal reports
/// `Unknown` instead of pretending the value was zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Seconds on any monotonic clock. Only differences matter.
    pub t: f64,
    /// Requests currently being executed.
    pub running: f64,
    /// Requests queued and not yet admitted.
    pub waiting: f64,
    /// Cumulative generated tokens (a counter).
    pub generation_tokens_total: f64,
    /// Fraction of KV cache in use, 0.0 to 1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kv_usage: Option<f64>,
    /// Cumulative preemptions (a counter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preemptions_total: Option<f64>,
}

impl Sample {
    pub(crate) fn is_finite(&self) -> bool {
        self.t.is_finite()
            && self.running.is_finite()
            && self.waiting.is_finite()
            && self.generation_tokens_total.is_finite()
            && self.kv_usage.map_or(true, f64::is_finite)
            && self.preemptions_total.map_or(true, f64::is_finite)
    }
}

/// A sample plus the per-second rates computed against the previous sample.
///
/// The first observation in a trace has no rates (`None`), so any rule that
/// depends on a rate is `Unknown` there.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub sample: Sample,
    pub token_rate: Option<f64>,
    pub preemption_rate: Option<f64>,
}

/// Increase of a counter between two scrapes, following the Prometheus rule
/// for resets: if the counter went down, the server restarted and the new
/// value is everything counted since the restart.
pub(crate) fn counter_increase(prev: f64, cur: f64) -> f64 {
    if cur >= prev {
        cur - prev
    } else {
        cur
    }
}
