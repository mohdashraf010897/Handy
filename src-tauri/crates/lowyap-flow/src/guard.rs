//! Deterministic cleanup for Whisper transcripts using script and audio evidence.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

use crate::accent_asr::inspect_compression_and_confidence;
use crate::multilingual::{
    effective_hindustani_script, has_spoken_lang, normalize_script_output,
    strip_hallucination_loops, HindustaniScript,
};

/// Optional audio and decoder evidence for one transcript chunk or span.
#[derive(Clone, Copy, Debug, Default)]
pub struct SilenceGuardContext {
    /// Root-mean-square amplitude of the chunk's 16 kHz PCM.
    pub rms_energy: Option<f32>,
    /// The decoder's probability that the chunk contains no speech.
    pub no_speech_prob: Option<f32>,
    /// The geometric or arithmetic mean of the token probabilities.
    pub mean_token_prob: Option<f32>,
    /// Whether the decoder was instructed to translate into English.
    pub output_was_translated: bool,
}

/// Computes root-mean-square PCM amplitude, treating non-finite samples as zero.
///
/// Returns zero for empty input. The calculation does not depend on sample rate.
pub fn pcm_rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_squares: f64 = samples
        .iter()
        .copied()
        .filter(|s| s.is_finite())
        .map(|s| f64::from(s).powi(2))
        .sum();
    (sum_squares / samples.len() as f64).sqrt() as f32
}

const SUBTITLE_ARTIFACTS: &str = concat!(
    r"subtitles by the amara\.org community|subtítulos realizados por la comunidad de amara\.org|",
    r"untertitel der amara\.org-community|untertitel im auftrag des zdf|amara\.org|",
    r"Субтитры сделал DimaTorzok|Редактор субтитров|",
    r"(?:اردو،\s*السلام\s*علیکم[۔.]?\s*)?آج\s+ہم\s+اس\s+موضوع\s+پر\s+بات\s+کریں\s+گے|",
    r"(?:नमस्ते[,،]?\s*)?आज\s+हम\s+इस\s+विषय\s+पर\s+बात\s+करेंगे|",
    r"ご視聴ありがとうございました|시청해주셔서 감사합니다|謝謝觀看|字幕|موسیقی|موسيقى",
);
const BRACKETED_ARTIFACTS: &str = r"blank_audio|(?:upbeat\s+)?music|applause";

static ARTIFACTS: LazyLock<Regex> = LazyLock::new(|| {
    let bracketed = format!("{BRACKETED_ARTIFACTS}|{SUBTITLE_ARTIFACTS}|[♪♫]+");
    built_in_regex(&format!(
        r"(?i)(?:\[\s*(?:{bracketed})\s*\]|\(\s*(?:{bracketed})\s*\)|[♪♫]+|\b(?:blank_audio|{SUBTITLE_ARTIFACTS})\b)[.!?,。！？،؟۔]*"
    ))
});

// Keep real script letters out of this class even if they look like punctuation.
static CONTENT: LazyLock<Regex> = LazyLock::new(|| built_in_regex(r"[^\p{P}\p{M}\p{C}\s]"));

const DEVANAGARI_LANGS: &[&str] = &["hi", "mr", "ne", "sa"];
const OTHER_ARABIC_LANGS: &[&str] = &["ar", "fa", "ps"];
const SCRIPT_LANGUAGES: &[(&str, &[&str])] = &[
    ("Arabic", &["ur", "ar", "fa", "ps"]),
    ("Devanagari", DEVANAGARI_LANGS),
    ("Han", &["zh", "ja", "yue"]),
    ("Hiragana", &["ja"]),
    ("Katakana", &["ja"]),
    ("Bopomofo", &["zh", "yue"]),
    ("Hangul", &["ko"]),
    (
        "Cyrillic",
        &["ru", "uk", "bg", "sr", "be", "kk", "mk", "mn"],
    ),
    ("Greek", &["el"]),
    ("Hebrew", &["he", "yi", "iw"]),
    ("Thai", &["th"]),
    ("Armenian", &["hy"]),
    ("Bengali", &["bn", "as"]),
    ("Ethiopic", &["am", "ti"]),
    ("Georgian", &["ka"]),
    ("Gujarati", &["gu"]),
    ("Gurmukhi", &["pa"]),
    ("Kannada", &["kn"]),
    ("Khmer", &["km"]),
    ("Lao", &["lo"]),
    ("Malayalam", &["ml"]),
    ("Myanmar", &["my"]),
    ("Oriya", &["or"]),
    ("Sinhala", &["si"]),
    ("Tamil", &["ta"]),
    ("Telugu", &["te"]),
    ("Tibetan", &["bo"]),
];

// One capture per script, followed by any other non-Latin script. The suffix
// keeps combining marks and joiners with the character they were attached to.
static SCRIPT_CHARS: LazyLock<Regex> = LazyLock::new(|| {
    let groups = SCRIPT_LANGUAGES
        .iter()
        .map(|(script, _)| match *script {
            "Katakana" => r"([\p{sc=Katakana}ーｰ])".to_owned(),
            _ => format!(r"(\p{{sc={script}}})"),
        })
        .chain(std::iter::once(
            r"([^\p{sc=Latin}\p{sc=Common}\p{sc=Inherited}])".to_owned(),
        ))
        .collect::<Vec<_>>()
        .join("|");
    built_in_regex(&format!(r"(?:{groups})[\p{{M}}\u{{200C}}\u{{200D}}]*"))
});

fn built_in_regex(pattern: &str) -> Regex {
    // Patterns come only from the constants above, never from transcripts.
    match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid built-in transcript regex: {error}"),
    }
}

/// Removes known subtitle and music artifacts, then rejects whole silence
/// phantoms when the audio or decoder evidence is weak.
///
/// Translation restricts output to Latin and shared script characters.
/// Otherwise the spoken profile allows its configured scripts; an empty
/// profile leaves scripts unrestricted except under a Roman Hinglish lock.
/// The returned transcript has collapsed whitespace and repetition loops.
pub fn guard_transcript_text(
    text: &str,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
    ctx: SilenceGuardContext,
) -> String {
    let artifacts_removed = ARTIFACTS.replace_all(text, " ");
    let low_confidence = ctx.rms_energy.is_some_and(|r| r < 0.007)
        || ctx.no_speech_prob.is_some_and(|p| p > 0.65)
        || ctx.mean_token_prob.is_some_and(|p| p < 0.30);
    let allows_japanese = has_spoken_lang(spoken_languages, "ja");
    if should_drop(&artifacts_removed, low_confidence, allows_japanese) {
        return String::new();
    }
    if inspect_compression_and_confidence(
        &artifacts_removed,
        ctx.mean_token_prob,
        ctx.no_speech_prob,
    )
    .rejected
    {
        return String::new();
    }

    let effective = effective_hindustani_script(script_pref, spoken_languages);
    let roman_lock = effective == HindustaniScript::RomanHinglish;
    // Let the legacy leading "हे" rescue and Roman transliteration run before
    // locking native text. Pass `script_pref` so `normalize_script_output` can
    // distinguish `Auto` from an explicit `UrduPersoArabic` lock, and rescue
    // unrequested Devanagari into Roman Hinglish instead of letting `lock_scripts`
    // wipe Hindustani speech into empty text (e.g. `ID=68` on `["en", "ur"]`).
    let normalized = if !ctx.output_was_translated {
        Cow::Owned(normalize_script_output(
            &artifacts_removed,
            spoken_languages,
            script_pref,
        ))
    } else {
        artifacts_removed
    };
    let locked = lock_scripts(
        &normalized,
        spoken_languages,
        roman_lock,
        ctx.output_was_translated,
    );
    let deshrapneled = strip_stray_mixed_script_shrapnel(&locked);
    let cleaned = strip_hallucination_loops(&deshrapneled);
    if should_drop(&cleaned, low_confidence, allows_japanese) {
        String::new()
    } else {
        cleaned
    }
}

fn should_drop(text: &str, low_confidence: bool, allows_japanese: bool) -> bool {
    // Only trim the edges: matching substrings would delete real sentences.
    let text = text.trim_matches(|c: char| !CONTENT.is_match(c.encode_utf8(&mut [0; 4])));
    if has_perso_arabic_subword_shrapnel(text, low_confidence) {
        return true;
    }
    let phrase = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    match phrase.as_str() {
        ""
        | "thanks for watching"
        | "thank you for watching"
        | "please subscribe"
        | "gracias por ver"
        | "gracias por ver el video"
        | "danke fürs zuschauen"
        | "vielen dank fürs zuschauen" => true,
        "ん" => low_confidence || !allows_japanese,
        "thank you"
        | "thank you very much"
        | "bye"
        | "you"
        | "gracias"
        | "muchas gracias"
        | "vielen dank"
        | "danke"
        | "merci"
        | "merci beaucoup"
        | "धन्यवाद"
        | "शुक्रिया"
        | "شکریہ"
        | "شكرا" => low_confidence,
        _ => false,
    }
}

/// Detects Whisper BPE subword shrapnel emitted when forced `<|ur|>` decoding
/// runs on breath/hesitation noise (e.g. `"جایجا لیٹزو و ٩٥ م ٹ س 15 ر ج"`),
/// without dropping high-confidence Urdu/Arabic sentences that legitimately name
/// two single letters (e.g. `"حرف ب اور پ لکھیں"`).
fn has_perso_arabic_subword_shrapnel(text: &str, low_confidence: bool) -> bool {
    let mut total_tokens = 0usize;
    let mut isolated_letters = 0usize;
    for tok in text.split_whitespace() {
        total_tokens += 1;
        let trimmed = tok.trim_matches(|c: char| !c.is_alphabetic());
        let mut chars = trimmed.chars();
        if let (Some(only), None) = (chars.next(), chars.next()) {
            if is_perso_arabic_letter(only) && only != 'و' && only != 'ء' {
                isolated_letters += 1;
            }
        }
    }
    if low_confidence {
        isolated_letters >= 2
    } else {
        isolated_letters >= 4 && isolated_letters.saturating_mul(3) >= total_tokens
    }
}

/// Strips stray 1-2 character Perso-Arabic subword fragments injected into an
/// overwhelmingly ASCII-Latin phrase (e.g. `"سی bits come out ی league ou wool"`)
/// while leaving genuine bilingual Urdu words (`>= 3` Perso-Arabic letters such as
/// `"بھائی"`, `"انشاءاللہ"`, `"شکریہ"`, `"مسئلہ"`) completely intact.
fn strip_stray_mixed_script_shrapnel(text: &str) -> Cow<'_, str> {
    let mut latin_letters = 0usize;
    let mut arabic_letters = 0usize;
    for c in text.chars() {
        if c.is_ascii_alphabetic() {
            latin_letters += 1;
        } else if is_perso_arabic_letter(c) {
            arabic_letters += 1;
        }
    }
    if arabic_letters == 0 || latin_letters < 10 || latin_letters < arabic_letters.saturating_mul(4)
    {
        return Cow::Borrowed(text);
    }
    let has_real_urdu_word = text
        .split_whitespace()
        .any(|tok| tok.chars().filter(|&c| is_perso_arabic_letter(c)).count() >= 3);
    if has_real_urdu_word {
        return Cow::Borrowed(text);
    }
    let kept: Vec<&str> = text
        .split_whitespace()
        .filter(|tok| !tok.chars().any(is_perso_arabic_letter))
        .collect();
    Cow::Owned(kept.join(" "))
}

fn is_perso_arabic_letter(c: char) -> bool {
    matches!(
        c as u32,
        0x0620..=0x065F
            | 0x066E..=0x06D5
            | 0x06EE..=0x06EF
            | 0x06FA..=0x06FF
            | 0x0750..=0x077F
            | 0x08A0..=0x08FF
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF
    )
}

fn lock_scripts(text: &str, langs: &[String], roman_lock: bool, translated: bool) -> String {
    let unrestricted = !translated && langs.is_empty();
    let allowed: Vec<bool> = SCRIPT_LANGUAGES
        .iter()
        .map(|&(script, codes)| {
            !translated
                && if roman_lock && script == "Arabic" {
                    OTHER_ARABIC_LANGS
                        .iter()
                        .any(|code| has_spoken_lang(langs, code))
                } else {
                    unrestricted || codes.iter().any(|code| has_spoken_lang(langs, code))
                }
        })
        .chain(std::iter::once(unrestricted))
        .collect();
    let mut out = String::with_capacity(text.len());
    for token in text.split_whitespace() {
        let kept = if token.is_ascii() || (unrestricted && !roman_lock) {
            Cow::Borrowed(token)
        } else {
            strip_unallowed_chars(token, &allowed)
        };
        if matches!(&kept, Cow::Owned(_)) && !CONTENT.is_match(&kept) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&kept);
    }
    out
}

fn strip_unallowed_chars<'a>(token: &'a str, allowed: &[bool]) -> Cow<'a, str> {
    let mut rejected = SCRIPT_CHARS
        .captures_iter(token)
        .filter_map(|captures| {
            let script = captures.iter().skip(1).position(|m| m.is_some())?;
            (!allowed[script]).then(|| captures.get_match())
        })
        .peekable();
    let mut out = String::new();
    let mut cursor = 0;
    while let Some(span) = rejected.next() {
        let mut start = span.start();
        let mut end = span.end();
        while let Some(next) = rejected.next_if(|next| next.start() == end) {
            end = next.end();
        }
        // Drop brackets that wrapped only the rejected script, not those
        // enclosing an allowed part of the word or sentence.
        if let (Some(left), Some(right)) = (
            token[cursor..start].chars().next_back(),
            token[end..].chars().next(),
        ) {
            if matches!((left, right), ('(', ')') | ('[', ']') | ('{', '}')) {
                start -= left.len_utf8();
                end += right.len_utf8();
            }
        }
        out.push_str(&token[cursor..start]);
        cursor = end;
        if out.chars().next_back().is_some_and(char::is_alphanumeric)
            && token[end..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
        {
            out.push(' ');
        }
    }
    if cursor == 0 {
        Cow::Borrowed(token)
    } else {
        out.push_str(&token[cursor..]);
        Cow::Owned(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn langs(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_owned()).collect()
    }

    #[test]
    fn strips_artifacts_anywhere_in_english_and_hinglish() {
        let artifacts = [
            "[BLANK_AUDIO]",
            "[Music]",
            "[music]",
            "(Music)",
            "(music)",
            "(upbeat music)",
            "[Applause]",
            "♪",
            "♫",
            "موسیقی",
            "موسيقى",
            "[موسیقی]",
            "ご視聴ありがとうございました",
            "시청해주셔서 감사합니다",
            "Субтитры сделал DimaTorzok",
            "Редактор субтитров",
            "Amara.org",
            "字幕",
            "謝謝觀看",
            "ん",
            "[字幕]",
        ];
        let ctx = SilenceGuardContext::default();
        for artifact in artifacts {
            let input = format!("Before {artifact} after.");
            assert_eq!(
                guard_transcript_text(&input, &langs(&["en"]), HindustaniScript::Auto, ctx),
                "Before after.",
                "{artifact}"
            );
            assert_eq!(
                guard_transcript_text(
                    &format!("हाँ {artifact} ठीक है।"),
                    &langs(&["en", "hi"]),
                    HindustaniScript::RomanHinglish,
                    ctx
                ),
                "Haan theek hai.",
                "{artifact}"
            );
            assert_eq!(
                guard_transcript_text(
                    &format!("« {artifact} » …"),
                    &langs(&["en"]),
                    HindustaniScript::Auto,
                    ctx
                ),
                "",
                "{artifact}"
            );
        }
        assert_eq!(
            guard_transcript_text(
                "I like music.",
                &langs(&["en"]),
                HindustaniScript::Auto,
                ctx
            ),
            "I like music."
        );
    }

    #[test]
    fn each_low_confidence_signal_can_drop_a_whole_phantom() {
        let spoken = langs(&["en"]);
        for ctx in [
            SilenceGuardContext {
                rms_energy: Some(0.001),
                ..Default::default()
            },
            SilenceGuardContext {
                rms_energy: Some(0.05),
                no_speech_prob: Some(0.85),
                ..Default::default()
            },
            SilenceGuardContext {
                mean_token_prob: Some(0.29),
                ..Default::default()
            },
        ] {
            for phrase in [
                "Thank you.",
                "thank you",
                "thank you very much.",
                "you",
                "bye!",
                "…",
                "Thank you. Thank you. Thank you. Thank you.",
            ] {
                assert_eq!(
                    guard_transcript_text(phrase, &spoken, HindustaniScript::Auto, ctx),
                    "",
                    "{phrase}"
                );
            }
            assert_eq!(
                guard_transcript_text("Thank you, everyone!", &spoken, HindustaniScript::Auto, ctx),
                "Thank you, everyone!"
            );
        }
    }

    #[test]
    fn preserves_genuine_thanks_with_strong_or_missing_audio_evidence() {
        for rms_energy in [None, Some(0.05), Some(0.015), Some(0.007)] {
            let ctx = SilenceGuardContext {
                rms_energy,
                no_speech_prob: Some(0.65),
                mean_token_prob: Some(0.30),
                ..Default::default()
            };
            assert_eq!(
                guard_transcript_text(
                    "[Music]  Thank you.",
                    &langs(&["en"]),
                    HindustaniScript::Auto,
                    ctx
                ),
                "Thank you."
            );
        }
    }

    #[test]
    fn drops_promotional_phantoms_but_preserves_configured_japanese() {
        let ctx = SilenceGuardContext::default();
        for phrase in [
            "Thanks for watching.",
            "THANK YOU FOR WATCHING!",
            "Please subscribe.",
            "ん。",
        ] {
            assert_eq!(
                guard_transcript_text(phrase, &[], HindustaniScript::Auto, ctx),
                ""
            );
        }
        assert_eq!(
            guard_transcript_text("ん。", &langs(&["ja-JP"]), HindustaniScript::Auto, ctx),
            "ん。"
        );
    }

    #[test]
    fn drops_urdu_and_arabic_phantoms_only_on_low_evidence() {
        for (phrase, lang) in [("شکریہ۔", "ur"), ("شكرا", "ar")] {
            let spoken = langs(&[lang]);
            assert_eq!(
                guard_transcript_text(
                    phrase,
                    &spoken,
                    HindustaniScript::UrduPersoArabic,
                    SilenceGuardContext::default()
                ),
                phrase
            );
            assert_eq!(
                guard_transcript_text(
                    phrase,
                    &spoken,
                    HindustaniScript::UrduPersoArabic,
                    SilenceGuardContext {
                        no_speech_prob: Some(0.85),
                        ..Default::default()
                    }
                ),
                ""
            );
        }

        // Regression tests for Take #51 (isolated BPE subword shrapnel) and
        // Take #54 (1-2 letter Perso-Arabic shrapnel inside English hesitation),
        // while preserving genuine bilingual Urdu words.
        let bilingual = langs(&["en", "ur"]);
        assert_eq!(
            guard_transcript_text(
                "جایجا لیٹزو و ٩٥ م ٹ س 15 ر ج و ان پندرہ ج پندرہ س ر م س اللہ",
                &bilingual,
                HindustaniScript::UrduPersoArabic,
                SilenceGuardContext::default()
            ),
            ""
        );
        assert_eq!(
            guard_transcript_text(
                "سی bits come out ی league ou wool.",
                &bilingual,
                HindustaniScript::UrduPersoArabic,
                SilenceGuardContext::default()
            ),
            "bits come out league ou wool."
        );
        assert_eq!(
            guard_transcript_text(
                "Sure بھائی، انشاءاللہ we will ship today.",
                &bilingual,
                HindustaniScript::UrduPersoArabic,
                SilenceGuardContext::default()
            ),
            "Sure بھائی، انشاءاللہ we will ship today."
        );
        // High-confidence Urdu sentence naming two single letters must be preserved,
        // while low-confidence noise with two isolated letters is still dropped.
        assert_eq!(
            guard_transcript_text(
                "حرف ب اور پ لکھیں",
                &langs(&["ur"]),
                HindustaniScript::UrduPersoArabic,
                SilenceGuardContext {
                    mean_token_prob: Some(0.82),
                    ..SilenceGuardContext::default()
                }
            ),
            "حرف ب اور پ لکھیں"
        );
        assert_eq!(
            guard_transcript_text(
                "م س",
                &langs(&["ur"]),
                HindustaniScript::UrduPersoArabic,
                SilenceGuardContext {
                    mean_token_prob: Some(0.18),
                    ..SilenceGuardContext::default()
                }
            ),
            ""
        );
    }

    #[test]
    fn preserves_explicit_scripts_and_their_punctuation_verbatim() {
        let ctx = SilenceGuardContext::default();
        for (codes, script, text) in [
            (
                &["en", "ur"][..],
                HindustaniScript::UrduPersoArabic,
                "Status: کیا آپ آ رہے ہیں؟",
            ),
            (
                &["ur"][..],
                HindustaniScript::UrduPersoArabic,
                "کیا آپ آ رہے ہیں؟",
            ),
            (
                &["en", "ar"][..],
                HindustaniScript::Auto,
                "Status: أذهبتَ إلى المستشفى، يا محمد؟",
            ),
            (
                &["ar"][..],
                HindustaniScript::RomanHinglish,
                "أذهبتَ إلى المستشفى، يا محمد؟",
            ),
            (
                &["en", "hi"][..],
                HindustaniScript::Devanagari,
                "Status: हाँ, सब ठीक है।",
            ),
            (
                &["en", "hi"][..],
                HindustaniScript::Auto,
                "Status: हाँ, सब ठीक है।",
            ),
            (
                &["en", "hi"][..],
                HindustaniScript::RomanHinglish,
                "Haan, SDK rules UI mein kuch pending nahi hai.",
            ),
        ] {
            assert_eq!(
                guard_transcript_text(text, &langs(codes), script, ctx),
                text
            );
        }
    }

    #[test]
    fn roman_lock_transliterates_devanagari_and_drops_unconfigured_perso_arabic() {
        let text = "हाँ, SDK rules UI में कुछ pending नहीं है। اردو";
        for codes in [&["en", "ur"][..], &[]] {
            assert_eq!(
                guard_transcript_text(
                    text,
                    &langs(codes),
                    HindustaniScript::RomanHinglish,
                    SilenceGuardContext::default()
                ),
                "Haan, SDK rules UI mein kuch pending nahi hai."
            );
        }
    }

    #[test]
    fn normalizes_recoverable_devanagari_before_locking_native_text() {
        assert_eq!(
            guard_transcript_text(
                "हे, can you hear me out?",
                &langs(&["en", "ur"]),
                HindustaniScript::Auto,
                SilenceGuardContext::default(),
            ),
            "Hey, can you hear me out?"
        );
        // Regression test for Nov 23 12:10 PM (`ID=68`): pure Devanagari
        // Hindustani decoded under `["en", "ur"]` + `Auto` must transliterate
        // to Roman Hinglish rather than being wiped to `""` by `lock_scripts`.
        assert_eq!(
            guard_transcript_text(
                "और भाई बताओ क्या चल रहा है?",
                &langs(&["en", "ur"]),
                HindustaniScript::Auto,
                SilenceGuardContext::default(),
            ),
            "Aur bhai batao kya chal raha hai?"
        );
        // Prompt-echo cliché hallucinations are stripped cleanly.
        assert_eq!(
            guard_transcript_text(
                "آج ہم اس موضوع پر بات کریں گے۔",
                &langs(&["en", "ur"]),
                HindustaniScript::Auto,
                SilenceGuardContext::default(),
            ),
            ""
        );
    }

    #[test]
    fn removes_out_of_profile_tokens_and_attached_script_islands() {
        let text = "ПриветHelloこん  謝謝 한글 مرحبا नमस्ते กข עִבְרִית Γειά, (БГ!) world!";
        assert_eq!(
            guard_transcript_text(
                text,
                &langs(&["en"]),
                HindustaniScript::Auto,
                SilenceGuardContext::default()
            ),
            "Hello world!"
        );
        assert_eq!(
            guard_transcript_text(
                "确认 かなー status",
                &langs(&["ZH-CN"]),
                HindustaniScript::Auto,
                SilenceGuardContext::default()
            ),
            "确认 status"
        );
        assert_eq!(
            guard_transcript_text(
                "Hello(бг) testمرحباagain!",
                &langs(&["en"]),
                HindustaniScript::Auto,
                SilenceGuardContext::default(),
            ),
            "Hello test again!"
        );
    }

    #[test]
    fn translation_overrides_all_configured_scripts_and_roman_transliteration() {
        let spoken = langs(&[
            "en", "ur", "hi", "zh", "ja", "ko", "ru", "th", "he", "el", "ta",
        ]);
        let ctx = SilenceGuardContext {
            output_was_translated: true,
            ..Default::default()
        };
        let text = "Café 42! مرحبا ठीक 漢字 かな 한글 БГ กข עִבְרִית Γειά தமிழ்";
        assert_eq!(
            guard_transcript_text(text, &spoken, HindustaniScript::RomanHinglish, ctx),
            "Café 42!"
        );
    }

    #[test]
    fn rms_handles_pcm_and_ignores_non_finite_samples() {
        assert_eq!(pcm_rms_energy(&[]), 0.0);
        assert_eq!(pcm_rms_energy(&[-0.25, 0.25]), 0.25);
        assert_eq!(pcm_rms_energy(&[1.0, f32::NAN, f32::INFINITY, 0.0]), 0.5);
    }

    #[test]
    fn compression_gate_drops_multilingual_loops_while_preserving_short_code_switches() {
        let spoken = langs(&["en", "ur"]);
        let ctx = SilenceGuardContext::default();
        for loop_text in [
            "Thank you. Thank you. Thank you. Thank you.",
            "اور اور اور اور اور",
            "هاهاهاهاهاهاهاهاهاهاهاهاهاهاهاهاهاها",
        ] {
            assert_eq!(
                guard_transcript_text(loop_text, &spoken, HindustaniScript::Auto, ctx),
                "",
                "expected loop rejection for: {loop_text}"
            );
        }

        assert_eq!(
            guard_transcript_text(
                "Let's ship this aur bhai kal milte hain",
                &spoken,
                HindustaniScript::Auto,
                ctx
            ),
            "Let's ship this aur bhai kal milte hain"
        );
        assert_eq!(
            guard_transcript_text("یہ PR merge کر دو", &spoken, HindustaniScript::Auto, ctx),
            "یہ PR merge کر دو"
        );
    }
}
