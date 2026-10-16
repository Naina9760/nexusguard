# Changelog

## 0.3.0

Checked against what vLLM and SGLang export and what people reported when they hung.

### Changed
- `engine_stalled` now counts a running request as work and any generated or prompt token as progress. The old rule needed a non-empty queue, and vLLM issue #18431 includes a hang with `Running: 1, Waiting: 0`, which it would have missed. It also treated a long prompt being read as a stall; prompt tokens now count as progress.
- Windows with a hole in them (the metrics endpoint went quiet) are no longer treated as covered. After an outage a rule waits for fresh samples before it can fire.
- The timeline now shows running requests, prompt tokens per second and KV use, so a figure explains why a rule fired.

### Added
- SGLang support, with the server detected from the metric prefix (`--server auto|vllm|sglang`).
- Rules that need a metric the server does not export are marked unavailable, and the web page says which metric is missing.
- Scenarios `idle`, `long-prefill`, `hang-running-only` and `hang-saturated`, the last two following log lines from vLLM issue #18431.
- `docs/VALIDATION.md`, recording where each metric name and rule comes from and what is still unverified.
- Tests: regression for the missed hang, false-alarm guards, outage handling, verbatim documentation lines, and two property tests for gap handling.

## 0.2.0

A rewrite around what an inference server exposes.

### Changed
- The monitor reads a server's Prometheus `/metrics` page. The old per-request `SchedulerState` input is gone, because real servers do not expose which request is at the head of the queue.
- The single starvation check is replaced by a small bounded past-time temporal logic with three-valued verdicts, and three built-in rules: `engine_stalled`, `kv_thrash` and `queue_backlog`.
- README rewritten to describe what the tool does and what it does not.

### Added
- `serve`: a local web page with the live verdict timeline, rule cards and an event log. `--demo` plays synthetic data.
- `render`: draws a trace as a timeline SVG. The figures in `docs/assets/` come from it.
- `watch`, `replay`, `simulate` and `rules` commands.
- Counter-reset handling, edge-triggered events, optional webhook and exec output, and `scrape_failing` / `scrape_recovered` events.
- Unit, scenario and property-based tests, and CI running format, clippy, tests and a figure-freshness check.
- Architecture and window-anatomy diagrams, CONTRIBUTING, SECURITY, issue and PR templates, and license files.
- `examples/fake_metrics_server.py` for trying the tool without a GPU.

### Removed
- The "automatic rollback" and "cluster state secured" output. It did nothing.
- Unused dependencies (`crossbeam-queue`, `arc-swap`) and the duplicate `src/src/lib.rs`, which Cargo never compiled.

### Fixed
- Placeholder author email and repository URL in `Cargo.toml`.
- Unterminated code block and malformed clone URL in the README.

## 0.1.0

Initial prototype.
