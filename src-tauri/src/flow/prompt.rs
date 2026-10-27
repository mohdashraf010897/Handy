//! Tone-aware cleanup prompt.
//!
//! Only a style instruction is appended to the user's selected prompt; the
//! app name and bundle id are never added, because the cleanup provider may
//! be a cloud API.

use std::borrow::Cow;

use tauri::AppHandle;

use crate::flow::context::tone_for;
use crate::flow::settings::Tone;
use crate::flow::{settings, timing};
use crate::settings::AppSettings;

fn tone_instruction(tone: Tone) -> Option<&'static str> {
    match tone {
        Tone::Default => None,
        Tone::Casual => Some(
            "Style: the text is going into a chat message. Keep it conversational and \
             concise; light punctuation is fine. Do not add greetings, sign-offs or \
             formality that was not spoken.",
        ),
        Tone::Formal => Some(
            "Style: the text is going into an email or document. Use complete sentences, \
             correct capitalization and punctuation, and a professional register. Do not \
             add greetings, sign-offs or content that was not spoken.",
        ),
        Tone::Code => Some(
            "Style: the text is going into a code editor or terminal. Keep identifiers, \
             commands, file paths, flags and technical terms exactly as spoken, and do not \
             rephrase technical content or add explanations.",
        ),
    }
}

/// Appends the tone instruction to a cleanup prompt; `Default` leaves it as is.
pub fn apply_tone(prompt: &str, tone: Tone) -> String {
    match tone_instruction(tone) {
        Some(instruction) => format!(
            "{}\n\n{instruction} Still return only the cleaned-up transcript.",
            prompt.trim_end()
        ),
        None => prompt.to_string(),
    }
}

/// Returns `settings` with the selected cleanup prompt adjusted to the tone of
/// the app captured at hotkey press. Borrowed unchanged when tone is off, the
/// tone is `Default`, or no live dictation is being cleaned up (e.g. a retry
/// from history).
pub fn with_tone<'a>(app: &AppHandle, settings: &'a AppSettings) -> Cow<'a, AppSettings> {
    let flow = settings::get(app);
    if !flow.tone_enabled {
        return Cow::Borrowed(settings);
    }
    let tone = timing::choose_tone(|context| tone_for(context, &flow));
    tone_settings(settings, tone)
}

fn tone_settings(settings: &AppSettings, tone: Tone) -> Cow<'_, AppSettings> {
    if tone == Tone::Default {
        return Cow::Borrowed(settings);
    }
    let Some(selected) = settings.post_process_selected_prompt_id.as_deref() else {
        return Cow::Borrowed(settings);
    };
    let Some(index) = settings
        .post_process_prompts
        .iter()
        .position(|p| p.id == selected && !p.prompt.trim().is_empty())
    else {
        return Cow::Borrowed(settings);
    };
    let mut toned = settings.clone();
    if let Some(prompt) = toned.post_process_prompts.get_mut(index) {
        prompt.prompt = apply_tone(&prompt.prompt, tone);
    }
    Cow::Owned(toned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{get_default_settings, LLMPrompt};

    fn settings_with_prompt(prompt: &str) -> AppSettings {
        let mut settings = get_default_settings();
        settings.post_process_prompts = vec![LLMPrompt {
            id: "p1".into(),
            name: "Clean".into(),
            prompt: prompt.into(),
        }];
        settings.post_process_selected_prompt_id = Some("p1".into());
        settings
    }

    #[test]
    fn default_tone_leaves_prompt_untouched() {
        assert_eq!(
            apply_tone("Fix: ${output}", Tone::Default),
            "Fix: ${output}"
        );
        let settings = settings_with_prompt("Fix: ${output}");
        assert!(matches!(
            tone_settings(&settings, Tone::Default),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn tone_is_appended_after_the_transcript_placeholder() {
        let toned = apply_tone(
            "Clean this.\n<transcript>${output}</transcript>\n",
            Tone::Code,
        );
        let placeholder = toned.find("${output}").expect("placeholder kept");
        let instruction = toned.find("code editor").expect("instruction added");
        assert!(placeholder < instruction);
        assert!(toned.ends_with("Still return only the cleaned-up transcript."));
    }

    #[test]
    fn every_non_default_tone_has_a_distinct_instruction() {
        let casual = apply_tone("p", Tone::Casual);
        let formal = apply_tone("p", Tone::Formal);
        let code = apply_tone("p", Tone::Code);
        assert!(casual.contains("chat message"));
        assert!(formal.contains("email or document"));
        assert!(code.contains("code editor"));
    }

    #[test]
    fn only_the_selected_prompt_is_changed() {
        let mut settings = settings_with_prompt("Selected ${output}");
        settings.post_process_prompts.push(LLMPrompt {
            id: "p2".into(),
            name: "Other".into(),
            prompt: "Other ${output}".into(),
        });
        let toned = tone_settings(&settings, Tone::Formal);
        assert!(toned.post_process_prompts[0]
            .prompt
            .contains("professional"));
        assert_eq!(toned.post_process_prompts[1].prompt, "Other ${output}");
        assert_eq!(
            settings.post_process_prompts[0].prompt,
            "Selected ${output}"
        );
    }

    #[test]
    fn missing_or_empty_selected_prompt_is_left_alone() {
        let mut none_selected = settings_with_prompt("x");
        none_selected.post_process_selected_prompt_id = None;
        assert!(matches!(
            tone_settings(&none_selected, Tone::Casual),
            Cow::Borrowed(_)
        ));
        let empty = settings_with_prompt("   ");
        assert!(matches!(
            tone_settings(&empty, Tone::Casual),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn instruction_never_names_the_app() {
        for tone in [Tone::Casual, Tone::Formal, Tone::Code] {
            let toned = apply_tone("p", tone).to_lowercase();
            for name in [
                "slack",
                "apple mail",
                "outlook",
                "vscode",
                "terminal.app",
                "com.",
            ] {
                assert!(!toned.contains(name), "{tone:?} mentions {name}");
            }
        }
    }
}
