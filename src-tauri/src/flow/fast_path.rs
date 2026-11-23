//! Tauri tone adapter for the deterministic dictation fast path.

use crate::flow::settings::Tone;
use lowyap_flow::fast_path as flow_core;

impl From<Tone> for flow_core::Tone {
    fn from(tone: Tone) -> Self {
        match tone {
            Tone::Default => Self::Default,
            Tone::Casual => Self::Casual,
            Tone::Formal => Self::Formal,
            Tone::Code => Self::Code,
        }
    }
}

pub fn should_bypass_llm(
    text: &str,
    tone: Tone,
    fast_path_enabled: bool,
    full_utterance_snippet: bool,
) -> bool {
    flow_core::should_bypass_llm(text, tone.into(), fast_path_enabled, full_utterance_snippet)
}

#[cfg(test)]
pub fn apply_deterministic_polish(text: &str, tone: Tone) -> String {
    flow_core::apply_deterministic_polish(text, tone.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_uses_the_shell_tone_and_snippet_override() {
        let text = "I can't make it today.";
        assert!(!should_bypass_llm(text, Tone::Formal, true, false));
        assert!(should_bypass_llm(text, Tone::Casual, true, false));
        assert!(should_bypass_llm(text, Tone::Formal, false, true));
    }

    #[test]
    fn code_tone_treats_the_literal_url_as_one_word() {
        assert!(should_bypass_llm(
            "Visit https://github.com/slash/dash/colon",
            Tone::Code,
            true,
            false
        ));
    }

    #[test]
    fn casual_polish_trims_a_terminal_period() {
        assert_eq!(
            apply_deterministic_polish("  Sounds good. ", Tone::Casual),
            "Sounds good"
        );
    }
}
