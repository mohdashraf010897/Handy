//! Deterministic conversational name-cue resolver (`SV-89`).
//!
//! Resolves anonymous speaker clusters (such as `"speaker_1"`) to human names
//! (`"Speaker 1 (Rahul)"`) ONLY from high-precision self-introductions on
//! `Others` or direct vocative handoffs from `Me`.

use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct RosterAttendee {
    display_name: String,
    first_name: String,
    first_lower: String,
    phonetic_key: String,
}

/// Resolves speaker cluster IDs to conversational names using deterministic
/// self-introduction, vocative-handoff cues, and optional local calendar
/// attendee rosters (`SV-89`, `SV-94`).
#[derive(Clone, Debug, Default)]
pub struct NameCueResolver {
    bindings: BTreeMap<String, String>,
    pending_vocative: Option<PendingVocative>,
    roster: Vec<RosterAttendee>,
    remote_speakers_seen: BTreeSet<String>,
}

impl NameCueResolver {
    /// Creates an empty [`NameCueResolver`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a [`NameCueResolver`] seeded with expected non-self attendee
    /// names from a local calendar invite (`SV-94`).
    pub fn with_expected_attendees<I, S>(attendees: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut resolver = Self::new();
        resolver.set_expected_attendees(attendees);
        resolver
    }

    /// Replaces the expected attendee roster (preserving learned bindings).
    pub fn set_expected_attendees<I, S>(&mut self, attendees: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.roster.clear();
        let mut seen = BTreeSet::new();
        for raw in attendees {
            if let Some(entry) = parse_roster_attendee(raw.as_ref()) {
                let key = entry.display_name.to_ascii_lowercase();
                if seen.insert(key) {
                    self.roster.push(entry);
                }
            }
        }
    }

    /// Clears all learned speaker-to-name bindings and pending vocative handoffs.
    pub fn reset(&mut self) {
        self.bindings.clear();
        self.pending_vocative = None;
        self.remote_speakers_seen.clear();
    }

    /// Returns the explicitly bound name for `speaker_id`, if one has been resolved.
    pub fn bound_name(&self, speaker_id: &str) -> Option<&str> {
        self.bindings.get(speaker_id).map(String::as_str)
    }

    /// When a 1:1 calendar invite has a single remote attendee and at most one
    /// remote speaker cluster has spoken, returns that attendee's display name.
    pub fn sole_attendee_hint_for(&self, speaker_id: &str) -> Option<&str> {
        if speaker_id.is_empty() || speaker_id == "me" || self.roster.len() != 1 {
            return None;
        }
        if !self.bindings.is_empty() {
            return None;
        }
        let only_speaker_so_far = self.remote_speakers_seen.is_empty()
            || (self.remote_speakers_seen.len() == 1
                && self.remote_speakers_seen.contains(speaker_id));
        if only_speaker_so_far {
            Some(self.roster[0].display_name.as_str())
        } else {
            None
        }
    }

    /// Formats the user-facing display label for `speaker_id`.
    ///
    /// Returns `"Me"` when `speaker_id == "me"`, `"{default_label} ({Name})"`
    /// when `speaker_id` is bound to `Name` (or uniquely hinted in a 1:1 call),
    /// or `default_label` unchanged otherwise.
    pub fn display_label_for(&self, speaker_id: &str, default_label: &str) -> String {
        if speaker_id == "me" {
            return "Me".to_owned();
        }
        let resolved = self
            .bindings
            .get(speaker_id)
            .map(String::as_str)
            .or_else(|| self.sole_attendee_hint_for(speaker_id));
        if let Some(name) = resolved {
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
            let vocative = extract_me_vocative(&normalized)
                .map(|c| self.snap_to_roster(&c))
                .or_else(|| extract_roster_vocative(&normalized, &self.roster));
            if let Some(candidate) = vocative {
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

        if !speaker_id.is_empty() && speaker_id != "me" {
            self.remote_speakers_seen.insert(speaker_id.to_owned());
        }

        // Consume any pending vocative handoff on the first subsequent non-Me turn.
        let pending = self.pending_vocative.take();

        // 1. Direct self-introduction on `Others` takes priority over speculative handoff.
        if let Some(intro_name) = extract_others_self_intro(&normalized) {
            let snapped = self.snap_to_roster(&intro_name);
            return self.try_bind(speaker_id, &snapped);
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

    /// Returns the number of distinct remote speaker IDs observed so far.
    pub fn remote_speaker_count(&self) -> usize {
        self.remote_speakers_seen.len()
    }

    /// Snaps a spoken first name to an expected calendar attendee when an
    /// unambiguous exact or phonetic/Levenshtein match exists.
    pub fn snap_to_roster(&self, candidate: &str) -> String {
        snap_name_against_roster(candidate, &self.roster).unwrap_or_else(|| candidate.to_owned())
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

fn parse_roster_attendee(raw: &str) -> Option<RosterAttendee> {
    let cleaned = raw
        .split('<')
        .next()
        .unwrap_or(raw)
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '(' | ')' | ','))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.len() < 2 || cleaned.len() > 48 || cleaned.contains('@') {
        return None;
    }
    let first_token = cleaned.split_whitespace().next()?;
    let first_alpha: String = first_token
        .chars()
        .filter(|c| c.is_alphabetic() || *c == '-')
        .collect();
    if first_alpha.chars().count() < 2 {
        return None;
    }
    let first_lower = first_alpha.to_lowercase();
    if is_blocklisted_name(&first_lower) {
        return None;
    }
    Some(RosterAttendee {
        display_name: cleaned,
        first_name: first_alpha,
        phonetic_key: phonetic_skeleton(&first_lower),
        first_lower,
    })
}

fn phonetic_skeleton(lower: &str) -> String {
    // Normalize common cross-accent vowel/consonant transliteration variants
    // ("rahool" -> "rahul", "preeya" -> "priya", "sara" -> "sarah", "mohammed" -> "mohamad").
    let mut s = lower
        .replace("oo", "u")
        .replace("ee", "i")
        .replace("ph", "f")
        .replace("kh", "k")
        .replace("gh", "g");
    if s.len() >= 4 && s.ends_with('h') && !s.ends_with("sh") && !s.ends_with("ch") {
        s.pop();
    }
    let mut dedup = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for ch in s.chars() {
        if prev != Some(ch) {
            dedup.push(ch);
            prev = Some(ch);
        }
    }
    dedup
}

fn ascii_edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

fn snap_name_against_roster(candidate: &str, roster: &[RosterAttendee]) -> Option<String> {
    if roster.is_empty() {
        return None;
    }
    let trimmed = candidate
        .trim_matches(|c: char| !c.is_alphabetic() && c != '-')
        .trim();
    if trimmed.chars().count() < 2 {
        return None;
    }
    let cand_lower = trimmed.to_lowercase();
    if is_blocklisted_name(&cand_lower) {
        return None;
    }

    // 1. Exact first-name or full-name match.
    let exact: Vec<&RosterAttendee> = roster
        .iter()
        .filter(|r| r.first_lower == cand_lower || r.display_name.eq_ignore_ascii_case(trimmed))
        .collect();
    if exact.len() == 1 {
        return Some(exact[0].display_name.clone());
    }
    if exact.len() > 1 {
        return Some(exact[0].first_name.clone());
    }

    // 2. Phonetic skeleton or tight edit-distance match (same leading letter).
    let cand_skel = phonetic_skeleton(&cand_lower);
    let cand_first = cand_lower.chars().next()?;
    let cand_len = cand_lower.chars().count();

    let fuzzy: Vec<&RosterAttendee> = roster
        .iter()
        .filter(|r| {
            let Some(r_first) = r.first_lower.chars().next() else {
                return false;
            };
            if r_first != cand_first {
                return false;
            }
            if r.phonetic_key == cand_skel && cand_skel.len() >= 3 {
                return true;
            }
            let max_dist = if cand_len >= 7 && r.first_lower.chars().count() >= 7 {
                2
            } else if cand_len >= 4 && r.first_lower.chars().count() >= 4 {
                1
            } else {
                0
            };
            max_dist > 0 && ascii_edit_distance(&cand_lower, &r.first_lower) <= max_dist
        })
        .collect();

    if fuzzy.len() == 1 {
        Some(fuzzy[0].display_name.clone())
    } else if fuzzy.len() > 1 {
        // Only collapse if all fuzzy matches share the exact same canonical first name.
        let canonical = &fuzzy[0].first_lower;
        if fuzzy.iter().all(|r| &r.first_lower == canonical) {
            Some(fuzzy[0].first_name.clone())
        } else {
            None
        }
    } else {
        None
    }
}

fn extract_roster_vocative(text: &str, roster: &[RosterAttendee]) -> Option<String> {
    if roster.is_empty() {
        return None;
    }
    let clauses: Vec<&str> = text
        .split(['.', '!', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    for clause in clauses {
        let tokens: Vec<&str> = clause.split_whitespace().collect();
        if tokens.len() < 2 {
            continue;
        }
        // Check direct handoff phrases: "thanks <Name>", "go ahead <Name>", "over to you <Name>"
        for i in 0..tokens.len() {
            let w0 = clean_word_lower(tokens[i]);
            let w1 = tokens
                .get(i + 1)
                .map(|&t| clean_word_lower(t))
                .unwrap_or_default();
            let w2 = tokens
                .get(i + 2)
                .map(|&t| clean_word_lower(t))
                .unwrap_or_default();

            if w0 == "thanks" && tokens.get(i + 1).is_some() {
                if let Some(matched) = snap_name_against_roster(tokens[i + 1], roster) {
                    return Some(matched);
                }
            }
            if (w0 == "thank" && w1 == "you") || (w0 == "go" && w1 == "ahead") {
                if let Some(&raw) = tokens.get(i + 2) {
                    if let Some(matched) = snap_name_against_roster(raw, roster) {
                        return Some(matched);
                    }
                }
            }
            if w0 == "over" && w1 == "to" && w2 == "you" {
                if let Some(&raw) = tokens.get(i + 3) {
                    if let Some(matched) = snap_name_against_roster(raw, roster) {
                        return Some(matched);
                    }
                }
            }
        }

        // Check trailing vocative address at end of question/clause:
        // e.g. "What do you think, Priya?" or "Does Friday work for you, Rahul?"
        if tokens.len() >= 3 {
            let prev = tokens[tokens.len() - 2];
            let last = tokens[tokens.len() - 1];
            if prev.ends_with(',') || clause.ends_with('?') {
                if let Some(matched) = snap_name_against_roster(last, roster) {
                    return Some(matched);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod calendar_roster_tests {
    use super::*;

    #[test]
    fn one_on_one_calendar_roster_attributes_single_remote_speaker_and_backs_off_if_second_joins() {
        let mut resolver = NameCueResolver::with_expected_attendees(["Sarah Chen"]);

        // First utterance in a 1:1 call resolves immediately to Sarah Chen without needing "Hi I'm Sarah"
        resolver.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            1_000,
            4_000,
            "Yeah the Q4 roadmap looks solid on our side.",
        );
        assert_eq!(
            resolver.display_label_for("speaker_1", "Speaker 1"),
            "Speaker 1 (Sarah Chen)"
        );

        // If an unexpected second remote speaker joins, the speculative 1:1 hint steps aside cleanly
        resolver.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_2",
            5_000,
            7_500,
            "Sorry for joining late folks.",
        );
        assert_eq!(
            resolver.display_label_for("speaker_1", "Speaker 1"),
            "Speaker 1"
        );
        assert_eq!(
            resolver.display_label_for("speaker_2", "Speaker 2"),
            "Speaker 2"
        );
    }

    #[test]
    fn snaps_phonetic_asr_variants_and_handoffs_to_calendar_attendees() {
        let mut resolver = NameCueResolver::with_expected_attendees(["Rahul Verma", "Priya Nair"]);

        // Phonetic ASR variant "Rahool" snaps to exact invitee "Rahul Verma"
        let bound = resolver.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            1_000,
            3_500,
            "Hi everyone, I'm Rahool from the mobile team.",
        );
        assert_eq!(bound.as_deref(), Some("Rahul Verma"));
        assert_eq!(
            resolver.display_label_for("speaker_1", "Speaker 1"),
            "Speaker 1 (Rahul Verma)"
        );

        // Roster-backed trailing question vocative ("What do you think, Preeya?")
        resolver.observe_transcript_turn(
            SpeakerChannel::Me,
            "me",
            4_000,
            6_000,
            "Makes sense. What do you think, Preeya?",
        );
        let bound_2 = resolver.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_2",
            6_500,
            9_000,
            "I agree with the rollout timeline.",
        );
        assert_eq!(bound_2.as_deref(), Some("Priya Nair"));
        assert_eq!(
            resolver.display_label_for("speaker_2", "Speaker 2"),
            "Speaker 2 (Priya Nair)"
        );
    }

    #[test]
    fn ambiguous_duplicate_first_names_keep_first_name_without_guessing_surname() {
        let mut resolver = NameCueResolver::with_expected_attendees(["Sarah Chen", "Sarah Lee"]);

        resolver.observe_transcript_turn(
            SpeakerChannel::Me,
            "me",
            1_000,
            2_500,
            "Hey Sara, can you walk us through the metrics?",
        );
        let bound = resolver.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            3_000,
            5_500,
            "Sure, pulling up the dashboard now.",
        );
        assert_eq!(bound.as_deref(), Some("Sarah"));
    }
}
