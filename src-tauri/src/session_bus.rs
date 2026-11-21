use lowyap_ports::{
    resolve_overlay_priority, DetectedCallBanner, DictationPhase, MeetingPhase, OverlayProjection,
    SessionBusSnapshot, SessionEventSink, SessionStateMachine,
};
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

use crate::meeting::session::MeetingSessionManager;
use crate::TranscriptionCoordinator;

#[derive(Default)]
pub(crate) struct SessionBus {
    state: Mutex<BusState>,
    published_seq: Arc<AtomicU64>,
}

#[derive(Default)]
struct BusState {
    machine: SessionStateMachine,
    notice_epoch: u64,
}

struct TauriSessionEventSink {
    app: AppHandle,
    published_seq: Arc<AtomicU64>,
}

impl SessionEventSink for TauriSessionEventSink {
    fn publish_snapshot(&self, snapshot: &SessionBusSnapshot) {
        let next_snapshot = snapshot.clone();
        let handle = self.app.clone();
        let published_seq = Arc::clone(&self.published_seq);
        let _ = self.app.run_on_main_thread(move || {
            if next_snapshot.seq > published_seq.fetch_max(next_snapshot.seq, Ordering::AcqRel) {
                let _ = handle.emit("session-bus-state", &next_snapshot);
                crate::overlay::apply_session_overlay(&handle);
            }
        });
    }
}

static BUS: Lazy<SessionBus> = Lazy::new(SessionBus::default);

pub(crate) fn bus() -> &'static SessionBus {
    &BUS
}

/// Epoch milliseconds for the wire, backed by a monotonic clock for deadlines.
pub(crate) fn now_ms() -> u64 {
    static CLOCK: Lazy<(Instant, u64)> = Lazy::new(|| {
        (
            Instant::now(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        )
    });
    CLOCK.1.saturating_add(CLOCK.0.elapsed().as_millis() as u64)
}

impl SessionBus {
    pub(crate) fn snapshot(&self) -> SessionBusSnapshot {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .machine
            .snapshot(now_ms())
    }

    pub(crate) fn tray_state(&self) -> (SessionBusSnapshot, bool) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            state.machine.snapshot(now_ms()),
            state.machine.is_meeting_pill_dismissed(),
        )
    }

    pub(crate) fn native_overlay(&self, dictation_enabled: bool) -> OverlayProjection {
        let (snapshot, dismissed) = self.tray_state();
        if dictation_enabled {
            snapshot.overlay
        } else {
            resolve_overlay_priority(
                &DictationPhase::Idle,
                &snapshot.meeting,
                snapshot.detected_call.as_ref(),
                None,
                dismissed,
                snapshot.server_now_ms,
            )
        }
    }

    pub(crate) fn update<R>(
        &self,
        app: &AppHandle,
        update: impl FnOnce(&mut SessionStateMachine, u64) -> R,
    ) -> R {
        self.update_state(app, |state, now| update(&mut state.machine, now))
    }

    fn update_state<R>(&self, app: &AppHandle, update: impl FnOnce(&mut BusState, u64) -> R) -> R {
        let (result, changed, after, tray_changed) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = now_ms();
            let before = state.machine.snapshot(now);
            let was_dismissed = state.machine.is_meeting_pill_dismissed();
            let result = update(&mut state, now);
            let after = state.machine.snapshot(now);
            let tray_changed = before.tray_meeting != after.tray_meeting
                || was_dismissed != state.machine.is_meeting_pill_dismissed();
            (result, before.seq != after.seq, after, tray_changed)
        };
        if changed {
            TauriSessionEventSink {
                app: app.clone(),
                published_seq: Arc::clone(&self.published_seq),
            }
            .publish_snapshot(&after);
            if tray_changed {
                crate::tray::update_tray_menu(app);
            }
        }
        result
    }

    pub(crate) fn sync_meeting(
        &self,
        app: &AppHandle,
        read_live: impl FnOnce() -> Option<(MeetingPhase, Option<DetectedCallBanner>)>,
    ) {
        self.update(app, |machine, now| {
            if let Some((meeting, call)) = read_live() {
                let before = machine.snapshot(now);
                if before.meeting != meeting || before.detected_call != call {
                    machine.sync_meeting(meeting, call, now);
                }
            }
        });
    }

    pub(crate) fn set_pill_dismissed(&self, app: &AppHandle, dismissed: Option<bool>) -> bool {
        self.update_state(app, |state, now| {
            let current = state.machine.snapshot(now);
            if !current.tray_meeting.active {
                return false;
            }
            let was_dismissed = state.machine.is_meeting_pill_dismissed();
            let next = dismissed.unwrap_or(!was_dismissed);
            if next != was_dismissed {
                state.machine.set_meeting_pill_dismissed(next, now);
            }
            true
        })
    }

    pub(crate) fn show_notice(
        &self,
        app: &AppHandle,
        kind: &str,
        take_id: Option<u64>,
    ) -> Option<u64> {
        self.update_state(app, |state, now| {
            if let Some(take_id) = take_id {
                let before = state.machine.snapshot(now).seq;
                let completed = state
                    .machine
                    .complete_or_abort_dictation(Some(take_id), now);
                if completed.seq == before {
                    return None;
                }
            }
            state.notice_epoch = state.notice_epoch.wrapping_add(1);
            state
                .machine
                .set_transient_notice(Some(kind.to_owned()), now);
            Some(state.notice_epoch)
        })
    }

    pub(crate) fn clear_notice(&self, app: &AppHandle, epoch: u64) {
        self.update_state(app, |state, now| {
            if epoch == state.notice_epoch && state.machine.snapshot(now).transient_notice.is_some()
            {
                state.machine.set_transient_notice(None, now);
            }
        });
    }
}

#[tauri::command(async)]
#[specta::specta]
pub fn get_session_bus_state_json(app: AppHandle) -> String {
    if let Some(meeting) = app.try_state::<Arc<MeetingSessionManager>>() {
        meeting.sync_session_bus();
    }
    let snapshot = app
        .try_state::<TranscriptionCoordinator>()
        .and_then(|coordinator| coordinator.session_snapshot())
        .unwrap_or_else(|| bus().snapshot());
    serde_json::to_string(&snapshot).unwrap_or_else(|err| {
        log::error!("Failed to serialize session bus snapshot: {err}");
        String::new()
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn undo_cancel_recording(app: AppHandle, take_id: Option<u64>) -> bool {
    app.try_state::<TranscriptionCoordinator>()
        .is_some_and(|coordinator| coordinator.undo_soft_cancel(take_id))
}

#[tauri::command(async)]
#[specta::specta]
pub fn confirm_cancel_recording(app: AppHandle, take_id: Option<u64>) -> bool {
    app.try_state::<TranscriptionCoordinator>()
        .is_some_and(|coordinator| coordinator.confirm_soft_cancel(take_id))
}

#[tauri::command(async)]
#[specta::specta]
pub fn set_meeting_pill_dismissed(app: AppHandle, dismissed: bool) -> bool {
    bus().set_pill_dismissed(&app, Some(dismissed))
}

#[tauri::command]
#[specta::specta]
pub fn open_meetings_workspace(app: AppHandle) {
    crate::show_main_window(&app);
    let _ = app.emit("navigate-section", "meetings");
}
