//! Systemic 4-layer accent and multilingual ASR reliability primitives (`SV-99`).
//!
//! 1. **Layer 1 (`mask_interior_silence_collars`)**: Pre-ASR silence-collar masking
//!    that attenuates `>= 350 ms` low-energy spans (`20 ms` frame RMS `< 0.0045`) to a
//!    `-48 dBFS` comfort gain floor (`0.004`, never dead-zero digital silence) with
//!    `20 ms` (`320`-sample) raised-cosine collars and `30 ms` (`480`-sample) speech
//!    onset/coda safety margins.
//! 2. **Layer 2 (`stitch_overlapping_transcripts`)**: `LocalAgreement`-style boundary
//!    stitcher deduplicating `2..=12`-word overlaps, `>= 5`-char single-word overlaps,
//!    and `>= 4`-char split prefix fragments across sliding-window audio cuts while
//!    preserving unspaced CJK/Thai script joins.
//! 3. **Layer 3 (`inspect_compression_and_confidence`)**: Pure-Rust UTF-8 character
//!    4-gram and token-diversity compression gate combined with decoder confidence
//!    (`avg_token_prob`, `no_speech_prob`) that rejects pathological hallucination
//!    loops in any script while preserving genuine 1–3 word bilingual code-switches.
//! 4. **Layer 4 (`accent_phonetic_skeleton`, `accent_phonetic_similarity`,
//!    `is_accent_phonetic_match`)**: Global accent phonetic skeleton normalizing
//!    South Asian, Middle Eastern, Hispanic, and European consonant/vowel shifts
//!    with common-English-word immunity.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

/// Canonical ASR sample rate (`16 kHz` mono PCM).
pub const SAMPLE_RATE_16K: usize = 16_000;

/// Duration of one RMS analysis frame and raised-cosine fade collar (`20 ms` = `320` samples).
pub const SILENCE_COLLAR_FRAME_SAMPLES: usize = 20 * SAMPLE_RATE_16K / 1000;

/// Minimum contiguous low-energy span before silence-collar masking engages (`350 ms` = `5,600` samples).
pub const SILENCE_COLLAR_MIN_SPAN_SAMPLES: usize = 350 * SAMPLE_RATE_16K / 1000;

/// Untouched safety margin preserved adjacent to speech onsets and codas (`30 ms` = `480` samples).
pub const SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES: usize = 30 * SAMPLE_RATE_16K / 1000;

/// 20 ms frame RMS threshold below which audio is classified as non-speech (`0.0045`).
pub const SILENCE_COLLAR_LOW_RMS_THRESHOLD: f32 = 0.0045;

/// Gentle `-48 dBFS` comfort gain floor (`0.004`) applied inside masked pauses so
/// Whisper's mel-energy floor never hits dead-zero digital silence.
pub const SILENCE_COLLAR_FLOOR_GAIN: f32 = 0.004;

/// Sub-audible comfort dither amplitude (`-100 dBFS`) injected only when an interior
/// pause between speech segments contains literal `0.0` hardware-gated samples.
const ZERO_PAUSE_COMFORT_DITHER: f32 = 1.0e-5;

/// Attenuates contiguous `>= 350 ms` low-energy (`20 ms` frame RMS `< 0.0045`) spans
/// Returns `true` when an audio buffer contains only room tone, mic hiss, or a soft
/// breath puff (`overall RMS < 0.0045` and `peak 20 ms window RMS < 0.0090`) with no
/// voiced human speech—preserving `-12 dB` distant whispers (`peak 20 ms >= 0.011`)
/// while preventing Whisper/Parakeet neural decoders from hallucinating on silence.
pub fn is_non_speech_energy_floor(samples: &[f32], sample_rate_hz: u32) -> bool {
    if samples.is_empty() {
        return true;
    }
    let frame_samples = ((sample_rate_hz as usize) / 50).max(1);
    let mut total_sq = 0.0_f64;
    let mut total_n = 0usize;
    let mut max_frame_rms = 0.0_f32;
    for chunk in samples.chunks(frame_samples) {
        let mut frame_sq = 0.0_f64;
        let mut frame_n = 0usize;
        for &s in chunk {
            if s.is_finite() {
                let v = f64::from(s);
                frame_sq += v * v;
                frame_n += 1;
            }
        }
        total_sq += frame_sq;
        total_n += frame_n;
        if frame_n >= frame_samples / 2 {
            let f_rms = (frame_sq / frame_n as f64).sqrt() as f32;
            if f_rms > max_frame_rms {
                max_frame_rms = f_rms;
            }
        }
    }
    if total_n == 0 {
        return true;
    }
    let overall_rms = (total_sq / total_n as f64).sqrt() as f32;
    overall_rms < 0.0045 && max_frame_rms < 0.0090
}

/// Smoothly attenuates low-energy silence/breath spans (`>= 220 ms` with `RMS < 0.0075`)
/// in 16 kHz mono PCM to a `-48 dBFS` comfort floor (`gain = 0.004`) using `20 ms`
/// (`320`-sample) raised-cosine fade collars while leaving `30 ms` (`480` samples) on
/// each side of speech 100% untouched.
///
/// Returns the number of low-energy spans masked.
pub fn mask_interior_silence_collars(samples: &mut [f32]) -> usize {
    let len = samples.len();
    if len < SILENCE_COLLAR_MIN_SPAN_SAMPLES {
        return 0;
    }

    let mut prefix_sq = Vec::with_capacity(len + 1);
    prefix_sq.push(0.0_f64);
    let mut sum_sq = 0.0_f64;
    for &sample in samples.iter() {
        let val = if sample.is_finite() {
            f64::from(sample)
        } else {
            0.0
        };
        sum_sq += val * val;
        prefix_sq.push(sum_sq);
    }

    let max_frame_sq_sum = (SILENCE_COLLAR_FRAME_SAMPLES as f64)
        * f64::from(SILENCE_COLLAR_LOW_RMS_THRESHOLD)
        * f64::from(SILENCE_COLLAR_LOW_RMS_THRESHOLD);

    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    for win_start in 0..=(len - SILENCE_COLLAR_FRAME_SAMPLES) {
        let win_end = win_start + SILENCE_COLLAR_FRAME_SAMPLES;
        let frame_sq = prefix_sq[win_end] - prefix_sq[win_start];
        if frame_sq < max_frame_sq_sum {
            match &mut current {
                Some((_, end)) if win_start <= *end => {
                    *end = win_end;
                }
                Some((start, end)) => {
                    if *end - *start >= SILENCE_COLLAR_MIN_SPAN_SAMPLES {
                        spans.push((*start, *end));
                    }
                    current = Some((win_start, win_end));
                }
                None => {
                    current = Some((win_start, win_end));
                }
            }
        }
    }
    if let Some((start, end)) = current {
        if end - start >= SILENCE_COLLAR_MIN_SPAN_SAMPLES {
            spans.push((start, end));
        }
    }

    let mut masked_count = 0usize;
    for (span_start, span_end) in spans {
        let has_left_speech = span_start > 0;
        let has_right_speech = span_end < len;
        let has_any_speech = has_left_speech || has_right_speech;

        let (fade_out_start, interior_start) = if has_left_speech {
            let fo_start = span_start + SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES;
            (fo_start, fo_start + SILENCE_COLLAR_FRAME_SAMPLES)
        } else {
            (span_start, span_start)
        };

        let interior_end = if has_right_speech {
            span_end
                .saturating_sub(SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES)
                .saturating_sub(SILENCE_COLLAR_FRAME_SAMPLES)
        } else {
            span_end
        };

        if interior_start > interior_end {
            continue;
        }

        // 20 ms (320-sample) raised-cosine fade-out from 1.0 down to SILENCE_COLLAR_FLOOR_GAIN.
        if has_left_speech {
            for i in 0..SILENCE_COLLAR_FRAME_SAMPLES {
                let idx = fade_out_start + i;
                let phase = std::f32::consts::PI * ((i + 1) as f32)
                    / ((SILENCE_COLLAR_FRAME_SAMPLES + 1) as f32);
                let weight = 0.5 * (1.0 + phase.cos());
                let gain = SILENCE_COLLAR_FLOOR_GAIN + (1.0 - SILENCE_COLLAR_FLOOR_GAIN) * weight;
                samples[idx] = apply_comfort_gain(samples[idx], gain, weight, idx, has_any_speech);
            }
        }

        // Interior gentle -48 dBFS comfort floor (gain = 0.004, never dead-zero digital silence).
        for (offset, sample) in samples[interior_start..interior_end].iter_mut().enumerate() {
            let idx = interior_start + offset;
            *sample =
                apply_comfort_gain(*sample, SILENCE_COLLAR_FLOOR_GAIN, 0.0, idx, has_any_speech);
        }

        // 20 ms (320-sample) raised-cosine fade-in from SILENCE_COLLAR_FLOOR_GAIN back to 1.0.
        if has_right_speech {
            for i in 0..SILENCE_COLLAR_FRAME_SAMPLES {
                let idx = interior_end + i;
                let phase = std::f32::consts::PI * ((i + 1) as f32)
                    / ((SILENCE_COLLAR_FRAME_SAMPLES + 1) as f32);
                let weight = 0.5 * (1.0 - phase.cos());
                let gain = SILENCE_COLLAR_FLOOR_GAIN + (1.0 - SILENCE_COLLAR_FLOOR_GAIN) * weight;
                samples[idx] = apply_comfort_gain(samples[idx], gain, weight, idx, has_any_speech);
            }
        }

        masked_count += 1;
    }

    masked_count
}

#[inline]
fn apply_comfort_gain(
    sample: f32,
    gain: f32,
    speech_weight: f32,
    sample_index: usize,
    has_any_speech: bool,
) -> f32 {
    if !sample.is_finite() || sample == 0.0 {
        if has_any_speech {
            let sign = if sample_index.is_multiple_of(2) {
                1.0_f32
            } else {
                -1.0_f32
            };
            sign * ZERO_PAUSE_COMFORT_DITHER * (1.0 - speech_weight)
        } else {
            0.0
        }
    } else {
        sample * gain
    }
}

/// Scripts written without spaces between words, plus fullwidth CJK punctuation.
static UNSPACED_SCRIPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"[\p{Han}\p{Hiragana}\p{Katakana}\p{Bopomofo}\p{Thai}\p{Lao}\p{Khmer}\p{Myanmar}\x{3000}-\x{303f}\x{ff00}-\x{ffef}]",
    )
    .expect("valid built-in unspaced script regex")
});

fn is_unspaced_char(c: char) -> bool {
    UNSPACED_SCRIPT.is_match(c.encode_utf8(&mut [0; 4]))
}

fn is_combining_word_mark(c: char) -> bool {
    let code = c as u32;
    matches!(
        code,
        0x0300..=0x036F
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x0900..=0x0903
            | 0x093A..=0x094F
            | 0x0951..=0x0957
            | 0x0962..=0x0963
    )
}

fn normalize_stitch_token(token: &str) -> String {
    token
        .chars()
        .filter(|&c| c.is_alphanumeric() || is_combining_word_mark(c))
        .flat_map(char::to_lowercase)
        .collect()
}

fn token_word_char_count(normalized: &str) -> usize {
    normalized.chars().filter(|c| c.is_alphanumeric()).count()
}

const REPEATABLE_SINGLE_WORDS: &[&str] = &["hello", "sorry", "really", "never", "please", "again"];

const NON_PREFIX_STOPWORDS: &[&str] = &[
    "with", "some", "what", "when", "where", "there", "here", "from", "that", "your", "them",
    "they", "then", "than", "have", "will", "were", "been", "good", "well", "just", "very", "more",
    "most", "each", "much", "such", "only", "also", "over", "into", "onto",
];

#[derive(Clone, Copy)]
struct TokenSlice<'a> {
    raw: &'a str,
    start: usize,
    end: usize,
}

fn whitespace_token_slices(text: &str) -> Vec<TokenSlice<'_>> {
    let mut out = Vec::new();
    let mut byte_idx = 0usize;
    while byte_idx < text.len() {
        let rest = &text[byte_idx..];
        let trimmed = rest.trim_start();
        let ws_len = rest.len() - trimmed.len();
        byte_idx += ws_len;
        if byte_idx >= text.len() {
            break;
        }
        let token_len = text[byte_idx..]
            .find(char::is_whitespace)
            .unwrap_or(text.len() - byte_idx);
        out.push(TokenSlice {
            raw: &text[byte_idx..byte_idx + token_len],
            start: byte_idx,
            end: byte_idx + token_len,
        });
        byte_idx += token_len;
    }
    out
}

fn is_trailing_cut_punct(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '…' | '۔' | '،' | '।')
}

fn ends_with_hyphen(raw: &str) -> bool {
    raw.ends_with(['-', '‐', '‑', '–', '—'])
}

fn lowercase_first_alpha(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut lowered = false;
    for ch in word.chars() {
        if !lowered && ch.is_alphabetic() {
            out.extend(ch.to_lowercase());
            lowered = true;
        } else {
            out.push(ch);
        }
    }
    out
}

fn is_unspaced_overlap_punct(c: char) -> bool {
    matches!(
        c,
        '。' | '，' | '、' | '！' | '？' | '；' | '：' | '.' | ',' | '!' | '?' | ';' | ':' | '…'
    )
}

fn append_unspaced_with_overlap_dedup(joined: &mut String, chunk: &str, max_chars: usize) {
    // Strip trailing sentence-cut punctuation on `joined` only while comparing
    // normalized suffix-prefix character runs across the overlapping CJK/Thai boundary.
    let joined_core = joined.trim_end_matches(is_unspaced_overlap_punct);
    let chunk_core = chunk.trim_start_matches(is_unspaced_overlap_punct);
    let prev_chars: Vec<(usize, char)> = joined_core.char_indices().collect();
    let next_chars: Vec<(usize, char)> = chunk_core.char_indices().collect();
    let max_k = prev_chars.len().min(next_chars.len()).min(max_chars);

    for k in (2..=max_k).rev() {
        let p_slice = &prev_chars[prev_chars.len() - k..];
        let n_slice = &next_chars[..k];
        let same = p_slice
            .iter()
            .zip(n_slice.iter())
            .all(|((_, a), (_, b))| a == b && (a.is_alphanumeric() || is_unspaced_char(*a)));
        if same {
            joined.truncate(joined_core.len());
            if k < next_chars.len() {
                let rem_byte = next_chars[k].0;
                joined.push_str(&chunk_core[rem_byte..]);
            }
            return;
        }
    }

    joined.push_str(chunk);
}

/// Joins overlapping chunk transcripts in order using `LocalAgreement` suffix-prefix
/// overlap deduplication while preserving unspaced CJK/Thai joining semantics.
///
/// Assumes every chunk after the first came from a sliding-overlap boundary. When
/// per-chunk overlap metadata is available, prefer [`stitch_transcript_boundaries`]
/// so clean pause cuts never deduplicate intentional repeated words across pauses.
pub fn stitch_overlapping_transcripts<S: AsRef<str>>(texts: &[S]) -> String {
    let flagged: Vec<(&str, bool)> = texts
        .iter()
        .enumerate()
        .map(|(idx, s)| (s.as_ref(), idx > 0))
        .collect();
    stitch_transcript_boundaries(&flagged)
}

/// Joins chunk transcripts in order, running suffix-prefix overlap deduplication
/// **only** on boundaries where `overlaps_with_prev == true` (sliding-window cuts)
/// while concatenating clean silence-cut boundaries verbatim:
///
/// - Clean pause boundaries (`overlaps_with_prev == false`) preserve legitimate
///   repeated words spoken after a pause (e.g. `"Wait."` + `"Wait, let me check"`).
/// - Sliding-overlap boundaries (`overlaps_with_prev == true`) deduplicate:
///   - `2..=16` character suffix-prefix overlaps on unspaced CJK/Thai scripts,
///   - `2..=12` word case/punctuation-insensitive overlaps on spaced scripts,
///   - `1`-word boundary overlaps (`>= 5` chars, non-reduplicative), and
///   - `>= 4`-char truncated chunk-end prefix fragments (`"trans-"` + `"transcription"`).
pub fn stitch_transcript_boundaries<S: AsRef<str>>(segments: &[(S, bool)]) -> String {
    const MAX_OVERLAP_WORDS: usize = 12;
    const MAX_UNSPACED_OVERLAP_CHARS: usize = 16;

    let mut joined = String::new();
    let mut pending_overlap = false;
    for (raw_chunk, overlaps_with_prev) in segments {
        let chunk = raw_chunk.as_ref().trim();
        let Some(first_char) = chunk.chars().next() else {
            // If an overlapping chunk decoded to empty, carry its overlap flag
            // only if nothing joined yet; otherwise reset cleanly.
            pending_overlap = false;
            continue;
        };
        if joined.is_empty() {
            joined.push_str(chunk);
            pending_overlap = false;
            continue;
        }
        let has_overlap = *overlaps_with_prev || pending_overlap;
        pending_overlap = false;

        let last_char = joined.chars().next_back();
        if last_char.is_some_and(is_unspaced_char) || is_unspaced_char(first_char) {
            if has_overlap {
                append_unspaced_with_overlap_dedup(&mut joined, chunk, MAX_UNSPACED_OVERLAP_CHARS);
            } else {
                joined.push_str(chunk);
            }
            continue;
        }

        if !has_overlap {
            joined.push(' ');
            joined.push_str(chunk);
            continue;
        }

        let prev_tokens = whitespace_token_slices(&joined);
        let next_tokens = whitespace_token_slices(chunk);
        if prev_tokens.is_empty() || next_tokens.is_empty() {
            joined.push(' ');
            joined.push_str(chunk);
            continue;
        }

        let tail_count = prev_tokens.len().min(MAX_OVERLAP_WORDS);
        let head_count = next_tokens.len().min(MAX_OVERLAP_WORDS);
        let prev_tail = &prev_tokens[prev_tokens.len() - tail_count..];
        let next_head = &next_tokens[..head_count];

        let prev_norm: Vec<String> = prev_tail
            .iter()
            .map(|t| normalize_stitch_token(t.raw))
            .collect();
        let next_norm: Vec<String> = next_head
            .iter()
            .map(|t| normalize_stitch_token(t.raw))
            .collect();

        let max_k = tail_count.min(head_count);
        let mut matched_k: Option<usize> = None;

        // 1. Check 2..=12-word suffix-prefix exact normalized overlap (longest first).
        if max_k >= 2 {
            for k in (2..=max_k).rev() {
                let p_slice = &prev_norm[tail_count - k..];
                let n_slice = &next_norm[..k];
                if p_slice.iter().all(|w| !w.is_empty()) && p_slice == n_slice {
                    matched_k = Some(k);
                    break;
                }
            }
        }

        // 2. Check 1-word exact normalized overlap (>= 5 alphanumeric chars, non-reduplicative).
        if matched_k.is_none() {
            let last_p = &prev_norm[tail_count - 1];
            let first_n = &next_norm[0];
            if !last_p.is_empty()
                && last_p == first_n
                && token_word_char_count(last_p) >= 5
                && !REPEATABLE_SINGLE_WORDS.contains(&last_p.as_str())
            {
                matched_k = Some(1);
            }
        }

        if let Some(k) = matched_k {
            if k < next_tokens.len() {
                let prev_last_raw = prev_tail[tail_count - 1].raw;
                let next_end_raw = next_tokens[k - 1].raw;
                let prev_had_cut_punct = prev_last_raw
                    .chars()
                    .next_back()
                    .is_some_and(is_trailing_cut_punct);
                let next_has_punct = next_end_raw
                    .chars()
                    .next_back()
                    .is_some_and(is_trailing_cut_punct);
                if prev_had_cut_punct && !next_has_punct {
                    while joined
                        .chars()
                        .next_back()
                        .is_some_and(is_trailing_cut_punct)
                    {
                        joined.pop();
                    }
                }
                let remainder = chunk[next_tokens[k].start..].trim_start();
                if !remainder.is_empty() {
                    joined.push(' ');
                    joined.push_str(remainder);
                }
            }
            continue;
        }

        // 3. Check truncated prefix fragment at end of previous chunk (e.g. "trans-" / "trans" + "transcription").
        let last_p = &prev_norm[tail_count - 1];
        let first_n = &next_norm[0];
        let last_raw = prev_tail[tail_count - 1].raw;
        let prefix_len = token_word_char_count(last_p);
        let full_len = token_word_char_count(first_n);
        if prefix_len >= 4
            && full_len > prefix_len
            && first_n.starts_with(last_p.as_str())
            && (ends_with_hyphen(last_raw) || !NON_PREFIX_STOPWORDS.contains(&last_p.as_str()))
        {
            let fragment_started_lower = last_raw
                .chars()
                .find(|c| c.is_alphabetic())
                .is_some_and(char::is_lowercase);
            let cut_at = prev_tail[tail_count - 1].start;
            joined.truncate(cut_at);
            let trimmed_len = joined.trim_end().len();
            joined.truncate(trimmed_len);

            if !joined.is_empty() {
                joined.push(' ');
            }
            if fragment_started_lower {
                let adjusted_first = lowercase_first_alpha(next_tokens[0].raw);
                joined.push_str(&adjusted_first);
                let rest = &chunk[next_tokens[0].end..];
                joined.push_str(rest);
            } else {
                joined.push_str(chunk);
            }
            continue;
        }

        joined.push(' ');
        joined.push_str(chunk);
    }

    joined
}

/// Specific condition that triggered [`inspect_compression_and_confidence`] rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressionGateReason {
    /// Character 4-gram / token repetition compression ratio exceeded `2.8` on `>= 32` chars.
    HighCompressionRatio,
    /// Pathological single-token or exact phrase tiling loop in any script.
    PathologicalTokenLoop,
    /// Elevated repetition combined with low token confidence or high no-speech probability.
    LowConfidenceRepetition,
    /// Simultaneous extreme `no_speech_prob` (`> 0.80`) and abyssal `avg_token_prob` (`< 0.20`).
    ExtremeLowConfidence,
}

/// Result of inspecting a decoded transcript with [`inspect_compression_and_confidence`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompressionConfidenceVerdict {
    /// Repetition compression ratio (`>= 1.0`; higher values indicate lower entropy / loops).
    pub compression_ratio: f32,
    /// Whether the transcript should be dropped as a decoder hallucination.
    pub rejected: bool,
    /// Diagnostic reason when `rejected` is `true`.
    pub reason: Option<CompressionGateReason>,
}

/// Evaluates UTF-8 character 4-gram and token-diversity compression alongside optional
/// decoder confidence (`avg_token_prob`, `no_speech_prob`) to reject pathological
/// Whisper repetition loops in any script while preserving genuine 1–3 word bilingual
/// code-switches.
pub fn inspect_compression_and_confidence(
    text: &str,
    avg_token_prob: Option<f32>,
    no_speech_prob: Option<f32>,
) -> CompressionConfidenceVerdict {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return CompressionConfidenceVerdict {
            compression_ratio: 1.0,
            rejected: false,
            reason: None,
        };
    }

    // Collapse internal whitespace runs and lowercase into a normalized char vector.
    let mut norm_chars: Vec<char> = Vec::with_capacity(trimmed.len());
    let mut prev_ws = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                norm_chars.push(' ');
                prev_ws = true;
            }
        } else {
            norm_chars.extend(ch.to_lowercase());
            prev_ws = false;
        }
    }

    let char_count = norm_chars.len();
    let ngram4_ratio = if char_count >= 4 {
        let total = char_count - 3;
        let mut unique: HashSet<[char; 4]> = HashSet::with_capacity(total);
        for win in norm_chars.windows(4) {
            unique.insert([win[0], win[1], win[2], win[3]]);
        }
        total as f32 / unique.len().max(1) as f32
    } else {
        1.0
    };

    let words: Vec<String> = trimmed
        .split_whitespace()
        .map(normalize_stitch_token)
        .filter(|w| !w.is_empty())
        .collect();
    let word_count = words.len();
    let unique_words: HashSet<&str> = words.iter().map(String::as_str).collect();
    let unique_word_count = unique_words.len();

    let mut max_consecutive_run = 0usize;
    let mut current_run = 0usize;
    for idx in 0..word_count {
        if idx > 0 && words[idx] == words[idx - 1] {
            current_run += 1;
        } else {
            current_run = 1;
        }
        max_consecutive_run = max_consecutive_run.max(current_run);
    }

    let word_ratio = if word_count >= 4 {
        word_count as f32 / unique_word_count.max(1) as f32
    } else {
        1.0
    };
    let compression_ratio = ngram4_ratio.max(word_ratio);

    // Simultaneous extreme non-speech probability and near-zero token confidence.
    if no_speech_prob.is_some_and(|nsp| nsp > 0.80) && avg_token_prob.is_some_and(|tp| tp < 0.20) {
        return CompressionConfidenceVerdict {
            compression_ratio,
            rejected: true,
            reason: Some(CompressionGateReason::ExtremeLowConfidence),
        };
    }

    // Character/4-gram compression ratio > 2.8 on >= 32 chars catches loops in any script.
    if char_count >= 32 && ngram4_ratio > 2.8 {
        return CompressionConfidenceVerdict {
            compression_ratio,
            rejected: true,
            reason: Some(CompressionGateReason::HighCompressionRatio),
        };
    }

    // Pathological single-word loop (e.g. "اور اور اور اور اور") or exact phrase tiling (>= 3x).
    let is_single_token_loop = word_count >= 4 && unique_word_count == 1;
    let is_exact_phrase_tile = (1..=6).any(|k| {
        word_count >= k * 3
            && word_count.is_multiple_of(k)
            && words.chunks(k).all(|chunk| chunk == &words[..k])
    });
    let is_severe_word_loop = (word_count >= 6 && word_ratio > 2.8)
        || (max_consecutive_run >= 5 && unique_word_count * 3 <= word_count);

    if is_single_token_loop || is_exact_phrase_tile || is_severe_word_loop {
        return CompressionConfidenceVerdict {
            compression_ratio,
            rejected: true,
            reason: Some(CompressionGateReason::PathologicalTokenLoop),
        };
    }

    // Confidence-assisted gate: moderate repetition paired with weak acoustic confidence.
    let weak_confidence =
        avg_token_prob.is_some_and(|tp| tp < 0.35) || no_speech_prob.is_some_and(|nsp| nsp > 0.70);
    if weak_confidence
        && ((char_count >= 20 && compression_ratio > 2.0)
            || (word_count >= 4 && unique_word_count * 2 <= word_count)
            || max_consecutive_run >= 3)
    {
        return CompressionConfidenceVerdict {
            compression_ratio,
            rejected: true,
            reason: Some(CompressionGateReason::LowConfidenceRepetition),
        };
    }

    CompressionConfidenceVerdict {
        compression_ratio,
        rejected: false,
        reason: None,
    }
}

/// Common English vocabulary immune from fuzzy phonetic brand/lexicon snapping
/// unless the user's custom word is an exact case-insensitive match.
const IMMUNE_COMMON_ENGLISH_WORDS: &[&str] = &[
    "transcription",
    "translation",
    "production",
    "function",
    "application",
    "information",
    "connection",
    "question",
    "location",
    "action",
    "version",
    "session",
    "section",
    "attention",
    "condition",
    "direction",
    "discussion",
    "education",
    "exception",
    "execution",
    "expression",
    "foundation",
    "generation",
    "implement",
    "implementation",
    "instruction",
    "integration",
    "interaction",
    "migration",
    "navigation",
    "notification",
    "operation",
    "organization",
    "permission",
    "collection",
    "position",
    "prediction",
    "presentation",
    "protection",
    "reaction",
    "reduction",
    "reflection",
    "relation",
    "resolution",
    "restriction",
    "selection",
    "situation",
    "solution",
    "submission",
    "suggestion",
    "transition",
    "validation",
    "variation",
];

/// Returns `true` when `word` is a common standard English word protected against
/// false-positive phonetic lexicon snapping.
pub fn is_immune_common_english_word(word: &str) -> bool {
    let cleaned: String = word
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    IMMUNE_COMMON_ENGLISH_WORDS.contains(&cleaned.as_str())
}

fn fold_latin_char(ch: char) -> Option<char> {
    if ch.is_ascii_alphanumeric() {
        return Some(ch.to_ascii_lowercase());
    }
    match ch {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => {
            Some('a')
        }
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => Some('e'),
        'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => Some('i'),
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => Some('o'),
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => Some('u'),
        'ý' | 'ÿ' | 'Ý' | 'Ÿ' => Some('y'),
        'ñ' | 'Ñ' => Some('n'),
        'ç' | 'Ç' => Some('s'),
        _ => None,
    }
}

fn is_ascii_vowel_or_y(ch: char) -> bool {
    matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

/// Computes a global accent phonetic skeleton normalizing common South Asian,
/// Middle Eastern, Hispanic, and European phonetic shifts:
///
/// - Aspirated & fricative digraphs: `ph -> f`, `gh -> g`, `kh -> k`, `th -> t`,
///   `dh -> d`, `bh -> b`, `sh -> s`
/// - Labial-velar & velar shifts: `wh/w -> v`, `qu/q/ck -> k`,
///   `c` before `e/i/y -> s` else `k`, `z/x -> s`
/// - Vowel runs `[aeiouy]+ -> a` (preserving leading vowel onset vs consonant onset)
/// - Adjacent duplicate consonants collapsed (`ff -> f`, `ll -> l`, `rr -> r`, `tt -> t`, `pp -> p`)
pub fn accent_phonetic_skeleton(word: &str) -> String {
    let chars: Vec<char> = word.chars().filter_map(fold_latin_char).collect();
    if chars.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(chars.len());
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();

        let (mapped, step) = match (ch, next) {
            ('p', Some('h')) => ('f', 2),
            ('g', Some('h')) => ('g', 2),
            ('k', Some('h')) => ('k', 2),
            ('t', Some('h')) => ('t', 2),
            ('d', Some('h')) => ('d', 2),
            ('b', Some('h')) => ('b', 2),
            ('s', Some('h')) => ('s', 2),
            ('w', Some('h')) => ('v', 2),
            ('q', Some('u')) => ('k', 2),
            ('c', Some('k')) => ('k', 2),
            ('c', Some('h')) => ('c', 2),
            ('w', _) => ('v', 1),
            ('q', _) => ('k', 1),
            ('c', Some('e' | 'i' | 'y')) => ('s', 1),
            ('c', _) => ('k', 1),
            ('z' | 'x', _) => ('s', 1),
            (v, _) if is_ascii_vowel_or_y(v) => ('a', 1),
            (other, _) => (other, 1),
        };

        if mapped == 'a' {
            if !out.ends_with('a') {
                out.push('a');
            }
        } else if mapped.is_ascii_alphabetic() {
            if !out.ends_with(mapped) {
                out.push(mapped);
            }
        } else {
            out.push(mapped);
        }

        i += step;
    }

    out
}

fn char_levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let m = b_chars.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Returns normalized edit similarity in `[0.0, 1.0]` between the global accent
/// phonetic skeletons of `a` and `b` (`0.0` when either has an empty skeleton).
pub fn accent_phonetic_similarity(a: &str, b: &str) -> f64 {
    let skel_a = accent_phonetic_skeleton(a);
    let skel_b = accent_phonetic_skeleton(b);
    if skel_a.is_empty() || skel_b.is_empty() {
        return 0.0;
    }
    if skel_a == skel_b {
        return 1.0;
    }
    let len_a = skel_a.chars().count();
    let len_b = skel_b.chars().count();
    let max_len = len_a.max(len_b);
    let dist = char_levenshtein(&skel_a, &skel_b);
    (1.0 - (dist as f64 / max_len as f64)).clamp(0.0, 1.0)
}

/// Returns `true` when `candidate` is an accent-phonetic match for `target`:
///
/// - Both normalize to purely alphabetic words of `>= 5` characters within `25%`
///   length tolerance (minimum 2 chars).
/// - `candidate` is not in [`IMMUNE_COMMON_ENGLISH_WORDS`] (unless identical to `target`).
/// - Normalized surface Levenshtein distance is `<= 0.42`.
/// - `accent_phonetic_similarity(candidate, target) >= 0.84` or exact skeleton match.
pub fn is_accent_phonetic_match(candidate: &str, target: &str) -> bool {
    let cand_clean: String = candidate.chars().filter_map(fold_latin_char).collect();
    let targ_clean: String = target.chars().filter_map(fold_latin_char).collect();

    if cand_clean.is_empty()
        || targ_clean.is_empty()
        || !cand_clean.chars().all(|c| c.is_ascii_alphabetic())
        || !targ_clean.chars().all(|c| c.is_ascii_alphabetic())
    {
        return false;
    }

    let cand_len = cand_clean.chars().count();
    let targ_len = targ_clean.chars().count();
    if cand_len < 5 || targ_len < 5 {
        return false;
    }

    if cand_clean != targ_clean && is_immune_common_english_word(&cand_clean) {
        return false;
    }

    let max_len = cand_len.max(targ_len) as f64;
    let len_diff = cand_len.abs_diff(targ_len) as f64;
    if len_diff > (max_len * 0.25).max(2.0) {
        return false;
    }

    let surface_dist = char_levenshtein(&cand_clean, &targ_clean) as f64 / max_len;
    if surface_dist > 0.42 {
        return false;
    }

    let skel_cand = accent_phonetic_skeleton(&cand_clean);
    let skel_targ = accent_phonetic_skeleton(&targ_clean);
    (!skel_cand.is_empty() && skel_cand == skel_targ)
        || accent_phonetic_similarity(&cand_clean, &targ_clean) >= 0.84
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_collar_masks_long_interior_pauses_and_preserves_speech_margins() {
        let speech_a = vec![0.18_f32; SAMPLE_RATE_16K]; // 1.0s active speech
        let pause = vec![0.002_f32; 8_000]; // 500ms low-energy pause (< 0.0045 RMS)
        let speech_b = vec![-0.15_f32; SAMPLE_RATE_16K]; // 1.0s active speech

        let mut pcm = Vec::with_capacity(speech_a.len() + pause.len() + speech_b.len());
        pcm.extend_from_slice(&speech_a);
        pcm.extend_from_slice(&pause);
        pcm.extend_from_slice(&speech_b);
        let original = pcm.clone();

        let spans = mask_interior_silence_collars(&mut pcm);
        assert_eq!(spans, 1);

        // Speech segments and 30 ms (480-sample) onset/coda margins are bit-exact untouched.
        let pause_start = SAMPLE_RATE_16K;
        let pause_end = SAMPLE_RATE_16K + 8_000;
        assert_eq!(
            &pcm[..pause_start + SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES],
            &original[..pause_start + SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES]
        );
        assert_eq!(
            &pcm[pause_end - SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES..],
            &original[pause_end - SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES..]
        );

        // Interior is smoothly attenuated by gain = 0.004 (-48 dBFS floor) and never dead-zero.
        let interior_start =
            pause_start + SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES + SILENCE_COLLAR_FRAME_SAMPLES;
        let interior_end =
            pause_end - SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES - SILENCE_COLLAR_FRAME_SAMPLES;
        for &s in &pcm[interior_start..interior_end] {
            assert!(s != 0.0, "masked interior must never be dead-zero");
            assert!((s - 0.002 * SILENCE_COLLAR_FLOOR_GAIN).abs() < 1e-7);
        }

        // Fade-out collar is strictly monotonically decreasing; short (< 350 ms) pauses stay untouched.
        let fade_out = &pcm[pause_start + SILENCE_COLLAR_SAFETY_MARGIN_SAMPLES..interior_start];
        assert!(fade_out.windows(2).all(|w| w[0] > w[1]));

        let mut short_pause_pcm = Vec::new();
        short_pause_pcm.extend(std::iter::repeat_n(0.18_f32, 8_000));
        short_pause_pcm.extend(std::iter::repeat_n(0.002_f32, 4_800)); // 300ms < 350ms
        short_pause_pcm.extend(std::iter::repeat_n(0.18_f32, 8_000));
        let expected_short = short_pause_pcm.clone();
        assert_eq!(mask_interior_silence_collars(&mut short_pause_pcm), 0);
        assert_eq!(short_pause_pcm, expected_short);
    }

    #[test]
    fn silence_collar_prevents_dead_zero_inside_hardware_gated_pause() {
        let mut pcm = Vec::new();
        pcm.extend(std::iter::repeat_n(0.2_f32, 8_000));
        pcm.extend(std::iter::repeat_n(0.0_f32, 6_400)); // 400ms hardware-zeroed pause
        pcm.extend(std::iter::repeat_n(0.2_f32, 8_000));

        assert_eq!(mask_interior_silence_collars(&mut pcm), 1);
        let interior = &pcm[8_000 + 800..14_400 - 800];
        assert!(interior
            .iter()
            .all(|&s| s != 0.0 && s.abs() <= SILENCE_COLLAR_FLOOR_GAIN));
    }

    #[test]
    fn stitcher_deduplicates_multiword_single_long_word_and_prefix_fragments() {
        assert_eq!(
            stitch_overlapping_transcripts(&[
                "we are testing the live",
                "the live transcription engine"
            ]),
            "we are testing the live transcription engine"
        );
        assert_eq!(
            stitch_overlapping_transcripts(&[
                "We are testing the live.",
                "The live transcription engine."
            ]),
            "We are testing the live transcription engine."
        );
        assert_eq!(
            stitch_overlapping_transcripts(&["ہم لائیو ٹیسٹ کر رہے", "ٹیسٹ کر رہے ہیں"]),
            "ہم لائیو ٹیسٹ کر رہے ہیں"
        );
        assert_eq!(
            stitch_overlapping_transcripts(&["हम लाइव टेस्ट कर रहे", "टेस्ट कर रहे हैं"]),
            "हम लाइव टेस्ट कर रहे हैं"
        );
        // 1-word boundary overlap rules: >= 5 chars collapses, short/conversational preserved.
        assert_eq!(
            stitch_overlapping_transcripts(&["transcription", "transcription works"]),
            "transcription works"
        );
        assert_eq!(
            stitch_overlapping_transcripts(&["hello", "hello"]),
            "hello hello"
        );
        assert_eq!(
            stitch_overlapping_transcripts(&["very", "very"]),
            "very very"
        );
        // Split prefix fragment recovery ("trans-" / "trans" -> "transcription").
        assert_eq!(
            stitch_overlapping_transcripts(&["we love trans-", "Transcription works"]),
            "we love transcription works"
        );
        assert_eq!(
            stitch_overlapping_transcripts(&["trans", "transcription works"]),
            "transcription works"
        );
        // Unspaced CJK preservation + overlap deduplication.
        assert_eq!(
            stitch_overlapping_transcripts(&["今天天气很好。", "我们去公园吧。"]),
            "今天天气很好。我们去公园吧。"
        );
        assert_eq!(
            stitch_overlapping_transcripts(&["今天天气很好。", "天气很好我们去公园吧。"]),
            "今天天气很好我们去公园吧。"
        );
        // Clean pause boundaries (`overlaps_with_prev == false`) preserve legitimate repeated words.
        assert_eq!(
            stitch_transcript_boundaries(&[
                ("transcription", false),
                ("transcription works", false),
            ]),
            "transcription transcription works"
        );
        assert_eq!(
            stitch_transcript_boundaries(&[("Wait.", false), ("Wait, let me check.", false),]),
            "Wait. Wait, let me check."
        );
    }

    #[test]
    fn compression_gate_rejects_multilingual_loops_and_preserves_bilingual_code_switches() {
        for hallucination in [
            "Thank you. Thank you. Thank you. Thank you.",
            "اور اور اور اور اور",
            "لالالالالالالالالالالالالالالالالالالالا",
            "क्योंकि क्योंकि क्योंकि क्योंकि क्योंकि",
        ] {
            let verdict = inspect_compression_and_confidence(hallucination, None, None);
            assert!(verdict.rejected, "should reject loop: {hallucination}");
        }

        for genuine in [
            "Let's ship this aur bhai kal milte hain",
            "یہ PR merge کر دو",
            "Sure بھائی، انشاءاللہ we will ship today.",
            "Thank you, everyone!",
        ] {
            let verdict = inspect_compression_and_confidence(genuine, Some(0.42), Some(0.20));
            assert!(!verdict.rejected, "must preserve genuine speech: {genuine}");
        }

        let extreme = inspect_compression_and_confidence("random words", Some(0.12), Some(0.90));
        assert_eq!(
            extreme.reason,
            Some(CompressionGateReason::ExtremeLowConfidence)
        );
    }

    #[test]
    fn accent_phonetic_skeleton_matches_global_pronunciations_and_protects_common_words() {
        let pairs = [
            ("grafias", "Graffias"),
            ("fantom", "Phantom"),
            ("vatsapp", "WhatsApp"),
            ("kloudflare", "Cloudflare"),
            ("soopabase", "Supabase"),
            ("kubernetees", "Kubernetes"),
        ];
        for (spoken, canonical) in pairs {
            assert_eq!(
                accent_phonetic_skeleton(spoken),
                accent_phonetic_skeleton(canonical),
                "{spoken} vs {canonical}"
            );
            assert!(
                (accent_phonetic_similarity(spoken, canonical) - 1.0).abs() < 1e-9,
                "{spoken} vs {canonical}"
            );
            assert!(
                is_accent_phonetic_match(spoken, canonical),
                "expected phonetic match for {spoken} -> {canonical}"
            );
        }

        for immune in [
            "transcription",
            "translation",
            "production",
            "function",
            "application",
            "information",
            "connection",
            "question",
            "location",
            "action",
            "version",
            "session",
            "section",
        ] {
            assert!(!is_accent_phonetic_match(immune, "lowyap"));
            assert!(!is_accent_phonetic_match(immune, "Verizon"));
            assert!(!is_accent_phonetic_match(immune, "Transkriptor"));
        }
    }
}
