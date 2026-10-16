# What has been checked, and against what

NexusGuard has not been run against a live vLLM or SGLang server. This page records what each metric name and each rule rests on, so you can see which parts are verified and which are not.

## Metric names

"Docs" means the official documentation page. "Source" means the server's own source code on its main branch. Both were read on 2026-10-17; names change between versions, so check them against your own `/metrics` page.

### vLLM

| Signal | Metric | Checked against |
|---|---|---|
| running requests | `vllm:num_requests_running` | docs ([design/metrics](https://docs.vllm.ai/en/stable/design/metrics/)) and source |
| waiting requests | `vllm:num_requests_waiting` | docs and source |
| KV cache use (0 to 1) | `vllm:kv_cache_usage_perc` | docs and source. Older versions used `vllm:gpu_cache_usage_perc`; the parser tries both. |
| generated tokens | `vllm:generation_tokens_total` | docs. The source defines `vllm:generation_tokens`; the `_total` suffix is added by the Prometheus client when counters are exposed. |
| prompt tokens | `vllm:prompt_tokens_total` | source (`vllm/v1/metrics/loggers.py`). Incremented every iteration from `iteration_stats.num_prompt_tokens`, so it moves while a long prompt is read. |
| preemptions | `vllm:num_preemptions_total` | source only. The docs page does not list a preemption counter, but the source defines `vllm:num_preemptions` ("Cumulative number of preemption from the engine"). |

### SGLang

Needs `--enable-metrics`. Names are from the [production metrics page](https://docs.sglang.io/references/production_metrics.html).

| Signal | Metric | Notes |
|---|---|---|
| running requests | `sglang:num_running_reqs` | |
| waiting requests | `sglang:num_queue_reqs` | |
| KV cache use | `sglang:token_usage` | A fraction (the docs example shows 0.28). |
| generated tokens | `sglang:generation_tokens_total` | |
| prompt tokens | `sglang:prompt_tokens_total` | |
| preemptions | none | The page lists no retraction or preemption metric, so `kv_thrash` is disabled for SGLang and the web page says so. |

With tensor parallelism, SGLang records request metrics on rank 0 only by default. If you start it with `--enable-metrics-for-all-schedulers`, each rank reports, and NexusGuard's sums count requests once per rank. That does not change whether a value is zero, so the rules still work.

### Not supported

Text Generation Inference exposes `tgi_queue_size` and `tgi_batch_current_size`, but its token count is a histogram (`tgi_request_generated_tokens`) that only grows when a request finishes. During a long generation the count stays flat, so a stall rule cannot tell slow from stuck. Supporting it would need a different kind of rule.

### Lines checked verbatim

The tests include lines copied from the two documentation pages, including SGLang's scientific-notation counter (`8.128902e+06`) and vLLM's histogram series `vllm:iteration_tokens_total_sum`, which has "tokens_total" in its name but is not a counter of generated tokens.

## Reports of real hangs

vLLM issue [#18431](https://github.com/vllm-project/vllm/issues/18431), "Engine stuck with requests are blocked", collects log lines from servers that stopped making progress while `/health` still returned 200. Three of them:

| Reported log line | Simulated by | What `engine_stalled` does |
|---|---|---|
| Running 64, Waiting 34, KV cache 96.8%, 0.0 tokens/s | `hang-saturated` | fires after 30 s (tested) |
| Running 17, Waiting 770, KV cache 13.5%, 0.0 tokens/s | not separately simulated | same logic as the first line, so it would fire after 30 s |
| Running 1, Waiting 0, KV cache 9.6%, 0.0 tokens/s | `hang-running-only` | fires after 30 s (tested) |

The third line has an empty queue. The first version of `engine_stalled` required a non-empty queue and would have missed it. That is why the rule now counts running requests as work. `tests/scenarios.rs` has a regression test that runs the old rule on that shape and checks that it stays silent.

The synthetic scenarios follow these numbers. They are not recordings, and the log lines are the only thing taken from the reports. The issue gives no root cause, and its thread says the stall persisted on newer versions.

## Two guards against false alarms

- An idle server (nothing running, nothing queued, no tokens) is not a stall. Scenario `idle`.
- A very long prompt can produce no output for minutes while the server reads it. Prompt tokens count as progress, so that is not a stall. Scenario `long-prefill`.

Neither guard has been tested against a real server either.

## What is still unverified

- The default thresholds (30 s, 20 s, 60 s, 90%).
- Whether `/metrics` keeps updating on a hung engine for every vLLM version. The issue's reporter says the running-request count and KV usage stayed constant while the server hung, which suggests it does, but that is one report.
- Behaviour with data-parallel or multi-model deployments beyond summing the series.
