//! Deterministic synthetic traces, so the rules can be demonstrated and tested
//! without a GPU or a running server. These are *not* recordings of a real
//! server; they are shaped after the symptoms the rules look for.

use crate::sample::Sample;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Steady traffic, short queue, moderate KV usage.
    Healthy,
    /// Healthy, then output stops while requests stay queued.
    Stall,
    /// Healthy, then KV cache sits near full and preemptions climb.
    Thrash,
    /// Monitoring starts while the server is already stalled, so the rule has
    /// to watch for its whole window before it can be sure.
    AttachedStalled,
}

impl Scenario {
    pub const ALL: [(&'static str, Scenario); 4] = [
        ("healthy", Scenario::Healthy),
        ("stall", Scenario::Stall),
        ("thrash", Scenario::Thrash),
        ("attached-stalled", Scenario::AttachedStalled),
    ];

    pub fn parse(name: &str) -> Option<Scenario> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }
}

/// Samples at which the fault begins, in the faulty scenarios.
pub const FAULT_STARTS_AT_SAMPLE: usize = 20;

/// Generates `count` samples spaced `interval` seconds apart.
pub fn generate(scenario: Scenario, count: usize, interval: f64) -> Vec<Sample> {
    let mut out = Vec::with_capacity(count);
    let mut tokens = 0.0_f64;
    let mut preemptions = 0.0_f64;

    for i in 0..count {
        let faulty = i >= FAULT_STARTS_AT_SAMPLE;
        let running = 8.0;
        let mut waiting = (i % 4) as f64;
        let mut kv = 0.55 + 0.10 * ((i % 10) as f64 / 10.0);
        let mut token_rate = 800.0;

        match scenario {
            Scenario::Healthy => {}
            Scenario::Stall if faulty => {
                waiting = 5.0;
                token_rate = 0.0;
            }
            Scenario::Thrash if faulty => {
                waiting = 6.0;
                kv = 0.96;
                token_rate = 200.0;
                preemptions += 1.0;
            }
            Scenario::AttachedStalled => {
                waiting = 5.0;
                token_rate = 0.0;
            }
            _ => {}
        }
        if i > 0 {
            tokens += token_rate * interval;
        }
        out.push(Sample {
            t: i as f64 * interval,
            running,
            waiting,
            generation_tokens_total: tokens,
            kv_usage: Some(kv),
            preemptions_total: Some(preemptions),
        });
    }
    out
}
