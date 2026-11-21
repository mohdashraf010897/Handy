//! Sub-350ms deterministic fast-path for short, clean utterances.
//!
//! Short transcripts without fillers, self-corrections, or tone-specific
//! rewrite cues bypass the LLM round-trip and paste immediately.

use crate::spoken_urls::is_formatted_url_token;

/// Writing style used to decide whether to skip LLM cleanup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    /// Preserve the normal cleanup style.
    #[default]
    Default,
    /// Prefer conversational text and light punctuation.
    Casual,
    /// Prefer complete sentences and a professional register.
    Formal,
    /// Preserve identifiers, commands, and paths.
    Code,
}

const FILLER_WORDS: &[&str] = &[
    "um", "uh", "umm", "uhh", "er", "erm", "ah", "hmm", "uh-huh", "mm-hmm",
];

const SELF_CORRECTION_OR_FORMAT_CUES: &[&[&str]] = &[
    &["actually"],
    &["scratch", "that"],
    &["no", "wait"],
    &["wait", "no"],
    &["i", "mean"],
    &["make", "that"],
    &["new", "line"],
    &["next", "line"],
    &["new", "paragraph"],
    &["bullet", "point"],
    &["numbered", "list"],
];

const CODE_SYMBOL_CUES: &[&str] = &[
    "dot",
    "slash",
    "backslash",
    "dash",
    "hyphen",
    "underscore",
    "colon",
    "semicolon",
    "equals",
    "arrow",
    "open",
    "close",
    "bracket",
    "paren",
    "camel",
    "snake",
];

const FORMAL_REWRITE_CUES: &[&str] = &[
    "can't", "won't", "don't", "didn't", "isn't", "aren't", "wasn't", "weren't", "i'm", "it's",
    "that's", "there's", "we're", "they're", "you're", "i've", "we've", "i'll", "we'll", "gonna",
    "wanna", "gotta", "kinda", "sorta", "yeah", "yep", "nope", "nah", "hey", "yo", "thx", "pls",
];

/// Decides whether `text` can skip the LLM cleanup round-trip.
pub fn should_bypass_llm(
    text: &str,
    tone: Tone,
    fast_path_enabled: bool,
    full_utterance_snippet: bool,
) -> bool {
    if full_utterance_snippet {
        return true;
    }
    if text.trim().is_empty() {
        return true;
    }
    if !fast_path_enabled {
        return false;
    }

    let words = extract_lowercase_words(text);
    if words.is_empty() || words.len() > 10 {
        return false;
    }

    if words.iter().any(|w| FILLER_WORDS.contains(&w.as_str())) {
        return false;
    }

    for cue in SELF_CORRECTION_OR_FORMAT_CUES {
        if words
            .windows(cue.len())
            .any(|window| window.iter().map(String::as_str).eq(cue.iter().copied()))
        {
            return false;
        }
    }

    match tone {
        Tone::Code => {
            if words.iter().any(|w| CODE_SYMBOL_CUES.contains(&w.as_str())) {
                return false;
            }
        }
        Tone::Formal => {
            if words
                .iter()
                .any(|w| FORMAL_REWRITE_CUES.contains(&w.as_str()))
            {
                return false;
            }
        }
        Tone::Default | Tone::Casual => {}
    }

    true
}

/// Applies lightweight deterministic cleanup when bypassing or skipping the LLM.
pub fn apply_deterministic_fast_path(text: &str) -> String {
    apply_deterministic_polish(text, Tone::Default)
}

/// Trims the transcript and, in casual tone, drops a single terminal period
/// from single-line, single-sentence text.
pub fn apply_deterministic_polish(text: &str, tone: Tone) -> String {
    let trimmed = text.trim();
    if tone == Tone::Casual
        && !trimmed.contains('\n')
        && !trimmed.contains('\r')
        && !trimmed.contains(". ")
        && !trimmed.contains("! ")
        && !trimmed.contains("? ")
        && trimmed.ends_with('.')
        && !trimmed.ends_with("..")
    {
        if let Some(without_period) = trimmed.strip_suffix('.') {
            return without_period.to_string();
        }
    }
    trimmed.to_string()
}

/// Extracts lowercased word tokens consisting of alphanumeric runs with
/// internal apostrophes (`'` / `’`) or hyphens (`-`). Formatted URL tokens
/// count as a single token so path segments neither inflate the 10-word budget
/// nor collide with `CODE_SYMBOL_CUES`.
fn extract_lowercase_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();

    for chunk in text.split_whitespace() {
        let trimmed_chunk = chunk.trim_matches(|c: char| !c.is_alphanumeric() && c != '/');
        if is_formatted_url_token(trimmed_chunk) {
            words.push(trimmed_chunk.to_lowercase());
            continue;
        }

        let mut chars = chunk.chars().peekable();
        while let Some(ch) = chars.next() {
            if !ch.is_alphanumeric() {
                continue;
            }
            let mut word = String::new();
            word.extend(ch.to_lowercase());

            while let Some(&next) = chars.peek() {
                if next.is_alphanumeric() {
                    if let Some(c) = chars.next() {
                        word.extend(c.to_lowercase());
                    }
                } else if matches!(next, '\'' | '\u{2019}' | '-') {
                    let mut lookahead = chars.clone();
                    let _ = lookahead.next();
                    if lookahead.peek().is_some_and(|c| c.is_alphanumeric()) {
                        let punct = chars.next().unwrap_or('\'');
                        word.push(if punct == '-' { '-' } else { '\'' });
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            words.push(word);
        }
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_fast_path_trims_without_rewriting_punctuation() {
        assert_eq!(
            apply_deterministic_fast_path("  Sounds good.  "),
            "Sounds good."
        );
    }

    #[test]
    fn short_clean_phrase_bypasses_llm() {
        assert!(should_bypass_llm(
            "Sounds good, see you at 3.",
            Tone::Casual,
            true,
            false
        ));
        assert!(should_bypass_llm("On my way!", Tone::Default, true, false));
    }

    #[test]
    fn casual_polish_strips_single_trailing_period_only() {
        assert_eq!(
            apply_deterministic_polish("  Sounds good. ", Tone::Casual),
            "Sounds good"
        );
        assert_eq!(
            apply_deterministic_polish("Wait...", Tone::Casual),
            "Wait..."
        );
        assert_eq!(
            apply_deterministic_polish("Sounds good. See you soon.", Tone::Casual),
            "Sounds good. See you soon."
        );
        assert_eq!(
            apply_deterministic_polish("Line one\nLine two.", Tone::Casual),
            "Line one\nLine two."
        );
        assert_eq!(
            apply_deterministic_polish("Sounds good.", Tone::Formal),
            "Sounds good."
        );
    }

    #[test]
    fn more_than_ten_words_routes_to_llm() {
        let eleven_words = "one two three four five six seven eight nine ten eleven";
        assert!(!should_bypass_llm(eleven_words, Tone::Default, true, false));

        let ten_words = "one two three four five six seven eight nine ten";
        assert!(should_bypass_llm(ten_words, Tone::Default, true, false));
    }

    #[test]
    fn fillers_and_self_corrections_route_to_llm() {
        assert!(!should_bypass_llm(
            "Um send it now",
            Tone::Default,
            true,
            false
        ));
        assert!(!should_bypass_llm(
            "Uh-huh sounds right",
            Tone::Casual,
            true,
            false
        ));
        assert!(!should_bypass_llm(
            "Meet at 4, actually 5",
            Tone::Default,
            true,
            false
        ));
        assert!(!should_bypass_llm(
            "Wait, no, send it tomorrow",
            Tone::Default,
            true,
            false
        ));
        assert!(!should_bypass_llm(
            "Hello new line world",
            Tone::Default,
            true,
            false
        ));
    }

    #[test]
    fn formal_tone_routes_contractions_and_slang_to_llm_but_bypasses_clean_formal() {
        assert!(!should_bypass_llm(
            "I can't make it today.",
            Tone::Formal,
            true,
            false
        ));
        assert!(!should_bypass_llm(
            "Yeah sounds good.",
            Tone::Formal,
            true,
            false
        ));
        assert!(should_bypass_llm(
            "Thank you for your help.",
            Tone::Formal,
            true,
            false
        ));
        assert!(should_bypass_llm(
            "I cannot attend today.",
            Tone::Formal,
            true,
            false
        ));
    }

    #[test]
    fn code_tone_routes_spoken_symbol_cues_to_llm() {
        assert!(!should_bypass_llm(
            "foo underscore bar",
            Tone::Code,
            true,
            false
        ));
        assert!(!should_bypass_llm(
            "config dot yaml",
            Tone::Code,
            true,
            false
        ));
        assert!(should_bypass_llm("git status", Tone::Code, true, false));
        assert!(should_bypass_llm(
            "github.com/lowyap/open-issues",
            Tone::Code,
            true,
            false
        ));
    }

    #[test]
    fn full_utterance_snippet_always_bypasses_even_when_disabled_or_long() {
        let long_snippet =
            "one two three four five six seven eight nine ten eleven twelve dot slash";
        assert!(should_bypass_llm(long_snippet, Tone::Code, false, true));
        assert!(!should_bypass_llm(
            "Sounds good",
            Tone::Default,
            false,
            false
        ));
    }
}
