#!/usr/bin/env bash
# Redraws the timeline figures in docs/assets/ from the engine itself.
# architecture.svg and window-anatomy.svg are drawn by hand and are not touched.
set -euo pipefail

cd "$(dirname "$0")/.."
out=docs/assets
bin="cargo run --quiet --release --"

for scenario in stall thrash attached-stalled hang-running-only long-prefill; do
  $bin simulate "$scenario" --count 90 | $bin render - --out "$out/timeline-$scenario.svg"
  echo "wrote $out/timeline-$scenario.svg"
done
