//! Escape/Undo transitions. The transcription coordinator owns the recorder
//! effects as well as the take ID, so a stale timer cannot touch new audio.

use crate::managers::{audio::AudioRecordingManager, transcription::TranscriptionManager};
use crate::session_bus::bus;
use crate::{shortcut, tray};
use lowyap_ports::{DictationPhase, SessionStateMachine, DEFAULT_SOFT_CANCEL_GRACE_MS};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

pub(crate) enum Escape {
    Retained { take_id: u64, epoch: u64 },
    Discarded { recording_was_active: bool },
}

pub(crate) fn escape(app: &AppHandle, lock_active: impl FnOnce()) -> Escape {
    let audio = app.state::<Arc<AudioRecordingManager>>();
    match bus().snapshot().dictation {
        DictationPhase::Recording { take_id, .. } if audio.is_recording() => {
            lock_active();
            if audio.is_paused() || audio.pause_recording() {
                let epoch = bus().update(app, |machine, now| {
                    machine
                        .soft_cancel_dictation(take_id, now, DEFAULT_SOFT_CANCEL_GRACE_MS)
                        .map(|(epoch, _)| epoch)
                });
                if let Some(epoch) = epoch {
                    audio.set_wal_soft_cancelled(true);
                    audio.invalidate_recording_readiness();
                    audio.remove_mute();
                    shortcut::register_cancel_shortcut(app);
                    tray::set_tray_state(app, tray::TrayIconState::Idle);
                    return Escape::Retained { take_id, epoch };
                }
            }
        }
        DictationPhase::SoftCancelled { take_id, .. } => {
            finalize_hard_cancel_for_take(app, Some(take_id), None);
            return Escape::Discarded {
                recording_was_active: true,
            };
        }
        _ => {}
    }

    Escape::Discarded {
        recording_was_active: hard_cancel(app),
    }
}

fn hard_cancel(app: &AppHandle) -> bool {
    let audio = app.state::<Arc<AudioRecordingManager>>();
    let recording_was_active = audio.is_recording();
    shortcut::unregister_cancel_shortcut(app);
    audio.remove_mute();
    audio.cancel_recording();
    let tm = app.state::<Arc<TranscriptionManager>>();
    tm.cancel_stream();
    tm.cancel_transcription();
    tm.maybe_unload_immediately("cancellation");
    tray::set_tray_state(app, tray::TrayIconState::Idle);
    bus().update(app, |machine, now| {
        machine.complete_or_abort_dictation(None, now)
    });
    crate::overlay::sync_overlay_from_session_bus(app);
    recording_was_active
}

fn resolve_soft_cancel(
    machine: &SessionStateMachine,
    requested: Option<u64>,
    now: u64,
) -> Option<u64> {
    match machine.snapshot(now).dictation {
        DictationPhase::SoftCancelled { take_id, .. }
            if requested.is_none_or(|id| id == take_id) =>
        {
            Some(take_id)
        }
        _ => None,
    }
}

fn discard_take(
    machine: &mut SessionStateMachine,
    requested: Option<u64>,
    epoch: Option<u64>,
    now: u64,
) -> bool {
    let Some(take_id) = resolve_soft_cancel(machine, requested, now) else {
        return false;
    };
    machine
        .expire_or_confirm_cancel(take_id, epoch, now)
        .is_some()
}

pub(crate) fn finalize_hard_cancel_for_take(
    app: &AppHandle,
    take_id: Option<u64>,
    epoch: Option<u64>,
) -> bool {
    if !bus().update(app, |machine, now| {
        discard_take(machine, take_id, epoch, now)
    }) {
        return false;
    }
    hard_cancel(app);
    true
}

pub(crate) fn expire_soft_cancel_if_current(app: &AppHandle, take_id: u64, epoch: u64) -> bool {
    finalize_hard_cancel_for_take(app, Some(take_id), Some(epoch))
}

pub(crate) fn undo(app: &AppHandle, take_id: Option<u64>) -> bool {
    let restored = bus().update(app, |machine, now| {
        let take_id = resolve_soft_cancel(machine, take_id, now)?;
        machine.undo_soft_cancel(take_id, now)
    });
    let Some(restored) = restored else {
        return false;
    };
    let DictationPhase::Recording { take_id, .. } = restored.dictation else {
        return false;
    };
    let audio = app.state::<Arc<AudioRecordingManager>>();
    if !audio.resume_recording() {
        hard_cancel(app);
        return false;
    }
    audio.set_wal_soft_cancelled(false);
    audio.apply_mute();
    tray::set_tray_state(app, tray::TrayIconState::Recording);
    shortcut::register_cancel_shortcut(app);
    crate::overlay::restore_recording_after_undo(app, take_id);
    let _ = app.emit("recording-pause-state", false);
    let _ = app.emit("recording-lock-state", true);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use lowyap_ports::{AudioCaptureMode, MeetingPhase, OverlayProjection};

    const TAKE: u64 = 7;
    const CANCELLED: u64 = 200;
    const DEADLINE: u64 = CANCELLED + DEFAULT_SOFT_CANCEL_GRACE_MS;

    fn retain(machine: &mut SessionStateMachine) -> u64 {
        machine.start_dictation(TAKE, 100, false);
        machine
            .soft_cancel_dictation(TAKE, CANCELLED, DEFAULT_SOFT_CANCEL_GRACE_MS)
            .unwrap()
            .0
    }

    #[test]
    fn undo_and_recancel_reject_the_first_timer() {
        let mut machine = SessionStateMachine::new();
        let epoch = retain(&mut machine);
        assert!(machine.undo_soft_cancel(TAKE, DEADLINE - 1).is_some());
        assert!(!discard_take(
            &mut machine,
            Some(TAKE),
            Some(epoch),
            DEADLINE
        ));
        machine
            .soft_cancel_dictation(TAKE, DEADLINE, DEFAULT_SOFT_CANCEL_GRACE_MS)
            .unwrap();
        assert!(!discard_take(
            &mut machine,
            Some(TAKE),
            Some(epoch),
            DEADLINE + DEFAULT_SOFT_CANCEL_GRACE_MS
        ));
    }

    #[test]
    fn second_escape_discards_before_deadline() {
        let mut machine = SessionStateMachine::new();
        retain(&mut machine);
        assert!(!discard_take(
            &mut machine,
            Some(TAKE + 1),
            None,
            CANCELLED + 1
        ));
        assert!(discard_take(&mut machine, Some(TAKE), None, CANCELLED + 1));
        assert_eq!(
            machine.snapshot(CANCELLED + 1).dictation,
            DictationPhase::Idle
        );
    }

    #[test]
    fn expiry_restores_running_meeting_overlay() {
        let mut machine = SessionStateMachine::new();
        machine.sync_meeting(
            MeetingPhase::Recording {
                meeting_id: "meeting".into(),
                title: "Test".into(),
                started_at_ms: 10,
                accumulated_paused_ms: 0,
                app_name: None,
                capture_mode: AudioCaptureMode::DualStream,
            },
            None,
            10,
        );
        let epoch = retain(&mut machine);
        assert!(!discard_take(
            &mut machine,
            Some(TAKE),
            Some(epoch),
            DEADLINE - 1
        ));
        assert!(discard_take(
            &mut machine,
            Some(TAKE),
            Some(epoch),
            DEADLINE
        ));
        assert!(
            matches!(machine.snapshot(DEADLINE).overlay, OverlayProjection::MeetingActive { meeting_id, .. } if meeting_id == "meeting")
        );
    }
}
