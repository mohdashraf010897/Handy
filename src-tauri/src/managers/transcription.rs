use crate::audio_toolkit::chunking;
use crate::audio_toolkit::{
    apply_custom_words, detect_output_language, normalize_transcription_output,
    remove_filler_words, OutputLanguageEvidence,
};
use crate::chinese_script::{convert_chinese_script, ChineseVariety};
use crate::engine_supervisor::{
    CancelMark, DeviceInfo, DeviceSelector, EngineError, EngineSupervisor, LoadSpec, LoadedInfo,
    Unloading,
};
#[cfg(target_os = "macos")]
use crate::flow::macos_speech::{self, MACOS_NATIVE_SPEECH_MODEL_ID};
use crate::language_detection::speech::speech_head;
use crate::language_detection::{
    IdentifyError, LanguageDecision, LanguageDetector, LanguageSource, Plan,
};
use crate::managers::audio::{create_smoothed_detector, AudioRecordingManager};
use crate::managers::model::ModelManager;
use crate::settings::{
    get_settings, AppSettings, ChineseScript, ModelUnloadTimeout, TranscribeAcceleratorSetting,
};
use anyhow::Result;
use chunked::{ChunkSetup, ChunkedDecode, PendingSetup};
use live::LiveSession;
use log::{debug, error, info, warn};
use policy::{batch_decode, live_decode, BatchDecode, ModelTraits};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, Manager};
use tauri_specta::Event;
use transcribe_cpp::{
    Backend, RunExtension, RunOptions, Task, TimestampKind, Transcript, WhisperRunOptions,
};

mod chunked;
mod job;
mod live;
pub mod policy;

pub use job::ChunkControl;

const STREAM_PERF_LOG_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelStateEvent {
    pub event_type: String,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
}

/// Live transcription snapshot emitted to the overlay during a streaming run.
/// `committed` is the append-only, flicker-free prefix; `tentative` is the
/// volatile suffix the model may still rewrite.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamTextEvent {
    pub committed: String,
    pub tentative: String,
}

/// Phase of the streaming overlay card, emitted to drive its UI state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StreamPhase {
    /// Receiving audio / live text (or waiting for the stream to begin). Rust
    /// does not emit this today; the frontend starts in this phase and Rust only
    /// emits transitions away from it.
    Listening,
    /// Finalizing or post-processing — show a spinner.
    Working,
}

/// Semantic kind of "working" phase, used to localize the spinner label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StreamWorkKind {
    Transcribing,
    Polishing,
}

/// Emitted to switch the streaming overlay to a working spinner.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamPhaseEvent {
    pub phase: StreamPhase,
    /// Present only when `phase` is `Working`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<StreamWorkKind>,
}

/// Commands sent to the streaming worker thread. Audio frames and the finalize
/// request travel the same channel so FIFO ordering guarantees every fed frame
/// is processed before finalize runs.
enum StreamCmd {
    Feed(Vec<f32>),
    /// Flush the live decode and reply with what it decoded, or `None` if
    /// there is no usable live result (caller should fall back to batch
    /// transcription). `Err(Cancelled)` if the finalize itself was cancelled.
    /// `samples` is the whole recording's length, for a chunked decode that
    /// checks it saw it all.
    Finalize {
        reply: mpsc::Sender<StreamFinalizeReply>,
        samples: usize,
        timing_session: Option<u64>,
    },
    Cancel,
}

type StreamFinalizeReply = Result<Option<RunOutcome>, EngineError>;

/// Routes real-time audio frames to the active streaming worker. Shared between
/// the [`TranscriptionManager`] (opens/closes the route) and the audio recorder's
/// per-frame callback (feeds frames). The recorder holds an `Arc<StreamRouter>`
/// directly, so a frame with no stream pending costs a single relaxed atomic
/// load — no Tauri state lookup, no mutex lock.
pub struct StreamRouter {
    /// The active streaming worker's route, present from `start_stream`
    /// until `finalize_live`/`cancel_stream`.
    route: Mutex<Option<Route>>,
    /// True while a stream is pending or active (channel is open). The audio
    /// callback checks this first to avoid the mutex lock when no stream runs.
    open: Arc<AtomicBool>,
}

/// One streaming worker's command channel.
struct Route {
    tx: mpsc::Sender<StreamCmd>,
    /// Set by `cancel_stream`, so the worker drops audio still queued ahead
    /// of the `Cancel` instead of decoding it.
    cancelled: Arc<AtomicBool>,
}

impl StreamRouter {
    fn new() -> Self {
        Self {
            route: Mutex::new(None),
            open: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Open a fresh command channel for a new streaming session, returning the
    /// receiver the worker should drain and its cancel flag. Caller must
    /// ensure no prior channel is still open.
    fn open(&self) -> (mpsc::Receiver<StreamCmd>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel::<StreamCmd>();
        let cancelled = Arc::new(AtomicBool::new(false));
        *self.route.lock().unwrap() = Some(Route {
            tx,
            cancelled: Arc::clone(&cancelled),
        });
        self.open.store(true, Ordering::Relaxed);
        (rx, cancelled)
    }

    /// Take the route out (closing the channel to new feeds), so the caller
    /// can send the final `Finalize`/`Cancel` command.
    fn take(&self) -> Option<Route> {
        self.open.store(false, Ordering::Relaxed);
        self.route.lock().unwrap().take()
    }

    /// Drop the channel and mark closed without sending a final command (used
    /// when the worker exits without a finalize/cancel handshake).
    fn clear(&self) {
        self.open.store(false, Ordering::Relaxed);
        *self.route.lock().unwrap() = None;
    }

    /// Forward a 16 kHz frame to the active streaming worker. Cheap no-op (a
    /// single relaxed atomic load) when no stream is pending.
    pub fn feed(&self, frame: &[f32]) {
        if !self.open.load(Ordering::Relaxed) {
            return;
        }
        if let Some(route) = self.route.lock().unwrap().as_ref() {
            let _ = route.tx.send(StreamCmd::Feed(frame.to_vec()));
        }
    }

    /// Whether a stream is pending or active.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }
}

/// RAII guard that clears the `is_loading` flag and notifies waiters on drop.
/// Ensures the loading flag is always reset, even on early returns or panics.
pub struct LoadingGuard {
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        // Recover from a poisoned mutex instead of panicking —
        // a panic inside Drop calls abort().
        let mut is_loading = match self.is_loading.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!("Recovered poisoned is_loading mutex during LoadingGuard drop — a panic occurred earlier this session");
                e.into_inner()
            }
        };
        *is_loading = false;
        self.loading_condvar.notify_all();
    }
}

/// RAII guard that clears the streaming worker flags on any worker exit -
/// normal return, early return, or a panic that unwinds the detached worker
/// thread. Tokens prevent an older worker from clearing a newer worker's
/// state if a start/finalize race ever slips through.
struct StreamWorkerGuard {
    worker_id: u64,
    active_stream_worker: Arc<AtomicU64>,
    stream_active: Arc<AtomicBool>,
}

impl Drop for StreamWorkerGuard {
    fn drop(&mut self) {
        if self.active_stream_worker.load(Ordering::Acquire) == self.worker_id {
            self.stream_active.store(false, Ordering::Release);
        }
        let _ = self.active_stream_worker.compare_exchange(
            self.worker_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

#[derive(Clone)]
pub struct TranscriptionManager {
    /// The transcribe-cpp engine: owns the worker process, its model, and
    /// recovery from crashes and hangs.
    engine: EngineSupervisor,
    model_manager: Arc<ModelManager>,
    app_handle: AppHandle,
    current_model_id: Arc<Mutex<Option<String>>>,
    last_activity: Arc<AtomicU64>,
    shutdown_signal: Arc<AtomicBool>,
    watcher_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
    reload_model_on_next_use: Arc<AtomicBool>,
    /// Routes real-time audio frames to the active streaming worker; see
    /// [`StreamRouter`]. Shared with the audio recorder so per-frame feeds skip
    /// Tauri state and the manager lock.
    router: Arc<StreamRouter>,
    /// True only while a transcribe-cpp stream is actually in flight (set by
    /// the worker once the stream begins). Used for overlay/UI decisions.
    stream_active: Arc<AtomicBool>,
    /// Streaming uses three independent flags: router open = frames should
    /// route, worker active = no second worker may start, stream active = UI
    /// should show a live session.
    ///
    /// Monotonic id source for stream workers; zero means "no worker".
    next_stream_worker_id: Arc<AtomicU64>,
    /// Nonzero while a stream worker exists, even if it has not leased the engine
    /// yet. This prevents a second worker from starting after finalize/cancel
    /// closes the router but before the first worker has fully exited.
    active_stream_worker: Arc<AtomicU64>,
    /// True while a background job is queued or running. It holds the model,
    /// so one transcript is never written by two models.
    job_active: Arc<AtomicBool>,
    /// Handy's spoken-language detector, loaded in this process on first
    /// use and unloaded with the model.
    language_detector: Arc<LanguageDetector>,
}

impl TranscriptionManager {
    pub fn new(app_handle: &AppHandle, model_manager: Arc<ModelManager>) -> Result<Self> {
        let manager = Self {
            engine: EngineSupervisor::new(!transcribe_gpu_disabled_for_host()),
            model_manager,
            app_handle: app_handle.clone(),
            current_model_id: Arc::new(Mutex::new(None)),
            last_activity: Arc::new(AtomicU64::new(Self::now_ms())),
            shutdown_signal: Arc::new(AtomicBool::new(false)),
            watcher_handle: Arc::new(Mutex::new(None)),
            is_loading: Arc::new(Mutex::new(false)),
            loading_condvar: Arc::new(Condvar::new()),
            reload_model_on_next_use: Arc::new(AtomicBool::new(false)),
            router: Arc::new(StreamRouter::new()),
            stream_active: Arc::new(AtomicBool::new(false)),
            next_stream_worker_id: Arc::new(AtomicU64::new(1)),
            active_stream_worker: Arc::new(AtomicU64::new(0)),
            job_active: Arc::new(AtomicBool::new(false)),
            language_detector: Arc::new(LanguageDetector::default()),
        };

        // Start the idle watcher
        {
            let app_handle_cloned = app_handle.clone();
            let manager_cloned = manager.clone();
            let shutdown_signal = manager.shutdown_signal.clone();
            let handle = thread::spawn(move || {
                debug!("Idle watcher thread started");
                while !shutdown_signal.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(10)); // Check every 10 seconds

                    // Check shutdown signal again after sleep
                    if shutdown_signal.load(Ordering::Relaxed) {
                        break;
                    }

                    let settings = get_settings(&app_handle_cloned);
                    let timeout = settings.model_unload_timeout;

                    // Skip Immediately — that variant is handled by
                    // maybe_unload_immediately() after each transcription.
                    // Treating it as 0s here would unload the model mid-recording.
                    if timeout == ModelUnloadTimeout::Immediately {
                        continue;
                    }

                    // While recording or transcribing a file, keep the idle
                    // timer fresh so the model is never unloaded mid-session.
                    let is_recording = app_handle_cloned
                        .try_state::<Arc<AudioRecordingManager>>()
                        .is_some_and(|a| a.is_recording());
                    if is_recording || manager_cloned.job_active() {
                        manager_cloned.touch_activity();
                        continue;
                    }

                    if let Some(limit_seconds) = timeout.to_seconds() {
                        let last = manager_cloned.last_activity.load(Ordering::Relaxed);
                        let now_ms = TranscriptionManager::now_ms();
                        let idle_ms = now_ms.saturating_sub(last);
                        let limit_ms = limit_seconds * 1000;

                        if idle_ms > limit_ms {
                            // idle -> unload
                            if manager_cloned.has_loaded_model() {
                                let unload_start = std::time::Instant::now();
                                info!(
                                    "Model idle for {}s (limit: {}s), unloading",
                                    idle_ms / 1000,
                                    limit_seconds
                                );
                                match manager_cloned.unload_model() {
                                    Ok(()) => {
                                        let unload_duration = unload_start.elapsed();
                                        info!(
                                            "Model unloaded due to inactivity (took {}ms)",
                                            unload_duration.as_millis()
                                        );
                                    }
                                    Err(e) => {
                                        error!("Failed to unload idle model: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
                debug!("Idle watcher thread shutting down gracefully");
            });
            *manager.watcher_handle.lock().unwrap() = Some(handle);
        }

        Ok(manager)
    }

    pub fn is_model_loaded(&self) -> bool {
        if self.has_loaded_model() {
            return true;
        }
        // The shortcut also uses this as a readiness gate. Instant Voice
        // needs no loaded worker or file before the microphone can open.
        #[cfg(target_os = "macos")]
        {
            let settings = get_settings(&self.app_handle);
            return self.should_use_native_speech(&settings);
        }
        #[cfg(not(target_os = "macos"))]
        false
    }

    fn has_loaded_model(&self) -> bool {
        // A transcribe-cpp model stays loaded while it is busy, so a batch
        // run or stream in progress never reads as "unloaded".
        #[cfg(target_os = "macos")]
        if self.get_current_model().as_deref() == Some(MACOS_NATIVE_SPEECH_MODEL_ID) {
            return true;
        }
        self.engine.loaded().is_some()
    }

    /// Accelerator changes should not disturb the current transcription. Mark
    /// the cached engine stale; the next model-use path reloads it with the
    /// latest settings.
    pub fn reload_model_on_next_use(&self) {
        self.reload_model_on_next_use.store(true, Ordering::Release);
    }

    /// An explicit transcribe.cpp accelerator or GPU device change is the
    /// user's way of trying the GPU again: forget every GPU failure so far,
    /// so the next load may use (and list) the GPU.
    pub fn retry_transcribe_gpu(&self, reason: &str) {
        self.engine.retry_gpu(reason);
    }

    /// The engine gives up on a model by itself when its worker can't be
    /// restarted (e.g. the file is gone, or it crashes on CPU too). Keep the
    /// loaded model id and the UI in step, so the next use loads the model
    /// again and reports why it can't.
    fn forget_model_if_engine_dropped(&self, model_id: &str, error: &str) {
        if self.engine.loaded().is_some() {
            return;
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            if current_model.as_deref() != Some(model_id) {
                return;
            }
            *current_model = None;
        }
        warn!(
            "Transcription model '{}' is no longer loaded: {}",
            model_id, error
        );
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "unloaded".to_string(),
                model_id: None,
                model_name: None,
                error: Some(error.to_string()),
            },
        );
    }

    /// Atomically check whether a model load is in progress and, if not, mark
    /// one as starting. Returns a [`LoadingGuard`] whose [`Drop`] impl will
    /// clear the flag and wake waiters. Returns `None` if a load is already in
    /// progress.
    pub fn try_start_loading(&self) -> Option<LoadingGuard> {
        let mut is_loading = self.is_loading.lock().unwrap();
        if *is_loading {
            return None;
        }
        *is_loading = true;
        Some(LoadingGuard {
            is_loading: self.is_loading.clone(),
            loading_condvar: self.loading_condvar.clone(),
        })
    }

    /// Unload the model. Returns once the transcribe-cpp worker has exited,
    /// which frees all of its memory (e.g. before its file is deleted).
    pub fn unload_model(&self) -> Result<()> {
        let unload_start = std::time::Instant::now();
        debug!("Starting to unload model");
        self.begin_unload().wait();
        debug!(
            "Model unloaded manually (took {}ms)",
            unload_start.elapsed().as_millis()
        );
        Ok(())
    }

    /// Unload the model without waiting for the transcribe-cpp worker to
    /// exit, so it never blocks the caller (e.g. the event loop) behind a
    /// transcription in progress. The model reads as unloaded at once.
    pub fn request_unload(&self) {
        drop(self.begin_unload());
    }

    /// Every state change of an unload, done now on the caller's thread: a
    /// dictation started right after sees no model and loads it again, and
    /// its load is queued behind this unload, so the unload can never affect
    /// it. Only the worker's exit is left to wait for.
    fn begin_unload(&self) -> Unloading {
        let unloading = self.engine.unload();
        self.language_detector.unload();
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = None;
        }

        // Emit unloaded event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "unloaded".to_string(),
                model_id: None,
                model_name: None,
                error: None,
            },
        );
        unloading
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// Reset the idle timer to now.
    fn touch_activity(&self) {
        self.last_activity.store(Self::now_ms(), Ordering::Relaxed);
    }

    /// Unloads the model immediately if the setting is enabled and the model
    /// is loaded. Never waits for the worker to exit, so it is safe on any
    /// thread, including the event loop.
    pub fn maybe_unload_immediately(&self, context: &str) {
        // A file job keeps the model for its remaining chunks; it unloads
        // the model itself when it ends.
        if self.job_active() {
            return;
        }
        let settings = get_settings(&self.app_handle);
        if settings.model_unload_timeout == ModelUnloadTimeout::Immediately
            && self.has_loaded_model()
        {
            info!("Immediately unloading model after {}", context);
            self.request_unload();
        }
    }

    pub fn load_model(&self, model_id: &str) -> Result<()> {
        self.load_model_with_device(model_id, None)
    }

    /// Like [`load_model`](Self::load_model), but lets a caller hard-select the
    /// compute device for this one load by its transcribe-cpp device
    /// registry index (the index shown by `--list-devices`). `None` keeps the
    /// persisted accelerator setting (which may be Auto). Only affects
    /// transcribe-cpp (whisper-family) models; the selection is not persisted.
    pub fn load_model_with_device(
        &self,
        model_id: &str,
        device_index: Option<usize>,
    ) -> Result<()> {
        let load_start = std::time::Instant::now();
        debug!("Starting to load model: {}", model_id);

        // Emit loading started event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_started".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: None,
                error: None,
            },
        );

        let model_info = match self.model_manager.get_model_info(model_id) {
            Some(model_info) => model_info,
            None => {
                let error_msg = format!("Model not found: {}", model_id);
                let _ = self.app_handle.emit(
                    "model-state-changed",
                    ModelStateEvent {
                        event_type: "loading_failed".to_string(),
                        model_id: Some(model_id.to_string()),
                        model_name: None,
                        error: Some(error_msg.clone()),
                    },
                );
                return Err(anyhow::anyhow!(error_msg));
            }
        };

        #[cfg(target_os = "macos")]
        if model_id == MACOS_NATIVE_SPEECH_MODEL_ID {
            self.engine.unload().wait();
            self.language_detector.unload();
            *self.current_model_id.lock().unwrap() = Some(model_id.to_string());
            self.touch_activity();
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_completed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name),
                    error: None,
                },
            );
            return Ok(());
        }

        // Every failure after loading starts must emit a terminal event so the
        // frontend can never remain in its loading state.
        let emit_loading_failed = |error_msg: &str| {
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
        };

        if !model_info.is_downloaded {
            let error_msg = "Model not downloaded";
            emit_loading_failed(error_msg);
            return Err(anyhow::anyhow!(error_msg));
        }

        let model_path = self
            .model_manager
            .get_model_path(model_id)
            .inspect_err(|error| emit_loading_failed(&error.to_string()))?;

        // Clear the id before loading: if the new load fails, status should
        // read "no loaded model", not the model the worker is replacing.
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = None;
        }

        // The backend is chosen at load time. With an explicit
        // `device_index` (the --device-index flag) hard-select that
        // registered device; otherwise re-read the persisted
        // accelerator preference (so an accelerator change marked for
        // reload takes effect here). The worker resolves the selector
        // to a device handle, and rejects an index that isn't a
        // loadable device (a host without GPU access lists none).
        let (backend, device) = match device_index {
            Some(index) => (Backend::Auto, DeviceSelector::Index(index)),
            None => {
                let settings = get_settings(&self.app_handle);
                let accelerator = settings.transcribe_accelerator;
                match resolve_gpu_device(accelerator, settings.transcribe_gpu_device.as_deref()) {
                    // Backend::Auto accepts an exact GPU device.
                    Some(key) => (Backend::Auto, DeviceSelector::Key(key)),
                    // Without a valid exact device, backend selection
                    // handles the retired generic GPU state and host
                    // CPU guard.
                    None => (select_transcribe_backend(accelerator), DeviceSelector::Auto),
                }
            }
        };
        let requested_device = format!("{:?}", device);
        let info = self
            .engine
            .load(LoadSpec {
                path: model_path.clone(),
                backend,
                device,
            })
            .map_err(|e| {
                let error_msg = format!("Failed to load model {}: {}", model_id, e);
                emit_loading_failed(&error_msg);
                anyhow::anyhow!(error_msg)
            })?;
        // Reconcile the registry's advertised capabilities with the
        // loaded model's real ones (GGUF metadata) so badges/gating
        // reflect runtime truth, not the pre-download probe. The
        // load-completed event below triggers the frontend refresh.
        let caps = &info.capabilities;
        self.model_manager.set_runtime_capabilities(
            model_id,
            caps.supports_streaming,
            caps.supports_translate,
            caps.supports_language_detect,
            caps.languages.clone(),
        );
        // The bound backend may differ from the request (e.g. CPU
        // fallback under Auto or after a GPU crash); log what loaded.
        info!(
            "Loaded transcribe-cpp model '{}' (requested {:?}, requested device {}, \
             bound backend '{}', bound device '{}', supports_streaming={}, \
             supports_translate={}, supports_language_detect={})",
            model_id,
            backend,
            requested_device,
            info.backend,
            info.device,
            caps.supports_streaming,
            caps.supports_translate,
            caps.supports_language_detect
        );

        // Update the current model ID
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = Some(model_id.to_string());
        }

        // Reset idle timer so the watcher doesn't immediately unload a just-loaded model
        self.touch_activity();

        // Emit loading completed event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_completed".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: Some(model_info.name.clone()),
                error: None,
            },
        );

        let load_duration = load_start.elapsed();
        debug!(
            "Successfully loaded transcription model: {} (took {}ms)",
            model_id,
            load_duration.as_millis()
        );
        Ok(())
    }

    /// Kicks off the model loading in a background thread if it's not already loaded
    pub fn initiate_model_load(&self) {
        let mut is_loading = self.is_loading.lock().unwrap();
        if *is_loading {
            return;
        }

        let reload_pending = self.reload_model_on_next_use.load(Ordering::Acquire);
        if !reload_pending && self.has_loaded_model() {
            return;
        }

        let settings = get_settings(&self.app_handle);
        #[cfg(target_os = "macos")]
        if settings.selected_model != MACOS_NATIVE_SPEECH_MODEL_ID
            && self.should_use_native_speech(&settings)
        {
            // Do not load an incomplete file or persist a temporary choice.
            // Batch can use the system recognizer immediately.
            return;
        }

        *is_loading = true;
        let self_clone = self.clone();
        thread::spawn(move || {
            if reload_pending {
                self_clone
                    .reload_model_on_next_use
                    .store(false, Ordering::Release);
            }
            if let Err(e) = self_clone.load_model(&settings.selected_model) {
                error!("Failed to load model: {}", e);
            }
            let mut is_loading = self_clone.is_loading.lock().unwrap();
            *is_loading = false;
            self_clone.loading_condvar.notify_all();
        });
    }

    pub fn get_current_model(&self) -> Option<String> {
        let current_model = self.current_model_id.lock().unwrap();
        current_model.clone()
    }

    /// The compute backend the currently-loaded engine is bound to, for
    /// diagnostics (e.g. confirming `--device-index` actually bound a GPU rather
    /// than falling back to CPU/auto); `None` when no model is loaded.
    pub fn current_backend(&self) -> Option<String> {
        self.engine.loaded().map(|info| info.backend)
    }

    /// Whether a file job is queued or running, which holds the model.
    pub fn job_active(&self) -> bool {
        self.job_active.load(Ordering::Acquire)
    }

    pub fn set_job_active(&self, active: bool) {
        self.job_active.store(active, Ordering::Release);
    }

    /// Refuses a model switch, unload or delete while a file job holds the
    /// model.
    pub fn ensure_model_unlocked(&self) -> std::result::Result<(), String> {
        if self.job_active() {
            return Err("The model can't change while a file is transcribing".to_string());
        }
        Ok(())
    }

    /// Whether a live streaming run is currently in flight.
    pub fn is_streaming(&self) -> bool {
        self.stream_active.load(Ordering::Acquire)
    }

    /// Shared handle to the stream router, used by the audio recorder to feed
    /// real-time frames without going through Tauri state on every frame.
    pub fn stream_router(&self) -> Arc<StreamRouter> {
        Arc::clone(&self.router)
    }

    /// Begin a recording's live decode on the held engine's session. Audio
    /// frames pushed via [`StreamRouter::feed`] (captured directly by the
    /// audio recorder) are streamed, with preview text emitted to the overlay
    /// as [`StreamTextEvent`], or cut into long-form chunks decoded as the
    /// recording goes; [`policy::live_decode`] picks which.
    ///
    /// Non-blocking: spawns a worker that waits for any in-progress model load,
    /// then begins the live decode the loaded model calls for. With none, the
    /// worker idles until finalize/cancel and reports `None` so the caller
    /// falls back to batch transcription. Frames sent before the decode begins
    /// queue on the channel and are not lost.
    pub fn start_stream(&self) {
        if self.router.is_open() || self.active_stream_worker.load(Ordering::Acquire) != 0 {
            warn!("start_stream called while a stream worker is already active");
            return;
        }
        let worker_id = self.next_stream_worker_id.fetch_add(1, Ordering::Relaxed);
        if self
            .active_stream_worker
            .compare_exchange(0, worker_id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("start_stream lost a race with another stream worker");
            return;
        }
        let (rx, cancelled) = self.router.open();
        self.stream_active.store(false, Ordering::Release);

        let manager = self.clone();
        thread::spawn(move || manager.run_stream_worker(rx, cancelled, worker_id));
    }

    fn run_stream_worker(
        &self,
        rx: mpsc::Receiver<StreamCmd>,
        cancelled: Arc<AtomicBool>,
        worker_id: u64,
    ) {
        let _worker = StreamWorkerGuard {
            worker_id,
            active_stream_worker: Arc::clone(&self.active_stream_worker),
            stream_active: Arc::clone(&self.stream_active),
        };

        // Wait for any in-progress model load to finish (start_stream races the
        // background load kicked off when recording starts).
        {
            let mut is_loading = self.is_loading.lock().unwrap();
            while *is_loading {
                is_loading = self.loading_condvar.wait(is_loading).unwrap();
            }
        }
        if cancelled.load(Ordering::Acquire) {
            self.router.clear();
            drain_until_finalize(rx);
            return;
        }

        let model_id = self.get_current_model().unwrap_or_default();

        // The loaded model (not the ModelManager copy) is the source of truth
        // for run-path capabilities.
        let Some(info) = self.engine.loaded() else {
            info!(
                "Live preview: model '{}' is not a loaded transcribe-cpp model; \
                 streaming is unavailable, using batch transcription",
                model_id
            );
            self.router.clear();
            drain_until_finalize(rx);
            return;
        };
        let caps = &info.capabilities;
        info!(
            "Live preview: model '{}' arch='{}' variant='{}' supports_streaming={} \
             supports_translate={} languages={:?}",
            model_id,
            info.arch,
            info.variant,
            caps.supports_streaming,
            caps.supports_translate,
            caps.languages,
        );
        let mode = live_decode(
            ModelTraits::of(&info),
            get_settings(&self.app_handle).long_form_chunking,
        );
        let Some(mut session) = LiveSession::begin(self, mode, &info, &model_id) else {
            // Nothing to decode live: the recording is transcribed in batch.
            self.router.clear();
            drain_until_finalize(rx);
            return;
        };
        self.touch_activity();

        while let Ok(cmd) = rx.recv() {
            match cmd {
                StreamCmd::Feed(pcm) => {
                    // Audio queued ahead of a cancel is dropped, not decoded.
                    if cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    self.touch_activity();
                    session.feed(pcm);
                }
                StreamCmd::Finalize {
                    reply,
                    samples,
                    timing_session,
                } => {
                    let outcome = crate::flow::timing::with_asr_session(timing_session, || {
                        session.finish(samples)
                    });
                    let _ = reply.send(outcome);
                    return;
                }
                // Dropping the session resets the worker's stream.
                StreamCmd::Cancel => return,
            }
        }
        // The channel closed without finalize/cancel; dropping the session
        // resets the worker's stream. `_worker` drops after, clearing this
        // worker's flags.
    }

    /// Flush the live decode (a stream, or chunks decoded while recording)
    /// and return what it produced. `recorded_samples` is the length of the
    /// whole recording: a chunked decode that saw less reports no result.
    ///
    /// `Ok(None)` means no usable live result exists and the caller should
    /// fall back to batch transcription. `Err` means the transcription was
    /// cancelled. There is no timeout here: waiting behind a model load is a
    /// queue position, not a failure (#1841), and the engine bounds the
    /// finalize itself by the audio it still has to decode.
    fn finalize_live(&self, recorded_samples: usize) -> Result<Option<RunOutcome>> {
        let Some(route) = self.router.take() else {
            return Ok(None);
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        let finalize = StreamCmd::Finalize {
            reply: reply_tx,
            samples: recorded_samples,
            timing_session: crate::flow::timing::current_asr_session_id(),
        };
        if route.tx.send(finalize).is_err() {
            return Ok(None);
        }
        match reply_rx.recv() {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Ok(None),
        }
    }

    /// Abandon any live decode without producing text (e.g. on cancel). A
    /// chunk already decoding finishes; audio queued behind it is dropped.
    pub fn cancel_stream(&self) {
        if let Some(route) = self.router.take() {
            route.cancelled.store(true, Ordering::Release);
            let _ = route.tx.send(StreamCmd::Cancel);
        }
        self.stream_active.store(false, Ordering::Release);
    }

    /// Stop the transcription in progress (a batch run or stream finalize),
    /// and any queued behind it, for an explicit user cancel. The worker doing
    /// it is stopped; the model stays loaded and the next use starts a fresh
    /// worker for it.
    pub fn cancel_transcription(&self) {
        self.engine.cancel();
    }

    /// Emit a working-phase event to the streaming overlay (spinner + label).
    pub fn emit_stream_working(&self, kind: StreamWorkKind) {
        let _ = StreamPhaseEvent {
            phase: StreamPhase::Working,
            kind: Some(kind),
        }
        .emit(&self.app_handle);
    }

    /// Emit a committed transcript snapshot (e.g. instant Stage-A text right
    /// before Stage-B LLM polishing begins) so the user sees their transcribed
    /// words immediately and can click `[Paste raw]` at any moment.
    pub fn emit_stream_snapshot(&self, committed: &str) {
        emit_stream_text(&self.app_handle, committed, "");
    }

    /// Transcribe a whole clip in batch: a re-transcribed history entry, or
    /// a file from the CLI.
    pub fn transcribe(&self, audio: Vec<f32>) -> Result<String> {
        Ok(self.complete(Instant::now(), audio, None)?.text)
    }

    /// Transcribe a finished recording: what its live decode produced, or a
    /// batch run of the whole recording when there is no live result.
    pub fn transcribe_recording(&self, samples: Vec<f32>) -> Result<Transcription> {
        let started = Instant::now();
        let live = self.finalize_live(samples.len())?;
        self.complete(started, samples, live)
    }

    /// The final text of one transcription, from its live result or else a
    /// batch run of `audio`. Every transcription finishes here.
    fn complete(
        &self,
        started: Instant,
        audio: Vec<f32>,
        live: Option<RunOutcome>,
    ) -> Result<Transcription> {
        #[cfg(debug_assertions)]
        if std::env::var("HANDY_FORCE_TRANSCRIPTION_FAILURE").is_ok() {
            return Err(anyhow::anyhow!(
                "Simulated transcription failure (HANDY_FORCE_TRANSCRIPTION_FAILURE)"
            ));
        }

        let audio_len = audio.len();
        debug!("Audio vector length: {}", audio_len);
        if audio.is_empty() {
            debug!("Empty audio vector");
            self.maybe_unload_immediately("empty audio");
            return Ok(Transcription {
                text: String::new(),
                language: None,
            });
        }

        let outcome = match live {
            Some(outcome) => outcome,
            None => self.decode_batch(audio)?,
        };
        let settings = get_settings(&self.app_handle);
        let language = outcome
            .setup
            .spoken_language(outcome.model_detected_language.as_deref());
        let final_result = finish_text(outcome, &settings);

        let translation_note = if settings.translate_to_english {
            " (translated)"
        } else {
            ""
        };
        // Real-time factor. Input PCM is 16 kHz mono, so audio length in seconds
        // is samples / 16000. `speedup` is audio_secs / elapsed_secs — e.g. 4.00x
        // means transcribed 4x faster than real time
        let elapsed_secs = started.elapsed().as_secs_f64();
        let audio_secs = audio_len as f64 / 16_000.0;
        let speedup = real_time_factor(audio_secs, elapsed_secs);
        info!(
            "Transcription completed in {:.2}s for {:.2}s of audio ({:.2}x real-time){}",
            elapsed_secs, audio_secs, speedup, translation_note
        );

        if final_result.is_empty() {
            info!("Transcription result is empty");
        } else {
            info!(
                "Transcription result: {}",
                crate::utils::redact_text(&final_result)
            );
        }

        self.maybe_unload_immediately("transcription");

        Ok(Transcription {
            text: final_result,
            language: Some(language),
        })
    }

    /// Decode a whole clip with the loaded model, loading it first if a load
    /// is under way.
    fn decode_batch(&self, audio: Vec<f32>) -> Result<RunOutcome> {
        // A cancel from here on (during the model-load wait or chunk planning
        // included) stops every run of this transcription.
        let mark = self.engine.cancel_mark();
        self.touch_activity();
        #[cfg(target_os = "macos")]
        {
            let settings = get_settings(&self.app_handle);
            if self.should_use_native_speech(&settings) {
                return self.decode_native_speech(&audio, &settings);
            }
        }
        let batch = self.batch_context(&audio)?;
        self.transcribe_cpp(mark, batch, audio)
    }

    #[cfg(target_os = "macos")]
    fn decode_native_speech(&self, audio: &[f32], settings: &AppSettings) -> Result<RunOutcome> {
        let locale = macos_speech::resolve_locale(
            &settings.selected_language,
            &settings.spoken_languages,
            macos_speech::is_available,
        )?;
        let text = macos_speech::transcribe_pcm_16k(audio, locale)?;
        let language = locale.map(|locale| {
            crate::managers::model::canonical_language_code(locale).to_ascii_lowercase()
        });
        let source = match language.as_deref() {
            Some(lang)
                if crate::managers::model::canonical_language_code(
                    settings.selected_language.trim(),
                )
                .eq_ignore_ascii_case(lang) =>
            {
                LanguageSource::Selected
            }
            _ => LanguageSource::Fallback,
        };
        Ok(RunOutcome {
            text,
            model_detected_language: None,
            setup: RunSetup {
                run_options: RunOptions::default(),
                languages: language.iter().cloned().collect(),
                applied_language_hint: language.clone(),
                language: LanguageDecision { language, source },
                output_was_translated: false,
                script_pref: crate::flow::multilingual::active_hindustani_script(&self.app_handle),
                spoken_languages: settings.spoken_languages.clone(),
                selected_language: settings.selected_language.clone(),
                urdu_retry_options: None,
                spoken_candidate_options: Vec::new(),
                prior_chunk_language: None,
                all_custom_words_prompted: false,
            },
        })
    }

    /// Waits out any model load, then reads what a batch transcription of
    /// `audio` runs with: the loaded model, the settings, and the language
    /// resolved for it.
    fn batch_context(&self, audio: &[f32]) -> Result<BatchContext> {
        {
            let mut is_loading = self.is_loading.lock().unwrap();
            while *is_loading {
                is_loading = self.loading_condvar.wait(is_loading).unwrap();
            }
        }
        let Some(info) = self.engine.loaded() else {
            #[cfg(target_os = "macos")]
            if self.get_current_model().as_deref() == Some(MACOS_NATIVE_SPEECH_MODEL_ID) {
                return Err(anyhow::anyhow!(
                    "Choose a downloaded model for background file transcription."
                ));
            }
            return Err(anyhow::anyhow!("Model is not loaded for transcription."));
        };

        let settings = get_settings(&self.app_handle);
        // Validate against the model that's actually loaded (which can differ
        // from settings.selected_model when a caller loaded a specific model —
        // e.g. the --transcribe-file path's --model), not the persisted
        // selection.
        let active_model = self
            .get_current_model()
            .unwrap_or_else(|| settings.selected_model.clone());
        let plan = self.language_plan(&settings, &active_model, &info);
        let setup = self.setup_for(&info, &settings, &plan, audio);
        Ok(BatchContext {
            info,
            settings,
            active_model,
            plan,
            setup,
        })
    }

    /// How a transcription on `model_id` picks its language, decided before
    /// any audio. Computed fresh each time and never written back to
    /// settings, so the persisted intent survives switching models and back.
    fn language_plan(&self, settings: &AppSettings, model_id: &str, info: &LoadedInfo) -> Plan {
        let model = self.model_manager.get_model_info(model_id);
        crate::language_detection::plan_for(
            settings,
            model.as_ref(),
            info.capabilities.supports_streaming,
        )
    }

    /// Resolves `plan` with the transcription's `audio` into the run setup
    /// every run of it uses. Detection runs only for a detect plan.
    fn setup_for(
        &self,
        info: &LoadedInfo,
        settings: &AppSettings,
        plan: &Plan,
        audio: &[f32],
    ) -> RunSetup {
        // No VAD pass when the detector isn't there to use the speech.
        let speech = if !plan.detects() {
            Some(Vec::new())
        } else if self.model_manager.language_detector_path().is_some() {
            Some(self.detection_speech(settings, audio))
        } else {
            None
        };
        let language = plan.decide(speech.as_deref(), |speech, allowed| {
            let path = self
                .model_manager
                .language_detector_path()
                .ok_or(IdentifyError::Missing)?;
            self.language_detector
                .identify(&path, speech, allowed)
                .map_err(IdentifyError::Failed)
        });
        info!(
            "Transcription language: {} ({:?})",
            language.intent(),
            language.source
        );
        let script_pref = crate::flow::multilingual::active_hindustani_script(&self.app_handle);
        run_setup(info, settings, language, script_pref)
    }

    /// The first window of speech in `audio`: silence is always cut out by
    /// VAD first, whatever `vad_enabled` says, so the window is all speech.
    fn detection_speech(&self, settings: &AppSettings, audio: &[f32]) -> Vec<f32> {
        create_smoothed_detector(settings.vad_backend, &self.app_handle)
            .and_then(|mut vad| {
                speech_head(
                    &mut vad,
                    audio,
                    settings.language_detection_window_samples(),
                )
            })
            .unwrap_or_else(|e| {
                warn!("Language detection: VAD failed: {e:#}");
                Vec::new()
            })
    }

    fn transcribe_cpp(
        &self,
        mark: CancelMark,
        batch: BatchContext,
        audio: Vec<f32>,
    ) -> Result<RunOutcome> {
        let BatchContext {
            info,
            settings,
            active_model,
            plan,
            setup,
        } = batch;
        let vad_backend = settings.vad_backend;
        // `mark` covers every chunk, so a cancel between two stops the rest.
        // A failed chunk fails the whole transcription.
        let (transcripts, setup) =
            match batch_decode(ModelTraits::of(&info), settings.long_form_chunking) {
                BatchDecode::Chunked => {
                    let chunk_setup = if settings
                        .selected_language
                        .trim()
                        .eq_ignore_ascii_case("auto")
                    {
                        ChunkSetup::Pending(Box::new(PendingSetup {
                            plan,
                            info,
                            settings,
                        }))
                    } else {
                        ChunkSetup::Ready(Box::new(setup))
                    };
                    ChunkedDecode::decode_clip(
                        self,
                        mark,
                        chunk_setup,
                        audio,
                        &active_model,
                        vad_backend,
                    )
                    .map_err(engine_error)?
                }
                BatchDecode::Whole => {
                    let mut setup = setup;
                    let transcript = self
                        .decode_chunk_for_spoken_languages(mark, audio, &mut setup, &active_model)
                        .map_err(engine_error)?;
                    (vec![(transcript, false)], setup)
                }
            };
        record_last_decode_chunk_count(transcripts.len() as u32);
        if crate::flow::timing::current_asr_session_id().is_some() {
            crate::flow::timing::record_chunk_stats(0, transcripts.len() as u32);
        }
        let (text, model_detected_language) = join_chunks(transcripts);
        Ok(RunOutcome {
            setup,
            text,
            model_detected_language,
        })
    }

    /// Splits a bilingual `"auto"` chunk at natural clause pauses when needed,
    /// decodes each clause with `spoken_languages` disambiguation + Urdu script
    /// governance, and merges clause outputs back into a single chunk `Transcript`
    /// with chunk-relative millisecond segment/token offsets preserved.
    fn decode_chunk_for_spoken_languages(
        &self,
        mark: CancelMark,
        pcm: Vec<f32>,
        setup: &mut RunSetup,
        active_model: &str,
    ) -> Result<Transcript, EngineError> {
        let clause_ranges = if !setup.output_was_translated {
            crate::flow::multilingual::split_bilingual_clauses(
                &pcm,
                &setup.spoken_languages,
                &setup.selected_language,
            )
        } else {
            vec![0..pcm.len()]
        };

        // Coalesce micro-clauses across all clips (`split_bilingual_clauses`
        // only splits clips <= 20s, so gating on >= 25s left coalescing dead
        // and shattered 4.5-14s live chunks into 3-4 separate 30s Whisper runs).
        let clause_ranges = coalesce_short_clauses(clause_ranges);
        if clause_ranges.len() <= 1 {
            return self.run_clause_for_spoken_languages(mark, pcm, setup, active_model);
        }

        info!(
            "Splitting {:.1}s bilingual auto-mode audio into {} clauses",
            pcm.len() as f64 / 16_000.0,
            clause_ranges.len()
        );
        let mut text_parts: Vec<String> = Vec::with_capacity(clause_ranges.len());
        let mut merged_segments = Vec::new();
        let mut merged_tokens = Vec::new();
        let mut first_language: Option<String> = None;

        for range in clause_ranges {
            let offset_ms = (range.start as i64 * 1000) / 16_000;
            let clause_pcm = pcm[range].to_vec();
            let transcript =
                self.run_clause_for_spoken_languages(mark, clause_pcm, setup, active_model)?;
            if first_language.is_none() {
                first_language = transcript.language.clone();
            }
            for mut seg in transcript.segments {
                seg.t0_ms = seg.t0_ms.saturating_add(offset_ms);
                seg.t1_ms = seg.t1_ms.saturating_add(offset_ms);
                merged_segments.push(seg);
            }
            for mut tok in transcript.tokens {
                tok.t0_ms = tok.t0_ms.saturating_add(offset_ms);
                tok.t1_ms = tok.t1_ms.saturating_add(offset_ms);
                merged_tokens.push(tok);
            }
            let normalized = crate::flow::multilingual::normalize_script_output(
                &transcript.text,
                &setup.spoken_languages,
                setup.script_pref,
            );
            let trimmed = normalized.trim();
            if !trimmed.is_empty() {
                text_parts.push(trimmed.to_string());
            }
        }

        let mut combined = Transcript::default();
        combined.text = text_parts.join(" ");
        combined.language = first_language;
        combined.segments = merged_segments;
        combined.tokens = merged_tokens;
        Ok(combined)
    }

    /// Decodes one clause/chunk, constraining Whisper's `"auto"` output to the
    /// user's `spoken_languages` when Pass 1 hallucinates an out-of-profile
    /// language/script or repetition loop, and retrying Hindustani in Urdu
    /// Perso-Arabic script when appropriate.
    fn run_clause_for_spoken_languages(
        &self,
        mark: CancelMark,
        pcm: Vec<f32>,
        setup: &mut RunSetup,
        active_model: &str,
    ) -> Result<Transcript, EngineError> {
        let is_auto = setup.selected_language.trim().eq_ignore_ascii_case("auto");
        let needs_retry_buffer = is_auto
            && !setup.output_was_translated
            && (!setup.spoken_candidate_options.is_empty() || setup.urdu_retry_options.is_some());
        let retry_pcm = needs_retry_buffer.then(|| pcm.clone());
        let rms_energy = Some(lowyap_flow::pcm_rms_energy(&pcm));

        let mut transcript = self.run_batch(mark, pcm, &setup.run_options, active_model)?;
        if let Some(retry_pcm) = retry_pcm {
            transcript =
                self.resolve_clause_transcript(mark, retry_pcm, setup, active_model, transcript)?;
        }

        // Only speech tokens should contribute to the guard's confidence check.
        let speech_token_probs: Vec<f32> = transcript
            .tokens
            .iter()
            .filter_map(|token| {
                let text = token.text.trim();
                let special = (text.starts_with('[') && text.ends_with(']'))
                    || (text.starts_with("<|") && text.ends_with("|>"));
                (!special).then_some(token.p)
            })
            .collect();
        let mean_token_prob = lowyap_flow::geometric_mean_token_prob(&speech_token_probs);
        transcript.text = lowyap_flow::guard_transcript_text(
            &transcript.text,
            &setup.spoken_languages,
            setup.script_pref.into(),
            lowyap_flow::SilenceGuardContext {
                rms_energy,
                no_speech_prob: None,
                mean_token_prob,
                output_was_translated: setup.output_was_translated,
            },
        );
        Ok(transcript)
    }

    fn resolve_clause_transcript(
        &self,
        mark: CancelMark,
        retry_pcm: Vec<f32>,
        setup: &mut RunSetup,
        active_model: &str,
        transcript: Transcript,
    ) -> Result<Transcript, EngineError> {
        let pass1_probs: Vec<f32> = transcript.tokens.iter().map(|tok| tok.p).collect();
        let mean_prob = crate::flow::multilingual::geometric_mean_token_prob(&pass1_probs);

        if !setup.spoken_candidate_options.is_empty()
            && crate::flow::multilingual::needs_spoken_language_disambiguation(
                transcript.language.as_deref(),
                &transcript.text,
                mean_prob,
                &setup.spoken_languages,
                &setup.selected_language,
            )
        {
            let candidates = setup.spoken_candidate_options.clone();
            info!(
                "Disambiguating auto clause across spoken_languages {:?} (pass1 lang={:?}, mean_p={:?})",
                candidates.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
                transcript.language,
                mean_prob
            );

            let mut best: Option<(String, f32, Transcript)> = None;
            for (cand_lang, cand_opts) in candidates {
                let mut cand = self.run_batch(mark, retry_pcm.clone(), &cand_opts, active_model)?;
                if cand.language.is_none() {
                    cand.language = Some(cand_lang.clone());
                }
                let probs: Vec<f32> = cand.tokens.iter().map(|tok| tok.p).collect();
                let continuity_bonus = if setup
                    .prior_chunk_language
                    .as_deref()
                    .is_some_and(|prev| prev.eq_ignore_ascii_case(&cand_lang))
                {
                    0.25
                } else {
                    0.0
                };
                let score = crate::flow::multilingual::score_candidate_transcript(
                    &cand.text,
                    &cand_lang,
                    &probs,
                    &setup.spoken_languages,
                ) + continuity_bonus;
                debug!(
                    "Spoken-language candidate '{}' scored {:.3} (tokens={})",
                    cand_lang,
                    score,
                    probs.len()
                );
                if best
                    .as_ref()
                    .is_none_or(|(_, best_score, _)| score > *best_score)
                {
                    best = Some((cand_lang, score, cand));
                }
            }

            if let Some((chosen_lang, chosen_score, chosen)) = best {
                if chosen_score > -5.0 {
                    info!(
                        "Selected spoken_languages candidate '{}' (score={:.3})",
                        chosen_lang, chosen_score
                    );
                    if setup.applied_language_hint.is_some() {
                        setup.applied_language_hint = Some(chosen_lang.clone());
                        setup.language.language = Some(chosen_lang);
                    }
                    return Ok(chosen);
                }
            }
        }

        let skip_auto_urdu_retry_for_roman_hinglish = setup.script_pref
            == crate::flow::multilingual::HindustaniScript::Auto
            && setup
                .spoken_languages
                .iter()
                .any(|l| l.split(['-', '_']).next() == Some("en"))
            && !setup
                .spoken_languages
                .iter()
                .any(|l| l.split(['-', '_']).next() == Some("ar"))
            && transcript.language.as_deref() == Some("hi")
            && !lowyap_flow::contains_perso_arabic(&transcript.text);

        if let Some(urdu_options) = setup
            .urdu_retry_options
            .clone()
            .filter(|_| !skip_auto_urdu_retry_for_roman_hinglish)
        {
            if crate::flow::multilingual::should_retry_chunk_as_urdu(
                transcript.language.as_deref(),
                &transcript.text,
                &setup.spoken_languages,
                setup.script_pref,
                &setup.selected_language,
            ) {
                info!(
                    "Retrying transcription clause as Urdu ('ur') after Hindi/Devanagari output (detected={:?})",
                    transcript.language
                );
                let mut retried = self.run_batch(mark, retry_pcm, &urdu_options, active_model)?;
                let guarded = lowyap_flow::guard_transcript_text(
                    &retried.text,
                    &setup.spoken_languages,
                    setup.script_pref.into(),
                    lowyap_flow::SilenceGuardContext {
                        output_was_translated: setup.output_was_translated,
                        ..Default::default()
                    },
                );
                if !guarded.trim().is_empty() && lowyap_flow::contains_perso_arabic(&guarded) {
                    retried.text = guarded;
                    if retried.language.is_none() {
                        retried.language = Some("ur".to_string());
                    }
                    if setup.applied_language_hint.is_some() {
                        setup.applied_language_hint = Some("ur".to_string());
                        setup.language.language = Some("ur".to_string());
                    }
                    return Ok(retried);
                }
            }
        }

        Ok(transcript)
    }

    /// One batch run of a transcription that began at `mark`, so a cancel
    /// since then stops it. Keeps the idle watcher from unloading the model
    /// between the runs of a chunked transcription.
    fn run_batch(
        &self,
        mark: CancelMark,
        pcm: Vec<f32>,
        run_options: &RunOptions,
        active_model: &str,
    ) -> Result<Transcript, EngineError> {
        self.touch_activity();
        if lowyap_flow::is_non_speech_energy_floor(&pcm, 16_000) {
            return Ok(Transcript::default());
        }
        self.engine
            .transcribe_since(mark, pcm, run_options.clone())
            .inspect_err(|e| {
                if !matches!(e, EngineError::Cancelled) {
                    self.forget_model_if_engine_dropped(active_model, &e.to_string());
                }
            })
    }
    #[cfg(target_os = "macos")]
    fn should_use_native_speech(&self, settings: &AppSettings) -> bool {
        if settings.selected_model == MACOS_NATIVE_SPEECH_MODEL_ID
            || self.get_current_model().as_deref() == Some(MACOS_NATIVE_SPEECH_MODEL_ID)
        {
            return native_speech_ready(settings);
        }
        // Only fall back to Instant Voice when the selected `.gguf` model is not yet
        // downloaded on disk (e.g., first-run zero-wait or background HD download).
        // Downloaded `.gguf` models that unloaded on idle wait on `is_loading` in `batch_context`.
        !self.has_loaded_model()
            && self
                .model_manager
                .get_model_path(&settings.selected_model)
                .is_err()
            && native_speech_ready(settings)
    }
}

#[cfg(target_os = "macos")]
fn native_speech_ready(settings: &AppSettings) -> bool {
    macos_speech::resolve_locale(
        &settings.selected_language,
        &settings.spoken_languages,
        macos_speech::is_available,
    )
    .is_ok()
}

/// What a batch transcription runs with, read once before its first run.
struct BatchContext {
    info: LoadedInfo,
    settings: AppSettings,
    active_model: String,
    plan: Plan,
    setup: RunSetup,
}

/// A finished transcription.
pub struct Transcription {
    pub text: String,
    /// The language it ran in and how that was chosen; `None` when there was
    /// no audio to run.
    pub language: Option<LanguageDecision>,
}

/// What one transcription decoded (live or in batch), with how its runs were
/// set up, which post-processing needs to know.
struct RunOutcome {
    setup: RunSetup,
    text: String,
    /// The language the model itself detected, if it reported one.
    model_detected_language: Option<String>,
}

/// One transcription's final text: its output language resolved, then
/// post-processed.
fn finish_text(outcome: RunOutcome, settings: &AppSettings) -> String {
    let RunOutcome {
        setup,
        text,
        model_detected_language,
    } = outcome;
    let script_pref = setup.script_pref;
    let custom_words_prompted = setup.custom_words_prompted();
    let output_language = setup.output_language(settings, model_detected_language);
    debug!("Output language evidence: {:?}", output_language);
    // Skip fuzzy correction if the model already got the custom words.
    post_process_transcription_text_with_script(
        text,
        settings,
        custom_words_prompted,
        &output_language,
        &setup.languages,
        script_pref,
    )
}

/// How every run of one transcription (each chunk, or a whole clip) is
/// configured on the loaded model, and what post-processing needs to know.
struct RunSetup {
    run_options: RunOptions,
    /// The language every run uses, and how it was chosen.
    language: LanguageDecision,
    languages: Vec<String>,
    applied_language_hint: Option<String>,
    output_was_translated: bool,
    script_pref: crate::flow::multilingual::HindustaniScript,
    spoken_languages: Vec<String>,
    selected_language: String,
    urdu_retry_options: Option<RunOptions>,
    spoken_candidate_options: Vec<(String, RunOptions)>,
    prior_chunk_language: Option<String>,
    all_custom_words_prompted: bool,
}

impl RunSetup {
    /// Custom words went to the model as true hotword vocabulary; skip fuzzy correction
    /// only when every custom word was sent to a non-Whisper vocabulary backend.
    fn custom_words_prompted(&self) -> bool {
        self.all_custom_words_prompted
    }

    /// The language the runs were in. When the model decided, the language
    /// it reported (if any).
    fn spoken_language(&self, model_detected_language: Option<&str>) -> LanguageDecision {
        let mut decision = self.language.clone();
        if decision.language.is_none() {
            decision.language = model_detected_language
                .filter(|language| !language.is_empty() && *language != "auto")
                .map(str::to_string);
        }
        decision
    }

    /// What the output language is known to be, given the model's own
    /// detection when it reported one.
    fn output_language(
        &self,
        settings: &AppSettings,
        model_detected_language: Option<String>,
    ) -> OutputLanguageEvidence {
        with_model_detected_language(
            resolve_output_language_evidence(
                settings,
                self.applied_language_hint.as_deref(),
                self.language.source == LanguageSource::Detected,
                &self.languages,
                self.output_was_translated,
            ),
            model_detected_language,
        )
    }
}

/// The loaded model's live capabilities (cheap GGUF-metadata reads) are the
/// source of truth, not the ModelManager copy; translate is gated on what the
/// model advertises.
fn run_setup(
    info: &LoadedInfo,
    settings: &AppSettings,
    language: LanguageDecision,
    script_pref: crate::flow::multilingual::HindustaniScript,
) -> RunSetup {
    let model_supports_translate = info.capabilities.supports_translate;
    let languages = info.capabilities.languages.clone();
    debug!(
        "transcribe-cpp model '{}' on '{}': initial_prompt={}, vocabulary={}, translate={}, languages={:?}",
        settings.selected_model,
        info.backend,
        info.supports_initial_prompt,
        info.supports_vocabulary,
        model_supports_translate,
        languages
    );

    let run_plan = transcribe_cpp_run_plan(
        settings.translate_to_english,
        language.intent(),
        &languages,
        model_supports_translate,
    );
    let output_was_translated = run_plan.target_language.as_deref() == Some("en");
    let applied_language_hint = run_plan.language.clone();
    if applied_language_hint.is_none()
        && matches!(
            language.source,
            LanguageSource::Detected | LanguageSource::Fallback
        )
    {
        if let Some(chosen) = &language.language {
            warn!(
                "Language '{}' isn't one the loaded model advertises ({:?}); it runs without a language",
                chosen, languages
            );
        }
    }
    // The model wasn't told a language, so record what it reports instead.
    let language = if applied_language_hint.is_none() && language.language.is_some() {
        LanguageDecision {
            language: None,
            source: LanguageSource::Model,
        }
    } else {
        language
    };

    let is_whisper = info.arch.eq_ignore_ascii_case("whisper");
    let vocabulary =
        vocabulary_for_model(&info.arch, info.supports_vocabulary, &settings.custom_words);
    let all_custom_words_prompted =
        info.supports_vocabulary && !is_whisper && !settings.custom_words.is_empty();
    let prompt = crate::flow::multilingual::build_initial_prompt(
        info.supports_initial_prompt,
        info.supports_vocabulary,
        run_plan.language.as_deref(),
        &settings.spoken_languages,
        script_pref,
        &settings.custom_words,
    );

    let token_timestamps = if info.capabilities.max_timestamp_kind == TimestampKind::Token {
        TimestampKind::Token
    } else {
        TimestampKind::Auto
    };

    // Whisper defaults in transcribe-cpp run a 6-tier temperature ladder
    // (`0.0, 0.2, 0.4, 0.6, 0.8, 1.0`) where the `T=1.0` random sample wins
    // unconditionally whenever `no_speech_prob <= 0.60`, stalling hesitation
    // chunks for 7-10 s and spewing multilingual hallucinations. Pin Pass 1
    // to deterministic greedy decode (`T=0.0`, `temperature_inc=0.0`) with
    // tight compression/no-speech gating, and pin forced `<|lang|>` retries
    // (`urdu_retry_options` / `spoken_candidate_options`) to reject mismatched
    // language hypotheses immediately (`no_speech_thold = -1.0`, `logprob_thold = -0.85`).
    let whisper_pass1_ext = is_whisper.then(|| {
        RunExtension::Whisper(WhisperRunOptions {
            temperature: Some(0.0),
            temperature_inc: Some(0.0),
            compression_ratio_thold: Some(2.2),
            logprob_thold: Some(-1.0),
            no_speech_thold: Some(0.30),
            ..Default::default()
        })
    });
    let whisper_strict_ext = is_whisper.then(|| {
        RunExtension::Whisper(WhisperRunOptions {
            temperature: Some(0.0),
            temperature_inc: Some(0.0),
            compression_ratio_thold: Some(2.2),
            logprob_thold: Some(-0.85),
            no_speech_thold: Some(-1.0),
            ..Default::default()
        })
    });

    let urdu_retry_options = if !output_was_translated
        && applied_language_hint.as_deref() != Some("ur")
        && languages.iter().any(|l| l == "ur")
    {
        Some(RunOptions {
            task: Task::Transcribe,
            timestamps: token_timestamps,
            language: Some("ur".to_string()),
            target_language: None,
            vocabulary: vocabulary.clone(),
            prompt: crate::flow::multilingual::build_initial_prompt(
                info.supports_initial_prompt,
                info.supports_vocabulary,
                Some("ur"),
                &settings.spoken_languages,
                script_pref,
                &settings.custom_words,
            ),
            family: whisper_strict_ext.clone(),
            ..Default::default()
        })
    } else {
        None
    };

    let spoken_candidate_options = if !output_was_translated
        && run_plan.language.is_none()
        && settings
            .selected_language
            .trim()
            .eq_ignore_ascii_case("auto")
        && !settings.spoken_languages.is_empty()
    {
        crate::flow::multilingual::candidate_languages_for_spoken(
            &settings.spoken_languages,
            script_pref,
            &languages,
        )
        .into_iter()
        .map(|cand_lang| {
            let cand_prompt = crate::flow::multilingual::build_initial_prompt(
                info.supports_initial_prompt,
                info.supports_vocabulary,
                Some(&cand_lang),
                &settings.spoken_languages,
                script_pref,
                &settings.custom_words,
            );
            let opts = RunOptions {
                task: Task::Transcribe,
                timestamps: token_timestamps,
                language: Some(cand_lang.clone()),
                target_language: None,
                vocabulary: vocabulary.clone(),
                prompt: cand_prompt,
                family: whisper_strict_ext.clone(),
                ..Default::default()
            };
            (cand_lang, opts)
        })
        .collect()
    } else {
        Vec::new()
    };

    let pass1_timestamps = if !spoken_candidate_options.is_empty() {
        token_timestamps
    } else {
        TimestampKind::Auto
    };

    let run_options = RunOptions {
        task: run_plan.task,
        timestamps: pass1_timestamps,
        language: run_plan.language,
        target_language: run_plan.target_language,
        vocabulary,
        prompt,
        family: whisper_pass1_ext,
        ..Default::default()
    };

    debug!(
        "transcribe-cpp run: task={:?}, language={:?}, vocabulary_terms={}, prompt_len={}, spoken_candidates={}",
        run_options.task,
        run_options.language,
        run_options.vocabulary.len(),
        run_options.prompt.as_ref().map_or(0, String::len),
        spoken_candidate_options.len()
    );

    RunSetup {
        run_options,
        language,
        languages,
        applied_language_hint,
        output_was_translated,
        script_pref,
        spoken_languages: settings.spoken_languages.clone(),
        selected_language: settings.selected_language.clone(),
        urdu_retry_options,
        spoken_candidate_options,
        prior_chunk_language: None,
        all_custom_words_prompted,
    }
}

/// A batch run's error: a cancel stays recognisable as one.
fn engine_error(e: EngineError) -> anyhow::Error {
    match e {
        EngineError::Cancelled => EngineError::Cancelled.into(),
        e => anyhow::anyhow!("transcribe-cpp transcription failed: {}", e),
    }
}

fn coalesce_short_clauses(ranges: Vec<std::ops::Range<usize>>) -> Vec<std::ops::Range<usize>> {
    let min_samples =
        lowyap_ports::ChunkingPolicy::live_dictation().min_bilingual_clause_ms as usize * 16;
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = merged.last_mut() {
            if previous.end == range.start && previous.end - previous.start < min_samples {
                previous.end = range.end;
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

static LAST_DECODE_CHUNK_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub(crate) fn record_last_decode_chunk_count(count: u32) {
    LAST_DECODE_CHUNK_COUNT.store(count, Ordering::Relaxed);
}

/// Number of chunks decoded during the most recent transcription call in this process.
pub fn last_decode_chunk_count() -> u32 {
    LAST_DECODE_CHUNK_COUNT.load(Ordering::Relaxed)
}

/// One transcription's text from its chunks, in order, and the model's
/// detected language (Whisper's audio-based LID, auto mode only; `None` when a
/// language hint was passed). Chunks that disagree leave it unknown. A single
/// transcript passes through untouched.
fn join_chunks(transcripts: Vec<(Transcript, bool)>) -> (String, Option<String>) {
    let language = agreed_language(transcripts.iter().map(|(t, _)| t));
    let text = match <[(Transcript, bool); 1]>::try_from(transcripts) {
        Ok([(one, _)]) => one.text,
        Err(many) => chunking::join_transcript_boundaries(
            &many
                .iter()
                .map(|(t, ov)| (t.text.as_str(), *ov))
                .collect::<Vec<_>>(),
        ),
    };
    (text, language)
}

/// The language every chunk's model reported, if they all agree.
fn agreed_language<'a>(transcripts: impl IntoIterator<Item = &'a Transcript>) -> Option<String> {
    let mut detected = transcripts.into_iter().map(|t| t.language.clone());
    detected
        .next()
        .flatten()
        .filter(|first| detected.all(|language| language.as_ref() == Some(first)))
}

fn emit_stream_text(app_handle: &AppHandle, committed: &str, tentative: &str) {
    let _ = StreamTextEvent {
        committed: committed.to_string(),
        tentative: tentative.to_string(),
    }
    .emit(app_handle);
}

fn lock_perf(perf: &Mutex<StreamPerf>) -> MutexGuard<'_, StreamPerf> {
    perf.lock().unwrap_or_else(|e| e.into_inner())
}

struct StreamPerf {
    feed_count: u64,
    emit_count: u64,
    streamed_samples: u64,
    stream_compute_elapsed: Duration,
    last_log: Instant,
    latest_revision: i32,
    latest_input_received_ms: i64,
    latest_audio_committed_ms: i64,
    latest_buffered_ms: i64,
}

impl StreamPerf {
    fn new() -> Self {
        Self {
            feed_count: 0,
            emit_count: 0,
            streamed_samples: 0,
            stream_compute_elapsed: Duration::ZERO,
            last_log: Instant::now(),
            latest_revision: 0,
            latest_input_received_ms: 0,
            latest_audio_committed_ms: 0,
            latest_buffered_ms: 0,
        }
    }

    fn record_feed(&mut self, samples: usize) {
        self.feed_count += 1;
        self.streamed_samples += samples as u64;
    }

    fn record_compute(&mut self, elapsed: Duration) {
        self.stream_compute_elapsed += elapsed;
    }

    fn record_update(&mut self, update: &transcribe_cpp::StreamUpdate) {
        self.latest_revision = update.revision;
        self.latest_input_received_ms = update.input_received_ms;
        self.latest_audio_committed_ms = update.audio_committed_ms;
        self.latest_buffered_ms = update.buffered_ms;
    }

    fn record_emit(&mut self) {
        self.emit_count += 1;
    }

    fn maybe_log(&mut self) {
        if self.last_log.elapsed() < STREAM_PERF_LOG_INTERVAL {
            return;
        }

        let audio_secs = self.audio_secs();
        let compute_secs = self.compute_secs();
        debug!(
            "Live preview perf: {:.2}s streamed audio, {:.2}s model compute ({:.2}x real-time), \
             input_received={:.2}s, committed_audio={:.2}s, buffered={}ms, revision={}, \
             {} frames fed, {} updates emitted",
            audio_secs,
            compute_secs,
            real_time_factor(audio_secs, compute_secs),
            self.latest_input_received_ms as f64 / 1000.0,
            self.latest_audio_committed_ms as f64 / 1000.0,
            self.latest_buffered_ms,
            self.latest_revision,
            self.feed_count,
            self.emit_count,
        );
        self.last_log = Instant::now();
    }

    fn log_finalized(&self, chars: usize) {
        let audio_secs = self.audio_secs();
        let compute_secs = self.compute_secs();
        info!(
            "Live preview finalized in {:.2}s model compute for {:.2}s streamed audio ({:.2}x real-time): \
             input_received={:.2}s, committed_audio={:.2}s, buffered={}ms, revision={}, \
             {} frames fed, {} updates emitted, {} chars",
            compute_secs,
            audio_secs,
            real_time_factor(audio_secs, compute_secs),
            self.latest_input_received_ms as f64 / 1000.0,
            self.latest_audio_committed_ms as f64 / 1000.0,
            self.latest_buffered_ms,
            self.latest_revision,
            self.feed_count,
            self.emit_count,
            chars
        );
    }

    fn audio_secs(&self) -> f64 {
        self.streamed_samples as f64 / 16_000.0
    }

    fn compute_secs(&self) -> f64 {
        self.stream_compute_elapsed.as_secs_f64()
    }
}

fn real_time_factor(audio_secs: f64, compute_secs: f64) -> f64 {
    if compute_secs > 0.0 {
        audio_secs / compute_secs
    } else {
        0.0
    }
}

/// Resolve how confidently Handy knows the language of the text produced by a
/// transcription run. The UI language is deliberately not part of this
/// decision.
fn resolve_output_language_evidence(
    settings: &AppSettings,
    applied_language_hint: Option<&str>,
    detected: bool,
    supported_languages: &[String],
    translated_to_english: bool,
) -> OutputLanguageEvidence {
    if translated_to_english {
        return OutputLanguageEvidence::TranslatedToEnglish;
    }

    // Stored language intent is only evidence when this specific engine run
    // actually received the hint. Some multilingual engines (notably Parakeet
    // V3) always auto-detect and ignore Handy's selection; transcribe-cpp also
    // drops a requested hint when the loaded model does not advertise it.
    if let Some(language) = applied_language_hint.filter(|lang| !lang.is_empty() && *lang != "auto")
    {
        if detected {
            return OutputLanguageEvidence::Detected(language.to_string());
        }
        if settings.selected_language != "auto"
            && crate::managers::model::canonical_language_code(&settings.selected_language)
                == crate::managers::model::canonical_language_code(language)
        {
            return OutputLanguageEvidence::UserSelected(language.to_string());
        }

        // The engine may have required a concrete fallback even though the
        // user's persisted language was auto or unsupported.
        return OutputLanguageEvidence::ModelConstrained(language.to_string());
    }

    // A single-language model has a known output language without needing a
    // selectable language hint.
    if let [language] = supported_languages {
        return OutputLanguageEvidence::ModelConstrained(language.clone());
    }

    OutputLanguageEvidence::Unknown
}

/// Upgrade [`OutputLanguageEvidence::Unknown`] with the language the model
/// itself detected during the run (audio-based LID, e.g. Whisper in auto
/// mode). Stronger evidence resolved before the run is never overridden.
fn with_model_detected_language(
    evidence: OutputLanguageEvidence,
    detected: Option<String>,
) -> OutputLanguageEvidence {
    match (evidence, detected) {
        (OutputLanguageEvidence::Unknown, Some(language))
            if !language.is_empty() && language != "auto" =>
        {
            OutputLanguageEvidence::ModelDetected(language)
        }
        (evidence, _) => evidence,
    }
}

/// Custom words for transcribe-cpp's `vocabulary` slot, in priority order.
/// Empty for models without Feature::Vocabulary, which would warn on every run
/// and rely on fuzzy correction instead.
///
/// On Whisper, `transcribe-cpp` renders `vocabulary` as `" Glossary: <terms>"`
/// inside `<|startofprev|>`, which acts as an unpunctuated continuation prefix
/// rather than true hotword biasing and causes Whisper to hallucinate ASCII
/// brand tokens over unrelated spoken nouns on pause boundaries (e.g. replacing
/// `"transcription"` with `"LOWYAP"`). ASCII custom words are therefore left to
/// deterministic Soundex + Levenshtein n-gram post-correction (`apply_custom_words`)
/// and Stage-B `lexicon_terms`, while non-ASCII Unicode terms still pass through.
fn vocabulary_for_model(
    arch: &str,
    supports_vocabulary: bool,
    custom_words: &[String],
) -> Vec<String> {
    if !supports_vocabulary {
        return Vec::new();
    }
    if arch.eq_ignore_ascii_case("whisper") {
        custom_words
            .iter()
            .filter(|w| !w.is_ascii())
            .cloned()
            .collect()
    } else {
        custom_words.to_vec()
    }
}

struct TranscribeCppRunPlan {
    task: Task,
    language: Option<String>,
    target_language: Option<String>,
}

/// Build the transcribe-cpp language/task options shared by batch and live
/// streaming paths.
fn transcribe_cpp_run_plan(
    translate_to_english: bool,
    effective_language: &str,
    model_languages: &[String],
    model_supports_translate: bool,
) -> TranscribeCppRunPlan {
    let requested_language = match effective_language {
        "auto" => None,
        other => Some(other.to_string()),
    };
    // Only pass a language the loaded model actually advertises (per
    // capabilities().languages); otherwise auto-detect rather than failing with
    // UNSUPPORTED_LANGUAGE. Language-agnostic models report an empty list, so
    // they always stay on auto.
    let language = requested_language.filter(|lang| model_languages.iter().any(|l| l == lang));
    let (task, target_language) = cpp_translation_task(
        translate_to_english,
        model_supports_translate,
        language.as_deref(),
    );

    TranscribeCppRunPlan {
        task,
        language,
        target_language,
    }
}

fn post_process_transcription_text(
    raw: String,
    settings: &AppSettings,
    custom_words_already_prompted: bool,
    output_language: &OutputLanguageEvidence,
    supported_languages: &[String],
) -> String {
    post_process_transcription_text_with_script(
        raw,
        settings,
        custom_words_already_prompted,
        output_language,
        supported_languages,
        crate::flow::multilingual::HindustaniScript::Auto,
    )
}

fn post_process_transcription_text_with_script(
    raw: String,
    settings: &AppSettings,
    custom_words_already_prompted: bool,
    output_language: &OutputLanguageEvidence,
    supported_languages: &[String],
    script_pref: crate::flow::multilingual::HindustaniScript,
) -> String {
    let converts_script = settings.chinese_script != ChineseScript::AsTranscribed;
    fail_open_text_transform(raw, |raw| {
        let raw = lowyap_flow::guard_transcript_text(
            &raw,
            &settings.spoken_languages,
            script_pref.into(),
            lowyap_flow::SilenceGuardContext {
                rms_energy: None,
                no_speech_prob: None,
                mean_token_prob: None,
                output_was_translated: matches!(
                    output_language,
                    OutputLanguageEvidence::TranslatedToEnglish
                ),
            },
        );
        let raw = crate::flow::multilingual::normalize_script_output(
            &raw,
            &settings.spoken_languages,
            script_pref,
        );

        // Last-resort language evidence: confidence-gated detection from the
        // transcribed text itself, constrained to the model's languages. Only
        // consulted when it can change the outcome (built-in gated fillers or
        // Chinese script conversion).
        let output_language = match output_language {
            OutputLanguageEvidence::Unknown
                if converts_script
                    || (settings.filler_word_removal_enabled
                        && settings.custom_filler_words.is_none()) =>
            {
                match detect_output_language(&raw, supported_languages) {
                    Some(language) => {
                        debug!("Text-based language detection resolved '{}'", language);
                        OutputLanguageEvidence::TextDetected(language)
                    }
                    None => OutputLanguageEvidence::Unknown,
                }
            }
            other => other.clone(),
        };

        // Convert the script before custom words so they match in the script
        // the user writes in. Only output known to be Chinese is touched, so
        // e.g. Japanese kanji are never rewritten.
        let variety = output_language
            .language()
            .and_then(ChineseVariety::from_language);
        let raw = match variety {
            Some(variety) if converts_script => {
                convert_chinese_script(&raw, variety, settings.chinese_script)
            }
            _ => raw,
        };

        let corrected = if !settings.custom_words.is_empty() && !custom_words_already_prompted {
            apply_custom_words(
                &raw,
                &settings.custom_words,
                settings.word_correction_threshold,
            )
        } else {
            raw
        };

        let without_fillers = remove_filler_words(
            &corrected,
            &output_language,
            &settings.custom_filler_words,
            settings.filler_word_removal_enabled,
        );

        normalize_transcription_output(&without_fillers)
    })
}

/// Characters to wait for before detecting the preview's language. A lone
/// Chinese character reads as Mandarin, but Japanese often opens with kanji
/// before any kana; waiting a few characters keeps those from locking in.
const PREVIEW_DETECTION_MIN_CHARS: usize = 6;

/// Converts live-preview text into the configured Chinese script as it streams.
///
/// The preview is cosmetic: the final paste re-resolves the language and
/// converts on its own, so this only has to look right while speaking.
struct PreviewScript {
    script: ChineseScript,
    variety: Option<ChineseVariety>,
    /// The language wasn't known when the stream started (auto-detect), so it
    /// is detected from the streamed text until it turns out to be Chinese.
    detect: bool,
}

impl PreviewScript {
    fn new(script: ChineseScript, output_language: &OutputLanguageEvidence) -> Self {
        let enabled = script != ChineseScript::AsTranscribed;
        Self {
            script,
            variety: output_language
                .language()
                .and_then(ChineseVariety::from_language)
                .filter(|_| enabled),
            detect: enabled && *output_language == OutputLanguageEvidence::Unknown,
        }
    }

    /// Converts the model's raw committed/tentative text. Always fed the raw
    /// text, never a previous conversion, so it stays consistent with the
    /// final conversion. Once Chinese is detected it sticks for the rest of
    /// the stream so the preview doesn't flip back and forth.
    fn convert(
        &mut self,
        committed: &str,
        tentative: &str,
        supported_languages: &[String],
    ) -> (String, String) {
        if self.variety.is_none() && self.detect {
            let text = format!("{committed}{tentative}");
            if text.chars().count() >= PREVIEW_DETECTION_MIN_CHARS {
                self.variety = detect_output_language(&text, supported_languages)
                    .as_deref()
                    .and_then(ChineseVariety::from_language);
            }
        }

        match self.variety {
            Some(variety) => (
                convert_chinese_script(committed, variety, self.script),
                convert_chinese_script(tentative, variety, self.script),
            ),
            None => (committed.to_string(), tentative.to_string()),
        }
    }
}

/// Optional text cleanup must never discard a successful model result. The
/// transform is pure and owns its input, so recovering the untouched text is
/// safe even if a bug in custom-word or filler filtering unwinds.
fn fail_open_text_transform<F>(raw: String, transform: F) -> String
where
    F: FnOnce(String) -> String,
{
    let fallback = raw.clone();
    match catch_unwind(AssertUnwindSafe(|| transform(raw))) {
        Ok(processed) => processed,
        Err(payload) => {
            error!(
                "Optional transcription text post-processing panicked: {}; using the raw transcription",
                panic_payload_message(payload.as_ref())
            );
            fallback
        }
    }
}

/// Decide a transcribe-cpp run's task + translation target from settings.
///
/// "Translate to English" only fires where the model advertises translation.
/// Unlike transcribe-rs (which forces the target to English itself when its
/// `translate` flag is set), transcribe-cpp requires an explicit
/// `target_language`: a null target defaults to the *source*, so a non-English
/// source silently becomes e.g. es→es and Canary rejects the unadvertised pair.
/// An English source is skipped entirely — en→en is not a real translation, and
/// it's reachable by default since auto-detect-less models coerce intent to "en".
///
/// Returns `(task, target_language)` ready to drop into `RunOptions`.
fn cpp_translation_task(
    translate_to_english: bool,
    model_supports_translate: bool,
    source_language: Option<&str>,
) -> (Task, Option<String>) {
    let translate_to_en =
        translate_to_english && model_supports_translate && source_language != Some("en");
    if translate_to_en {
        (Task::Translate, Some("en".to_string()))
    } else {
        (Task::Transcribe, None)
    }
}

/// Drain a stream command channel, ignoring fed audio, until the caller
/// finalizes or cancels. Used when streaming can't actually run (model not
/// loaded / not streaming-capable) so the finalize handshake still completes
/// and the caller falls back to batch transcription.
fn drain_until_finalize(rx: mpsc::Receiver<StreamCmd>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            StreamCmd::Feed(_) => {}
            StreamCmd::Finalize { reply, .. } => {
                let _ = reply.send(Ok(None));
                break;
            }
            StreamCmd::Cancel => break,
        }
    }
}

/// Log the compute devices transcribe-cpp registers.
///
/// Devices are listed by a transcription worker process (see
/// [`crate::engine_supervisor`]), so GPU driver initialization never runs in
/// the app process. Listing opens the GPU, which on macOS loads ggml's Metal
/// library and compiles it when the system shader cache does not hold it yet
/// (the first launch after an install or update), so the app calls this from
/// a background thread instead of its startup path.
pub fn report_compute_devices(tm: &TranscriptionManager) {
    if transcribe_gpu_disabled_for_host() {
        warn!(
            "Windows x64 build is running under emulation on an ARM64 host; \
             disabling transcribe.cpp GPU acceleration and using CPU"
        );
    }
    match transcribe_compute_devices(&tm.engine) {
        Ok(devices) => info!(
            "transcribe-cpp initialized with {} compute device(s): [{}]",
            devices.len(),
            devices
                .iter()
                .map(|d| format!("{} ({})", d.name, d.kind))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(e) => warn!("transcribe-cpp compute devices are unavailable: {}", e),
    }
}

/// Human-readable list of the transcribe-cpp compute devices, for the
/// `--list-devices` flag. The reported `index` is the value to pass to
/// `--device-index`. Errors if the devices could not be listed.
pub fn describe_compute_devices(tm: &TranscriptionManager) -> Result<Vec<String>> {
    Ok(transcribe_compute_devices(&tm.engine)?
        .into_iter()
        .map(|d| {
            let idx = d
                .index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_string());
            let vram_mb = d.memory_total / (1024 * 1024);
            format!(
                "index={} kind={} name={} vram={}MB",
                idx,
                d.kind,
                d.label(),
                vram_mb
            )
        })
        .collect())
}

/// Map Handy's whisper accelerator setting to a transcribe-cpp [`Backend`].
///
/// `Auto` lets the library pick the best device (with CPU fallback), while
/// `Cpu` forces strict CPU. `Gpu` only remains as the companion setting for an
/// exact device; without a valid exact device it has the retired generic GPU
/// state's new Auto semantics. An emulated x64 process on Windows ARM64 forces
/// strict CPU for every setting.
fn select_transcribe_backend(setting: TranscribeAcceleratorSetting) -> Backend {
    select_transcribe_backend_for_host(setting, transcribe_gpu_disabled_for_host())
}

fn select_transcribe_backend_for_host(
    setting: TranscribeAcceleratorSetting,
    gpu_disabled: bool,
) -> Backend {
    match effective_transcribe_accelerator(setting, gpu_disabled) {
        TranscribeAcceleratorSetting::Cpu => Backend::Cpu,
        TranscribeAcceleratorSetting::Auto | TranscribeAcceleratorSetting::Gpu => Backend::Auto,
    }
}

/// Resolve the user's persisted GPU choice to the device key the worker
/// should load on. Registry indices and handles are process-local, so
/// settings store a key based on the backend's stable `device_id` (falling
/// back to name for backends such as Metal that do not report one). The
/// worker falls back to automatic selection if that device is gone.
fn resolve_gpu_device(
    setting: TranscribeAcceleratorSetting,
    gpu_device: Option<&str>,
) -> Option<String> {
    if transcribe_gpu_disabled_for_host() || setting != TranscribeAcceleratorSetting::Gpu {
        return None;
    }
    gpu_device.map(str::to_string)
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct GpuDeviceOption {
    pub id: String,
    pub name: String,
    pub total_vram_mb: usize,
}

fn transcribe_gpu_disabled_for_host() -> bool {
    crate::utils::is_windows_x64_emulated_on_arm64()
}

fn effective_transcribe_accelerator(
    setting: TranscribeAcceleratorSetting,
    gpu_disabled: bool,
) -> TranscribeAcceleratorSetting {
    if gpu_disabled {
        TranscribeAcceleratorSetting::Cpu
    } else {
        setting
    }
}

/// The compute devices the latest worker listed. On a host without GPU
/// access every worker is CPU-only, so it lists no GPU.
fn transcribe_compute_devices(engine: &EngineSupervisor) -> Result<Vec<DeviceInfo>> {
    engine
        .devices()
        .ok_or_else(|| anyhow::anyhow!("compute devices could not be listed (see the log for why)"))
}

fn available_transcribe_accelerators(gpu_disabled: bool) -> Vec<String> {
    if gpu_disabled {
        vec!["cpu".to_string()]
    } else {
        vec!["auto".to_string(), "cpu".to_string(), "gpu".to_string()]
    }
}

fn gpu_device_options(engine: &EngineSupervisor) -> Vec<GpuDeviceOption> {
    // GPU compute devices as the latest worker listed them, so a GPU that
    // comes back is picked up. `id` is a persistent identity key, never the
    // process-local registry index. It uses the backend's device_id where
    // available and its name otherwise (Metal). `total_vram_mb` is 0 when the
    // backend does not report capacity. Empty if listing failed.
    transcribe_compute_devices(engine)
        .unwrap_or_default()
        .into_iter()
        .filter(DeviceInfo::is_gpu)
        .map(|d| GpuDeviceOption {
            id: d.key.clone(),
            name: d.label().to_string(),
            total_vram_mb: (d.memory_total / (1024 * 1024)) as usize,
        })
        .collect()
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct AvailableAccelerators {
    pub transcribe: Vec<String>,
    pub gpu_devices: Vec<GpuDeviceOption>,
}

/// Return the accelerators available to this process on its current host.
pub fn get_available_accelerators(tm: &TranscriptionManager) -> AvailableAccelerators {
    let transcribe_options = available_transcribe_accelerators(transcribe_gpu_disabled_for_host());

    AvailableAccelerators {
        transcribe: transcribe_options,
        gpu_devices: gpu_device_options(&tm.engine),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_bilingual_clauses_coalesce_without_losing_ranges_or_crossing_gaps() {
        let ranges: Vec<_> = (0..45).map(|i| i * 48_000..(i + 1) * 48_000).collect();
        let merged = coalesce_short_clauses(ranges);
        assert_eq!(merged.len(), 12);
        assert_eq!(merged.first().unwrap().start, 0);
        assert_eq!(merged.last().unwrap().end, 45 * 48_000);
        assert!(merged.windows(2).all(|pair| pair[0].end == pair[1].start));
        assert_eq!(
            coalesce_short_clauses(vec![0..16_000, 32_000..48_000]),
            vec![0..16_000, 32_000..48_000]
        );
        assert_eq!(
            coalesce_short_clauses(vec![0..176_000, 176_000..192_000]),
            vec![0..176_000, 176_000..192_000]
        );
        // Live chunks (~4.5s-6.5s) and sub-11s clips coalesce micro-clauses into a
        // single ASR pass instead of running 3-4 separate 30s-padded Whisper passes.
        assert_eq!(
            coalesce_short_clauses(vec![0..35_200, 35_200..76_800, 76_800..104_000]),
            vec![0..104_000]
        );
    }

    fn languages(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_string()).collect()
    }

    #[test]
    fn normal_hosts_preserve_every_transcribe_accelerator_setting() {
        for setting in [
            TranscribeAcceleratorSetting::Auto,
            TranscribeAcceleratorSetting::Cpu,
            TranscribeAcceleratorSetting::Gpu,
        ] {
            assert_eq!(effective_transcribe_accelerator(setting, false), setting);
        }
        assert_eq!(
            available_transcribe_accelerators(false),
            ["auto", "cpu", "gpu"]
        );
        assert_eq!(
            select_transcribe_backend_for_host(TranscribeAcceleratorSetting::Auto, false),
            Backend::Auto
        );
        assert_eq!(
            select_transcribe_backend_for_host(TranscribeAcceleratorSetting::Cpu, false),
            Backend::Cpu
        );
        assert_eq!(
            select_transcribe_backend_for_host(TranscribeAcceleratorSetting::Gpu, false),
            Backend::Auto
        );
    }

    #[test]
    fn emulated_x64_on_arm64_forces_every_transcribe_setting_to_cpu() {
        for setting in [
            TranscribeAcceleratorSetting::Auto,
            TranscribeAcceleratorSetting::Cpu,
            TranscribeAcceleratorSetting::Gpu,
        ] {
            assert_eq!(
                effective_transcribe_accelerator(setting, true),
                TranscribeAcceleratorSetting::Cpu
            );
            assert_eq!(
                select_transcribe_backend_for_host(setting, true),
                Backend::Cpu
            );
        }
        assert_eq!(available_transcribe_accelerators(true), ["cpu"]);
    }

    #[test]
    fn optional_text_transform_falls_back_to_raw_text_after_panic() {
        let raw = "原始轉錄。".to_string();
        let result = fail_open_text_transform(raw.clone(), |_| {
            panic!("simulated optional cleanup failure")
        });

        assert_eq!(result, raw);
    }

    #[test]
    fn non_chinese_output_is_never_converted() {
        let settings = AppSettings {
            chinese_script: ChineseScript::TraditionalTw,
            ..Default::default()
        };
        for evidence in [
            OutputLanguageEvidence::ModelDetected("ja".to_string()),
            OutputLanguageEvidence::UserSelected("en".to_string()),
        ] {
            let result = post_process_transcription_text(
                "学校に行きます".to_string(),
                &settings,
                false,
                &evidence,
                &languages(&["zh", "ja", "en"]),
            );

            assert_eq!(result, "学校に行きます", "{evidence:?}");
        }
    }

    #[test]
    fn auto_detected_chinese_text_is_converted() {
        let settings = AppSettings {
            chinese_script: ChineseScript::Simplified,
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let result = post_process_transcription_text(
            "我們今天下午一起去學校圖書館看書，然後再去吃晚飯。".to_string(),
            &settings,
            false,
            &OutputLanguageEvidence::Unknown,
            &languages(&["zh", "en", "ja"]),
        );

        assert_eq!(result, "我们今天下午一起去学校图书馆看书，然后再去吃晚饭。");
    }

    #[test]
    fn portuguese_transcription_does_not_use_english_ui_filler_words() {
        let settings = AppSettings {
            app_language: "en".to_string(),
            selected_language: "pt-BR".to_string(),
            ..Default::default()
        };
        let supported = languages(&["en", "pt"]);
        let evidence =
            resolve_output_language_evidence(&settings, Some("pt"), false, &supported, false);

        let result = post_process_transcription_text(
            "eu vi um carro".to_string(),
            &settings,
            false,
            &evidence,
            &supported,
        );

        assert_eq!(
            evidence,
            OutputLanguageEvidence::UserSelected("pt".to_string())
        );
        assert_eq!(result, "eu vi um carro");
    }

    #[test]
    fn norwegian_alias_is_recorded_as_user_selected_evidence() {
        let settings = AppSettings {
            selected_language: "no".to_string(),
            ..Default::default()
        };

        let evidence = resolve_output_language_evidence(
            &settings,
            Some("nb"),
            false,
            &languages(&["nb"]),
            false,
        );

        assert_eq!(
            evidence,
            OutputLanguageEvidence::UserSelected("nb".to_string())
        );
    }

    #[test]
    fn auto_language_without_detection_skips_gated_filler_removal() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            ..Default::default()
        };
        let evidence = resolve_output_language_evidence(
            &settings,
            None,
            false,
            &languages(&["en", "pt"]),
            false,
        );

        // Too short for a reliable text detection, so the gated "um" must
        // survive; the universal "uhm" is removed regardless.
        let result = post_process_transcription_text(
            "um uhm ok".to_string(),
            &settings,
            false,
            &evidence,
            &languages(&["en", "pt"]),
        );

        assert_eq!(evidence, OutputLanguageEvidence::Unknown);
        assert_eq!(result, "um ok");
    }

    #[test]
    fn unknown_evidence_with_confident_text_detection_removes_gated_fillers() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            ..Default::default()
        };

        let result = post_process_transcription_text(
            "um so the weather forecast said it would probably rain throughout the whole weekend"
                .to_string(),
            &settings,
            false,
            &OutputLanguageEvidence::Unknown,
            &languages(&["en", "pt", "es", "de"]),
        );

        assert_eq!(
            result,
            "so the weather forecast said it would probably rain throughout the whole weekend"
        );
    }

    #[test]
    fn unknown_evidence_with_portuguese_text_preserves_um() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            ..Default::default()
        };

        let result = post_process_transcription_text(
            "eu vi um carro na rua ontem de manhã quando fui ao mercado".to_string(),
            &settings,
            false,
            &OutputLanguageEvidence::Unknown,
            &languages(&["en", "pt", "es", "de"]),
        );

        assert_eq!(
            result,
            "eu vi um carro na rua ontem de manhã quando fui ao mercado"
        );
    }

    #[test]
    fn model_detected_language_upgrades_unknown_evidence_only() {
        assert_eq!(
            with_model_detected_language(OutputLanguageEvidence::Unknown, Some("en".to_string())),
            OutputLanguageEvidence::ModelDetected("en".to_string())
        );
        assert_eq!(
            with_model_detected_language(OutputLanguageEvidence::Unknown, Some("auto".to_string())),
            OutputLanguageEvidence::Unknown
        );
        assert_eq!(
            with_model_detected_language(OutputLanguageEvidence::Unknown, None),
            OutputLanguageEvidence::Unknown
        );
        assert_eq!(
            with_model_detected_language(
                OutputLanguageEvidence::UserSelected("pt".to_string()),
                Some("en".to_string())
            ),
            OutputLanguageEvidence::UserSelected("pt".to_string())
        );
    }

    #[test]
    fn auto_language_uses_single_language_model_as_evidence() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            ..Default::default()
        };

        let evidence =
            resolve_output_language_evidence(&settings, None, false, &languages(&["en"]), false);

        assert_eq!(
            evidence,
            OutputLanguageEvidence::ModelConstrained("en".to_string())
        );
    }

    #[test]
    fn unsupported_explicit_language_uses_model_fallback_as_evidence() {
        let settings = AppSettings {
            selected_language: "pt".to_string(),
            ..Default::default()
        };

        let evidence = resolve_output_language_evidence(
            &settings,
            Some("en"),
            false,
            &languages(&["en", "de"]),
            false,
        );

        assert_eq!(
            evidence,
            OutputLanguageEvidence::ModelConstrained("en".to_string())
        );
    }

    #[test]
    fn ignored_user_language_is_not_output_evidence() {
        let settings = AppSettings {
            // Parakeet V3 ignores language hints and auto-detects even when a
            // selection from the previously active model remains persisted.
            selected_language: "en".to_string(),
            ..Default::default()
        };
        let supported = languages(&["en", "de", "pt"]);

        let evidence = resolve_output_language_evidence(&settings, None, false, &supported, false);
        assert_eq!(evidence, OutputLanguageEvidence::Unknown);

        let result = post_process_transcription_text(
            "eu vi um carro".to_string(),
            &settings,
            false,
            &evidence,
            &supported,
        );
        assert_eq!(result, "eu vi um carro");
    }

    #[test]
    fn unapplied_transcribe_cpp_language_is_not_output_evidence() {
        let settings = AppSettings {
            selected_language: "en".to_string(),
            ..Default::default()
        };
        let supported = languages(&[]);
        let plan = transcribe_cpp_run_plan(false, "en", &supported, false);

        assert_eq!(plan.language, None);
        assert_eq!(
            resolve_output_language_evidence(
                &settings,
                plan.language.as_deref(),
                false,
                &supported,
                false,
            ),
            OutputLanguageEvidence::Unknown
        );
    }

    #[test]
    fn translated_output_is_treated_as_english() {
        let settings = AppSettings {
            selected_language: "pt".to_string(),
            ..Default::default()
        };

        let evidence = resolve_output_language_evidence(
            &settings,
            Some("pt"),
            false,
            &languages(&["en", "pt"]),
            true,
        );

        assert_eq!(evidence, OutputLanguageEvidence::TranslatedToEnglish);
    }

    #[test]
    fn transcribe_cpp_run_plan_skips_english_translation() {
        let plan = transcribe_cpp_run_plan(true, "en", &languages(&["en", "es"]), true);

        assert!(matches!(plan.task, Task::Transcribe));
        assert_eq!(plan.language.as_deref(), Some("en"));
        assert_eq!(plan.target_language, None);
    }

    #[test]
    fn transcribe_cpp_run_plan_translates_supported_non_english() {
        let plan = transcribe_cpp_run_plan(true, "es", &languages(&["en", "es"]), true);

        assert!(matches!(plan.task, Task::Translate));
        assert_eq!(plan.language.as_deref(), Some("es"));
        assert_eq!(plan.target_language.as_deref(), Some("en"));
    }

    #[test]
    fn transcribe_cpp_run_plan_requires_model_translation_support() {
        let plan = transcribe_cpp_run_plan(true, "es", &languages(&["en", "es"]), false);

        assert!(matches!(plan.task, Task::Transcribe));
        assert_eq!(plan.language.as_deref(), Some("es"));
        assert_eq!(plan.target_language, None);
    }

    #[test]
    fn fuzzy_correction_is_skipped_exactly_when_vocabulary_was_sent() {
        let settings = AppSettings {
            selected_language: "en".to_string(),
            custom_words: vec!["Handy".to_string()],
            ..Default::default()
        };
        let evidence = OutputLanguageEvidence::UserSelected("en".to_string());
        let supported = languages(&["en"]);

        // The model was already prompted with the term: leave its output alone.
        let prompted = post_process_transcription_text(
            "i use handy every day".to_string(),
            &settings,
            true,
            &evidence,
            &supported,
        );
        assert_eq!(prompted, "i use handy every day");

        // No vocabulary sent: the fuzzy post-correction fixes the casing.
        let corrected = post_process_transcription_text(
            "i use handy every day".to_string(),
            &settings,
            false,
            &evidence,
            &supported,
        );
        assert_eq!(corrected, "i use Handy every day");
    }

    #[test]
    fn run_setup_populates_initial_prompt_and_keeps_fuzzy_words_when_vocab_unsupported() {
        let info = LoadedInfo {
            arch: "whisper".to_string(),
            variant: "large-v3".to_string(),
            backend: "metal".to_string(),
            device: "Apple M3".to_string(),
            on_gpu: true,
            capabilities: transcribe_cpp::Capabilities {
                native_sample_rate: 16_000,
                languages: languages(&["en", "ur", "hi", "ar"]),
                translate_target_languages: languages(&["en"]),
                max_timestamp_kind: transcribe_cpp::TimestampKind::Word,
                supports_language_detect: true,
                supports_translate: true,
                supports_streaming: false,
                supports_spec_decode: false,
                max_audio_ms: 30_000,
            },
            supports_initial_prompt: true,
            supports_long_form: true,
            supports_vocabulary: false,
        };
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            spoken_languages: languages(&["en", "ur", "ar"]),
            custom_words: vec!["Lowyap".to_string()],
            ..Default::default()
        };

        let setup = run_setup(
            &info,
            &settings,
            LanguageDecision {
                language: None,
                source: LanguageSource::Model,
            },
            crate::flow::multilingual::HindustaniScript::Auto,
        );

        let prompt = setup.run_options.prompt.as_deref().expect("initial prompt");
        assert!(
            !prompt.contains("اردو"),
            "mixed en+ur Pass 1 must stay script-neutral so quiet English is not translated to Urdu"
        );
        assert!(prompt.contains("Lowyap"));
        assert!(
            !setup.custom_words_prompted(),
            "prompt-only glossary hint must still allow downstream fuzzy custom-word correction"
        );
        let retry = setup.urdu_retry_options.expect("urdu retry options");
        assert_eq!(retry.language.as_deref(), Some("ur"));
        assert!(retry
            .prompt
            .as_deref()
            .is_some_and(|p| p.contains("اردو اور عربی")));
        assert!(retry.family.is_some());
    }

    #[test]
    fn whisper_vocab_supported_keeps_ascii_glossary_out_of_startofprev_and_preserves_nouns() {
        let info = LoadedInfo {
            arch: "whisper".to_string(),
            variant: "large-v3".to_string(),
            backend: "metal".to_string(),
            device: "Apple M3".to_string(),
            on_gpu: true,
            capabilities: transcribe_cpp::Capabilities {
                native_sample_rate: 16_000,
                languages: languages(&["en", "ur", "hi"]),
                translate_target_languages: languages(&["en"]),
                max_timestamp_kind: transcribe_cpp::TimestampKind::Segment,
                supports_language_detect: true,
                supports_translate: true,
                supports_streaming: false,
                supports_spec_decode: false,
                max_audio_ms: 30_000,
            },
            supports_initial_prompt: true,
            supports_long_form: true,
            supports_vocabulary: true,
        };
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            spoken_languages: languages(&["en", "ur"]),
            custom_words: vec!["LOWYAP".to_string()],
            ..Default::default()
        };

        let setup = run_setup(
            &info,
            &settings,
            LanguageDecision {
                language: None,
                source: LanguageSource::Model,
            },
            crate::flow::multilingual::HindustaniScript::Auto,
        );

        assert!(
            setup.run_options.vocabulary.is_empty(),
            "Whisper must not inject ASCII custom words into <|startofprev|> Glossary"
        );
        assert!(setup.run_options.prompt.is_none());
        assert!(setup.run_options.family.is_some());
        assert!(
            !setup.custom_words_prompted(),
            "post-ASR fuzzy custom_words must remain active on Whisper"
        );

        let evidence = OutputLanguageEvidence::ModelDetected("en".to_string());
        let supported = languages(&["en", "ur"]);
        assert_eq!(
            post_process_transcription_text(
                "But before that can you check the latest transcription".to_string(),
                &settings,
                setup.custom_words_prompted(),
                &evidence,
                &supported,
            ),
            "But before that can you check the latest transcription"
        );
        assert_eq!(
            post_process_transcription_text(
                "Can you check the latest loyap build".to_string(),
                &settings,
                setup.custom_words_prompted(),
                &evidence,
                &supported,
            ),
            "Can you check the latest LOWYAP build"
        );
    }

    #[test]
    fn post_process_rescues_devanagari_and_collapses_hallucination_loops() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            spoken_languages: languages(&["en", "ur"]),
            ..Default::default()
        };
        let supported = languages(&["en", "ur", "hi"]);

        assert_eq!(
            post_process_transcription_text_with_script(
                "हे, can you hear me out?".to_string(),
                &settings,
                false,
                &OutputLanguageEvidence::ModelDetected("hi".to_string()),
                &supported,
                crate::flow::multilingual::HindustaniScript::Auto,
            ),
            "Hey, can you hear me out?"
        );

        assert_eq!(
            post_process_transcription_text_with_script(
                "हेलो आप क्या करते हैं?".to_string(),
                &settings,
                false,
                &OutputLanguageEvidence::ModelDetected("hi".to_string()),
                &supported,
                crate::flow::multilingual::HindustaniScript::RomanHinglish,
            ),
            "Hello aap kya karte hain?"
        );

        // Pure phrase-tiling phantom loop is dropped outright (`""`), whereas a
        // trailing decoder loop after real speech collapses to one occurrence.
        assert_eq!(
            post_process_transcription_text_with_script(
                "Thank you. Thank you. Thank you. Thank you. Thank you.".to_string(),
                &settings,
                false,
                &OutputLanguageEvidence::ModelDetected("en".to_string()),
                &supported,
                crate::flow::multilingual::HindustaniScript::Auto,
            ),
            ""
        );
        assert_eq!(
            post_process_transcription_text_with_script(
                "Ship the build today. Thank you. Thank you. Thank you. Thank you. Thank you."
                    .to_string(),
                &settings,
                false,
                &OutputLanguageEvidence::ModelDetected("en".to_string()),
                &supported,
                crate::flow::multilingual::HindustaniScript::Auto,
            ),
            "Ship the build today. Thank you."
        );
        assert_eq!(
            post_process_transcription_text_with_script(
                "और भाई बताओ क्या चल रहा है?".to_string(),
                &settings,
                false,
                &OutputLanguageEvidence::ModelDetected("hi".to_string()),
                &supported,
                crate::flow::multilingual::HindustaniScript::Auto,
            ),
            "Aur bhai batao kya chal raha hai?"
        );
    }

    #[test]
    fn post_process_drops_phantoms_but_preserves_english_and_roman_hinglish() {
        let settings = AppSettings {
            spoken_languages: languages(&["en", "hi"]),
            ..Default::default()
        };
        assert_eq!(
            post_process_transcription_text_with_script(
                "[BLANK_AUDIO] Sounds good. 안녕하세요 カタカナ Haan bilkul ♪".to_owned(),
                &settings,
                false,
                &OutputLanguageEvidence::ModelDetected("en".to_owned()),
                &[],
                crate::flow::multilingual::HindustaniScript::RomanHinglish,
            ),
            "Sounds good. Haan bilkul"
        );
    }

    #[test]
    fn post_process_drops_music_phantom_but_preserves_configured_urdu() {
        let settings = AppSettings {
            spoken_languages: languages(&["ur"]),
            ..Default::default()
        };
        assert_eq!(
            post_process_transcription_text_with_script(
                "السلام علیکم موسیقی".to_owned(),
                &settings,
                false,
                &OutputLanguageEvidence::ModelDetected("ur".to_owned()),
                &[],
                crate::flow::multilingual::HindustaniScript::UrduPersoArabic,
            ),
            "السلام علیکم"
        );
    }

    #[test]
    fn post_process_preserves_translated_english_for_urdu_only_profile() {
        let settings = AppSettings {
            spoken_languages: languages(&["ur"]),
            chinese_script: ChineseScript::TraditionalTw,
            ..Default::default()
        };
        assert_eq!(
            post_process_transcription_text_with_script(
                "السلام علیکم The meeting starts now.".to_owned(),
                &settings,
                false,
                &OutputLanguageEvidence::TranslatedToEnglish,
                &[],
                crate::flow::multilingual::HindustaniScript::UrduPersoArabic,
            ),
            "The meeting starts now."
        );
    }
}

impl Drop for TranscriptionManager {
    fn drop(&mut self) {
        // Skip shutdown unless this is the very last clone. TranscriptionManager
        // is cloned by initiate_model_load() and the watcher thread — those
        // clones dropping must not kill the watcher. The watcher thread holds
        // its own clone, so current_model_id's strong_count is always >= 2 while
        // the watcher is alive. (current_model_id is only ever cloned along with
        // the manager; shutdown_signal is not, since the watcher also holds a
        // standalone clone of it.) When it reaches 1, only this instance
        // remains and we can safely shut down.
        if Arc::strong_count(&self.current_model_id) > 1 {
            return;
        }

        // Signal the watcher thread to shutdown
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Wait for the thread to finish gracefully.
        // Use match instead of unwrap to avoid panicking if the mutex is
        // poisoned — a panic inside Drop calls abort().
        let mut guard = match self.watcher_handle.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!("Recovered poisoned watcher_handle mutex during TranscriptionManager drop — a panic occurred earlier this session");
                e.into_inner()
            }
        };
        if let Some(handle) = guard.take() {
            if let Err(e) = handle.join() {
                warn!("Failed to join idle watcher thread: {:?}", e);
            } else {
                debug!("Idle watcher thread joined successfully");
            }
        }
    }
}
