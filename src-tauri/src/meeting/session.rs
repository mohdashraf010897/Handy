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
const DETECTOR_POLL_INTERVAL: Duration = Duration::from_millis(2_500);
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

#[derive(Default)]
struct MeetingClock {
    started_at_ms: u64,
    paused_at_ms: Option<u64>,
    accumulated_paused_ms: u64,
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
        manager
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
        let mut active_guard = self
            .inner
            .active_meeting
            .lock()
            .map_err(|_| "meeting state is unavailable".to_string())?;
        if active_guard.is_some() || self.is_meeting_active() {
            return Err("A meeting session is already active.".to_string());
        }

        let capture = self.inner.capture_backend.start_capture()?;
        let resolved_template = template.unwrap_or(MeetingTemplate::General);
        let resolved_title = normalize_title(title);
        let meeting_id = self.next_meeting_id();
        let started_at = crate::session_bus::now_ms();
        let started_at_ms = i64::try_from(started_at).unwrap_or(i64::MAX);

        let capture_mode = capture.capture_mode();
        let output_route = capture.output_route();
        let app_name = self.inner.live_state.lock().ok().and_then(|state| {
            state
                .detected_call
                .as_ref()
                .map(|call| call.app_name.clone())
        });

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
            self.inner
                .clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .paused_at_ms = Some(crate::session_bus::now_ms());
            active.paused_flag.store(true, Ordering::Release);
        }

        let state = self.get_live_state();
        emit_live_state(&self.inner, &state);
        Ok(state)
    }

    pub fn resume_meeting(&self) -> Result<MeetingLiveState, String> {
        let (meeting_id, marker_elapsed) = {
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
            if let Some(paused_at) = clock.paused_at_ms.take() {
                clock.accumulated_paused_ms = clock
                    .accumulated_paused_ms
                    .saturating_add(crate::session_bus::now_ms().saturating_sub(paused_at));
            }
            live.status = MeetingStatus::Recording;
            active.paused_flag.store(false, Ordering::Release);
            (active.meeting_id.clone(), live.elapsed_ms)
        };

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

        let state = self.get_live_state();
        emit_live_state(&self.inner, &state);
        Ok(state)
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
                let rollup_strings = to_rollup_strings(&fresh_detail.rollups);
                let port_segments = to_port_segments(&fresh_detail.segments);
                let backends = build_production_completion_backends(self.inner.app_handle.as_ref());
                if let Some(generated_title) = lowyap_summarize::generate_meeting_title(
                    &fresh_detail.rough_notes,
                    &rollup_strings,
                    &port_segments,
                    backends.first().map(|b| b.as_ref()),
                ) {
                    let _ = self.inner.store.update_placeholder_title_if_unchanged(
                        &meeting_id,
                        &fresh_detail.summary.title,
                        &generated_title,
                    )?;
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

        let inner = Arc::clone(&self.inner);
        let handle = thread::spawn(move || {
            while !inner.detector_stop.load(Ordering::Acquire) {
                if should_skip_detection(&inner) {
                    thread::sleep(DETECTOR_POLL_INTERVAL);
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

                thread::sleep(DETECTOR_POLL_INTERVAL);
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
) {
    let capture_mode = capture.capture_mode();
    let mut echo_canceller = (inner.echo_canceller_factory)();
    let mut segmenter = (inner.segmenter_factory)(inner.app_handle.as_ref());
    let mut diarizer = (inner.diarizer_factory)(capture_mode);
    let mut name_resolver = lowyap_diarize::NameCueResolver::new();
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
                        diarizer.as_mut(),
                        &mut name_resolver,
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
            &chunk,
            echo_canceller.as_mut(),
            segmenter.as_mut(),
            diarizer.as_mut(),
            &mut name_resolver,
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
                &tail_chunk,
                echo_canceller.as_mut(),
                segmenter.as_mut(),
                diarizer.as_mut(),
                &mut name_resolver,
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

    for span in segmenter.flush() {
        persist_utterance_span(
            &inner,
            &meeting_id,
            &span,
            diarizer.as_mut(),
            &mut name_resolver,
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
    chunk: &crate::meeting::types::DualPcmChunk,
    echo_canceller: &mut dyn lowyap_ports::EchoCanceller,
    segmenter: &mut dyn lowyap_ports::UtteranceSegmenter,
    diarizer: &mut dyn lowyap_ports::SpeakerDiarizer,
    name_resolver: &mut lowyap_diarize::NameCueResolver,
    me_only_enroll_buf: &mut Vec<f32>,
    me_samples_seen: &mut u64,
    others_samples_seen: &mut u64,
) {
    let port_route: lowyap_ports::OutputRouteKind = chunk.output_route.into();
    let others_rms = rms_energy(&chunk.others_16k);
    if !chunk.me_16k.is_empty() {
        let cleaned_me = echo_canceller.process_16k(&chunk.me_16k, &chunk.others_16k, port_route);
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
            persist_utterance_span(inner, meeting_id, &span, diarizer, name_resolver);
        }
    }

    if !chunk.others_16k.is_empty() {
        let start_ms = samples_to_ms(*others_samples_seen);
        *others_samples_seen = others_samples_seen.saturating_add(chunk.others_16k.len() as u64);
        for span in segmenter.push_chunk(
            lowyap_ports::SpeakerChannel::Others,
            &chunk.others_16k,
            start_ms,
        ) {
            persist_utterance_span(inner, meeting_id, &span, diarizer, name_resolver);
        }
    }
}

fn persist_utterance_span(
    inner: &MeetingSessionManagerInner,
    meeting_id: &str,
    span: &lowyap_ports::UtteranceSpan,
    diarizer: &mut dyn lowyap_ports::SpeakerDiarizer,
    name_resolver: &mut lowyap_diarize::NameCueResolver,
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
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    let identity = diarizer.identify(span);
    let was_unbound = name_resolver.bound_name(&identity.speaker_id).is_none();
    name_resolver.observe_transcript_turn(
        identity.channel,
        &identity.speaker_id,
        span.start_ms,
        span.end_ms,
        trimmed,
    );
    let newly_bound = was_unbound && name_resolver.bound_name(&identity.speaker_id).is_some();
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
        Ok(segment) => push_recent_segment(inner, segment),
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
            MeetingStatus::Paused => MeetingPhase::Paused {
                meeting_id: live.active_meeting_id.clone()?,
                title: live.title.clone(),
                started_at_ms: clock.started_at_ms,
                paused_at_ms: clock.paused_at_ms?,
                accumulated_paused_ms: clock.accumulated_paused_ms,
                app_name: live.app_name.clone(),
                capture_mode,
            },
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
}
