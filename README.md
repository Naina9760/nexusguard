# NexusGuard

NexusGuard watches an LLM inference server's metrics and tells you when the engine has stalled, the KV cache is thrashing, or the queue has stopped draining. It is a single Rust binary with a small web page for looking at what happened.

![NexusGuard web UI in demo mode, showing a stall being detected](docs/assets/ui-demo.png)

Version 0.2, early. It has been tested on synthetic traces and a fake metrics server. It has not been run against a live vLLM or SGLang server yet, so the metric names and default thresholds are unproven (see [Limits](#limits)).

## Why

An inference server can stop producing tokens without logging an error, and its `/metrics` endpoint keeps answering, so an "is it up" probe passes. What gives it away is a pattern over time: requests are waiting and the token counter has not moved for half a minute. NexusGuard checks for patterns like that.

## Try it without a GPU

```bash
cargo build --release
./target/release/nexusguard serve --demo stall
```

Open http://127.0.0.1:9100. The demo plays a synthetic server that stops producing tokens 40 seconds in (simulated time, replayed at two samples per second). `engine_stalled` fires at 70 s and `queue_backlog` at 94 s.

To point it at a real server instead:

```bash
./target/release/nexusguard serve --url http://localhost:8000/metrics
```

Without the web page, `watch` does the same job and prints one JSON line per event:

```bash
./target/release/nexusguard watch --url http://localhost:8000/metrics
```

## Reading the timeline

The top three strips are what the server did: queue length, tokens per second, KV cache use. Each row underneath is one rule, coloured by what the rule concluded at that moment. The bracket under a row shows how far back the rule looks.

![Timeline of a stall: tokens stop at 40 s, engine_stalled fires at 70 s, queue_backlog at 94 s](docs/assets/timeline-stall.svg)

A rule needs a full window of history before it can say a pattern holds. If you start watching a server that is already stalled, the rule shows hatched ("not enough data yet") until the window fills, then turns red:

![Timeline when attaching to a server that is already stalled: hatched for 30 s, then the alarm](docs/assets/timeline-attached-stalled.svg)

The rule's verdict has three values, and the picture below shows why. One sample that breaks the pattern settles the question straight away. A window that reaches back before monitoring started cannot be confirmed.

![Three examples of a rule reaching unknown, true and false](docs/assets/window-anatomy.svg)

Every figure here is drawn by the same code that decides the alerts. `scripts/regen-figures.sh` redraws them, and CI fails if they are out of date.

## What it checks

| Rule | Severity | Fires when |
|---|---|---|
| `engine_stalled` | critical | for 30 s, requests were waiting and zero tokens were generated |
| `kv_thrash` | warning | for 20 s, KV cache use stayed at 90% or more, and at least one preemption happened in that time |
| `queue_backlog` | warning | for 60 s, at least one request was always waiting |

Every number is a flag (`--stall-secs`, `--thrash-secs`, `--kv-threshold`, `--backlog-secs`, `--backlog-min`). `nexusguard rules` prints the active rules.

Each rule fires once when its pattern starts to hold and sends a `recovered` event when it stops. It does not repeat on every scrape. `watch` and `serve` also report `scrape_failing` when the metrics endpoint cannot be read three times in a row.

## Commands

| Command | What it does |
|---|---|
| `serve` | `watch` plus a local web page. `--demo healthy\|stall\|thrash\|attached-stalled` plays synthetic data. |
| `watch` | Scrape on an interval and print an event when a rule starts or stops firing. |
| `replay FILE` | Run a recorded trace (JSON lines) through the rules. Exits 2 if a rule fired, so it works as a CI check. |
| `render FILE` | Draw a trace as the timeline SVG. |
| `simulate SCENARIO` | Print a synthetic trace. |
| `rules` | Print the active rules. |

`watch` and `serve` can send events elsewhere, both off by default: `--webhook URL` posts each event as JSON, and `--exec PROGRAM` runs a program (no shell) with the event on stdin. `serve` listens on 127.0.0.1 only and has no login, so keep it on loopback unless you put something in front of it.

## How it works

![Data flow: metrics page, parse, trace, rule engine, monitor, outputs](docs/assets/architecture.svg)

Each scrape becomes a sample. The trace keeps a bounded history and works out per-second rates from the counters, treating a drop in a counter as a restart, as Prometheus does. Rules are small formulas over that history: comparisons, `and`, `or`, `not`, and two time-bounded operators, "always for the last N seconds" and "at some point in the last N seconds". [docs/DESIGN.md](docs/DESIGN.md) has the details.

## Limits

- It has not been run against a live vLLM or SGLang server. The metric names (`vllm:num_requests_running`, `vllm:num_requests_waiting`, `vllm:kv_cache_usage_perc`, `vllm:generation_tokens_total`, `vllm:num_preemptions_total`) come from vLLM's documentation. Older versions used other names, and the parser tries the known alternatives.
- It sees aggregate numbers only: queue length and token counts. It cannot say which request is stuck, or catch one request starving while others make progress.
- `engine_stalled` needs `waiting > 0`. If clients hang while the server reports an empty queue, nothing matches.
- It reports and does nothing else. Use `--exec` to hook an alert up to your own action.
- The default thresholds are guesses. A batch job with very long prompts can go quiet for a while without being broken.
- If scrapes are far apart compared with a rule's window, the window holds too few points to mean much.
- It is runtime monitoring, not a proof about the server. The rule logic itself is checked with property-based tests.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

See [CONTRIBUTING.md](CONTRIBUTING.md). If you run a real server, the most useful thing you can send is a sample of its `/metrics` page: there is an issue template for it.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
