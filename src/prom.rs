//! Turns a Prometheus text-format page into a [`Sample`].
//!
//! Each supported server has a [`Profile`]: the metric names for each signal.
//! Names differ between servers and between versions of the same server (for
//! example vLLM's KV usage gauge was `vllm:gpu_cache_usage_perc` before
//! `vllm:kv_cache_usage_perc`), so each field is a list of candidates and the
//! first one present on the page wins. `docs/VALIDATION.md` records where each
//! name came from.

use std::fmt;

use crate::sample::Sample;

pub struct MetricNames {
    pub running: &'static [&'static str],
    pub waiting: &'static [&'static str],
    pub kv_usage: &'static [&'static str],
    pub generation_tokens: &'static [&'static str],
    pub prompt_tokens: &'static [&'static str],
    pub preemptions: &'static [&'static str],
}

pub struct Profile {
    pub name: &'static str,
    /// A metric-name prefix that identifies the server on a page.
    pub prefix: &'static str,
    pub names: MetricNames,
}

pub const VLLM: Profile = Profile {
    name: "vllm",
    prefix: "vllm:",
    names: MetricNames {
        running: &["vllm:num_requests_running"],
        waiting: &["vllm:num_requests_waiting"],
        kv_usage: &["vllm:kv_cache_usage_perc", "vllm:gpu_cache_usage_perc"],
        generation_tokens: &["vllm:generation_tokens_total", "vllm:generation_tokens"],
        prompt_tokens: &["vllm:prompt_tokens_total", "vllm:prompt_tokens"],
        preemptions: &["vllm:num_preemptions_total", "vllm:num_preemptions"],
    },
};

/// SGLang needs `--enable-metrics`. It exports no preemption (retraction) counter.
pub const SGLANG: Profile = Profile {
    name: "sglang",
    prefix: "sglang:",
    names: MetricNames {
        running: &["sglang:num_running_reqs"],
        waiting: &["sglang:num_queue_reqs"],
        kv_usage: &["sglang:token_usage"],
        generation_tokens: &["sglang:generation_tokens_total"],
        prompt_tokens: &["sglang:prompt_tokens_total"],
        preemptions: &[],
    },
};

pub const PROFILES: [&Profile; 2] = [&VLLM, &SGLANG];

/// Which profile to use for a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerChoice {
    /// Pick by metric-name prefix on the page.
    Auto,
    Vllm,
    Sglang,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A required metric is not on the page.
    Missing(&'static str),
    /// Auto-detection found no known server prefix.
    UnknownServer,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Missing(name) => {
                write!(f, "required metric not found on the page: {name}")
            }
            ParseError::UnknownServer => write!(
                f,
                "no metrics with a known prefix (vllm: or sglang:) on the page; \
is this the right URL, and for SGLang was it started with --enable-metrics?"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

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

fn profile_for(
    series: &[(&str, f64)],
    choice: ServerChoice,
) -> Result<&'static Profile, ParseError> {
    match choice {
        ServerChoice::Vllm => Ok(&VLLM),
        ServerChoice::Sglang => Ok(&SGLANG),
        ServerChoice::Auto => PROFILES
            .into_iter()
            .find(|p| series.iter().any(|(n, _)| n.starts_with(p.prefix)))
            .ok_or(ParseError::UnknownServer),
    }
}

/// Builds a sample from a metrics page and says which profile read it.
///
/// A server can export one series per model or engine; gauges and counters are
/// summed across them, and KV usage takes the maximum (the fullest cache is the
/// one in trouble).
pub fn parse_page(
    text: &str,
    t: f64,
    choice: ServerChoice,
) -> Result<(Sample, &'static Profile), ParseError> {
    let series: Vec<(&str, f64)> = text.lines().filter_map(parse_line).collect();
    let profile = profile_for(&series, choice)?;
    let names = &profile.names;

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

    let sample = Sample {
        t,
        running: sum(names.running).ok_or(ParseError::Missing(names.running[0]))?,
        waiting: sum(names.waiting).ok_or(ParseError::Missing(names.waiting[0]))?,
        generation_tokens_total: sum(names.generation_tokens)
            .ok_or(ParseError::Missing(names.generation_tokens[0]))?,
        prompt_tokens_total: sum(names.prompt_tokens),
        kv_usage: max(names.kv_usage),
        preemptions_total: sum(names.preemptions),
    };
    Ok((sample, profile))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VLLM_PAGE: &str = r#"# HELP vllm:num_requests_running Number of requests in model execution batches.
# TYPE vllm:num_requests_running gauge
vllm:num_requests_running{engine="0",model_name="m1"} 3.0
vllm:num_requests_running{engine="1",model_name="m1"} 2.0
vllm:num_requests_waiting{engine="0",model_name="m1"} 4.0
vllm:kv_cache_usage_perc{engine="0",model_name="m1"} 0.41
vllm:kv_cache_usage_perc{engine="1",model_name="m1"} 0.93
vllm:generation_tokens_total{engine="0",model_name="m1"} 1000.0
vllm:generation_tokens_total{engine="1",model_name="m1"} 500.0
vllm:generation_tokens_created{engine="0",model_name="m1"} 1.7e9
vllm:prompt_tokens_total{engine="0",model_name="m1"} 9000.0
vllm:num_preemptions_total{engine="0",model_name="m1"} 7.0
"#;

    const SGLANG_PAGE: &str = r#"# TYPE sglang:num_running_reqs gauge
sglang:num_running_reqs{model_name="m",tp_rank="0"} 12.0
sglang:num_queue_reqs{model_name="m",tp_rank="0"} 3.0
sglang:token_usage{model_name="m",tp_rank="0"} 0.28
sglang:gen_throughput{model_name="m",tp_rank="0"} 412.5
sglang:generation_tokens_total{model_name="m"} 7000.0
sglang:prompt_tokens_total{model_name="m"} 21000.0
"#;

    fn vllm(text: &str) -> Result<Sample, ParseError> {
        parse_page(text, 12.5, ServerChoice::Auto).map(|(s, _)| s)
    }

    #[test]
    fn vllm_sums_gauges_and_counters_and_maxes_kv_usage() {
        let (s, p) = parse_page(VLLM_PAGE, 12.5, ServerChoice::Auto).unwrap();
        assert_eq!(p.name, "vllm");
        assert_eq!(s.t, 12.5);
        assert_eq!(s.running, 5.0);
        assert_eq!(s.waiting, 4.0);
        assert_eq!(s.generation_tokens_total, 1500.0);
        assert_eq!(s.prompt_tokens_total, Some(9000.0));
        assert_eq!(s.kv_usage, Some(0.93));
        assert_eq!(s.preemptions_total, Some(7.0));
    }

    #[test]
    fn sglang_page_is_detected_and_read() {
        let (s, p) = parse_page(SGLANG_PAGE, 1.0, ServerChoice::Auto).unwrap();
        assert_eq!(p.name, "sglang");
        assert_eq!(s.running, 12.0);
        assert_eq!(s.waiting, 3.0);
        assert_eq!(s.kv_usage, Some(0.28));
        assert_eq!(s.generation_tokens_total, 7000.0);
        assert_eq!(s.prompt_tokens_total, Some(21000.0));
        assert_eq!(
            s.preemptions_total, None,
            "SGLang has no preemption counter"
        );
    }

    #[test]
    fn an_unknown_page_says_so() {
        let err = parse_page("node_cpu_seconds_total 1\n", 0.0, ServerChoice::Auto)
            .map(|(s, _)| s)
            .unwrap_err();
        assert_eq!(err, ParseError::UnknownServer);
    }

    #[test]
    fn forcing_a_profile_skips_detection() {
        let err = parse_page("node_cpu_seconds_total 1\n", 0.0, ServerChoice::Sglang)
            .map(|(s, _)| s)
            .unwrap_err();
        assert_eq!(err, ParseError::Missing("sglang:num_running_reqs"));
    }

    #[test]
    fn optional_metrics_may_be_absent() {
        let page = "vllm:num_requests_running 1\nvllm:num_requests_waiting 0\nvllm:generation_tokens_total 5\n";
        let s = vllm(page).unwrap();
        assert_eq!(s.kv_usage, None);
        assert_eq!(s.preemptions_total, None);
        assert_eq!(s.prompt_tokens_total, None);
    }

    #[test]
    fn missing_required_metric_is_an_error_that_names_it() {
        let page = "vllm:num_requests_running 1\nvllm:num_requests_waiting 0\n";
        assert_eq!(
            vllm(page).unwrap_err(),
            ParseError::Missing("vllm:generation_tokens_total")
        );
    }

    #[test]
    fn falls_back_to_the_older_names() {
        let page = "vllm:num_requests_running 1\nvllm:num_requests_waiting 0\nvllm:generation_tokens 5\nvllm:gpu_cache_usage_perc 0.8\nvllm:prompt_tokens 9\n";
        let s = vllm(page).unwrap();
        assert_eq!(s.kv_usage, Some(0.8));
        assert_eq!(s.generation_tokens_total, 5.0);
        assert_eq!(s.prompt_tokens_total, Some(9.0));
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

    // Lines copied verbatim from the vLLM and SGLang documentation pages
    // (docs.vllm.ai/en/stable/usage/metrics, docs.sglang.io/references/production_metrics).
    #[test]
    fn parses_lines_copied_from_the_documentation() {
        let sglang_prompt = r#"sglang:prompt_tokens_total{model_name="meta-llama/Llama-3.1-8B-Instruct"} 8.128902e+06"#;
        assert_eq!(
            parse_line(sglang_prompt),
            Some(("sglang:prompt_tokens_total", 8_128_902.0))
        );
        let sglang_running =
            r#"sglang:num_running_reqs{model_name="meta-llama/Llama-3.1-8B-Instruct"} 162.0"#;
        assert_eq!(
            parse_line(sglang_running),
            Some(("sglang:num_running_reqs", 162.0))
        );
        let vllm_bucket = r#"vllm:iteration_tokens_total_bucket{le="1.0",model_name="unsloth/Llama-3.2-1B-Instruct"} 3.0"#;
        assert_eq!(
            parse_line(vllm_bucket),
            Some(("vllm:iteration_tokens_total_bucket", 3.0))
        );
    }

    #[test]
    fn a_histogram_whose_name_ends_in_total_is_not_a_token_counter() {
        // `vllm:iteration_tokens_total_sum` is a histogram series from the docs page.
        let page = "vllm:iteration_tokens_total_sum{model_name=\"m\"} 12345.0\nvllm:num_requests_running 1\nvllm:num_requests_waiting 0\n";
        assert_eq!(
            vllm(page).unwrap_err(),
            ParseError::Missing("vllm:generation_tokens_total")
        );
    }
}
