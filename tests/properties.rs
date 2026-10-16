//! Property-based tests: the temporal logic must obey its own laws on any trace.

use nexusguard::formula::{Cmp, Formula, Signal, Verdict};
use nexusguard::monitor::{Event, Monitor};
use nexusguard::rules::{builtin_rules, RuleConfig};
use nexusguard::sample::Sample;
use nexusguard::trace::Trace;
use proptest::prelude::*;

#[derive(Debug, Clone)]
struct Step {
    dt: f64,
    running: u8,
    waiting: u8,
    token_increase: u16,
    reset: bool,
    kv: Option<f64>,
    preempt_increase: Option<u8>,
}

fn step() -> impl Strategy<Value = Step> {
    (
        0.1f64..6.0,
        0u8..20,
        0u8..20,
        0u16..1500,
        prop::bool::weighted(0.05),
        prop::option::of(0.0f64..1.0),
        prop::option::of(0u8..3),
    )
        .prop_map(
            |(dt, running, waiting, token_increase, reset, kv, preempt_increase)| Step {
                dt,
                running,
                waiting,
                token_increase,
                reset,
                kv,
                preempt_increase,
            },
        )
}

fn build(steps: &[Step]) -> Vec<Sample> {
    let (mut t, mut tokens, mut preempt) = (0.0, 0.0, 0.0);
    steps
        .iter()
        .map(|s| {
            t += s.dt;
            tokens = if s.reset {
                0.0
            } else {
                tokens + f64::from(s.token_increase)
            };
            preempt += s.preempt_increase.map_or(0.0, f64::from);
            Sample {
                t,
                running: f64::from(s.running),
                waiting: f64::from(s.waiting),
                generation_tokens_total: tokens,
                kv_usage: s.kv,
                preemptions_total: s.preempt_increase.map(|_| preempt),
            }
        })
        .collect()
}

fn trace_of(samples: &[Sample]) -> Trace {
    let mut tr = Trace::new(10_000.0);
    for s in samples {
        tr.push(s.clone())
            .expect("generated samples are ordered and finite");
    }
    tr
}

fn atom() -> impl Strategy<Value = Formula> {
    let signal = prop_oneof![
        Just(Signal::Running),
        Just(Signal::Waiting),
        Just(Signal::KvUsage),
        Just(Signal::TokenRate),
        Just(Signal::PreemptionRate),
    ];
    let cmp = prop_oneof![
        Just(Cmp::Lt),
        Just(Cmp::Le),
        Just(Cmp::Gt),
        Just(Cmp::Ge),
        Just(Cmp::Eq)
    ];
    (signal, cmp, 0.0f64..20.0).prop_map(|(s, c, v)| Formula::pred(s, c, v))
}

proptest! {
    #[test]
    fn the_builtin_monitor_never_panics_on_any_ordered_input(steps in prop::collection::vec(step(), 0..120)) {
        let mut m = Monitor::new(builtin_rules(&RuleConfig::default()));
        for s in build(&steps) {
            prop_assert!(m.ingest(s).is_ok());
        }
    }

    #[test]
    fn violations_and_recoveries_alternate_per_rule(steps in prop::collection::vec(step(), 0..150)) {
        let mut m = Monitor::new(builtin_rules(&RuleConfig::default()));
        let mut firing: std::collections::HashMap<String, bool> = Default::default();
        for s in build(&steps) {
            for e in m.ingest(s).unwrap() {
                match e {
                    Event::Violation { rule, .. } => {
                        prop_assert!(!firing.insert(rule, true).unwrap_or(false), "violation while already firing");
                    }
                    Event::Recovered { rule, .. } => {
                        prop_assert!(firing.insert(rule, false).unwrap_or(false), "recovery while not firing");
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn double_negation_is_identity(steps in prop::collection::vec(step(), 1..60), f in atom(), w in 0.0f64..40.0) {
        let tr = trace_of(&build(&steps));
        let g = f.clone().historically(w);
        prop_assert_eq!(g.clone().not().not().eval(tr.observations()), g.eval(tr.observations()));
    }

    #[test]
    fn once_is_the_dual_of_historically(steps in prop::collection::vec(step(), 1..60), f in atom(), w in 0.0f64..40.0) {
        let tr = trace_of(&build(&steps));
        let once = f.clone().once(w).eval(tr.observations());
        let dual = f.not().historically(w).eval(tr.observations()).not();
        prop_assert_eq!(once, dual);
    }

    #[test]
    fn a_longer_always_window_implies_a_shorter_one(
        steps in prop::collection::vec(step(), 1..60), f in atom(), w1 in 0.0f64..20.0, extra in 0.0f64..20.0
    ) {
        let tr = trace_of(&build(&steps));
        let long = f.clone().historically(w1 + extra).eval(tr.observations());
        let short = f.historically(w1).eval(tr.observations());
        if long == Verdict::True {
            prop_assert_eq!(short, Verdict::True);
        }
    }

    #[test]
    fn and_is_commutative_and_or_is_commutative(steps in prop::collection::vec(step(), 1..40), a in atom(), b in atom()) {
        let tr = trace_of(&build(&steps));
        prop_assert_eq!(
            a.clone().and(b.clone()).eval(tr.observations()),
            b.clone().and(a.clone()).eval(tr.observations())
        );
        prop_assert_eq!(
            a.clone().or(b.clone()).eval(tr.observations()),
            b.or(a).eval(tr.observations())
        );
    }
}
