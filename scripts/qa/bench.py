#!/usr/bin/env python3
"""Run Handy's headless CLI over the QA corpus and report WER, RTF, latency, load time.

Usage: scripts/qa/bench.sh [--app PATH] [--repeat N] [--limit N] MODEL_ID [MODEL_ID ...]
MODEL_ID is the id printed by `handy --list-models`, e.g.
handy-computer/parakeet-unified-en-0.6b-gguf/parakeet-unified-en-0.6b-Q8_0.gguf

For each model, every utterance in .qa-corpus/manifest.jsonl is transcribed by one
process: `handy --transcribe-file X.wav --model ID --repeat N --json`. Before that, one
untimed warm-up process runs (it pays the cold Metal shader compile, ~3.8 s).

Metrics (per model, also per subset):
  wer            corpus WER = sum(edit distance) / sum(reference words), after
                 normalisation: lowercase, apostrophes removed, other punctuation -> space.
                 No number/spelling normalisation (Whisper's "2" vs reference "TWO" counts).
  rtf            sum(per-utterance latency) / sum(audio seconds); < 1 is faster than real time.
  latency p50/95 over utterances of the per-utterance median of the N repeats (model loaded).
  first p50/95   over utterances of the first repeat (first inference after a model load).
  load p50       median load_ms over measured processes (warm shader cache, model from disk).

Output: .qa-results/<label>/{results.json,summary.md,cli.log} (overwritten each run) and,
for the whole invocation, .qa-results/summary.md. <label> is the model filename
without `.gguf` (e.g. parakeet-unified-en-0.6b-Q8_0).
Stdlib only. Inference is greedy, so transcripts are deterministic; timings are not.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import re
import statistics
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CORPUS = REPO_ROOT / ".qa-corpus"
RESULTS = REPO_ROOT / ".qa-results"
DEFAULT_APP = REPO_ROOT / "src-tauri/target/release/bundle/macos/Handy.app"
SETTINGS = Path.home() / "Library/Application Support/com.pais.handy/settings_store.json"
# Settings that change what TranscriptionManager::transcribe returns (transcription.rs).
RELEVANT_SETTINGS = [
    "selected_language", "translate_to_english", "custom_words",
    "filler_word_removal_enabled", "custom_filler_words", "accelerator",
]


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def normalise(text: str) -> list[str]:
    text = text.lower().replace("\u2019", "'").replace("'", "")
    return re.sub(r"[^a-z0-9]+", " ", text).split()


def edit_distance(ref: list[str], hyp: list[str]) -> int:
    prev = list(range(len(hyp) + 1))
    for i, r in enumerate(ref, 1):
        cur = [i] + [0] * len(hyp)
        for j, h in enumerate(hyp, 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (r != h))
        prev = cur
    return prev[-1]


def pct(values: list[float], q: float) -> float:
    """Nearest-rank percentile (q in 0..100); deterministic for small samples."""
    s = sorted(values)
    k = max(1, -(-len(s) * q // 100))  # ceil
    return s[int(k) - 1]


def label(model: str) -> str:
    """Short, filesystem-safe name: `org/repo/name-Q8_0.gguf` -> `name-Q8_0`."""
    return re.sub(r"\.gguf$", "", model.rsplit("/", 1)[-1])


def handy_bin(app: Path) -> Path:
    b = app / "Contents/MacOS/handy" if app.suffix == ".app" else app
    if not b.exists():
        raise SystemExit(f"error: Handy binary not found at {b} (pass --app or set HANDY_APP)")
    return b


def run_cli(binary: Path, wav: Path, model: str, repeat: int, logf) -> dict:
    env = dict(os.environ, HANDY_DISABLE_UPDATER="1")
    cmd = [str(binary), "--transcribe-file", str(wav), "--model", model,
           "--repeat", str(repeat), "--json"]
    p = subprocess.run(cmd, capture_output=True, text=True, env=env)
    logf.write(f"$ {' '.join(cmd)}\n{p.stderr}\n")
    if p.returncode != 0:
        raise SystemExit(f"error: CLI exit {p.returncode} for {wav.name}: {p.stderr.strip()[-500:]}")
    return json.loads(p.stdout.strip().splitlines()[-1])


def summarise(rows: list[dict]) -> dict:
    errs = sum(r["errors"] for r in rows)
    words = sum(r["ref_words"] for r in rows)
    audio = sum(r["audio_secs"] for r in rows)
    lat = [r["latency_ms"] for r in rows]
    first = [r["first_ms"] for r in rows]
    return {
        "utterances": len(rows),
        "audio_secs": round(audio, 2),
        "ref_words": words,
        "errors": errs,
        "wer": round(errs / words, 4) if words else None,
        "rtf": round(sum(lat) / 1000 / audio, 4) if audio else None,
        "latency_p50_ms": pct(lat, 50),
        "latency_p95_ms": pct(lat, 95),
        "first_p50_ms": pct(first, 50),
        "first_p95_ms": pct(first, 95),
    }


def bench_model(binary: Path, model: str, repeat: int, manifest: list[dict]) -> dict:
    out = RESULTS / label(model)
    out.mkdir(parents=True, exist_ok=True)
    rows = []
    with (out / "cli.log").open("w") as logf:
        warm = run_cli(binary, CORPUS / manifest[0]["wav"], model, 1, logf)
        log(f"[bench] {model}: warm-up load {warm['load_ms']} ms (discarded),"
            f" backend {warm.get('bound_backend')}")
        for i, u in enumerate(manifest, 1):
            res = run_cli(binary, CORPUS / u["wav"], model, repeat, logf)
            ref, hyp = normalise(u["reference"]), normalise(res["text"])
            rows.append({
                "id": u["id"],
                "subset": u["subset"],
                "audio_secs": res["audio_secs"],
                "load_ms": res["load_ms"],
                "transcribe_ms": res["transcribe_ms"],
                "latency_ms": statistics.median(res["transcribe_ms"]),
                "first_ms": res["transcribe_ms"][0],
                "reference": u["reference"],
                "hypothesis": res["text"],
                "ref_words": len(ref),
                "errors": edit_distance(ref, hyp),
            })
            log(f"[bench] {model} {i}/{len(manifest)} {u['id']}"
                f" {rows[-1]['latency_ms']:.0f} ms err={rows[-1]['errors']}/{len(ref)}")
    loads = [r["load_ms"] for r in rows]
    result = {
        "model": model,
        "label": label(model),
        "bound_backend": warm.get("bound_backend"),
        "repeat": repeat,
        "cold_load_ms_discarded": warm["load_ms"],
        "load_p50_ms": statistics.median(loads),
        "load_p95_ms": pct(loads, 95),
        "overall": summarise(rows),
        "by_subset": {s: summarise([r for r in rows if r["subset"] == s])
                      for s in sorted({r["subset"] for r in rows})},
        "utterances": rows,
    }
    return result


def environment(binary: Path) -> dict:
    def sh(*cmd):
        try:
            return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout.strip()
        except (OSError, subprocess.CalledProcessError):
            return None
    settings = {}
    if SETTINGS.exists():
        s = json.loads(SETTINGS.read_text()).get("settings", {})
        settings = {k: s.get(k) for k in RELEVANT_SETTINGS}
    return {
        "date": dt.datetime.now().astimezone().isoformat(timespec="seconds"),
        "cpu": sh("sysctl", "-n", "machdep.cpu.brand_string") or platform.processor(),
        "os": f"macOS {sh('sw_vers', '-productVersion')}" if sys.platform == "darwin" else platform.platform(),
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "repo_commit": sh("git", "-C", str(REPO_ROOT), "rev-parse", "--short", "HEAD"),
        "app_settings": settings,
    }


def markdown(env: dict, results: list[dict]) -> str:
    lines = [
        f"Date {env['date']} · {env['cpu']} · {env['os']} · commit {env['repo_commit']}",
        f"Binary sha256 `{env['binary_sha256'][:12]}` · app settings `{json.dumps(env['app_settings'])}`",
        "",
        "| Model | Subset | Utts | Audio s | WER | RTF | p50 ms | p95 ms | 1st p50 ms | 1st p95 ms | Warm load p50 ms |",
        "| --- | --- | --: | --: | --: | --: | --: | --: | --: | --: | --: |",
    ]
    for r in results:
        for name, s in [("all", r["overall"]), *r["by_subset"].items()]:
            lines.append(
                f"| {r['label']} | {name} | {s['utterances']} | {s['audio_secs']:.1f} |"
                f" {s['wer'] * 100:.2f}% | {s['rtf']:.4f} | {s['latency_p50_ms']:.0f} |"
                f" {s['latency_p95_ms']:.0f} | {s['first_p50_ms']:.0f} | {s['first_p95_ms']:.0f} |"
                f" {r['load_p50_ms']:.0f} |"
            )
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("models", nargs="+", help="model ids as listed by `handy --list-models`")
    ap.add_argument("--app", type=Path, default=Path(os.environ.get("HANDY_APP", DEFAULT_APP)))
    ap.add_argument("--repeat", type=int, default=3)
    ap.add_argument("--limit", type=int, help="only the first N utterances (smoke test)")
    args = ap.parse_args()

    manifest_path = CORPUS / "manifest.jsonl"
    if not manifest_path.exists():
        raise SystemExit("error: corpus missing; run scripts/qa/fetch-corpus.sh first")
    manifest = [json.loads(l) for l in manifest_path.read_text().splitlines() if l.strip()]
    manifest = manifest[: args.limit] if args.limit else manifest

    binary = handy_bin(args.app)
    env = environment(binary)
    results = []
    for m in args.models:
        r = bench_model(binary, m, args.repeat, manifest)
        r["environment"] = env
        (RESULTS / label(m) / "results.json").write_text(json.dumps(r, indent=1) + "\n")
        (RESULTS / label(m) / "summary.md").write_text(markdown(env, [r]))
        results.append(r)
    md = markdown(env, results)
    (RESULTS / "summary.md").write_text(md)
    print(md)
    return 0


if __name__ == "__main__":
    sys.exit(main())
