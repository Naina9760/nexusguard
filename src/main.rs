use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::net::TcpListener;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

use nexusguard::monitor::{Event, Monitor};
use nexusguard::prom::ServerChoice;
use nexusguard::render::{timeline_svg, RenderOptions};
use nexusguard::rules::{builtin_rules, RuleConfig, Severity};
use nexusguard::sample::Sample;
use nexusguard::serve::{serve, Shared};
use nexusguard::sim::{generate, Scenario};
use nexusguard::sink::Emitter;
use nexusguard::watch::{self, Hooks, WatchConfig};

/// Watches an LLM inference server's metrics for stalls, KV-cache thrashing and queue backlog.
#[derive(Parser)]
#[command(name = "nexusguard", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scrape a /metrics endpoint (vLLM or SGLang) on an interval and report rule violations.
    Watch(WatchArgs),
    /// Same as watch, plus a local web page showing the live verdict timeline.
    Serve(ServeArgs),
    /// Run a recorded trace (one JSON sample per line) through the rules. Exits 2 if any rule fired.
    Replay(ReplayArgs),
    /// Draw a recorded trace as a verdict-timeline SVG.
    Render(RenderArgs),
    /// Print a synthetic trace as JSON lines.
    Simulate(SimulateArgs),
    /// Print the active rules and their thresholds.
    Rules(RuleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum ServerArg {
    /// Pick by the metric-name prefix on the page.
    Auto,
    Vllm,
    Sglang,
}

impl From<ServerArg> for ServerChoice {
    fn from(s: ServerArg) -> Self {
        match s {
            ServerArg::Auto => ServerChoice::Auto,
            ServerArg::Vllm => ServerChoice::Vllm,
            ServerArg::Sglang => ServerChoice::Sglang,
        }
    }
}

#[derive(Args, Clone)]
struct RuleArgs {
    /// Seconds with work in flight and no tokens generated or read before `engine_stalled` fires.
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
    /// Which server's metric names to read.
    #[arg(long, value_enum, default_value = "auto")]
    server: ServerArg,
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
    /// Which server's metric names to read.
    #[arg(long, value_enum, default_value = "auto")]
    server: ServerArg,
    /// Play a synthetic scenario instead of watching a server (see `simulate --help`).
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
#[command(after_help = scenario_help())]
struct SimulateArgs {
    /// Scenario name.
    scenario: String,
    #[arg(long, default_value_t = 60)]
    count: usize,
    #[arg(long, default_value_t = 2.0)]
    interval: f64,
}

fn scenario_help() -> String {
    format!("Scenarios: {}", Scenario::names())
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

/// `max_gap`: seconds of silence between samples that still counts as
/// continuous coverage. `None` means no limit.
fn build_monitor(args: &RuleArgs, max_gap: Option<f64>) -> Result<Monitor, String> {
    let cfg = rule_config(args);
    cfg.validate()?;
    let monitor = Monitor::new(builtin_rules(&cfg));
    Ok(match max_gap {
        Some(g) => monitor.with_max_gap(g),
        None => monitor,
    })
}

/// Five times the median spacing of a recorded trace, so a hole in it is not
/// mistaken for evidence. `None` if there are too few samples to tell.
fn gap_for_recorded(samples: &[Sample]) -> Option<f64> {
    let mut gaps: Vec<f64> = samples.windows(2).map(|w| w[1].t - w[0].t).collect();
    if gaps.is_empty() || gaps.iter().any(|g| !g.is_finite()) {
        return None;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    let median = gaps[gaps.len() / 2];
    (median > 0.0).then_some(median * 5.0)
}

/// Gap limit for live scraping: a few intervals, plus slack for a slow scrape.
fn gap_for_live(interval: Duration) -> f64 {
    interval.as_secs_f64() * 3.0 + 1.0
}

fn warn_if_interval_is_long(interval: Duration, args: &RuleArgs) {
    let shortest = args.stall_secs.min(args.thrash_secs).min(args.backlog_secs);
    if interval.as_secs_f64() * 3.0 > shortest {
        eprintln!(
            "nexusguard: warning: --interval {}s is long compared with the shortest rule window ({}s). \
Rules may stay at \"not enough data\"; scrape at least three times per window.",
            interval.as_secs_f64(),
            shortest
        );
    }
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
    let monitor = match build_monitor(&args, None) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    for r in monitor.rules() {
        let sev = match r.severity {
            Severity::Critical => "critical",
            Severity::Warning => "warning",
        };
        let needs: Vec<_> = r.pattern.signals().iter().map(|s| s.name()).collect();
        println!(
            "{} [{sev}]\n  fires when: {}\n  needs: {}\n  {}\n",
            r.name,
            r.pattern,
            needs.join(", "),
            r.description
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
    format!(
        "unknown scenario {name:?}; choose one of {}",
        Scenario::names()
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
    let samples = match read_samples(&args.file) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let mut monitor = match build_monitor(&args.rules, gap_for_recorded(&samples)) {
        Ok(m) => m,
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
    let samples = match read_samples(&args.file) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let mut monitor = match build_monitor(&args.rules, gap_for_recorded(&samples)) {
        Ok(m) => m,
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

struct WatchHooks {
    monitor: Monitor,
    emitter: Emitter,
}

impl Hooks for WatchHooks {
    fn sample(&mut self, sample: Sample) -> Vec<Event> {
        match self.monitor.ingest(sample) {
            Ok(events) => events,
            Err(e) => {
                eprintln!("nexusguard: dropped sample: {e}");
                Vec::new()
            }
        }
    }

    fn event(&mut self, event: &Event) {
        self.emitter.emit(event);
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
    let monitor = match build_monitor(&args.rules, Some(gap_for_live(interval))) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    warn_if_interval_is_long(interval, &args.rules);

    let cfg = WatchConfig {
        url: args.url,
        interval,
        max_scrapes: args.max_scrapes,
        max_scrape_failures: args.max_scrape_failures,
        server: args.server.into(),
    };
    let mut hooks = WatchHooks {
        monitor,
        emitter: Emitter::new(args.webhook, args.exec),
    };
    watch::run(&cfg, &mut hooks);
    ExitCode::SUCCESS
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct ServeHooks {
    shared: Arc<Mutex<Shared>>,
    emitter: Arc<Emitter>,
}

impl ServeHooks {
    fn ingest(&self, sample: Sample) -> Vec<Event> {
        match lock(&self.shared).monitor.ingest(sample) {
            Ok(events) => events,
            Err(e) => {
                eprintln!("nexusguard: dropped sample: {e}");
                Vec::new()
            }
        }
    }

    fn record(&self, event: &Event) {
        self.emitter.emit(event);
        lock(&self.shared).push_event(event.clone());
    }
}

impl Hooks for ServeHooks {
    fn sample(&mut self, sample: Sample) -> Vec<Event> {
        self.ingest(sample)
    }

    fn event(&mut self, event: &Event) {
        self.record(event);
    }

    fn server(&mut self, name: &'static str) {
        lock(&self.shared).server = Some(name.to_string());
    }

    fn scrape_error(&mut self, error: Option<&str>) {
        lock(&self.shared).scrape_error = error.map(str::to_owned);
    }
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
    // Demo samples are evenly spaced, so only live scraping needs a gap limit.
    let max_gap = scenario.is_none().then(|| gap_for_live(interval));
    let monitor = match build_monitor(&args.rules, max_gap) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    if scenario.is_none() {
        warn_if_interval_is_long(interval, &args.rules);
    }

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

    let mut hooks = ServeHooks {
        shared: Arc::clone(&shared),
        emitter: Arc::new(Emitter::new(None, None)),
    };
    let server_choice: ServerChoice = args.server.into();
    std::thread::spawn(move || {
        if let Some(scenario) = scenario {
            for sample in generate(scenario, 900, 2.0) {
                for e in hooks.ingest(sample) {
                    hooks.record(&e);
                }
                std::thread::sleep(Duration::from_secs_f64(args.demo_speed));
            }
        } else if let Some(url) = args.url {
            let cfg = WatchConfig {
                url,
                interval,
                max_scrapes: None,
                max_scrape_failures: args.max_scrape_failures,
                server: server_choice,
            };
            watch::run(&cfg, &mut hooks);
        }
    });

    match serve(listener, shared) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}
