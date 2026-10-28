#!/usr/bin/env python3
"""Fetch a small, pinned LibriSpeech subset as 16 kHz mono 16-bit WAV + references.

Source: LibriSpeech ASR corpus (Panayotov et al., 2015), https://www.openslr.org/12,
licence CC BY 4.0. Audio goes under .qa-corpus/ (gitignored) and is never committed.

The test-clean / test-other tarballs (~350 MB each) are streamed and reading stops as
soon as every needed utterance is extracted. Progress is saved per utterance, failed
streams are retried, and complete subsets are skipped, so an interrupted run resumes.

Pinning: scripts/qa/corpus.lock lists `<subset> <utterance-id> <pcm-sha256>`, where
pcm-sha256 is the sha256 of the raw little-endian int16 samples. LibriSpeech is native
16 kHz 16-bit FLAC, so decoding is lossless and the hash is identical whether
afconvert or ffmpeg decoded it. Normal runs fetch only lock entries and fail on any
hash mismatch. `--regenerate-lock` re-selects by the rule below and rewrites the lock.

Selection rule (regenerate only): walk the archive in stored order, at most one chapter
per speaker; from each chapter take the first K utterances (sorted by id) whose
reference has MIN_WORDS..MAX_WORDS words, until N chapters are filled.

Dependencies: Python 3 stdlib plus macOS afconvert (or ffmpeg if afconvert is absent).
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import wave
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
LOCK_PATH = Path(__file__).resolve().parent / "corpus.lock"

# Official openslr mirrors, fastest first as measured from India on 2026-10-27.
MIRRORS = [
    "https://openslr.trmal.net/resources/12/{subset}.tar.gz",
    "https://www.openslr.org/resources/12/{subset}.tar.gz",
    "https://openslr.elda.org/resources/12/{subset}.tar.gz",
]
READ_TIMEOUT_S = 120
ATTEMPTS = 4

# subset -> (chapters, utterances per chapter)
SELECTION = {
    "test-clean": (10, 3),
    "test-other": (5, 2),
}
MIN_WORDS = 8
MAX_WORDS = 40

NETWORK_ERRORS = (OSError, EOFError, tarfile.ReadError, http.client.HTTPException)


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def pcm_sha256(wav: Path) -> str:
    with wave.open(str(wav), "rb") as r:
        return hashlib.sha256(r.readframes(r.getnframes())).hexdigest()


def duration_s(wav: Path) -> float:
    with wave.open(str(wav), "rb") as r:
        return r.getnframes() / r.getframerate()


def open_stream(subset: str, attempt: int):
    """Open the tarball, rotating the preferred mirror on each attempt."""
    order = MIRRORS[attempt % len(MIRRORS):] + MIRRORS[: attempt % len(MIRRORS)]
    errors = []
    for tmpl in order:
        url = tmpl.format(subset=subset)
        try:
            log(f"[fetch] streaming {url}")
            req = urllib.request.Request(url, headers={"User-Agent": "handy-qa-corpus/1"})
            return urllib.request.urlopen(req, timeout=READ_TIMEOUT_S)
        except NETWORK_ERRORS as e:
            log(f"[fetch] {url} failed: {e}")
            errors.append(f"{url}: {e}")
    raise OSError(f"all mirrors failed for {subset}: {errors}")


def parse_member(name: str):
    """LibriSpeech/<subset>/<spk>/<chap>/<file> -> (spk, chap, file) or None."""
    parts = name.split("/")
    return (parts[2], parts[3], parts[4]) if len(parts) == 5 else None


def stream_select(subset: str, attempt: int, wanted: set[str] | None, rule):
    """Yield (utt_id, flac_bytes, reference) in archive order.

    `wanted` pins exact ids; otherwise `rule` = (chapters, per_chapter) selects.
    Reading stops once the target is reached.
    """
    flacs: dict[str, dict[str, bytes]] = {}
    trans: dict[str, dict[str, str]] = {}
    done: set[str] = set()
    used_speakers: set[str] = set()
    remaining = set(wanted) if wanted is not None else None
    state = {"chapters_left": rule[0] if rule else 0, "bytes": 0}
    started = time.monotonic()

    resp = open_stream(subset, attempt)

    class Counting:
        def read(self, n=-1):
            b = resp.read(n)
            state["bytes"] += len(b)
            return b

    def finish_chapter(key: str):
        ids = sorted(trans[key])
        if remaining is not None:
            picked = [u for u in ids if u in remaining]
        elif key.split("-")[0] in used_speakers or state["chapters_left"] <= 0:
            picked = []
        else:
            ok = [u for u in ids if MIN_WORDS <= len(trans[key][u].split()) <= MAX_WORDS]
            picked = ok[: rule[1]] if len(ok) >= rule[1] else []
            if picked:
                used_speakers.add(key.split("-")[0])
                state["chapters_left"] -= 1
        for u in picked:
            yield u, flacs[key][u], trans[key][u]
            if remaining is not None:
                remaining.discard(u)
        done.add(key)
        flacs.pop(key, None)
        trans.pop(key, None)

    try:
        with tarfile.open(fileobj=Counting(), mode="r|gz") as tar:
            for m in tar:
                parsed = parse_member(m.name) if m.isfile() else None
                if not parsed:
                    continue
                spk, chap, fname = parsed
                key = f"{spk}-{chap}"
                if key in done:
                    continue
                if fname.endswith(".trans.txt"):
                    t = {}
                    for line in tar.extractfile(m).read().decode("utf-8").splitlines():
                        if line.strip():
                            uid, ref = line.split(" ", 1)
                            t[uid] = ref.strip()
                    trans[key] = t
                elif fname.endswith(".flac"):
                    uid = fname[: -len(".flac")]
                    if remaining is None or uid in remaining:
                        flacs.setdefault(key, {})[uid] = tar.extractfile(m).read()
                # A chapter is complete once its transcript and every needed FLAC are in.
                if key in trans:
                    need = trans[key].keys() if remaining is None else trans[key].keys() & remaining
                    if all(u in flacs.get(key, {}) for u in need):
                        yield from finish_chapter(key)
                if (remaining is not None and not remaining) or (
                    remaining is None and state["chapters_left"] <= 0
                ):
                    break
    finally:
        resp.close()
        log(f"[fetch] {subset}: read {state['bytes'] / 1e6:.1f} MB compressed"
            f" in {time.monotonic() - started:.0f} s")
    if remaining:
        raise SystemExit(f"error: {subset}: not found in archive: {sorted(remaining)}")
    if remaining is None and state["chapters_left"] > 0:
        raise SystemExit(f"error: {subset}: archive ended with {state['chapters_left']} chapters unfilled")


def flac_to_wav(flac: Path, wav: Path) -> None:
    tmp = wav.with_suffix(".tmp.wav")
    if shutil.which("afconvert"):
        cmd = ["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(flac), str(tmp)]
    elif shutil.which("ffmpeg"):
        cmd = ["ffmpeg", "-nostdin", "-loglevel", "error", "-y", "-i", str(flac),
               "-ac", "1", "-ar", "16000", "-sample_fmt", "s16", str(tmp)]
    else:
        raise SystemExit("error: need afconvert (macOS) or ffmpeg to decode FLAC")
    subprocess.run(cmd, check=True)
    # Rewrite with the stdlib so every file has a canonical 44-byte PCM header
    # (afconvert may add extra chunks) and check the format the CLI requires.
    with wave.open(str(tmp), "rb") as r:
        if (r.getframerate(), r.getnchannels(), r.getsampwidth()) != (16000, 1, 2):
            raise SystemExit(f"error: {tmp}: unexpected format {r.getparams()}")
        frames = r.readframes(r.getnframes())
    with wave.open(str(wav), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(frames)
    tmp.unlink()


def fetch_subset(subset: str, out: Path, pinned: dict[str, str] | None) -> list[str]:
    """Ensure the subset's WAVs and refs exist; return the selected ids in order."""
    wav_dir = out / subset
    wav_dir.mkdir(parents=True, exist_ok=True)
    refs_path = out / "refs" / f"{subset}.json"
    refs_path.parent.mkdir(parents=True, exist_ok=True)
    refs: dict[str, str] = json.loads(refs_path.read_text()) if refs_path.exists() else {}
    chapters, per_chapter = SELECTION[subset]

    def have(u: str) -> bool:
        return u in refs and (wav_dir / f"{u}.wav").exists()

    if pinned is not None:
        for u, sha in pinned.items():
            if have(u) and pcm_sha256(wav_dir / f"{u}.wav") != sha:
                log(f"[fetch] {u}: PCM hash mismatch on disk, refetching")
                (wav_dir / f"{u}.wav").unlink()
        missing = {u for u in pinned if not have(u)}
        if not missing:
            log(f"[fetch] {subset}: all {len(pinned)} pinned WAVs present, no download")
            return sorted(pinned)
    elif len(refs) == chapters * per_chapter and all(have(u) for u in refs):
        # A complete refs file is only written once the rule has filled every slot.
        log(f"[fetch] {subset}: {len(refs)} selected WAVs present, no download")
        return sorted(refs)
    else:
        refs = {}  # partial rule selections are recomputed from the stream

    selected: list[str] = []
    with tempfile.TemporaryDirectory() as td:
        for attempt in range(ATTEMPTS):
            wanted = None if pinned is None else {u for u in pinned if not have(u)}
            if wanted is not None and not wanted:
                break
            rule = None if pinned is not None else (chapters, per_chapter)
            selected = []
            try:
                for uid, flac_bytes, ref in stream_select(subset, attempt, wanted, rule):
                    selected.append(uid)
                    wav = wav_dir / f"{uid}.wav"
                    if not (wav.exists() and refs.get(uid) == ref):
                        flac = Path(td) / f"{uid}.flac"
                        flac.write_bytes(flac_bytes)
                        flac_to_wav(flac, wav)
                    if pinned is not None and pcm_sha256(wav) != pinned[uid]:
                        raise SystemExit(f"error: {uid}: PCM sha256 differs from corpus.lock")
                    refs[uid] = ref
                    refs_path.write_text(json.dumps(refs, indent=1, sort_keys=True) + "\n")
                break
            except NETWORK_ERRORS as e:
                log(f"[fetch] {subset}: attempt {attempt + 1}/{ATTEMPTS} failed: {e!r}")
                if attempt == ATTEMPTS - 1:
                    raise SystemExit(f"error: {subset}: giving up after {ATTEMPTS} attempts")
                time.sleep(5)
    if pinned is not None:
        return sorted(pinned)
    # Drop stale files from an earlier, different selection.
    for u in set(refs) - set(selected):
        refs.pop(u)
    refs_path.write_text(json.dumps(refs, indent=1, sort_keys=True) + "\n")
    return sorted(selected)


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--out", type=Path, default=REPO_ROOT / ".qa-corpus")
    ap.add_argument("--regenerate-lock", action="store_true",
                    help="re-select utterances by rule and rewrite scripts/qa/corpus.lock")
    args = ap.parse_args()
    out: Path = args.out.resolve()

    lock: dict[str, dict[str, str]] = {s: {} for s in SELECTION}
    if not args.regenerate_lock:
        if not LOCK_PATH.exists():
            raise SystemExit(f"error: {LOCK_PATH} missing; run with --regenerate-lock")
        for line in LOCK_PATH.read_text().splitlines():
            if line.strip() and not line.startswith("#"):
                subset, uid, sha = line.split()
                lock[subset][uid] = sha

    manifest = []
    lock_lines = []
    for subset in SELECTION:
        ids = fetch_subset(subset, out, None if args.regenerate_lock else lock[subset])
        refs = json.loads((out / "refs" / f"{subset}.json").read_text())
        for u in ids:
            wav = out / subset / f"{u}.wav"
            manifest.append({
                "id": u,
                "subset": subset,
                "wav": str(wav.relative_to(out)),
                "duration_s": round(duration_s(wav), 3),
                "reference": refs[u],
            })
            lock_lines.append(f"{subset} {u} {pcm_sha256(wav)}")

    with (out / "manifest.jsonl").open("w") as f:
        for e in manifest:
            f.write(json.dumps(e, sort_keys=True) + "\n")

    if args.regenerate_lock:
        header = [
            "# Pinned LibriSpeech utterances for the QA benchmark (CC BY 4.0, openslr.org/12).",
            "# <subset> <utterance-id> <sha256 of decoded int16 PCM>.",
            "# Regenerate: scripts/qa/fetch-corpus.sh --regenerate-lock",
        ]
        LOCK_PATH.write_text("\n".join(header + lock_lines) + "\n")
        log(f"[fetch] wrote {LOCK_PATH}")

    total = sum(e["duration_s"] for e in manifest)
    log(f"[fetch] {len(manifest)} utterances, {total:.1f} s audio -> {out / 'manifest.jsonl'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
