//! Deterministic conversational name-cue resolver (`SV-89`).
//!
//! Resolves anonymous speaker clusters (such as `"speaker_1"`) to human names
//! (`"Speaker 1 (Rahul)"`) ONLY from high-precision self-introductions on
//! `Others` or direct vocative handoffs from `Me`.

use std::collections::BTreeMap;

use lowyap_ports::SpeakerChannel;

/// Maximum elapsed gap in milliseconds between the end of `Me`'s vocative turn
/// and the start of the responding `Others` turn.
pub const VOCATIVE_HANDOFF_WINDOW_MS: u64 = 8_000;

#[derive(Clone, Debug)]
struct PendingVocative {
    candidate_name: String,
    me_start_ms: u64,
    me_end_ms: u64,
}

/// Resolves speaker cluster IDs to conversational names using deterministic
/// self-introduction and vocative-handoff cues.
#[derive(Clone, Debug, Default)]
pub struct NameCueResolver {
    bindings: BTreeMap<String, String>,
    pending_vocative: Option<PendingVocative>,
}

impl NameCueResolver {
    /// Creates an empty [`NameCueResolver`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears all learned speaker-to-name bindings and pending vocative handoffs.
    pub fn reset(&mut self) {
        self.bindings.clear();
        self.pending_vocative = None;
    }

    /// Returns the bound name for `speaker_id`, if one has been resolved.
    pub fn bound_name(&self, speaker_id: &str) -> Option<&str> {
        self.bindings.get(speaker_id).map(String::as_str)
    }

    /// Formats the user-facing display label for `speaker_id`.
    ///
    /// Returns `"Me"` when `speaker_id == "me"`, `"{default_label} ({Name})"`
    /// when `speaker_id` is bound to `Name`, or `default_label` unchanged otherwise.
    pub fn display_label_for(&self, speaker_id: &str, default_label: &str) -> String {
        if speaker_id == "me" {
            return "Me".to_owned();
        }
        if let Some(name) = self.bindings.get(speaker_id) {
            let suffix = format!("({name})");
            if default_label.ends_with(&suffix) {
                default_label.to_owned()
            } else {
                format!("{default_label} ({name})")
            }
        } else {
            default_label.to_owned()
        }
    }

    /// Observes a transcribed turn and returns `Some(name)` when `speaker_id`
    /// is newly or consistently bound to a validated person name.
    pub fn observe_transcript_turn(
        &mut self,
        channel: SpeakerChannel,
        speaker_id: &str,
        start_ms: u64,
        end_ms: u64,
        text: &str,
    ) -> Option<String> {
        let normalized = normalize_apostrophes(text);

        let is_me_turn = speaker_id.eq_ignore_ascii_case("me")
            || (channel == SpeakerChannel::Me && speaker_id.is_empty());

        if is_me_turn {
            if let Some(candidate) = extract_me_vocative(&normalized) {
                self.pending_vocative = Some(PendingVocative {
                    candidate_name: candidate,
                    me_start_ms: start_ms,
                    me_end_ms: end_ms.max(start_ms),
                });
            } else if let Some(pending) = &self.pending_vocative {
                if start_ms > pending.me_end_ms.saturating_add(VOCATIVE_HANDOFF_WINDOW_MS) {
                    self.pending_vocative = None;
                }
            }
            return None;
        }

        // Consume any pending vocative handoff on the first subsequent non-Me turn.
        let pending = self.pending_vocative.take();

        // 1. Direct self-introduction on `Others` takes priority over speculative handoff.
        if let Some(intro_name) = extract_others_self_intro(&normalized) {
            return self.try_bind(speaker_id, &intro_name);
        }

        // 2. Direct vocative handoff from `Me` within <= 8_000 ms after `Me` ended.
        if let Some(handoff) = pending {
            let within_window = start_ms >= handoff.me_start_ms
                && start_ms <= handoff.me_end_ms.saturating_add(VOCATIVE_HANDOFF_WINDOW_MS);
            if within_window && !self.is_name_bound_to_other(speaker_id, &handoff.candidate_name) {
                return self.try_bind(speaker_id, &handoff.candidate_name);
            }
        }

        None
    }

    fn is_name_bound_to_other(&self, speaker_id: &str, candidate: &str) -> bool {
        self.bindings
            .iter()
            .any(|(id, existing)| id != speaker_id && existing.eq_ignore_ascii_case(candidate))
    }

    fn try_bind(&mut self, speaker_id: &str, candidate: &str) -> Option<String> {
        if speaker_id.is_empty() || speaker_id == "me" {
            return None;
        }
        if let Some(existing) = self.bindings.get(speaker_id) {
            if existing == candidate {
                return Some(existing.clone());
            }
            // Do not overwrite an already established non-matching speaker binding.
            return None;
        }
        let owned = candidate.to_owned();
        self.bindings.insert(speaker_id.to_owned(), owned.clone());
        Some(owned)
    }
}

fn normalize_apostrophes(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '’' | '‘' | '`' => '\'',
            _ => ch,
        })
        .collect()
}

fn extract_others_self_intro(text: &str) -> Option<String> {
    for clause in split_clauses(text) {
        let tokens: Vec<&str> = clause.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }

        for i in 0..tokens.len() {
            let w0 = clean_word_lower(tokens[i]);

            // Greeting + optional collective address + "I'm <Name>" / "I am <Name>":
            // e.g. "Hi, I'm Rahul", "Hi everyone, I'm Rahul", "Hello, I am Priya"
            if matches!(w0.as_str(), "hi" | "hello" | "hey") {
                for offset in [1usize, 2usize] {
                    if offset == 2 {
                        let Some(&mid) = tokens.get(i + 1) else {
                            break;
                        };
                        if !matches!(
                            clean_word_lower(mid).as_str(),
                            "everyone" | "everybody" | "team" | "folks" | "all" | "there" | "guys"
                        ) {
                            break;
                        }
                    }
                    let Some(&t1) = tokens.get(i + offset) else {
                        continue;
                    };
                    let w1 = clean_word_lower(t1);
                    if w1 == "i'm" {
                        if let Some(&raw_name) = tokens.get(i + offset + 1) {
                            if valid_post_intro_boundary(
                                raw_name,
                                tokens.get(i + offset + 2).copied(),
                            ) {
                                if let Some(name) = validate_candidate_name(raw_name) {
                                    return Some(name);
                                }
                            }
                        }
                    } else if w1 == "i"
                        && tokens
                            .get(i + offset + 1)
                            .is_some_and(|&t2| clean_word_lower(t2) == "am")
                    {
                        if let Some(&raw_name) = tokens.get(i + offset + 2) {
                            if valid_post_intro_boundary(
                                raw_name,
                                tokens.get(i + offset + 3).copied(),
                            ) {
                                if let Some(name) = validate_candidate_name(raw_name) {
                                    return Some(name);
                                }
                            }
                        }
                    }
                }
            }

            // "This is <Name> speaking" / "This is <Name> from ..."
            if w0 == "this"
                && tokens
                    .get(i + 1)
                    .is_some_and(|&t1| clean_word_lower(t1) == "is")
            {
                if let (Some(&raw_name), Some(&next_tok)) = (tokens.get(i + 2), tokens.get(i + 3)) {
                    let next_lower = clean_word_lower(next_tok);
                    if !has_sentence_stop(raw_name)
                        && matches!(next_lower.as_str(), "speaking" | "from")
                    {
                        if let Some(name) = validate_candidate_name(raw_name) {
                            return Some(name);
                        }
                    }
                }
            }

            // "My name is <Name>" / "My name's <Name>"
            if w0 == "my" {
                if let Some(&t1) = tokens.get(i + 1) {
                    let w1 = clean_word_lower(t1);
                    if w1 == "name"
                        && tokens
                            .get(i + 2)
                            .is_some_and(|&t2| clean_word_lower(t2) == "is")
                    {
                        if let Some(&raw_name) = tokens.get(i + 3) {
                            if let Some(name) = validate_candidate_name(raw_name) {
                                return Some(name);
                            }
                        }
                    } else if w1 == "name's" {
                        if let Some(&raw_name) = tokens.get(i + 2) {
                            if let Some(name) = validate_candidate_name(raw_name) {
                                return Some(name);
                            }
                        }
                    }
                }
            }

            // Roman Hindi/Urdu: "Mera naam <Name> hai"
            if w0 == "mera"
                && tokens
                    .get(i + 1)
                    .is_some_and(|&t1| clean_word_lower(t1) == "naam")
            {
                if let (Some(&raw_name), Some(&next_tok)) = (tokens.get(i + 2), tokens.get(i + 3)) {
                    if !has_sentence_stop(raw_name) && clean_word_lower(next_tok) == "hai" {
                        if let Some(name) = validate_candidate_name(raw_name) {
                            return Some(name);
                        }
                    }
                }
            }
        }
    }
    None
}

fn valid_post_intro_boundary(raw_name: &str, next_tok: Option<&str>) -> bool {
    let Some(next) = next_tok else {
        return true;
    };
    if raw_name.ends_with([',', '.', '!', '?', ';', ':']) {
        return true;
    }
    matches!(
        clean_word_lower(next).as_str(),
        "and" | "from" | "here" | "speaking" | "calling" | "joining" | "on" | "with"
    )
}

fn extract_me_vocative(text: &str) -> Option<String> {
    for clause in split_clauses(text) {
        let tokens: Vec<&str> = clause.split_whitespace().collect();
        if tokens.len() < 2 {
            continue;
        }

        let first_lower = clean_word_lower(tokens[0]);

        // 1. "Hey <Name>, ..." / "Hi <Name>, ..." / "Hello <Name>, ..."
        if matches!(first_lower.as_str(), "hey" | "hi" | "hello") && tokens.len() >= 3 {
            let raw_name = tokens[1];
            let has_comma = raw_name.ends_with(',');
            let followed_by_prompt = is_vocative_continuation(&tokens[2..]);
            if has_comma || followed_by_prompt {
                if let Some(name) = validate_candidate_name(raw_name) {
                    return Some(name);
                }
            }
        }

        // 2. "<Name>, can you / could you / what do you ..."
        if tokens[0].ends_with(',') && is_vocative_continuation(&tokens[1..]) {
            if let Some(name) = validate_candidate_name(tokens[0]) {
                return Some(name);
            }
        }
    }
    None
}

fn is_vocative_continuation(rest: &[&str]) -> bool {
    let Some(&first) = rest.first() else {
        return false;
    };
    let w0 = clean_word_lower(first);
    let w1 = rest
        .get(1)
        .map(|&s| clean_word_lower(s))
        .unwrap_or_default();
    let w2 = rest
        .get(2)
        .map(|&s| clean_word_lower(s))
        .unwrap_or_default();

    matches!(
        (w0.as_str(), w1.as_str()),
        ("can", "you")
            | ("could", "you")
            | ("would", "you")
            | ("will", "you")
            | ("do", "you")
            | ("did", "you")
            | ("are", "you")
            | ("have", "you")
            | ("should", "we")
            | ("any", "thoughts")
            | ("your", "thoughts")
    ) || matches!(
        (w0.as_str(), w1.as_str(), w2.as_str()),
        ("what", "do", "you")
            | ("what", "are", "your")
            | ("what", "are", "you")
            | ("how", "do", "you")
            | ("where", "do", "you")
    )
}

fn split_clauses(text: &str) -> Vec<&str> {
    text.split(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn has_sentence_stop(token: &str) -> bool {
    token.ends_with(['.', '!', '?', ';', ':'])
}

fn clean_word_lower(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '\'')
        .to_ascii_lowercase()
}

/// Validates that `raw` (stripped of surrounding punctuation) is a single
/// `[A-Z][a-z]{1,19}` ASCII alphabetic token (`2..=20` chars) not in the blocklist.
pub fn validate_candidate_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|c: char| {
        matches!(
            c,
            ',' | '.' | '!' | '?' | ';' | ':' | '"' | '\'' | '(' | ')' | '-'
        )
    });

    if !(2..=20).contains(&trimmed.len()) {
        return None;
    }

    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_lowercase()) {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if is_blocklisted_name(&lower) {
        return None;
    }

    Some(trimmed.to_owned())
}

fn is_blocklisted_name(lower: &str) -> bool {
    matches!(
        lower,
        // Collective / indefinite pronouns & group nouns
        "everyone"
            | "everybody"
            | "someone"
            | "somebody"
            | "anyone"
            | "anybody"
            | "team"
            | "guys"
            | "folks"
            | "people"
            | "all"
            | "both"
            | "friends"
            // Deictics, discourse markers & conversational fillers
            | "there"
            | "here"
            | "sure"
            | "yeah"
            | "yep"
            | "nope"
            | "yes"
            | "well"
            | "okay"
            | "alright"
            | "right"
            | "thanks"
            | "thank"
            | "please"
            | "sorry"
            | "hello"
            | "hey"
            // States, participles & common adjectives after "I'm"
            | "ready"
            | "going"
            | "doing"
            | "trying"
            | "looking"
            | "working"
            | "speaking"
            | "calling"
            | "joining"
            | "sharing"
            | "presenting"
            | "listening"
            | "waiting"
            | "thinking"
            | "testing"
            | "checking"
            | "talking"
            | "having"
            | "making"
            | "taking"
            | "getting"
            | "coming"
            | "running"
            | "starting"
            | "good"
            | "great"
            | "fine"
            | "glad"
            | "happy"
            | "back"
            | "late"
            | "early"
            | "muted"
            | "online"
            | "done"
            | "free"
            | "busy"
            // Honorifics & informal address (English + Hindi/Urdu)
            | "bhai"
            | "yaar"
            | "bro"
            | "sir"
            | "madam"
            | "maam"
            | "boss"
            | "man"
            | "buddy"
            | "mate"
            | "dude"
            | "didi"
            // Days of the week
            | "monday"
            | "tuesday"
            | "wednesday"
            | "thursday"
            | "friday"
            | "saturday"
            | "sunday"
            // Months of the year
            | "january"
            | "february"
            | "march"
            | "april"
            | "may"
            | "june"
            | "july"
            | "august"
            | "september"
            | "october"
            | "november"
            | "december"
            // Temporal nouns
            | "today"
            | "tomorrow"
            | "yesterday"
            | "tonight"
            | "morning"
            | "afternoon"
            | "evening"
            // Collaboration tools & brands
            | "google"
            | "zoom"
            | "slack"
            | "teams"
            | "meet"
            | "webex"
            | "discord"
            | "github"
            | "jira"
            | "linear"
            | "notion"
            | "figma"
    )
}
