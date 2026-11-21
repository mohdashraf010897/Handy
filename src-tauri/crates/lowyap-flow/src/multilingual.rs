//! Multilingual orthographic steering, unwanted-Devanagari rescue, Urdu retry
//! policy, colloquial Roman Hinglish transliteration, and Whisper repetition
//! loop shielding.

use std::fs;
use std::ops::Range;
use std::path::Path;

use serde::{Deserialize, Serialize};

const MAX_INITIAL_PROMPT_CHARS: usize = 320;
const MAX_GLOSSARY_WORDS: usize = 24;
const MIN_LOOP_NGRAM_WORDS: usize = 2;
const MAX_LOOP_NGRAM_WORDS: usize = 8;
const MIN_LOOP_REPEATS: usize = 4;

/// Preferred output script when speaking Hindi/Urdu (Hindustani).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HindustaniScript {
    /// Infer the script from the configured Hindi and Urdu languages.
    #[default]
    Auto,
    /// Render Hindi or Urdu speech in colloquial Latin script.
    RomanHinglish,
    /// Prefer Urdu in Perso-Arabic script.
    UrduPersoArabic,
    /// Prefer Hindi in Devanagari script.
    Devanagari,
}

/// Reads `hindustani_script` from a settings JSON file, falling back to
/// [`HindustaniScript::Auto`] if the file or preference is unavailable or invalid.
pub fn read_hindustani_script_from_path(path: &Path) -> HindustaniScript {
    let Ok(contents) = fs::read_to_string(path) else {
        return HindustaniScript::Auto;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return HindustaniScript::Auto;
    };
    value
        .get("hindustani_script")
        .and_then(|v| HindustaniScript::deserialize(v).ok())
        .unwrap_or(HindustaniScript::Auto)
}

fn base_lang(code: &str) -> &str {
    let trimmed = code.trim();
    trimmed.split(['-', '_']).next().unwrap_or(trimmed)
}

pub(crate) fn has_spoken_lang(spoken_languages: &[String], iso: &str) -> bool {
    spoken_languages
        .iter()
        .any(|lang| base_lang(lang).eq_ignore_ascii_case(iso))
}

/// Resolves [`HindustaniScript::Auto`] from the user's configured `spoken_languages`.
pub fn effective_hindustani_script(
    pref: HindustaniScript,
    spoken_languages: &[String],
) -> HindustaniScript {
    if pref != HindustaniScript::Auto {
        return pref;
    }
    let has_hi = has_spoken_lang(spoken_languages, "hi");
    let has_ur = has_spoken_lang(spoken_languages, "ur");
    if has_hi && !has_ur {
        HindustaniScript::Devanagari
    } else if has_ur && !has_hi {
        HindustaniScript::UrduPersoArabic
    } else {
        HindustaniScript::Auto
    }
}

/// Builds an orthographic steering prompt followed by any non-empty `base_prompt`.
///
/// With no fixed `lang`, a mixed-language profile stays neutral. The result is
/// capped at 320 Unicode characters, and is `None` when there is no prompt text.
pub fn build_initial_prompt(
    base_prompt: Option<&str>,
    lang: Option<&str>,
    script_pref: HindustaniScript,
    spoken_languages: &[String],
) -> Option<String> {
    let steering = initial_prompt_steering(lang, spoken_languages, script_pref).unwrap_or("");
    let base = base_prompt.map(str::trim).unwrap_or("");
    match (steering.is_empty(), base.is_empty()) {
        (true, true) => None,
        (false, true) => Some(steering.to_owned()),
        (true, false) => Some(base.chars().take(MAX_INITIAL_PROMPT_CHARS).collect()),
        (false, false) => Some(
            steering
                .chars()
                .chain(std::iter::once(' '))
                .chain(base.chars())
                .take(MAX_INITIAL_PROMPT_CHARS)
                .collect(),
        ),
    }
}

fn initial_prompt_steering(
    resolved_lang: Option<&str>,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
) -> Option<&'static str> {
    let normalized_lang = resolved_lang
        .map(base_lang)
        .filter(|lang| !lang.is_empty() && !lang.eq_ignore_ascii_case("auto"))
        .map(str::to_ascii_lowercase);
    let lang = normalized_lang.as_deref();

    let all_perso_arabic = !spoken_languages.is_empty()
        && spoken_languages.iter().all(|code| {
            matches!(
                base_lang(code).to_ascii_lowercase().as_str(),
                "ur" | "ar" | "fa" | "ps"
            )
        });

    let effective = effective_hindustani_script(script_pref, spoken_languages);
    // Critical Whisper invariant: `<|startofprev|>` (`RunOptions.prompt`) runs
    // BEFORE Whisper predicts the `<|lang|>` token when `lang == None`. Injecting
    // Perso-Arabic or Hinglish script on Pass 1 while `"en"` (or another Latin
    // language) is also in `spoken_languages` biases quiet English utterances
    // into `<|ur|>` translation! Keep Pass 1 neutral when mixed languages are
    // configured; `should_retry_chunk_as_urdu` re-decodes Hindustani chunks with
    // `lang = Some("ur")` + Perso-Arabic steering, and `normalize_script_output`
    // transliterates Devanagari to Roman Hinglish deterministically.
    match effective {
        HindustaniScript::RomanHinglish if matches!(lang, Some("hi") | Some("ur")) => Some(
            "Haan bilkul, theek hai. Let's check the status aur next steps par baat karte hain.",
        ),
        HindustaniScript::UrduPersoArabic => {
            let allow_perso_arabic_prompt =
                matches!(lang, Some("ur") | Some("ar")) || (lang.is_none() && all_perso_arabic);
            if allow_perso_arabic_prompt && has_spoken_lang(spoken_languages, "ar") {
                Some("اردو اور عربی: السلام علیکم۔ لم يَفْعَلْ، لن يَفْعَلَ، ماضٍ ومضارع۔")
            } else if allow_perso_arabic_prompt {
                Some("اردو، السلام علیکم۔ آج ہم اس موضوع پر بات کریں گے۔")
            } else {
                None
            }
        }
        HindustaniScript::Devanagari if lang == Some("hi") => {
            Some("नमस्ते, आज हम इस विषय पर बात करेंगे।")
        }
        _ => None,
    }
}

/// Builds the legacy steering prompt and custom-word glossary for a model.
///
/// Returns `None` if the model does not accept a prompt, and includes the
/// glossary only when there is no separate vocabulary slot.
pub fn build_initial_prompt_for_model(
    supports_initial_prompt: bool,
    supports_vocabulary: bool,
    resolved_lang: Option<&str>,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
    custom_words: &[String],
) -> Option<String> {
    if !supports_initial_prompt {
        return None;
    }
    let steering = initial_prompt_steering(resolved_lang, spoken_languages, script_pref);
    let mut prompt = String::new();
    let mut char_count = 0usize;

    if let Some(base) = steering {
        prompt.push_str(base);
        char_count = base.chars().count();
    }

    if !custom_words.is_empty() && !supports_vocabulary {
        let mut added_any = false;
        for word in custom_words
            .iter()
            .map(|w| w.trim())
            .filter(|w| !w.is_empty() && !w.contains("<|") && !w.contains('\0'))
            .take(MAX_GLOSSARY_WORDS)
        {
            let sep = if !added_any {
                if prompt.is_empty() {
                    ""
                } else {
                    " "
                }
            } else {
                ", "
            };
            let sep_chars = sep.chars().count();
            let word_chars = word.chars().count();
            if char_count + sep_chars + word_chars > MAX_INITIAL_PROMPT_CHARS {
                if prompt.is_empty() {
                    prompt.extend(word.chars().take(MAX_INITIAL_PROMPT_CHARS));
                }
                break;
            }
            prompt.push_str(sep);
            prompt.push_str(word);
            char_count += sep_chars + word_chars;
            added_any = true;
        }
    }

    if prompt.is_empty() {
        None
    } else if prompt.chars().count() > MAX_INITIAL_PROMPT_CHARS {
        Some(prompt.chars().take(MAX_INITIAL_PROMPT_CHARS).collect())
    } else {
        Some(prompt)
    }
}

/// Returns `true` if `text` contains any character in the Devanagari block (`U+0900..=U+097F`).
pub fn contains_devanagari(text: &str) -> bool {
    text.chars().any(is_devanagari_char)
}

#[inline]
fn is_devanagari_char(c: char) -> bool {
    ('\u{0900}'..='\u{097F}').contains(&c)
}

#[inline]
fn is_perso_arabic_char(c: char) -> bool {
    matches!(
        c,
        '\u{0600}'..='\u{06FF}'
            | '\u{0750}'..='\u{077F}'
            | '\u{08A0}'..='\u{08FF}'
            | '\u{FB50}'..='\u{FDFF}'
            | '\u{FE70}'..='\u{FEFF}'
    )
}

/// Returns `true` if `text` contains a character in the Perso-Arabic blocks
/// used by the transcript heuristics.
pub fn contains_perso_arabic(text: &str) -> bool {
    text.chars().any(is_perso_arabic_char)
}

#[inline]
fn is_devanagari_word_char(c: char) -> bool {
    is_devanagari_char(c)
        && !matches!(
            c,
            '\u{0964}' | '\u{0965}' | '\u{0966}'..='\u{096F}' | '\u{0970}'
        )
}

/// Detects a predominantly ASCII-English sentence (> 75% of letters are ASCII
/// Latin) whose only Devanagari content is a spurious leading `"हे"` interjection
/// (e.g. `"हे, can you hear me out?"`).
fn split_spurious_leading_he<'a>(
    text: &'a str,
    spoken_languages: &[String],
) -> Option<(&'a str, &'a str)> {
    if has_spoken_lang(spoken_languages, "hi") || !contains_devanagari(text) {
        return None;
    }

    let ascii_latin = text.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let total_letters = text
        .chars()
        .filter(|c| c.is_alphabetic() || is_devanagari_word_char(*c))
        .count();
    if total_letters == 0 || ascii_latin * 4 <= total_letters * 3 {
        return None;
    }

    let leading_byte_len = text
        .char_indices()
        .take_while(|(_, c)| c.is_whitespace() || matches!(c, '"' | '\'' | '“' | '‘' | '(' | '['))
        .last()
        .map(|(idx, c)| idx + c.len_utf8())
        .unwrap_or(0);

    let after_leading = &text[leading_byte_len..];
    let rest = after_leading.strip_prefix("हे")?;
    if rest.chars().next().is_some_and(is_devanagari_word_char) {
        return None;
    }
    if contains_devanagari(rest) {
        return None;
    }
    Some((&text[..leading_byte_len], rest))
}

/// Returns `true` when more than 75% of letter characters in `text` are ASCII
/// Latin (`a..=z` / `A..=Z`), indicating predominantly English speech.
fn is_predominantly_ascii_latin(text: &str) -> bool {
    let ascii_latin = text.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let total_letters = text
        .chars()
        .filter(|c| c.is_alphabetic() || is_devanagari_word_char(*c))
        .count();
    total_letters > 0 && ascii_latin * 4 > total_letters * 3
}

/// Decides whether an untranslated, auto-detected chunk should be retried in
/// Urdu based on its transcript and effective script, without a detector hint.
pub fn should_retry_chunk_as_urdu(
    text: &str,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
    output_was_translated: bool,
) -> bool {
    !output_was_translated
        && should_retry_chunk_as_urdu_with_context(
            None,
            text,
            spoken_languages,
            script_pref,
            "auto",
        )
}

/// Decides whether an auto-detected chunk/utterance should be re-decoded once
/// with `language = Some("ur")` and an Urdu initial prompt because Whisper
/// defaulted Hindustani audio to Hindi (`"hi"`) / Devanagari.
pub fn should_retry_chunk_as_urdu_with_context(
    model_detected_lang: Option<&str>,
    text: &str,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
    explicit_selected_lang: &str,
) -> bool {
    if !explicit_selected_lang.trim().eq_ignore_ascii_case("auto") {
        return false;
    }
    if effective_hindustani_script(script_pref, spoken_languages)
        != HindustaniScript::UrduPersoArabic
    {
        return false;
    }
    // Never force `language = "ur"` on a predominantly English utterance
    // (such as `"Hey, can you hear me out?"` or `"हे, can you hear me out?"`),
    // even if Whisper's acoustic detector tagged the clip `"hi"`, because
    // re-decoding English audio with `<|ur|>` translates it into Urdu.
    if has_spoken_lang(spoken_languages, "en") && is_predominantly_ascii_latin(text) {
        return false;
    }
    let detected_hi = model_detected_lang
        .map(base_lang)
        .is_some_and(|lang| lang.eq_ignore_ascii_case("hi"));
    contains_devanagari(text) || (detected_hi && !is_predominantly_ascii_latin(text))
}

/// Normalizes script output according to the user's effective Hindustani script
/// preference and spoken languages.
pub fn normalize_script_output(
    text: &str,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
) -> String {
    if !contains_devanagari(text) {
        return text.to_string();
    }
    match effective_hindustani_script(script_pref, spoken_languages) {
        HindustaniScript::RomanHinglish => devanagari_to_roman_hinglish(text),
        HindustaniScript::UrduPersoArabic | HindustaniScript::Auto => {
            if let Some((prefix, rest)) = split_spurious_leading_he(text, spoken_languages) {
                let mut out = String::with_capacity(text.len() + 3);
                out.push_str(prefix);
                out.push_str("Hey");
                out.push_str(rest);
                out
            } else if has_spoken_lang(spoken_languages, "en")
                && !has_spoken_lang(spoken_languages, "hi")
                && is_predominantly_ascii_latin(text)
            {
                devanagari_to_roman_hinglish(text)
            } else {
                text.to_string()
            }
        }
        HindustaniScript::Devanagari => text.to_string(),
    }
}

/// Deterministic, zero-latency Devanagari-to-colloquial Roman Hinglish
/// transliterator. Leaves all Latin/English, numbers, punctuation, and
/// Perso-Arabic spans untouched.
pub fn devanagari_to_roman_hinglish(text: &str) -> String {
    if !contains_devanagari(text) {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut at_sentence_start = true;
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if is_devanagari_word_char(ch) {
            while chars
                .peek()
                .is_some_and(|&(_, c)| is_devanagari_word_char(c))
            {
                chars.next();
            }
            let end = chars.peek().map_or(text.len(), |&(index, _)| index);
            let word = &text[start..end];
            let roman = transliterate_devanagari_word(word);
            if at_sentence_start {
                push_capitalized_ascii(&mut out, &roman);
            } else {
                out.push_str(&roman);
            }
            at_sentence_start = false;
        } else if matches!(ch, '\u{0964}' | '\u{0965}') {
            out.push('.');
            at_sentence_start = true;
        } else if ch == '\u{0970}' {
            out.push('.');
        } else if ('\u{0966}'..='\u{096F}').contains(&ch) {
            let digit = char::from_u32(u32::from('0') + (u32::from(ch) - 0x0966)).unwrap_or(ch);
            out.push(digit);
            at_sentence_start = false;
        } else {
            out.push(ch);
            if matches!(ch, '.' | '!' | '?' | '\n') {
                at_sentence_start = true;
            } else if !ch.is_whitespace() && !matches!(ch, '"' | '\'' | '“' | '‘' | '(' | '[' | '{')
            {
                at_sentence_start = false;
            }
        }
    }

    out
}

fn push_capitalized_ascii(out: &mut String, word: &str) {
    let mut chars = word.chars();
    if let Some(first) = chars.next() {
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
    }
}

fn irregular_hinglish_word(word: &str) -> Option<&'static str> {
    match word {
        "हे" => Some("hey"),
        "हेलो" | "हैलो" | "हॅलो" => Some("hello"),
        "हाय" => Some("hi"),
        "है" => Some("hai"),
        "हैं" | "हँ" => Some("hain"),
        "हूँ" | "हूं" => Some("hoon"),
        "हो" => Some("ho"),
        "था" => Some("tha"),
        "थी" => Some("thi"),
        "थे" => Some("the"),
        "मैं" | "मै" => Some("main"),
        "में" => Some("mein"),
        "नहीं" | "नही" => Some("nahi"),
        "क्या" => Some("kya"),
        "क्यों" | "क्यूँ" | "क्यूं" => Some("kyun"),
        "क्योंकि" => Some("kyunki"),
        "कैसे" => Some("kaise"),
        "कैसा" => Some("kaisa"),
        "कैसी" => Some("kaisi"),
        "कहाँ" | "कहां" => Some("kahan"),
        "यहाँ" | "यहां" => Some("yahan"),
        "वहाँ" | "वहां" => Some("wahan"),
        "कौन" => Some("kaun"),
        "और" => Some("aur"),
        "तो" => Some("toh"),
        "यह" | "ये" => Some("yeh"),
        "वह" | "वो" => Some("woh"),
        "ठीक" => Some("theek"),
        "करते" => Some("karte"),
        "करता" => Some("karta"),
        "करती" => Some("karti"),
        "करना" => Some("karna"),
        "करके" => Some("karke"),
        "करो" => Some("karo"),
        "करें" => Some("karein"),
        "किया" => Some("kiya"),
        "आप" => Some("aap"),
        "आपका" => Some("aapka"),
        "आपके" => Some("aapke"),
        "आपको" => Some("aapko"),
        "हम" => Some("hum"),
        "हमें" => Some("humein"),
        "तुम" => Some("tum"),
        "मुझे" => Some("mujhe"),
        "चाहिए" | "चाहिये" => Some("chahiye"),
        "हाँ" | "हां" => Some("haan"),
        "अच्छा" => Some("accha"),
        "बहुत" => Some("bahut"),
        "थोड़ा" => Some("thoda"),
        "ज़्यादा" | "ज्यादा" => Some("zyada"),
        "कुछ" => Some("kuch"),
        "कोई" => Some("koi"),
        "बिल्कुल" | "बिलकुल" => Some("bilkul"),
        "दोबारा" | "दुबारा" => Some("dobara"),
        "फिर" => Some("phir"),
        "लेकिन" => Some("lekin"),
        "अरे" => Some("arey"),
        _ => None,
    }
}

fn transliterate_devanagari_word(word: &str) -> String {
    if let Some(mapped) = irregular_hinglish_word(word) {
        return mapped.to_string();
    }

    let chars: Vec<char> = word.chars().collect();
    let mut out = String::with_capacity(chars.len() * 2);
    let mut i = 0usize;
    let mut consonant_count = 0usize;
    let mut prev_was_virama = false;

    while i < chars.len() {
        let ch = chars[i];
        if is_consonant(ch) {
            let base = if ch == 'क'
                && chars.get(i + 1) == Some(&'\u{094D}')
                && chars.get(i + 2) == Some(&'ष')
            {
                i += 3;
                "ksh"
            } else if ch == 'ज'
                && chars.get(i + 1) == Some(&'\u{094D}')
                && chars.get(i + 2) == Some(&'ञ')
            {
                i += 3;
                "gy"
            } else if ch == 'च'
                && chars.get(i + 1) == Some(&'\u{094D}')
                && chars.get(i + 2) == Some(&'छ')
            {
                i += 3;
                "cch"
            } else if chars.get(i + 1) == Some(&'\u{093C}') {
                i += 2;
                consonant_roman(ch, true)
            } else {
                i += 1;
                consonant_roman(ch, false)
            };
            out.push_str(base);

            if chars.get(i) == Some(&'\u{094D}') {
                i += 1;
                prev_was_virama = true;
            } else if let Some(&m) = chars.get(i).filter(|&&m| is_matra(m)) {
                i += 1;
                let at_word_end = is_word_tail_only_modifiers(&chars, i);
                out.push_str(matra_roman(m, at_word_end && consonant_count > 0));
                prev_was_virama = false;
            } else {
                let at_word_end = is_word_tail_only_modifiers(&chars, i);
                if !at_word_end || prev_was_virama || (consonant_count == 0 && i == chars.len()) {
                    out.push('a');
                }
                prev_was_virama = false;
            }
            consonant_count += 1;
        } else if let Some(vowel) = independent_vowel_roman(ch) {
            if ch == 'ए' && out.ends_with('i') {
                out.push_str("ye");
            } else {
                out.push_str(vowel);
            }
            i += 1;
            prev_was_virama = false;
        } else if ch == '\u{0901}' {
            out.push('n');
            i += 1;
        } else if ch == '\u{0902}' {
            let labial_next = chars
                .get(i + 1)
                .is_some_and(|c| matches!(c, 'प' | 'फ' | 'ब' | 'भ' | 'म'));
            out.push(if labial_next { 'm' } else { 'n' });
            i += 1;
        } else if ch == '\u{0903}' {
            out.push('h');
            i += 1;
        } else {
            i += 1;
        }
    }

    out
}

fn is_word_tail_only_modifiers(chars: &[char], idx: usize) -> bool {
    chars[idx..]
        .iter()
        .all(|c| matches!(c, '\u{0901}' | '\u{0902}' | '\u{0903}'))
}

fn is_consonant(c: char) -> bool {
    ('\u{0915}'..='\u{0939}').contains(&c) || ('\u{0958}'..='\u{095F}').contains(&c)
}

fn consonant_roman(c: char, nukta: bool) -> &'static str {
    if nukta {
        return match c {
            'क' => "q",
            'ख' => "kh",
            'ग' => "gh",
            'ज' => "z",
            'ड' => "d",
            'ढ' => "dh",
            'फ' => "f",
            'य' => "y",
            other => consonant_roman(other, false),
        };
    }
    match c {
        'क' => "k",
        'ख' => "kh",
        'ग' => "g",
        'घ' => "gh",
        'ङ' => "ng",
        'च' => "ch",
        'छ' => "chh",
        'ज' => "j",
        'झ' => "jh",
        'ञ' => "ny",
        'ट' | 'त' => "t",
        'ठ' | 'थ' => "th",
        'ड' | 'द' | '\u{095C}' => "d",
        'ढ' | 'ध' | '\u{095D}' => "dh",
        'ण' | 'न' | 'ऩ' => "n",
        'प' => "p",
        'फ' => "ph",
        'ब' => "b",
        'भ' => "bh",
        'म' => "m",
        'य' | '\u{095F}' => "y",
        'र' | 'ऱ' => "r",
        'ल' | 'ळ' | 'ऴ' => "l",
        'व' => "v",
        'श' | 'ष' => "sh",
        'स' => "s",
        'ह' => "h",
        '\u{0958}' => "q",
        '\u{0959}' => "kh",
        '\u{095A}' => "gh",
        '\u{095B}' => "z",
        '\u{095E}' => "f",
        _ => "",
    }
}

fn independent_vowel_roman(c: char) -> Option<&'static str> {
    match c {
        'अ' | 'ऄ' => Some("a"),
        'आ' => Some("aa"),
        'इ' => Some("i"),
        'ई' => Some("ee"),
        'उ' => Some("u"),
        'ऊ' => Some("oo"),
        'ऋ' | 'ॠ' => Some("ri"),
        'ऌ' | 'ॡ' => Some("li"),
        'ऍ' | 'ऎ' | 'ए' => Some("e"),
        'ऐ' => Some("ai"),
        'ऑ' | 'ऒ' | 'ओ' => Some("o"),
        'औ' => Some("au"),
        _ => None,
    }
}

fn is_matra(c: char) -> bool {
    matches!(
        c,
        '\u{093E}'
            | '\u{093F}'
            | '\u{0940}'
            | '\u{0941}'
            | '\u{0942}'
            | '\u{0943}'
            | '\u{0944}'
            | '\u{0945}'
            | '\u{0946}'
            | '\u{0947}'
            | '\u{0948}'
            | '\u{0949}'
            | '\u{094A}'
            | '\u{094B}'
            | '\u{094C}'
    )
}

fn matra_roman(c: char, word_end: bool) -> &'static str {
    match c {
        '\u{093E}' => {
            if word_end {
                "a"
            } else {
                "aa"
            }
        }
        '\u{093F}' => "i",
        '\u{0940}' => {
            if word_end {
                "i"
            } else {
                "ee"
            }
        }
        '\u{0941}' => "u",
        '\u{0942}' => "oo",
        '\u{0943}' | '\u{0944}' => "ri",
        '\u{0945}' | '\u{0946}' | '\u{0947}' => "e",
        '\u{0948}' => "ai",
        '\u{0949}' | '\u{094A}' | '\u{094B}' => "o",
        '\u{094C}' => "au",
        _ => "",
    }
}

/// Strips known Whisper subtitle watermark hallucinations and collapses runaway
/// repetition loops where an identical 2..=8 word phrase repeats >= 4 times
/// consecutively down to a single occurrence.
pub fn strip_hallucination_loops(text: &str) -> String {
    let cleaned = strip_subtitle_watermarks(text);
    if cleaned.is_empty() {
        return cleaned;
    }

    let spans = split_preserving_whitespace(&cleaned);
    if spans.len() < MIN_LOOP_NGRAM_WORDS * MIN_LOOP_REPEATS {
        return cleaned;
    }

    let keys: Vec<String> = spans.iter().map(|s| normalized_word_key(s.word)).collect();
    let mut out = String::with_capacity(cleaned.len());
    let mut collapsed_any = false;
    let mut idx = 0usize;

    while idx < spans.len() {
        let mut matched_loop: Option<(usize, usize)> = None;
        for n in MIN_LOOP_NGRAM_WORDS..=MAX_LOOP_NGRAM_WORDS {
            if idx + n * MIN_LOOP_REPEATS > spans.len() {
                break;
            }
            if keys[idx..idx + n].iter().any(String::is_empty) {
                continue;
            }
            let mut repeats = 1usize;
            while idx + (repeats + 1) * n <= spans.len()
                && keys[idx..idx + n] == keys[idx + repeats * n..idx + (repeats + 1) * n]
            {
                repeats += 1;
            }
            if repeats >= MIN_LOOP_REPEATS {
                matched_loop = Some((n, repeats));
                break;
            }
        }

        if let Some((n, repeats)) = matched_loop {
            collapsed_any = true;
            for offset in 0..n {
                let span = &spans[idx + offset];
                if idx + offset > 0 || !span.leading_ws.is_empty() {
                    out.push_str(span.leading_ws);
                }
                if offset + 1 == n {
                    let last_repeat_word = spans[idx + repeats * n - 1].word;
                    out.push_str(&merge_closing_punctuation(span.word, last_repeat_word));
                } else {
                    out.push_str(span.word);
                }
            }
            idx += repeats * n;
        } else {
            out.push_str(spans[idx].leading_ws);
            out.push_str(spans[idx].word);
            idx += 1;
        }
    }

    if let Some(last) = spans.last() {
        out.push_str(last.trailing_ws);
    }

    if collapsed_any {
        out.trim().to_string()
    } else {
        cleaned
    }
}

struct WordSpan<'a> {
    leading_ws: &'a str,
    word: &'a str,
    trailing_ws: &'a str,
}

fn split_preserving_whitespace(text: &str) -> Vec<WordSpan<'_>> {
    let mut spans: Vec<WordSpan<'_>> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let word_and_tail = rest.trim_start();
        if word_and_tail.is_empty() {
            if let Some(last) = spans.last_mut() {
                last.trailing_ws = rest;
            }
            break;
        }
        let end = word_and_tail
            .find(char::is_whitespace)
            .unwrap_or(word_and_tail.len());
        spans.push(WordSpan {
            leading_ws: &rest[..rest.len() - word_and_tail.len()],
            word: &word_and_tail[..end],
            trailing_ws: "",
        });
        rest = &word_and_tail[end..];
    }

    spans
}

fn is_edge_punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '،' | '۔' | '؟' | '।' | '॥' | '“' | '”' | '‘' | '’' | '…' | '«' | '»'
        )
}

fn normalized_word_key(word: &str) -> String {
    word.trim_matches(is_edge_punct)
        .chars()
        .flat_map(char::to_lowercase)
        .collect()
}

fn merge_closing_punctuation(first_word: &str, last_word: &str) -> String {
    let first_core = first_word.trim_end_matches(is_edge_punct);
    let first_punct = &first_word[first_core.len()..];
    let last_core = last_word.trim_end_matches(is_edge_punct);
    let last_punct = &last_word[last_core.len()..];

    let chosen_punct = if !last_punct.is_empty() && (first_punct.is_empty() || first_punct == ",") {
        last_punct
    } else {
        first_punct
    };
    format!("{first_core}{chosen_punct}")
}

fn strip_subtitle_watermarks(text: &str) -> String {
    const WATERMARKS: &[&str] = &["subtitles by the amara.org community", "amara.org"];

    let lower = text.to_ascii_lowercase();
    if !WATERMARKS.iter().any(|w| lower.contains(w)) {
        return text.to_string();
    }

    let mut current = text.to_string();
    for pat in WATERMARKS {
        loop {
            let haystack_lower = current.to_ascii_lowercase();
            let Some(pos) = haystack_lower.find(pat) else {
                break;
            };
            // Only strip when `pos` and `end` sit on char + word/punct boundaries.
            if !current.is_char_boundary(pos) || !current.is_char_boundary(pos + pat.len()) {
                break;
            }
            let before_ok = current[..pos]
                .chars()
                .next_back()
                .is_none_or(|c| c.is_whitespace() || is_edge_punct(c));
            let mut end = pos + pat.len();
            let after_ok = current[end..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || is_edge_punct(c));
            if !before_ok || !after_ok {
                break;
            }
            // Also consume trailing punctuation attached directly to the watermark.
            while let Some(ch) = current[end..].chars().next() {
                if matches!(ch, '.' | '!' | '?' | ',') {
                    end += ch.len_utf8();
                } else {
                    break;
                }
            }
            let mut next = String::with_capacity(current.len());
            next.push_str(current[..pos].trim_end_matches(' '));
            let tail = current[end..].trim_start_matches(' ');
            if !next.is_empty()
                && !tail.is_empty()
                && !next.ends_with('\n')
                && !tail.starts_with('\n')
            {
                next.push(' ');
            }
            next.push_str(tail);
            current = next;
        }
    }

    current.trim().to_string()
}

/// Stage-2 LLM script instruction appended to cleanup system prompts when the
/// user's effective Hindustani script or bilingual profile calls for
/// deterministic script and code-switch governance.
pub fn llm_script_instruction(
    script_pref: HindustaniScript,
    spoken_languages: &[String],
) -> Option<&'static str> {
    match effective_hindustani_script(script_pref, spoken_languages) {
        HindustaniScript::RomanHinglish => Some(
            "Never translate between languages across clauses: keep every English sentence, \
             greeting, or technical term in English, and keep Hindi/Urdu conversational speech \
             in natural Latin-script Roman Hinglish (never Devanagari or Urdu Perso-Arabic script).",
        ),
        HindustaniScript::UrduPersoArabic if has_spoken_lang(spoken_languages, "ar") => Some(
            "Never translate between languages across clauses: keep every English sentence or \
             clause in English (Latin script), keep Urdu speech in Perso-Arabic script (never \
             Devanagari), preserve Classical Arabic phrases and morphology/conjugation terms \
             with accurate Arabic spelling (such as لم، لن، ماضی، مضارع), and convert phonetically \
             transliterated English software terms back to clean English when unambiguous.",
        ),
        HindustaniScript::UrduPersoArabic => Some(
            "Never translate between languages across clauses: keep every English sentence, \
             greeting, or phrase in English (Latin script), keep Urdu speech in Perso-Arabic \
             script (never Devanagari), and preserve English technical terms cleanly in Latin script.",
        ),
        HindustaniScript::Devanagari if distinct_spoken_language_count(spoken_languages) >= 2 => Some(
            "Never translate between languages across clauses: keep every English sentence or \
             phrase in English (Latin script) and keep Hindi clauses in Devanagari.",
        ),
        HindustaniScript::Auto if distinct_spoken_language_count(spoken_languages) >= 2 => Some(
            "Never translate between languages across clauses: preserve each sentence or clause \
             in the exact language and script in which it was spoken.",
        ),
        HindustaniScript::Devanagari | HindustaniScript::Auto => None,
    }
}

const BILINGUAL_SAMPLE_RATE: usize = 16_000;
/// Short-to-medium bilingual dictations (`4.5 s ..= 20.0 s`) split at genuine
/// inter-clause breath pauses (`>= 2.0 s` per clause, up to 4 clauses) while
/// longer monologues (`> 20.0 s`) stay intact for Whisper's native 30 s seeker.
const MIN_BILINGUAL_SPLIT_SAMPLES: usize = (BILINGUAL_SAMPLE_RATE * 9) / 2;
const MAX_BILINGUAL_CLIP_SAMPLES: usize = BILINGUAL_SAMPLE_RATE * 20;
const MIN_BILINGUAL_CLAUSE_SAMPLES: usize = BILINGUAL_SAMPLE_RATE * 2;
const MAX_BILINGUAL_CLAUSE_SAMPLES: usize = BILINGUAL_SAMPLE_RATE * 14;
const MAX_BILINGUAL_CLAUSES: usize = 4;
const ENVELOPE_BIN_SAMPLES: usize = BILINGUAL_SAMPLE_RATE / 20; // 50 ms

fn distinct_spoken_language_count(spoken_languages: &[String]) -> usize {
    let mut seen: Vec<String> = Vec::new();
    for raw in spoken_languages {
        let code = base_lang(raw).to_ascii_lowercase();
        if code.is_empty() {
            continue;
        }
        if !seen.contains(&code) {
            seen.push(code);
        }
    }
    seen.len()
}

/// Splits 4.5–20 seconds of 16 kHz PCM at natural breath pauses or low-energy
/// valleys when `selected_language == "auto"` and `spoken_languages` contains
/// two or more languages, allowing Whisper to assign an independent `<|lang|>`
/// token to each spoken clause (e.g. English followed by Urdu/Hinglish).
pub fn split_bilingual_clauses(
    pcm: &[f32],
    spoken_languages: &[String],
    selected_language: &str,
) -> Vec<Range<usize>> {
    let len = pcm.len();
    if len == 0 {
        return Vec::new();
    }
    if !selected_language.trim().eq_ignore_ascii_case("auto")
        || distinct_spoken_language_count(spoken_languages) < 2
        || !(MIN_BILINGUAL_SPLIT_SAMPLES..=MAX_BILINGUAL_CLIP_SAMPLES).contains(&len)
    {
        return std::iter::once(0..len).collect();
    }

    let n_bins = len / ENVELOPE_BIN_SAMPLES;
    if n_bins < 20 {
        return std::iter::once(0..len).collect();
    }

    let mut rms_bins = Vec::with_capacity(n_bins);
    for bin_idx in 0..n_bins {
        let start = bin_idx * ENVELOPE_BIN_SAMPLES;
        let slice = &pcm[start..start + ENVELOPE_BIN_SAMPLES];
        let mut sum_sq = 0.0_f64;
        for &s in slice {
            if s.is_finite() {
                let v = f64::from(s);
                sum_sq += v * v;
            }
        }
        rms_bins.push(((sum_sq / (ENVELOPE_BIN_SAMPLES as f64)).sqrt()) as f32);
    }

    // 3-bin (150 ms) moving average smooths single-syllable consonant dips while
    // preserving genuine >= 150-400 ms inter-clause pauses.
    let mut smooth = vec![0.0_f32; n_bins];
    for (i, smoothed) in smooth.iter_mut().enumerate() {
        let lo = i.saturating_sub(1);
        let hi = (i + 1).min(n_bins - 1);
        let count = (hi - lo + 1) as f32;
        let sum: f32 = rms_bins[lo..=hi].iter().copied().sum();
        *smoothed = sum / count;
    }

    let mut sorted = rms_bins.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let speech_ref = sorted[(sorted.len() * 80) / 100];
    if speech_ref <= 1e-5 {
        return std::iter::once(0..len).collect();
    }

    let mut ranges = Vec::new();
    let mut start = 0usize;

    while len.saturating_sub(start) >= MIN_BILINGUAL_SPLIT_SAMPLES
        && ranges.len() + 1 < MAX_BILINGUAL_CLAUSES
    {
        let search_lo = start + MIN_BILINGUAL_CLAUSE_SAMPLES;
        let search_hi =
            (start + MAX_BILINGUAL_CLAUSE_SAMPLES).min(len - MIN_BILINGUAL_CLAUSE_SAMPLES);
        if search_lo >= search_hi {
            break;
        }

        let bin_lo = (search_lo / ENVELOPE_BIN_SAMPLES).min(n_bins - 1);
        let bin_hi = (search_hi / ENVELOPE_BIN_SAMPLES).min(n_bins - 1);
        if bin_lo > bin_hi {
            break;
        }

        let interior_min = smooth[bin_lo..=bin_hi]
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);

        // Split ONLY at genuine low-energy pause runs (`interior_min <= 48%` of active
        // speech RMS, i.e. >= ~6.4 dB drop) — never force blind mid-word cuts on
        // continuous monolingual speech that lacks a breath valley.
        let mut best_valley_cut: Option<(usize, f32)> = None;
        if interior_min <= speech_ref * 0.48 {
            let valley_ceiling =
                (interior_min + 0.25 * (speech_ref - interior_min)).min(speech_ref * 0.52);
            let dynamic_span = (speech_ref - interior_min).max(1e-5);
            let mut i = bin_lo;
            while i <= bin_hi {
                if smooth[i] <= valley_ceiling {
                    let run_start = i;
                    let mut min_idx = i;
                    let mut min_val = smooth[i];
                    while i <= bin_hi && smooth[i] <= valley_ceiling {
                        if smooth[i] < min_val {
                            min_val = smooth[i];
                            min_idx = i;
                        }
                        i += 1;
                    }
                    let run_len = (i - run_start).min(8) as f32;
                    let depth_score = (valley_ceiling - min_val) / dynamic_span;
                    let score = depth_score + 0.08 * run_len;
                    let cut_sample = if i - run_start >= 4 {
                        ((run_start + i) * ENVELOPE_BIN_SAMPLES) / 2
                    } else {
                        min_idx * ENVELOPE_BIN_SAMPLES + ENVELOPE_BIN_SAMPLES / 2
                    };
                    let cut_clamped = cut_sample.clamp(search_lo, search_hi);
                    if best_valley_cut.is_none_or(|(_, best_score)| score > best_score) {
                        best_valley_cut = Some((cut_clamped, score));
                    }
                } else {
                    i += 1;
                }
            }
        }

        let Some((cut, _)) = best_valley_cut else {
            break;
        };
        if cut <= start || cut >= len {
            break;
        }
        ranges.push(start..cut);
        start = cut;
    }

    if start < len {
        ranges.push(start..len);
    }
    ranges
}

/// Returns up to three ordered candidate Whisper language codes from the
/// spoken profile and effective Hindustani script, without a model allowlist.
pub fn candidate_languages_for_spoken(
    spoken_languages: &[String],
    script_pref: HindustaniScript,
) -> Vec<String> {
    candidate_languages_for_model(spoken_languages, script_pref, &[])
}

/// Ordered candidate Whisper language codes derived from `spoken_languages` and
/// the effective Hindustani script (capped at 3 candidates to bound worst-case
/// disambiguation latency), restricted to `supported_languages` if non-empty.
pub fn candidate_languages_for_model(
    spoken_languages: &[String],
    script_pref: HindustaniScript,
    supported_languages: &[String],
) -> Vec<String> {
    let effective_script = effective_hindustani_script(script_pref, spoken_languages);
    let mut candidates: Vec<String> = Vec::new();

    for raw in spoken_languages {
        let base = base_lang(raw).to_ascii_lowercase();
        if base.is_empty() {
            continue;
        }
        let target = if matches!(base.as_str(), "ur" | "hi") {
            match effective_script {
                HindustaniScript::UrduPersoArabic => "ur".to_string(),
                HindustaniScript::Devanagari
                | HindustaniScript::RomanHinglish
                | HindustaniScript::Auto => {
                    if base == "ur" && effective_script == HindustaniScript::Auto {
                        "ur".to_string()
                    } else {
                        "hi".to_string()
                    }
                }
            }
        } else {
            base
        };

        let model_supports = supported_languages.is_empty()
            || supported_languages
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&target));
        if model_supports && !candidates.contains(&target) {
            candidates.push(target);
            if candidates.len() >= 3 {
                break;
            }
        }
    }

    candidates
}

/// Returns `true` when `detected_lang` is compatible with the user's configured
/// `spoken_languages`, treating `"hi"` and `"ur"` as acoustic Hindustani twins
/// and accepting absent or automatic detection.
pub fn is_lang_compatible_with_spoken(
    detected_lang: Option<&str>,
    spoken_languages: &[String],
) -> bool {
    if spoken_languages.is_empty() {
        return true;
    }
    let Some(det) = detected_lang.map(base_lang) else {
        return true;
    };
    if det.is_empty() || det.eq_ignore_ascii_case("auto") {
        return true;
    }
    if has_spoken_lang(spoken_languages, det) {
        return true;
    }
    if matches!(det.to_ascii_lowercase().as_str(), "hi" | "ur")
        && (has_spoken_lang(spoken_languages, "hi") || has_spoken_lang(spoken_languages, "ur"))
    {
        return true;
    }
    false
}

fn has_unexpected_script(text: &str, spoken_languages: &[String]) -> bool {
    let allows_arabic = ["ur", "ar", "fa", "ps", "sd", "ks", "ug"]
        .iter()
        .any(|l| has_spoken_lang(spoken_languages, l));
    let allows_devanagari = ["hi", "ur", "mr", "ne", "sa"]
        .iter()
        .any(|l| has_spoken_lang(spoken_languages, l));
    let allows_greek = has_spoken_lang(spoken_languages, "el");
    let allows_cyrillic = ["ru", "uk", "bg", "sr", "be", "kk", "mk", "mn"]
        .iter()
        .any(|l| has_spoken_lang(spoken_languages, l));
    let allows_hangul = has_spoken_lang(spoken_languages, "ko");
    let allows_cjk = ["zh", "yue", "ja", "ko"]
        .iter()
        .any(|l| has_spoken_lang(spoken_languages, l));
    let allows_hebrew = ["he", "iw", "yi"]
        .iter()
        .any(|l| has_spoken_lang(spoken_languages, l));
    let allows_thai = has_spoken_lang(spoken_languages, "th");

    let mut unexpected_letters = 0usize;
    let mut total_letters = 0usize;

    for c in text.chars() {
        let is_script_letter =
            c.is_alphabetic() || is_devanagari_word_char(c) || is_perso_arabic_char(c);
        if !is_script_letter {
            continue;
        }
        total_letters += 1;
        let unexpected = if is_perso_arabic_char(c) {
            !allows_arabic
        } else if is_devanagari_word_char(c) {
            !allows_devanagari
        } else if ('\u{0370}'..='\u{03FF}').contains(&c) {
            !allows_greek
        } else if ('\u{0400}'..='\u{04FF}').contains(&c) {
            !allows_cyrillic
        } else if ('\u{AC00}'..='\u{D7AF}').contains(&c) || ('\u{1100}'..='\u{11FF}').contains(&c) {
            !allows_hangul
        } else if ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3040}'..='\u{30FF}').contains(&c) {
            !allows_cjk
        } else if ('\u{0590}'..='\u{05FF}').contains(&c) {
            !allows_hebrew
        } else if ('\u{0E00}'..='\u{0E7F}').contains(&c) {
            !allows_thai
        } else {
            false
        };
        if unexpected {
            unexpected_letters += 1;
        }
    }

    total_letters > 0 && unexpected_letters * 4 > total_letters
}

fn normalized_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|raw| {
            raw.chars()
                .filter(|c| {
                    c.is_alphanumeric() || is_devanagari_char(*c) || is_perso_arabic_char(*c)
                })
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Detects short-clause Whisper repetition hallucinations such as
/// `"अजय को अजय को"` (exact half-repeat) or `"مرحبا، سو بھاو، مرحبا"`.
pub fn is_repetitive_hallucination(text: &str) -> bool {
    let words = normalized_words(text);
    let n = words.len();
    if n < 3 {
        return false;
    }
    if n.is_multiple_of(2) && words[..n / 2] == words[n / 2..] {
        return true;
    }
    let mut unique: Vec<&str> = Vec::with_capacity(n);
    let mut max_freq = 0usize;
    for w in &words {
        let freq = words.iter().filter(|other| *other == w).count();
        max_freq = max_freq.max(freq);
        if !unique.contains(&w.as_str()) {
            unique.push(w.as_str());
        }
    }
    if n >= 4 && unique.len() * 2 <= n {
        return true;
    }
    n <= 8 && max_freq >= 3
}

/// Computes geometric-mean token confidence in `(0.0, 1.0]` from `transcribe-cpp`
/// token probabilities, ignoring `NaN`/non-positive sentinel entries.
pub fn geometric_mean_token_prob(probs: &[f32]) -> Option<f32> {
    let mut sum_ln = 0.0_f64;
    let mut count = 0usize;
    for &p in probs {
        if p.is_finite() && p > 0.0 && p <= 1.0 {
            sum_ln += f64::from(p.max(1e-4)).ln();
            count += 1;
        }
    }
    if count == 0 {
        None
    } else {
        Some((sum_ln / (count as f64)).exp() as f32)
    }
}

/// Returns `true` when an auto-detected transcript should be disambiguated
/// using its text, detected language, and geometric-mean token confidence.
pub fn needs_spoken_language_disambiguation(
    text: &str,
    detected_lang: Option<&str>,
    token_probs: &[f32],
    spoken_languages: &[String],
) -> bool {
    needs_spoken_language_disambiguation_with_context(
        detected_lang,
        text,
        geometric_mean_token_prob(token_probs),
        spoken_languages,
        "auto",
    )
}

/// Returns `true` when an auto-detected Pass 1 clause requires candidate
/// disambiguation across `spoken_languages` because Whisper predicted an
/// out-of-profile language/script (e.g. Greek `"el"`, Korean `"ko"`, Spanish `"es"`),
/// fell into a repetition loop (e.g. `"अजय को अजय को"`), or had very low acoustic
/// token confidence.
pub fn needs_spoken_language_disambiguation_with_context(
    detected_lang: Option<&str>,
    text: &str,
    mean_token_prob: Option<f32>,
    spoken_languages: &[String],
    explicit_selected_lang: &str,
) -> bool {
    if !explicit_selected_lang.trim().eq_ignore_ascii_case("auto") || spoken_languages.is_empty() {
        return false;
    }
    if text.trim().is_empty() {
        return false;
    }
    if !is_lang_compatible_with_spoken(detected_lang, spoken_languages) {
        return true;
    }
    if has_unexpected_script(text, spoken_languages) {
        return true;
    }
    if is_repetitive_hallucination(text) {
        return true;
    }
    distinct_spoken_language_count(spoken_languages) >= 2
        && mean_token_prob.is_some_and(|p| p < 0.38)
}

/// Scores a candidate transcript for acoustic confidence and lexical diversity.
///
/// Without a candidate language or spoken profile, script alignment and
/// cross-translation penalties are not applied. Empty transcripts score `-10.0`.
pub fn score_candidate_transcript(text: &str, token_probs: &[f32]) -> f32 {
    score_candidate(text, None, token_probs, &[])
}

/// Scores a candidate language decode on `[text, lang, token_probs]` so we pick
/// the acoustically grounded language rather than a forced cross-translation.
pub fn score_candidate_transcript_for_language(
    text: &str,
    lang: &str,
    token_probs: &[f32],
    spoken_languages: &[String],
) -> f32 {
    score_candidate(text, Some(lang), token_probs, spoken_languages)
}

fn score_candidate(
    text: &str,
    lang: Option<&str>,
    token_probs: &[f32],
    spoken_languages: &[String],
) -> f32 {
    let cleaned = strip_hallucination_loops(text);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return -10.0;
    }

    let words = normalized_words(trimmed);
    if words.is_empty() {
        return -10.0;
    }

    // 1. Acoustic token posterior confidence (weight 2.2x when available).
    let acoustic = geometric_mean_token_prob(token_probs).map_or(1.1, |p| 2.2 * p);

    // 2. Script alignment with the candidate language code.
    let mut latin_letters = 0usize;
    let mut arabic_letters = 0usize;
    let mut devanagari_letters = 0usize;
    let mut total_letters = 0usize;
    for c in trimmed.chars() {
        if c.is_ascii_alphabetic() {
            latin_letters += 1;
            total_letters += 1;
        } else if is_perso_arabic_char(c) {
            arabic_letters += 1;
            total_letters += 1;
        } else if is_devanagari_word_char(c) {
            devanagari_letters += 1;
            total_letters += 1;
        } else if c.is_alphabetic() {
            total_letters += 1;
        }
    }

    let script_score = if let Some(lang) = lang {
        candidate_script_score(
            lang,
            latin_letters,
            arabic_letters,
            devanagari_letters,
            total_letters,
        )
    } else {
        0.0
    };

    // 3. Lexical diversity & anti-repetition penalty.
    let mut unique: Vec<&str> = Vec::with_capacity(words.len());
    for w in &words {
        if !unique.contains(&w.as_str()) {
            unique.push(w.as_str());
        }
    }
    let diversity = unique.len() as f32 / words.len() as f32;
    let repetition_term = if is_repetitive_hallucination(trimmed) {
        -1.4
    } else if words.len() >= 2 && unique.len() < words.len() {
        // Penalize even a 2x repeat in a short clause (e.g. "مرحبا، سو بھاو، مرحبا").
        0.35 * diversity - 0.35
    } else {
        0.35 * diversity
    };

    // A forced Urdu decode can translate English greetings into Arabic.
    let cross_translate_penalty = if lang
        .is_some_and(|lang| base_lang(lang).eq_ignore_ascii_case("ur"))
        && !has_spoken_lang(spoken_languages, "ar")
        && trimmed.contains("مرحبا")
    {
        -0.85
    } else {
        0.0
    };

    acoustic + script_score + repetition_term + cross_translate_penalty
}

fn candidate_script_score(
    lang: &str,
    latin_letters: usize,
    arabic_letters: usize,
    devanagari_letters: usize,
    total_letters: usize,
) -> f32 {
    if total_letters == 0 {
        -1.0
    } else {
        let base = base_lang(lang).to_ascii_lowercase();
        match base.as_str() {
            "ur" | "ar" | "fa" | "ps" | "sd" => {
                let ratio = arabic_letters as f32 / total_letters as f32;
                if arabic_letters == 0 {
                    -0.9
                } else {
                    0.55 * ratio
                }
            }
            "hi" | "mr" | "ne" => {
                let ratio = devanagari_letters as f32 / total_letters as f32;
                if devanagari_letters == 0 {
                    -0.9
                } else {
                    0.55 * ratio
                }
            }
            _ => {
                let ratio = latin_letters as f32 / total_letters as f32;
                if ratio < 0.5 {
                    -0.9
                } else {
                    0.55 * ratio
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn langs(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|c| (*c).to_string()).collect()
    }

    #[test]
    fn rescues_spurious_leading_devanagari_he_in_english_for_urdu_user() {
        let spoken = langs(&["en", "ur"]);
        assert_eq!(
            normalize_script_output(" हे, can you hear me out?", &spoken, HindustaniScript::Auto),
            " Hey, can you hear me out?"
        );
        assert!(
            !should_retry_chunk_as_urdu_with_context(
                Some("hi"),
                " हे, can you hear me out?",
                &spoken,
                HindustaniScript::Auto,
                "auto"
            ),
            "predominantly English sentence with leading 'हे' must not force <|ur|> re-decode"
        );
        assert!(
            !should_retry_chunk_as_urdu_with_context(
                Some("hi"),
                "Hey, can you hear me out?",
                &spoken,
                HindustaniScript::Auto,
                "auto"
            ),
            "100% ASCII English tagged Some(\"hi\") by Whisper must never force <|ur|> re-decode"
        );
    }

    #[test]
    fn transliterates_devanagari_hindustani_to_colloquial_roman_hinglish() {
        assert_eq!(
            devanagari_to_roman_hinglish("हेलो आप क्या करते हैं?"),
            "Hello aap kya karte hain?"
        );
        assert_eq!(
            normalize_script_output(
                "हेलो आप क्या करते हैं?",
                &langs(&["en", "ur"]),
                HindustaniScript::RomanHinglish
            ),
            "Hello aap kya karte hain?"
        );
        assert_eq!(
            devanagari_to_roman_hinglish("हाँ, SDK rules UI में कुछ pending नहीं है।"),
            "Haan, SDK rules UI mein kuch pending nahi hai."
        );
    }

    #[test]
    fn preserves_perso_arabic_urdu_and_classical_arabic_verbatim() {
        let arabic_urdu = "صرف میں مضارع مجہول: أذهبتَ إلى المستشفى، يا محمد؟ لم يفعل / لن يفعل";
        assert_eq!(devanagari_to_roman_hinglish(arabic_urdu), arabic_urdu);
        assert_eq!(
            normalize_script_output(
                arabic_urdu,
                &langs(&["en", "ur", "ar"]),
                HindustaniScript::Auto
            ),
            arabic_urdu
        );
    }

    #[test]
    fn builds_initial_prompts_for_urdu_arabic_hinglish_and_glossary_fallback() {
        assert_eq!(
            build_initial_prompt_for_model(
                true,
                true,
                None,
                &langs(&["en", "ur", "ar"]),
                HindustaniScript::Auto,
                &langs(&["Lowyap"]),
            ),
            None,
            "mixed en+ur+ar Pass 1 (lang=None) must stay neutral so English is not translated to Urdu"
        );

        let urdu_ar = build_initial_prompt_for_model(
            true,
            true,
            Some("ur"),
            &langs(&["en", "ur", "ar"]),
            HindustaniScript::Auto,
            &langs(&["Lowyap"]),
        )
        .expect("urdu+ar retry/explicit prompt");
        assert!(urdu_ar.contains("اردو اور عربی"));
        assert!(urdu_ar.contains("لم يَفْعَلْ"));
        assert!(
            !urdu_ar.contains("Lowyap"),
            "models with supports_vocabulary=true receive custom_words via vocabulary slot"
        );

        let pure_perso_arabic = build_initial_prompt_for_model(
            true,
            true,
            None,
            &langs(&["ur", "ar"]),
            HindustaniScript::Auto,
            &[],
        )
        .expect("pure ur+ar spoken_languages can steer on Pass 1");
        assert!(pure_perso_arabic.contains("اردو اور عربی"));

        assert_eq!(
            build_initial_prompt_for_model(
                true,
                true,
                Some("en"),
                &langs(&["en", "ur"]),
                HindustaniScript::Auto,
                &[],
            ),
            None,
            "explicit fixed English must not inject Urdu script"
        );

        let hinglish_with_glossary = build_initial_prompt_for_model(
            true,
            false,
            Some("hi"),
            &langs(&["en", "ur"]),
            HindustaniScript::RomanHinglish,
            &langs(&["Lowyap", "SDK", "GraphQL"]),
        )
        .expect("hinglish prompt with glossary fallback");
        assert!(hinglish_with_glossary.starts_with("Haan bilkul, theek hai."));
        assert!(hinglish_with_glossary.contains("Lowyap, SDK, GraphQL"));
        assert!(hinglish_with_glossary.chars().count() <= MAX_INITIAL_PROMPT_CHARS);

        assert_eq!(
            build_initial_prompt_for_model(
                false,
                false,
                Some("ur"),
                &langs(&["en", "ur"]),
                HindustaniScript::RomanHinglish,
                &langs(&["Lowyap"]),
            ),
            None
        );
    }

    #[test]
    fn retries_chunk_as_urdu_when_whisper_emits_hindi_or_devanagari_for_en_ur_user() {
        let spoken = langs(&["en", "ur"]);
        assert!(should_retry_chunk_as_urdu_with_context(
            Some("hi"),
            "हेलो आप क्या करते हैं?",
            &spoken,
            HindustaniScript::Auto,
            "auto"
        ));
        assert!(should_retry_chunk_as_urdu_with_context(
            Some("en"),
            "हेलो आप क्या करते हैं?",
            &spoken,
            HindustaniScript::Auto,
            "auto"
        ));
        assert!(!should_retry_chunk_as_urdu_with_context(
            Some("hi"),
            "हेलो आप क्या करते हैं?",
            &spoken,
            HindustaniScript::RomanHinglish,
            "auto"
        ));
        assert!(!should_retry_chunk_as_urdu_with_context(
            Some("hi"),
            "हेलो आप क्या करते हैं?",
            &langs(&["en", "hi"]),
            HindustaniScript::Auto,
            "auto"
        ));
        assert!(!should_retry_chunk_as_urdu_with_context(
            Some("hi"),
            "हेलो आप क्या करते हैं?",
            &spoken,
            HindustaniScript::Auto,
            "en"
        ));
    }

    #[test]
    fn collapses_whisper_hallucination_loops_without_touching_normal_repeats() {
        assert_eq!(
            strip_hallucination_loops("Thank you. Thank you. Thank you. Thank you. Thank you."),
            "Thank you."
        );
        assert_eq!(
            strip_hallucination_loops("Thank you, thank you, thank you, thank you, thank you."),
            "Thank you."
        );
        assert_eq!(
            strip_hallucination_loops("Thank you, thank you!"),
            "Thank you, thank you!"
        );
        assert_eq!(
            strip_hallucination_loops("Subtitles by the Amara.org community."),
            ""
        );
        assert_eq!(
            strip_hallucination_loops("Let's merge the PR. Subtitles by the Amara.org community."),
            "Let's merge the PR."
        );
    }

    #[test]
    fn reads_hindustani_script_from_flow_settings_json_leniently() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
        for (file, expected) in [
            ("missing.json", HindustaniScript::Auto),
            ("roman_hinglish.json", HindustaniScript::RomanHinglish),
            ("missing_script.json", HindustaniScript::Auto),
            ("invalid_script.json", HindustaniScript::Auto),
        ] {
            assert_eq!(read_hindustani_script_from_path(&dir.join(file)), expected);
        }
    }

    #[test]
    fn splits_bilingual_clauses_at_energy_valley_only_in_multilingual_auto_mode() {
        // Construct a 6.6s 16 kHz signal: 2.6s Clause 1 (0.18 amp) + 0.35s valley (0.015 amp)
        // + 3.65s Clause 2 (0.18 amp).
        let mut pcm = Vec::with_capacity(16_000 * 7);
        pcm.extend(std::iter::repeat_n(0.18_f32, 41_600)); // 0.00s..2.60s
        pcm.extend(std::iter::repeat_n(0.015_f32, 5_600)); // 2.60s..2.95s valley
        pcm.extend(std::iter::repeat_n(0.18_f32, 58_400)); // 2.95s..6.60s

        let ranges = split_bilingual_clauses(&pcm, &langs(&["en", "ur"]), "auto");
        assert_eq!(ranges.len(), 2, "expected 2 clauses across energy valley");
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].end, ranges[1].start);
        assert_eq!(ranges[1].end, pcm.len());
        let split_secs = ranges[0].end as f32 / 16_000.0;
        assert!(
            (2.55..=3.00).contains(&split_secs),
            "split should land inside the 2.60s..2.95s valley, got {split_secs:.2}s"
        );

        // Monolingual spoken_languages or fixed selected_language must never split.
        assert_eq!(
            split_bilingual_clauses(&pcm, &langs(&["en"]), "auto"),
            vec![0..pcm.len()]
        );
        assert_eq!(
            split_bilingual_clauses(&pcm, &langs(&["en", "ur"]), "en"),
            vec![0..pcm.len()]
        );
        // Continuous speech without a genuine pause valley or long (> 20 s) monologues
        // must never be sliced into blind mid-word fragments.
        let continuous_9s = vec![0.18_f32; 9 * 16_000];
        assert_eq!(
            split_bilingual_clauses(&continuous_9s, &langs(&["en", "ur"]), "auto"),
            vec![0..continuous_9s.len()]
        );
        let long_monologue_30s = vec![0.18_f32; 30 * 16_000];
        assert_eq!(
            split_bilingual_clauses(&long_monologue_30s, &langs(&["en", "ur"]), "auto"),
            vec![0..long_monologue_30s.len()]
        );
    }

    #[test]
    fn detects_out_of_profile_or_repetitive_hallucinations_for_disambiguation() {
        let spoken = langs(&["en", "ur"]);
        // Out-of-profile Greek / Korean hallucinations must trigger disambiguation.
        assert!(needs_spoken_language_disambiguation_with_context(
            Some("el"),
            "Τώρα με 30% έκπτωση.",
            Some(0.72),
            &spoken,
            "auto"
        ));
        assert!(needs_spoken_language_disambiguation_with_context(
            Some("ko"),
            "감사합니다",
            Some(0.80),
            &spoken,
            "auto"
        ));
        // Repetitive Hindi hallucination ("अजय को अजय को") must trigger disambiguation
        // rather than blindly forcing `<|ur|>`.
        assert!(needs_spoken_language_disambiguation_with_context(
            Some("hi"),
            "अजय को अजय को",
            Some(0.65),
            &spoken,
            "auto"
        ));
        // Clean Devanagari Hindustani ("क्या तुम्हें मेरी बात समझ आ रही है?") is compatible
        // with spoken `"ur"` and non-repetitive, so it uses the fast single `<|ur|>` retry.
        assert!(!needs_spoken_language_disambiguation_with_context(
            Some("hi"),
            "क्या तुम्हें मेरी बात समझ आ रही है?",
            Some(0.78),
            &spoken,
            "auto"
        ));
        // Confident English passes through untouched in 1 pass.
        assert!(!needs_spoken_language_disambiguation_with_context(
            Some("en"),
            "How are you doing?",
            Some(0.85),
            &spoken,
            "auto"
        ));
    }

    #[test]
    fn scores_candidate_transcripts_preferring_confident_diverse_in_script_output() {
        let spoken = langs(&["en", "ur"]);
        let en_good = score_candidate_transcript_for_language(
            "Hey Zoom, how are you?",
            "en",
            &[0.82, 0.79, 0.88, 0.91, 0.86],
            &spoken,
        );
        let ur_cross_translated = score_candidate_transcript_for_language(
            "مرحبا، سو بھاو، مرحبا",
            "ur",
            &[0.34, 0.28, 0.31, 0.35],
            &spoken,
        );
        assert!(
            en_good > ur_cross_translated + 1.0,
            "genuine English ({en_good:.2}) must decisively beat cross-translated repetitive Urdu ({ur_cross_translated:.2})"
        );
    }

    #[test]
    fn llm_script_instruction_forbids_cross_clause_translation_in_bilingual_modes() {
        let auto_inst =
            llm_script_instruction(HindustaniScript::Auto, &langs(&["en", "ur"])).expect("auto");
        assert!(auto_inst.contains("Never translate between languages across clauses"));
        assert!(auto_inst.contains("keep every English sentence"));

        let hi_inst = llm_script_instruction(HindustaniScript::Auto, &langs(&["en", "hi"]))
            .expect("bilingual en+hi");
        assert!(hi_inst.contains("Never translate between languages across clauses"));
    }

    #[test]
    fn prompt_uses_optional_base_and_keeps_mixed_auto_language_neutral() {
        let spoken = langs(&["en", "ur"]);
        assert_eq!(
            build_initial_prompt(Some("Lowyap"), None, HindustaniScript::Auto, &spoken),
            Some("Lowyap".to_owned())
        );
        let prompt = build_initial_prompt(
            Some(&"é".repeat(320)),
            Some("ur-PK"),
            HindustaniScript::Auto,
            &spoken,
        )
        .expect("script steering plus base prompt");
        assert!(prompt.starts_with("اردو"));
        assert!(prompt.ends_with('é'));
        assert_eq!(prompt.chars().count(), 320);
    }

    #[test]
    fn translated_and_english_chunks_do_not_retry_as_urdu() {
        let spoken = langs(&["en", "ur"]);
        for (text, translated, expected) in [
            ("क्या आप आ रहे हैं?", true, false),
            ("क्या आप आ रहे हैं?", false, true),
            ("हे, can you hear me out?", false, false),
        ] {
            assert_eq!(
                should_retry_chunk_as_urdu(text, &spoken, HindustaniScript::Auto, translated),
                expected
            );
        }
    }

    #[test]
    fn ranks_and_disambiguates_without_assuming_missing_language_metadata() {
        let spoken = langs(&["EN-US", "ur-PK", "hi", "ar", "el"]);
        assert_eq!(
            candidate_languages_for_spoken(&spoken, HindustaniScript::UrduPersoArabic),
            ["en", "ur", "ar"]
        );
        assert_eq!(
            candidate_languages_for_model(
                &spoken,
                HindustaniScript::UrduPersoArabic,
                &langs(&["en", "el"])
            ),
            ["en", "el"]
        );
        assert!(is_lang_compatible_with_spoken(None, &spoken));
        assert!(is_lang_compatible_with_spoken(
            Some("hi-IN"),
            &langs(&["ur"])
        ));
        assert!(contains_perso_arabic("Hello دنیا"));
        assert!(needs_spoken_language_disambiguation(
            "Hello world",
            None,
            &[0.16, 0.81],
            &spoken
        ));
        assert!(!needs_spoken_language_disambiguation(
            "Hello world",
            None,
            &[f32::NAN],
            &spoken
        ));
        assert!(
            score_candidate_transcript("The meeting starts soon", &[0.9])
                > score_candidate_transcript("starts soon starts soon", &[0.9])
        );
    }
}
