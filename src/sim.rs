//! Deterministic synthetic traces, so the rules can be demonstrated and tested
//! without a GPU or a running server. These are not recordings of a real
//! server. They are shaped after the symptoms the rules look for, and the two
//! `hang-*` scenarios follow the numbers people pasted into vLLM issue #18431.

use crate::sample::Sample;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Steady traffic, short queue, moderate KV usage.
    Healthy,
    /// Healthy, then all progress stops while requests stay queued.
    Stall,
    /// Healthy, then KV cache sits near full and preemptions climb.
    Thrash,
    /// Monitoring starts while the server is already stalled, so the rule has
    /// to watch for its whole window before it can be sure.
    AttachedStalled,
    /// Nothing running, nothing queued, no tokens. An idle server is not a stall.
    Idle,
    /// One request reading a very long prompt: no output tokens for minutes, but
    /// prompt tokens keep being processed. Slow is not stuck.
    LongPrefill,
    /// Healthy, then one request running, empty queue, no progress, low KV
    /// usage (the "Running: 1 reqs, Waiting: 0 reqs, 9.6%" report).
    HangRunningOnly,
    /// Healthy, then a full batch and a queue, KV cache almost full, no progress
    /// (the "Running: 64 reqs, Waiting: 34 reqs, 96.8%" report).
    HangSaturated,
}

impl Scenario {
    pub const ALL: [(&'static str, Scenario); 8] = [
        ("healthy", Scenario::Healthy),
        ("stall", Scenario::Stall),
        ("thrash", Scenario::Thrash),
        ("attached-stalled", Scenario::AttachedStalled),
        ("idle", Scenario::Idle),
        ("long-prefill", Scenario::LongPrefill),
        ("hang-running-only", Scenario::HangRunningOnly),
        ("hang-saturated", Scenario::HangSaturated),
    ];

    pub fn parse(name: &str) -> Option<Scenario> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Sample at which the fault begins, in the scenarios that have one.
pub const FAULT_STARTS_AT_SAMPLE: usize = 20;

/// What the server is doing during one sample interval.
struct Shape {
    running: f64,
    waiting: f64,
    kv: f64,
    generated_per_s: f64,
    prompt_per_s: f64,
    preemptions: f64,
}

fn shape(scenario: Scenario, i: usize) -> Shape {
    let healthy = Shape {
        running: 8.0,
        waiting: (i % 4) as f64,
        kv: 0.55 + 0.10 * ((i % 10) as f64 / 10.0),
        generated_per_s: 800.0,
        prompt_per_s: 300.0,
        preemptions: 0.0,
    };
    let faulty = i >= FAULT_STARTS_AT_SAMPLE;

    match scenario {
        Scenario::Healthy => healthy,
        Scenario::Stall if faulty => Shape {
            waiting: 5.0,
            generated_per_s: 0.0,
            prompt_per_s: 0.0,
            ..healthy
        },
        Scenario::Thrash if faulty => Shape {
            waiting: 6.0,
            kv: 0.96,
            generated_per_s: 200.0,
            prompt_per_s: 100.0,
            preemptions: 1.0,
            ..healthy
        },
        Scenario::AttachedStalled => Shape {
            waiting: 5.0,
            generated_per_s: 0.0,
            prompt_per_s: 0.0,
            ..healthy
        },
        Scenario::Idle => Shape {
            running: 0.0,
            waiting: 0.0,
            kv: 0.02,
            generated_per_s: 0.0,
            prompt_per_s: 0.0,
            preemptions: 0.0,
        },
        Scenario::LongPrefill if i >= 5 => Shape {
            running: 1.0,
            waiting: 0.0,
            kv: 0.30 + 0.004 * i as f64,
            generated_per_s: 0.0,
            prompt_per_s: 2000.0,
            preemptions: 0.0,
        },
        Scenario::HangRunningOnly if faulty => Shape {
            running: 1.0,
            waiting: 0.0,
            kv: 0.096,
            generated_per_s: 0.0,
            prompt_per_s: 0.0,
            preemptions: 0.0,
        },
        Scenario::HangSaturated if faulty => Shape {
            running: 64.0,
            waiting: 34.0,
            kv: 0.968,
            generated_per_s: 0.0,
            prompt_per_s: 0.0,
            preemptions: 0.0,
        },
        _ => healthy,
    }
}

/// Generates `count` samples spaced `interval` seconds apart.
pub fn generate(scenario: Scenario, count: usize, interval: f64) -> Vec<Sample> {
    let mut out = Vec::with_capacity(count);
    let (mut generated, mut prompt, mut preemptions) = (0.0_f64, 0.0_f64, 0.0_f64);

    for i in 0..count {
        let s = shape(scenario, i);
        if i > 0 {
            generated += s.generated_per_s * interval;
            prompt += s.prompt_per_s * interval;
            preemptions += s.preemptions;
        }
        out.push(Sample {
            t: i as f64 * interval,
            running: s.running,
            waiting: s.waiting,
            generation_tokens_total: generated,
            prompt_tokens_total: Some(prompt),
            kv_usage: Some(s.kv),
            preemptions_total: Some(preemptions),
        });
    }
    out
}
