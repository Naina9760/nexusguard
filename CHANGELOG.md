# Changelog

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
