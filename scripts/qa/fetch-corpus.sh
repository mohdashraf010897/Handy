#!/usr/bin/env bash
# Download the pinned LibriSpeech QA subset (see scripts/qa/corpus.lock) into
# .qa-corpus/ as 16 kHz mono 16-bit WAV + reference transcripts.
# Usage: scripts/qa/fetch-corpus.sh [--regenerate-lock] [--out DIR]
set -euo pipefail
cd "$(dirname "$0")/../.."
exec python3 scripts/qa/fetch_corpus.py "$@"
