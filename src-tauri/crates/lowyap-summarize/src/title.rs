//! Placeholder meeting-title detection and automatic title generation.

use lowyap_ports::TranscriptSegment;

use crate::budget::{contains_apple_intelligence_blocked_script, truncate_chars_with_ellipsis};
use crate::local_gguf::{strip_reasoning_and_fences, CompletionBackend};

const TITLE_SYSTEM_PROMPT: &str = "Generate a concise 3 to 7 word title for this meeting. \
Return ONLY the title on a single plain line with no quotes, no markdown heading, and no trailing punctuation. \
Preserve the meeting's original language or multilingual key terms.";

const TITLE_SYSTEM_PROMPT_LATIN_ONLY: &str = "Generate a concise 3 to 7 word descriptive English title in Latin script capturing the core topic of this meeting (for example: UI Components Observability Discussion or Garage Week Ticket Updates). \
Return ONLY the title on a single plain line with no quotes, no markdown heading, and no trailing punctuation. \
Preserve technical acronyms, product names, and Romanized domain terms, and never output Perso-Arabic or Devanagari characters.";

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
    for seg in select_representative_title_segments(segments, 10) {
        snippet.push_str(&seg.speaker.display_label);
        snippet.push_str(": ");
        snippet.push_str(&truncate_chars_with_ellipsis(seg.text.trim(), 160));
        snippet.push('\n');
    }

    let budget = backend.max_prompt_chars().clamp(128, 1_800);
    let mut bounded = truncate_chars_with_ellipsis(&snippet, budget);

    if !backend.supports_non_latin_scripts() && contains_apple_intelligence_blocked_script(&bounded)
    {
        if is_predominantly_english_title_context(&bounded) {
            let roman = lowyap_flow::hindustani_to_roman_hinglish(&bounded);
            if !contains_apple_intelligence_blocked_script(&roman) {
                bounded = roman;
            } else {
                return None;
            }
        } else {
            return None;
        }
    }

    let system_prompt = if backend.supports_non_latin_scripts() {
        TITLE_SYSTEM_PROMPT
    } else {
        TITLE_SYSTEM_PROMPT_LATIN_ONLY
    };
    let raw = backend.complete(system_prompt, &bounded, 24).ok()?;
    let cleaned = sanitize_llm_title(&raw);
    if !backend.supports_non_latin_scripts() && contains_apple_intelligence_blocked_script(&cleaned)
    {
        return None;
    }
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
    let mut explicit_candidates = Vec::new();

    for line in rough_notes.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !is_mic_check_or_greeting_only(trimmed) {
            explicit_candidates.push(trimmed);
        }
    }

    for rollup in rollups {
        for line in rollup.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && !is_mic_check_or_greeting_only(trimmed)
            {
                explicit_candidates.push(trimmed);
            }
        }
    }

    let mut short_fallback: Option<String> = None;

    for raw in explicit_candidates {
        if let Some((title, word_count)) = extract_title_from_line(raw) {
            if word_count >= 3 {
                return Some(title);
            }
            if short_fallback.is_none() {
                short_fallback = Some(title);
            }
        }
    }

    let total_duration_ms = segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
    let rep_segments = select_representative_title_segments(segments, 20);
    let mut best_scored: Option<(i32, usize, String)> = None;

    for (idx, seg) in rep_segments.iter().enumerate() {
        let raw = seg.text.trim();
        if raw.is_empty() {
            continue;
        }
        if let Some((title, word_count)) = extract_title_from_line(raw) {
            if word_count < 3 {
                if short_fallback.is_none() {
                    short_fallback = Some(title);
                }
                continue;
            }
            let score = score_segment_title_candidate(seg, &title, word_count, total_duration_ms);
            match &best_scored {
                None => best_scored = Some((score, idx, title)),
                Some((best_score, best_idx, _))
                    if score > *best_score || (score == *best_score && idx < *best_idx) =>
                {
                    best_scored = Some((score, idx, title));
                }
                _ => {}
            }
        }
    }

    best_scored.map(|(_, _, title)| title).or(short_fallback)
}

fn score_segment_title_candidate(
    seg: &TranscriptSegment,
    extracted_title: &str,
    title_word_count: usize,
    total_duration_ms: u64,
) -> i32 {
    let raw = seg.text.trim();
    let raw_words: Vec<&str> = raw.split_whitespace().collect();
    let mut score = 0i32;

    if (4..=8).contains(&title_word_count) {
        score += 18;
    } else if title_word_count == 3 {
        score += 6;
    }

    if (6..=28).contains(&raw_words.len()) {
        score += 14;
    }

    // Short 3-to-4-word bare questions ("کیا گیا اپنی؟", "What happened there?")
    // are almost never the real topic when declarative turns exist.
    let is_question = raw.ends_with(['?', '؟']);
    if is_question && raw_words.len() <= 4 {
        score -= 35;
    } else if is_question && raw_words.len() <= 6 {
        score -= 12;
    }

    // Opening 10 seconds of a >= 30s call are predominantly greetings/audio setup.
    if total_duration_ms >= 30_000 && seg.start_ms < 10_000 {
        score -= 14;
    }

    // Reward multi-letter technical acronyms / proper identifiers (ECC, HRD, AEC3, SDK, API).
    let has_acronym = extracted_title.split_whitespace().any(|w| {
        let alpha: Vec<char> = w.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        alpha.len() >= 2 && alpha.iter().all(|c| c.is_ascii_uppercase())
    });
    if has_acronym {
        score += 12;
    }

    // Reward concrete topic nouns across English, Urdu, Arabic, and Hinglish.
    let lower = extracted_title.to_ascii_lowercase();
    for kw in [
        "design",
        "document",
        "ticket",
        "sprint",
        "garage",
        "release",
        "launch",
        "review",
        "architecture",
        "holiday",
        "roadmap",
        "contract",
        "delivery",
        "verb",
        "conjugation",
    ] {
        if lower.contains(kw) {
            score += 10;
            break;
        }
    }

    for kw in [
        "فعل",
        "مضارع",
        "ماضی",
        "گردان",
        "ضمیر",
        "جینتی",
        "چھٹی",
        "کلینڈر",
        "معیار",
        "منصوبہ",
        "فیصلہ",
    ] {
        if extracted_title.contains(kw) || raw.contains(kw) {
            score += 12;
            break;
        }
    }

    score
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

        // Strip conversational throat-clearing preambles when >= 3 substantive words remain.
        for preamble in [
            "yeah, so from my side,",
            "yeah so from my side",
            "so from my side,",
            "so from my side",
            "from my side,",
            "from my side",
            "i had a discussion with",
            "had a discussion with",
            "i wanted to talk about",
            "wanted to talk about",
            "let's talk about",
            "today we will talk about",
        ] {
            if let Some(after) = strip_ascii_prefix_ignore_case(rest, preamble) {
                let candidate = after.trim_start_matches([',', ' ']);
                if candidate.split_whitespace().count() >= 3 {
                    rest = candidate;
                }
            }
        }

        for urdu_preamble in [
            "ہاں جی، میرے کو بھی آج ہی پتہ چلا ہے کیونکہ میرا تو",
            "ہاں جی میرے کو بھی آج ہی پتہ چلا ہے کیونکہ میرا تو",
            "ہاں جی، میرے کو بھی آج ہی پتہ چلا ہے کیونکہ",
            "ہاں جی میرے کو بھی آج ہی پتہ چلا ہے کیونکہ",
            "ہاں جی،",
            "جی ہاں،",
        ] {
            if let Some(after) = rest.strip_prefix(urdu_preamble) {
                let candidate = after.trim_start_matches(['،', ',', ' ']);
                if candidate.split_whitespace().count() >= 3 {
                    rest = candidate;
                }
            }
        }

        if rest == prev {
            break;
        }
    }

    // If a long Urdu/Hindustani explanation leads into a concrete "تو <topic>" conclusion,
    // prefer the concise conclusion clause when it has 3..=8 words.
    if rest.split_whitespace().count() > 9 {
        if let Some((_, rhs)) = rest.rsplit_once(" تو ") {
            let rhs_trimmed = rhs.trim();
            let rhs_words = rhs_trimmed.split_whitespace().count();
            if (3..=8).contains(&rhs_words) {
                return rhs_trimmed.to_owned();
            }
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
            | "yeah"
            | "yes"
            | "actually"
            | "basically"
    )
}

fn is_mic_check_or_greeting_only(text: &str) -> bool {
    let unicode_clean: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if unicode_clean.is_empty() {
        return true;
    }

    let unicode_word_count = unicode_clean.split_whitespace().count();

    // Perso-Arabic / Urdu / Arabic greetings, salutations, and empty topic intros.
    if unicode_word_count <= 9 {
        for greeting in [
            "السلام علیکم",
            "السلام عليكم",
            "وعلیکم السلام",
            "وعليكم السلام",
            "آج ہم اس موضوع پر بات کریں گے",
            "آج ہم بات کریں گے",
            "کیسے ہیں آپ",
            "کیا حال ہے",
            "شکریہ",
            "جزاک اللہ",
        ] {
            if unicode_clean == greeting || unicode_clean.starts_with(&format!("{greeting} ")) {
                return true;
            }
        }
    }

    // Short noisy greeting transliterations ("ہیو ساگر، ہائی ایر جی ہوتھ۔", "ہیلو ایوری ون").
    if unicode_word_count <= 7 {
        let u_words: Vec<&str> = unicode_clean.split_whitespace().collect();
        if matches!(
            u_words.first().copied(),
            Some("ہیو" | "ہائے" | "ہائی" | "ہیلو" | "سلام")
        ) && (unicode_word_count <= 4
            || u_words
                .iter()
                .skip(1)
                .any(|w| matches!(*w, "ہائی" | "ہائے" | "ہیلو" | "جی")))
        {
            return true;
        }
    }

    let lower: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.is_empty() {
        return true;
    }
    let joined = words.join(" ");

    // Call-setup, transcription-bot notices, and screen-share chatter.
    if words.len() <= 12 {
        for boilerplate in [
            "this meeting is being transcribed",
            "this meeting is being recorded",
            "recording in progress",
            "i ll share my screen",
            "i will share my screen",
            "let me share my screen",
            "can you see my screen",
            "are you able to see my screen",
        ] {
            if joined == boilerplate || joined.starts_with(&format!("{boilerplate} ")) {
                return true;
            }
        }
    }

    if words.len() <= 9 {
        for pattern in [
            "am i audible",
            "can you hear me",
            "are you able to hear",
            "you are on mute",
            "assalamu alaikum",
            "as salamu alaikum",
            "walaikum assalam",
            "wa alaikum assalam",
            "namaste",
            "sat sri akal",
            "good morning",
            "good afternoon",
            "good evening",
            "hello everyone",
            "hi everyone",
            "hey everyone",
            "thank you",
            "thanks everyone",
            "you re welcome",
            "bye bye",
            "see you",
            "haan ji",
            "hanji",
            "hello",
            "testing",
            "audible",
        ] {
            if joined == pattern || (joined.starts_with(&format!("{pattern} ")) && words.len() <= 7)
            {
                return true;
            }
        }
    }
    false
}

/// Picks up to `limit` substantive transcript turns spread across opening,
/// middle, and closing portions of the meeting while skipping mic-check/greeting
/// throwaways whenever substantive turns exist.
fn select_representative_title_segments(
    segments: &[TranscriptSegment],
    limit: usize,
) -> Vec<&TranscriptSegment> {
    let non_empty: Vec<&TranscriptSegment> = segments
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .collect();
    if non_empty.is_empty() || limit == 0 {
        return Vec::new();
    }
    let substantive: Vec<&TranscriptSegment> = non_empty
        .iter()
        .copied()
        .filter(|s| !is_mic_check_or_greeting_only(&s.text))
        .collect();
    let pool = if substantive.is_empty() {
        &non_empty
    } else {
        &substantive
    };
    if pool.len() <= limit {
        return pool.clone();
    }
    let mut out = Vec::with_capacity(limit);
    let last_idx = pool.len() - 1;
    for slot in 0..limit {
        let idx = (slot * last_idx) / (limit - 1).max(1);
        out.push(pool[idx]);
    }
    out
}

fn is_predominantly_english_title_context(text: &str) -> bool {
    let mut latin = 0usize;
    let mut non_latin = 0usize;
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            latin += 1;
        } else if ch.is_alphabetic() {
            non_latin += 1;
        }
    }
    latin >= 120 && latin >= non_latin.saturating_mul(3)
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

    #[test]
    fn generate_meeting_title_skips_mic_checks_and_romanizes_english_dominant_hindustani_for_apple()
    {
        let apple_like = MockTitleBackend {
            response: Ok("Content Cards Delivery & Holiday Schedule".to_owned()),
            supports_non_latin: false,
        };
        let segments = vec![
            seg("Me", "Hello? Am I audible?"),
            seg("Speaker 1", "Can you hear me?"),
            seg(
                "Speaker 1",
                "We need to finalize the Content Cards SDK delivery contract and verify backward compatibility across Android and iOS builds before release.",
            ),
            seg(
                "Me",
                "Haan aur گورنانک جینتی کی چھٹی سے پہلے QA signoff complete kar lenge for the production rollout.",
            ),
        ];

        // Deterministic fallback skips "Hello? Am I audible?" and "Can you hear me?"
        let det_title = generate_meeting_title("", &[], &segments, None).expect("det title");
        assert!(
            det_title.contains("Content Cards"),
            "expected mic-check lines to be skipped, got '{det_title}'"
        );

        // Predominantly-English meeting with minor Hindustani script islands uses Apple Intelligence via Roman bridge
        let llm_title =
            generate_meeting_title("", &[], &segments, Some(&apple_like)).expect("llm title");
        assert_eq!(llm_title, "Content Cards Delivery & Holiday Schedule");
    }

    #[test]
    fn real_world_multilingual_openings_skip_greetings_boilerplate_and_short_questions() {
        // 1. Islamic greeting + empty intro must be skipped in favor of the Arabic verb lesson
        let arabic_workshop = vec![
            TranscriptSegment {
                start_ms: 60_000,
                end_ms: 64_000,
                ..seg("Speaker 1", "وعلیکم السلام ورحمت اللہ وبرکاتہ۔")
            },
            TranscriptSegment {
                start_ms: 65_000,
                end_ms: 69_000,
                ..seg("Speaker 1", "آج ہم اس موضوع پر بات کریں گے۔")
            },
            TranscriptSegment {
                start_ms: 120_000,
                end_ms: 132_000,
                ..seg(
                    "Speaker 1",
                    "فعل مضارع اور ماضی کی گردان میں ضمیر کی تبدیلی سمجھیں۔",
                )
            },
        ];
        let workshop_title =
            generate_meeting_title("", &[], &arabic_workshop, None).expect("workshop title");
        assert!(
            !workshop_title.contains("وعلیکم السلام"),
            "must never title a meeting after Assalamu Alaikum: got '{workshop_title}'"
        );
        assert!(
            workshop_title.contains("فعل مضارع"),
            "expected substantive Arabic verb topic, got '{workshop_title}'"
        );

        // 2. Noisy Hinglish greeting + Zoom transcription notice + screen-share setup skipped
        let standup = vec![
            TranscriptSegment {
                start_ms: 4_000,
                end_ms: 7_000,
                ..seg("Speaker 1", "ہیو ساگر، ہائی ایر جی ہوتھ۔")
            },
            TranscriptSegment {
                start_ms: 8_000,
                end_ms: 11_000,
                ..seg("Speaker 2", "This meeting is being transcribed.")
            },
            TranscriptSegment {
                start_ms: 12_000,
                end_ms: 15_000,
                ..seg("Me", "I'll share my screen.")
            },
            TranscriptSegment {
                start_ms: 45_000,
                end_ms: 58_000,
                ..seg(
                    "Me",
                    "Yeah, so from my side, I had a discussion with Raj from the ECC team regarding the HRD design document.",
                )
            },
        ];
        let standup_title = generate_meeting_title("", &[], &standup, None).expect("standup title");
        assert!(
            !standup_title.contains("ہیو ساگر") && !standup_title.contains("transcribed"),
            "must skip noisy greeting & transcription notice, got '{standup_title}'"
        );
        assert!(
            standup_title.contains("ECC") || standup_title.contains("HRD"),
            "expected substantive standup topic, got '{standup_title}'"
        );

        // 3. Short 3-word Urdu question ("کیا گیا اپنی؟") loses to Guru Nanak Jayanti holiday answer
        let holiday_check = vec![
            TranscriptSegment {
                start_ms: 1_000,
                end_ms: 2_500,
                ..seg("Speaker 1", "کیا گیا اپنی؟")
            },
            TranscriptSegment {
                start_ms: 3_000,
                end_ms: 16_000,
                ..seg(
                    "Me",
                    "ہاں جی، میرے کو بھی آج ہی پتہ چلا ہے کیونکہ میرا تو کلینڈر انڈیا کے حساب سے مارک رہتا ہے تو گورنانک جینتی چھٹی دکھا رہا تھا کل۔",
                )
            },
        ];
        let holiday_title =
            generate_meeting_title("", &[], &holiday_check, None).expect("holiday title");
        assert!(
            holiday_title.contains("گورنانک جینتی"),
            "expected Guru Nanak Jayanti holiday clause, got '{holiday_title}'"
        );
    }

    #[test]
    fn latin_only_title_backend_rejects_perso_arabic_hallucination_and_falls_back_cleanly() {
        let hallucinating_apple = MockTitleBackend {
            response: Ok("جب یونیفائیڈ سلکے درون پیلے گئے ہیں لہذا".to_owned()),
            supports_non_latin: false,
        };
        let segments = vec![
            seg(
                "Me",
                "We are reviewing UI Components Observability telemetry and unified event schemas across cards.",
            ),
            seg(
                "Speaker 1",
                "Let us lock the observability attributes before Friday's SDK cut.",
            ),
        ];
        let title = generate_meeting_title("", &[], &segments, Some(&hallucinating_apple))
            .expect("must fall back cleanly instead of returning Perso-Arabic hallucination");
        assert!(
            !contains_apple_intelligence_blocked_script(&title),
            "Latin-only backend must never return Perso-Arabic hallucination, got '{title}'"
        );
        assert!(title.contains("UI Components Observability"));
    }
}
