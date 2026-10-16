//! A bounded, time-ordered history of observations.

use std::collections::VecDeque;
use std::fmt;

use crate::sample::{counter_increase, Observation, Sample};

#[derive(Debug, Clone, PartialEq)]
pub enum TraceError {
    /// A field was NaN or infinite.
    NonFinite,
    /// The sample's timestamp is not later than the previous one.
    OutOfOrder { last: f64, got: f64 },
}

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceError::NonFinite => write!(f, "sample contains a NaN or infinite value"),
            TraceError::OutOfOrder { last, got } => write!(
                f,
                "sample timestamp {got} is not later than the previous timestamp {last}"
            ),
        }
    }
}

impl std::error::Error for TraceError {}

/// Keeps about the last `retention` seconds. One observation at or before the
/// cut-off stays, so a window of exactly that length still counts as covered.
#[derive(Debug, Clone)]
pub struct Trace {
    observations: VecDeque<Observation>,
    retention: f64,
}

impl Trace {
    pub fn new(retention_secs: f64) -> Self {
        let retention = if retention_secs.is_finite() && retention_secs > 0.0 {
            retention_secs
        } else {
            0.0
        };
        Self {
            observations: VecDeque::new(),
            retention,
        }
    }

    pub fn push(&mut self, sample: Sample) -> Result<(), TraceError> {
        if !sample.is_finite() {
            return Err(TraceError::NonFinite);
        }

        let (token_rate, prompt_rate, preemption_rate) = match self.observations.back() {
            None => (None, None, None),
            Some(prev) => {
                let last = prev.sample.t;
                if sample.t <= last {
                    return Err(TraceError::OutOfOrder {
                        last,
                        got: sample.t,
                    });
                }
                let dt = sample.t - last;
                let tokens = Some(
                    counter_increase(
                        prev.sample.generation_tokens_total,
                        sample.generation_tokens_total,
                    ) / dt,
                );
                let prompt = match (prev.sample.prompt_tokens_total, sample.prompt_tokens_total) {
                    (Some(a), Some(b)) => Some(counter_increase(a, b) / dt),
                    _ => None,
                };
                let preempt = match (prev.sample.preemptions_total, sample.preemptions_total) {
                    (Some(a), Some(b)) => Some(counter_increase(a, b) / dt),
                    _ => None,
                };
                (tokens, prompt, preempt)
            }
        };

        self.observations.push_back(Observation {
            sample,
            token_rate,
            prompt_rate,
            preemption_rate,
        });
        self.prune();
        Ok(())
    }

    fn prune(&mut self) {
        let newest = match self.observations.back() {
            Some(o) => o.sample.t,
            None => return,
        };
        let cutoff = newest - self.retention;
        while self.observations.len() >= 2 && self.observations[1].sample.t <= cutoff {
            self.observations.pop_front();
        }
    }

    pub fn observations(&self) -> &VecDeque<Observation> {
        &self.observations
    }

    pub fn len(&self) -> usize {
        self.observations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, tokens: f64) -> Sample {
        Sample {
            t,
            running: 1.0,
            waiting: 0.0,
            generation_tokens_total: tokens,
            prompt_tokens_total: None,
            kv_usage: None,
            preemptions_total: None,
        }
    }

    #[test]
    fn first_observation_has_no_rates() {
        let mut tr = Trace::new(60.0);
        tr.push(s(0.0, 100.0)).unwrap();
        assert_eq!(tr.observations()[0].token_rate, None);
    }

    #[test]
    fn rate_is_increase_over_elapsed_time() {
        let mut tr = Trace::new(60.0);
        tr.push(s(0.0, 100.0)).unwrap();
        tr.push(s(2.0, 300.0)).unwrap();
        assert_eq!(tr.observations()[1].token_rate, Some(100.0));
    }

    #[test]
    fn counter_reset_counts_value_since_restart() {
        let mut tr = Trace::new(60.0);
        tr.push(s(0.0, 1000.0)).unwrap();
        tr.push(s(2.0, 40.0)).unwrap();
        assert_eq!(tr.observations()[1].token_rate, Some(20.0));
    }

    #[test]
    fn rejects_out_of_order_and_equal_timestamps() {
        let mut tr = Trace::new(60.0);
        tr.push(s(5.0, 0.0)).unwrap();
        assert!(matches!(
            tr.push(s(5.0, 0.0)),
            Err(TraceError::OutOfOrder { .. })
        ));
        assert!(matches!(
            tr.push(s(4.0, 0.0)),
            Err(TraceError::OutOfOrder { .. })
        ));
        assert_eq!(tr.len(), 1);
    }

    #[test]
    fn rejects_non_finite_values() {
        let mut tr = Trace::new(60.0);
        let mut bad = s(0.0, 0.0);
        bad.waiting = f64::NAN;
        assert_eq!(tr.push(bad), Err(TraceError::NonFinite));
        assert!(tr.is_empty());
    }

    #[test]
    fn missing_preemption_counter_gives_no_preemption_rate() {
        let mut tr = Trace::new(60.0);
        let mut a = s(0.0, 0.0);
        a.preemptions_total = Some(1.0);
        tr.push(a).unwrap();
        tr.push(s(2.0, 0.0)).unwrap();
        assert_eq!(tr.observations()[1].preemption_rate, None);
    }

    #[test]
    fn prune_keeps_one_observation_at_or_before_the_cutoff() {
        let mut tr = Trace::new(10.0);
        for i in 0..20 {
            tr.push(s(i as f64, 0.0)).unwrap();
        }
        let first = tr.observations().front().unwrap().sample.t;
        let last = tr.observations().back().unwrap().sample.t;
        assert!(first <= last - 10.0, "window start must stay covered");
        assert!(
            tr.len() <= 13,
            "old observations must be dropped, got {}",
            tr.len()
        );
    }
}
