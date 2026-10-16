//! Built-in rules. A rule describes a *bad pattern*: it fires when its pattern holds.

use serde::Serialize;

use crate::formula::{Cmp, Formula, Signal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Critical,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,
    pub severity: Severity,
    pub description: String,
    /// The rule fires when this evaluates to `True`.
    pub pattern: Formula,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuleConfig {
    /// Seconds with a non-empty queue and zero token output before `engine_stalled` fires.
    pub stall_secs: f64,
    /// Seconds of high KV usage (with at least one preemption) before `kv_thrash` fires.
    pub thrash_secs: f64,
    /// KV usage fraction counted as "high".
    pub kv_threshold: f64,
    /// Seconds the queue must stay at or above `backlog_min` before `queue_backlog` fires.
    pub backlog_secs: f64,
    pub backlog_min: f64,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            stall_secs: 30.0,
            thrash_secs: 20.0,
            kv_threshold: 0.9,
            backlog_secs: 60.0,
            backlog_min: 1.0,
        }
    }
}

impl RuleConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [
            ("stall_secs", self.stall_secs),
            ("thrash_secs", self.thrash_secs),
            ("backlog_secs", self.backlog_secs),
        ] {
            if !(v.is_finite() && v > 0.0) {
                return Err(format!("{name} must be a positive number, got {v}"));
            }
        }
        if !(self.kv_threshold.is_finite() && self.kv_threshold > 0.0 && self.kv_threshold <= 1.0) {
            return Err(format!(
                "kv_threshold must be in (0, 1], got {}",
                self.kv_threshold
            ));
        }
        if !(self.backlog_min.is_finite() && self.backlog_min >= 1.0) {
            return Err(format!(
                "backlog_min must be at least 1, got {}",
                self.backlog_min
            ));
        }
        Ok(())
    }
}

/// The three built-in rules. Call [`RuleConfig::validate`] first.
pub fn builtin_rules(cfg: &RuleConfig) -> Vec<Rule> {
    let waiting = Formula::pred(Signal::Waiting, Cmp::Gt, 0.0);
    let no_tokens = Formula::pred(Signal::TokenRate, Cmp::Eq, 0.0);

    let stalled = waiting.and(no_tokens).historically(cfg.stall_secs);

    let kv_high =
        Formula::pred(Signal::KvUsage, Cmp::Ge, cfg.kv_threshold).historically(cfg.thrash_secs);
    let preempting = Formula::pred(Signal::PreemptionRate, Cmp::Gt, 0.0).once(cfg.thrash_secs);
    let thrash = kv_high.and(preempting);

    let backlog =
        Formula::pred(Signal::Waiting, Cmp::Ge, cfg.backlog_min).historically(cfg.backlog_secs);

    vec![
        Rule {
            name: "engine_stalled".into(),
            severity: Severity::Critical,
            description: format!(
                "Requests are queued but no tokens were generated for {}s. The engine may be hung or deadlocked.",
                cfg.stall_secs
            ),
            pattern: stalled,
        },
        Rule {
            name: "kv_thrash".into(),
            severity: Severity::Warning,
            description: format!(
                "KV cache usage stayed at or above {:.0}% for {}s while the server was preempting requests.",
                cfg.kv_threshold * 100.0,
                cfg.thrash_secs
            ),
            pattern: thrash,
        },
        Rule {
            name: "queue_backlog".into(),
            severity: Severity::Warning,
            description: format!(
                "At least {} request(s) waited in the queue continuously for {}s. The server may be overloaded.",
                cfg.backlog_min, cfg.backlog_secs
            ),
            pattern: backlog,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_produce_three_rules() {
        let cfg = RuleConfig::default();
        assert!(cfg.validate().is_ok());
        let names: Vec<_> = builtin_rules(&cfg).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["engine_stalled", "kv_thrash", "queue_backlog"]);
    }

    #[test]
    fn rejects_nonsense_thresholds() {
        let bad = |f: fn(&mut RuleConfig)| {
            let mut c = RuleConfig::default();
            f(&mut c);
            c.validate().is_err()
        };
        assert!(bad(|c| c.stall_secs = 0.0));
        assert!(bad(|c| c.stall_secs = f64::NAN));
        assert!(bad(|c| c.thrash_secs = -1.0));
        assert!(bad(|c| c.kv_threshold = 0.0));
        assert!(bad(|c| c.kv_threshold = 1.5));
        assert!(bad(|c| c.backlog_min = 0.0));
    }
}
