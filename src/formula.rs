//! Past-time temporal logic over observations.
//!
//! Atoms compare a signal with a constant. `Historically[w]` and `Once[w]` look
//! back `w` seconds, so a verdict is ready as soon as a sample arrives.
//!
//! Verdicts are three-valued (Kleene). `Unknown` means data is missing: a signal
//! the server does not export, a rate on the first scrape, a window reaching
//! back before the first sample, or a window with a hole in it. `max_gap` is
//! the longest silence between samples that still counts as continuous. A
//! counterexample is definite even in a partial window.

use std::collections::VecDeque;
use std::fmt;

use serde::Serialize;

use crate::sample::Observation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    True,
    False,
    Unknown,
}

impl Verdict {
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Verdict {
        match self {
            Verdict::True => Verdict::False,
            Verdict::False => Verdict::True,
            Verdict::Unknown => Verdict::Unknown,
        }
    }

    pub fn and(self, other: Verdict) -> Verdict {
        match (self, other) {
            (Verdict::False, _) | (_, Verdict::False) => Verdict::False,
            (Verdict::True, Verdict::True) => Verdict::True,
            _ => Verdict::Unknown,
        }
    }

    pub fn or(self, other: Verdict) -> Verdict {
        match (self, other) {
            (Verdict::True, _) | (_, Verdict::True) => Verdict::True,
            (Verdict::False, Verdict::False) => Verdict::False,
            _ => Verdict::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Running,
    Waiting,
    KvUsage,
    /// Generated tokens per second since the previous scrape.
    TokenRate,
    /// Generated plus prompt tokens per second: any forward progress.
    ProgressRate,
    /// Preemptions per second since the previous scrape.
    PreemptionRate,
}

impl Signal {
    pub fn get(self, o: &Observation) -> Option<f64> {
        match self {
            Signal::Running => Some(o.sample.running),
            Signal::Waiting => Some(o.sample.waiting),
            Signal::KvUsage => o.sample.kv_usage,
            Signal::TokenRate => o.token_rate,
            Signal::ProgressRate => o.progress_rate(),
            Signal::PreemptionRate => o.preemption_rate,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Signal::Running => "running",
            Signal::Waiting => "waiting",
            Signal::KvUsage => "kv_usage",
            Signal::TokenRate => "token_rate",
            Signal::ProgressRate => "progress_rate",
            Signal::PreemptionRate => "preemption_rate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

impl Cmp {
    fn holds(self, a: f64, b: f64) -> bool {
        match self {
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
            Cmp::Gt => a > b,
            Cmp::Ge => a >= b,
            Cmp::Eq => a == b,
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
            Cmp::Eq => "==",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Formula {
    Pred(Signal, Cmp, f64),
    Not(Box<Formula>),
    And(Box<Formula>, Box<Formula>),
    Or(Box<Formula>, Box<Formula>),
    Historically(f64, Box<Formula>),
    Once(f64, Box<Formula>),
}

fn window(secs: f64) -> f64 {
    if secs.is_finite() && secs > 0.0 {
        secs
    } else {
        0.0
    }
}

/// Oldest observation in the window ending at `i`, and whether the window is
/// covered: data reaches `start` and no neighbours (counting the one just
/// before the window) are further apart than `max_gap`.
fn window_coverage(
    obs: &VecDeque<Observation>,
    i: usize,
    start: f64,
    max_gap: f64,
) -> (usize, bool) {
    let mut first = i;
    while first > 0 && obs[first - 1].sample.t >= start {
        first -= 1;
    }
    let reaches_back = obs[0].sample.t <= start;
    let from = first.saturating_sub(1);
    let dense = (from..i).all(|k| obs[k + 1].sample.t - obs[k].sample.t <= max_gap);
    (first, reaches_back && dense)
}

impl Formula {
    pub fn pred(signal: Signal, cmp: Cmp, value: f64) -> Formula {
        Formula::Pred(signal, cmp, value)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Formula {
        Formula::Not(Box::new(self))
    }

    pub fn and(self, other: Formula) -> Formula {
        Formula::And(Box::new(self), Box::new(other))
    }

    pub fn or(self, other: Formula) -> Formula {
        Formula::Or(Box::new(self), Box::new(other))
    }

    /// True when `self` held at every observation in the last `secs` seconds.
    pub fn historically(self, secs: f64) -> Formula {
        Formula::Historically(window(secs), Box::new(self))
    }

    /// True when `self` held at some observation in the last `secs` seconds.
    pub fn once(self, secs: f64) -> Formula {
        Formula::Once(window(secs), Box::new(self))
    }

    /// How far back in time the formula can look, in seconds (nested windows add up).
    pub fn lookback(&self) -> f64 {
        match self {
            Formula::Pred(..) => 0.0,
            Formula::Not(f) => f.lookback(),
            Formula::And(a, b) | Formula::Or(a, b) => a.lookback().max(b.lookback()),
            Formula::Historically(w, f) | Formula::Once(w, f) => w + f.lookback(),
        }
    }

    /// Every signal the formula reads, without duplicates, in first-use order.
    pub fn signals(&self) -> Vec<Signal> {
        fn walk(f: &Formula, out: &mut Vec<Signal>) {
            match f {
                Formula::Pred(s, ..) => {
                    if !out.contains(s) {
                        out.push(*s);
                    }
                }
                Formula::Not(f) | Formula::Historically(_, f) | Formula::Once(_, f) => walk(f, out),
                Formula::And(a, b) | Formula::Or(a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, &mut out);
        out
    }

    /// Evaluates at the latest observation with no limit on gaps between
    /// observations. An empty trace is `Unknown`.
    pub fn eval(&self, obs: &VecDeque<Observation>) -> Verdict {
        self.eval_with_gap(obs, f64::INFINITY)
    }

    /// Evaluates at the latest observation. Observations further apart than
    /// `max_gap` seconds do not count as continuous coverage.
    pub fn eval_with_gap(&self, obs: &VecDeque<Observation>, max_gap: f64) -> Verdict {
        match obs.len() {
            0 => Verdict::Unknown,
            n => self.eval_at(obs, n - 1, max_gap),
        }
    }

    /// Evaluates the formula at observation `i`, looking only at `0..=i`.
    pub fn eval_at(&self, obs: &VecDeque<Observation>, i: usize, max_gap: f64) -> Verdict {
        match self {
            Formula::Pred(signal, cmp, value) => match signal.get(&obs[i]) {
                Some(v) if cmp.holds(v, *value) => Verdict::True,
                Some(_) => Verdict::False,
                None => Verdict::Unknown,
            },
            Formula::Not(f) => f.eval_at(obs, i, max_gap).not(),
            Formula::And(a, b) => a.eval_at(obs, i, max_gap).and(b.eval_at(obs, i, max_gap)),
            Formula::Or(a, b) => a.eval_at(obs, i, max_gap).or(b.eval_at(obs, i, max_gap)),
            Formula::Historically(w, f) => {
                let start = obs[i].sample.t - w;
                let (first, covered) = window_coverage(obs, i, start, max_gap);
                let mut acc = Verdict::True;
                for j in (first..=i).rev() {
                    acc = acc.and(f.eval_at(obs, j, max_gap));
                    if acc == Verdict::False {
                        return Verdict::False;
                    }
                }
                if !covered && acc == Verdict::True {
                    Verdict::Unknown
                } else {
                    acc
                }
            }
            Formula::Once(w, f) => {
                let start = obs[i].sample.t - w;
                let (first, covered) = window_coverage(obs, i, start, max_gap);
                let mut acc = Verdict::False;
                for j in (first..=i).rev() {
                    acc = acc.or(f.eval_at(obs, j, max_gap));
                    if acc == Verdict::True {
                        return Verdict::True;
                    }
                }
                if !covered && acc == Verdict::False {
                    Verdict::Unknown
                } else {
                    acc
                }
            }
        }
    }
}

impl fmt::Display for Formula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Formula::Pred(s, c, v) => write!(f, "{} {} {}", s.name(), c.symbol(), v),
            Formula::Not(a) => write!(f, "not ({a})"),
            Formula::And(a, b) => write!(f, "({a}) and ({b})"),
            Formula::Or(a, b) => write!(f, "({a}) or ({b})"),
            Formula::Historically(w, a) => write!(f, "always[{w}s] ({a})"),
            Formula::Once(w, a) => write!(f, "once[{w}s] ({a})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::Sample;
    use crate::trace::Trace;

    fn trace_of(points: &[(f64, f64, f64)]) -> Trace {
        // (t, waiting, cumulative tokens)
        let mut tr = Trace::new(1000.0);
        for &(t, waiting, tokens) in points {
            tr.push(Sample {
                t,
                running: 1.0,
                waiting,
                generation_tokens_total: tokens,
                prompt_tokens_total: None,
                kv_usage: None,
                preemptions_total: None,
            })
            .unwrap();
        }
        tr
    }

    fn waiting_pos() -> Formula {
        Formula::pred(Signal::Waiting, Cmp::Gt, 0.0)
    }

    #[test]
    fn kleene_tables() {
        use Verdict::*;
        assert_eq!(True.and(Unknown), Unknown);
        assert_eq!(False.and(Unknown), False);
        assert_eq!(True.or(Unknown), True);
        assert_eq!(False.or(Unknown), Unknown);
        assert_eq!(Unknown.not(), Unknown);
    }

    #[test]
    fn empty_trace_is_unknown() {
        let tr = Trace::new(10.0);
        assert_eq!(waiting_pos().eval(tr.observations()), Verdict::Unknown);
    }

    #[test]
    fn historically_is_unknown_until_the_window_is_covered() {
        let tr = trace_of(&[(0.0, 1.0, 0.0), (10.0, 1.0, 0.0), (20.0, 1.0, 0.0)]);
        let f = waiting_pos().historically(30.0);
        assert_eq!(f.eval(tr.observations()), Verdict::Unknown);
        let g = waiting_pos().historically(20.0);
        assert_eq!(g.eval(tr.observations()), Verdict::True);
    }

    #[test]
    fn historically_is_false_immediately_on_a_counterexample() {
        let tr = trace_of(&[(0.0, 1.0, 0.0), (10.0, 0.0, 0.0), (20.0, 1.0, 0.0)]);
        let f = waiting_pos().historically(300.0);
        assert_eq!(f.eval(tr.observations()), Verdict::False);
    }

    #[test]
    fn once_is_true_on_a_witness_even_if_the_window_is_not_covered() {
        let tr = trace_of(&[(0.0, 0.0, 0.0), (10.0, 1.0, 0.0)]);
        let f = waiting_pos().once(300.0);
        assert_eq!(f.eval(tr.observations()), Verdict::True);
    }

    #[test]
    fn once_is_unknown_when_no_witness_and_window_not_covered() {
        let tr = trace_of(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.0)]);
        let f = waiting_pos().once(300.0);
        assert_eq!(f.eval(tr.observations()), Verdict::Unknown);
    }

    #[test]
    fn once_is_false_when_covered_and_no_witness() {
        let tr = trace_of(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.0), (20.0, 0.0, 0.0)]);
        let f = waiting_pos().once(20.0);
        assert_eq!(f.eval(tr.observations()), Verdict::False);
    }

    #[test]
    fn rate_predicate_is_unknown_on_the_first_observation() {
        let tr = trace_of(&[(0.0, 1.0, 0.0)]);
        let f = Formula::pred(Signal::TokenRate, Cmp::Eq, 0.0);
        assert_eq!(f.eval(tr.observations()), Verdict::Unknown);
    }

    #[test]
    fn missing_optional_signal_is_unknown_not_false() {
        let tr = trace_of(&[(0.0, 1.0, 0.0), (2.0, 1.0, 0.0)]);
        let f = Formula::pred(Signal::KvUsage, Cmp::Ge, 0.9);
        assert_eq!(f.eval(tr.observations()), Verdict::Unknown);
    }

    #[test]
    fn nested_temporal_operators_evaluate_the_inner_formula_in_the_past() {
        // "at some point in the last 10s, waiting had been positive for the 4s before that point"
        let pts: Vec<(f64, f64, f64)> = (0..=10)
            .map(|i| {
                (
                    i as f64 * 2.0,
                    if (4..=8).contains(&i) { 1.0 } else { 0.0 },
                    0.0,
                )
            })
            .collect();
        let tr = trace_of(&pts);
        let f = waiting_pos().historically(4.0).once(20.0);
        assert_eq!(f.eval(tr.observations()), Verdict::True);
        let never = waiting_pos().historically(20.0).once(20.0);
        assert_eq!(never.eval(tr.observations()), Verdict::False);
    }

    #[test]
    fn display_is_readable() {
        let f = waiting_pos()
            .and(Formula::pred(Signal::TokenRate, Cmp::Eq, 0.0))
            .historically(30.0);
        assert_eq!(
            f.to_string(),
            "always[30s] ((waiting > 0) and (token_rate == 0))"
        );
    }

    #[test]
    fn a_hole_in_the_window_makes_the_verdict_unknown() {
        // Scrapes every 2s, then a 30s silence, then two more.
        let tr = trace_of(&[
            (0.0, 1.0, 0.0),
            (2.0, 1.0, 0.0),
            (4.0, 1.0, 0.0),
            (34.0, 1.0, 0.0),
            (36.0, 1.0, 0.0),
        ]);
        let f = waiting_pos().historically(30.0);
        assert_eq!(
            f.eval_with_gap(tr.observations(), f64::INFINITY),
            Verdict::True
        );
        assert_eq!(f.eval_with_gap(tr.observations(), 6.0), Verdict::Unknown);
    }

    #[test]
    fn a_hole_never_hides_a_counterexample() {
        let tr = trace_of(&[
            (0.0, 1.0, 0.0),
            (2.0, 0.0, 0.0),
            (34.0, 1.0, 0.0),
            (36.0, 1.0, 0.0),
        ]);
        let f = waiting_pos().historically(40.0);
        assert_eq!(f.eval_with_gap(tr.observations(), 6.0), Verdict::False);
    }

    #[test]
    fn steady_scrapes_within_the_gap_are_still_covered() {
        let pts: Vec<_> = (0..20).map(|i| (i as f64 * 2.0, 1.0, 0.0)).collect();
        let tr = trace_of(&pts);
        let f = waiting_pos().historically(20.0);
        assert_eq!(f.eval_with_gap(tr.observations(), 6.0), Verdict::True);
    }

    #[test]
    fn progress_rate_counts_prompt_tokens() {
        let mut tr = Trace::new(100.0);
        for (t, gen, prompt) in [(0.0, 0.0, 0.0), (2.0, 0.0, 400.0), (4.0, 0.0, 800.0)] {
            tr.push(Sample {
                t,
                running: 1.0,
                waiting: 0.0,
                generation_tokens_total: gen,
                prompt_tokens_total: Some(prompt),
                kv_usage: None,
                preemptions_total: None,
            })
            .unwrap();
        }
        let none_generated = Formula::pred(Signal::TokenRate, Cmp::Eq, 0.0);
        let no_progress = Formula::pred(Signal::ProgressRate, Cmp::Eq, 0.0);
        assert_eq!(none_generated.eval(tr.observations()), Verdict::True);
        assert_eq!(no_progress.eval(tr.observations()), Verdict::False);
    }

    #[test]
    fn signals_lists_each_signal_once() {
        let f = waiting_pos()
            .or(Formula::pred(Signal::Running, Cmp::Gt, 0.0))
            .and(Formula::pred(Signal::ProgressRate, Cmp::Eq, 0.0))
            .and(waiting_pos())
            .historically(30.0);
        let names: Vec<_> = f.signals().iter().map(|s| s.name()).collect();
        assert_eq!(names, ["waiting", "running", "progress_rate"]);
    }
}
