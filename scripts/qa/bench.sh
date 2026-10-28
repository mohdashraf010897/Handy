#!/usr/bin/env bash
# Benchmark Handy's headless CLI over the QA corpus (see scripts/qa/bench.py).
# Usage: scripts/qa/bench.sh [--app PATH/Handy.app] [--repeat N] [--limit N] MODEL_ID...
# MODEL_ID as printed by `handy --list-models`, e.g.
#   handy-computer/parakeet-unified-en-0.6b-gguf/parakeet-unified-en-0.6b-Q8_0.gguf
set -euo pipefail
cd "$(dirname "$0")/../.."
export HANDY_DISABLE_UPDATER=1
exec python3 scripts/qa/bench.py "$@"
