//! Deterministic voice snippet expansion.
//!
//! Runs before fast-path or LLM cleanup so spoken triggers expand reliably
//! whether AI post-processing is enabled or not.

use crate::flow::settings::{normalize_trigger, VoiceSnippet};

/// Result of expanding configured voice snippets against a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetExpansion {
    pub text: String,
    pub matched_any: bool,
    pub full_utterance_match: bool,
}

struct PreparedSnippet<'a> {
    trigger_lower: String,
    parts_lower: Vec<String>,
    char_len: usize,
    expansion: &'a str,
}

/// Expands configured voice snippets in `input`.
///
/// Checks for an exact full-utterance match first (ignoring outer whitespace,
/// collapsed internal whitespace, case, and at most one trailing `.`, `!`, or
/// `?` added by ASR), then falls back to single-pass greedy longest-trigger
/// inline word-boundary replacement.
pub fn expand_snippets(input: &str, snippets: &[VoiceSnippet]) -> SnippetExpansion {
    if input.trim().is_empty() || snippets.is_empty() {
        return SnippetExpansion {
            text: input.to_string(),
            matched_any: false,
            full_utterance_match: false,
        };
    }

    let mut prepared: Vec<PreparedSnippet<'_>> = snippets
        .iter()
        .filter_map(|snippet| {
            let normalized = normalize_trigger(&snippet.trigger);
            if normalized.is_empty() || snippet.expansion.trim().is_empty() {
                return None;
            }
            let char_len = normalized.chars().count();
            let trigger_lower = normalized.to_lowercase();
            let parts_lower = normalized.split(' ').map(str::to_lowercase).collect();
            Some(PreparedSnippet {
                trigger_lower,
                parts_lower,
                char_len,
                expansion: &snippet.expansion,
            })
        })
        .collect();

    if prepared.is_empty() {
        return SnippetExpansion {
            text: input.to_string(),
            matched_any: false,
            full_utterance_match: false,
        };
    }

    // Check full-utterance match first so URLs/emails never keep a trailing ASR period.
    let trimmed = input.trim();
    let stripped = trimmed
        .strip_suffix(['.', '!', '?'])
        .unwrap_or(trimmed)
        .trim();
    let full_stripped_lower = normalize_trigger(stripped).to_lowercase();
    let full_unstripped_lower = normalize_trigger(trimmed).to_lowercase();

    for item in &prepared {
        if (!full_stripped_lower.is_empty() && item.trigger_lower == full_stripped_lower)
            || item.trigger_lower == full_unstripped_lower
        {
            return SnippetExpansion {
                text: item.expansion.to_string(),
                matched_any: true,
                full_utterance_match: true,
            };
        }
    }

    // Longest triggers first so "my zoom link" wins over "zoom".
    prepared.sort_by_key(|a| std::cmp::Reverse(a.char_len));

    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut out = String::new();
    let mut matched_any = false;
    let mut unflushed_byte = 0usize;
    let mut idx = 0usize;

    while idx < chars.len() {
        let left_boundary = idx == 0 || !chars[idx - 1].1.is_alphanumeric();
        if left_boundary {
            let mut matched = None;
            for item in &prepared {
                if let Some(end_idx) = match_trigger_at(&chars, idx, &item.parts_lower) {
                    let right_boundary =
                        end_idx == chars.len() || !chars[end_idx].1.is_alphanumeric();
                    if right_boundary {
                        matched = Some((end_idx, item.expansion));
                        break;
                    }
                }
            }

            if let Some((end_idx, expansion)) = matched {
                if !matched_any {
                    out.reserve(input.len() + expansion.len());
                    matched_any = true;
                }
                let match_start_byte = chars[idx].0;
                let match_end_byte = chars.get(end_idx).map_or(input.len(), |&(b, _)| b);
                out.push_str(&input[unflushed_byte..match_start_byte]);
                out.push_str(expansion);
                unflushed_byte = match_end_byte;
                idx = end_idx;
                continue;
            }
        }
        idx += 1;
    }

    if !matched_any {
        return SnippetExpansion {
            text: input.to_string(),
            matched_any: false,
            full_utterance_match: false,
        };
    }

    out.push_str(&input[unflushed_byte..]);
    SnippetExpansion {
        text: out,
        matched_any: true,
        full_utterance_match: false,
    }
}

fn match_trigger_at(
    chars: &[(usize, char)],
    start: usize,
    parts_lower: &[String],
) -> Option<usize> {
    let mut pos = start;
    for (part_idx, part_lower) in parts_lower.iter().enumerate() {
        if part_idx > 0 {
            if pos >= chars.len() || !chars[pos].1.is_whitespace() {
                return None;
            }
            while pos < chars.len() && chars[pos].1.is_whitespace() {
                pos += 1;
            }
        }
        let mut target_iter = part_lower.chars();
        while !target_iter.as_str().is_empty() {
            let (_, ch) = *chars.get(pos)?;
            for lower_ch in ch.to_lowercase() {
                if target_iter.next() != Some(lower_ch) {
                    return None;
                }
            }
            pos += 1;
        }
    }
    Some(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(trigger: &str, expansion: &str) -> VoiceSnippet {
        VoiceSnippet {
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
        }
    }

    #[test]
    fn full_utterance_strips_trailing_asr_punctuation() {
        let snippets = [snippet("my calendar link", "https://cal.com/me")];

        assert_eq!(
            expand_snippets("My calendar link.", &snippets),
            SnippetExpansion {
                text: "https://cal.com/me".to_string(),
                matched_any: true,
                full_utterance_match: true,
            }
        );
        assert_eq!(
            expand_snippets("  my   calendar link? ", &snippets),
            SnippetExpansion {
                text: "https://cal.com/me".to_string(),
                matched_any: true,
                full_utterance_match: true,
            }
        );
        assert_eq!(
            expand_snippets("MY CALENDAR LINK!", &snippets),
            SnippetExpansion {
                text: "https://cal.com/me".to_string(),
                matched_any: true,
                full_utterance_match: true,
            }
        );
    }

    #[test]
    fn longest_trigger_wins_over_shorter_prefix_or_substring() {
        let snippets = [
            snippet("zoom", "Zoom App"),
            snippet("my zoom link", "https://zoom.us/j/12345"),
        ];

        let result = expand_snippets("Join zoom using my zoom link.", &snippets);
        assert_eq!(
            result,
            SnippetExpansion {
                text: "Join Zoom App using https://zoom.us/j/12345.".to_string(),
                matched_any: true,
                full_utterance_match: false,
            }
        );
    }

    #[test]
    fn inline_replacement_respects_word_boundaries_and_case() {
        let snippets = [snippet("zoom", "https://zoom.us/my/room")];

        let untouched = expand_snippets("We are zooming past prezoom milestones.", &snippets);
        assert_eq!(
            untouched,
            SnippetExpansion {
                text: "We are zooming past prezoom milestones.".to_string(),
                matched_any: false,
                full_utterance_match: false,
            }
        );

        let matched = expand_snippets("Hop on (ZOOM), please!", &snippets);
        assert_eq!(
            matched,
            SnippetExpansion {
                text: "Hop on (https://zoom.us/my/room), please!".to_string(),
                matched_any: true,
                full_utterance_match: false,
            }
        );
    }

    #[test]
    fn non_ascii_urdu_and_accented_text_never_corrupts_slice_boundaries() {
        let snippets = [
            snippet("my zoom link", "https://zoom.us/j/999"),
            snippet("ای میل", "team@lowyap.com"),
        ];

        let mixed = expand_snippets(
            "یہ My Zoom Link ہے اور میرا ای میل بھیجیں، café!",
            &snippets,
        );
        assert_eq!(
            mixed,
            SnippetExpansion {
                text: "یہ https://zoom.us/j/999 ہے اور میرا team@lowyap.com بھیجیں، café!"
                    .to_string(),
                matched_any: true,
                full_utterance_match: false,
            }
        );

        // Multi-byte case-folding character (İ -> i\u{307}) before trigger must not panic.
        let turkish = expand_snippets("İstanbul: my zoom link!", &snippets);
        assert_eq!(turkish.text, "İstanbul: https://zoom.us/j/999!");
        assert!(turkish.matched_any);
    }

    #[test]
    fn expansion_is_single_pass_and_not_recursive() {
        let snippets = [
            snippet("first trigger", "contains second trigger inside"),
            snippet("second trigger", "SHOULD_NOT_APPEAR"),
        ];

        let inline = expand_snippets("Say first trigger now", &snippets);
        assert_eq!(
            inline,
            SnippetExpansion {
                text: "Say contains second trigger inside now".to_string(),
                matched_any: true,
                full_utterance_match: false,
            }
        );
    }

    #[test]
    fn empty_input_or_snippets_returns_unchanged() {
        let snippets = [snippet("sig", "Best, Alex")];
        assert_eq!(
            expand_snippets("   ", &snippets),
            SnippetExpansion {
                text: "   ".to_string(),
                matched_any: false,
                full_utterance_match: false,
            }
        );
        assert_eq!(
            expand_snippets("sig", &[]),
            SnippetExpansion {
                text: "sig".to_string(),
                matched_any: false,
                full_utterance_match: false,
            }
        );
    }
}
