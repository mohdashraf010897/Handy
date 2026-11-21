use serde_json::json;

use super::*;
use crate::test_support::assert_wire;

fn frame(samples: Vec<f32>) -> PcmFrame16k {
    PcmFrame16k {
        samples,
        timestamp_ms: 37,
    }
}

#[test]
fn rms_averages_squared_samples_before_taking_the_root() {
    assert!((frame(vec![-1.0, 0.0, 0.0, 0.0]).rms() - 0.5).abs() < f32::EPSILON);
}

#[test]
fn empty_audio_has_no_level_or_duration() {
    let empty = frame(vec![]);
    assert_eq!(
        (empty.rms(), empty.peak_abs(), empty.duration_ms()),
        (0.0, 0.0, 0)
    );
}

#[test]
fn peak_uses_magnitude_for_negative_samples() {
    assert_eq!(frame(vec![-0.75, 0.25, 0.5]).peak_abs(), 0.75);
}

#[test]
fn duration_truncates_fractional_milliseconds_at_16_khz() {
    assert_eq!(frame(vec![0.0; 1_615]).duration_ms(), 100);
}

#[test]
fn speaker_channels_keep_wire_values_distinct_from_display_labels() {
    for (channel, wire, label) in [
        (SpeakerChannel::Me, "me", "Me"),
        (SpeakerChannel::Others, "others", "Others"),
    ] {
        assert_wire(channel, json!(wire));
        assert_eq!((channel.as_str(), channel.display_label()), (wire, label));
    }
}

#[test]
fn audio_route_and_capture_mode_match_existing_wire_values() {
    assert_wire(OutputRouteKind::default(), json!("unknown"));
    assert_wire(OutputRouteKind::Headphones, json!("headphones"));
    assert_wire(OutputRouteKind::Speakers, json!("speakers"));
    assert_wire(AudioCaptureMode::default(), json!("mic_only_fallback"));
    assert_wire(AudioCaptureMode::DualStream, json!("dual_stream"));
}

#[test]
fn meeting_status_matches_existing_wire_values() {
    assert_wire(MeetingStatus::default(), json!("idle"));
    assert_wire(MeetingStatus::Recording, json!("recording"));
    assert_wire(MeetingStatus::Paused, json!("paused"));
    assert_wire(MeetingStatus::Summarizing, json!("summarizing"));
}

#[test]
fn meeting_templates_match_existing_wire_values() {
    assert_wire(MeetingTemplate::default(), json!("general"));
    assert_wire(MeetingTemplate::OneOnOne, json!("one_on_one"));
    assert_wire(MeetingTemplate::Standup, json!("standup"));
    assert_wire(MeetingTemplate::LanguageLesson, json!("language_lesson"));
    assert_wire(MeetingTemplate::ClientCall, json!("client_call"));
}

#[test]
fn transcript_preserves_capture_channel_with_refined_speaker_attribution() {
    assert_wire(
        TranscriptSegment {
            speaker: SpeakerIdentity {
                channel: SpeakerChannel::Others,
                speaker_id: "speaker-1".into(),
                display_label: "Speaker 1".into(),
                confidence: Some(0.75),
            },
            start_ms: 10,
            end_ms: 55,
            text: "Hello".into(),
            language: None,
        },
        json!({
            "speaker": {
                "channel": "others", "speaker_id": "speaker-1",
                "display_label": "Speaker 1", "confidence": 0.75
            },
            "start_ms": 10, "end_ms": 55, "text": "Hello", "language": null
        }),
    );
}

#[test]
fn pcm_frame_preserves_samples_and_time_on_the_wire() {
    assert_wire(
        frame(vec![-0.25, 0.5]),
        json!({"samples": [-0.25, 0.5], "timestamp_ms": 37}),
    );
}

#[test]
fn mic_only_chunk_keeps_an_empty_system_audio_track_on_the_wire() {
    assert_wire(
        DualStreamChunk16k {
            me_16k: vec![0.5],
            others_16k: vec![],
            output_route: OutputRouteKind::Speakers,
            start_ms: 10,
        },
        json!({"me_16k": [0.5], "others_16k": [], "output_route": "speakers", "start_ms": 10}),
    );
}

#[test]
fn utterance_span_preserves_its_source_channel_and_boundaries() {
    assert_wire(
        UtteranceSpan {
            channel: SpeakerChannel::Me,
            samples_16k: vec![-0.5],
            start_ms: 5,
            end_ms: 6,
        },
        json!({"channel": "me", "samples_16k": [-0.5], "start_ms": 5, "end_ms": 6}),
    );
}
