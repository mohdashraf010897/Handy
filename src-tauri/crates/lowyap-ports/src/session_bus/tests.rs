use serde_json::json;

use super::*;
use crate::test_support::assert_wire;

fn recording(meeting_id: &str, accumulated_paused_ms: u64) -> MeetingPhase {
    MeetingPhase::Recording {
        meeting_id: meeting_id.into(),
        title: "Planning".into(),
        started_at_ms: 100,
        accumulated_paused_ms,
        app_name: None,
        capture_mode: AudioCaptureMode::DualStream,
    }
}

fn paused(paused_at_ms: u64, accumulated_paused_ms: u64) -> MeetingPhase {
    MeetingPhase::Paused {
        meeting_id: "m1".into(),
        title: "Planning".into(),
        started_at_ms: 100,
        paused_at_ms,
        accumulated_paused_ms,
        app_name: Some("Zoom".into()),
        capture_mode: AudioCaptureMode::MicOnlyFallback,
    }
}

fn summarizing() -> MeetingPhase {
    MeetingPhase::Summarizing {
        meeting_id: "m1".into(),
        title: "Planning".into(),
    }
}

fn call() -> DetectedCallBanner {
    DetectedCallBanner {
        call_id: "c1".into(),
        app_name: "Zoom".into(),
        output_route: OutputRouteKind::Headphones,
    }
}

fn soft_cancelled() -> (SessionStateMachine, u64) {
    let mut machine = SessionStateMachine::new();
    machine.start_dictation(1, 100, false);
    let (epoch, _) = machine
        .soft_cancel_dictation(1, 200, DEFAULT_SOFT_CANCEL_GRACE_MS)
        .unwrap();
    (machine, epoch)
}

#[test]
fn all_dictation_phases_take_foreground_and_completion_restores_meeting() {
    let mut machine = SessionStateMachine::new();
    machine.sync_meeting(recording("m1", 0), Some(call()), 100);
    assert_eq!(
        machine.start_dictation(1, 200, true).overlay,
        OverlayProjection::DictationRecording {
            take_id: 1,
            locked: true,
            paused: false
        },
    );
    assert_eq!(
        machine
            .finish_dictation_to_transcribing(1, 300)
            .unwrap()
            .overlay,
        OverlayProjection::DictationTranscribing { take_id: 1 },
    );
    assert_eq!(
        machine
            .finish_dictation_to_post_processing(1, 400)
            .unwrap()
            .overlay,
        OverlayProjection::DictationPostProcessing { take_id: 1 },
    );
    assert!(matches!(
        machine.complete_or_abort_dictation(Some(1), 500).overlay,
        OverlayProjection::MeetingActive {
            elapsed_ms: 400,
            ..
        }
    ));
}

#[test]
fn soft_cancel_has_foreground_until_its_timer_restores_the_meeting() {
    let mut machine = SessionStateMachine::new();
    machine.sync_meeting(recording("m1", 0), Some(call()), 100);
    machine.start_dictation(1, 150, false);
    let (epoch, snapshot) = machine.soft_cancel_dictation(1, 200, 500).unwrap();
    assert_eq!(
        snapshot.overlay,
        OverlayProjection::DictationSoftCancelled {
            take_id: 1,
            cancelled_at_ms: 200,
            expires_at_ms: 700,
            grace_ms: 500,
        },
    );
    let snapshot = machine
        .expire_or_confirm_cancel(1, Some(epoch), 700)
        .unwrap();
    assert_eq!(snapshot.dictation, DictationPhase::Idle);
    assert!(matches!(
        snapshot.overlay,
        OverlayProjection::MeetingActive {
            elapsed_ms: 600,
            ..
        }
    ));
}

#[test]
fn undo_restores_locked_unpaused_recording_and_invalidates_the_timer() {
    let (mut machine, epoch) = soft_cancelled();
    let resumed = machine.undo_soft_cancel(1, 5_199).unwrap();
    assert_eq!(
        resumed.dictation,
        DictationPhase::Recording {
            take_id: 1,
            started_at_ms: 200,
            locked: true,
            paused: false
        },
    );
    assert_eq!(
        machine.expire_or_confirm_cancel(1, Some(epoch), 5_200),
        None
    );
    assert_eq!(machine.snapshot(5_200).dictation, resumed.dictation);
    assert_eq!(machine.snapshot(5_200).seq, resumed.seq);
}

#[test]
fn stale_timer_cannot_expire_a_new_take_even_if_it_is_soft_cancelled() {
    let (mut machine, epoch) = soft_cancelled();
    let started = machine.start_dictation(2, 300, true);
    assert_eq!(
        machine.expire_or_confirm_cancel(1, Some(epoch), 5_200),
        None
    );
    assert_eq!(machine.snapshot(5_200).dictation, started.dictation);
    let (_, second) = machine.soft_cancel_dictation(2, 5_300, 500).unwrap();
    assert_eq!(
        machine.expire_or_confirm_cancel(1, Some(epoch), 5_900),
        None
    );
    assert_eq!(machine.snapshot(5_900).dictation, second.dictation);
}

#[test]
fn recancelling_the_same_take_does_not_reuse_the_old_timer_epoch() {
    let (mut machine, old_epoch) = soft_cancelled();
    machine.undo_soft_cancel(1, 300).unwrap();
    let (new_epoch, _) = machine.soft_cancel_dictation(1, 400, 100).unwrap();
    assert!(new_epoch > old_epoch);
    assert_eq!(
        machine.expire_or_confirm_cancel(1, Some(old_epoch), 5_200),
        None
    );
    assert!(machine
        .expire_or_confirm_cancel(1, Some(new_epoch), 5_200)
        .is_some());
}

#[test]
fn undo_rejects_the_deadline_and_later_timestamps() {
    let (mut machine, _) = soft_cancelled();
    for now in [5_200, 5_201] {
        let before = machine.snapshot(now);
        assert_eq!(machine.undo_soft_cancel(1, now), None);
        assert_eq!(machine.snapshot(now), before);
    }
}

#[test]
fn early_timer_does_not_end_the_undo_window() {
    let (mut machine, epoch) = soft_cancelled();
    let before = machine.snapshot(5_199);
    assert_eq!(
        machine.expire_or_confirm_cancel(1, Some(epoch), 5_199),
        None
    );
    assert_eq!(machine.snapshot(5_199), before);
}

#[test]
fn explicit_confirmation_before_deadline_only_clears_the_matching_soft_cancel() {
    let (mut machine, _) = soft_cancelled();
    assert_eq!(machine.expire_or_confirm_cancel(2, None, 300), None);
    assert_eq!(
        machine
            .expire_or_confirm_cancel(1, None, 300)
            .unwrap()
            .dictation,
        DictationPhase::Idle
    );
    machine.start_dictation(2, 400, true);
    assert_eq!(machine.expire_or_confirm_cancel(2, None, 410), None);
}

#[test]
fn soft_cancel_requires_the_matching_recording_phase() {
    let mut machine = SessionStateMachine::new();
    assert_eq!(machine.soft_cancel_dictation(1, 100, 500), None);
    machine.start_dictation(1, 200, false);
    assert_eq!(machine.soft_cancel_dictation(2, 300, 500), None);
    machine.soft_cancel_dictation(1, 400, 500).unwrap();
    assert_eq!(machine.soft_cancel_dictation(1, 500, 500), None);
    assert_eq!(machine.undo_soft_cancel(2, 500), None);
}

#[test]
fn pause_updates_both_flags_for_only_the_correct_take() {
    let mut machine = SessionStateMachine::new();
    machine.start_dictation(1, 100, false);
    assert_eq!(machine.set_dictation_paused(2, true, true, 200), None);
    assert_eq!(
        machine
            .set_dictation_paused(1, true, true, 200)
            .unwrap()
            .dictation,
        DictationPhase::Recording {
            take_id: 1,
            started_at_ms: 100,
            locked: true,
            paused: true
        }
    );
}

#[test]
fn completion_callbacks_cannot_skip_stages_or_overwrite_a_different_take() {
    let mut machine = SessionStateMachine::new();
    machine.start_dictation(2, 100, false);
    let before = machine.snapshot(200);
    assert_eq!(machine.finish_dictation_to_post_processing(2, 200), None);
    assert_eq!(machine.finish_dictation_to_transcribing(1, 200), None);
    assert_eq!(machine.complete_or_abort_dictation(Some(1), 200), before);
    machine.finish_dictation_to_transcribing(2, 300).unwrap();
    assert_eq!(machine.finish_dictation_to_post_processing(1, 400), None);
    assert_eq!(
        machine.complete_or_abort_dictation(None, 400).dictation,
        DictationPhase::Idle
    );
}

#[test]
fn meeting_elapsed_excludes_multiple_completed_pauses_and_freezes_during_each_pause() {
    // Recording from 100, paused over [300, 500] and [800, 900].
    assert_eq!(paused(300, 0).elapsed_ms(499), 200);
    assert_eq!(recording("m1", 200).elapsed_ms(500), 200);
    assert_eq!(paused(800, 200).elapsed_ms(899), 500);
    assert_eq!(recording("m1", 300).elapsed_ms(900), 500);
    assert_eq!(recording("m1", 300).elapsed_ms(1_100), 700);
}

#[test]
fn elapsed_duration_saturates_when_timing_is_inconsistent() {
    assert_eq!(recording("m1", 0).elapsed_ms(99), 0);
    assert_eq!(paused(150, 100).elapsed_ms(250), 0);
}

#[test]
fn dismissing_the_meeting_hides_only_its_pill_and_never_reveals_a_call_banner() {
    let mut machine = SessionStateMachine::new();
    machine.sync_meeting(recording("m1", 0), Some(call()), 100);
    let minimized = machine.set_meeting_pill_dismissed(true, 200);
    assert_eq!(minimized.overlay, OverlayProjection::Hidden);
    assert!(minimized.tray_meeting.active);
    assert_eq!(minimized.tray_meeting.elapsed_ms, 100);
}

#[test]
fn dismissal_persists_through_pause_and_resume_but_resets_for_a_new_meeting() {
    let mut machine = SessionStateMachine::new();
    machine.sync_meeting(recording("m1", 0), None, 100);
    machine.set_meeting_pill_dismissed(true, 200);
    assert_eq!(
        machine.sync_meeting(paused(300, 0), None, 300).overlay,
        OverlayProjection::Hidden
    );
    assert_eq!(
        machine
            .sync_meeting(recording("m1", 100), None, 400)
            .overlay,
        OverlayProjection::Hidden
    );
    assert!(matches!(
        machine.sync_meeting(recording("m2", 0), None, 500).overlay,
        OverlayProjection::MeetingActive { meeting_id, .. } if meeting_id == "m2"
    ));
}

#[test]
fn summarizing_still_projects_when_the_meeting_pill_was_dismissed() {
    let mut machine = SessionStateMachine::new();
    machine.sync_meeting(recording("m1", 0), None, 100);
    machine.set_meeting_pill_dismissed(true, 200);
    let snapshot = machine.sync_meeting(summarizing(), Some(call()), 300);
    assert_eq!(
        snapshot.overlay,
        OverlayProjection::MeetingSummarizing {
            meeting_id: "m1".into(),
            title: "Planning".into(),
        }
    );
    assert!(snapshot.tray_meeting.summarizing);
    assert!(!snapshot.tray_meeting.active);
}

#[test]
fn notice_yields_to_dictation_and_clearing_it_restores_the_meeting() {
    let mut machine = SessionStateMachine::new();
    machine.sync_meeting(recording("m1", 0), Some(call()), 100);
    assert_eq!(
        machine
            .set_transient_notice(Some("notice".into()), 200)
            .overlay,
        OverlayProjection::TransientNotice {
            notice_kind: "notice".into()
        },
    );
    machine.start_dictation(1, 300, false);
    let (_, snapshot) = machine.soft_cancel_dictation(1, 400, 500).unwrap();
    assert!(matches!(
        snapshot.overlay,
        OverlayProjection::DictationSoftCancelled { .. }
    ));
    assert!(matches!(
        machine
            .expire_or_confirm_cancel(1, None, 500)
            .unwrap()
            .overlay,
        OverlayProjection::TransientNotice { .. }
    ));
    assert!(matches!(
        machine.set_transient_notice(None, 600).overlay,
        OverlayProjection::MeetingActive { .. }
    ));
}

#[test]
fn an_idle_meeting_projects_the_current_call_and_clears_when_detection_ends() {
    let mut machine = SessionStateMachine::new();
    let snapshot = machine.sync_meeting(MeetingPhase::Idle, Some(call()), 100);
    assert_eq!(
        snapshot.overlay,
        OverlayProjection::CallDetected {
            call_id: "c1".into(),
            app_name: "Zoom".into(),
            output_route: OutputRouteKind::Headphones,
        }
    );
    assert_eq!(
        machine.sync_meeting(MeetingPhase::Idle, None, 200).overlay,
        OverlayProjection::Hidden
    );
}

#[test]
fn snapshots_update_time_without_advancing_the_state_revision() {
    let mut machine = SessionStateMachine::new();
    let synced = machine.sync_meeting(recording("m1", 0), None, 100);
    let later = machine.snapshot(300);
    assert_eq!(
        (
            synced.seq,
            later.seq,
            later.server_now_ms,
            later.tray_meeting.elapsed_ms
        ),
        (1, 1, 300, 200)
    );
}

#[test]
fn idle_snapshot_has_exact_wire_keys_and_nulls() {
    assert_wire(
        SessionStateMachine::new().snapshot(25),
        json!({
            "seq": 0, "server_now_ms": 25, "dictation": {"phase": "idle"}, "meeting": {"phase": "idle"},
            "detected_call": null, "transient_notice": null, "overlay": {"mode": "hidden"},
            "tray_meeting": {"active": false, "paused": false, "summarizing": false,
                             "meeting_id": null, "title": null, "elapsed_ms": 0}
        }),
    );
}

#[test]
fn soft_cancel_snapshot_tags_phase_and_overlay_mode_separately() {
    let (machine, _) = soft_cancelled();
    assert_wire(
        machine.snapshot(300),
        json!({
            "seq": 2, "server_now_ms": 300,
            "dictation": {"phase": "soft_cancelled", "take_id": 1, "cancelled_at_ms": 200,
                          "expires_at_ms": 5200, "grace_ms": 5000},
            "meeting": {"phase": "idle"}, "detected_call": null, "transient_notice": null,
            "overlay": {"mode": "dictation_soft_cancelled", "take_id": 1, "cancelled_at_ms": 200,
                        "expires_at_ms": 5200, "grace_ms": 5000},
            "tray_meeting": {"active": false, "paused": false, "summarizing": false,
                             "meeting_id": null, "title": null, "elapsed_ms": 0}
        }),
    );
}

#[test]
fn paused_meeting_snapshot_projects_frozen_elapsed_and_keeps_detected_call_data() {
    let mut machine = SessionStateMachine::new();
    assert_wire(
        machine.sync_meeting(paused(500, 100), Some(call()), 900),
        json!({
            "seq": 1, "server_now_ms": 900, "dictation": {"phase": "idle"},
            "meeting": {"phase": "paused", "meeting_id": "m1", "title": "Planning",
                        "started_at_ms": 100, "paused_at_ms": 500, "accumulated_paused_ms": 100,
                        "app_name": "Zoom", "capture_mode": "mic_only_fallback"},
            "detected_call": {"call_id": "c1", "app_name": "Zoom", "output_route": "headphones"},
            "transient_notice": null,
            "overlay": {"mode": "meeting_active", "meeting_id": "m1", "title": "Planning", "paused": true,
                        "elapsed_ms": 300, "started_at_ms": 100, "accumulated_paused_ms": 100,
                        "paused_at_ms": 500, "app_name": "Zoom"},
            "tray_meeting": {"active": true, "paused": true, "summarizing": false,
                             "meeting_id": "m1", "title": "Planning", "elapsed_ms": 300}
        }),
    );
}

#[test]
fn unpaused_meeting_uses_null_for_unknown_app_and_pause_start() {
    let mut machine = SessionStateMachine::new();
    let snapshot = machine.sync_meeting(recording("m1", 0), None, 200);
    assert_wire(
        snapshot.meeting,
        json!({
            "phase": "recording", "meeting_id": "m1", "title": "Planning", "started_at_ms": 100,
            "accumulated_paused_ms": 0, "app_name": null, "capture_mode": "dual_stream"
        }),
    );
    assert_wire(
        snapshot.overlay,
        json!({
            "mode": "meeting_active", "meeting_id": "m1", "title": "Planning", "paused": false,
            "elapsed_ms": 100, "started_at_ms": 100, "accumulated_paused_ms": 0,
            "paused_at_ms": null, "app_name": null
        }),
    );
}

#[test]
fn active_dictation_pipeline_uses_exact_wire_tags() {
    let mut machine = SessionStateMachine::new();
    let snapshot = machine.start_dictation(2, 100, true);
    assert_wire(
        snapshot.dictation,
        json!({
            "phase": "recording", "take_id": 2, "started_at_ms": 100, "locked": true, "paused": false
        }),
    );
    assert_wire(
        snapshot.overlay,
        json!({
            "mode": "dictation_recording", "take_id": 2, "locked": true, "paused": false
        }),
    );
    let snapshot = machine.finish_dictation_to_transcribing(2, 200).unwrap();
    assert_wire(
        snapshot.dictation,
        json!({"phase": "transcribing", "take_id": 2}),
    );
    assert_wire(
        snapshot.overlay,
        json!({"mode": "dictation_transcribing", "take_id": 2}),
    );
    let snapshot = machine.finish_dictation_to_post_processing(2, 300).unwrap();
    assert_wire(
        snapshot.dictation,
        json!({"phase": "post_processing", "take_id": 2}),
    );
    assert_wire(
        snapshot.overlay,
        json!({"mode": "dictation_post_processing", "take_id": 2}),
    );
}

#[test]
fn call_notice_and_summary_projections_use_exact_wire_tags() {
    let mut machine = SessionStateMachine::new();
    let snapshot = machine.sync_meeting(MeetingPhase::Idle, Some(call()), 100);
    assert_wire(
        snapshot.overlay,
        json!({
            "mode": "call_detected", "call_id": "c1", "app_name": "Zoom", "output_route": "headphones"
        }),
    );
    let snapshot = machine.set_transient_notice(Some("notice".into()), 200);
    assert_wire(
        snapshot.overlay,
        json!({"mode": "transient_notice", "notice_kind": "notice"}),
    );
    machine.set_transient_notice(None, 300);
    let snapshot = machine.sync_meeting(summarizing(), None, 400);
    assert_wire(
        snapshot.meeting,
        json!({"phase": "summarizing", "meeting_id": "m1", "title": "Planning"}),
    );
    assert_wire(
        snapshot.overlay,
        json!({"mode": "meeting_summarizing", "meeting_id": "m1", "title": "Planning"}),
    );
}
