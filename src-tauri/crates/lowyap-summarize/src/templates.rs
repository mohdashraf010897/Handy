//! Meeting note template heading contracts and output validators.

use lowyap_ports::MeetingTemplate;

const GENERAL_HEADINGS: &[&str] = &[
    "## TL;DR",
    "## Key Decisions & Discussion",
    "## Expanded from Your Notes",
    "## Action Items",
];

const ONE_ON_ONE_HEADINGS: &[&str] = &[
    "## Check-in & Wins",
    "## Blockers & Coaching",
    "## Feedback & Career",
    "## Next Steps",
];

const STANDUP_HEADINGS: &[&str] = &[
    "## Progress Since Last Sync",
    "## Today's Plan",
    "## Blockers & Risks",
];

const LANGUAGE_LESSON_HEADINGS: &[&str] = &[
    "## Lesson Summary",
    "## New Vocabulary & Phrases",
    "## Grammar & Corrections",
    "## Practice Homework",
];

const CLIENT_CALL_HEADINGS: &[&str] = &[
    "## Client Goals & Pain Points",
    "## Product / Scope Alignment",
    "## Objections & Open Questions",
    "## Follow-ups & Owners",
];

/// Returns the exact Markdown level-2 headings required for `template`.
pub const fn required_headings(template: MeetingTemplate) -> &'static [&'static str] {
    match template {
        MeetingTemplate::General => GENERAL_HEADINGS,
        MeetingTemplate::OneOnOne => ONE_ON_ONE_HEADINGS,
        MeetingTemplate::Standup => STANDUP_HEADINGS,
        MeetingTemplate::LanguageLesson => LANGUAGE_LESSON_HEADINGS,
        MeetingTemplate::ClientCall => CLIENT_CALL_HEADINGS,
    }
}

/// Returns the stable snake-case identifier for `template`.
pub const fn template_label(template: MeetingTemplate) -> &'static str {
    match template {
        MeetingTemplate::General => "general",
        MeetingTemplate::OneOnOne => "one_on_one",
        MeetingTemplate::Standup => "standup",
        MeetingTemplate::LanguageLesson => "language_lesson",
        MeetingTemplate::ClientCall => "client_call",
    }
}

/// Validates that expanded meeting notes contain every required heading for
/// `template` and at least one substantive non-heading content line.
pub fn validate_expanded_notes(md: &str, template: MeetingTemplate) -> bool {
    let trimmed = md.trim();
    if trimmed.is_empty() {
        return false;
    }

    let all_headings_present = required_headings(template)
        .iter()
        .all(|heading| trimmed.lines().any(|line| line.trim() == *heading));
    if !all_headings_present {
        return false;
    }

    trimmed.lines().any(|line| {
        let t = line.trim();
        !t.is_empty() && !t.starts_with('#')
    })
}

/// Validates that rolling-window summary output is formatted as concise bullet
/// points rather than empty text or unstructured prose.
pub fn validate_rollup_bullets(md: &str) -> bool {
    let trimmed = md.trim();
    if trimmed.is_empty() {
        return false;
    }

    let mut bullet_lines = 0usize;
    let mut non_heading_lines = 0usize;

    for line in trimmed.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        non_heading_lines += 1;

        let payload = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("• "));
        if let Some(body) = payload {
            if body.trim().chars().count() >= 3 {
                bullet_lines += 1;
            }
        }
    }

    bullet_lines >= 1 && bullet_lines * 2 >= non_heading_lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_expanded_notes_enforces_exact_headings_for_all_templates() {
        let all_templates = [
            MeetingTemplate::General,
            MeetingTemplate::OneOnOne,
            MeetingTemplate::Standup,
            MeetingTemplate::LanguageLesson,
            MeetingTemplate::ClientCall,
        ];

        for template in all_templates {
            let mut valid = String::from("# Weekly Sync\n\n");
            for heading in required_headings(template) {
                valid.push_str(heading);
                valid.push_str("\n- Concrete bullet point\n\n");
            }
            assert!(
                validate_expanded_notes(&valid, template),
                "expected valid markdown for {:?}",
                template
            );

            // Headings only, no body lines -> invalid
            let headings_only = required_headings(template).join("\n");
            assert!(!validate_expanded_notes(&headings_only, template));

            // Missing one heading -> invalid
            let missing_last = required_headings(template)
                .iter()
                .take(required_headings(template).len() - 1)
                .map(|h| format!("{h}\n- Item"))
                .collect::<Vec<_>>()
                .join("\n\n");
            assert!(!validate_expanded_notes(&missing_last, template));
        }
    }

    #[test]
    fn validate_rollup_bullets_accepts_bullets_and_rejects_prose() {
        assert!(validate_rollup_bullets(
            "- **[Me]** Shipped AEC3 adapter.\n- **[Speaker 1]** Demo is ready."
        ));
        assert!(!validate_rollup_bullets(""));
        assert!(!validate_rollup_bullets("- "));
        assert!(!validate_rollup_bullets(
            "This is a plain paragraph.\nWithout bullet markers.\nStill prose."
        ));
    }
}
