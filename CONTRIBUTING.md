# Contributing

Thanks for looking. The most useful contribution right now is evidence from a real server, because NexusGuard has only been run on synthetic data.

## Send a metrics sample

If you run vLLM, SGLang or another server with a Prometheus endpoint, open an issue with the "Metrics from a real server" template and paste the output of `curl http://your-server/metrics` (remove anything private, such as model or host names). That tells us whether the metric names in `src/prom.rs` match your version.

## Set up

You need a recent stable Rust toolchain.

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The same three commands run in CI.

## Where things are

See the table in [docs/DESIGN.md](docs/DESIGN.md). In short: `prom.rs` reads a page, `trace.rs` keeps history, `formula.rs` evaluates rules, `rules.rs` defines them, `monitor.rs` turns verdicts into events, `render.rs` and `serve.rs` show them.

## Add a rule

Rules are formulas in `src/rules.rs`. A rule describes the bad pattern and fires when it is true. For example, a rule for "output is slow while the queue is long":

```rust
let slow = Formula::pred(Signal::Waiting, Cmp::Ge, 10.0)
    .and(Formula::pred(Signal::TokenRate, Cmp::Lt, 50.0))
    .historically(30.0);
```

Then add it to `builtin_rules`, give it a name, a severity and a one-sentence description, and add a scenario test in `tests/scenarios.rs` that shows it firing on a trace and staying quiet on `Scenario::Healthy`. If the rule needs a new signal, add it to `Signal` in `src/formula.rs` and to `Sample` in `src/sample.rs`.

## Add a server's metric names

`src/prom.rs` has a `MetricNames` table with a list of candidate names per field. Add a second table for the new server and a test that parses a real sample page.

## Figures

The timeline figures in `docs/assets/` are generated. After a change to `render.rs`, `sim.rs` or the rules, run:

```bash
./scripts/regen-figures.sh
```

and commit the result. CI fails if they are stale. `architecture.svg` and `window-anatomy.svg` are drawn by hand.

## Pull requests

- Keep a change to one thing.
- Add or update a test. For logic changes, a property test in `tests/properties.rs` is welcome.
- Say in the PR what you ran. If you could not test it on a real server, say that.
- Do not add dependencies without a reason. The web server is deliberately standard library only.
