use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::net::TcpListener;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand};

use nexusguard::monitor::{Event, Monitor};
use nexusguard::prom::VLLM;
use nexusguard::render::{timeline_svg, RenderOptions};
use nexusguard::rules::{builtin_rules, RuleConfig, Severity};
use nexusguard::sample::Sample;
use nexusguard::serve::{serve, Shared};
use nexusguard::sim::{generate, Scenario};
use nexusguard::sink::Emitter;
use nexusguard::watch::{self, WatchConfig};

/// Watches an LLM inference server's metrics for stalls, KV-cache thrashing and queue backlog.
#[derive(Parser)]
#[command(name = "nexusguard", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scrape a vLLM-style /metrics endpoint on an interval and report rule violations.
    Watch(WatchArgs),
    /// Same as watch, plus a local web page showing the live verdict timeline.
    Serve(ServeArgs),
    /// Run a recorded trace (one JSON sample per line) through the rules. Exits 2 if any rule fired.
    Replay(ReplayArgs),
    /// Draw a recorded trace as a verdict-timeline SVG.
    Render(RenderArgs),
    /// Print a synthetic trace as JSON lines (healthy, stall, thrash or attached-stalled).
    Simulate(SimulateArgs),
    /// Print the active rules and their thresholds.
    Rules(RuleArgs),
}

#[derive(Args, Clone)]
struct RuleArgs {
    /// Seconds of queued requests with zero token output before `engine_stalled` fires.
    #[arg(long, default_value_t = 30.0)]
    stall_secs: f64,
    /// Seconds of high KV usage with preemptions before `kv_thrash` fires.
    #[arg(long, default_value_t = 20.0)]
    thrash_secs: f64,
    /// KV usage fraction (0 to 1) counted as high.
    #[arg(long, default_value_t = 0.9)]
    kv_threshold: f64,
    /// Seconds of continuous queue backlog before `queue_backlog` fires.
    #[arg(long, default_value_t = 60.0)]
    backlog_secs: f64,
    /// Queue length at or above which the queue counts as backlogged.
    #[arg(long, default_value_t = 1.0)]
    backlog_min: f64,
}

#[derive(Args)]
struct WatchArgs {
    /// Metrics URL, e.g. http://localhost:8000/metrics
    #[arg(long)]
    url: String,
    /// Seconds between scrapes.
    #[arg(long, default_value_t = 2.0)]
    interval: f64,
    /// Stop after this many scrapes (default: run until interrupted).
    #[arg(long)]
    max_scrapes: Option<u64>,
    /// Consecutive failed scrapes before a `scrape_failing` event.
    #[arg(long, default_value_t = 3)]
    max_scrape_failures: u32,
    /// POST every event as JSON to this URL. Off by default; nothing is sent anywhere unless set.
    #[arg(long)]
    webhook: Option<String>,
    /// Run this program (no shell) for every event, with the event JSON on its stdin.
    #[arg(long)]
    exec: Option<String>,
    #[command(flatten)]
    rules: RuleArgs,
}

#[derive(Args)]
#[command(group(ArgGroup::new("source").required(true).args(["url", "demo"])))]
struct ServeArgs {
    /// Address for the web page. Anything other than loopback has no authentication.
    #[arg(long, default_value = "127.0.0.1:9100")]
    listen: String,
    /// Metrics URL to watch, e.g. http://localhost:8000/metrics
    #[arg(long)]
    url: Option<String>,
    /// Play a synthetic scenario instead of watching a server: healthy, stall, thrash or attached-stalled.
    #[arg(long)]
    demo: Option<String>,
    /// Demo only: real seconds between synthetic samples.
    #[arg(long, default_value_t = 0.5)]
    demo_speed: f64,
    /// Seconds between scrapes.
    #[arg(long, default_value_t = 2.0)]
    interval: f64,
    /// Consecutive failed scrapes before a `scrape_failing` event.
    #[arg(long, default_value_t = 3)]
    max_scrape_failures: u32,
    #[command(flatten)]
    rules: RuleArgs,
}

#[derive(Args)]
struct ReplayArgs {
    /// Trace file, or `-` for stdin.
    file: String,
    #[command(flatten)]
    rules: RuleArgs,
}

#[derive(Args)]
struct RenderArgs {
    /// Trace file, or `-` for stdin.
    file: String,
    /// Write the SVG here instead of stdout.
    #[arg(long)]
    out: Option<String>,
    /// Only draw the last N samples.
    #[arg(long)]
    last: Option<usize>,
    #[command(flatten)]
    rules: RuleArgs,
}

#[derive(Args)]
struct SimulateArgs {
    /// healthy, stall, thrash or attached-stalled
    scenario: String,
    #[arg(long, default_value_t = 60)]
    count: usize,
    #[arg(long, default_value_t = 2.0)]
    interval: f64,
}

fn rule_config(args: &RuleArgs) -> RuleConfig {
    RuleConfig {
        stall_secs: args.stall_secs,
        thrash_secs: args.thrash_secs,
        kv_threshold: args.kv_threshold,
        backlog_secs: args.backlog_secs,
        backlog_min: args.backlog_min,
    }
}

fn build_monitor(args: &RuleArgs) -> Result<Monitor, String> {
    let cfg = rule_config(args);
    cfg.validate()?;
    Ok(Monitor::new(builtin_rules(&cfg)))
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Watch(a) => watch_cmd(a),
        Command::Serve(a) => serve_cmd(a),
        Command::Replay(a) => replay(a),
        Command::Render(a) => render(a),
        Command::Simulate(a) => simulate(a),
        Command::Rules(a) => rules(a),
    }
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("nexusguard: {msg}");
    ExitCode::FAILURE
}

fn open_input(path: &str) -> Result<Box<dyn BufRead>, String> {
    if path == "-" {
        Ok(Box::new(BufReader::new(io::stdin())))
    } else {
        File::open(path)
            .map(|f| Box::new(BufReader::new(f)) as Box<dyn BufRead>)
            .map_err(|e| format!("{path}: {e}"))
    }
}

fn parse_interval(secs: f64) -> Result<Duration, String> {
    if secs.is_finite() && secs > 0.0 && secs <= 3600.0 {
        Ok(Duration::from_secs_f64(secs))
    } else {
        Err("--interval must be between 0 and 3600 seconds".into())
    }
}

fn rules(args: RuleArgs) -> ExitCode {
    let monitor = match build_monitor(&args) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    for r in monitor.rules() {
        let sev = match r.severity {
            Severity::Critical => "critical",
            Severity::Warning => "warning",
        };
        println!(
            "{} [{sev}]\n  fires when: {}\n  {}\n",
            r.name, r.pattern, r.description
        );
    }
    ExitCode::SUCCESS
}

fn simulate(args: SimulateArgs) -> ExitCode {
    let Some(scenario) = Scenario::parse(&args.scenario) else {
        return fail(unknown_scenario(&args.scenario));
    };
    if !(args.interval.is_finite() && args.interval > 0.0) {
        return fail("--interval must be a positive number");
    }
    for sample in generate(scenario, args.count, args.interval) {
        match serde_json::to_string(&sample) {
            Ok(line) => println!("{line}"),
            Err(e) => return fail(e),
        }
    }
    ExitCode::SUCCESS
}

fn unknown_scenario(name: &str) -> String {
    let names: Vec<_> = Scenario::ALL.iter().map(|(n, _)| *n).collect();
    format!(
        "unknown scenario {name:?}; choose one of {}",
        names.join(", ")
    )
}

fn read_samples(path: &str) -> Result<Vec<Sample>, String> {
    let mut samples = Vec::new();
    for (idx, line) in open_input(path)?.lines().enumerate() {
        let line = line.map_err(|e| format!("line {}: {e}", idx + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        samples.push(serde_json::from_str(&line).map_err(|e| format!("line {}: {e}", idx + 1))?);
    }
    Ok(samples)
}

fn replay(args: ReplayArgs) -> ExitCode {
    let mut monitor = match build_monitor(&args.rules) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let samples = match read_samples(&args.file) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };

    let emitter = Emitter::new(None, None);
    let (mut count, mut violations) = (0usize, 0usize);
    for (idx, sample) in samples.into_iter().enumerate() {
        match monitor.ingest(sample) {
            Ok(events) => {
                for e in events {
                    if matches!(e, Event::Violation { .. }) {
                        violations += 1;
                    }
                    emitter.emit(&e);
                }
            }
            Err(e) => return fail(format!("sample {}: {e}", idx + 1)),
        }
        count += 1;
    }
    eprintln!("nexusguard: replayed {count} sample(s), {violations} violation(s)");
    if violations > 0 {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn render(args: RenderArgs) -> ExitCode {
    let mut monitor = match build_monitor(&args.rules) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let samples = match read_samples(&args.file) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    for (idx, sample) in samples.into_iter().enumerate() {
        if let Err(e) = monitor.ingest(sample) {
            return fail(format!("sample {}: {e}", idx + 1));
        }
    }
    let frames: Vec<_> = monitor.frames().iter().cloned().collect();
    let svg = timeline_svg(
        &frames,
        monitor.rules(),
        &RenderOptions {
            kv_threshold: Some(args.rules.kv_threshold),
            last_frames: args.last,
        },
    );
    match args.out {
        Some(path) => match std::fs::write(&path, svg) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(format!("{path}: {e}")),
        },
        None => {
            print!("{svg}");
            ExitCode::SUCCESS
        }
    }
}

fn watch_cmd(args: WatchArgs) -> ExitCode {
    let interval = match parse_interval(args.interval) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    if args.max_scrape_failures == 0 {
        return fail("--max-scrape-failures must be at least 1");
    }
    let mut monitor = match build_monitor(&args.rules) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let emitter = Emitter::new(args.webhook, args.exec);
    let cfg = WatchConfig {
        url: args.url,
        interval,
        max_scrapes: args.max_scrapes,
        max_scrape_failures: args.max_scrape_failures,
        names: &VLLM,
    };
    watch::run(
        &cfg,
        |sample| match monitor.ingest(sample) {
            Ok(events) => events,
            Err(e) => {
                eprintln!("nexusguard: dropped sample: {e}");
                Vec::new()
            }
        },
        |event| emitter.emit(event),
        |_| {},
    );
    ExitCode::SUCCESS
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn serve_cmd(args: ServeArgs) -> ExitCode {
    let interval = match parse_interval(args.interval) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    if args.max_scrape_failures == 0 {
        return fail("--max-scrape-failures must be at least 1");
    }
    let scenario = match args.demo.as_deref() {
        Some(name) => match Scenario::parse(name) {
            Some(s) => Some(s),
            None => return fail(unknown_scenario(name)),
        },
        None => None,
    };
    if scenario.is_some() && !(args.demo_speed.is_finite() && args.demo_speed >= 0.0) {
        return fail("--demo-speed must be zero or a positive number");
    }
    let monitor = match build_monitor(&args.rules) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };

    let mode = match (&args.demo, &args.url) {
        (Some(d), _) => format!("demo: {d} (synthetic data)"),
        (None, Some(u)) => format!("live: {u}"),
        (None, None) => unreachable!("clap requires --url or --demo"),
    };
    let shared = Arc::new(Mutex::new(Shared::new(
        monitor,
        mode,
        Some(args.rules.kv_threshold),
    )));

    let listener = match TcpListener::bind(&args.listen) {
        Ok(l) => l,
        Err(e) => return fail(format!("cannot listen on {}: {e}", args.listen)),
    };
    let addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    eprintln!("nexusguard: web UI at http://{addr}/");
    if !addr.ip().is_loopback() {
        eprintln!("nexusguard: warning: {addr} is not a loopback address and the page has no authentication");
    }

    let emitter = Arc::new(Emitter::new(None, None));
    {
        let shared = Arc::clone(&shared);
        let emitter = Arc::clone(&emitter);
        std::thread::spawn(move || {
            let record = |e: &Event| {
                emitter.emit(e);
                lock(&shared).push_event(e.clone());
            };
            let ingest = |sample: Sample| match lock(&shared).monitor.ingest(sample) {
                Ok(events) => events,
                Err(e) => {
                    eprintln!("nexusguard: dropped sample: {e}");
                    Vec::new()
                }
            };

            if let Some(scenario) = scenario {
                for sample in generate(scenario, 900, 2.0) {
                    for e in ingest(sample) {
                        record(&e);
                    }
                    std::thread::sleep(Duration::from_secs_f64(args.demo_speed));
                }
            } else if let Some(url) = args.url {
                let cfg = WatchConfig {
                    url,
                    interval,
                    max_scrapes: None,
                    max_scrape_failures: args.max_scrape_failures,
                    names: &VLLM,
                };
                watch::run(&cfg, ingest, record, |err| {
                    lock(&shared).scrape_error = err.map(str::to_owned);
                });
            }
        });
    }

    match serve(listener, shared) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}
