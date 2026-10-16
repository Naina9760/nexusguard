# Design notes

## Data flow

![Data flow](assets/architecture.svg)

| Module | Job |
|---|---|
| `prom.rs` | Reads the Prometheus text format into a `Sample`. Holds one metric-name profile per server (vLLM, SGLang) and picks one from the metric prefixes on the page. Gauges and counters are summed across series (one per engine or model); KV usage takes the maximum, because the fullest cache is the one in trouble. A malformed line is skipped. A missing required metric is an error that names the metric. |
| `trace.rs` | Keeps roughly the last N seconds, where N is the longest lookback of any rule. Rejects non-finite values and timestamps that do not increase. Computes per-second rates, treating a drop in a counter as a restart (the Prometheus rule). |
| `formula.rs` | The temporal logic. |
| `rules.rs` | The three built-in rules, written as formulas. |
| `monitor.rs` | Runs the rules on each sample, emits an event when a rule starts or stops firing, and records a `Frame` (the signals plus one verdict per rule) for display. |
| `render.rs` | Draws frames as the timeline SVG. |
| `serve.rs` | The web page: three static files, a JSON state endpoint and the timeline. Standard library only. |
| `watch.rs` | The scrape loop shared by `watch` and `serve`. It reports back through a small `Hooks` trait, so each command decides what to do with samples, events, the detected server and scrape errors. |
| `sink.rs` | Stdout, webhook and exec output. |
| `sim.rs` | Synthetic traces for demos and tests. |

## The logic

Atoms compare one signal (`running`, `waiting`, `kv_usage`, `token_rate`, `progress_rate`, `preemption_rate`) with a constant. `progress_rate` is generated plus prompt tokens per second, so reading a long prompt counts as progress. Connectives are `not`, `and`, `or`. The two temporal operators look backwards only:

- `always[w] f`: `f` held at every sample in the last `w` seconds.
- `once[w] f`: `f` held at some sample in the last `w` seconds.

Because they only look back, a verdict exists as soon as a sample arrives. Nothing waits on the future, which is what lets the same code run live and on a recorded trace.

### Three values

Verdicts follow Kleene's strong three-valued logic.

![How a rule reaches unknown, true or false](assets/window-anatomy.svg)

| Situation | Verdict |
|---|---|
| An atom reads a signal the server does not export | unknown |
| An atom reads a rate on the first sample | unknown |
| `always[w] f`, one sample in the window breaks `f` | false, even if the window is not yet full |
| `always[w] f`, every sample holds, but the trace starts after the window start | unknown |
| `always[w] f`, every sample holds, but two neighbouring samples in the window are further apart than `max_gap` | unknown |
| `always[w] f`, every sample holds and the window is covered | true |
| `once[w] f`, one sample satisfies `f` | true, even if the window is not yet full |
| `once[w] f`, none satisfy it and the window is not covered | unknown |

A rule starts firing when its pattern is true and stops when it is false. Unknown changes nothing, so a missing metric or a short history cannot raise or clear an alert.

Rules are written as the bad pattern (`always[30s] (waiting > 0 and token_rate == 0)`) and fire when it is true. If a rule were written as an invariant and fired on false, an unknown result would be ambiguous. As a bad pattern, unknown just means "don't alert yet".

## Holes in the data

`max_gap` is the longest silence between two samples that still counts as continuous coverage. Live monitoring sets it to three scrape intervals plus a second, and replay sets it to five times the trace's median spacing. If the samples inside a window are further apart than that, for example because the metrics endpoint stopped answering, the window is not covered, and a verdict that would be true (or false, for `once`) becomes unknown.

This only ever removes certainty. A counterexample inside a hole still settles an `always` rule as false, and a property test checks that a gap limit can never turn a true verdict into a false one or the reverse. After an outage the rule waits until the hole has left its window.

## The timeline

`render.rs` draws from the `Frame`s the monitor keeps, so it cannot disagree with the alerts. Neighbouring samples with the same verdict are merged into one rectangle, which keeps the SVG small. A window that starts in the middle of an alarm draws no firing marker at its left edge, because the real firing time is outside the window. The web page fetches the same SVG from `/timeline.svg`.

## Laws checked by property tests

- `not (not f) == f`
- `once[w] f == not (always[w] (not f))`
- if `always[w1 + e] f` is true then `always[w1] f` is true
- `and` and `or` commute
- violations and recoveries alternate for each rule
- a gap limit only changes a verdict to unknown, never to its opposite
- a longer `always` window still implies a shorter one when a gap limit is set
- the built-in monitor never panics on ordered input

## Known gaps

- `max_gap` is one number for all rules. A rule with a short window and a slow scrape interval can stay at unknown; `watch` warns about that at start-up.
- Rates use the time between consecutive scrapes. Very uneven scraping makes them noisy.
- `==` on floats is exact. That is fine for `token_rate == 0` (a counter that did not move gives exactly zero) and wrong for computed thresholds.
- The web page has no authentication. It binds to loopback and rejects requests whose `Host` header is not a loopback name, which blocks DNS rebinding from a browser, but anyone with a shell on the machine can read it.
