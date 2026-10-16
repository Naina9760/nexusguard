//! End-to-end behaviour of the built-in rules over synthetic traces.

use nexusguard::formula::{Cmp, Formula, Signal};
use nexusguard::monitor::{Event, Monitor};
use nexusguard::rules::{builtin_rules, RuleConfig};
use nexusguard::sample::Sample;
use nexusguard::sim::{generate, Scenario};

fn monitor() -> Monitor {
    Monitor::new(builtin_rules(&RuleConfig::default()))
}

fn run(samples: Vec<Sample>) -> Vec<Event> {
    let mut m = monitor();
    let mut out = Vec::new();
    for s in samples {
        out.extend(m.ingest(s).expect("synthetic samples are well formed"));
    }
    out
}

fn violation_times(events: &[Event], rule: &str) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Violation { rule: r, t, .. } if r == rule => Some(*t),
            _ => None,
        })
        .collect()
}

fn recovery_times(events: &[Event], rule: &str) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Recovered { rule: r, t } if r == rule => Some(*t),
            _ => None,
        })
        .collect()
}

#[test]
fn healthy_traffic_raises_nothing() {
    let events = run(generate(Scenario::Healthy, 300, 2.0));
    assert_eq!(events, vec![]);
}

#[test]
fn a_stall_fires_once_after_the_configured_window_and_not_before() {
    // Output stops at t=40s; the default window is 30s, so the first moment
    // the whole window is stalled is t=70s.
    let events = run(generate(Scenario::Stall, 60, 2.0));
    assert_eq!(violation_times(&events, "engine_stalled"), vec![70.0]);
    assert_eq!(violation_times(&events, "kv_thrash"), Vec::<f64>::new());
}

#[test]
fn a_stall_also_eventually_looks_like_a_backlog() {
    // The queue is last empty at t=32s (the healthy prefix cycles 0..3), so it
    // is non-empty from t=34s onward; the backlog window is 60s: t=94s.
    let events = run(generate(Scenario::Stall, 60, 2.0));
    assert_eq!(violation_times(&events, "queue_backlog"), vec![94.0]);
}

#[test]
fn thrashing_fires_once_and_is_not_mistaken_for_a_stall() {
    // KV is near full from t=40s; the thrash window is 20s, so t=60s.
    let events = run(generate(Scenario::Thrash, 60, 2.0));
    assert_eq!(violation_times(&events, "kv_thrash"), vec![60.0]);
    assert_eq!(
        violation_times(&events, "engine_stalled"),
        Vec::<f64>::new()
    );
}

#[test]
fn a_short_pause_shorter_than_the_window_is_ignored() {
    let mut samples = generate(Scenario::Healthy, 60, 2.0);
    // Freeze output for 10 seconds (5 samples) while requests wait.
    let frozen = samples[24].generation_tokens_total;
    for s in samples.iter_mut().skip(25).take(5) {
        s.generation_tokens_total = frozen;
        s.waiting = 3.0;
    }
    assert_eq!(
        violation_times(&run(samples), "engine_stalled"),
        Vec::<f64>::new()
    );
}

#[test]
fn a_stall_that_ends_is_reported_as_recovered() {
    let mut samples = generate(Scenario::Stall, 50, 2.0);
    let last = samples.last().unwrap().clone();
    // Output resumes at t=100s.
    for k in 1..=5 {
        samples.push(Sample {
            t: last.t + 2.0 * k as f64,
            generation_tokens_total: last.generation_tokens_total + 1600.0 * k as f64,
            ..last.clone()
        });
    }
    let events = run(samples);
    assert_eq!(violation_times(&events, "engine_stalled"), vec![70.0]);
    assert_eq!(recovery_times(&events, "engine_stalled"), vec![100.0]);
}

#[test]
fn a_missing_kv_metric_never_raises_a_thrash_alert() {
    let mut samples = generate(Scenario::Thrash, 60, 2.0);
    for s in &mut samples {
        s.kv_usage = None;
    }
    assert_eq!(
        violation_times(&run(samples), "kv_thrash"),
        Vec::<f64>::new()
    );
}

#[test]
fn a_server_restart_in_the_middle_of_a_stall_does_not_hide_it() {
    // The token counter drops to zero (process restart) and stays there. A
    // counter reset must not be read as output, so the stall is still found.
    let mut samples = generate(Scenario::Stall, 60, 2.0);
    for s in samples.iter_mut().skip(30) {
        s.generation_tokens_total = 0.0;
    }
    let events = run(samples);
    assert_eq!(violation_times(&events, "engine_stalled").len(), 1);
}

#[test]
fn an_idle_server_is_not_a_stall() {
    let events = run(generate(Scenario::Idle, 300, 2.0));
    assert_eq!(events, vec![]);
}

#[test]
fn a_long_prefill_is_slow_not_stuck() {
    // Ten minutes with no output tokens, but prompt tokens keep being read.
    let events = run(generate(Scenario::LongPrefill, 300, 2.0));
    assert_eq!(
        violation_times(&events, "engine_stalled"),
        Vec::<f64>::new()
    );
}

#[test]
fn a_hang_with_one_running_request_and_an_empty_queue_is_caught() {
    // The "Running: 1 reqs, Waiting: 0 reqs, KV 9.6%, 0 tokens/s" report.
    let samples = generate(Scenario::HangRunningOnly, 60, 2.0);
    let events = run(samples.clone());
    assert_eq!(violation_times(&events, "engine_stalled"), vec![70.0]);

    // The first version of the rule needed a non-empty queue and never fired here.
    let old_rule = Formula::pred(Signal::Waiting, Cmp::Gt, 0.0)
        .and(Formula::pred(Signal::TokenRate, Cmp::Eq, 0.0))
        .historically(30.0);
    let mut m = nexusguard::monitor::Monitor::new(vec![nexusguard::rules::Rule {
        name: "old".into(),
        severity: nexusguard::rules::Severity::Critical,
        description: String::new(),
        pattern: old_rule,
    }]);
    let mut fired = false;
    for s in samples {
        fired |= !m.ingest(s).unwrap().is_empty();
    }
    assert!(!fired, "the old rule should have missed this hang");
}

#[test]
fn a_saturated_hang_is_a_stall_and_not_thrashing() {
    // The "Running: 64 reqs, Waiting: 34 reqs, KV 96.8%, 0 tokens/s" report.
    let events = run(generate(Scenario::HangSaturated, 60, 2.0));
    assert_eq!(violation_times(&events, "engine_stalled"), vec![70.0]);
    assert_eq!(violation_times(&events, "kv_thrash"), Vec::<f64>::new());
}

#[test]
fn a_scrape_outage_inside_the_window_delays_the_alarm_instead_of_faking_it() {
    // Stall begins at t=40s. The metrics endpoint then goes silent from t=44s to
    // t=80s and comes back still stalled.
    let mut samples = generate(Scenario::Stall, 80, 2.0);
    samples.drain(22..41);

    let unlimited = run(samples.clone());
    assert_eq!(violation_times(&unlimited, "engine_stalled"), vec![82.0]);

    let mut guarded = Monitor::new(builtin_rules(&RuleConfig::default())).with_max_gap(6.0);
    let mut events = Vec::new();
    for s in samples {
        events.extend(guarded.ingest(s).unwrap());
    }
    let fired = violation_times(&events, "engine_stalled");
    assert_eq!(fired.len(), 1);
    assert!(
        fired[0] >= 112.0,
        "alarm at {} rests on evidence that spans the outage",
        fired[0]
    );
}
