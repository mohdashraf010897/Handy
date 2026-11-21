use serde_json::json;

use super::*;
use crate::test_support::assert_wire;

#[test]
fn stateful_ports_can_move_to_a_worker_as_trait_objects() {
    fn assert_send<T: Send>() {}
    assert_send::<Box<dyn EchoCanceller>>();
    assert_send::<Box<dyn UtteranceSegmenter>>();
    assert_send::<Box<dyn SpeakerDiarizer>>();
}

#[test]
fn shared_ports_can_be_called_from_multiple_workers_as_trait_objects() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Box<dyn AsrProvider>>();
    assert_send_sync::<Box<dyn NoteSummarizer>>();
    assert_send_sync::<Box<dyn SessionEventSink>>();
}

#[test]
fn asr_result_keeps_unknown_language_and_no_speech_metadata() {
    assert_wire(
        AsrResult {
            text: String::new(),
            detected_language: None,
            no_speech_prob: Some(0.875),
        },
        json!({"text": "", "detected_language": null, "no_speech_prob": 0.875}),
    );
}

#[test]
fn window_summary_keeps_language_instruction_and_window_index() {
    assert_wire(
        WindowSummaryRequest {
            window_index: 7,
            segments: vec![],
            language_instruction: Some("Write in English.".into()),
        },
        json!({"window_index": 7, "segments": [], "language_instruction": "Write in English."}),
    );
}

#[test]
fn hybrid_notes_keeps_rough_notes_and_rollups_separate() {
    assert_wire(
        HybridNotesRequest {
            title: "Check-in".into(),
            template: MeetingTemplate::OneOnOne,
            rough_notes: "Discuss tests".into(),
            rollups: vec!["Review planned".into()],
            tail_segments: vec![],
            language_instruction: None,
        },
        json!({
            "title": "Check-in", "template": "one_on_one", "rough_notes": "Discuss tests",
            "rollups": ["Review planned"], "tail_segments": [], "language_instruction": null
        }),
    );
}

#[test]
fn summary_output_preserves_markdown_and_provenance() {
    assert_wire(
        SummaryOutput {
            markdown: "- Follow up".into(),
            source_id: "local_extractive".into(),
        },
        json!({"markdown": "- Follow up", "source_id": "local_extractive"}),
    );
}
