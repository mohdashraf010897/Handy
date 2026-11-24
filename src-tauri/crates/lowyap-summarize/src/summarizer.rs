//! Hierarchical map-reduce meeting summarizer with script-aware backend
//! routing and deterministic multilingual fallback (`SV-90`).

use std::collections::{HashMap, HashSet};

use lowyap_ports::{
    HybridNotesRequest, MeetingTemplate, NoteSummarizer, PortError, SummaryOutput,
    TranscriptSegment, WindowSummaryRequest,
};

use crate::budget::{
    chunk_rollups_by_budget, chunk_segments_by_budget, contains_apple_intelligence_blocked_script,
    segment_formatted_chars, truncate_chars_with_ellipsis,
};
use crate::local_gguf::{strip_reasoning_and_fences, CompletionBackend};
use crate::templates::{
    normalize_expanded_notes_markdown, required_headings, template_label, validate_expanded_notes,
    validate_rollup_bullets,
};

/// Default per-prompt character budget for [`HybridMapReduceSummarizer`].
pub const DEFAULT_MAX_INPUT_CHARS: usize = 12_000;

/// Minimum viable character budget; smaller budgets return a context-overflow
/// error because no meaningful prompt framing fits.
pub const MIN_MAX_INPUT_CHARS: usize = 256;

const SOURCE_LOCAL_EXTRACTIVE: &str = "local_extractive";
const MAX_WINDOW_BULLETS: usize = 12;
const MAX_HIGHLIGHTS: usize = 12;
const MAX_ACTION_ITEMS: usize = 10;
const MAX_REDUCE_ROUNDS: usize = 6;
const RECENT_TAIL_KEEP_SEGMENTS: usize = 12;

/// Hierarchical map-reduce [`NoteSummarizer`] that chains zero or more
/// [`CompletionBackend`]s (such as a local loopback Qwen3-4B server and an
/// optional Apple Intelligence adapter) with a deterministic multilingual
/// synthesizer fallback.
///
/// Any transcript or rollup list exceeding `max_input_chars` is automatically
/// partitioned and reduced in UTF-8-safe chunks, guaranteeing zero context
/// overflows for meetings of arbitrary length.
pub struct HybridMapReduceSummarizer {
    backends: Vec<Box<dyn CompletionBackend>>,
    max_input_chars: usize,
}

impl Default for HybridMapReduceSummarizer {
    fn default() -> Self {
        Self::deterministic_only(DEFAULT_MAX_INPUT_CHARS)
    }
}

impl HybridMapReduceSummarizer {
    /// Creates a summarizer with an ordered fallback chain of completion
    /// backends and a strict per-prompt character budget.
    pub fn new(backends: Vec<Box<dyn CompletionBackend>>, max_input_chars: usize) -> Self {
        Self {
            backends,
            max_input_chars,
        }
    }

    /// Creates a summarizer with `backends` and [`DEFAULT_MAX_INPUT_CHARS`].
    pub fn with_default_budget(backends: Vec<Box<dyn CompletionBackend>>) -> Self {
        Self::new(backends, DEFAULT_MAX_INPUT_CHARS)
    }

    /// Creates a pure-Rust deterministic multilingual summarizer with no LLM
    /// backends attached.
    pub fn deterministic_only(max_input_chars: usize) -> Self {
        Self::new(Vec::new(), max_input_chars)
    }

    /// Returns the number of configured completion backends.
    pub fn backend_count(&self) -> usize {
        self.backends.len()
    }

    fn ensure_viable_budget(&self) -> Result<(), PortError> {
        if self.max_input_chars < MIN_MAX_INPUT_CHARS {
            return Err(format!(
                "context overflow: max_input_chars ({}) is below minimum viable budget ({MIN_MAX_INPUT_CHARS})",
                self.max_input_chars
            ));
        }
        Ok(())
    }

    fn try_backends<F>(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_tokens: u32,
        validator: F,
    ) -> Option<SummaryOutput>
    where
        F: Fn(&str) -> bool,
    {
        self.try_backends_mapped(
            system_prompt,
            user_prompt,
            max_tokens,
            str::to_owned,
            validator,
        )
    }

    fn try_backends_mapped<M, F>(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_tokens: u32,
        map_output: M,
        validator: F,
    ) -> Option<SummaryOutput>
    where
        M: Fn(&str) -> String,
        F: Fn(&str) -> bool,
    {
        if self.backends.is_empty() {
            return None;
        }

        let prompt_chars = system_prompt
            .chars()
            .count()
            .saturating_add(user_prompt.chars().count());
        let has_blocked_script = contains_apple_intelligence_blocked_script(system_prompt)
            || contains_apple_intelligence_blocked_script(user_prompt);
        let mut saw_latin_locale_rejection = false;

        for backend in &self.backends {
            if !backend.supports_non_latin_scripts() && has_blocked_script {
                continue;
            }
            let effective_sys = if backend.supports_non_latin_scripts() {
                std::borrow::Cow::Borrowed(system_prompt)
            } else {
                std::borrow::Cow::Owned(sanitize_system_prompt_for_latin_backend(system_prompt))
            };
            let effective_user = if backend.supports_non_latin_scripts() {
                std::borrow::Cow::Borrowed(user_prompt)
            } else {
                std::borrow::Cow::Owned(
                    crate::budget::fold_latin_diacritics_for_apple_intelligence(user_prompt),
                )
            };
            let effective_chars = if backend.supports_non_latin_scripts() {
                prompt_chars
            } else {
                effective_sys
                    .chars()
                    .count()
                    .saturating_add(effective_user.chars().count())
            };
            if effective_chars > backend.max_prompt_chars() {
                continue;
            }

            match backend.complete(&effective_sys, &effective_user, max_tokens) {
                Ok(raw) => {
                    let cleaned = strip_reasoning_and_fences(&raw);
                    let normalized = map_output(&cleaned);
                    if (!backend.supports_non_latin_scripts()
                        && contains_apple_intelligence_blocked_script(&normalized))
                        || !validator(&normalized)
                    {
                        continue;
                    }
                    return Some(SummaryOutput {
                        markdown: normalized,
                        source_id: backend.source_id().to_owned(),
                    });
                }
                Err(err) if !backend.supports_non_latin_scripts() => {
                    let lower = err.to_ascii_lowercase();
                    if lower.contains("unsupported language")
                        || lower.contains("locale")
                        || lower.contains("guardrail")
                    {
                        saw_latin_locale_rejection = true;
                    }
                }
                Err(_) => {}
            }
        }

        // Second pass: when no non-Latin-capable backend succeeded (e.g. only
        // on-device Apple Intelligence is available without local_gguf) and the
        // meeting has Perso-Arabic/Devanagari speech or triggered a locale block,
        // Romanize + ASCII-fold the prompt so Apple Intelligence produces clean
        // structured English notes preserving Romanized multilingual terms.
        if (has_blocked_script || saw_latin_locale_rejection)
            && has_substantive_meeting_content_for_romanized_pass(user_prompt)
        {
            let roman_sys = crate::budget::fold_latin_diacritics_for_apple_intelligence(
                &sanitize_system_prompt_for_latin_backend(system_prompt),
            );
            let roman_user = crate::budget::romanize_and_fold_for_apple_intelligence(user_prompt);
            if !contains_apple_intelligence_blocked_script(&roman_sys)
                && !contains_apple_intelligence_blocked_script(&roman_user)
            {
                let roman_chars = roman_sys
                    .chars()
                    .count()
                    .saturating_add(roman_user.chars().count());
                for backend in &self.backends {
                    if backend.supports_non_latin_scripts()
                        || roman_chars > backend.max_prompt_chars()
                    {
                        continue;
                    }
                    if let Ok(raw) = backend.complete(&roman_sys, &roman_user, max_tokens) {
                        let cleaned = strip_reasoning_and_fences(&raw);
                        let normalized = map_output(&cleaned);
                        if !contains_apple_intelligence_blocked_script(&normalized)
                            && validator(&normalized)
                        {
                            return Some(SummaryOutput {
                                markdown: normalized,
                                source_id: backend.source_id().to_owned(),
                            });
                        }
                    }
                }
            }
        }

        None
    }

    fn summarize_single_segment_chunk(
        &self,
        segments: &[TranscriptSegment],
        language_instruction: Option<&str>,
        max_bullets: usize,
    ) -> SummaryOutput {
        let system_prompt = build_window_system_prompt(language_instruction);
        let transcript = format_segments_for_prompt(segments);
        let user_prompt = format!(
            "Meeting transcript window. Return 3-{max_bullets} concise speaker-attributed bullets only:\n\n{transcript}"
        );

        if let Some(out) =
            self.try_backends(&system_prompt, &user_prompt, 512, validate_rollup_bullets)
        {
            return out;
        }

        SummaryOutput {
            markdown: synthesize_window_bullets(segments, max_bullets),
            source_id: SOURCE_LOCAL_EXTRACTIVE.to_owned(),
        }
    }

    fn reduce_rollup_strings(
        &self,
        rollups: Vec<String>,
        target_chars: usize,
        language_instruction: Option<&str>,
    ) -> Vec<String> {
        let mut current = rollups;
        let chunk_budget = target_chars.max(160);

        for _ in 0..MAX_REDUCE_ROUNDS {
            let total_chars: usize = current.iter().map(|r| r.chars().count() + 2).sum();
            if total_chars <= target_chars {
                break;
            }

            let groups = chunk_rollups_by_budget(&current, chunk_budget);
            let mut next_round = Vec::with_capacity(groups.len());

            for group in groups {
                let merged_text = group.join("\n");
                let system_prompt = build_rollup_reduce_system_prompt(language_instruction);
                let user_prompt = format!(
                    "Merge and deduplicate these rolling meeting summaries into concise bullets while preserving every distinct speaker, decision, action item, and non-English term:\n\n{merged_text}"
                );

                let reduced = self
                    .try_backends(&system_prompt, &user_prompt, 512, validate_rollup_bullets)
                    .map(|out| out.markdown)
                    .unwrap_or_else(|| merge_and_dedupe_rollup_lines(&group, MAX_WINDOW_BULLETS));

                next_round.push(reduced);
            }

            let next_total: usize = next_round.iter().map(|r| r.chars().count() + 2).sum();
            current = next_round;

            // If a single group already converged or stopped shrinking, apply a
            // deterministic budget-safe merge across all remaining bullets.
            if next_total >= total_chars || current.len() <= 1 {
                if next_total > target_chars {
                    let merged = merge_and_dedupe_rollup_lines(&current, MAX_WINDOW_BULLETS);
                    let bounded = truncate_lines_to_budget(&merged, target_chars);
                    current = if bounded.is_empty() {
                        Vec::new()
                    } else {
                        vec![bounded]
                    };
                }
                break;
            }
        }

        current
    }
}

impl NoteSummarizer for HybridMapReduceSummarizer {
    fn name(&self) -> &'static str {
        "hybrid_map_reduce_summarizer"
    }

    fn max_input_chars(&self) -> usize {
        self.max_input_chars
    }

    fn summarize_window(&self, req: &WindowSummaryRequest) -> Result<SummaryOutput, PortError> {
        self.ensure_viable_budget()?;

        let non_empty: Vec<TranscriptSegment> = req
            .segments
            .iter()
            .filter(|s| !s.text.trim().is_empty())
            .cloned()
            .collect();

        if non_empty.is_empty() {
            return Ok(SummaryOutput {
                markdown: "- [00:00] No clear speech captured in this window.".to_owned(),
                source_id: SOURCE_LOCAL_EXTRACTIVE.to_owned(),
            });
        }

        let prompt_overhead = 420
            + req
                .language_instruction
                .as_deref()
                .map_or(0, |s| s.chars().count());
        let chunk_budget = self
            .max_input_chars
            .saturating_sub(prompt_overhead)
            .max(128);

        let formatted_chars: usize = non_empty.iter().map(segment_formatted_chars).sum();
        if formatted_chars <= chunk_budget {
            return Ok(self.summarize_single_segment_chunk(
                &non_empty,
                req.language_instruction.as_deref(),
                MAX_WINDOW_BULLETS,
            ));
        }

        // Map phase: summarize each budget-safe sub-window.
        let sub_windows = chunk_segments_by_budget(&non_empty, chunk_budget);
        let mut partial_rollups = Vec::with_capacity(sub_windows.len());
        let mut backend_source: Option<String> = None;

        for sub in &sub_windows {
            let out = self.summarize_single_segment_chunk(
                sub,
                req.language_instruction.as_deref(),
                MAX_WINDOW_BULLETS,
            );
            if out.source_id != SOURCE_LOCAL_EXTRACTIVE {
                backend_source = Some(out.source_id);
            }
            partial_rollups.push(out.markdown);
        }

        // Reduce phase: hierarchically compact partials to fit within budget.
        let reduced_rollups = self.reduce_rollup_strings(
            partial_rollups,
            chunk_budget,
            req.language_instruction.as_deref(),
        );
        let merged_input = reduced_rollups.join("\n");

        if !self.backends.is_empty() {
            let system_prompt =
                build_rollup_reduce_system_prompt(req.language_instruction.as_deref());
            let user_prompt = format!(
                "Produce at most {MAX_WINDOW_BULLETS} deduplicated bullets preserving all distinct speakers, decisions, action items, and multilingual terms:\n\n{merged_input}"
            );
            if let Some(out) =
                self.try_backends(&system_prompt, &user_prompt, 512, validate_rollup_bullets)
            {
                return Ok(out);
            }
        }

        let markdown = merge_and_dedupe_rollup_lines(&reduced_rollups, MAX_WINDOW_BULLETS);
        Ok(SummaryOutput {
            markdown,
            source_id: backend_source.unwrap_or_else(|| SOURCE_LOCAL_EXTRACTIVE.to_owned()),
        })
    }

    fn expand_hybrid_notes(&self, req: &HybridNotesRequest) -> Result<SummaryOutput, PortError> {
        self.ensure_viable_budget()?;

        let non_empty_tail: Vec<TranscriptSegment> = req
            .tail_segments
            .iter()
            .filter(|s| !s.text.trim().is_empty())
            .cloned()
            .collect();

        let lang_chars = req
            .language_instruction
            .as_deref()
            .map_or(0, |s| s.chars().count());
        let framing_overhead = 560 + lang_chars + req.title.chars().count().min(140);
        let content_budget = self
            .max_input_chars
            .saturating_sub(framing_overhead)
            .max(192);

        let rough_budget = (content_budget / 3).max(96);
        let bounded_rough = truncate_chars_with_ellipsis(req.rough_notes.trim(), rough_budget);

        let mut working_rollups: Vec<String> = req
            .rollups
            .iter()
            .filter(|r| !r.trim().is_empty())
            .cloned()
            .collect();

        let tail_chars: usize = non_empty_tail.iter().map(segment_formatted_chars).sum();
        let rollups_chars: usize = working_rollups.iter().map(|r| r.chars().count() + 2).sum();
        let total_content = bounded_rough
            .chars()
            .count()
            .saturating_add(rollups_chars)
            .saturating_add(tail_chars);

        // Step 1: If the combined input exceeds budget and tail_segments is large,
        // map-reduce earlier tail segments into rollups while keeping a bounded
        // recent tail slice.
        let effective_tail: Vec<TranscriptSegment> =
            if total_content > content_budget && !non_empty_tail.is_empty() {
                let tail_budget = (content_budget / 3).max(128);
                let (earlier, recent) = split_tail_to_budget(&non_empty_tail, tail_budget);

                if !earlier.is_empty() {
                    let window_budget = content_budget.saturating_sub(128).max(128);
                    let chunks = chunk_segments_by_budget(earlier, window_budget);
                    for (idx, chunk) in chunks.into_iter().enumerate() {
                        let sub_req = WindowSummaryRequest {
                            window_index: idx as u32,
                            segments: chunk,
                            language_instruction: req.language_instruction.clone(),
                        };
                        let out = self.summarize_window(&sub_req)?;
                        working_rollups.push(out.markdown);
                    }
                }
                recent
            } else {
                non_empty_tail
            };

        // Step 2: Hierarchically reduce rollups if rollups + rough + tail still
        // exceed the remaining prompt budget.
        let effective_tail_chars: usize = effective_tail.iter().map(segment_formatted_chars).sum();
        let rollup_budget = content_budget
            .saturating_sub(bounded_rough.chars().count())
            .saturating_sub(effective_tail_chars)
            .max(128);
        let reduced_rollups = self.reduce_rollup_strings(
            working_rollups,
            rollup_budget,
            req.language_instruction.as_deref(),
        );

        // Step 3: Attempt LLM synthesis with exact template validation.
        if !self.backends.is_empty() {
            let (system_prompt, user_prompt) = build_hybrid_prompts(
                &req.title,
                req.template,
                &bounded_rough,
                &reduced_rollups,
                &effective_tail,
                req.language_instruction.as_deref(),
            );
            let has_rough_notes = !bounded_rough.trim().is_empty();
            if let Some(out) = self.try_backends_mapped(
                &system_prompt,
                &user_prompt,
                1_536,
                |md| normalize_expanded_notes_markdown(md, req.template, has_rough_notes),
                |md| validate_expanded_notes(md, req.template),
            ) {
                return Ok(out);
            }
        }

        // Step 4: Deterministic multilingual synthesizer fallback.
        let markdown = synthesize_hybrid_markdown(
            &req.title,
            req.template,
            &req.rough_notes,
            &reduced_rollups,
            &effective_tail,
        );

        Ok(SummaryOutput {
            markdown,
            source_id: SOURCE_LOCAL_EXTRACTIVE.to_owned(),
        })
    }
}

/// Splits `segments` into `(earlier, recent)` where `recent` contains at most
/// `RECENT_TAIL_KEEP_SEGMENTS` trailing segments whose combined formatted chars
/// stay within `tail_budget`.
fn split_tail_to_budget(
    segments: &[TranscriptSegment],
    tail_budget: usize,
) -> (&[TranscriptSegment], Vec<TranscriptSegment>) {
    let mut keep_count = 0usize;
    let mut used_chars = 0usize;

    for seg in segments.iter().rev().take(RECENT_TAIL_KEEP_SEGMENTS) {
        let cost = segment_formatted_chars(seg);
        if used_chars.saturating_add(cost) > tail_budget {
            break;
        }
        used_chars = used_chars.saturating_add(cost);
        keep_count += 1;
    }

    let split_at = segments.len().saturating_sub(keep_count);
    (&segments[..split_at], segments[split_at..].to_vec())
}

fn build_window_system_prompt(language_instruction: Option<&str>) -> String {
    let mut prompt = String::from(
        "You summarize a meeting transcript window for local-first notes. \
Return only concise bullet points using `- **[Speaker]** ...` format. \
Preserve speaker attribution, concrete decisions, action items, and every multilingual term verbatim. \
Do not speculate or invent facts.",
    );
    if let Some(inst) = language_instruction
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        prompt.push(' ');
        prompt.push_str(inst);
    }
    prompt
}

fn build_rollup_reduce_system_prompt(language_instruction: Option<&str>) -> String {
    let mut prompt = String::from(
        "You merge rolling meeting summary windows into a compact deduplicated bullet list \
using `- **[Speaker]** ...` format. Preserve all distinct speakers, decisions, action items, \
and non-English/multilingual terms verbatim.",
    );
    if let Some(inst) = language_instruction
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        prompt.push(' ');
        prompt.push_str(inst);
    }
    prompt
}

fn build_hybrid_prompts(
    title: &str,
    template: MeetingTemplate,
    rough_notes: &str,
    rollups: &[String],
    tail_segments: &[TranscriptSegment],
    language_instruction: Option<&str>,
) -> (String, String) {
    let headings = required_headings(template).join("\n");
    let mut system_prompt = format!(
        "You expand rough meeting notes into structured hybrid meeting notes using ONLY the provided \
rough notes, rolling summaries, and latest transcript tail. Keep speaker attribution explicit and \
preserve multilingual terms verbatim. Do not invent facts.\n\nRequired headings (exact):\n{headings}"
    );
    if let Some(inst) = language_instruction
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(inst);
    }

    let rough_block = if rough_notes.trim().is_empty() {
        "- (none)".to_owned()
    } else {
        rough_notes.trim().to_owned()
    };
    let rollups_block = if rollups.is_empty() {
        "- (none)".to_owned()
    } else {
        rollups.join("\n")
    };
    let tail_block = if tail_segments.is_empty() {
        "(none)".to_owned()
    } else {
        format_segments_for_prompt(tail_segments)
    };

    let user_prompt = format!(
        "Meeting title: {}\nTemplate: {}\n\nRough notes:\n{}\n\nRolling summaries:\n{}\n\nLatest transcript tail:\n{}\n\nReturn Markdown with the exact required headings.",
        one_line(title, 140),
        template_label(template),
        rough_block,
        rollups_block,
        tail_block
    );

    (system_prompt, user_prompt)
}

fn format_segments_for_prompt(segments: &[TranscriptSegment]) -> String {
    let mut out = String::new();
    for seg in segments {
        if seg.text.trim().is_empty() {
            continue;
        }
        out.push_str(&format!(
            "[{}] [{}] {}\n",
            format_mm_ss(seg.start_ms),
            seg.speaker.display_label,
            one_line(&seg.text, 280)
        ));
    }
    out
}

/// Script bucket used to guarantee script-diverse selection during deterministic
/// compaction so non-Latin turns are never crowded out by earlier Latin turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ScriptBucket {
    PersoArabic,
    Devanagari,
    OtherNonLatin,
    Latin,
}

fn classify_script(text: &str) -> ScriptBucket {
    for ch in text.chars() {
        if matches!(
            ch,
            '\u{0600}'..='\u{06FF}'
                | '\u{0750}'..='\u{077F}'
                | '\u{08A0}'..='\u{08FF}'
                | '\u{FB50}'..='\u{FDFF}'
                | '\u{FE70}'..='\u{FEFF}'
        ) {
            return ScriptBucket::PersoArabic;
        }
        if ('\u{0900}'..='\u{097F}').contains(&ch) {
            return ScriptBucket::Devanagari;
        }
        if ch as u32 >= 0x0590 && !ch.is_ascii() && ch.is_alphabetic() {
            return ScriptBucket::OtherNonLatin;
        }
    }
    ScriptBucket::Latin
}

/// Normalizes a transcript or bullet line into a semantic deduplication key,
/// stripping bullet symbols, `[min XX]` / `[MM:SS]` prefixes, and speaker tags
/// while preserving all Unicode words.
fn normalize_body_key(raw: &str) -> String {
    let mut rest = raw.trim();
    loop {
        let prev = rest;
        rest = rest
            .trim_start_matches(['-', '*', '•', '–', '—'])
            .trim_start();

        if rest.starts_with('[') {
            if let Some(close) = rest.find(']') {
                rest = rest[close + 1..].trim_start();
            }
        }

        if let Some(after_bold) = rest.strip_prefix("**") {
            if let Some(close) = after_bold.find("**") {
                let tail = after_bold[close + 2..].trim_start();
                rest = tail.strip_prefix(':').map_or(tail, str::trim_start);
            }
        }

        if rest == prev {
            break;
        }
    }

    rest.split_whitespace()
        .map(|w| {
            w.chars()
                .map(|c| c.to_ascii_lowercase())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug)]
struct CandidateBullet {
    rendered: String,
    key: String,
    speaker: String,
    script: ScriptBucket,
    is_action_or_decision: bool,
}

fn synthesize_window_bullets(segments: &[TranscriptSegment], limit: usize) -> String {
    let mut unique: Vec<CandidateBullet> = Vec::new();
    let mut seen = HashSet::new();

    for seg in segments {
        let trimmed = seg.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = normalize_body_key(trimmed);
        if key.is_empty() || !seen.insert(key.clone()) {
            continue;
        }
        let rendered = format!(
            "- **[{}]** [{}]: {}",
            seg.speaker.display_label,
            format_mm_ss(seg.start_ms),
            one_line(trimmed, 240)
        );
        unique.push(CandidateBullet {
            rendered,
            key,
            speaker: seg.speaker.display_label.clone(),
            script: classify_script(trimmed),
            is_action_or_decision: looks_like_action_or_decision(trimmed),
        });
    }

    let selected = select_diverse_bullets(&unique, limit);
    if selected.is_empty() {
        "- [00:00] No clear speech captured in this window.".to_owned()
    } else {
        selected.join("\n")
    }
}

fn merge_and_dedupe_rollup_lines(rollups: &[String], limit: usize) -> String {
    let mut unique: Vec<CandidateBullet> = Vec::new();
    let mut seen = HashSet::new();

    for rollup in rollups {
        for line in rollup.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let key = normalize_body_key(trimmed);
            if key.is_empty() || !seen.insert(key.clone()) {
                continue;
            }
            let stripped = trimmed.trim_start_matches(['-', '*', '•']).trim_start();
            let rendered = format!("- {}", one_line(stripped, 240));
            unique.push(CandidateBullet {
                rendered,
                key,
                speaker: extract_speaker_tag(stripped),
                script: classify_script(stripped),
                is_action_or_decision: looks_like_action_or_decision(stripped),
            });
        }
    }

    let selected = select_diverse_bullets(&unique, limit);
    if selected.is_empty() {
        "- No summary bullets captured.".to_owned()
    } else {
        selected.join("\n")
    }
}

/// Selects up to `limit` unique bullets while guaranteeing coverage across
/// distinct scripts, distinct speakers, and action/decision cues before filling
/// remaining slots in chronological order.
fn select_diverse_bullets(candidates: &[CandidateBullet], limit: usize) -> Vec<String> {
    if candidates.len() <= limit {
        return candidates.iter().map(|c| c.rendered.clone()).collect();
    }

    let mut chosen_indices: Vec<usize> = Vec::with_capacity(limit);
    let mut chosen_set: HashSet<usize> = HashSet::new();
    let mut seen_scripts: HashSet<ScriptBucket> = HashSet::new();
    let mut seen_speakers: HashSet<String> = HashSet::new();

    // Pass 1: Ensure every present script bucket is represented (round-robin up
    // to 2 items per non-Latin bucket if available so both Urdu & Arabic survive).
    let mut script_counts: HashMap<ScriptBucket, usize> = HashMap::new();
    for (idx, item) in candidates.iter().enumerate() {
        if chosen_indices.len() >= limit {
            break;
        }
        let bucket_cap = if item.script == ScriptBucket::Latin {
            1
        } else {
            2
        };
        let count = script_counts.entry(item.script).or_insert(0);
        if *count < bucket_cap {
            *count += 1;
            seen_scripts.insert(item.script);
            seen_speakers.insert(item.speaker.clone());
            chosen_set.insert(idx);
            chosen_indices.push(idx);
        }
    }

    // Pass 2: Ensure every distinct speaker is represented.
    for (idx, item) in candidates.iter().enumerate() {
        if chosen_indices.len() >= limit {
            break;
        }
        if !chosen_set.contains(&idx)
            && !item.speaker.is_empty()
            && seen_speakers.insert(item.speaker.clone())
        {
            chosen_set.insert(idx);
            chosen_indices.push(idx);
        }
    }

    // Pass 3: Prioritize action/decision items.
    for (idx, item) in candidates.iter().enumerate() {
        if chosen_indices.len() >= limit {
            break;
        }
        if !chosen_set.contains(&idx) && item.is_action_or_decision {
            chosen_set.insert(idx);
            chosen_indices.push(idx);
        }
    }

    // Pass 4: Fill remaining slots in original order.
    for idx in 0..candidates.len() {
        if chosen_indices.len() >= limit {
            break;
        }
        if chosen_set.insert(idx) {
            chosen_indices.push(idx);
        }
    }

    chosen_indices.sort_unstable();
    let _ = seen_scripts;
    chosen_indices
        .into_iter()
        .map(|idx| candidates[idx].rendered.clone())
        .collect()
}

fn has_substantive_meeting_content_for_romanized_pass(text: &str) -> bool {
    let mut latin_alpha = 0usize;
    let mut non_latin_alpha = 0usize;
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&ch) {
            latin_alpha += 1;
        } else if ch.is_alphabetic() {
            non_latin_alpha += 1;
        }
    }
    let total_alpha = latin_alpha + non_latin_alpha;
    // Only Romanize for a Latin-only 3B backend when the transcript is
    // predominantly English (>= 120 alphabetic chars AND <= 25% non-Latin script).
    // Heavy Arabic/Urdu/Hindi meetings (> 25% non-Latin) become phonetic
    // transliteration that an English-only 3B model cannot comprehend—causing
    // either 45s locale-guard timeouts or hallucinated generic corporate notes.
    // Falling through to `local_extractive` preserves original script, speaker
    // attribution, and verbatim bilingual highlights in <100ms.
    total_alpha >= 120 && non_latin_alpha.saturating_mul(4) <= total_alpha
}

/// Replaces non-Latin script instructions (`Perso-Arabic` / `Devanagari`) with an
/// explicit Latin-script directive when invoking a Latin-only backend such as
/// on-device Apple Intelligence, preventing FoundationModels `Unsupported language`
/// rejections or garbled Perso-Arabic output.
fn sanitize_system_prompt_for_latin_backend(system_prompt: &str) -> String {
    let romanized = lowyap_flow::hindustani_to_roman_hinglish(system_prompt);
    if !romanized.contains("Perso-Arabic") && !romanized.contains("Devanagari") {
        return romanized;
    }

    let mut kept: Vec<&str> = Vec::new();
    for line in romanized.lines() {
        if line.contains("Perso-Arabic") || line.contains("Devanagari") {
            kept.push(
                "Write all headings and bullet points in clear English (Latin script), preserving speaker labels, technical terms, and Romanized Hinglish/multilingual terms accurately. Never output Perso-Arabic or Devanagari characters.",
            );
        } else {
            kept.push(line);
        }
    }
    kept.join("\n")
}

fn truncate_lines_to_budget(multiline: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for line in multiline.lines() {
        let addition = if out.is_empty() {
            line.chars().count()
        } else {
            line.chars().count() + 1
        };
        if out.chars().count().saturating_add(addition) > max_chars {
            if out.is_empty() {
                return truncate_chars_with_ellipsis(line, max_chars);
            }
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

fn synthesize_hybrid_markdown(
    title: &str,
    template: MeetingTemplate,
    rough_notes: &str,
    rollups: &[String],
    tail_segments: &[TranscriptSegment],
) -> String {
    let normalized_title = if title.trim().is_empty() {
        "Meeting Notes"
    } else {
        title.trim()
    };

    let all_candidates = collect_all_candidates(rollups, tail_segments);
    let highlights = select_diverse_bullets(&all_candidates, MAX_HIGHLIGHTS);
    let decisions = select_decisions_and_discussion(&all_candidates, MAX_HIGHLIGHTS);
    let rough_expansions = expand_rough_bullets(rough_notes, &all_candidates);
    let actions = extract_multilingual_actions(rough_notes, &all_candidates, MAX_ACTION_ITEMS);

    let mut md = String::new();
    md.push_str(&format!("# {normalized_title}\n\n"));

    let headings = required_headings(template);
    match template {
        MeetingTemplate::General => {
            write_section(
                &mut md,
                headings[0],
                &highlights,
                "No major updates were captured.",
            );
            write_section(
                &mut md,
                headings[1],
                &decisions,
                "No explicit decisions were captured in the summarized windows.",
            );
            write_section(
                &mut md,
                headings[2],
                &rough_expansions,
                "No live rough bullets were captured.",
            );
            write_section(
                &mut md,
                headings[3],
                &actions,
                "No explicit action items were detected.",
            );
        }
        MeetingTemplate::OneOnOne => {
            write_section(
                &mut md,
                headings[0],
                &highlights,
                "No wins were clearly stated.",
            );
            write_section(
                &mut md,
                headings[1],
                if rough_expansions.is_empty() {
                    &decisions
                } else {
                    &rough_expansions
                },
                "No blockers or coaching topics were captured.",
            );
            write_section(
                &mut md,
                headings[2],
                &decisions,
                "No direct feedback items were captured.",
            );
            write_section(
                &mut md,
                headings[3],
                &actions,
                "No next steps were explicitly identified.",
            );
        }
        MeetingTemplate::Standup => {
            write_section(
                &mut md,
                headings[0],
                &highlights,
                "No concrete progress update was captured.",
            );
            write_section(
                &mut md,
                headings[1],
                if rough_expansions.is_empty() {
                    &decisions
                } else {
                    &rough_expansions
                },
                "No explicit plan items were captured in rough notes.",
            );
            write_section(
                &mut md,
                headings[2],
                &actions,
                "No blockers or risks were explicitly detected.",
            );
        }
        MeetingTemplate::LanguageLesson => {
            write_section(
                &mut md,
                headings[0],
                &highlights,
                "No clear lesson summary was captured.",
            );
            write_section(
                &mut md,
                headings[1],
                if rough_expansions.is_empty() {
                    &highlights
                } else {
                    &rough_expansions
                },
                "No new vocabulary or phrases were clearly noted.",
            );
            write_section(
                &mut md,
                headings[2],
                &decisions,
                "No explicit grammar or correction snippets were captured.",
            );
            write_section(
                &mut md,
                headings[3],
                &actions,
                "No homework actions were explicitly assigned.",
            );
        }
        MeetingTemplate::ClientCall => {
            write_section(
                &mut md,
                headings[0],
                &highlights,
                "No explicit client pain points were captured.",
            );
            write_section(
                &mut md,
                headings[1],
                if rough_expansions.is_empty() {
                    &decisions
                } else {
                    &rough_expansions
                },
                "No scope alignment notes were captured in rough bullets.",
            );
            write_section(
                &mut md,
                headings[2],
                &decisions,
                "No explicit objections or open questions were captured.",
            );
            write_section(
                &mut md,
                headings[3],
                &actions,
                "No follow-up owners were explicitly identified.",
            );
        }
    }

    md
}

fn write_section(buf: &mut String, heading: &str, bullets: &[String], fallback: &str) {
    buf.push_str(heading);
    buf.push('\n');
    if bullets.is_empty() {
        buf.push_str("- ");
        buf.push_str(fallback);
        buf.push('\n');
    } else {
        for bullet in bullets {
            let trimmed = bullet.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('-') {
                buf.push_str(trimmed);
            } else {
                buf.push_str("- ");
                buf.push_str(trimmed);
            }
            buf.push('\n');
        }
    }
    buf.push('\n');
}

fn collect_all_candidates(
    rollups: &[String],
    tail_segments: &[TranscriptSegment],
) -> Vec<CandidateBullet> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for rollup in rollups {
        for line in rollup.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let key = normalize_body_key(trimmed);
            if key.is_empty() || !seen.insert(key.clone()) {
                continue;
            }
            let stripped = trimmed.trim_start_matches(['-', '*', '•']).trim_start();
            out.push(CandidateBullet {
                rendered: format!("- {}", one_line(stripped, 240)),
                key,
                speaker: extract_speaker_tag(stripped),
                script: classify_script(stripped),
                is_action_or_decision: looks_like_action_or_decision(stripped),
            });
        }
    }

    for seg in tail_segments {
        let trimmed = seg.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = normalize_body_key(trimmed);
        if key.is_empty() || !seen.insert(key.clone()) {
            continue;
        }
        out.push(CandidateBullet {
            rendered: format!(
                "- **[{}]** [{}]: {}",
                seg.speaker.display_label,
                format_mm_ss(seg.start_ms),
                one_line(trimmed, 240)
            ),
            key,
            speaker: seg.speaker.display_label.clone(),
            script: classify_script(trimmed),
            is_action_or_decision: looks_like_action_or_decision(trimmed),
        });
    }

    out
}

fn select_decisions_and_discussion(candidates: &[CandidateBullet], limit: usize) -> Vec<String> {
    let mut prioritized: Vec<CandidateBullet> = candidates
        .iter()
        .filter(|c| looks_like_decision(&c.rendered))
        .cloned()
        .collect();
    let mut seen: HashSet<String> = prioritized.iter().map(|c| c.key.clone()).collect();

    for item in candidates {
        if seen.insert(item.key.clone()) {
            prioritized.push(item.clone());
        }
    }

    select_diverse_bullets(&prioritized, limit)
}

fn expand_rough_bullets(rough_notes: &str, candidates: &[CandidateBullet]) -> Vec<String> {
    let bullets = parse_rough_bullets(rough_notes);
    let mut expanded = Vec::with_capacity(bullets.len());

    for (idx, bullet) in bullets.iter().enumerate() {
        let tokens = tokenize_lower(bullet);
        let best = candidates
            .iter()
            .filter_map(|cand| {
                let lower = cand.rendered.to_lowercase();
                let score = tokens
                    .iter()
                    .filter(|tok| tok.chars().count() > 2 && lower.contains(tok.as_str()))
                    .count();
                (score > 0).then_some((score, cand))
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, cand)| cand)
            .or_else(|| {
                if candidates.is_empty() {
                    None
                } else {
                    Some(&candidates[idx % candidates.len()])
                }
            });

        match best {
            Some(evidence) => {
                let evidence_body = evidence.rendered.trim_start_matches('-').trim();
                expanded.push(format!(
                    "- {}\n  - Evidence: {}",
                    one_line(bullet, 180),
                    one_line(evidence_body, 210)
                ));
            }
            None => expanded.push(format!("- {}", one_line(bullet, 180))),
        }
    }

    expanded
}

fn extract_multilingual_actions(
    rough_notes: &str,
    candidates: &[CandidateBullet],
    limit: usize,
) -> Vec<String> {
    let mut actions = Vec::new();
    let mut seen = HashSet::new();

    for cand in candidates {
        if looks_like_action(&cand.rendered) && seen.insert(cand.key.clone()) {
            actions.push(cand.rendered.clone());
            if actions.len() >= limit {
                return actions;
            }
        }
    }

    for bullet in parse_rough_bullets(rough_notes) {
        if looks_like_action(&bullet) {
            let key = normalize_body_key(&bullet);
            if seen.insert(key) {
                actions.push(format!("- **[Me]**: {}", one_line(&bullet, 200)));
                if actions.len() >= limit {
                    return actions;
                }
            }
        }
    }

    actions
}

fn parse_rough_bullets(raw: &str) -> Vec<String> {
    let mut bullets = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed
            .trim_start_matches(['-', '*', '•', '–', '—'])
            .trim_start()
            .to_owned();
        if !normalized.is_empty() {
            bullets.push(normalized);
        }
    }
    bullets
}

fn extract_speaker_tag(text: &str) -> String {
    if let Some(after_open) = text.strip_prefix("**[") {
        if let Some(end) = after_open.find("]**") {
            return after_open[..end].to_owned();
        }
    }
    if let Some(after_open) = text.strip_prefix("**") {
        if let Some(end) = after_open.find("**") {
            return after_open[..end].trim_matches(['[', ']']).to_owned();
        }
    }
    String::new()
}

fn looks_like_action(text: &str) -> bool {
    let lower = text.to_lowercase();
    let ascii_cues = [
        "action item",
        "action:",
        "follow up",
        "follow-up",
        "need to",
        "will ",
        "let's",
        "todo",
        "next step",
        "karna hai",
        "rakhna hai",
        "bhej",
    ];
    let unicode_cues = [
        "اگلا قدم",
        "مراجعة",
        "خطة",
        "متابعة",
        "करना है",
        "रखना है",
        "अगला कदम",
    ];

    ascii_cues.iter().any(|cue| lower.contains(cue))
        || unicode_cues.iter().any(|cue| text.contains(cue))
}

fn looks_like_decision(text: &str) -> bool {
    let lower = text.to_lowercase();
    let ascii_cues = ["decision", "decided", "agreed", "must drop", "target"];
    let unicode_cues = ["فیصلہ", "قرار", "निर्णय"];
    ascii_cues.iter().any(|cue| lower.contains(cue))
        || unicode_cues.iter().any(|cue| text.contains(cue))
}

fn looks_like_action_or_decision(text: &str) -> bool {
    looks_like_action(text) || looks_like_decision(text)
}

fn tokenize_lower(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn one_line(text: &str, max_chars: usize) -> String {
    let flattened: String = text
        .chars()
        .map(|ch| if ch == '\n' || ch == '\r' { ' ' } else { ch })
        .collect();
    let collapsed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars_with_ellipsis(&collapsed, max_chars)
}

fn format_mm_ss(ms: u64) -> String {
    let total_sec = ms / 1_000;
    let minutes = total_sec / 60;
    let seconds = total_sec % 60;
    format!("{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use lowyap_ports::{SpeakerChannel, SpeakerIdentity};

    use super::*;

    const SCENARIO_5_MARKERS: [&str; 4] = ["AEC3", "demo", "میٹنگ نوٹس", "مراجعة خطة الإطلاق"];

    fn scenario_5_segments() -> Vec<TranscriptSegment> {
        let templates: [(&str, SpeakerChannel, &str, &str); 6] = [
            (
                "me",
                SpeakerChannel::Me,
                "en",
                "Action item: ship the AEC3 adapter and keep end-to-end echo leak under five percent before Friday's milestone review.",
            ),
            (
                "speaker_1",
                SpeakerChannel::Others,
                "hi-Latn",
                "Bhai kal ka demo bilkul ready rakhna hai, pipeline smooth chal rahi hai aur latency target forty milliseconds se kam hai.",
            ),
            (
                "speaker_2",
                SpeakerChannel::Others,
                "ur",
                "اگلا قدم: میٹنگ نوٹس کا معیار بہتر بنانا اور اردو و انگریزی اصطلاحات کو محفوظ رکھنا ہے تاکہ کوئی تفصیل ضائع نہ ہو۔",
            ),
            (
                "me",
                SpeakerChannel::Me,
                "en",
                "Decision: VAD gating must drop silent spans below minus forty-five dBFS so Whisper never hallucinates phantom Thank you lines.",
            ),
            (
                "speaker_1",
                SpeakerChannel::Others,
                "ar",
                "مراجعة خطة الإطلاق الأسبوعية مع التأكد من استقرار تسجيل الصوت المزدوج وعدم تجاوز حد السياق للنموذج المحلي.",
            ),
            (
                "speaker_2",
                SpeakerChannel::Others,
                "en",
                "Follow up on speaker enrollment: clean ME_ONLY windows should anchor the local centroid while separating remote voices.",
            ),
        ];

        let mut segments = Vec::with_capacity(195);
        for idx in 0..195usize {
            let (speaker_id, channel, lang, base_text) = templates[idx % templates.len()];
            let start_ms = (idx as u64) * 20_000;
            let display_label = match speaker_id {
                "me" => "Me",
                "speaker_1" => "Speaker 1",
                _ => "Speaker 2",
            };
            segments.push(TranscriptSegment {
                speaker: SpeakerIdentity {
                    channel,
                    speaker_id: speaker_id.to_owned(),
                    display_label: display_label.to_owned(),
                    confidence: Some(0.92),
                },
                start_ms,
                end_ms: start_ms + 18_000,
                text: format!("[min {:02}] {base_text}", start_ms / 60_000),
                language: Some(lang.to_owned()),
            });
        }
        segments
    }

    #[test]
    fn scenario_5_single_shot_65_minute_meeting_succeeds_across_all_templates() {
        let summarizer = HybridMapReduceSummarizer::deterministic_only(12_000);
        let segments = scenario_5_segments();

        let templates = [
            MeetingTemplate::General,
            MeetingTemplate::OneOnOne,
            MeetingTemplate::Standup,
            MeetingTemplate::LanguageLesson,
            MeetingTemplate::ClientCall,
        ];

        for template in templates {
            let req = HybridNotesRequest {
                title: "Q4 Audio Modularity & Multilingual Rollout".to_owned(),
                template,
                rough_notes: "Check AEC3 ship date, demo readiness, and Urdu/Arabic preservation."
                    .to_owned(),
                rollups: Vec::new(),
                tail_segments: segments.clone(),
                language_instruction: Some(
                    "Preserve English, Roman Hindi/Urdu, and Arabic/Urdu terms verbatim."
                        .to_owned(),
                ),
            };

            let out = summarizer
                .expand_hybrid_notes(&req)
                .expect("65-min single-shot meeting must never overflow");
            assert_eq!(out.source_id, "local_extractive");
            assert!(
                validate_expanded_notes(&out.markdown, template),
                "invalid template output for {:?}:\n{}",
                template,
                out.markdown
            );

            for marker in SCENARIO_5_MARKERS {
                assert!(
                    out.markdown.contains(marker),
                    "missing marker '{marker}' in {:?} single-shot output:\n{}",
                    template,
                    out.markdown
                );
            }

            // Deduplication check: 195 repetitive segments must NOT explode into 195 bullets.
            let bullet_count = out
                .markdown
                .lines()
                .filter(|line| line.trim_start().starts_with("- "))
                .count();
            assert!(
                bullet_count <= 40,
                "expected deduplicated concise notes, got {bullet_count} bullets"
            );
        }
    }

    #[test]
    fn scenario_5_rolling_windows_preserve_all_four_markers_with_zero_overflows() {
        let summarizer = HybridMapReduceSummarizer::deterministic_only(4_000);
        let segments = scenario_5_segments();

        // Simulate 13 five-minute windows (15 segments each) -> rollups + final tail.
        let mut rollups = Vec::new();
        for (window_index, window) in segments[..180].chunks(15).enumerate() {
            let out = summarizer
                .summarize_window(&WindowSummaryRequest {
                    window_index: window_index as u32,
                    segments: window.to_vec(),
                    language_instruction: Some("Preserve all languages verbatim.".to_owned()),
                })
                .expect("window summary should not overflow");
            assert!(validate_rollup_bullets(&out.markdown));
            rollups.push(out.markdown);
        }

        let hybrid = summarizer
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Q4 Audio Modularity & Multilingual Rollout".to_owned(),
                template: MeetingTemplate::General,
                rough_notes: "Check AEC3 ship date, demo readiness, and Urdu/Arabic preservation."
                    .to_owned(),
                rollups,
                tail_segments: segments[180..].to_vec(),
                language_instruction: Some("Preserve all languages verbatim.".to_owned()),
            })
            .expect("rolling-window hybrid notes should succeed with 0 overflows");

        assert!(validate_expanded_notes(
            &hybrid.markdown,
            MeetingTemplate::General
        ));
        for marker in SCENARIO_5_MARKERS {
            assert!(
                hybrid.markdown.contains(marker),
                "missing marker '{marker}' in rolling output:\n{}",
                hybrid.markdown
            );
        }
    }

    #[test]
    fn summarize_window_map_reduces_oversized_single_window_without_overflow() {
        let summarizer = HybridMapReduceSummarizer::deterministic_only(1_500);
        let segments = scenario_5_segments();

        let out = summarizer
            .summarize_window(&WindowSummaryRequest {
                window_index: 0,
                segments,
                language_instruction: None,
            })
            .expect("oversized window must map-reduce instead of overflowing");

        assert!(validate_rollup_bullets(&out.markdown));
        let bullet_count = out.markdown.lines().filter(|l| l.starts_with("- ")).count();
        assert!(bullet_count <= MAX_WINDOW_BULLETS);
        for marker in SCENARIO_5_MARKERS {
            assert!(
                out.markdown.contains(marker),
                "missing marker '{marker}' in map-reduced window:\n{}",
                out.markdown
            );
        }
    }

    #[test]
    fn context_overflow_error_only_returned_when_max_input_chars_below_256() {
        let tiny = HybridMapReduceSummarizer::deterministic_only(128);
        let err = tiny
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Test".to_owned(),
                template: MeetingTemplate::General,
                rough_notes: String::new(),
                rollups: Vec::new(),
                tail_segments: Vec::new(),
                language_instruction: None,
            })
            .expect_err("budget < 256 must return context overflow error");
        assert!(err.contains("context overflow"), "unexpected error: {err}");
    }

    struct CountingBackend {
        source: &'static str,
        supports_non_latin: bool,
        calls: Arc<AtomicUsize>,
        reply: String,
    }

    impl CompletionBackend for CountingBackend {
        fn source_id(&self) -> &'static str {
            self.source
        }

        fn max_prompt_chars(&self) -> usize {
            16_000
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
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn apple_intelligence_backend_is_automatically_bypassed_for_perso_arabic_and_devanagari() {
        let apple_calls = Arc::new(AtomicUsize::new(0));
        let gguf_calls = Arc::new(AtomicUsize::new(0));

        let apple = Box::new(CountingBackend {
            source: "apple_intelligence",
            supports_non_latin: false,
            calls: Arc::clone(&apple_calls),
            reply: "## TL;DR\n- Apple\n## Key Decisions & Discussion\n- A\n## Expanded from Your Notes\n- A\n## Action Items\n- A".to_owned(),
        });
        let gguf = Box::new(CountingBackend {
            source: "local_gguf",
            supports_non_latin: true,
            calls: Arc::clone(&gguf_calls),
            reply: "## TL;DR\n- میٹنگ نوٹس ready\n## Key Decisions & Discussion\n- Keep Urdu\n## Expanded from Your Notes\n- Rough note\n## Action Items\n- Ship".to_owned(),
        });

        let summarizer = HybridMapReduceSummarizer::with_default_budget(vec![apple, gguf]);

        // Urdu input -> Apple Intelligence MUST be skipped, routed to local_gguf
        let urdu_out = summarizer
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Multilingual Sync".to_owned(),
                template: MeetingTemplate::General,
                rough_notes: "میٹنگ نوٹس".to_owned(),
                rollups: Vec::new(),
                tail_segments: Vec::new(),
                language_instruction: None,
            })
            .expect("should route to local_gguf");
        assert_eq!(urdu_out.source_id, "local_gguf");
        assert_eq!(apple_calls.load(Ordering::SeqCst), 0);
        assert_eq!(gguf_calls.load(Ordering::SeqCst), 1);

        // Devanagari input -> Apple Intelligence MUST also be skipped
        let hindi_out = summarizer
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Hindi Sync".to_owned(),
                template: MeetingTemplate::General,
                rough_notes: "कल का डेमो तैयार रखना है".to_owned(),
                rollups: Vec::new(),
                tail_segments: Vec::new(),
                language_instruction: None,
            })
            .expect("should route Devanagari to local_gguf");
        assert_eq!(hindi_out.source_id, "local_gguf");
        assert_eq!(apple_calls.load(Ordering::SeqCst), 0);
        assert_eq!(gguf_calls.load(Ordering::SeqCst), 2);

        // Pure English input -> Apple Intelligence is eligible and invoked
        let en_out = summarizer
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "English Sync".to_owned(),
                template: MeetingTemplate::General,
                rough_notes: "Ship AEC3 on Friday".to_owned(),
                rollups: Vec::new(),
                tail_segments: Vec::new(),
                language_instruction: None,
            })
            .expect("should allow apple_intelligence on Latin-only input");
        assert_eq!(en_out.source_id, "apple_intelligence");
        assert_eq!(apple_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn twenty_four_rollups_hierarchically_reduce_without_overflow() {
        let summarizer = HybridMapReduceSummarizer::deterministic_only(2_000);
        let rollups: Vec<String> = (0..24)
            .map(|idx| {
                format!(
                    "- **[Me]** [{idx:02}:00]: Action item: ship AEC3 adapter milestone {idx}\n\
                     - **[Speaker 1]** [{idx:02}:15]: Bhai kal ka demo ready rakhna hai\n\
                     - **[Speaker 2]** [{idx:02}:30]: اگلا قدم: میٹنگ نوٹس محفوظ رکھنا\n\
                     - **[Speaker 1]** [{idx:02}:45]: مراجعة خطة الإطلاق الأسبوعية"
                )
            })
            .collect();

        let out = summarizer
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Two-Hour Multilingual Review".to_owned(),
                template: MeetingTemplate::General,
                rough_notes: "Track AEC3 and launch plan.".to_owned(),
                rollups,
                tail_segments: Vec::new(),
                language_instruction: None,
            })
            .expect("24 rollups must hierarchically reduce with 0 overflows");

        assert!(validate_expanded_notes(
            &out.markdown,
            MeetingTemplate::General
        ));
        for marker in SCENARIO_5_MARKERS {
            assert!(
                out.markdown.contains(marker),
                "missing marker '{marker}' after 24-rollup reduction:\n{}",
                out.markdown
            );
        }
    }

    #[test]
    fn invalid_llm_output_falls_back_to_local_extractive_deterministically() {
        let calls = Arc::new(AtomicUsize::new(0));
        let bad_llm = Box::new(CountingBackend {
            source: "local_gguf",
            supports_non_latin: true,
            calls: Arc::clone(&calls),
            reply: "Sure! Here are some unstructured thoughts without headings.".to_owned(),
        });

        let summarizer = HybridMapReduceSummarizer::with_default_budget(vec![bad_llm]);
        let out = summarizer
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Standup".to_owned(),
                template: MeetingTemplate::Standup,
                rough_notes: "Need to finalize AEC3 benchmarks".to_owned(),
                rollups: Vec::new(),
                tail_segments: scenario_5_segments().into_iter().take(6).collect(),
                language_instruction: None,
            })
            .expect("invalid LLM markdown must fall back cleanly");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(out.source_id, "local_extractive");
        assert!(validate_expanded_notes(
            &out.markdown,
            MeetingTemplate::Standup
        ));
        assert!(out.markdown.contains("AEC3"));
    }

    #[test]
    fn apple_intelligence_strips_perso_arabic_instruction_and_normalizes_missing_empty_rough_notes_heading(
    ) {
        struct AssertingAppleBackend;
        impl CompletionBackend for AssertingAppleBackend {
            fn source_id(&self) -> &'static str {
                "apple_intelligence"
            }
            fn max_prompt_chars(&self) -> usize {
                9_000
            }
            fn supports_non_latin_scripts(&self) -> bool {
                false
            }
            fn complete(
                &self,
                system_prompt: &str,
                user_prompt: &str,
                _max_tokens: u32,
            ) -> Result<String, PortError> {
                assert!(
                    !system_prompt.contains("keep Urdu speech in Perso-Arabic"),
                    "Latin-only backend must never receive Perso-Arabic output directive: {system_prompt}"
                );
                assert!(
                    !contains_apple_intelligence_blocked_script(user_prompt),
                    "user_prompt must be Romanized before calling Latin-only backend"
                );
                // Simulate 3B model using 'and' instead of '&' and omitting Expanded from Your Notes when rough_notes is empty
                Ok("### TL;DR\n- Reviewed UI components observability telemetry.\n\n### Key Decisions and Discussion\n- Standardized event names across cards.\n\n### Action Items\n- **Me**: Ship schema PR on Monday.".to_owned())
            }
        }

        let summarizer =
            HybridMapReduceSummarizer::with_default_budget(vec![Box::new(AssertingAppleBackend)]);
        let segments = vec![
            TranscriptSegment {
                speaker: lowyap_ports::SpeakerIdentity {
                    channel: SpeakerChannel::Me,
                    speaker_id: "me".into(),
                    display_label: "Me".into(),
                    confidence: Some(1.0),
                },
                start_ms: 0,
                end_ms: 8_000,
                text: "We need to standardize UI components observability metrics across all product surfaces before next week's release.".into(),
                language: Some("en".into()),
            },
            TranscriptSegment {
                speaker: lowyap_ports::SpeakerIdentity {
                    channel: SpeakerChannel::Others,
                    speaker_id: "speaker_1".into(),
                    display_label: "Speaker 1".into(),
                    confidence: Some(0.9),
                },
                start_ms: 9_000,
                end_ms: 18_000,
                text: "Agreed, let's also verify latency histograms, payload validation rules, and dashboard alerts in staging today. میں فیڈنگ کرتا ہوں.".into(),
                language: Some("ur".into()),
            },
        ];

        let out = summarizer
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Meeting 2026-11-23 15:54".into(),
                template: MeetingTemplate::General,
                rough_notes: String::new(),
                rollups: Vec::new(),
                tail_segments: segments,
                language_instruction: Some(
                    "When summarizing multilingual speech, keep Urdu speech in Perso-Arabic script (never Devanagari) and keep English terms in Latin script.".into(),
                ),
            })
            .expect("apple_intelligence summary should succeed after heading normalization");

        assert_eq!(out.source_id, "apple_intelligence");
        assert!(validate_expanded_notes(
            &out.markdown,
            MeetingTemplate::General
        ));
        assert!(out.markdown.contains("## Key Decisions & Discussion"));
        assert!(out.markdown.contains("## Expanded from Your Notes"));
    }

    #[test]
    fn heavy_arabic_or_urdu_meeting_skips_latin_only_backend_and_preserves_extractive_script() {
        struct PanicIfCalledAppleBackend;
        impl CompletionBackend for PanicIfCalledAppleBackend {
            fn source_id(&self) -> &'static str {
                "apple_intelligence"
            }
            fn max_prompt_chars(&self) -> usize {
                9_000
            }
            fn supports_non_latin_scripts(&self) -> bool {
                false
            }
            fn complete(&self, _sys: &str, _user: &str, _max_tokens: u32) -> Result<String, PortError> {
                panic!("Latin-only 3B backend must not be invoked on heavy Perso-Arabic transcripts");
            }
        }

        let summarizer = HybridMapReduceSummarizer::with_default_budget(vec![Box::new(
            PanicIfCalledAppleBackend,
        )]);
        let segments = vec![
            TranscriptSegment {
                speaker: lowyap_ports::SpeakerIdentity {
                    channel: SpeakerChannel::Others,
                    speaker_id: "speaker_1".into(),
                    display_label: "Speaker 1".into(),
                    confidence: Some(0.95),
                },
                start_ms: 0,
                end_ms: 15_000,
                text: "سمعت الأذانة فذهبت إلى المسجد، هذه الجملة الفعلية في الدرس الخامس.".into(),
                language: Some("ar".into()),
            },
            TranscriptSegment {
                speaker: lowyap_ports::SpeakerIdentity {
                    channel: SpeakerChannel::Me,
                    speaker_id: "me".into(),
                    display_label: "Me".into(),
                    confidence: Some(1.0),
                },
                start_ms: 16_000,
                end_ms: 28_000,
                text: "نعم فهمت، الفاعل ضمير مستتر والمفعول به الأذانة منصوبة بالفتحة.".into(),
                language: Some("ar".into()),
            },
        ];

        let out = summarizer
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Arabic Grammar Lesson".into(),
                template: MeetingTemplate::General,
                rough_notes: String::new(),
                rollups: Vec::new(),
                tail_segments: segments,
                language_instruction: None,
            })
            .expect("heavy Arabic meeting should deterministically fall back to local_extractive");

        assert_eq!(out.source_id, SOURCE_LOCAL_EXTRACTIVE);
        assert!(out.markdown.contains("سمعت الأذانة"));
    }
}
