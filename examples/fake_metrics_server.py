#!/usr/bin/env python3
"""A fake /metrics endpoint for trying NexusGuard without a GPU.

It serves vLLM-style metric names with scripted values: steady output, then
(optionally) output stops while requests stay queued, which is what
`engine_stalled` looks for. It is not a model server.

    python3 examples/fake_metrics_server.py --port 8099 --stall-after 10
    nexusguard watch --url http://127.0.0.1:8099/metrics --interval 1 --stall-secs 5
"""
import argparse
import time
from http.server import BaseHTTPRequestHandler, HTTPServer


def page(elapsed: float, stall_after: float) -> str:
    stalled = stall_after > 0 and elapsed >= stall_after
    produced_for = min(elapsed, stall_after) if stall_after > 0 else elapsed
    tokens = 800.0 * produced_for
    waiting = 5 if stalled else int(elapsed) % 3
    labels = '{engine="0",model_name="fake-model"}'
    return (
        f"vllm:num_requests_running{labels} 8.0\n"
        f"vllm:num_requests_waiting{labels} {float(waiting)}\n"
        f"vllm:kv_cache_usage_perc{labels} 0.55\n"
        f"vllm:generation_tokens_total{labels} {tokens}\n"
        f"vllm:num_preemptions_total{labels} 0.0\n"
    )


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8099)
    ap.add_argument("--stall-after", type=float, default=10.0,
                    help="seconds after start when output stops; 0 means never")
    args = ap.parse_args()
    start = time.monotonic()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path != "/metrics":
                self.send_error(404)
                return
            body = page(time.monotonic() - start, args.stall_after).encode()
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
