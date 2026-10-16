#!/usr/bin/env python3
"""A fake /metrics endpoint for trying NexusGuard without a GPU.

It serves vLLM-style or SGLang-style metric names with scripted values: steady
output, then (optionally) all progress stops while requests stay queued, which
is what `engine_stalled` looks for. It is not a model server.

    python3 examples/fake_metrics_server.py --port 8099 --stall-after 10
    python3 examples/fake_metrics_server.py --port 8099 --flavor sglang
    nexusguard watch --url http://127.0.0.1:8099/metrics --interval 1 --stall-secs 5
"""
import argparse
import time
from http.server import BaseHTTPRequestHandler, HTTPServer


def page(elapsed: float, stall_after: float, flavor: str) -> str:
    stalled = stall_after > 0 and elapsed >= stall_after
    produced_for = min(elapsed, stall_after) if stall_after > 0 else elapsed
    generated = 800.0 * produced_for
    prompt = 300.0 * produced_for
    waiting = 5.0 if stalled else float(int(elapsed) % 3)
    if flavor == "sglang":
        labels = '{model_name="fake-model",tp_rank="0"}'
        return (
            f"sglang:num_running_reqs{labels} 8.0\n"
            f"sglang:num_queue_reqs{labels} {waiting}\n"
            f"sglang:token_usage{labels} 0.55\n"
            f"sglang:generation_tokens_total{labels} {generated}\n"
            f"sglang:prompt_tokens_total{labels} {prompt}\n"
        )
    labels = '{engine="0",model_name="fake-model"}'
    return (
        f"vllm:num_requests_running{labels} 8.0\n"
        f"vllm:num_requests_waiting{labels} {waiting}\n"
        f"vllm:kv_cache_usage_perc{labels} 0.55\n"
        f"vllm:generation_tokens_total{labels} {generated}\n"
        f"vllm:prompt_tokens_total{labels} {prompt}\n"
        f"vllm:num_preemptions_total{labels} 0.0\n"
    )


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8099)
    ap.add_argument("--stall-after", type=float, default=10.0,
                    help="seconds after start when all progress stops; 0 means never")
    ap.add_argument("--flavor", choices=["vllm", "sglang"], default="vllm")
    args = ap.parse_args()
    start = time.monotonic()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path != "/metrics":
                self.send_error(404)
                return
            body = page(time.monotonic() - start, args.stall_after, args.flavor).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; version=0.0.4")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    HTTPServer((args.host, args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
