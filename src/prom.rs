//! Turns a Prometheus text-format page into a [`Sample`].
//!
//! The names below are the vLLM ones. Different vLLM versions have used
//! different names (for example `vllm:gpu_cache_usage_perc` before
//! `vllm:kv_cache_usage_perc`), so each field has a list of candidates and the
//! first one present on the page wins. Verify them against the version you run.

use std::fmt;

use crate::sample::Sample;

pub struct MetricNames {
    pub running: &'static [&'static str],
    pub waiting: &'static [&'static str],
    pub kv_usage: &'static [&'static str],
    pub tokens: &'static [&'static str],
    pub preemptions: &'static [&'static str],
}

pub const VLLM: MetricNames = MetricNames {
    running: &["vllm:num_requests_running"],
    waiting: &["vllm:num_requests_waiting"],
    kv_usage: &["vllm:kv_cache_usage_perc", "vllm:gpu_cache_usage_perc"],
    tokens: &["vllm:generation_tokens_total", "vllm:generation_tokens"],
    preemptions: &["vllm:num_preemptions_total", "vllm:num_preemptions"],
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingMetric(pub &'static str);

impl fmt::Display for MissingMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "required metric not found on the page: {}", self.0)
    }
}

impl std::error::Error for MissingMetric {}

/// Parses one exposition line into `(metric name, value)`.
/// Comments, blank lines and anything malformed return `None`.
fn parse_line(line: &str) -> Option<(&str, f64)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let name_end = line.find(|c: char| c == '{' || c.is_whitespace())?;
    let name = &line[..name_end];
    let mut rest = &line[name_end..];

    if rest.starts_with('{') {
        // Skip the label set. Label values are quoted and may contain `}` or spaces.
        let mut in_quotes = false;
        let mut escaped = false;
        let mut end = None;
        for (i, c) in rest.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' if in_quotes => escaped = true,
                '"' => in_quotes = !in_quotes,
                '}' if !in_quotes => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        rest = &rest[end? + 1..];
    }

    let value = rest.split_whitespace().next()?.parse::<f64>().ok()?;
    Some((name, value))
}

fn collect(series: &[(&str, f64)], candidates: &[&str]) -> Option<Vec<f64>> {
    candidates.iter().find_map(|cand| {
        let values: Vec<f64> = series
            .iter()
            .filter(|(n, _)| n == cand)
            .map(|(_, v)| *v)
            .collect();
        if values.is_empty() {
            None
        } else {
            Some(values)
        }
    })
}

/// Builds a sample from a metrics page.
///
/// A server can export one series per model or engine; gauges and counters are
/// summed across them, and KV usage takes the maximum (the fullest cache is the
/// one in trouble).
pub fn parse_sample(text: &str, t: f64, names: &MetricNames) -> Result<Sample, MissingMetric> {
    let series: Vec<(&str, f64)> = text.lines().filter_map(parse_line).collect();

    let sum = |c: &[&str]| collect(&series, c).map(|v| v.iter().sum::<f64>());
    // `f64::max` ignores NaN, which would hide a broken series; keep it so the trace rejects it.
    let max = |c: &[&str]| {
        collect(&series, c).map(|v| {
            v.into_iter().fold(f64::NEG_INFINITY, |a, b| {
                if a.is_nan() || b.is_nan() {
                    f64::NAN
                } else {
                    a.max(b)
                }
            })
        })
    };

    Ok(Sample {
        t,
        running: sum(names.running).ok_or(MissingMetric(names.running[0]))?,
        waiting: sum(names.waiting).ok_or(MissingMetric(names.waiting[0]))?,
        generation_tokens_total: sum(names.tokens).ok_or(MissingMetric(names.tokens[0]))?,
        kv_usage: max(names.kv_usage),
        preemptions_total: sum(names.preemptions),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"# HELP vllm:num_requests_running Number of requests in model execution batches.
# TYPE vllm:num_requests_running gauge
vllm:num_requests_running{engine="0",model_name="m1"} 3.0
vllm:num_requests_running{engine="1",model_name="m1"} 2.0
vllm:num_requests_waiting{engine="0",model_name="m1"} 4.0
vllm:kv_cache_usage_perc{engine="0",model_name="m1"} 0.41
vllm:kv_cache_usage_perc{engine="1",model_name="m1"} 0.93
vllm:generation_tokens_total{engine="0",model_name="m1"} 1000.0
vllm:generation_tokens_total{engine="1",model_name="m1"} 500.0
vllm:generation_tokens_created{engine="0",model_name="m1"} 1.7e9
vllm:num_preemptions_total{engine="0",model_name="m1"} 7.0
"#;

    #[test]
    fn sums_gauges_and_counters_and_maxes_kv_usage() {
        let s = parse_sample(PAGE, 12.5, &VLLM).unwrap();
        assert_eq!(s.t, 12.5);
        assert_eq!(s.running, 5.0);
        assert_eq!(s.waiting, 4.0);
        assert_eq!(s.generation_tokens_total, 1500.0);
        assert_eq!(s.kv_usage, Some(0.93));
        assert_eq!(s.preemptions_total, Some(7.0));
    }

    #[test]
    fn optional_metrics_may_be_absent() {
        let page = "vllm:num_requests_running 1\nvllm:num_requests_waiting 0\nvllm:generation_tokens_total 5\n";
        let s = parse_sample(page, 0.0, &VLLM).unwrap();
        assert_eq!(s.kv_usage, None);
        assert_eq!(s.preemptions_total, None);
    }

    #[test]
    fn missing_required_metric_is_an_error_that_names_it() {
        let page = "vllm:num_requests_running 1\nvllm:num_requests_waiting 0\n";
        let err = parse_sample(page, 0.0, &VLLM).unwrap_err();
        assert_eq!(err, MissingMetric("vllm:generation_tokens_total"));
    }

    #[test]
    fn falls_back_to_the_older_kv_metric_name() {
        let page = "vllm:num_requests_running 1\nvllm:num_requests_waiting 0\nvllm:generation_tokens_total 5\nvllm:gpu_cache_usage_perc 0.8\n";
        assert_eq!(parse_sample(page, 0.0, &VLLM).unwrap().kv_usage, Some(0.8));
    }

    #[test]
    fn label_values_may_contain_braces_quotes_and_spaces() {
        let line = r#"vllm:num_requests_waiting{model_name="a } \" b",x="y"} 9 1700000000"#;
        assert_eq!(parse_line(line), Some(("vllm:num_requests_waiting", 9.0)));
    }

    #[test]
    fn garbage_lines_are_skipped_not_fatal() {
        assert_eq!(parse_line("not a metric"), None);
        assert_eq!(parse_line("metric{unterminated 1"), None);
        assert_eq!(parse_line("# comment"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn accepts_the_special_float_spellings_prometheus_uses() {
        let (_, v) = parse_line("m NaN").unwrap();
        assert!(v.is_nan());
        let (_, v) = parse_line("m +Inf").unwrap();
        assert!(v.is_infinite());
    }
}
