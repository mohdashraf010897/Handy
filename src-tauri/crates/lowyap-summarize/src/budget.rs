//! Script-aware token estimation, Apple Intelligence script gating, and
//! UTF-8-safe budget chunking for multilingual meeting transcripts.

use lowyap_ports::TranscriptSegment;

/// Conservative characters-per-token divisor for dense non-Latin scripts
/// (Perso-Arabic, Devanagari, CJK, Hangul, Thai, Hebrew).
const DENSE_SCRIPT_CHARS_PER_TOKEN: f64 = 1.5;

/// Conservative characters-per-token divisor for ASCII and Latin scripts.
const LATIN_SCRIPT_CHARS_PER_TOKEN: f64 = 3.6;

/// Per-segment formatting overhead (`[MM:SS] [Speaker] \n`).
const SEGMENT_FORMAT_OVERHEAD_CHARS: usize = 14;

/// Per-rollup separator overhead (`\n\n`).
const ROLLUP_SEPARATOR_OVERHEAD_CHARS: usize = 2;

/// Estimates token count conservatively with script-aware character weights.
///
/// Perso-Arabic, Devanagari, CJK, Hangul, Thai, and Hebrew code points tokenize
/// far more densely (~`1.5` chars/token) than ASCII/Latin (~`3.6` chars/token).
/// Counting them with separate divisors prevents silent context overflows on
/// multilingual meetings.
pub fn estimate_multilingual_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut dense_chars = 0usize;
    let mut standard_chars = 0usize;

    for ch in text.chars() {
        if is_dense_token_script(ch) {
            dense_chars += 1;
        } else {
            standard_chars += 1;
        }
    }

    let estimate = (dense_chars as f64 / DENSE_SCRIPT_CHARS_PER_TOKEN)
        + (standard_chars as f64 / LATIN_SCRIPT_CHARS_PER_TOKEN);
    estimate.ceil() as usize
}

/// Returns `true` if `text` contains Perso-Arabic or Devanagari characters that
/// trigger `UnsupportedLanguageOrLocale` errors in macOS Apple Intelligence
/// `FoundationModels`.
pub fn contains_apple_intelligence_blocked_script(text: &str) -> bool {
    text.chars().any(is_apple_intelligence_blocked_char)
}

/// Splits transcript segments into UTF-8-safe chunks whose estimated formatted
/// character footprint stays within `max_chars`.
///
/// Never splits mid-codepoint. If a single segment alone exceeds `max_chars`,
/// its text is gracefully truncated on a Unicode scalar boundary so every
/// emitted chunk respects the budget.
pub fn chunk_segments_by_budget(
    segments: &[TranscriptSegment],
    max_chars: usize,
) -> Vec<Vec<TranscriptSegment>> {
    if segments.is_empty() {
        return Vec::new();
    }

    let budget = max_chars.max(1);
    let mut chunks: Vec<Vec<TranscriptSegment>> = Vec::new();
    let mut current: Vec<TranscriptSegment> = Vec::new();
    let mut current_chars = 0usize;

    for seg in segments {
        let cost = segment_formatted_chars(seg);
        if cost > budget {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_chars = 0;
            }
            let speaker_overhead = seg
                .speaker
                .display_label
                .chars()
                .count()
                .saturating_add(SEGMENT_FORMAT_OVERHEAD_CHARS);
            let allowed_text_chars = budget.saturating_sub(speaker_overhead);
            let mut truncated = seg.clone();
            truncated.text = truncate_chars_with_ellipsis(&seg.text, allowed_text_chars);
            chunks.push(vec![truncated]);
            continue;
        }

        if !current.is_empty() && current_chars.saturating_add(cost) > budget {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }

        current_chars = current_chars.saturating_add(cost);
        current.push(seg.clone());
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Splits rollup strings into UTF-8-safe groups whose combined character count
/// stays within `max_chars`.
///
/// Single oversized rollups are truncated on a Unicode scalar boundary so
/// downstream prompts never overflow.
pub fn chunk_rollups_by_budget(rollups: &[String], max_chars: usize) -> Vec<Vec<String>> {
    if rollups.is_empty() {
        return Vec::new();
    }

    let budget = max_chars.max(1);
    let mut chunks: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_chars = 0usize;

    for rollup in rollups {
        let cost = rollup
            .chars()
            .count()
            .saturating_add(ROLLUP_SEPARATOR_OVERHEAD_CHARS);

        if cost > budget {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_chars = 0;
            }
            let allowed = budget
                .saturating_sub(ROLLUP_SEPARATOR_OVERHEAD_CHARS)
                .max(1);
            chunks.push(vec![truncate_chars_with_ellipsis(rollup, allowed)]);
            continue;
        }

        if !current.is_empty() && current_chars.saturating_add(cost) > budget {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }

        current_chars = current_chars.saturating_add(cost);
        current.push(rollup.clone());
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Computes the estimated formatted character cost of one transcript segment.
pub(crate) fn segment_formatted_chars(seg: &TranscriptSegment) -> usize {
    seg.speaker
        .display_label
        .chars()
        .count()
        .saturating_add(seg.text.chars().count())
        .saturating_add(SEGMENT_FORMAT_OVERHEAD_CHARS)
}

/// Truncates `text` to at most `max_chars` Unicode scalar values, appending
/// `'…'` when truncation occurs and `max_chars >= 2`.
pub(crate) fn truncate_chars_with_ellipsis(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_owned();
    }

    if max_chars == 1 {
        return text.chars().take(1).collect();
    }

    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

/// Returns `true` for Perso-Arabic or Devanagari code points blocked by Apple
/// Intelligence `FoundationModels`.
const fn is_apple_intelligence_blocked_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{0600}'..='\u{06FF}' // Arabic / Urdu / Persian
            | '\u{0750}'..='\u{077F}' // Arabic Supplement
            | '\u{08A0}'..='\u{08FF}' // Arabic Extended-A
            | '\u{FB50}'..='\u{FDFF}' // Arabic Presentation Forms-A
            | '\u{FE70}'..='\u{FEFF}' // Arabic Presentation Forms-B
            | '\u{0900}'..='\u{097F}' // Devanagari
    )
}

/// Returns `true` for non-Latin scripts with high subword-token density.
const fn is_dense_token_script(ch: char) -> bool {
    is_apple_intelligence_blocked_char(ch)
        || matches!(
            ch,
            '\u{0590}'..='\u{05FF}' // Hebrew
                | '\u{0E00}'..='\u{0E7F}' // Thai
                | '\u{1100}'..='\u{11FF}' // Hangul Jamo
                | '\u{3000}'..='\u{30FF}' // CJK Symbols, Hiragana, Katakana
                | '\u{3130}'..='\u{318F}' // Hangul Compatibility Jamo
                | '\u{3400}'..='\u{4DBF}' // CJK Unified Ideographs Ext A
                | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
                | '\u{A8E0}'..='\u{A8FF}' // Devanagari Extended
                | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
                | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
                | '\u{FF00}'..='\u{FFEF}' // Halfwidth / Fullwidth Forms
        )
}

#[cfg(test)]
mod tests {
    use lowyap_ports::{SpeakerChannel, SpeakerIdentity};

    use super::*;

    fn make_segment(label: &str, text: &str, start_ms: u64) -> TranscriptSegment {
        TranscriptSegment {
            speaker: SpeakerIdentity {
                channel: SpeakerChannel::Others,
                speaker_id: label.to_ascii_lowercase(),
                display_label: label.to_owned(),
                confidence: Some(0.9),
            },
            start_ms,
            end_ms: start_ms + 5_000,
            text: text.to_owned(),
            language: None,
        }
    }

    #[test]
    fn estimate_multilingual_tokens_weights_dense_scripts_higher_than_latin() {
        let english = "a".repeat(36);
        let urdu = "ن".repeat(36);
        let devanagari = "क".repeat(36);
        let cjk = "漢".repeat(36);

        assert_eq!(estimate_multilingual_tokens(""), 0);
        assert_eq!(estimate_multilingual_tokens(&english), 10);
        assert_eq!(estimate_multilingual_tokens(&urdu), 24);
        assert_eq!(estimate_multilingual_tokens(&devanagari), 24);
        assert_eq!(estimate_multilingual_tokens(&cjk), 24);
    }

    #[test]
    fn apple_intelligence_script_guard_flags_perso_arabic_and_devanagari() {
        assert!(!contains_apple_intelligence_blocked_script(
            "Ship AEC3 before Friday; bhai kal ka demo ready rakhna hai."
        ));
        assert!(contains_apple_intelligence_blocked_script(
            "Meeting notes: میٹنگ نوٹس"
        ));
        assert!(contains_apple_intelligence_blocked_script(
            "Arabic plan: مراجعة خطة الإطلاق"
        ));
        assert!(contains_apple_intelligence_blocked_script(
            "Hindi check: मीटिंग नोट्स तैयार हैं"
        ));
        // Presentation forms block
        assert!(contains_apple_intelligence_blocked_script("\u{FB50}"));
        assert!(contains_apple_intelligence_blocked_script("\u{FE70}"));
    }

    #[test]
    fn chunk_segments_handles_multibyte_monster_segment_on_char_boundary() {
        let monster = make_segment("Speaker 1", &"میٹنگ نوٹس ".repeat(80), 0);
        let chunks = chunk_segments_by_budget(&[monster], 60);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 1);
        assert!(segment_formatted_chars(&chunks[0][0]) <= 60);
        assert!(chunks[0][0].text.ends_with('…'));
    }

    #[test]
    fn chunk_rollups_packs_within_budget_and_truncates_oversized_entry() {
        let rollups = vec![
            "- Short bullet one".to_owned(),
            "- Short bullet two".to_owned(),
            "مراجعة خطة الإطلاق ".repeat(30),
        ];

        let chunks = chunk_rollups_by_budget(&rollups, 80);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2);
        assert_eq!(chunks[1].len(), 1);
        assert!(chunks[1][0].chars().count() <= 78);
        assert!(chunks[1][0].ends_with('…'));
    }
}
