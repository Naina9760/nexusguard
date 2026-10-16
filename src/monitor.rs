//! Feeds samples to the rules and reports when a rule starts or stops firing.

use std::collections::VecDeque;

use serde::Serialize;

use crate::formula::{Signal, Verdict};
use crate::rules::{Rule, Severity};
use crate::sample::Sample;
use crate::trace::{Trace, TraceError};

/// Something worth telling an operator about.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A rule's pattern became true. Emitted once, not on every sample while it stays true.
    Violation {
        rule: String,
        severity: Severity,
        t: f64,
        description: String,
        pattern: String,
    },
    /// A rule that was firing is now definitely no longer true.
    Recovered { rule: String, t: f64 },
    /// The metrics endpoint could not be read several times in a row.
    ScrapeFailing {
        t: f64,
        consecutive: u32,
        error: String,
    },
    /// The metrics endpoint is readable again.
    ScrapeRecovered { t: f64 },
}

/// Everything known about one sample after the rules ran on it. The timeline
/// renderer and the web UI are drawn from these.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Frame {
    pub t: f64,
    pub running: f64,
    pub waiting: f64,
    pub kv_usage: Option<f64>,
    pub token_rate: Option<f64>,
    pub prompt_rate: Option<f64>,
    pub preemption_rate: Option<f64>,
    /// One verdict per rule, in rule order.
    pub verdicts: Vec<Verdict>,
    /// Whether each rule was firing after this sample.
    pub firing: Vec<bool>,
}

impl Frame {
    /// Whether this frame carries a value for `signal`. The first frame has no
    /// rates yet, and some servers do not export every metric.
    pub fn has(&self, signal: Signal) -> bool {
        match signal {
            Signal::Running | Signal::Waiting => true,
            Signal::KvUsage => self.kv_usage.is_some(),
            Signal::TokenRate | Signal::ProgressRate => self.token_rate.is_some(),
            Signal::PreemptionRate => self.preemption_rate.is_some(),
        }
    }
}

/// How many frames the monitor keeps for display by default.
pub const DEFAULT_HISTORY: usize = 900;

pub struct Monitor {
    trace: Trace,
    rules: Vec<Rule>,
    firing: Vec<bool>,
    frames: VecDeque<Frame>,
    history: usize,
    max_gap: f64,
}

impl Monitor {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self::with_history(rules, DEFAULT_HISTORY)
    }

    pub fn with_history(rules: Vec<Rule>, history: usize) -> Self {
        let retention = rules
            .iter()
            .map(|r| r.pattern.lookback())
            .fold(1.0_f64, f64::max);
        let firing = vec![false; rules.len()];
        Self {
            trace: Trace::new(retention),
            rules,
            firing,
            frames: VecDeque::new(),
            history: history.max(1),
            max_gap: f64::INFINITY,
        }
    }

    /// Observations further apart than this many seconds do not count as
    /// continuous coverage (see `formula`). Live monitoring sets it from the
    /// scrape interval, so an outage inside a rule's window cannot be mistaken
    /// for evidence. The default is no limit.
    pub fn with_max_gap(mut self, secs: f64) -> Self {
        self.max_gap = if secs.is_finite() && secs > 0.0 {
            secs
        } else {
            f64::INFINITY
        };
        self
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Whether each rule is firing right now, in rule order.
    pub fn firing(&self) -> &[bool] {
        &self.firing
    }

    /// The most recent frames, oldest first.
    pub fn frames(&self) -> &VecDeque<Frame> {
        &self.frames
    }

    /// Adds a sample and returns the rule transitions it caused.
    ///
    /// A rule starts firing when its pattern evaluates to `True` and stops when
    /// it evaluates to `False`. `Unknown` leaves the state unchanged, so a
    /// missing metric or a short history never raises or clears an alert.
    pub fn ingest(&mut self, sample: Sample) -> Result<Vec<Event>, TraceError> {
        let t = sample.t;
        self.trace.push(sample)?;

        let observations = self.trace.observations();
        let verdicts: Vec<Verdict> = self
            .rules
            .iter()
            .map(|r| r.pattern.eval_with_gap(observations, self.max_gap))
            .collect();
        let latest = observations.back().expect("a sample was just pushed");

        let mut events = Vec::new();
        for ((rule, firing), verdict) in
            self.rules.iter().zip(self.firing.iter_mut()).zip(&verdicts)
        {
            match verdict {
                Verdict::True if !*firing => {
                    *firing = true;
                    events.push(Event::Violation {
                        rule: rule.name.clone(),
                        severity: rule.severity,
                        t,
                        description: rule.description.clone(),
                        pattern: rule.pattern.to_string(),
                    });
                }
                Verdict::False if *firing => {
                    *firing = false;
                    events.push(Event::Recovered {
                        rule: rule.name.clone(),
                        t,
                    });
                }
                _ => {}
            }
        }

        if self.frames.len() >= self.history {
            self.frames.pop_front();
        }
        self.frames.push_back(Frame {
            t,
            running: latest.sample.running,
            waiting: latest.sample.waiting,
            kv_usage: latest.sample.kv_usage,
            token_rate: latest.token_rate,
            prompt_rate: latest.prompt_rate,
            preemption_rate: latest.preemption_rate,
            verdicts,
            firing: self.firing.clone(),
        });
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{builtin_rules, RuleConfig};
    use crate::sim::{generate, Scenario};

    #[test]
    fn every_sample_produces_a_frame_with_one_verdict_per_rule() {
        let rules = builtin_rules(&RuleConfig::default());
        let n = rules.len();
        let mut m = Monitor::new(rules);
        for s in generate(Scenario::Stall, 40, 2.0) {
            m.ingest(s).unwrap();
        }
        assert_eq!(m.frames().len(), 40);
        assert!(m
            .frames()
            .iter()
            .all(|f| f.verdicts.len() == n && f.firing.len() == n));
    }

    #[test]
    fn history_is_capped() {
        let mut m = Monitor::with_history(builtin_rules(&RuleConfig::default()), 10);
        for s in generate(Scenario::Healthy, 50, 2.0) {
            m.ingest(s).unwrap();
        }
        assert_eq!(m.frames().len(), 10);
        assert_eq!(m.frames().back().unwrap().t, 98.0);
    }

    #[test]
    fn a_rejected_sample_leaves_no_frame() {
        let mut m = Monitor::new(builtin_rules(&RuleConfig::default()));
        let s = generate(Scenario::Healthy, 2, 2.0);
        m.ingest(s[1].clone()).unwrap();
        assert!(m.ingest(s[0].clone()).is_err());
        assert_eq!(m.frames().len(), 1);
    }
}
