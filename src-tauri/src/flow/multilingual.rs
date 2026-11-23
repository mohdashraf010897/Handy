//! Tauri settings adapter for the multilingual flow policies.

#[cfg(test)]
use std::path::Path;

use lowyap_flow::multilingual as flow_core;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

pub use flow_core::{
    geometric_mean_token_prob,
    needs_spoken_language_disambiguation_with_context as needs_spoken_language_disambiguation,
    score_candidate_transcript_for_language as score_candidate_transcript, split_bilingual_clauses,
};

// Keep the shell's helper names while the implementation lives in lowyap-flow.
#[expect(unused_imports)]
pub use flow_core::{
    contains_devanagari, devanagari_to_roman_hinglish, is_repetitive_hallucination,
    strip_hallucination_loops,
};

/// Preferred output script when speaking Hindi/Urdu (Hindustani).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum HindustaniScript {
    #[default]
    Auto,
    RomanHinglish,
    UrduPersoArabic,
    Devanagari,
}

impl From<HindustaniScript> for flow_core::HindustaniScript {
    fn from(script: HindustaniScript) -> Self {
        match script {
            HindustaniScript::Auto => Self::Auto,
            HindustaniScript::RomanHinglish => Self::RomanHinglish,
            HindustaniScript::UrduPersoArabic => Self::UrduPersoArabic,
            HindustaniScript::Devanagari => Self::Devanagari,
        }
    }
}

impl From<flow_core::HindustaniScript> for HindustaniScript {
    fn from(script: flow_core::HindustaniScript) -> Self {
        match script {
            flow_core::HindustaniScript::Auto => Self::Auto,
            flow_core::HindustaniScript::RomanHinglish => Self::RomanHinglish,
            flow_core::HindustaniScript::UrduPersoArabic => Self::UrduPersoArabic,
            flow_core::HindustaniScript::Devanagari => Self::Devanagari,
        }
    }
}

/// Reads the user's script preference from the shell's cached Flow settings,
/// honoring non-persisted `--transcribe-file --language` overrides first.
pub fn active_hindustani_script(app: &AppHandle) -> HindustaniScript {
    if let Some(cli_lang) = crate::settings::cli_language_override() {
        let primary = cli_lang.split(['-', '_']).next().unwrap_or(cli_lang);
        return if primary.eq_ignore_ascii_case("hi") {
            HindustaniScript::Devanagari
        } else if primary.eq_ignore_ascii_case("ur") {
            HindustaniScript::UrduPersoArabic
        } else {
            HindustaniScript::Auto
        };
    }
    crate::flow::settings::get(app).hindustani_script
}

#[cfg(test)]
pub(crate) fn read_hindustani_script_from_path(path: &Path) -> HindustaniScript {
    flow_core::read_hindustani_script_from_path(path).into()
}

#[expect(dead_code, reason = "Preserve the shell helper API")]
pub fn effective_hindustani_script(
    pref: HindustaniScript,
    spoken_languages: &[String],
) -> HindustaniScript {
    flow_core::effective_hindustani_script(pref.into(), spoken_languages).into()
}

pub fn build_initial_prompt(
    supports_initial_prompt: bool,
    supports_vocabulary: bool,
    resolved_lang: Option<&str>,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
    custom_words: &[String],
) -> Option<String> {
    flow_core::build_initial_prompt_for_model(
        supports_initial_prompt,
        supports_vocabulary,
        resolved_lang,
        spoken_languages,
        script_pref.into(),
        custom_words,
    )
}

pub fn should_retry_chunk_as_urdu(
    model_detected_lang: Option<&str>,
    text: &str,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
    explicit_selected_lang: &str,
) -> bool {
    flow_core::should_retry_chunk_as_urdu_with_context(
        model_detected_lang,
        text,
        spoken_languages,
        script_pref.into(),
        explicit_selected_lang,
    )
}

pub fn candidate_languages_for_spoken(
    spoken_languages: &[String],
    script_pref: HindustaniScript,
    supported_languages: &[String],
) -> Vec<String> {
    flow_core::candidate_languages_for_model(
        spoken_languages,
        script_pref.into(),
        supported_languages,
    )
}

#[expect(dead_code, reason = "Preserve the shell helper API")]
pub fn is_lang_compatible_with_spoken(detected_lang: &str, spoken_languages: &[String]) -> bool {
    flow_core::is_lang_compatible_with_spoken(Some(detected_lang), spoken_languages)
}

pub fn normalize_script_output(
    text: &str,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
) -> String {
    flow_core::normalize_script_output(text, spoken_languages, script_pref.into())
}

pub fn llm_script_instruction(
    script_pref: HindustaniScript,
    spoken_languages: &[String],
) -> Option<&'static str> {
    flow_core::llm_script_instruction(script_pref.into(), spoken_languages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_conversions_preserve_the_settings_wire_format() {
        use flow_core::HindustaniScript as CoreScript;

        assert_eq!(HindustaniScript::default(), HindustaniScript::Auto);
        for (shell, core, wire) in [
            (HindustaniScript::Auto, CoreScript::Auto, "auto"),
            (
                HindustaniScript::RomanHinglish,
                CoreScript::RomanHinglish,
                "roman_hinglish",
            ),
            (
                HindustaniScript::UrduPersoArabic,
                CoreScript::UrduPersoArabic,
                "urdu_perso_arabic",
            ),
            (
                HindustaniScript::Devanagari,
                CoreScript::Devanagari,
                "devanagari",
            ),
        ] {
            let json = serde_json::Value::String(wire.to_owned());
            assert_eq!(CoreScript::from(shell), core);
            assert_eq!(HindustaniScript::from(core), shell);
            assert_eq!(serde_json::to_value(shell).unwrap(), json);
            assert_eq!(serde_json::to_value(core).unwrap(), json);
            assert_eq!(
                serde_json::from_value::<HindustaniScript>(json.clone()).unwrap(),
                shell
            );
            assert_eq!(serde_json::from_value::<CoreScript>(json).unwrap(), core);
        }
    }

    #[test]
    fn model_prompt_uses_shell_script_and_vocabulary_settings() {
        let spoken = ["hi".to_owned()];
        let script = HindustaniScript::RomanHinglish;
        let words = ["Lowyap".to_owned()];
        assert_eq!(
            build_initial_prompt(true, false, Some("hi"), &spoken, script, &words),
            flow_core::build_initial_prompt_for_model(
                true,
                false,
                Some("hi"),
                &spoken,
                script.into(),
                &words,
            )
        );
    }

    #[test]
    fn normalizes_to_the_shell_script_preference() {
        let spoken = ["en".to_owned(), "ur".to_owned()];
        assert_eq!(
            normalize_script_output(
                "हेलो आप क्या करते हैं?",
                &spoken,
                HindustaniScript::RomanHinglish
            ),
            "Hello aap kya karte hain?"
        );
    }

    #[test]
    fn settings_script_reader_falls_back_to_auto_for_non_json() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert_eq!(
            read_hindustani_script_from_path(&path),
            HindustaniScript::Auto
        );
    }
}
