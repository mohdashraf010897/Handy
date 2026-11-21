use crate::flow::multilingual::HindustaniScript;
use crate::meeting::adapters;
use crate::meeting::types::{MeetingRollup, MeetingSegment, MeetingTemplate};
use crate::settings::get_settings;
use log::warn;
use lowyap_ports::NoteSummarizer;
use lowyap_summarize::{HybridMapReduceSummarizer, DEFAULT_MAX_INPUT_CHARS};
use tauri::AppHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryResult {
    pub text: String,
    pub summary_source: String,
}

pub async fn summarize_five_min_window(
    app: &AppHandle,
    segments: &[MeetingSegment],
    hindustani_script: HindustaniScript,
) -> SummaryResult {
    let fallback = summarize_five_min_window_local(segments);
    if segments.is_empty() {
        return fallback;
    }

    let settings = get_settings(app);
    let language_instruction = adapters::script_language_instruction(
        hindustani_script,
        &settings.spoken_languages,
        segments,
    );
    let req = adapters::to_window_summary_request(0, segments, language_instruction);
    let engine = adapters::build_production_note_summarizer(Some(app));

    match tokio::task::spawn_blocking(move || engine.summarize_window(&req)).await {
        Ok(Ok(output)) if !output.markdown.trim().is_empty() => adapters::to_summary_result(output),
        Ok(Ok(_)) => fallback,
        Ok(Err(err)) => {
            warn!("meeting window summarization fell back to local extractive: {err}");
            fallback
        }
        Err(err) => {
            warn!("meeting window summarization worker failed ({err}) — using local extractive");
            fallback
        }
    }
}

pub fn summarize_five_min_window_local(segments: &[MeetingSegment]) -> SummaryResult {
    let engine = HybridMapReduceSummarizer::deterministic_only(DEFAULT_MAX_INPUT_CHARS);
    let req = adapters::to_window_summary_request(0, segments, None);
    match engine.summarize_window(&req) {
        Ok(output) if !output.markdown.trim().is_empty() => adapters::to_summary_result(output),
        _ => SummaryResult {
            text: "- [00:00] No clear speech captured in this window.".to_string(),
            summary_source: "local_extractive".to_string(),
        },
    }
}

pub async fn generate_hybrid_meeting_notes(
    app: &AppHandle,
    title: &str,
    template: MeetingTemplate,
    rough_notes: &str,
    rollups: &[MeetingRollup],
    tail_segments: &[MeetingSegment],
    hindustani_script: HindustaniScript,
) -> SummaryResult {
    let fallback = generate_hybrid_meeting_notes_local(
        title,
        template,
        rough_notes,
        rollups,
        tail_segments,
        hindustani_script,
    );

    let settings = get_settings(app);
    let language_instruction = adapters::script_language_instruction(
        hindustani_script,
        &settings.spoken_languages,
        tail_segments,
    );
    let req = adapters::to_hybrid_notes_request(
        title,
        template,
        rough_notes,
        rollups,
        tail_segments,
        language_instruction,
    );
    let engine = adapters::build_production_note_summarizer(Some(app));

    match tokio::task::spawn_blocking(move || engine.expand_hybrid_notes(&req)).await {
        Ok(Ok(output)) if !output.markdown.trim().is_empty() => adapters::to_summary_result(output),
        Ok(Ok(_)) => fallback,
        Ok(Err(err)) => {
            warn!("meeting summary generation fell back to local extractive: {err}");
            fallback
        }
        Err(err) => {
            warn!("meeting summary generation worker failed ({err}) — using local extractive");
            fallback
        }
    }
}

pub fn generate_hybrid_meeting_notes_local(
    title: &str,
    template: MeetingTemplate,
    rough_notes: &str,
    rollups: &[MeetingRollup],
    tail_segments: &[MeetingSegment],
    hindustani_script: HindustaniScript,
) -> SummaryResult {
    let language_instruction =
        adapters::script_language_instruction(hindustani_script, &[], tail_segments);
    let req = adapters::to_hybrid_notes_request(
        title,
        template,
        rough_notes,
        rollups,
        tail_segments,
        language_instruction,
    );
    let engine = HybridMapReduceSummarizer::deterministic_only(DEFAULT_MAX_INPUT_CHARS);
    match engine.expand_hybrid_notes(&req) {
        Ok(output) => adapters::to_summary_result(output),
        Err(err) => {
            warn!("deterministic hybrid meeting summary returned error: {err}");
            SummaryResult {
                text: format!("# {}\n", title.trim()),
                summary_source: "local_extractive".to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::types::SpeakerChannel;
    use lowyap_summarize::budget::{chunk_rollups_by_budget, chunk_segments_by_budget};
    use lowyap_summarize::templates::required_headings;

    fn sample_segments() -> Vec<MeetingSegment> {
        vec![
            MeetingSegment {
                id: 1,
                meeting_id: "m1".to_string(),
                speaker: SpeakerChannel::Me,
                start_ms: 1_000,
                end_ms: 4_000,
                text: "Let's finalize the roadmap today.".to_string(),
                language: Some("en".to_string()),
                speaker_id: Some("me".to_string()),
                speaker_label: Some("Me".to_string()),
            },
            MeetingSegment {
                id: 2,
                meeting_id: "m1".to_string(),
                speaker: SpeakerChannel::Others,
                start_ms: 5_000,
                end_ms: 9_000,
                text: "We need to align with client expectations and risks.".to_string(),
                language: Some("en".to_string()),
                speaker_id: None,
                speaker_label: None,
            },
            MeetingSegment {
                id: 3,
                meeting_id: "m1".to_string(),
                speaker: SpeakerChannel::Me,
                start_ms: 10_000,
                end_ms: 14_000,
                text: "todo: send the action plan by Friday".to_string(),
                language: Some("en".to_string()),
                speaker_id: Some("me".to_string()),
                speaker_label: Some("Me".to_string()),
            },
        ]
    }

    fn sample_rollups() -> Vec<MeetingRollup> {
        vec![
            MeetingRollup {
                id: 1,
                meeting_id: "m1".to_string(),
                window_index: 0,
                start_ms: 0,
                end_ms: 300_000,
                summary_bullets:
                    "- [00:05] **Me**: Discussed launch plan\n- [02:00] **Others**: Need security follow-up"
                        .to_string(),
            },
            MeetingRollup {
                id: 2,
                meeting_id: "m1".to_string(),
                window_index: 1,
                start_ms: 300_000,
                end_ms: 600_000,
                summary_bullets: "- [05:00] **Others**: Asked scope questions".to_string(),
            },
        ]
    }

    #[test]
    fn summarize_window_local_should_emit_bullets() {
        let result = summarize_five_min_window_local(&sample_segments());
        assert_eq!(result.summary_source, "local_extractive");
        assert!(result.text.lines().count() >= 3);
        assert!(result.text.contains("[00:01]"));
    }

    #[test]
    fn local_hybrid_notes_should_include_required_headings_for_every_template() {
        let title = "Weekly sync";
        let rough = "- finalize release notes\n- follow up with client";
        let rollups = sample_rollups();
        let tail = sample_segments();

        for template in [
            MeetingTemplate::General,
            MeetingTemplate::OneOnOne,
            MeetingTemplate::Standup,
            MeetingTemplate::LanguageLesson,
            MeetingTemplate::ClientCall,
        ] {
            let result = generate_hybrid_meeting_notes_local(
                title,
                template,
                rough,
                &rollups,
                &tail,
                HindustaniScript::Auto,
            );
            for heading in required_headings(template.into()) {
                assert!(
                    result.text.contains(heading),
                    "missing heading '{}' for template {:?}",
                    heading,
                    template
                );
            }
        }
    }

    #[test]
    fn local_hybrid_notes_should_preserve_rough_bullets_with_evidence() {
        let rough = "- ship wave 3\n- need to validate risks";
        let result = generate_hybrid_meeting_notes_local(
            "Roadmap",
            MeetingTemplate::General,
            rough,
            &sample_rollups(),
            &sample_segments(),
            HindustaniScript::RomanHinglish,
        );

        assert!(result.text.contains("ship wave 3"));
        assert!(result.text.contains("need to validate risks"));
        assert!(result.text.contains("Evidence:"));
        assert!(result.text.contains("[Me]") || result.text.contains("[Others]"));
    }

    #[test]
    fn hybrid_prompt_should_bound_tail_context_for_long_meetings() {
        let rollups: Vec<MeetingRollup> = (0..13)
            .map(|idx| MeetingRollup {
                id: i64::from(idx + 1),
                meeting_id: "m1".to_string(),
                window_index: idx,
                start_ms: u64::from(idx) * 300_000,
                end_ms: (u64::from(idx) + 1) * 300_000,
                summary_bullets: format!("- [{}] rollup bullet {}", idx, "x".repeat(300)),
            })
            .collect();

        let mut tail = Vec::new();
        for idx in 0_u32..120 {
            tail.push(MeetingSegment {
                id: i64::from(idx + 1),
                meeting_id: "m1".to_string(),
                speaker: if idx % 2 == 0 {
                    SpeakerChannel::Me
                } else {
                    SpeakerChannel::Others
                },
                start_ms: u64::from(idx) * 5_000,
                end_ms: (u64::from(idx) + 1) * 5_000,
                text: format!("{} {}", "long tail text", "y".repeat(500)),
                language: Some("en".to_string()),
                speaker_id: None,
                speaker_label: None,
            });
        }

        let rollup_strings = adapters::to_rollup_strings(&rollups);
        let port_tail = adapters::to_port_segments(&tail);
        let rollup_chunks = chunk_rollups_by_budget(&rollup_strings, DEFAULT_MAX_INPUT_CHARS / 2);
        let tail_chunks = chunk_segments_by_budget(&port_tail, DEFAULT_MAX_INPUT_CHARS / 2);
        assert!(!rollup_chunks.is_empty());
        assert!(!tail_chunks.is_empty());
        for chunk in &rollup_chunks {
            let chunk_chars: usize = chunk.iter().map(|s| s.chars().count()).sum();
            assert!(
                chunk_chars <= DEFAULT_MAX_INPUT_CHARS / 2,
                "rollup chunk should stay bounded within budget, got {chunk_chars}"
            );
        }
        for chunk in &tail_chunks {
            let chunk_chars: usize = chunk.iter().map(|s| s.text.chars().count()).sum();
            assert!(
                chunk_chars <= DEFAULT_MAX_INPUT_CHARS / 2,
                "tail chunk should stay bounded within budget, got {chunk_chars}"
            );
        }
        let notes = generate_hybrid_meeting_notes_local(
            "65-min meeting",
            MeetingTemplate::General,
            "- rough",
            &rollups,
            &tail,
            HindustaniScript::Auto,
        );
        assert!(notes.text.contains("## TL;DR"));
    }

    #[test]
    fn sixty_minute_multilingual_meeting_respects_budget_and_preserves_headings() {
        let mut rollups = Vec::new();
        for win in 0_u32..12 {
            let start_ms = u64::from(win) * 300_000;
            let end_ms = start_ms + 300_000;
            let win_segments = vec![
                MeetingSegment {
                    id: i64::from(win * 3 + 1),
                    meeting_id: "m-60m".to_string(),
                    speaker: SpeakerChannel::Me,
                    start_ms: start_ms + 5_000,
                    end_ms: start_ms + 12_000,
                    text: format!(
                        "Window {win}: kal tak staging deploy karna hai and verify latency metrics."
                    ),
                    language: Some("hi".to_string()),
                    speaker_id: Some("me".to_string()),
                    speaker_label: Some("Me".to_string()),
                },
                MeetingSegment {
                    id: i64::from(win * 3 + 2),
                    meeting_id: "m-60m".to_string(),
                    speaker: SpeakerChannel::Others,
                    start_ms: start_ms + 30_000,
                    end_ms: start_ms + 42_000,
                    text: format!(
                        "ہمیں کل تک ریلیز چیک لسٹ مکمل کرنی ہے اور ماضی و مضارع کے نوٹس محفوظ رکھنے ہیں۔ ({win})"
                    ),
                    language: Some("ur".to_string()),
                    speaker_id: Some("speaker_1".to_string()),
                    speaker_label: Some("Speaker 1 (Rahul)".to_string()),
                },
                MeetingSegment {
                    id: i64::from(win * 3 + 3),
                    meeting_id: "m-60m".to_string(),
                    speaker: SpeakerChannel::Others,
                    start_ms: start_ms + 90_000,
                    end_ms: start_ms + 105_000,
                    text: format!(
                        "Action: Speaker 2 will validate API schema contract for window {win}."
                    ),
                    language: Some("en".to_string()),
                    speaker_id: Some("speaker_2".to_string()),
                    speaker_label: Some("Speaker 2".to_string()),
                },
            ];

            let win_summary = summarize_five_min_window_local(&win_segments);
            assert_eq!(win_summary.summary_source, "local_extractive");
            assert!(!win_summary.text.is_empty());

            rollups.push(MeetingRollup {
                id: i64::from(win + 1),
                meeting_id: "m-60m".to_string(),
                window_index: win,
                start_ms,
                end_ms,
                summary_bullets: win_summary.text,
            });
        }

        let tail_segments = vec![
            MeetingSegment {
                id: 901,
                meeting_id: "m-60m".to_string(),
                speaker: SpeakerChannel::Others,
                start_ms: 3_540_000,
                end_ms: 3_555_000,
                text: "ہم جمعہ تک فائنل رپورٹ بھیجیں گے ان شاء اللہ۔".to_string(),
                language: Some("ur".to_string()),
                speaker_id: Some("speaker_1".to_string()),
                speaker_label: Some("Speaker 1 (Rahul)".to_string()),
            },
            MeetingSegment {
                id: 902,
                meeting_id: "m-60m".to_string(),
                speaker: SpeakerChannel::Me,
                start_ms: 3_560_000,
                end_ms: 3_575_000,
                text: "Todo: send final Q4 signoff checklist to Rahul and Speaker 2.".to_string(),
                language: Some("en".to_string()),
                speaker_id: Some("me".to_string()),
                speaker_label: Some("Me".to_string()),
            },
        ];

        for template in [
            MeetingTemplate::General,
            MeetingTemplate::LanguageLesson,
            MeetingTemplate::ClientCall,
        ] {
            let result = generate_hybrid_meeting_notes_local(
                "Multilingual 60-Min Review",
                template,
                "- confirm staging deploy\n- فائنل رپورٹ جمعہ تک",
                &rollups,
                &tail_segments,
                HindustaniScript::UrduPersoArabic,
            );

            assert_eq!(result.summary_source, "local_extractive");
            for heading in required_headings(template.into()) {
                assert!(
                    result.text.contains(heading),
                    "missing required heading '{heading}' in 60-min multilingual output"
                );
            }
            assert!(result.text.contains("Speaker 1 (Rahul)"));
            assert!(result.text.contains("فائنل رپورٹ"));
            assert!(
                result.text.chars().count() < 24_000,
                "final notes must stay compact without overflow, got {} chars",
                result.text.chars().count()
            );
        }
    }

    #[test]
    fn local_hybrid_notes_should_be_deterministic() {
        let args = (
            "Determinism",
            MeetingTemplate::ClientCall,
            "- follow up on blockers",
            sample_rollups(),
            sample_segments(),
        );

        let first = generate_hybrid_meeting_notes_local(
            args.0,
            args.1,
            args.2,
            &args.3,
            &args.4,
            HindustaniScript::Auto,
        );
        let second = generate_hybrid_meeting_notes_local(
            args.0,
            args.1,
            args.2,
            &args.3,
            &args.4,
            HindustaniScript::Auto,
        );

        assert_eq!(first, second);
    }
}
