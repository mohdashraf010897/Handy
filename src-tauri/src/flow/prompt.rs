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
            "Style: the text is going into a chat message. Make it sound like a quick, \
             friendly message: use contractions (do not -> don't), keep it short, and drop \
             the final period on a one-sentence message. Keep words like hey and thanks. \
             Write file names, numbers and links in their usual written form. \
             Do not add greetings, sign-offs, emoji or content that was not spoken.",
        ),
        Tone::Formal => Some(
            "Style: the text is going into an email or document. Rewrite it in a polished, \
             professional register: complete sentences, no contractions (can't -> cannot), \
             no slang or filler, and casual words made formal (hey -> hello, thanks -> \
             thank you, gonna -> going to, yeah -> yes, can you -> could you). End every \
             sentence with a period or question mark. Keep the meaning and every fact \
             unchanged. Do not add greetings, sign-offs or content that was not spoken.",
        ),
        Tone::Code => Some(
            "Style: the text is going into a code editor or terminal. Turn spoken symbols \
             into characters inside identifiers, paths and commands (config dot yaml -> \
             config.yaml, dash dash force -> --force, user underscore id -> user_id, \
             slash etc -> /etc). Keep identifiers, flags and technical terms exactly as \
             spoken, remove filler words (um, uh, so, like), do not add markdown or \
             backticks, and do not rephrase technical content or add explanations.",
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

    #[test]
    fn each_tone_sends_a_different_selected_prompt() {
        let settings = settings_with_prompt("Clean: ${output}");
        let prompts: Vec<String> = [Tone::Default, Tone::Casual, Tone::Formal, Tone::Code]
            .into_iter()
            .map(|tone| {
                tone_settings(&settings, tone).post_process_prompts[0]
                    .prompt
                    .clone()
            })
            .collect();
        assert_eq!(prompts[0], "Clean: ${output}");
        for (i, a) in prompts.iter().enumerate() {
            assert!(a.contains("${output}"), "placeholder kept: {a}");
            for b in &prompts[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// Calls the real cleanup path with Apple Intelligence once per tone.
    /// Run: `cargo test apple_intelligence_cleanup_per_tone -- --ignored --nocapture`
    #[test]
    #[ignore = "calls on-device Apple Intelligence; needs Apple silicon with it enabled"]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn apple_intelligence_cleanup_per_tone() {
        use crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID;

        assert!(
            crate::apple_intelligence::check_apple_intelligence_availability(),
            "Apple Intelligence is not available on this Mac"
        );
        let mut settings = get_default_settings();
        settings.post_process_provider_id = APPLE_INTELLIGENCE_PROVIDER_ID.into();
        let default_prompt = settings.post_process_prompts[0].id.clone();
        settings.post_process_selected_prompt_id = Some(default_prompt);
        let transcript = "um hey so can you uh send me the file later \
                          i need to check the config dot yaml before the meeting";
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let mut prompts = Vec::new();
        for tone in [Tone::Default, Tone::Casual, Tone::Formal, Tone::Code] {
            let toned = tone_settings(&settings, tone);
            prompts.push(toned.post_process_prompts[0].prompt.clone());
            let output = runtime
                .block_on(crate::actions::post_process_transcription(
                    &toned, transcript,
                ))
                .unwrap_or_default();
            println!("{tone:?}: {output}");
            assert!(!output.trim().is_empty(), "{tone:?} returned nothing");
        }
        for (i, a) in prompts.iter().enumerate() {
            for b in &prompts[i + 1..] {
                assert_ne!(a, b, "tones sent the same prompt");
            }
        }
    }
}
