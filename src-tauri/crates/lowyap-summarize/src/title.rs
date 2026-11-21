//! Placeholder meeting-title detection and automatic title generation.

use lowyap_ports::TranscriptSegment;

use crate::budget::{contains_apple_intelligence_blocked_script, truncate_chars_with_ellipsis};
use crate::local_gguf::{strip_reasoning_and_fences, CompletionBackend};

const TITLE_SYSTEM_PROMPT: &str = "Generate a concise 3 to 7 word title for this meeting. \
Return ONLY the title on a single plain line with no quotes, no markdown heading, and no trailing punctuation. \
Preserve the meeting's original language or multilingual key terms.";

const MIN_LLM_TITLE_CHARS: usize = 3;
const MAX_LLM_TITLE_CHARS: usize = 72;
const MAX_FALLBACK_TITLE_CHARS: usize = 68;
const MAX_FALLBACK_TITLE_WORDS: usize = 8;

/// Returns `true` when `title` is empty, `"Untitled"`, `"Untitled Meeting"`, or
/// an auto-generated timestamp placeholder such as `"Meeting 2026-11-20 15:02"`
/// or `"Meeting — 2026-11-20"`.
pub fn is_placeholder_meeting_title(title: &str) -> bool {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    if matches!(lower.as_str(), "untitled" | "untitled meeting") {
        return true;
    }

    if let Some(rest) = lower.strip_prefix("meeting") {
        let after_prefix = rest.trim_start();
        if after_prefix.is_empty() {
            return true;
        }
        let after_sep = after_prefix
            .trim_start_matches(['-', '–', '—', ':'])
            .trim_start();
        if after_sep
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
        {
            return true;
        }
    }

    false
}

/// Generates a concise meeting title from rough notes, rollups, and transcript
/// segments, trying an optional [`CompletionBackend`] before falling back to
/// deterministic extraction.
///
/// Returns `None` when all inputs lack substantive text.
pub fn generate_meeting_title(
    rough_notes: &str,
    rollups: &[String],
    segments: &[TranscriptSegment],
    backend: Option<&dyn CompletionBackend>,
) -> Option<String> {
    let has_rough = !rough_notes.trim().is_empty();
    let has_rollups = rollups.iter().any(|r| !r.trim().is_empty());
    let has_segments = segments.iter().any(|s| !s.text.trim().is_empty());
    if !has_rough && !has_rollups && !has_segments {
        return None;
    }

    if let Some(llm) = backend {
        if let Some(title) = try_llm_title(rough_notes, rollups, segments, llm) {
            return Some(title);
        }
    }

    deterministic_meeting_title(rough_notes, rollups, segments)
}

fn try_llm_title(
    rough_notes: &str,
    rollups: &[String],
    segments: &[TranscriptSegment],
    backend: &dyn CompletionBackend,
) -> Option<String> {
    let mut snippet = String::new();
    if !rough_notes.trim().is_empty() {
        snippet.push_str("Rough notes:\n");
        snippet.push_str(&truncate_chars_with_ellipsis(rough_notes.trim(), 600));
        snippet.push('\n');
    }
    if let Some(first_rollup) = rollups.iter().find(|r| !r.trim().is_empty()) {
        snippet.push_str("Summary:\n");
        snippet.push_str(&truncate_chars_with_ellipsis(first_rollup.trim(), 600));
        snippet.push('\n');
    }
    for seg in segments
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .take(5)
    {
        snippet.push_str(&seg.speaker.display_label);
        snippet.push_str(": ");
        snippet.push_str(&truncate_chars_with_ellipsis(seg.text.trim(), 160));
        snippet.push('\n');
    }

    let budget = backend.max_prompt_chars().clamp(128, 1_800);
    let bounded = truncate_chars_with_ellipsis(&snippet, budget);

    if !backend.supports_non_latin_scripts() && contains_apple_intelligence_blocked_script(&bounded)
    {
        return None;
    }

    let raw = backend.complete(TITLE_SYSTEM_PROMPT, &bounded, 24).ok()?;
    let cleaned = sanitize_llm_title(&raw);
    let len = cleaned.chars().count();
    if (MIN_LLM_TITLE_CHARS..=MAX_LLM_TITLE_CHARS).contains(&len)
        && !is_placeholder_meeting_title(&cleaned)
    {
        Some(cleaned)
    } else {
        None
    }
}

fn sanitize_llm_title(raw: &str) -> String {
    let stripped = strip_reasoning_and_fences(raw);
    let first_line = stripped
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();

    let mut clean = first_line;
    loop {
        let prev = clean;
        clean = clean
            .trim_matches(['"', '\'', '`', '“', '”', '‘', '’'])
            .trim_start_matches(['#', '-', '*', '•', '>', ' '])
            .trim();
        if let Some(rest) = strip_ascii_prefix_ignore_case(clean, "title:") {
            clean = rest.trim();
        }
        if clean == prev {
            break;
        }
    }

    clean.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn deterministic_meeting_title(
    rough_notes: &str,
    rollups: &[String],
    segments: &[TranscriptSegment],
) -> Option<String> {
    let mut candidates = Vec::new();

    for line in rough_notes.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            candidates.push(trimmed);
        }
    }

    for rollup in rollups {
        for line in rollup.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                candidates.push(trimmed);
            }
        }
    }

    for seg in segments {
        let trimmed = seg.text.trim();
        if !trimmed.is_empty() {
            candidates.push(trimmed);
        }
    }

    let mut short_fallback: Option<String> = None;

    for raw in candidates {
        if let Some((title, word_count)) = extract_title_from_line(raw) {
            if word_count >= 3 {
                return Some(title);
            }
            if short_fallback.is_none() {
                short_fallback = Some(title);
            }
        }
    }

    short_fallback
}

fn extract_title_from_line(line: &str) -> Option<(String, usize)> {
    let cleaned = strip_leading_noise(line);
    if cleaned.is_empty() {
        return None;
    }

    let mut words: Vec<String> = Vec::new();
    let mut current_len = 0usize;

    for raw_word in cleaned.split_whitespace() {
        let trimmed_word = raw_word
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '“' | '”' | '*' | '_'
                )
            })
            .trim_end_matches([',', '.', ';', ':', '!', '?', '،', '۔']);

        if trimmed_word.is_empty() {
            continue;
        }

        if words.is_empty() && is_leading_filler(trimmed_word) {
            continue;
        }

        let word_len = trimmed_word.chars().count();
        let next_len = if words.is_empty() {
            word_len
        } else {
            current_len + 1 + word_len
        };

        if words.len() >= MAX_FALLBACK_TITLE_WORDS || next_len > MAX_FALLBACK_TITLE_CHARS {
            if words.is_empty() {
                let truncated =
                    truncate_chars_with_ellipsis(trimmed_word, MAX_FALLBACK_TITLE_CHARS);
                if !truncated.is_empty() {
                    words.push(truncated);
                }
            }
            break;
        }

        words.push(trimmed_word.to_owned());
        current_len = next_len;

        // Stop cleanly at the end of a clause if we already have >= 4 words.
        if words.len() >= 4 && raw_word.ends_with(['.', '!', '?', '۔', ';']) {
            break;
        }
    }

    if words.is_empty() {
        return None;
    }

    let joined = capitalize_leading_ascii(&words.join(" "));
    if is_placeholder_meeting_title(&joined) {
        return None;
    }

    Some((joined, words.len()))
}

fn strip_leading_noise(input: &str) -> String {
    let mut rest = input.trim();

    loop {
        let prev = rest;

        // Strip markdown bullet markers.
        rest = rest
            .trim_start_matches(['-', '*', '•', '–', '—', '#'])
            .trim_start();

        // Strip leading bracketed prefixes such as `[min 00]`, `[00:00]`, `[Me]`, `[Speaker 1]`.
        if rest.starts_with('[') {
            if let Some(close_idx) = rest.find(']') {
                rest = rest[close_idx + 1..].trim_start();
            }
        }

        // Strip bold speaker labels such as `**[Me]**:` or `**Speaker 1**:`.
        if let Some(after_open) = rest.strip_prefix("**") {
            if let Some(close_idx) = after_open.find("**") {
                let after_bold = after_open[close_idx + 2..].trim_start();
                if let Some(after_colon) = after_bold.strip_prefix(':') {
                    rest = after_colon.trim_start();
                } else {
                    rest = after_bold;
                }
            }
        }

        // Strip common action / decision cue labels.
        for label in [
            "action item:",
            "action items:",
            "action:",
            "decision:",
            "decisions:",
            "follow up:",
            "follow-up:",
            "next step:",
            "todo:",
            "note:",
        ] {
            if let Some(after) = strip_ascii_prefix_ignore_case(rest, label) {
                rest = after.trim_start();
            }
        }

        // Strip common Urdu / Arabic leading cue labels.
        for label in ["اگلا قدم:", "فیصلہ:"] {
            if let Some(after) = rest.strip_prefix(label) {
                rest = after.trim_start();
            }
        }

        if rest == prev {
            break;
        }
    }

    rest.to_owned()
}

fn strip_ascii_prefix_ignore_case<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    if input.len() < prefix.len() || !input.is_char_boundary(prefix.len()) {
        return None;
    }
    if input[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&input[prefix.len()..])
    } else {
        None
    }
}

fn is_leading_filler(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "um" | "umm"
            | "uh"
            | "uhh"
            | "er"
            | "ah"
            | "so"
            | "okay"
            | "ok"
            | "well"
            | "like"
            | "hey"
            | "hi"
            | "hello"
            | "actually"
            | "basically"
    )
}

fn capitalize_leading_ascii(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if first.is_ascii_lowercase() {
        let mut out = String::with_capacity(text.len());
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
        out
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use lowyap_ports::{PortError, SpeakerChannel, SpeakerIdentity};

    use super::*;

    struct MockTitleBackend {
        response: Result<String, PortError>,
        supports_non_latin: bool,
    }

    impl CompletionBackend for MockTitleBackend {
        fn source_id(&self) -> &'static str {
            "mock_title"
        }

        fn max_prompt_chars(&self) -> usize {
            4_000
        }

        fn supports_non_latin_scripts(&self) -> bool {
            self.supports_non_latin
        }

        fn complete(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
            _max_tokens: u32,
        ) -> Result<String, PortError> {
            self.response.clone()
        }
    }

    fn seg(speaker: &str, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            speaker: SpeakerIdentity {
                channel: SpeakerChannel::Me,
                speaker_id: speaker.to_ascii_lowercase(),
                display_label: speaker.to_owned(),
                confidence: Some(0.95),
            },
            start_ms: 0,
            end_ms: 5_000,
            text: text.to_owned(),
            language: None,
        }
    }

    #[test]
    fn placeholder_title_detection_handles_empty_untitled_and_timestamps() {
        for placeholder in [
            "",
            "   ",
            "Untitled",
            "untitled meeting",
            "Meeting 2026-11-20 15:02",
            "Meeting — 2026-11-20",
            "Meeting - 2026-01-05 09:30",
            "Meeting: 2026-11-20",
        ] {
            assert!(
                is_placeholder_meeting_title(placeholder),
                "expected '{placeholder}' to be detected as placeholder"
            );
        }

        for real in [
            "Q4 Audio Modularity & Multilingual Rollout",
            "Meeting with Design Team",
            "AEC3 Echo Cancellation Review",
            "میٹنگ نوٹس کا معیار",
        ] {
            assert!(
                !is_placeholder_meeting_title(real),
                "expected '{real}' to NOT be a placeholder"
            );
        }
    }

    #[test]
    fn generate_meeting_title_returns_none_for_empty_inputs() {
        assert_eq!(generate_meeting_title("   ", &[], &[], None), None);
    }

    #[test]
    fn generate_meeting_title_strips_prefixes_fillers_and_preserves_scripts() {
        let english = generate_meeting_title(
            "",
            &[],
            &[seg(
                "Me",
                "[min 00] Action item: Um, okay, so ship the AEC3 adapter before Friday review.",
            )],
            None,
        )
        .expect("expected English title");
        assert!(english.starts_with("Ship the AEC3 adapter"));
        assert!(english.chars().count() <= 68);

        let urdu = generate_meeting_title(
            "",
            &[],
            &[seg(
                "Speaker 2",
                "[min 02] اگلا قدم: میٹنگ نوٹس کا معیار بہتر بنانا اور اردو اصطلاحات محفوظ رکھنا۔",
            )],
            None,
        )
        .expect("expected Urdu title");
        assert!(urdu.contains("میٹنگ نوٹس"));
        assert!(urdu.chars().count() <= 68);

        let arabic = generate_meeting_title(
            "",
            &[],
            &[seg(
                "Speaker 1",
                "[min 04] مراجعة خطة الإطلاق الأسبوعية مع التأكد من استقرار الصوت المزدوج",
            )],
            None,
        )
        .expect("expected Arabic title");
        assert!(arabic.contains("مراجعة خطة الإطلاق"));
        assert!(arabic.chars().count() <= 68);

        let hinglish = generate_meeting_title(
            "",
            &[],
            &[seg(
                "Speaker 1",
                "So, okay, bhai kal ka demo bilkul ready rakhna hai.",
            )],
            None,
        )
        .expect("expected Hinglish title");
        assert!(hinglish.contains("demo"));
    }

    #[test]
    fn generate_meeting_title_uses_backend_and_skips_non_latin_unsafe_backend() {
        let good_llm = MockTitleBackend {
            response: Ok(
                "<think>picking title</think>\n\"# Q4 AEC3 & Multilingual Launch\"".to_owned(),
            ),
            supports_non_latin: true,
        };
        let title = generate_meeting_title("AEC3 launch notes", &[], &[], Some(&good_llm))
            .expect("expected LLM title");
        assert_eq!(title, "Q4 AEC3 & Multilingual Launch");

        // Backend that cannot handle Perso-Arabic must be bypassed automatically
        let apple_like = MockTitleBackend {
            response: Ok("Should Never Be Used".to_owned()),
            supports_non_latin: false,
        };
        let fallback_urdu = generate_meeting_title(
            "میٹنگ نوٹس کا معیار بہتر بنانا",
            &[],
            &[],
            Some(&apple_like),
        )
        .expect("expected fallback Urdu title");
        assert!(fallback_urdu.contains("میٹنگ نوٹس"));
        assert_ne!(fallback_urdu, "Should Never Be Used");
    }
}
