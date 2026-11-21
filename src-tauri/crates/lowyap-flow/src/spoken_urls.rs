//! Deterministic sub-millisecond formatter for spoken URLs, domains, ports, and paths.
//!
//! Converts spoken domain/protocol/path constructs (e.g. `"lowyap dot com"`,
//! `"https colon slash slash github dot com"`, `"github.com slash lowyap dash desktop"`)
//! while strictly leaving ordinary prose (`"Wait slash pause"`, `"One dot five"`) untouched.

const RECOGNIZED_TLDS: &[&str] = &[
    "com", "org", "net", "io", "ai", "dev", "app", "co", "sh", "rs", "gg", "edu", "gov", "me",
    "xyz", "tech", "cloud",
];

const PATH_STOPWORDS: &[&str] = &[
    "and", "or", "to", "for", "in", "on", "is", "was", "that", "which", "so", "but", "then",
    "please",
];

const BARE_DOMAIN_STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "to", "in", "on", "at", "by", "for", "from", "with", "into", "onto",
    "is", "was", "are", "were", "be", "do", "have", "can", "will", "i", "you", "he", "she", "it",
    "we", "they", "me", "my", "your", "our", "this", "that", "what", "who", "how", "tell", "ask",
    "give", "send", "show", "let", "make", "get", "go", "come", "put", "see", "say", "call",
    "help", "use", "try", "and", "or", "but", "so", "if", "not", "no", "yes",
];

const LEADING_WRAPPERS: &[char] = &['(', '[', '<', '"', '\''];
const TRAILING_PUNCT: &[char] = &[')', ']', '>', '"', '\'', '.', ',', '!', '?', ';', ':'];

#[derive(Clone, Copy)]
struct SpanToken<'a> {
    raw: &'a str,
    start: usize,
    end: usize,
}

fn tokenize_spans(input: &str) -> Vec<SpanToken<'_>> {
    let mut tokens = Vec::new();
    let mut in_tok = false;
    let mut tok_start = 0usize;

    for (idx, ch) in input.char_indices() {
        if ch.is_whitespace() {
            if in_tok {
                tokens.push(SpanToken {
                    raw: &input[tok_start..idx],
                    start: tok_start,
                    end: idx,
                });
                in_tok = false;
            }
        } else if !in_tok {
            tok_start = idx;
            in_tok = true;
        }
    }

    if in_tok {
        tokens.push(SpanToken {
            raw: &input[tok_start..],
            start: tok_start,
            end: input.len(),
        });
    }

    tokens
}

fn split_leading_wrappers(s: &str) -> (&str, &str) {
    let mut idx = 0;
    for (byte_idx, ch) in s.char_indices() {
        if LEADING_WRAPPERS.contains(&ch) {
            idx = byte_idx + ch.len_utf8();
        } else {
            break;
        }
    }
    (&s[..idx], &s[idx..])
}

fn split_trailing_punct(s: &str) -> (&str, &str) {
    let mut cut = s.len();
    for (byte_idx, ch) in s.char_indices().rev() {
        if TRAILING_PUNCT.contains(&ch) {
            cut = byte_idx;
        } else {
            break;
        }
    }
    (&s[..cut], &s[cut..])
}

fn is_recognized_tld(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    RECOGNIZED_TLDS.contains(&lower.as_str())
}

fn is_valid_host_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 {
        return false;
    }
    let bytes = label.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

fn is_valid_port(token: &str) -> bool {
    if token.is_empty() || token.len() > 5 || !token.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    matches!(token.parse::<u32>(), Ok(1..=65535))
}

fn is_valid_path_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    let lower = seg.to_ascii_lowercase();
    if PATH_STOPWORDS.contains(&lower.as_str())
        || matches!(
            lower.as_str(),
            "slash" | "dash" | "hyphen" | "underscore" | "dot" | "colon"
        )
    {
        return false;
    }
    let bytes = seg.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b'.' | b'~'))
}

/// Checks whether a clean (punctuation-stripped) token is already a literal URL or domain,
/// such as `github.com`, `https://lowyap.com`, `localhost`, or `localhost:3000`.
pub(crate) fn is_formatted_url_token(core: &str) -> bool {
    if core.is_empty() {
        return false;
    }
    let without_proto = ["https://", "http://"]
        .iter()
        .find_map(|p| {
            core.get(..p.len())
                .filter(|head| head.eq_ignore_ascii_case(p))
                .map(|_| &core[p.len()..])
        })
        .unwrap_or(core);

    let host_port = without_proto
        .split_once('/')
        .map_or(without_proto, |(hp, _)| hp);
    let host = match host_port.split_once(':') {
        Some((h, port)) => {
            if !is_valid_port(port) {
                return false;
            }
            h
        }
        None => host_port,
    };

    if host.eq_ignore_ascii_case("localhost") {
        return host != core
            || core.eq_ignore_ascii_case("localhost")
            || without_proto.len() < core.len();
    }

    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || !labels.iter().all(|l| is_valid_host_label(l)) {
        return false;
    }
    labels.last().is_some_and(|tld| is_recognized_tld(tld))
}

/// Attempts to parse `"http(s) colon (slash slash | double slash)"` starting at `idx`.
/// Returns `(scheme_prefix, next_token_idx)`.
fn match_spoken_protocol(tokens: &[SpanToken<'_>], idx: usize) -> Option<(&'static str, usize)> {
    let first = tokens.get(idx)?.raw;
    let (_, first_rest) = split_leading_wrappers(first);

    // Case A: "https" + "colon" + ("slash" "slash" | "double" "slash")
    let (scheme, after_colon_idx) = if first_rest.eq_ignore_ascii_case("https") {
        if !tokens.get(idx + 1)?.raw.eq_ignore_ascii_case("colon") {
            return None;
        }
        ("https://", idx + 2)
    } else if first_rest.eq_ignore_ascii_case("http") {
        if !tokens.get(idx + 1)?.raw.eq_ignore_ascii_case("colon") {
            return None;
        }
        ("http://", idx + 2)
    } else if first_rest.eq_ignore_ascii_case("https:") {
        ("https://", idx + 1)
    } else if first_rest.eq_ignore_ascii_case("http:") {
        ("http://", idx + 1)
    } else {
        return None;
    };

    let s1 = tokens.get(after_colon_idx)?.raw;
    let s2 = tokens.get(after_colon_idx + 1)?.raw;
    if (s1.eq_ignore_ascii_case("slash") || s1.eq_ignore_ascii_case("double"))
        && s2.eq_ignore_ascii_case("slash")
    {
        Some((scheme, after_colon_idx + 2))
    } else {
        None
    }
}

struct HostMatch<'a> {
    host: String,
    trailing_punct: &'a str,
    next_idx: usize,
    transformed: bool,
}

/// Attempts to parse a host (spoken domain `a dot b dot com`, `localhost`, or literal domain)
/// at `idx`. `has_protocol` relaxes bare-determiner guards when preceded by `http(s)://`.
fn match_host<'a>(
    tokens: &[SpanToken<'a>],
    idx: usize,
    has_protocol: bool,
) -> Option<HostMatch<'a>> {
    let tok = tokens.get(idx)?;
    let (lead, after_lead) = if has_protocol {
        ("", tok.raw)
    } else {
        split_leading_wrappers(tok.raw)
    };
    let _ = lead;
    let (first_core, first_trail) = split_trailing_punct(after_lead);
    if first_core.is_empty() {
        return None;
    }

    // 1. Spoken `dot` domain chain: `<label> ("dot" <label>)+` ending in a recognized TLD.
    if first_trail.is_empty() && is_valid_host_label(first_core) {
        let mut labels = vec![first_core];
        let mut trailing_on_end = "";
        let mut cursor = idx + 1;
        let mut last_valid_tld_step: Option<(usize, usize, &'a str)> = None;

        while cursor + 1 < tokens.len() && tokens[cursor].raw.eq_ignore_ascii_case("dot") {
            let candidate_raw = tokens[cursor + 1].raw;
            let (cand_lead, cand_after_lead) = split_leading_wrappers(candidate_raw);
            if !cand_lead.is_empty() {
                break;
            }
            let (cand_core, cand_trail) = split_trailing_punct(cand_after_lead);
            if !is_valid_host_label(cand_core) {
                break;
            }
            labels.push(cand_core);
            cursor += 2;

            if is_recognized_tld(cand_core) {
                last_valid_tld_step = Some((labels.len(), cursor, cand_trail));
            }

            if !cand_trail.is_empty() {
                trailing_on_end = cand_trail;
                break;
            }
        }

        let _ = trailing_on_end;
        if let Some((label_count, next_idx, end_trail)) = last_valid_tld_step {
            let chosen = &labels[..label_count];
            let first_lower = chosen[0].to_ascii_lowercase();
            let has_url_continuation = end_trail.is_empty()
                && tokens.get(next_idx).is_some_and(|kw| {
                    (kw.raw.eq_ignore_ascii_case("colon")
                        && tokens.get(next_idx + 1).is_some_and(|p| {
                            let (p_core, _) = split_trailing_punct(p.raw);
                            is_valid_port(p_core)
                        }))
                        || (kw.raw.eq_ignore_ascii_case("slash")
                            && tokens.get(next_idx + 1).is_some_and(|s| {
                                let (s_core, _) = split_trailing_punct(s.raw);
                                is_valid_path_segment(s_core)
                            }))
                });
            let blocked_bare_stopword = !has_protocol
                && !has_url_continuation
                && chosen.len() == 2
                && BARE_DOMAIN_STOPWORDS.contains(&first_lower.as_str());
            // Reject purely numeric pairs like `1 dot 5` (already filtered by TLD, double-guarded).
            let all_digits = chosen.iter().all(|l| l.bytes().all(|b| b.is_ascii_digit()));
            if !blocked_bare_stopword && !all_digits {
                let assembled = chosen
                    .iter()
                    .map(|l| l.to_ascii_lowercase())
                    .collect::<Vec<_>>()
                    .join(".");
                return Some(HostMatch {
                    host: assembled,
                    trailing_punct: end_trail,
                    next_idx,
                    transformed: true,
                });
            }
        }
    }

    // 2. Spoken `localhost`: valid when followed by `colon <port>`, `slash <seg>`, or protocol.
    if first_core.eq_ignore_ascii_case("localhost") {
        let has_port_next = first_trail.is_empty()
            && tokens
                .get(idx + 1)
                .is_some_and(|t| t.raw.eq_ignore_ascii_case("colon"))
            && tokens.get(idx + 2).is_some_and(|t| {
                let (p_core, _) = split_trailing_punct(t.raw);
                is_valid_port(p_core)
            });
        let has_slash_next = first_trail.is_empty()
            && tokens
                .get(idx + 1)
                .is_some_and(|t| t.raw.eq_ignore_ascii_case("slash"))
            && tokens.get(idx + 2).is_some_and(|t| {
                let (s_core, _) = split_trailing_punct(t.raw);
                is_valid_path_segment(s_core)
            });
        if has_protocol || has_port_next || has_slash_next {
            return Some(HostMatch {
                host: "localhost".to_string(),
                trailing_punct: first_trail,
                next_idx: idx + 1,
                transformed: has_protocol,
            });
        }
    }

    // 3. Already-literal URL or domain token (e.g. `github.com`, `localhost:3000`).
    if is_formatted_url_token(first_core) {
        return Some(HostMatch {
            host: first_core.to_string(),
            trailing_punct: first_trail,
            next_idx: idx + 1,
            transformed: has_protocol,
        });
    }

    None
}

/// Converts spoken URLs, domains, ports, and URL path segments in `< 1ms`.
pub fn format_spoken_urls(input: &str) -> String {
    if input.trim().is_empty() {
        return input.to_string();
    }

    let tokens = tokenize_spans(input);
    if tokens.is_empty() {
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    let mut last_flushed = 0usize;
    let mut formatted_any = false;
    let mut formatted_url_count = 0usize;
    let mut single_full_utterance_url = false;
    let mut idx = 0usize;

    while idx < tokens.len() {
        let start_idx = idx;
        let (lead_wrappers, _) = split_leading_wrappers(tokens[start_idx].raw);

        let (proto_opt, host_start_idx) = match match_spoken_protocol(&tokens, start_idx) {
            Some((scheme, next_i)) => (Some(scheme), next_i),
            None => (None, start_idx),
        };

        let Some(host_match) = match_host(&tokens, host_start_idx, proto_opt.is_some()) else {
            idx += 1;
            continue;
        };

        let mut url = String::new();
        if let Some(scheme) = proto_opt {
            url.push_str(scheme);
        }
        url.push_str(&host_match.host);

        let mut transformed = host_match.transformed || proto_opt.is_some();
        let mut trailing_punct = host_match.trailing_punct;
        let mut cursor = host_match.next_idx;

        // Optional spoken port (`colon 3000`) when host had no trailing punctuation, port, or path yet.
        let host_without_proto = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(&url);
        if trailing_punct.is_empty()
            && !host_without_proto.contains('/')
            && !host_without_proto.contains(':')
            && cursor + 1 < tokens.len()
            && tokens[cursor].raw.eq_ignore_ascii_case("colon")
        {
            let (port_core, port_trail) = split_trailing_punct(tokens[cursor + 1].raw);
            if is_valid_port(port_core) {
                url.push(':');
                url.push_str(port_core);
                trailing_punct = port_trail;
                cursor += 2;
                transformed = true;
            }
        }

        // Active URL path mode: consumes `slash <seg>` and internal `dash`/`hyphen`/`underscore`/`dot`
        // connectors between valid non-stopword path tokens, stopping at sentence punctuation or stopwords.
        let mut in_path = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(&url)
            .contains('/');

        while trailing_punct.is_empty() && cursor + 1 < tokens.len() {
            let connector = tokens[cursor].raw;
            let sep = if connector.eq_ignore_ascii_case("slash") {
                Some('/')
            } else if in_path
                && (connector.eq_ignore_ascii_case("dash")
                    || connector.eq_ignore_ascii_case("hyphen"))
            {
                Some('-')
            } else if in_path && connector.eq_ignore_ascii_case("underscore") {
                Some('_')
            } else if in_path && connector.eq_ignore_ascii_case("dot") {
                Some('.')
            } else {
                None
            };

            let Some(sep_ch) = sep else {
                break;
            };

            let seg_raw = tokens[cursor + 1].raw;
            let (seg_lead, seg_after_lead) = split_leading_wrappers(seg_raw);
            if !seg_lead.is_empty() {
                break;
            }
            let (seg_core, seg_trail) = split_trailing_punct(seg_after_lead);
            if !is_valid_path_segment(seg_core) {
                break;
            }

            if sep_ch == '/' && url.ends_with('/') {
                url.push_str(seg_core);
            } else {
                url.push(sep_ch);
                url.push_str(seg_core);
            }
            in_path = true;
            transformed = true;
            trailing_punct = seg_trail;
            cursor += 2;
        }

        if !transformed {
            idx = cursor.max(start_idx + 1);
            continue;
        }

        formatted_any = true;
        formatted_url_count += 1;

        let span_start = tokens[start_idx].start;
        let span_end = tokens[cursor - 1].end;
        let is_entire_utterance =
            start_idx == 0 && cursor == tokens.len() && lead_wrappers.is_empty();
        if is_entire_utterance && formatted_url_count == 1 {
            single_full_utterance_url = true;
        }

        out.push_str(&input[last_flushed..span_start]);
        out.push_str(lead_wrappers);
        out.push_str(&url);

        // Strip a single trailing ASR sentence period/question/exclamation ONLY when the entire
        // utterance is just the spoken URL itself (e.g. `"lowyap dot com."` -> `"lowyap.com"`),
        // while keeping legitimate sentence punctuation in prose (`"Check github.com/lowyap."`).
        let effective_trailing =
            if single_full_utterance_url && matches!(trailing_punct, "." | "!" | "?") {
                ""
            } else {
                trailing_punct
            };
        out.push_str(effective_trailing);

        last_flushed = span_end;
        idx = cursor;
    }

    if !formatted_any {
        return input.to_string();
    }

    out.push_str(&input[last_flushed..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_spoken_domains_subdomains_and_ports() {
        assert_eq!(format_spoken_urls("lowyap dot com"), "lowyap.com");
        assert_eq!(format_spoken_urls("Lowyap dot com."), "lowyap.com");
        assert_eq!(
            format_spoken_urls("docs dot lowyap dot com"),
            "docs.lowyap.com"
        );
        assert_eq!(
            format_spoken_urls("notes dot wisprflow dot ai"),
            "notes.wisprflow.ai"
        );
        assert_eq!(format_spoken_urls("localhost colon 3000"), "localhost:3000");
        assert_eq!(
            format_spoken_urls("localhost colon 3000."),
            "localhost:3000"
        );
    }

    #[test]
    fn formats_spoken_protocols() {
        assert_eq!(
            format_spoken_urls("https colon slash slash github dot com"),
            "https://github.com"
        );
        assert_eq!(
            format_spoken_urls("http colon double slash localhost colon 3000"),
            "http://localhost:3000"
        );
        assert_eq!(
            format_spoken_urls("HTTPS colon slash slash docs dot lowyap dot com."),
            "https://docs.lowyap.com"
        );
    }

    #[test]
    fn formats_url_paths_connectors_and_stops_at_stopwords_or_punctuation() {
        assert_eq!(
            format_spoken_urls("github.com slash lowyap"),
            "github.com/lowyap"
        );
        assert_eq!(
            format_spoken_urls("github.com slash lowyap."),
            "github.com/lowyap"
        );
        assert_eq!(
            format_spoken_urls("github dot com slash lowyap slash desktop"),
            "github.com/lowyap/desktop"
        );
        assert_eq!(
            format_spoken_urls("github.com slash lowyap dash desktop"),
            "github.com/lowyap-desktop"
        );
        assert_eq!(
            format_spoken_urls("github.com slash lowyap hyphen desktop underscore v2"),
            "github.com/lowyap-desktop_v2"
        );
        assert_eq!(
            format_spoken_urls("Check github.com slash lowyap."),
            "Check github.com/lowyap."
        );
        assert_eq!(
            format_spoken_urls("Open github.com slash lowyap and star the repo."),
            "Open github.com/lowyap and star the repo."
        );
        assert_eq!(
            format_spoken_urls("Open github.com slash lowyap dash and check."),
            "Open github.com/lowyap dash and check."
        );
        assert_eq!(
            format_spoken_urls("Visit (lowyap dot com), please!"),
            "Visit (lowyap.com), please!"
        );
        assert_eq!(
            format_spoken_urls("Check go dot dev slash doc"),
            "Check go.dev/doc"
        );
    }

    #[test]
    fn strict_zero_false_positive_prose_guard() {
        let prose_cases = [
            "Wait slash pause the video.",
            "Put a dot on the i.",
            "One dot five million",
            "Use a hyphen between words",
            "We lived through the dot com bubble.",
            "foo underscore bar",
            "config dot yaml",
            "Tell dot me what happened.",
            "Go to dot app settings.",
            "Can you dot io that?",
            "localhost:3000 colon 4000",
        ];
        for sentence in prose_cases {
            assert_eq!(
                format_spoken_urls(sentence),
                sentence,
                "prose sentence must remain untouched: {sentence}"
            );
        }
    }

    #[test]
    fn runs_well_under_one_millisecond() {
        let start = std::time::Instant::now();
        let out = format_spoken_urls(
            "Please check https colon slash slash github dot com slash lowyap dash desktop and docs dot lowyap dot com.",
        );
        let elapsed = start.elapsed();
        assert_eq!(
            out,
            "Please check https://github.com/lowyap-desktop and docs.lowyap.com."
        );
        assert!(
            elapsed.as_micros() < 1_000,
            "expected < 1ms, took {elapsed:?}"
        );
    }
}
