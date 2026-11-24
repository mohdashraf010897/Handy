use crate::flow::multilingual::HindustaniScript;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use crate::meeting::adapters::{
    build_production_completion_backends, build_production_echo_canceller,
    build_production_speaker_diarizer, build_production_utterance_segmenter, to_port_segments,
    to_rollup_strings,
};
use crate::meeting::audio_tap::SystemDualAudioBackend;
use crate::meeting::detector::CallDetectionState;
use crate::meeting::store::MeetingStore;
use crate::meeting::summarizer::{
    generate_hybrid_meeting_notes, generate_hybrid_meeting_notes_local, summarize_five_min_window,
    summarize_five_min_window_local, SummaryResult,
};
use crate::meeting::types::{
    AudioCaptureMode, DetectedCallInfo, DualAudioCaptureBackend, MeetingDetail, MeetingLiveState,
    MeetingRollup, MeetingSegment, MeetingStatus, MeetingSummaryRecord, MeetingTemplate,
    OutputRouteKind, SpeakerChannel,
};
use chrono::Local;
use log::{debug, warn};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const SAMPLE_RATE: u64 = 16_000;
const ROLLUP_WINDOW_MS: u64 = 300_000; // 5 minutes
const RECENT_SEGMENTS_CAP: usize = 40;
const DETECTOR_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(1_000);
const DETECTOR_ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(750);
const CALL_END_NEGATIVE_POLLS_REQUIRED: u8 = 3;
const SILENCE_RMS_THRESHOLD: f32 = 0.0025;

pub trait ChunkTranscriber: Send + Sync {
    fn transcribe_chunk(&self, samples: &[f32]) -> Result<String, String>;
}

#[derive(Clone)]
pub struct TranscriptionManagerChunkTranscriber {
    manager: Arc<TranscriptionManager>,
}

impl TranscriptionManagerChunkTranscriber {
    pub fn new(manager: Arc<TranscriptionManager>) -> Self {
        Self { manager }
    }
}

impl ChunkTranscriber for TranscriptionManagerChunkTranscriber {
    fn transcribe_chunk(&self, samples: &[f32]) -> Result<String, String> {
        if rms_energy(samples) < SILENCE_RMS_THRESHOLD {
            return Ok(String::new());
        }

        self.manager.initiate_model_load();
        self.manager
            .transcribe(samples.to_vec())
            .map_err(|err| err.to_string())
    }
}

pub trait CallDetectorBackend: Send + Sync {
    fn poll_detected_call(&self) -> Result<Option<DetectedCallInfo>, String>;
    fn dismiss_call(&self, call_id: Option<&str>) -> Result<(), String>;
    fn active_call_app(&self) -> Option<crate::meeting::macos_bridge::RunningCallApp> {
        None
    }
}

#[derive(Debug, Default)]
pub struct PlatformCallDetector {
    state: Mutex<CallDetectionState>,
}

impl PlatformCallDetector {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(CallDetectionState::new()),
        }
    }
}

impl CallDetectorBackend for PlatformCallDetector {
    fn poll_detected_call(&self) -> Result<Option<DetectedCallInfo>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "call detector state is unavailable".to_string())?;
        Ok(crate::meeting::detector::poll_call_detection(
            &mut state,
            false,
            now_ms_i64(),
        ))
    }

    fn dismiss_call(&self, call_id: Option<&str>) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "call detector state is unavailable".to_string())?;

        if let Some(id) = call_id {
            state.snooze_call(id);
        } else if let Some(active) = state.active_call_id().map(str::to_string) {
            state.snooze_call(&active);
        }
        Ok(())
    }

    fn active_call_app(&self) -> Option<crate::meeting::macos_bridge::RunningCallApp> {
        crate::meeting::macos_bridge::detect_running_call_app()
    }
}

#[derive(Default)]
pub struct NoopCallDetector;

impl CallDetectorBackend for NoopCallDetector {
    fn poll_detected_call(&self) -> Result<Option<DetectedCallInfo>, String> {
        Ok(None)
    }

    fn dismiss_call(&self, _call_id: Option<&str>) -> Result<(), String> {
        Ok(())
    }
}

struct ActiveMeeting {
    meeting_id: String,
    stop_flag: Arc<AtomicBool>,
    paused_flag: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SoftStopDeadline {
    stopped_at_ms: u64,
    expires_at_ms: u64,
    grace_ms: u64,
    epoch: u64,
}

#[derive(Default)]
struct MeetingClock {
    started_at_ms: u64,
    paused_at_ms: Option<u64>,
    accumulated_paused_ms: u64,
    soft_stop: Option<SoftStopDeadline>,
    soft_stop_epoch: u64,
    watched_call_bundle_id: Option<String>,
    call_end_negative_polls: u8,
    auto_stop_suppressed_until_rejoin: bool,
}

type EchoCancellerFactory = dyn Fn() -> Box<dyn lowyap_ports::EchoCanceller> + Send + Sync;
type UtteranceSegmenterFactory =
    dyn Fn(Option<&AppHandle>) -> Box<dyn lowyap_ports::UtteranceSegmenter> + Send + Sync;
type SpeakerDiarizerFactory =
    dyn Fn(AudioCaptureMode) -> Box<dyn lowyap_ports::SpeakerDiarizer> + Send + Sync;

struct MeetingSessionManagerInner {
    app_handle: Option<AppHandle>,
    store: MeetingStore,
    capture_backend: Arc<dyn DualAudioCaptureBackend>,
    transcriber: Arc<dyn ChunkTranscriber>,
    call_detector: Arc<dyn CallDetectorBackend>,
    echo_canceller_factory: Arc<EchoCancellerFactory>,
    segmenter_factory: Arc<UtteranceSegmenterFactory>,
    diarizer_factory: Arc<SpeakerDiarizerFactory>,
    audio_manager: Option<Arc<AudioRecordingManager>>,
    live_state: Mutex<MeetingLiveState>,
    // Take after live_state whenever both are needed.
    clock: Mutex<MeetingClock>,
    active_meeting: Mutex<Option<ActiveMeeting>>,
    detector_stop: AtomicBool,
    detector_handle: Mutex<Option<JoinHandle<()>>>,
    next_id: AtomicU64,
}

impl Drop for MeetingSessionManagerInner {
    fn drop(&mut self) {
        self.detector_stop.store(true, Ordering::Release);

        if let Ok(mut handle_guard) = self.detector_handle.lock() {
            if let Some(handle) = handle_guard.take() {
                let _ = handle.join();
            }
        }

        if let Ok(mut active_guard) = self.active_meeting.lock() {
            if let Some(mut active) = active_guard.take() {
                active.stop_flag.store(true, Ordering::Release);
                if let Some(worker) = active.worker.take() {
                    let _ = worker.join();
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct MeetingSessionManager {
    inner: Arc<MeetingSessionManagerInner>,
}

impl MeetingSessionManager {
    pub fn new_with_defaults(
        app_handle: AppHandle,
        transcription_manager: Arc<TranscriptionManager>,
        audio_manager: Option<Arc<AudioRecordingManager>>,
    ) -> Result<Self, String> {
        let store = MeetingStore::new(&app_handle)?;
        let capture_backend: Arc<dyn DualAudioCaptureBackend> = Arc::new(SystemDualAudioBackend);
        let transcriber: Arc<dyn ChunkTranscriber> = Arc::new(
            TranscriptionManagerChunkTranscriber::new(transcription_manager),
        );
        let call_detector: Arc<dyn CallDetectorBackend> = Arc::new(PlatformCallDetector::new());

        Ok(Self::new(
            app_handle,
            store,
            capture_backend,
            transcriber,
            call_detector,
            audio_manager,
        ))
    }

    pub fn new(
        app_handle: AppHandle,
        store: MeetingStore,
        capture_backend: Arc<dyn DualAudioCaptureBackend>,
        transcriber: Arc<dyn ChunkTranscriber>,
        call_detector: Arc<dyn CallDetectorBackend>,
        audio_manager: Option<Arc<AudioRecordingManager>>,
    ) -> Self {
        let manager = Self {
            inner: Arc::new(MeetingSessionManagerInner {
                app_handle: Some(app_handle),
                store,
                capture_backend,
                transcriber,
                call_detector,
                echo_canceller_factory: Arc::new(build_production_echo_canceller),
                segmenter_factory: Arc::new(build_production_utterance_segmenter),
                diarizer_factory: Arc::new(build_production_speaker_diarizer),
                audio_manager,
                live_state: Mutex::new(Self::default_live_state()),
                clock: Mutex::default(),
                active_meeting: Mutex::new(None),
                detector_stop: AtomicBool::new(false),
                detector_handle: Mutex::new(None),
                next_id: AtomicU64::new(1),
            }),
        };

        manager.spawn_detector_loop();
        let recovery_manager = manager.clone();
        thread::spawn(move || {
            let recovered = recovery_manager.recover_orphaned_meetings();
            if recovered > 0 {
                log::info!("meeting: recovered {recovered} orphaned meeting session(s) on startup");
            }
        });
        manager
    }

    /// Recovers meetings left with `ended_at_ms IS NULL` after a crash or force-quit:
    /// discards 0-segment empty stubs and finalizes substantive sessions with
    /// segment-derived `duration_ms`, hybrid notes, and auto-title.
    pub fn recover_orphaned_meetings(&self) -> usize {
        let orphan_ids = match self.inner.store.list_orphaned_meeting_ids() {
            Ok(ids) => ids,
            Err(err) => {
                warn!("meeting: failed to inspect orphaned meetings: {err}");
                return 0;
            }
        };
        if orphan_ids.is_empty() {
            return 0;
        }

        let active_id = self
            .inner
            .live_state
            .lock()
            .ok()
            .and_then(|live| live.active_meeting_id.clone());

        let mut handled = 0usize;
        for meeting_id in orphan_ids {
            if active_id.as_deref() == Some(meeting_id.as_str()) {
                continue;
            }
            let Ok(detail_before) = self.inner.store.get_meeting_detail(&meeting_id) else {
                continue;
            };
            if Self::should_discard_empty_meeting(
                &detail_before.segments,
                detail_before.rollups.len(),
                &detail_before.rough_notes,
            ) {
                if self.inner.store.delete_meeting(&meeting_id).unwrap_or(false) {
                    handled += 1;
                }
                continue;
            }

            let max_seg_end_ms = detail_before
                .segments
                .iter()
                .map(|s| s.end_ms)
                .max()
                .unwrap_or(0);
            let max_rollup_end_ms = detail_before
                .rollups
                .iter()
                .map(|r| r.end_ms)
                .max()
                .unwrap_or(0);
            let inferred_duration_ms = detail_before
                .summary
                .duration_ms
                .max(max_seg_end_ms)
                .max(max_rollup_end_ms)
                .max(1_000);
            let ended_at_ms = detail_before
                .summary
                .started_at_ms
                .saturating_add(i64::try_from(inferred_duration_ms).unwrap_or(1_000));

            if let Err(err) =
                self.inner
                    .store
                    .finalize_meeting(&meeting_id, ended_at_ms, inferred_duration_ms)
            {
                warn!("meeting: failed to finalize orphan {meeting_id}: {err}");
                continue;
            }

            if detail_before.expanded_notes_md.trim().is_empty() {
                let tail_segments = self
                    .inner
                    .store
                    .get_tail_segments(&meeting_id, 160)
                    .unwrap_or_else(|_| detail_before.segments.clone());
                let summary = self.generate_hybrid_summary(
                    &detail_before.summary,
                    &detail_before.rough_notes,
                    &detail_before.rollups,
                    &tail_segments,
                );
                let _ = self.inner.store.update_expanded_notes(
                    &meeting_id,
                    &summary.text,
                    &summary.summary_source,
                );

                if let Ok(fresh_detail) = self.inner.store.get_meeting_detail(&meeting_id) {
                    if lowyap_summarize::is_placeholder_meeting_title(&fresh_detail.summary.title) {
                        let mut rollup_strings = to_rollup_strings(&fresh_detail.rollups);
                        if rollup_strings.is_empty()
                            && summary.summary_source != "local_extractive"
                            && !summary.text.trim().is_empty()
                        {
                            rollup_strings.push(summary.text.clone());
                        }
                        let port_segments = to_port_segments(&fresh_detail.segments);
                        let backends =
                            build_production_completion_backends(self.inner.app_handle.as_ref());
                        if let Some(generated_title) = lowyap_summarize::generate_meeting_title(
                            &fresh_detail.rough_notes,
                            &rollup_strings,
                            &port_segments,
                            backends.first().map(|b| b.as_ref()),
                        ) {
                            let placeholder_title = fresh_detail.summary.title.clone();
                            if self
                                .inner
                                .store
                                .update_placeholder_title_if_unchanged(
                                    &meeting_id,
                                    &placeholder_title,
                                    &generated_title,
                                )
                                .unwrap_or(false)
                            {
                                let old_h1 = format!("# {}", placeholder_title.trim());
                                let trimmed_md = summary.text.trim_start();
                                if let Some(rest) = trimmed_md.strip_prefix(&old_h1) {
                                    if rest.is_empty()
                                        || rest.starts_with('\n')
                                        || rest.starts_with('\r')
                                    {
                                        let synced_md = format!("# {generated_title}{rest}");
                                        let _ = self.inner.store.update_expanded_notes(
                                            &meeting_id,
                                            &synced_md,
                                            &summary.summary_source,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            handled += 1;
        }

        handled
    }

    #[cfg(test)]
    fn new_for_tests(
        store: MeetingStore,
        capture_backend: Arc<dyn DualAudioCaptureBackend>,
        transcriber: Arc<dyn ChunkTranscriber>,
        call_detector: Arc<dyn CallDetectorBackend>,
    ) -> Self {
        use crate::meeting::adapters::{FixedTenSecSegmenterAdapter, SpeakerEchoGuardAdapter};
        Self {
            inner: Arc::new(MeetingSessionManagerInner {
                app_handle: None,
                store,
                capture_backend,
                transcriber,
                call_detector,
                echo_canceller_factory: Arc::new(|| Box::new(SpeakerEchoGuardAdapter::default())),
                segmenter_factory: Arc::new(|_| Box::new(FixedTenSecSegmenterAdapter::default())),
                diarizer_factory: Arc::new(build_production_speaker_diarizer),
                audio_manager: None,
                live_state: Mutex::new(Self::default_live_state()),
                clock: Mutex::default(),
                active_meeting: Mutex::new(None),
                detector_stop: AtomicBool::new(false),
                detector_handle: Mutex::new(None),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    #[cfg(test)]
    fn new_for_tests_with_production_dsp(
        store: MeetingStore,
        capture_backend: Arc<dyn DualAudioCaptureBackend>,
        transcriber: Arc<dyn ChunkTranscriber>,
        call_detector: Arc<dyn CallDetectorBackend>,
    ) -> Self {
        Self {
            inner: Arc::new(MeetingSessionManagerInner {
                app_handle: None,
                store,
                capture_backend,
                transcriber,
                call_detector,
                echo_canceller_factory: Arc::new(build_production_echo_canceller),
                segmenter_factory: Arc::new(build_production_utterance_segmenter),
                diarizer_factory: Arc::new(build_production_speaker_diarizer),
                audio_manager: None,
                live_state: Mutex::new(Self::default_live_state()),
                clock: Mutex::default(),
                active_meeting: Mutex::new(None),
                detector_stop: AtomicBool::new(false),
                detector_handle: Mutex::new(None),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn default_live_state() -> MeetingLiveState {
        MeetingLiveState {
            status: MeetingStatus::Idle,
            active_meeting_id: None,
            title: String::new(),
            template: MeetingTemplate::General,
            elapsed_ms: 0,
            capture_mode: AudioCaptureMode::MicOnlyFallback,
            output_route: OutputRouteKind::Unknown,
            app_name: None,
            rough_notes: String::new(),
            recent_segments: Vec::new(),
            rollup_count: 0,
            detected_call: None,
            auto_detect_enabled: true,
        }
    }

    pub fn get_live_state(&self) -> MeetingLiveState {
        self.inner
            .live_state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| Self::default_live_state())
    }

    pub fn is_meeting_active(&self) -> bool {
        self.get_live_state().status != MeetingStatus::Idle
    }

    pub(crate) fn sync_session_bus(&self) {
        sync_session_bus(&self.inner);
    }

    pub fn start_meeting(
        &self,
        title: Option<String>,
        template: Option<MeetingTemplate>,
    ) -> Result<MeetingLiveState, String> {
        let calendar_ctx = crate::meeting::macos_bridge::active_calendar_meeting();
        self.start_meeting_with_calendar(title, template, calendar_ctx)
    }

    pub fn start_meeting_with_calendar(
        &self,
        title: Option<String>,
        template: Option<MeetingTemplate>,
        calendar_ctx: Option<crate::meeting::macos_bridge::ActiveCalendarMeeting>,
    ) -> Result<MeetingLiveState, String> {
        let mut active_guard = self
            .inner
            .active_meeting
            .lock()
            .map_err(|_| "meeting state is unavailable".to_string())?;
        if active_guard.is_some() || self.is_meeting_active() {
            return Err("A meeting session is already active.".to_string());
        }

        let capture = self.inner.capture_backend.start_capture()?;
        let user_supplied_title = title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let (calendar_title, expected_attendees) = match calendar_ctx {
            Some(ctx) => (
                Some(ctx.title.trim().to_owned()).filter(|s| !s.is_empty()),
                ctx.attendees,
            ),
            None => (None, Vec::new()),
        };
        let resolved_template = template.unwrap_or_else(|| {
            if expected_attendees.len() == 1 {
                MeetingTemplate::OneOnOne
            } else {
                MeetingTemplate::General
            }
        });
        let resolved_title = normalize_title(user_supplied_title.or(calendar_title));
        let meeting_id = self.next_meeting_id();
        let started_at = crate::session_bus::now_ms();
        let started_at_ms = i64::try_from(started_at).unwrap_or(i64::MAX);

        let capture_mode = capture.capture_mode();
        let output_route = capture.output_route();
        let detected_from_state = self
            .inner
            .live_state
            .lock()
            .ok()
            .and_then(|state| state.detected_call.clone());
        let active_app_probe = if detected_from_state.is_none() {
            self.inner.call_detector.active_call_app()
        } else {
            None
        };
        let app_name = detected_from_state
            .as_ref()
            .map(|call| call.app_name.clone())
            .or_else(|| active_app_probe.as_ref().map(|app| app.app_name.clone()));
        let watched_call_bundle_id = detected_from_state
            .as_ref()
            .map(|call| call.app_bundle_id.clone())
            .filter(|b| !b.trim().is_empty())
            .or_else(|| active_app_probe.map(|app| app.bundle_id));

        self.inner.store.create_meeting(
            &meeting_id,
            &resolved_title,
            started_at_ms,
            resolved_template,
            capture_mode,
            output_route,
            app_name.as_deref(),
        )?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let paused_flag = Arc::new(AtomicBool::new(false));

        {
            let mut live = self
                .inner
                .live_state
                .lock()
                .map_err(|_| "live meeting state is unavailable".to_string())?;
            live.status = MeetingStatus::Recording;
            live.active_meeting_id = Some(meeting_id.clone());
            live.title = resolved_title;
            live.template = resolved_template;
            live.elapsed_ms = 0;
            live.capture_mode = capture_mode;
            live.output_route = output_route;
            live.app_name = app_name;
            live.rough_notes.clear();
            live.recent_segments.clear();
            live.rollup_count = 0;
            *self
                .inner
                .clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = MeetingClock {
                started_at_ms: started_at,
                watched_call_bundle_id,
                ..MeetingClock::default()
            };
        }

        let inner = Arc::clone(&self.inner);
        let worker_stop = Arc::clone(&stop_flag);
        let worker_pause = Arc::clone(&paused_flag);
        let worker_meeting_id = meeting_id.clone();

        let worker = thread::spawn(move || {
            run_capture_worker(
                inner,
                capture,
                worker_meeting_id,
                worker_stop,
                worker_pause,
                started_at_ms,
                expected_attendees,
            );
        });

        *active_guard = Some(ActiveMeeting {
            meeting_id,
            stop_flag,
            paused_flag,
            worker: Some(worker),
        });

        drop(active_guard);
        let state = self.get_live_state();
        emit_live_state(&self.inner, &state);
        Ok(state)
    }

    pub fn pause_meeting(&self) -> Result<MeetingLiveState, String> {
        {
            let active = self
                .inner
                .active_meeting
                .lock()
                .map_err(|_| "meeting state is unavailable".to_string())?;
            let active = active
                .as_ref()
                .filter(|active| !active.stop_flag.load(Ordering::Acquire))
                .ok_or_else(|| "No active meeting session exists.".to_string())?;
            let mut live = self
                .inner
                .live_state
                .lock()
                .map_err(|_| "live meeting state is unavailable".to_string())?;
            if live.status != MeetingStatus::Recording {
                return Err("Meeting is not currently recording.".to_string());
            }
            live.status = MeetingStatus::Paused;
            let mut clock = self
                .inner
                .clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            clock.paused_at_ms = Some(crate::session_bus::now_ms());
            clock.soft_stop = None;
            clock.call_end_negative_polls = 0;
            active.paused_flag.store(true, Ordering::Release);
        }

        let state = self.get_live_state();
        emit_live_state(&self.inner, &state);
        Ok(state)
    }

    pub fn resume_meeting(&self) -> Result<MeetingLiveState, String> {
        let (meeting_id, marker_elapsed, was_soft_stopped) = {
            let active = self
                .inner
                .active_meeting
                .lock()
                .map_err(|_| "meeting state is unavailable".to_string())?;
            let active = active
                .as_ref()
                .filter(|active| !active.stop_flag.load(Ordering::Acquire))
                .ok_or_else(|| "No active meeting session exists.".to_string())?;
            let mut live = self
                .inner
                .live_state
                .lock()
                .map_err(|_| "live meeting state is unavailable".to_string())?;
            if live.status != MeetingStatus::Paused {
                return Err("Meeting is not paused.".to_string());
            }
            let mut clock = self
                .inner
                .clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let was_soft_stopped = clock.soft_stop.take().is_some();
            if was_soft_stopped {
                // Suppress repeat auto-stop after an explicit post-hangup Resume
                // unless the user later joins a live call session again.
                clock.auto_stop_suppressed_until_rejoin = true;
                clock.watched_call_bundle_id = None;
            }
            clock.call_end_negative_polls = 0;
            if let Some(paused_at) = clock.paused_at_ms.take() {
                clock.accumulated_paused_ms = clock
                    .accumulated_paused_ms
                    .saturating_add(crate::session_bus::now_ms().saturating_sub(paused_at));
            }
            live.status = MeetingStatus::Recording;
            active.paused_flag.store(false, Ordering::Release);
            (active.meeting_id.clone(), live.elapsed_ms, was_soft_stopped)
        };

        if !was_soft_stopped {
            let marker_text = format!("[Paused {}]", format_mm_ss(marker_elapsed));
            if let Ok(segment) = self.inner.store.append_segment(
                &meeting_id,
                SpeakerChannel::Others,
                marker_elapsed,
                marker_elapsed,
                &marker_text,
                None,
            ) {
                push_recent_segment(&self.inner, segment);
            }
        }

        let state = self.get_live_state();
        emit_live_state(&self.inner, &state);
        Ok(state)
    }

    /// Transitions an active recording into the Wispr-style post-hangup grace
    /// window (`MeetingPhase::SoftStopped`), freezing capture immediately and
    /// scheduling auto-finalization after `grace_ms` unless resumed or stopped.
    pub(crate) fn soft_stop_on_call_end(&self, grace_ms: u64) -> Option<(String, u64)> {
        let now = crate::session_bus::now_ms();
        let (meeting_id, epoch) = {
            let active = self.inner.active_meeting.lock().ok()?;
            let active = active
                .as_ref()
                .filter(|active| !active.stop_flag.load(Ordering::Acquire))?;
            let mut live = self.inner.live_state.lock().ok()?;
            if live.status != MeetingStatus::Recording {
                return None;
            }
            live.status = MeetingStatus::Paused;
            let mut clock = self
                .inner
                .clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            clock.soft_stop_epoch = clock.soft_stop_epoch.saturating_add(1);
            let epoch = clock.soft_stop_epoch;
            clock.paused_at_ms = Some(now);
            clock.call_end_negative_polls = 0;
            clock.soft_stop = Some(SoftStopDeadline {
                stopped_at_ms: now,
                expires_at_ms: now.saturating_add(grace_ms),
                grace_ms,
                epoch,
            });
            active.paused_flag.store(true, Ordering::Release);
            (active.meeting_id.clone(), epoch)
        };

        let state = self.get_live_state();
        emit_live_state(&self.inner, &state);

        if grace_ms > 0 {
            let manager = self.clone();
            let target_meeting_id = meeting_id.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(grace_ms));
                manager.expire_soft_stop_if_matching(&target_meeting_id, epoch);
            });
        }

        Some((meeting_id, epoch))
    }

    pub(crate) fn expire_soft_stop_if_matching(
        &self,
        expected_meeting_id: &str,
        expected_epoch: u64,
    ) -> bool {
        let should_finalize = {
            let live = match self.inner.live_state.lock() {
                Ok(guard) => guard,
                Err(_) => return false,
            };
            if live.status != MeetingStatus::Paused
                || live.active_meeting_id.as_deref() != Some(expected_meeting_id)
            {
                return false;
            }
            let clock = self
                .inner
                .clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            clock
                .soft_stop
                .as_ref()
                .is_some_and(|deadline| deadline.epoch == expected_epoch)
        };

        if should_finalize {
            debug!(
                "call-end grace window expired for {} (epoch={}); auto-finalizing notes",
                expected_meeting_id, expected_epoch
            );
            self.stop_meeting().is_ok()
        } else {
            false
        }
    }

    /// Evaluates one poll tick of the active-meeting call watcher.
    /// Returns `true` if the meeting transitioned into `SoftStopped` on this tick.
    pub(crate) fn poll_active_call_watcher(&self) -> bool {
        let should_trigger = {
            let mut live = match self.inner.live_state.lock() {
                Ok(guard) => guard,
                Err(_) => return false,
            };
            if live.status != MeetingStatus::Recording || !live.auto_detect_enabled {
                return false;
            }
            let running_app = self.inner.call_detector.active_call_app();
            let mut clock = self
                .inner
                .clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            match running_app {
                Some(app) => {
                    clock.call_end_negative_polls = 0;
                    clock.auto_stop_suppressed_until_rejoin = false;
                    if clock.watched_call_bundle_id.is_none() {
                        clock.watched_call_bundle_id = Some(app.bundle_id);
                    }
                    if live.app_name.is_none() {
                        live.app_name = Some(app.app_name);
                    }
                    false
                }
                None => {
                    if clock.watched_call_bundle_id.is_some()
                        && !clock.auto_stop_suppressed_until_rejoin
                    {
                        clock.call_end_negative_polls =
                            clock.call_end_negative_polls.saturating_add(1);
                        clock.call_end_negative_polls >= CALL_END_NEGATIVE_POLLS_REQUIRED
                    } else {
                        clock.call_end_negative_polls = 0;
                        false
                    }
                }
            }
        };

        if should_trigger {
            self.soft_stop_on_call_end(lowyap_ports::DEFAULT_MEETING_AUTO_STOP_GRACE_MS)
                .is_some()
        } else {
            false
        }
    }

    pub fn update_live_rough_notes(&self, rough_notes: String) -> Result<(), String> {
        let meeting_id = self.active_meeting_ref()?;

        self.inner
            .store
            .update_rough_notes(&meeting_id, rough_notes.as_str())?;

        if let Ok(mut live) = self.inner.live_state.lock() {
            live.rough_notes = rough_notes;
        }

        Ok(())
    }

    pub fn stop_meeting(&self) -> Result<MeetingDetail, String> {
        // Keep `active_meeting` occupied (with `worker: None`) while joining the capture thread
        // so a concurrent `start_meeting` cannot open a second mic/tap before teardown completes.
        let (meeting_id, worker) = {
            let mut guard = self
                .inner
                .active_meeting
                .lock()
                .map_err(|_| "meeting state is unavailable".to_string())?;
            let active = guard
                .as_mut()
                .ok_or_else(|| "No active meeting session exists.".to_string())?;
            if active.stop_flag.swap(true, Ordering::AcqRel) {
                return Err("Meeting session is already stopping.".to_string());
            }
            (active.meeting_id.clone(), active.worker.take())
        };

        if let Ok(mut live) = self.inner.live_state.lock() {
            live.status = MeetingStatus::Summarizing;
            if let Ok(mut clock) = self.inner.clock.lock() {
                clock.soft_stop = None;
                clock.call_end_negative_polls = 0;
            }
        }
        emit_live_state(&self.inner, &self.get_live_state());

        if let Some(worker) = worker {
            let _ = worker.join();
        }

        if let Ok(mut guard) = self.inner.active_meeting.lock() {
            *guard = None;
        }

        let stop_result = (|| -> Result<MeetingDetail, String> {
            let elapsed_ms = self.get_live_state().elapsed_ms;
            let ended_at_ms = now_ms_i64();
            self.inner
                .store
                .finalize_meeting(&meeting_id, ended_at_ms, elapsed_ms)?;

            let detail_before = self.inner.store.get_meeting_detail(&meeting_id)?;
            let tail_segments = self.inner.store.get_tail_segments(&meeting_id, 160)?;

            // Hygiene guard: if a meeting ended with zero detected speech segments,
            // zero rollups, and zero user-typed rough notes, discard the empty row
            // instead of polluting the user's Meetings workspace with blank entries.
            if Self::should_discard_empty_meeting(
                &detail_before.segments,
                detail_before.rollups.len(),
                &detail_before.rough_notes,
            ) {
                log::info!(
                    "meeting: discarding empty meeting {} (0 spoken segments, 0 rollups, empty rough notes)",
                    meeting_id
                );
                let _ = self.inner.store.delete_meeting(&meeting_id);
                return Ok(detail_before);
            }

            let summary = self.generate_hybrid_summary(
                &detail_before.summary,
                &detail_before.rough_notes,
                &detail_before.rollups,
                &tail_segments,
            );

            self.inner.store.update_expanded_notes(
                &meeting_id,
                &summary.text,
                &summary.summary_source,
            )?;

            let fresh_detail = self.inner.store.get_meeting_detail(&meeting_id)?;
            if lowyap_summarize::is_placeholder_meeting_title(&fresh_detail.summary.title) {
                let mut rollup_strings = to_rollup_strings(&fresh_detail.rollups);
                if rollup_strings.is_empty()
                    && summary.summary_source != "local_extractive"
                    && !summary.text.trim().is_empty()
                {
                    rollup_strings.push(summary.text.clone());
                }
                let port_segments = to_port_segments(&fresh_detail.segments);
                let backends = build_production_completion_backends(self.inner.app_handle.as_ref());
                if let Some(generated_title) = lowyap_summarize::generate_meeting_title(
                    &fresh_detail.rough_notes,
                    &rollup_strings,
                    &port_segments,
                    backends.first().map(|b| b.as_ref()),
                ) {
                    let placeholder_title = fresh_detail.summary.title.clone();
                    let updated = self.inner.store.update_placeholder_title_if_unchanged(
                        &meeting_id,
                        &placeholder_title,
                        &generated_title,
                    )?;
                    if updated {
                        let old_h1 = format!("# {}", placeholder_title.trim());
                        let trimmed_md = summary.text.trim_start();
                        if let Some(rest) = trimmed_md.strip_prefix(&old_h1) {
                            if rest.is_empty() || rest.starts_with('\n') || rest.starts_with('\r') {
                                let synced_md = format!("# {generated_title}{rest}");
                                let _ = self.inner.store.update_expanded_notes(
                                    &meeting_id,
                                    &synced_md,
                                    &summary.summary_source,
                                );
                            }
                        }
                    }
                }
            }

            self.inner.store.get_meeting_detail(&meeting_id)
        })();

        if let Ok(mut live) = self.inner.live_state.lock() {
            let auto_detect_enabled = live.auto_detect_enabled;
            *live = Self::default_live_state();
            live.auto_detect_enabled = auto_detect_enabled;
        }
        emit_live_state(&self.inner, &self.get_live_state());

        stop_result
    }

    pub fn set_auto_detect_enabled(&self, enabled: bool) -> Result<bool, String> {
        let mut live = self
            .inner
            .live_state
            .lock()
            .map_err(|_| "live meeting state is unavailable".to_string())?;
        live.auto_detect_enabled = enabled;
        if !enabled {
            live.detected_call = None;
        }
        let state = live.clone();
        drop(live);
        emit_live_state(&self.inner, &state);
        Ok(enabled)
    }

    pub fn dismiss_detected_call(&self, call_id: Option<String>) -> Result<(), String> {
        self.inner
            .call_detector
            .dismiss_call(call_id.as_deref())
            .map_err(|err| format!("Failed to dismiss detected call: {err}"))?;

        if let Ok(mut live) = self.inner.live_state.lock() {
            if call_id.as_deref().is_none_or(|id| {
                live.detected_call
                    .as_ref()
                    .is_some_and(|call| call.call_id == id)
            }) {
                live.detected_call = None;
            }
        }

        emit_live_state(&self.inner, &self.get_live_state());

        Ok(())
    }

    pub fn list_meetings(&self, query: Option<&str>) -> Result<Vec<MeetingSummaryRecord>, String> {
        self.inner.store.list_meetings(query)
    }

    pub fn get_meeting_detail(&self, meeting_id: &str) -> Result<MeetingDetail, String> {
        self.inner.store.get_meeting_detail(meeting_id)
    }

    pub fn update_meeting_meta(
        &self,
        meeting_id: &str,
        title: Option<String>,
        template: Option<MeetingTemplate>,
        rough_notes: Option<String>,
    ) -> Result<MeetingDetail, String> {
        let existing = self.inner.store.get_meeting_detail(meeting_id)?;
        let next_title = title
            .map(|t| normalize_title(Some(t)))
            .unwrap_or(existing.summary.title);
        let next_template = template.unwrap_or(existing.summary.template);
        let next_rough = rough_notes.unwrap_or(existing.rough_notes);

        self.inner.store.update_meeting_meta(
            meeting_id,
            &next_title,
            next_template,
            &next_rough,
        )?;
        self.inner.store.get_meeting_detail(meeting_id)
    }

    pub fn regenerate_meeting_notes(
        &self,
        meeting_id: &str,
        template: Option<MeetingTemplate>,
    ) -> Result<MeetingDetail, String> {
        let mut detail = self.inner.store.get_meeting_detail(meeting_id)?;

        if let Some(next_template) = template {
            if next_template != detail.summary.template {
                self.inner.store.update_meeting_meta(
                    meeting_id,
                    &detail.summary.title,
                    next_template,
                    &detail.rough_notes,
                )?;
                detail = self.inner.store.get_meeting_detail(meeting_id)?;
            }
        }

        let tail_segments = self.inner.store.get_tail_segments(meeting_id, 160)?;
        let summary = self.generate_hybrid_summary(
            &detail.summary,
            &detail.rough_notes,
            &detail.rollups,
            &tail_segments,
        );

        self.inner.store.update_expanded_notes(
            meeting_id,
            &summary.text,
            &summary.summary_source,
        )?;

        self.inner.store.get_meeting_detail(meeting_id)
    }

    pub fn export_meeting_markdown(&self, meeting_id: &str) -> Result<String, String> {
        let detail = self.inner.store.get_meeting_detail(meeting_id)?;
        Ok(MeetingStore::format_meeting_markdown(&detail))
    }

    pub fn delete_meeting(&self, meeting_id: &str) -> Result<bool, String> {
        self.inner.store.delete_meeting(meeting_id)
    }

    fn active_meeting_ref(&self) -> Result<String, String> {
        let guard = self
            .inner
            .active_meeting
            .lock()
            .map_err(|_| "meeting state is unavailable".to_string())?;

        let active = guard
            .as_ref()
            .ok_or_else(|| "No active meeting session exists.".to_string())?;

        Ok(active.meeting_id.clone())
    }

    fn next_meeting_id(&self) -> String {
        let seq = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        format!("meeting-{}-{seq}", now_ms_i64())
    }

    fn generate_hybrid_summary(
        &self,
        summary: &MeetingSummaryRecord,
        rough_notes: &str,
        rollups: &[MeetingRollup],
        tail_segments: &[MeetingSegment],
    ) -> SummaryResult {
        if let Some(app) = self.inner.app_handle.as_ref() {
            let hindustani_script = crate::flow::multilingual::active_hindustani_script(app);
            tauri::async_runtime::block_on(generate_hybrid_meeting_notes(
                app,
                &summary.title,
                summary.template,
                rough_notes,
                rollups,
                tail_segments,
                hindustani_script,
            ))
        } else {
            generate_hybrid_meeting_notes_local(
                &summary.title,
                summary.template,
                rough_notes,
                rollups,
                tail_segments,
                HindustaniScript::Auto,
            )
        }
    }

    fn spawn_detector_loop(&self) {
        if self.inner.app_handle.is_none() {
            return;
        }

        let manager = self.clone();
        let inner = Arc::clone(&self.inner);
        let handle = thread::spawn(move || {
            while !inner.detector_stop.load(Ordering::Acquire) {
                let status = inner
                    .live_state
                    .lock()
                    .map(|g| g.status)
                    .unwrap_or(MeetingStatus::Idle);

                if status == MeetingStatus::Recording {
                    let _ = manager.poll_active_call_watcher();
                    thread::sleep(DETECTOR_ACTIVE_POLL_INTERVAL);
                    continue;
                }

                if should_skip_detection(&inner) {
                    thread::sleep(DETECTOR_IDLE_POLL_INTERVAL);
                    continue;
                }

                match inner.call_detector.poll_detected_call() {
                    Ok(Some(call)) => {
                        let mut emit_event = false;
                        if let Ok(mut live) = inner.live_state.lock() {
                            if live.status == MeetingStatus::Idle && live.auto_detect_enabled {
                                emit_event = live.detected_call.as_ref() != Some(&call);
                                live.detected_call = Some(call.clone());
                            }
                        }

                        if emit_event {
                            if let Some(app) = inner.app_handle.as_ref() {
                                let _ = app.emit("call-detected", &call);
                            }
                            sync_session_bus(&inner);
                        }
                    }
                    Ok(None) => {
                        if let Ok(mut live) = inner.live_state.lock() {
                            live.detected_call = None;
                        }
                        sync_session_bus(&inner);
                    }
                    Err(err) => {
                        warn!("call detector poll failed: {err}");
                    }
                }

                thread::sleep(DETECTOR_IDLE_POLL_INTERVAL);
            }
        });

        if let Ok(mut guard) = self.inner.detector_handle.lock() {
            *guard = Some(handle);
        }
    }
}

fn should_skip_detection(inner: &MeetingSessionManagerInner) -> bool {
    let live = match inner.live_state.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return true,
    };

    if !live.auto_detect_enabled {
        return true;
    }

    if live.status != MeetingStatus::Idle {
        return true;
    }

    if let Some(audio_manager) = inner.audio_manager.as_ref() {
        if audio_manager.is_recording() {
            return true;
        }
    }

    false
}

fn run_capture_worker(
    inner: Arc<MeetingSessionManagerInner>,
    mut capture: Box<dyn crate::meeting::types::ActiveDualCapture>,
    meeting_id: String,
    stop_flag: Arc<AtomicBool>,
    paused_flag: Arc<AtomicBool>,
    started_at_ms: i64,
    expected_attendees: Vec<String>,
) {
    let capture_mode = capture.capture_mode();
    let mut echo_canceller = (inner.echo_canceller_factory)();
    let mut segmenter = (inner.segmenter_factory)(inner.app_handle.as_ref());
    let mut diarizer = (inner.diarizer_factory)(capture_mode);
    let mut name_resolver =
        lowyap_diarize::NameCueResolver::with_expected_attendees(expected_attendees);
    let mut echo_dedup = EchoDeduplicator::default();
    let mut me_only_enroll_buf: Vec<f32> = Vec::with_capacity(8_000);
    let mut me_samples_seen: u64 = 0;
    let mut others_samples_seen: u64 = 0;
    let mut was_paused = false;
    let mut next_rollup_end_ms = ROLLUP_WINDOW_MS;
    let mut rollup_index: u32 = 0;

    debug!(
        "meeting capture worker started for {} (echo_canceller={}, segmenter={}, diarizer={})",
        meeting_id,
        echo_canceller.name(),
        segmenter.name(),
        diarizer.name()
    );

    loop {
        if stop_flag.load(Ordering::Acquire) {
            break;
        }

        let chunk = capture.drain_16k_frames();
        if paused_flag.load(Ordering::Acquire) {
            // Drain and drop while paused so meeting elapsed clock stays clean
            // and queued PCM doesn't explode memory; flush any pre-pause speech once.
            if !was_paused {
                was_paused = true;
                if me_only_enroll_buf.len() >= 3_200 {
                    diarizer.observe_me_clean(&me_only_enroll_buf);
                }
                me_only_enroll_buf.clear();
                for span in segmenter.flush() {
                    persist_utterance_span(
                        &inner,
                        &meeting_id,
                        &span,
                        capture_mode,
                        chunk.output_route,
                        diarizer.as_mut(),
                        &mut name_resolver,
                        &mut echo_dedup,
                    );
                }
                echo_canceller.reset();
            }
            if chunk.me_16k.is_empty() && chunk.others_16k.is_empty() {
                thread::sleep(Duration::from_millis(120));
            }
            continue;
        }
        was_paused = false;

        if chunk.me_16k.is_empty() && chunk.others_16k.is_empty() {
            thread::sleep(Duration::from_millis(120));
            continue;
        }

        if let Ok(mut live) = inner.live_state.lock() {
            live.output_route = chunk.output_route;
        }

        feed_dual_chunk(
            &inner,
            &meeting_id,
            capture_mode,
            &chunk,
            echo_canceller.as_mut(),
            segmenter.as_mut(),
            diarizer.as_mut(),
            &mut name_resolver,
            &mut echo_dedup,
            &mut me_only_enroll_buf,
            &mut me_samples_seen,
            &mut others_samples_seen,
        );

        let elapsed_ms = samples_to_ms(me_samples_seen.max(others_samples_seen));
        update_elapsed(&inner.live_state, elapsed_ms);
        emit_due_rollups(
            &inner,
            &meeting_id,
            elapsed_ms,
            &mut rollup_index,
            &mut next_rollup_end_ms,
        );
    }

    // Stop hardware capture first so no new frames enter the rings, then drain
    // any unread frames left in the capture rings before final utterance flush.
    capture.stop();
    if !paused_flag.load(Ordering::Acquire) {
        for _ in 0..8 {
            let tail_chunk = capture.drain_16k_frames();
            if tail_chunk.me_16k.is_empty() && tail_chunk.others_16k.is_empty() {
                break;
            }
            feed_dual_chunk(
                &inner,
                &meeting_id,
                capture_mode,
                &tail_chunk,
                echo_canceller.as_mut(),
                segmenter.as_mut(),
                diarizer.as_mut(),
                &mut name_resolver,
                &mut echo_dedup,
                &mut me_only_enroll_buf,
                &mut me_samples_seen,
                &mut others_samples_seen,
            );
        }
    }

    if me_only_enroll_buf.len() >= 3_200 {
        diarizer.observe_me_clean(&me_only_enroll_buf);
    }
    me_only_enroll_buf.clear();

    let final_route = capture.output_route();
    for span in segmenter.flush() {
        persist_utterance_span(
            &inner,
            &meeting_id,
            &span,
            capture_mode,
            final_route,
            diarizer.as_mut(),
            &mut name_resolver,
            &mut echo_dedup,
        );
    }

    let total_elapsed_ms = samples_to_ms(me_samples_seen.max(others_samples_seen));
    update_elapsed(&inner.live_state, total_elapsed_ms);
    emit_due_rollups(
        &inner,
        &meeting_id,
        total_elapsed_ms,
        &mut rollup_index,
        &mut next_rollup_end_ms,
    );

    debug!(
        "meeting worker completed for {} (started_at_ms={}, elapsed_ms={})",
        meeting_id, started_at_ms, total_elapsed_ms
    );
}

fn emit_due_rollups(
    inner: &MeetingSessionManagerInner,
    meeting_id: &str,
    elapsed_ms: u64,
    rollup_index: &mut u32,
    next_rollup_end_ms: &mut u64,
) {
    while elapsed_ms >= *next_rollup_end_ms {
        let rollup_start = *next_rollup_end_ms - ROLLUP_WINDOW_MS;
        let segments =
            match inner
                .store
                .get_segments_in_range(meeting_id, rollup_start, *next_rollup_end_ms)
            {
                Ok(segments) => segments,
                Err(err) => {
                    warn!("failed to load segments for rollup window: {err}");
                    Vec::new()
                }
            };

        let summary = if let Some(app) = inner.app_handle.as_ref() {
            let script = crate::flow::multilingual::active_hindustani_script(app);
            tauri::async_runtime::block_on(summarize_five_min_window(app, &segments, script))
        } else {
            summarize_five_min_window_local(&segments)
        };

        if let Err(err) = inner.store.upsert_rollup(
            meeting_id,
            *rollup_index,
            rollup_start,
            *next_rollup_end_ms,
            &summary.text,
        ) {
            warn!("failed to persist rollup window {rollup_index}: {err}");
        } else {
            *rollup_index = rollup_index.saturating_add(1);
            if let Ok(mut live) = inner.live_state.lock() {
                live.rollup_count = *rollup_index;
            }
        }

        *next_rollup_end_ms = next_rollup_end_ms.saturating_add(ROLLUP_WINDOW_MS);
    }
}

#[allow(clippy::too_many_arguments)]
fn feed_dual_chunk(
    inner: &MeetingSessionManagerInner,
    meeting_id: &str,
    capture_mode: AudioCaptureMode,
    chunk: &crate::meeting::types::DualPcmChunk,
    echo_canceller: &mut dyn lowyap_ports::EchoCanceller,
    segmenter: &mut dyn lowyap_ports::UtteranceSegmenter,
    diarizer: &mut dyn lowyap_ports::SpeakerDiarizer,
    name_resolver: &mut lowyap_diarize::NameCueResolver,
    echo_dedup: &mut EchoDeduplicator,
    me_only_enroll_buf: &mut Vec<f32>,
    me_samples_seen: &mut u64,
    others_samples_seen: &mut u64,
) {
    let port_route: lowyap_ports::OutputRouteKind = chunk.output_route.into();
    let others_rms = rms_energy(&chunk.others_16k);
    let cleaned_me = if !chunk.me_16k.is_empty() {
        Some(echo_canceller.process_16k(&chunk.me_16k, &chunk.others_16k, port_route))
    } else {
        None
    };

    // Advance `Others` first on each dual chunk so overlapping remote utterances
    // are visible to `EchoDeduplicator` immediately when a `Me` bleed span closes.
    if !chunk.others_16k.is_empty() {
        let start_ms = samples_to_ms(*others_samples_seen);
        *others_samples_seen = others_samples_seen.saturating_add(chunk.others_16k.len() as u64);
        for span in segmenter.push_chunk(
            lowyap_ports::SpeakerChannel::Others,
            &chunk.others_16k,
            start_ms,
        ) {
            persist_utterance_span(
                inner,
                meeting_id,
                &span,
                capture_mode,
                chunk.output_route,
                diarizer,
                name_resolver,
                echo_dedup,
            );
        }
    }

    if let Some(cleaned_me) = cleaned_me {
        let me_rms = rms_energy(&cleaned_me);
        if others_rms < 0.004 && me_rms >= 0.012 {
            me_only_enroll_buf.extend_from_slice(&cleaned_me);
            if me_only_enroll_buf.len() >= 8_000 {
                diarizer.observe_me_clean(me_only_enroll_buf);
                me_only_enroll_buf.clear();
            }
        } else if others_rms >= 0.006 {
            if me_only_enroll_buf.len() >= 3_200 {
                diarizer.observe_me_clean(me_only_enroll_buf);
            }
            me_only_enroll_buf.clear();
        }
        let start_ms = samples_to_ms(*me_samples_seen);
        *me_samples_seen = me_samples_seen.saturating_add(cleaned_me.len() as u64);
        for span in segmenter.push_chunk(lowyap_ports::SpeakerChannel::Me, &cleaned_me, start_ms) {
            persist_utterance_span(
                inner,
                meeting_id,
                &span,
                capture_mode,
                chunk.output_route,
                diarizer,
                name_resolver,
                echo_dedup,
            );
        }
    }
}

#[derive(Debug, Clone)]
struct RecentDedupeEntry {
    segment_id: Option<i64>,
    start_ms: u64,
    end_ms: u64,
    tokens: Vec<String>,
}

/// Cross-channel transcript deduplicator that catches residual loudspeaker bleed
/// when room reflections or CoreAudio route delays slip past linear/nonlinear AEC.
#[derive(Debug, Default)]
struct EchoDeduplicator {
    recent_others: Vec<RecentDedupeEntry>,
    recent_me: Vec<RecentDedupeEntry>,
}

impl EchoDeduplicator {
    const WINDOW_MS: u64 = 18_000;
    const OVERLAP_SLACK_MS: u64 = 2_600;

    fn normalize_tokens(text: &str) -> Vec<String> {
        let roman = lowyap_flow::hindustani_to_roman_hinglish(text);
        roman
            .split(|c: char| !c.is_alphanumeric())
            .map(|tok| tok.trim().to_ascii_lowercase())
            .filter(|tok| tok.chars().count() >= 2)
            .collect()
    }

    fn consonant_skeleton(token: &str) -> String {
        let mut out = String::with_capacity(token.len());
        let mut prev: Option<char> = None;
        for ch in token.chars() {
            if !ch.is_ascii_alphabetic() {
                continue;
            }
            let mapped = match ch {
                'a' | 'e' | 'i' | 'o' | 'u' | 'w' | 'y' | 'h' => continue,
                'v' => 'f',
                'q' => 'k',
                'c' => 's',
                'z' => 's',
                other => other,
            };
            if prev != Some(mapped) {
                out.push(mapped);
                prev = Some(mapped);
            }
        }
        out
    }

    fn windows_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
        let expanded_b_start = b_start.saturating_sub(Self::OVERLAP_SLACK_MS);
        let expanded_b_end = b_end.saturating_add(Self::OVERLAP_SLACK_MS);
        a_start <= expanded_b_end && a_end >= expanded_b_start
    }

    /// Fraction of `candidate_me` tokens explained by `others` tokens.
    fn me_containment_in_others(me_tokens: &[String], others_tokens: &[String]) -> f32 {
        if me_tokens.is_empty() || others_tokens.is_empty() {
            return 0.0;
        }
        let matched = me_tokens
            .iter()
            .filter(|mt| {
                let mt_skel = Self::consonant_skeleton(mt);
                others_tokens.iter().any(|ot| {
                    if ot == *mt {
                        return true;
                    }
                    if mt.len() >= 4 && ot.len() >= 4 && lowyap_flow::is_accent_phonetic_match(mt, ot) {
                        return true;
                    }
                    if mt_skel.len() >= 4 {
                        let ot_skel = Self::consonant_skeleton(ot);
                        if ot_skel == mt_skel {
                            return true;
                        }
                    }
                    false
                })
            })
            .count();
        matched as f32 / me_tokens.len() as f32
    }

    fn is_bleed_match(me_tokens: &[String], others_tokens: &[String]) -> bool {
        let containment = Self::me_containment_in_others(me_tokens, others_tokens);
        if me_tokens.len() == 1 {
            me_tokens[0].len() >= 5 && containment >= 1.0
        } else {
            containment >= 0.65
        }
    }

    fn prune(&mut self, now_end_ms: u64) {
        let min_keep = now_end_ms.saturating_sub(Self::WINDOW_MS);
        self.recent_others.retain(|e| e.end_ms >= min_keep);
        self.recent_me.retain(|e| e.end_ms >= min_keep);
    }

    fn should_drop_me(&mut self, start_ms: u64, end_ms: u64, text: &str) -> bool {
        self.prune(end_ms);
        let me_tokens = Self::normalize_tokens(text);
        if me_tokens.is_empty() {
            return false;
        }
        self.recent_others.iter().any(|other| {
            Self::windows_overlap(start_ms, end_ms, other.start_ms, other.end_ms)
                && Self::is_bleed_match(&me_tokens, &other.tokens)
        })
    }

    fn record_me(&mut self, segment_id: i64, start_ms: u64, end_ms: u64, text: &str) {
        let tokens = Self::normalize_tokens(text);
        if !tokens.is_empty() {
            self.recent_me.push(RecentDedupeEntry {
                segment_id: Some(segment_id),
                start_ms,
                end_ms,
                tokens,
            });
        }
    }

    /// Records a newly completed `Others` utterance and returns IDs of any
    /// earlier `Me` fragments that were actually echo-bleed subsets of it.
    fn record_others_and_collect_bleed_me_ids(
        &mut self,
        start_ms: u64,
        end_ms: u64,
        text: &str,
    ) -> Vec<i64> {
        self.prune(end_ms);
        let others_tokens = Self::normalize_tokens(text);
        if others_tokens.is_empty() {
            return Vec::new();
        }
        let mut stale_ids = Vec::new();
        self.recent_me.retain(|me| {
            let is_bleed = Self::windows_overlap(me.start_ms, me.end_ms, start_ms, end_ms)
                && Self::is_bleed_match(&me.tokens, &others_tokens);
            if is_bleed {
                if let Some(id) = me.segment_id {
                    stale_ids.push(id);
                }
            }
            !is_bleed
        });
        self.recent_others.push(RecentDedupeEntry {
            segment_id: None,
            start_ms,
            end_ms,
            tokens: others_tokens,
        });
        stale_ids
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_utterance_span(
    inner: &MeetingSessionManagerInner,
    meeting_id: &str,
    span: &lowyap_ports::UtteranceSpan,
    capture_mode: AudioCaptureMode,
    output_route: OutputRouteKind,
    diarizer: &mut dyn lowyap_ports::SpeakerDiarizer,
    name_resolver: &mut lowyap_diarize::NameCueResolver,
    echo_dedup: &mut EchoDeduplicator,
) {
    if span.samples_16k.is_empty() || rms_energy(&span.samples_16k) < SILENCE_RMS_THRESHOLD {
        return;
    }

    let text = match inner.transcriber.transcribe_chunk(&span.samples_16k) {
        Ok(text) => text,
        Err(err) => {
            warn!("failed to persist transcript segment: {err}");
            return;
        }
    };
    let normalized = if let Some(app) = inner.app_handle.as_ref() {
        let app_settings = crate::settings::get_settings(app);
        let script_pref = crate::flow::multilingual::active_hindustani_script(app);
        normalize_meeting_segment_script(text.trim(), &app_settings.spoken_languages, script_pref)
    } else {
        text.trim().to_owned()
    };
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return;
    }

    let dedup_active =
        capture_mode == AudioCaptureMode::DualStream && output_route != OutputRouteKind::Headphones;
    if dedup_active
        && span.channel == lowyap_ports::SpeakerChannel::Me
        && echo_dedup.should_drop_me(span.start_ms, span.end_ms, trimmed)
    {
        debug!(
            "suppressed loudspeaker echo-bleed duplicate on Me ({}..{} ms): {:?}",
            span.start_ms, span.end_ms, trimmed
        );
        return;
    }

    let identity = diarizer.identify(span);
    let prev_remote_count = name_resolver.remote_speaker_count();
    let was_unbound = name_resolver.bound_name(&identity.speaker_id).is_none();
    name_resolver.observe_transcript_turn(
        identity.channel,
        &identity.speaker_id,
        span.start_ms,
        span.end_ms,
        trimmed,
    );
    let newly_bound = was_unbound && name_resolver.bound_name(&identity.speaker_id).is_some();
    let sole_hint_revoked = prev_remote_count == 1 && name_resolver.remote_speaker_count() == 2;
    if sole_hint_revoked {
        let mut to_reset: Vec<(String, String)> = Vec::new();
        if let Ok(mut live) = inner.live_state.lock() {
            for existing in &mut live.recent_segments {
                if let Some(sid) = existing.speaker_id.as_deref() {
                    if sid != "me" && name_resolver.bound_name(sid).is_none() {
                        let clean_default = existing
                            .speaker_label
                            .as_deref()
                            .and_then(|lbl| lbl.split(" (").next())
                            .unwrap_or("Speaker 1")
                            .to_owned();
                        existing.speaker_label = Some(clean_default.clone());
                        if !to_reset.iter().any(|(seen, _)| seen == sid) {
                            to_reset.push((sid.to_owned(), clean_default));
                        }
                    }
                }
            }
        }
        for (sid, clean_default) in to_reset {
            let _ = inner
                .store
                .backfill_speaker_label(meeting_id, &sid, &clean_default);
        }
    }
    let resolved_label =
        name_resolver.display_label_for(&identity.speaker_id, &identity.display_label);
    let speaker = if identity.speaker_id == "me" {
        SpeakerChannel::Me
    } else if identity.speaker_id.is_empty() {
        identity.channel.into()
    } else {
        SpeakerChannel::Others
    };

    if newly_bound && !identity.speaker_id.is_empty() {
        let _ =
            inner
                .store
                .backfill_speaker_label(meeting_id, &identity.speaker_id, &resolved_label);
        if let Ok(mut live) = inner.live_state.lock() {
            for existing in &mut live.recent_segments {
                if existing.speaker_id.as_deref() == Some(identity.speaker_id.as_str()) {
                    existing.speaker_label = Some(resolved_label.clone());
                }
            }
        }
    }

    match inner.store.append_segment_attributed(
        meeting_id,
        speaker,
        span.start_ms,
        span.end_ms,
        trimmed,
        None,
        Some(&identity.speaker_id),
        Some(&resolved_label),
    ) {
        Ok(segment) => {
            if dedup_active {
                match speaker {
                    SpeakerChannel::Me => {
                        echo_dedup.record_me(segment.id, span.start_ms, span.end_ms, trimmed);
                    }
                    SpeakerChannel::Others => {
                        let bleed_ids = echo_dedup.record_others_and_collect_bleed_me_ids(
                            span.start_ms,
                            span.end_ms,
                            trimmed,
                        );
                        if !bleed_ids.is_empty() {
                            for id in &bleed_ids {
                                let _ = inner.store.delete_segment(meeting_id, *id);
                            }
                            if let Ok(mut live) = inner.live_state.lock() {
                                live.recent_segments
                                    .retain(|seg| !bleed_ids.contains(&seg.id));
                            }
                        }
                    }
                }
            }
            push_recent_segment(inner, segment);
        }
        Err(err) => warn!("failed to append transcript segment: {err}"),
    }
}

fn push_recent_segment(inner: &MeetingSessionManagerInner, segment: MeetingSegment) {
    let snapshot = if let Ok(mut live) = inner.live_state.lock() {
        live.recent_segments.push(segment);
        if live.recent_segments.len() > RECENT_SEGMENTS_CAP {
            let extra = live.recent_segments.len() - RECENT_SEGMENTS_CAP;
            live.recent_segments.drain(0..extra);
        }
        Some(live.clone())
    } else {
        None
    };

    if let Some(state) = snapshot {
        emit_live_state(inner, &state);
    }
}

fn emit_live_state(inner: &MeetingSessionManagerInner, state: &MeetingLiveState) {
    if let Some(app) = inner.app_handle.as_ref() {
        sync_session_bus(inner);
        let _ = app.emit("meeting-state-changed", state);
    }
}

fn sync_session_bus(inner: &MeetingSessionManagerInner) {
    let Some(app) = inner.app_handle.as_ref() else {
        return;
    };
    crate::session_bus::bus().sync_meeting(app, || {
        use lowyap_ports::{AudioCaptureMode as PortCaptureMode, DetectedCallBanner, MeetingPhase};
        let live = inner.live_state.lock().ok()?;
        let clock = inner.clock.lock().ok()?;
        let capture_mode = match live.capture_mode {
            AudioCaptureMode::DualStream => PortCaptureMode::DualStream,
            AudioCaptureMode::MicOnlyFallback => PortCaptureMode::MicOnlyFallback,
        };
        let phase = match live.status {
            MeetingStatus::Idle => MeetingPhase::Idle,
            MeetingStatus::Recording => MeetingPhase::Recording {
                meeting_id: live.active_meeting_id.clone()?,
                title: live.title.clone(),
                started_at_ms: clock.started_at_ms,
                accumulated_paused_ms: clock.accumulated_paused_ms,
                app_name: live.app_name.clone(),
                capture_mode,
            },
            MeetingStatus::Paused => {
                if let Some(soft_stop) = clock.soft_stop.as_ref() {
                    MeetingPhase::SoftStopped {
                        meeting_id: live.active_meeting_id.clone()?,
                        title: live.title.clone(),
                        started_at_ms: clock.started_at_ms,
                        stopped_at_ms: soft_stop.stopped_at_ms,
                        expires_at_ms: soft_stop.expires_at_ms,
                        grace_ms: soft_stop.grace_ms,
                        accumulated_paused_ms: clock.accumulated_paused_ms,
                        app_name: live.app_name.clone(),
                        capture_mode,
                    }
                } else {
                    MeetingPhase::Paused {
                        meeting_id: live.active_meeting_id.clone()?,
                        title: live.title.clone(),
                        started_at_ms: clock.started_at_ms,
                        paused_at_ms: clock.paused_at_ms?,
                        accumulated_paused_ms: clock.accumulated_paused_ms,
                        app_name: live.app_name.clone(),
                        capture_mode,
                    }
                }
            }
            MeetingStatus::Summarizing => MeetingPhase::Summarizing {
                meeting_id: live.active_meeting_id.clone()?,
                title: live.title.clone(),
            },
        };
        let call = live.detected_call.as_ref().map(|call| DetectedCallBanner {
            call_id: call.call_id.clone(),
            app_name: call.app_name.clone(),
            output_route: call.output_route.into(),
        });
        Some((phase, call))
    });
}

fn update_elapsed(live_state: &Mutex<MeetingLiveState>, elapsed_ms: u64) {
    if let Ok(mut live) = live_state.lock() {
        live.elapsed_ms = elapsed_ms;
    }
}

fn samples_to_ms(samples: u64) -> u64 {
    samples.saturating_mul(1_000) / SAMPLE_RATE
}

fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_sq: f32 = samples.iter().map(|sample| sample * sample).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn normalize_title(title: Option<String>) -> String {
    let raw = title.unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        format!("Meeting {}", Local::now().format("%Y-%m-%d %H:%M"))
    } else {
        trimmed.to_string()
    }
}

fn now_ms_i64() -> i64 {
    let now = std::time::SystemTime::now();
    let ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis();
    i64::try_from(ms).unwrap_or(i64::MAX)
}

fn format_mm_ss(ms: u64) -> String {
    let total_sec = ms / 1_000;
    let minutes = total_sec / 60;
    let seconds = total_sec % 60;
    format!("{minutes:02}:{seconds:02}")
}

impl MeetingSessionManager {
    /// Returns `true` when a stopped meeting has zero real spoken words (ignoring
    /// synthetic `[Paused ...]` markers), zero rollups, and empty rough notes,
    /// so we discard it instead of saving a blank "Meeting YYYY-MM-DD" entry.
    pub(crate) fn should_discard_empty_meeting(
        segments: &[MeetingSegment],
        rollup_count: usize,
        rough_notes: &str,
    ) -> bool {
        let has_spoken_segment = segments.iter().any(|seg| {
            let trimmed = seg.text.trim();
            !trimmed.is_empty() && !(trimmed.starts_with("[Paused") && trimmed.ends_with(']'))
        });
        !has_spoken_segment && rollup_count == 0 && rough_notes.trim().is_empty()
    }
}

/// Ensures single-script Roman Hinglish + English consistency in bilingual
/// `en` + `ur`/`hi` meetings when `script_pref` is `Auto` or `RomanHinglish`
/// (unless the user explicitly selected `UrduPersoArabic`, `Devanagari`, or `"ar"`).
pub(crate) fn normalize_meeting_segment_script(
    text: &str,
    spoken_languages: &[String],
    script_pref: HindustaniScript,
) -> String {
    if text.is_empty() {
        return String::new();
    }
    let has_ar = spoken_languages
        .iter()
        .any(|l| l.eq_ignore_ascii_case("ar"));
    let has_en = spoken_languages
        .iter()
        .any(|l| l.eq_ignore_ascii_case("en"));
    if has_ar {
        let normalized =
            lowyap_flow::normalize_script_output(text, spoken_languages, script_pref.into());
        return if has_en {
            lowyap_flow::restore_english_loanwords_in_perso_arabic(&normalized)
        } else {
            normalized
        };
    }
    let has_hindustani = spoken_languages
        .iter()
        .any(|l| l.eq_ignore_ascii_case("ur") || l.eq_ignore_ascii_case("hi"));

    match script_pref {
        HindustaniScript::RomanHinglish => lowyap_flow::hindustani_to_roman_hinglish(text),
        HindustaniScript::Auto if has_en && has_hindustani => {
            lowyap_flow::hindustani_to_roman_preserving_arabic(text)
        }
        _ => lowyap_flow::normalize_script_output(text, spoken_languages, script_pref.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::adapters::WINDOW_SAMPLES;
    use crate::meeting::types::{ActiveDualCapture, DualPcmChunk};
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use tempfile::tempdir;

    #[derive(Default)]
    struct MockChunkTranscriber;

    impl ChunkTranscriber for MockChunkTranscriber {
        fn transcribe_chunk(&self, samples: &[f32]) -> Result<String, String> {
            if rms_energy(samples) < SILENCE_RMS_THRESHOLD {
                return Ok(String::new());
            }
            let mean = samples.iter().copied().sum::<f32>() / samples.len() as f32;
            if mean >= 0.0 {
                Ok("Me discussed roadmap and will ship Friday".to_string())
            } else {
                Ok("Others need to validate blockers".to_string())
            }
        }
    }

    #[derive(Default, Clone)]
    struct SharedQueue {
        chunks: Arc<Mutex<VecDeque<DualPcmChunk>>>,
        stopped: Arc<AtomicBool>,
    }

    impl SharedQueue {
        fn push(&self, chunk: DualPcmChunk) {
            if let Ok(mut guard) = self.chunks.lock() {
                guard.push_back(chunk);
            }
        }
    }

    struct MockActiveCapture {
        queue: SharedQueue,
        capture_mode: AudioCaptureMode,
        output_route: OutputRouteKind,
    }

    impl ActiveDualCapture for MockActiveCapture {
        fn capture_mode(&self) -> AudioCaptureMode {
            self.capture_mode
        }

        fn output_route(&self) -> OutputRouteKind {
            self.output_route
        }

        fn drain_16k_frames(&mut self) -> DualPcmChunk {
            self.queue
                .chunks
                .lock()
                .ok()
                .and_then(|mut guard| guard.pop_front())
                .unwrap_or_else(|| DualPcmChunk {
                    me_16k: Vec::new(),
                    others_16k: Vec::new(),
                    output_route: self.output_route,
                })
        }

        fn stop(&mut self) {
            self.queue.stopped.store(true, Ordering::Release);
        }
    }

    #[derive(Clone)]
    struct MockBackend {
        queue: SharedQueue,
        capture_mode: AudioCaptureMode,
        output_route: OutputRouteKind,
    }

    impl MockBackend {
        fn new(capture_mode: AudioCaptureMode, output_route: OutputRouteKind) -> Self {
            Self {
                queue: SharedQueue::default(),
                capture_mode,
                output_route,
            }
        }

        fn push_chunk(&self, chunk: DualPcmChunk) {
            self.queue.push(chunk);
        }

        fn stopped(&self) -> bool {
            self.queue.stopped.load(Ordering::Acquire)
        }
    }

    impl DualAudioCaptureBackend for MockBackend {
        fn start_capture(&self) -> Result<Box<dyn ActiveDualCapture>, String> {
            Ok(Box::new(MockActiveCapture {
                queue: self.queue.clone(),
                capture_mode: self.capture_mode,
                output_route: self.output_route,
            }))
        }
    }

    fn wait_until<F>(timeout_ms: u64, mut predicate: F)
    where
        F: FnMut() -> bool,
    {
        let mut waited = 0;
        while waited < timeout_ms {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
            waited += 20;
        }
    }

    fn make_manager(
        backend: Arc<MockBackend>,
    ) -> (MeetingSessionManager, MeetingStore, tempfile::TempDir) {
        let dir = tempdir().expect("tempdir");
        let store = MeetingStore::open_in_path(dir.path().join("meetings.db")).expect("store");
        let capture_backend: Arc<dyn DualAudioCaptureBackend> = backend.clone();
        let manager = MeetingSessionManager::new_for_tests(
            store.clone(),
            capture_backend,
            Arc::new(MockChunkTranscriber),
            Arc::new(NoopCallDetector),
        );
        (manager, store, dir)
    }

    fn speech_chunk(me: f32, others: f32) -> DualPcmChunk {
        DualPcmChunk {
            me_16k: vec![me; WINDOW_SAMPLES],
            others_16k: vec![others; WINDOW_SAMPLES],
            output_route: OutputRouteKind::Headphones,
        }
    }

    #[test]
    fn should_persist_dual_channel_segments_for_me_and_others() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager, _store, _dir) = make_manager(Arc::clone(&backend));

        let live = manager
            .start_meeting(None, Some(MeetingTemplate::General))
            .expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");

        backend.push_chunk(speech_chunk(0.8, -0.8));

        wait_until(1_000, || {
            manager
                .get_meeting_detail(&meeting_id)
                .map(|detail| detail.segments.len() >= 2)
                .unwrap_or(false)
        });

        let detail = manager.stop_meeting().expect("stop");
        assert!(detail
            .segments
            .iter()
            .any(|segment| segment.speaker == SpeakerChannel::Me));
        assert!(detail
            .segments
            .iter()
            .any(|segment| segment.speaker == SpeakerChannel::Others));
        assert!(backend.stopped());
    }

    #[test]
    fn should_skip_near_silence_segments() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager, _store, _dir) = make_manager(Arc::clone(&backend));

        let live = manager.start_meeting(None, None).expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");

        backend.push_chunk(DualPcmChunk {
            me_16k: vec![0.0001; WINDOW_SAMPLES],
            others_16k: vec![0.0001; WINDOW_SAMPLES],
            output_route: OutputRouteKind::Headphones,
        });

        wait_until(300, || {
            manager
                .get_meeting_detail(&meeting_id)
                .map(|detail| !detail.segments.is_empty())
                .unwrap_or(false)
        });

        let detail = manager.stop_meeting().expect("stop");
        assert!(detail.segments.is_empty());
        let saved = manager.list_meetings(None).expect("list");
        assert!(
            saved.is_empty(),
            "zero-word silence meeting must be discarded instead of saved in meetings.db"
        );
    }

    #[test]
    fn should_preserve_zero_audio_meeting_when_user_typed_rough_notes() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager, _store, _dir) = make_manager(Arc::clone(&backend));

        let live = manager.start_meeting(None, None).expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");
        manager
            .update_live_rough_notes("- Action item typed manually".to_string())
            .expect("rough notes");

        let _detail = manager.stop_meeting().expect("stop");
        let saved = manager.list_meetings(None).expect("list");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, meeting_id);
    }

    #[test]
    fn should_transition_pause_resume_and_insert_pause_marker() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager, _store, _dir) = make_manager(Arc::clone(&backend));

        let live = manager.start_meeting(None, None).expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");

        let paused = manager.pause_meeting().expect("pause");
        assert_eq!(paused.status, MeetingStatus::Paused);

        let resumed = manager.resume_meeting().expect("resume");
        assert_eq!(resumed.status, MeetingStatus::Recording);

        let detail = manager.stop_meeting().expect("stop");
        let paused_marker = detail
            .segments
            .iter()
            .find(|segment| segment.text.contains("[Paused"))
            .map(|segment| segment.text.clone());
        assert!(paused_marker.is_some(), "pause marker should be persisted");

        assert_eq!(detail.summary.id, meeting_id);
    }

    #[test]
    fn should_create_rollups_every_five_minutes_of_elapsed_audio() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager, _store, _dir) = make_manager(Arc::clone(&backend));

        let live = manager.start_meeting(None, None).expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");

        for _ in 0..30 {
            backend.push_chunk(speech_chunk(0.9, 0.0));
        }

        wait_until(2_000, || manager.get_live_state().rollup_count >= 1);

        let detail = manager.stop_meeting().expect("stop");
        assert!(
            !detail.rollups.is_empty(),
            "expected at least one 5-minute rollup for meeting {meeting_id}"
        );
    }

    #[test]
    fn recent_segments_should_remain_bounded_to_cap() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager, _store, _dir) = make_manager(Arc::clone(&backend));

        manager.start_meeting(None, None).expect("start");

        for _ in 0..50 {
            backend.push_chunk(speech_chunk(0.7, 0.0));
        }

        wait_until(2_000, || {
            manager.get_live_state().recent_segments.len() >= RECENT_SEGMENTS_CAP
        });

        let live = manager.get_live_state();
        assert!(live.recent_segments.len() <= RECENT_SEGMENTS_CAP);

        let _ = manager.stop_meeting();
    }

    #[test]
    fn stop_meeting_should_generate_hybrid_summary() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager, _store, _dir) = make_manager(Arc::clone(&backend));

        manager
            .start_meeting(Some("Client call".to_string()), None)
            .expect("start");
        manager
            .update_live_rough_notes("- need to send recap\n- let's confirm owner".to_string())
            .expect("rough");
        backend.push_chunk(speech_chunk(0.8, -0.8));

        wait_until(800, || !manager.get_live_state().recent_segments.is_empty());

        let detail = manager.stop_meeting().expect("stop");
        assert!(!detail.expanded_notes_md.trim().is_empty());
        assert!(detail.expanded_notes_md.contains("## "));
        assert_eq!(detail.summary_source, "local_extractive");
        assert_eq!(manager.get_live_state().status, MeetingStatus::Idle);
    }

    #[derive(Default)]
    struct CountingSpeechTranscriber {
        calls: AtomicUsize,
    }

    impl ChunkTranscriber for CountingSpeechTranscriber {
        fn transcribe_chunk(&self, samples: &[f32]) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(format!(
                "voiced utterance ({} ms)",
                samples_to_ms(samples.len() as u64)
            ))
        }
    }

    fn synth_harmonic_voice(duration_ms: usize, f0_hz: f32, amplitude: f32) -> Vec<f32> {
        let total = duration_ms * 16;
        let mut out = Vec::with_capacity(total);
        for idx in 0..total {
            let t = idx as f32 / 16_000.0;
            let env = 0.65 + 0.35 * (2.0 * std::f32::consts::PI * 3.2 * t).sin();
            let mut acc = 0.0_f32;
            for h in 1..=6 {
                let hf = h as f32;
                acc += (2.0 * std::f32::consts::PI * f0_hz * hf * t).sin() / hf;
            }
            out.push((acc * 0.42 * amplitude * env).clamp(-0.95, 0.95));
        }
        out
    }

    fn make_production_dsp_manager(
        backend: Arc<MockBackend>,
        transcriber: Arc<CountingSpeechTranscriber>,
    ) -> (MeetingSessionManager, tempfile::TempDir) {
        let dir = tempdir().expect("tempdir");
        let store = MeetingStore::open_in_path(dir.path().join("meetings.db")).expect("store");
        let capture_backend: Arc<dyn DualAudioCaptureBackend> = backend;
        let transcriber_dyn: Arc<dyn ChunkTranscriber> = transcriber;
        let manager = MeetingSessionManager::new_for_tests_with_production_dsp(
            store,
            capture_backend,
            transcriber_dyn,
            Arc::new(NoopCallDetector),
        );
        (manager, dir)
    }

    #[test]
    fn production_dsp_emits_utterances_on_pause_boundary_without_waiting_ten_seconds() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let transcriber = Arc::new(CountingSpeechTranscriber::default());
        let (manager, _dir) =
            make_production_dsp_manager(Arc::clone(&backend), Arc::clone(&transcriber));

        let live = manager.start_meeting(None, None).expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");

        let mut me_pcm = synth_harmonic_voice(1_200, 145.0, 0.35);
        me_pcm.extend(vec![0.0; 700 * 16]);
        let others_pcm = vec![0.0; me_pcm.len()];
        backend.push_chunk(DualPcmChunk {
            me_16k: me_pcm,
            others_16k: others_pcm,
            output_route: OutputRouteKind::Headphones,
        });

        wait_until(3_000, || {
            manager
                .get_meeting_detail(&meeting_id)
                .map(|detail| !detail.segments.is_empty())
                .unwrap_or(false)
        });

        let detail = manager.stop_meeting().expect("stop");
        assert_eq!(detail.segments.len(), 1);
        assert_eq!(detail.segments[0].speaker, SpeakerChannel::Me);
        assert!(detail.segments[0].end_ms <= 2_000);
        assert_eq!(transcriber.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn production_dsp_suppresses_loudspeaker_echo_bleed_from_remote_monologue() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Speakers,
        ));
        let transcriber = Arc::new(CountingSpeechTranscriber::default());
        let (manager, _dir) =
            make_production_dsp_manager(Arc::clone(&backend), Arc::clone(&transcriber));

        let live = manager.start_meeting(None, None).expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");

        let mut others_pcm = synth_harmonic_voice(1_600, 195.0, 0.42);
        others_pcm.extend(vec![0.0; 700 * 16]);
        let delay_samples = 45 * 16;
        let mut me_bleed = vec![0.0_f32; others_pcm.len()];
        for idx in delay_samples..others_pcm.len() {
            let x = others_pcm[idx - delay_samples] * 0.45;
            me_bleed[idx] = (x + 0.08 * x * x).tanh();
        }
        for (me_frame, others_frame) in me_bleed.chunks(320).zip(others_pcm.chunks(320)) {
            backend.push_chunk(DualPcmChunk {
                me_16k: me_frame.to_vec(),
                others_16k: others_frame.to_vec(),
                output_route: OutputRouteKind::Speakers,
            });
        }

        wait_until(3_000, || {
            manager
                .get_meeting_detail(&meeting_id)
                .map(|detail| !detail.segments.is_empty())
                .unwrap_or(false)
        });

        let detail = manager.stop_meeting().expect("stop");
        assert!(
            detail
                .segments
                .iter()
                .any(|seg| seg.speaker == SpeakerChannel::Others),
            "expected remote speaker segment"
        );
        assert!(
            detail
                .segments
                .iter()
                .all(|seg| seg.speaker != SpeakerChannel::Me),
            "expected zero false Me echo-bleed segments, got {:?}",
            detail.segments
        );
    }

    #[test]
    fn production_dsp_never_invokes_transcriber_on_room_noise_or_silence() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Speakers,
        ));
        let transcriber = Arc::new(CountingSpeechTranscriber::default());
        let (manager, _dir) =
            make_production_dsp_manager(Arc::clone(&backend), Arc::clone(&transcriber));

        manager.start_meeting(None, None).expect("start");

        // Deterministic stationary PRNG room hiss (~ -50 dBFS) for 3 seconds.
        let mut state = 0x1234_5678_u32;
        let mut noise = Vec::with_capacity(3 * 16_000);
        for _ in 0..(3 * 16_000) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let unit = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
            noise.push(unit * 0.003);
        }
        backend.push_chunk(DualPcmChunk {
            me_16k: noise.clone(),
            others_16k: noise,
            output_route: OutputRouteKind::Speakers,
        });

        wait_until(3_000, || manager.get_live_state().elapsed_ms >= 2_500);

        let detail = manager.stop_meeting().expect("stop");
        assert!(detail.segments.is_empty());
        assert_eq!(transcriber.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn production_dsp_flushes_active_utterance_cleanly_on_pause() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let transcriber = Arc::new(CountingSpeechTranscriber::default());
        let (manager, _dir) =
            make_production_dsp_manager(Arc::clone(&backend), Arc::clone(&transcriber));

        let live = manager.start_meeting(None, None).expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");

        // Push 900 ms of continuous speech with NO trailing silence, then pause immediately.
        let me_pcm = synth_harmonic_voice(900, 140.0, 0.35);
        backend.push_chunk(DualPcmChunk {
            me_16k: me_pcm.clone(),
            others_16k: vec![0.0; me_pcm.len()],
            output_route: OutputRouteKind::Headphones,
        });

        wait_until(3_000, || manager.get_live_state().elapsed_ms >= 850);
        manager.pause_meeting().expect("pause");

        wait_until(3_000, || {
            manager
                .get_meeting_detail(&meeting_id)
                .map(|detail| !detail.segments.is_empty())
                .unwrap_or(false)
        });

        let detail = manager.stop_meeting().expect("stop");
        assert_eq!(detail.segments.len(), 1);
        assert_eq!(detail.segments[0].speaker, SpeakerChannel::Me);
    }

    struct ScriptedQueueTranscriber {
        turns: Mutex<std::collections::VecDeque<String>>,
    }

    impl ScriptedQueueTranscriber {
        fn new(turns: Vec<&str>) -> Self {
            Self {
                turns: Mutex::new(turns.into_iter().map(str::to_owned).collect()),
            }
        }
    }

    impl ChunkTranscriber for ScriptedQueueTranscriber {
        fn transcribe_chunk(&self, _samples: &[f32]) -> Result<String, String> {
            let mut guard = self.turns.lock().expect("turns lock");
            Ok(guard
                .pop_front()
                .unwrap_or_else(|| "follow-up discussion".to_string()))
        }
    }

    #[test]
    fn production_diarizer_separates_two_remote_voices_and_backfills_name_cues() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let transcriber: Arc<dyn ChunkTranscriber> = Arc::new(ScriptedQueueTranscriber::new(vec![
            "Let's kick off the release review.",
            "Status looks green on our side.",
            "Hi, I'm Rahul and I'll cover the mobile SDK.",
            "Priya, can you share the backend numbers?",
            "Sure, backend p95 latency dropped to 18ms.",
        ]));
        let dir = tempdir().expect("tempdir");
        let store = MeetingStore::open_in_path(dir.path().join("meetings.db")).expect("store");
        let manager = MeetingSessionManager::new_for_tests_with_production_dsp(
            store,
            backend.clone(),
            transcriber,
            Arc::new(NoopCallDetector),
        );

        let live = manager.start_meeting(None, None).expect("start");
        let meeting_id = live.active_meeting_id.expect("meeting id");

        let push_turn = |me_f0: Option<f32>, others_f0: Option<f32>| {
            let len_ms = 1_100;
            let silence_tail_ms = 750;
            let mut me_pcm = match me_f0 {
                Some(f0) => synth_harmonic_voice(len_ms, f0, 0.36),
                None => vec![0.0; len_ms * 16],
            };
            me_pcm.extend(vec![0.0; silence_tail_ms * 16]);

            let mut others_pcm = match others_f0 {
                Some(f0) => synth_harmonic_voice(len_ms, f0, 0.38),
                None => vec![0.0; len_ms * 16],
            };
            others_pcm.extend(vec![0.0; silence_tail_ms * 16]);

            backend.push_chunk(DualPcmChunk {
                me_16k: me_pcm,
                others_16k: others_pcm,
                output_route: OutputRouteKind::Headphones,
            });
        };

        // 1. Me (165 Hz)
        push_turn(Some(165.0), None);
        // 2. Speaker 1 anonymous turn (115 Hz)
        push_turn(None, Some(115.0));
        // 3. Speaker 1 self-introduces as Rahul (115 Hz) -> backfills turn 2 too!
        push_turn(None, Some(115.0));
        // 4. Me vocative handoff to Priya (165 Hz)
        push_turn(Some(165.0), None);
        // 5. Speaker 2 responds (240 Hz) -> bound to Priya!
        push_turn(None, Some(240.0));

        wait_until(4_000, || {
            manager
                .get_meeting_detail(&meeting_id)
                .map(|detail| detail.segments.len() >= 5)
                .unwrap_or(false)
        });

        let detail = manager.stop_meeting().expect("stop");
        assert_eq!(detail.segments.len(), 5);
        assert_eq!(detail.segments[0].speaker, SpeakerChannel::Me);
        assert_eq!(detail.segments[0].effective_speaker_label(), "Me");

        // Turn 2 was spoken BEFORE Rahul introduced himself in Turn 3, and was retroactively backfilled!
        assert_eq!(detail.segments[1].speaker_id.as_deref(), Some("speaker_1"));
        assert_eq!(
            detail.segments[1].effective_speaker_label(),
            "Speaker 1 (Rahul)"
        );
        assert_eq!(
            detail.segments[2].effective_speaker_label(),
            "Speaker 1 (Rahul)"
        );

        assert_eq!(detail.segments[3].speaker, SpeakerChannel::Me);
        assert_eq!(detail.segments[4].speaker_id.as_deref(), Some("speaker_2"));
        assert_eq!(
            detail.segments[4].effective_speaker_label(),
            "Speaker 2 (Priya)"
        );
    }

    #[test]
    fn stop_meeting_upgrades_placeholder_title_and_preserves_custom_title() {
        // 1. Placeholder ("Untitled" / None -> "Meeting — YYYY-MM-DD ...") gets upgraded to topical title.
        let backend_auto = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager_auto, _store_auto, _dir_auto) = make_manager(Arc::clone(&backend_auto));
        manager_auto.start_meeting(None, None).expect("start auto");
        manager_auto
            .update_live_rough_notes("- Mobile SDK Q4 release readiness\n- ship beta Friday".into())
            .expect("rough notes");
        backend_auto.push_chunk(speech_chunk(0.8, 0.0));
        wait_until(800, || {
            !manager_auto.get_live_state().recent_segments.is_empty()
        });
        let detail_auto = manager_auto.stop_meeting().expect("stop auto");
        assert!(
            !lowyap_summarize::is_placeholder_meeting_title(&detail_auto.summary.title),
            "expected placeholder title to be upgraded, got '{}'",
            detail_auto.summary.title
        );
        assert!(detail_auto.summary.title.contains("Mobile SDK"));

        // 2. Explicit user title ("Architecture Deep Dive") is never overwritten.
        let backend_custom = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let (manager_custom, _store_custom, _dir_custom) =
            make_manager(Arc::clone(&backend_custom));
        manager_custom
            .start_meeting(Some("Architecture Deep Dive".to_string()), None)
            .expect("start custom");
        manager_custom
            .update_live_rough_notes("- Different rough bullet topic".into())
            .expect("rough notes");
        let detail_custom = manager_custom.stop_meeting().expect("stop custom");
        assert_eq!(detail_custom.summary.title, "Architecture Deep Dive");
    }

    #[test]
    fn start_meeting_with_calendar_auto_titles_one_on_one_and_attributes_attendee_roster() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let transcriber: Arc<dyn ChunkTranscriber> = Arc::new(ScriptedQueueTranscriber::new(vec![
            "Yeah the onboarding conversion metrics jumped twelve percent.",
        ]));
        let dir = tempdir().expect("tempdir");
        let store = MeetingStore::open_in_path(dir.path().join("meetings.db")).expect("store");
        let manager = MeetingSessionManager::new_for_tests_with_production_dsp(
            store,
            backend.clone(),
            transcriber,
            Arc::new(NoopCallDetector),
        );

        let calendar_ctx = crate::meeting::macos_bridge::ActiveCalendarMeeting {
            title: "1:1: Product & Growth".to_string(),
            attendees: vec!["Sarah Chen".to_string()],
        };

        let live = manager
            .start_meeting_with_calendar(None, None, Some(calendar_ctx))
            .expect("start with calendar");
        assert_eq!(live.title, "1:1: Product & Growth");
        assert_eq!(live.template, MeetingTemplate::OneOnOne);
        let meeting_id = live.active_meeting_id.expect("meeting id");

        let mut others_pcm = synth_harmonic_voice(1_200, 195.0, 0.38);
        others_pcm.extend(vec![0.0; 750 * 16]);
        backend.push_chunk(DualPcmChunk {
            me_16k: vec![0.0; others_pcm.len()],
            others_16k: others_pcm,
            output_route: OutputRouteKind::Headphones,
        });

        wait_until(3_500, || {
            manager
                .get_meeting_detail(&meeting_id)
                .map(|detail| !detail.segments.is_empty())
                .unwrap_or(false)
        });

        let detail = manager.stop_meeting().expect("stop");
        assert_eq!(detail.summary.title, "1:1: Product & Growth");
        assert_eq!(detail.summary.template, MeetingTemplate::OneOnOne);
        assert_eq!(detail.segments.len(), 1);
        assert_eq!(
            detail.segments[0].effective_speaker_label(),
            "Speaker 1 (Sarah Chen)"
        );
    }

    #[test]
    fn echo_deduplicator_suppresses_cross_script_speaker_bleed_and_preserves_real_replies() {
        let mut dedup = EchoDeduplicator::default();
        let initial_pruned = dedup.record_others_and_collect_bleed_me_ids(
            1_000,
            4_500,
            "Haan ji sir, kal Guru Nanak Jayanti ki chhutti hai",
        );
        assert!(initial_pruned.is_empty());

        // Cross-script Urdu bleed on Me overlapping Others -> suppressed!
        assert!(dedup.should_drop_me(1_400, 4_100, "گورنانک جینتی کی چھٹی ہے",));

        // Distinct Me reply overlapping Others -> preserved!
        assert!(!dedup.should_drop_me(
            1_800,
            4_200,
            "Got it, let us move the deployment to Wednesday morning instead",
        ));

        // Non-overlapping Me utterance > 18s later -> preserved!
        assert!(!dedup.should_drop_me(28_000, 30_000, "گورنانک جینتی کی چھٹی ہے",));

        // Retroactive pruning when Me's bleed fragment closed slightly before Others' longer turn closed:
        dedup.record_me(42, 32_200, 34_100, "content cards delivery pipeline");
        let stale_ids = dedup.record_others_and_collect_bleed_me_ids(
            32_000,
            35_400,
            "We are reviewing the content cards delivery pipeline across mobile SDKs",
        );
        assert_eq!(stale_ids, vec![42]);
    }

    struct ToggleCallDetector {
        active_app: Mutex<Option<crate::meeting::macos_bridge::RunningCallApp>>,
    }

    impl ToggleCallDetector {
        fn new(initial: Option<(&str, &str)>) -> Self {
            Self {
                active_app: Mutex::new(initial.map(|(bundle_id, app_name)| {
                    crate::meeting::macos_bridge::RunningCallApp {
                        bundle_id: bundle_id.to_string(),
                        app_name: app_name.to_string(),
                        tier: crate::meeting::macos_bridge::RunningCallAppTier::Dedicated,
                    }
                })),
            }
        }

        fn set_app(&self, app: Option<(&str, &str)>) {
            *self.active_app.lock().expect("detector lock") =
                app.map(
                    |(bundle_id, app_name)| crate::meeting::macos_bridge::RunningCallApp {
                        bundle_id: bundle_id.to_string(),
                        app_name: app_name.to_string(),
                        tier: crate::meeting::macos_bridge::RunningCallAppTier::Dedicated,
                    },
                );
        }
    }

    impl CallDetectorBackend for ToggleCallDetector {
        fn poll_detected_call(&self) -> Result<Option<DetectedCallInfo>, String> {
            Ok(None)
        }

        fn dismiss_call(&self, _call_id: Option<&str>) -> Result<(), String> {
            Ok(())
        }

        fn active_call_app(&self) -> Option<crate::meeting::macos_bridge::RunningCallApp> {
            self.active_app.lock().expect("detector lock").clone()
        }
    }

    #[test]
    fn call_end_watcher_soft_stops_after_hangup_and_resume_suppresses_until_rejoin() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::DualStream,
            OutputRouteKind::Headphones,
        ));
        let detector = Arc::new(ToggleCallDetector::new(Some((
            "com.microsoft.teams2",
            "Microsoft Teams",
        ))));
        let dir = tempdir().expect("tempdir");
        let store = MeetingStore::open_in_path(dir.path().join("meetings.db")).expect("store");
        let manager = MeetingSessionManager::new_for_tests(
            store,
            backend,
            Arc::new(MockChunkTranscriber),
            detector.clone(),
        );

        // Mid-call start populates app_name even when detected_call banner was not active.
        let started = manager.start_meeting(None, None).expect("start");
        assert_eq!(started.app_name.as_deref(), Some("Microsoft Teams"));
        let meeting_id = started.active_meeting_id.expect("meeting id");

        // Hang up Teams -> 2 negative polls stay recording, 3rd poll triggers SoftStopped.
        detector.set_app(None);
        assert!(!manager.poll_active_call_watcher());
        assert!(!manager.poll_active_call_watcher());
        assert_eq!(manager.get_live_state().status, MeetingStatus::Recording);

        assert!(manager.poll_active_call_watcher());
        assert_eq!(manager.get_live_state().status, MeetingStatus::Paused);
        assert!(manager
            .inner
            .clock
            .lock()
            .expect("clock")
            .soft_stop
            .is_some());

        // Clicking Resume restores Recording and suppresses immediate re-stop while still off-call.
        let resumed = manager.resume_meeting().expect("resume");
        assert_eq!(resumed.status, MeetingStatus::Recording);
        for _ in 0..5 {
            assert!(
                !manager.poll_active_call_watcher(),
                "post-hangup Resume must not re-stop while user dictates wrap-up notes"
            );
        }
        assert_eq!(manager.get_live_state().status, MeetingStatus::Recording);

        // Rejoining a call re-arms the watcher, and hanging up + grace expiry finalizes notes.
        detector.set_app(Some(("com.microsoft.teams2", "Microsoft Teams")));
        assert!(!manager.poll_active_call_watcher());
        detector.set_app(None);
        let (_, epoch) = manager
            .soft_stop_on_call_end(0)
            .expect("explicit soft stop");
        assert!(manager.expire_soft_stop_if_matching(&meeting_id, epoch));
        assert_eq!(manager.get_live_state().status, MeetingStatus::Idle);
    }

    #[test]
    fn manual_in_person_meeting_without_call_app_never_auto_stops() {
        let backend = Arc::new(MockBackend::new(
            AudioCaptureMode::MicOnlyFallback,
            OutputRouteKind::Speakers,
        ));
        let detector = Arc::new(ToggleCallDetector::new(None));
        let dir = tempdir().expect("tempdir");
        let store = MeetingStore::open_in_path(dir.path().join("meetings.db")).expect("store");
        let manager = MeetingSessionManager::new_for_tests(
            store,
            backend,
            Arc::new(MockChunkTranscriber),
            detector,
        );

        let started = manager.start_meeting(None, None).expect("start");
        assert_eq!(started.app_name, None);
        for _ in 0..8 {
            assert!(!manager.poll_active_call_watcher());
        }
        assert_eq!(manager.get_live_state().status, MeetingStatus::Recording);
        let _ = manager.stop_meeting();
    }

    #[test]
    fn normalize_meeting_segment_script_unifies_bilingual_hinglish_and_respects_explicit_scripts() {
        let langs_en_ur = vec!["en".to_string(), "ur".to_string()];
        // Auto + en + ur converts stray Perso-Arabic islands to Roman Hinglish for 100% consistent meeting notes
        let unified = normalize_meeting_segment_script(
            "میں فیڈنگ کرتا ہوں for the UI component",
            &langs_en_ur,
            HindustaniScript::Auto,
        );
        assert!(
            unified
                .chars()
                .all(|c| !('\u{0600}'..='\u{06FF}').contains(&c)),
            "expected Roman Hinglish without Perso-Arabic islands, got '{unified}'"
        );
        assert!(unified.contains("UI component"));

        // Explicit UrduPersoArabic preference preserves Perso-Arabic
        let explicit_urdu = normalize_meeting_segment_script(
            "میٹنگ نوٹس کا معیار",
            &langs_en_ur,
            HindustaniScript::UrduPersoArabic,
        );
        assert!(explicit_urdu.contains("میٹنگ نوٹس"));
    }
}
