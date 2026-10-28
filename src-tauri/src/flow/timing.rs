//! Per-dictation stage timing.
//!
//! Upstream code marks pipeline points through one-line hooks; this module
//! groups the marks into sessions and logs exactly one `flow_timing {json}`
//! info line when a dictation finishes (typed, empty, silent or failed).
//! Cancelled dictations log nothing. The headline metric is
//! `release_to_pasted_ms`: hotkey release until the paste call returned.
//!
//! The line holds durations and flags only — never transcript text or the
//! identity of the frontmost app, which is not logged at any level.

use std::future::Future;
use std::sync::{Mutex, MutexGuard, Once, PoisonError};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use serde::Serialize;
use tauri::AppHandle;
use tauri_specta::Event;

use crate::actions::{TranscriptionCompleteEvent, TranscriptionOutcome};
use crate::flow::context::AppContext;
use crate::flow::settings::Tone;

type TaskId = tokio::task::Id;

/// Released sessions kept while awaiting completion; older ones were cancelled.
const MAX_INFLIGHT: usize = 4;
/// Released sessions older than this are dropped (cancelled or lost).
const STALE_AFTER: Duration = Duration::from_secs(300);
/// Tolerance when matching a completion event to its release mark.
const MATCH_SLACK: Duration = Duration::from_millis(5);

#[derive(Debug, Default, Clone)]
struct Session {
    id: u64,
    pressed_at: Option<Instant>,
    mic_ready_at: Option<Instant>,
    released_at: Option<Instant>,
    asr_start: Option<Instant>,
    asr_end: Option<Instant>,
    cleanup_start: Option<Instant>,
    cleanup_end: Option<Instant>,
    cleanup_ran: bool,
    paste_start: Option<Instant>,
    paste_end: Option<Instant>,
    /// The async task that ran ASR; cleanup in that same task belongs to this
    /// session. History retries run outside it, so they never match.
    task: Option<TaskId>,
    app: Option<AppContext>,
    app_capture_us: Option<u64>,
    tone: Option<Tone>,
}

#[derive(Debug)]
struct Tracker {
    next_id: u64,
    recording: Option<Session>,
    inflight: Vec<Session>,
}

impl Tracker {
    const fn new() -> Self {
        Self {
            next_id: 1,
            recording: None,
            inflight: Vec::new(),
        }
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    fn press(&mut self, now: Instant) -> u64 {
        if let Some(old) = self.recording.take() {
            debug!("flow_timing: session {} superseded before release", old.id);
        }
        let id = self.alloc_id();
        self.recording = Some(Session {
            id,
            pressed_at: Some(now),
            ..Session::default()
        });
        id
    }

    fn mic_ready(&mut self, now: Instant) {
        if let Some(session) = self.recording.as_mut() {
            session.mic_ready_at.get_or_insert(now);
        }
    }

    fn release(&mut self, now: Instant) -> u64 {
        let mut session = match self.recording.take() {
            Some(session) => session,
            None => Session {
                id: self.alloc_id(),
                ..Session::default()
            },
        };
        session.released_at = Some(now);

        self.inflight.retain(|s| {
            s.released_at
                .is_some_and(|released| now.saturating_duration_since(released) < STALE_AFTER)
        });
        while self.inflight.len() >= MAX_INFLIGHT {
            let dropped = self.inflight.remove(0);
            debug!("flow_timing: dropping unfinished session {}", dropped.id);
        }
        let id = session.id;
        self.inflight.push(session);
        id
    }

    fn by_id(&mut self, id: u64) -> Option<&mut Session> {
        self.inflight.iter_mut().find(|s| s.id == id)
    }

    fn asr_start(&mut self, id: u64, now: Instant, task: Option<TaskId>) -> bool {
        let Some(session) = self.by_id(id) else {
            return false;
        };
        session.asr_start = Some(now);
        session.task = task;
        true
    }

    fn asr_end(&mut self, id: u64, now: Instant) {
        if let Some(session) = self.by_id(id) {
            session.asr_end = Some(now);
        }
    }

    /// The session whose ASR ran in `task`, if its cleanup has not started.
    /// `None` outside a dictation task (e.g. a retry from history).
    fn awaiting_cleanup(&mut self, task: Option<TaskId>) -> Option<&mut Session> {
        let task = task?;
        self.inflight
            .iter_mut()
            .find(|s| s.task == Some(task) && s.asr_end.is_some() && s.cleanup_start.is_none())
    }

    fn choose_tone(
        &mut self,
        task: Option<TaskId>,
        choose: impl FnOnce(Option<&AppContext>) -> Tone,
    ) -> Tone {
        let Some(session) = self.awaiting_cleanup(task) else {
            return Tone::Default;
        };
        let tone = choose(session.app.as_ref());
        session.tone = Some(tone);
        tone
    }

    fn paste_start(&mut self, id: u64, now: Instant) {
        if let Some(session) = self.by_id(id) {
            session.paste_start.get_or_insert(now);
        }
    }

    fn paste_end(&mut self, id: u64, now: Instant) {
        if let Some(session) = self.by_id(id).filter(|s| s.paste_start.is_some()) {
            session.paste_end.get_or_insert(now);
        }
    }

    /// Removes the session a completion event belongs to. The event carries
    /// the time since the stop hook, which runs right after the release mark,
    /// so the match is the latest session released before that instant.
    fn complete(&mut self, now: Instant, since_stop: Duration) -> Option<Session> {
        let cutoff = now.checked_sub(since_stop)? + MATCH_SLACK;
        let index = self
            .inflight
            .iter()
            .enumerate()
            .filter(|(_, s)| s.released_at.is_some_and(|released| released <= cutoff))
            .max_by_key(|(_, s)| s.released_at)
            .map(|(index, _)| index)?;
        Some(self.inflight.remove(index))
    }
}

#[derive(Debug, Serialize, PartialEq)]
struct TimingReport {
    session: u64,
    outcome: TranscriptionOutcome,
    model: String,
    audio_s: f64,
    cleanup_ran: bool,
    tone: Option<Tone>,
    app_captured: bool,
    app_capture_us: Option<u64>,
    press_to_mic_ready_ms: Option<f64>,
    release_to_asr_start_ms: Option<f64>,
    asr_ms: Option<f64>,
    cleanup_ms: Option<f64>,
    paste_ms: Option<f64>,
    release_to_pasted_ms: Option<f64>,
    release_to_done_ms: Option<f64>,
}

impl TimingReport {
    fn new(
        session: &Session,
        outcome: TranscriptionOutcome,
        audio_seconds: f64,
        model: String,
        done_at: Instant,
    ) -> Self {
        let pasted_at = match outcome {
            TranscriptionOutcome::Typed => session.paste_end,
            _ => None,
        };
        Self {
            session: session.id,
            outcome,
            model,
            audio_s: (audio_seconds * 100.0).round() / 100.0,
            cleanup_ran: session.cleanup_ran,
            tone: session.tone,
            app_captured: session.app.is_some(),
            app_capture_us: session.app_capture_us,
            press_to_mic_ready_ms: span_ms(session.pressed_at, session.mic_ready_at),
            release_to_asr_start_ms: span_ms(session.released_at, session.asr_start),
            asr_ms: span_ms(session.asr_start, session.asr_end),
            cleanup_ms: span_ms(session.cleanup_start, session.cleanup_end),
            paste_ms: span_ms(session.paste_start, session.paste_end),
            release_to_pasted_ms: span_ms(session.released_at, pasted_at),
            release_to_done_ms: span_ms(session.released_at, Some(done_at)),
        }
    }
}

/// Milliseconds between two marks, rounded to 0.1 ms.
fn span_ms(from: Option<Instant>, to: Option<Instant>) -> Option<f64> {
    let elapsed = to?.checked_duration_since(from?)?;
    Some((elapsed.as_secs_f64() * 10_000.0).round() / 10.0)
}

static TRACKER: Mutex<Tracker> = Mutex::new(Tracker::new());
static LISTENER: Once = Once::new();

fn tracker() -> MutexGuard<'static, Tracker> {
    TRACKER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Marks the hotkey press and opens a new session.
pub fn press(app: &AppHandle) {
    let now = Instant::now();
    LISTENER.call_once(|| register_listener(app));
    tracker().press(now);
}

/// Attaches the frontmost app captured at press to the recording session.
pub fn attach_app(app: Option<AppContext>, capture_us: u64) {
    if let Some(session) = tracker().recording.as_mut() {
        // Never log the app's identity: the default file log level is Debug.
        debug!(
            "flow: session {} app_captured={} in {capture_us} us",
            session.id,
            app.is_some(),
        );
        session.app = app;
        session.app_capture_us = Some(capture_us);
    }
}

/// Marks the first real microphone samples of the recording session.
pub fn mic_ready() {
    tracker().mic_ready(Instant::now());
}

/// Marks the hotkey release, queues the session for completion and returns
/// its id; the stop hook passes it to the ASR and paste marks.
pub fn release() -> u64 {
    tracker().release(Instant::now())
}

/// Times the speech-to-text call of `session` and ties the session to the
/// current async task, so its cleanup can be found without guessing.
pub fn asr<T>(session: u64, transcribe: impl FnOnce() -> T) -> T {
    let started = tracker().asr_start(session, Instant::now(), tokio::task::try_id());
    let output = transcribe();
    if started {
        tracker().asr_end(session, Instant::now());
    }
    output
}

/// Picks the cleanup tone from the app captured for the dictation being
/// cleaned up in this task, and records it. `Default` outside a dictation
/// task (e.g. a retry from history), so retries never inherit a tone.
pub fn choose_tone(choose: impl FnOnce(Option<&AppContext>) -> Tone) -> Tone {
    tracker().choose_tone(tokio::task::try_id(), choose)
}

/// Times LLM cleanup of this task's dictation; `Some` output means it ran.
pub async fn cleanup<T>(post_process: impl Future<Output = Option<T>>) -> Option<T> {
    let id = tracker()
        .awaiting_cleanup(tokio::task::try_id())
        .map(|session| {
            session.cleanup_start = Some(Instant::now());
            session.id
        });
    let output = post_process.await;
    if let Some(id) = id {
        if let Some(session) = tracker().by_id(id) {
            session.cleanup_end = Some(Instant::now());
            session.cleanup_ran = output.is_some();
        }
    }
    output
}

/// Marks the paste start of `session`.
pub fn paste_start(session: u64) {
    tracker().paste_start(session, Instant::now());
}

/// Marks the paste end of `session`.
pub fn paste_end(session: u64) {
    tracker().paste_end(session, Instant::now());
}

fn register_listener(app: &AppHandle) {
    let handle = app.clone();
    TranscriptionCompleteEvent::listen_any(app, move |event| {
        let done_at = Instant::now();
        let payload = event.payload;
        finish(
            &handle,
            done_at,
            payload.outcome,
            payload.audio_seconds,
            payload.wait_seconds,
        );
    });
}

fn finish(
    app: &AppHandle,
    done_at: Instant,
    outcome: TranscriptionOutcome,
    audio_seconds: f64,
    since_stop_seconds: f64,
) {
    let Ok(since_stop) = Duration::try_from_secs_f64(since_stop_seconds) else {
        warn!("flow_timing: invalid wait_seconds {since_stop_seconds}");
        return;
    };
    let session = tracker().complete(done_at, since_stop);
    let Some(session) = session else {
        debug!("flow_timing: completion without a tracked session");
        return;
    };
    let model = crate::settings::get_settings(app).selected_model;
    let report = TimingReport::new(&session, outcome, audio_seconds, model, done_at);
    match serde_json::to_string(&report) {
        Ok(json) => info!("flow_timing {json}"),
        Err(e) => warn!("flow_timing: failed to serialize report: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// Distinct real task ids, as the dictation tasks would have.
    fn task_ids(n: usize) -> Vec<TaskId> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let mut ids = Vec::new();
            for _ in 0..n {
                ids.push(
                    tokio::spawn(async { tokio::task::id() })
                        .await
                        .expect("task"),
                );
            }
            ids
        })
    }

    fn with_app(tracker: &mut Tracker, app_id: &str) {
        if let Some(session) = tracker.recording.as_mut() {
            session.app = Some(AppContext {
                app_id: Some(app_id.into()),
                app_name: None,
            });
        }
    }

    /// Press, release and run ASR (ending 200 ms after it starts) in `task`.
    fn transcribed(
        tracker: &mut Tracker,
        base: Instant,
        press: u64,
        release: u64,
        task: Option<TaskId>,
    ) -> u64 {
        tracker.press(at(base, press));
        let id = tracker.release(at(base, release));
        assert!(tracker.asr_start(id, at(base, release + 10), task));
        tracker.asr_end(id, at(base, release + 210));
        id
    }

    fn tone_by_app(app: Option<&AppContext>) -> Tone {
        match app.and_then(|a| a.app_id.as_deref()) {
            Some("com.apple.mail") => Tone::Formal,
            Some("com.tinyspeck.slackmacgap") => Tone::Casual,
            _ => Tone::Default,
        }
    }

    #[test]
    fn full_typed_session_reports_every_stage() {
        let base = Instant::now();
        let task = task_ids(1)[0];
        let mut tracker = Tracker::new();
        tracker.press(base);
        tracker.mic_ready(at(base, 40));
        let id = tracker.release(at(base, 2_000));
        assert!(tracker.asr_start(id, at(base, 2_005), Some(task)));
        tracker.asr_end(id, at(base, 2_305));
        let session = tracker.awaiting_cleanup(Some(task)).expect("cleanup");
        session.cleanup_start = Some(at(base, 2_306));
        session.cleanup_end = Some(at(base, 2_806));
        session.cleanup_ran = true;
        tracker.paste_start(id, at(base, 2_810));
        tracker.paste_end(id, at(base, 2_900));

        let done = at(base, 2_901);
        let session = tracker
            .complete(done, Duration::from_millis(901))
            .expect("session completes");
        let report = TimingReport::new(
            &session,
            TranscriptionOutcome::Typed,
            1.234,
            "small".into(),
            done,
        );

        assert_eq!(report.press_to_mic_ready_ms, Some(40.0));
        assert_eq!(report.release_to_asr_start_ms, Some(5.0));
        assert_eq!(report.asr_ms, Some(300.0));
        assert_eq!(report.cleanup_ms, Some(500.0));
        assert_eq!(report.paste_ms, Some(90.0));
        assert_eq!(report.release_to_pasted_ms, Some(900.0));
        assert_eq!(report.release_to_done_ms, Some(901.0));
        assert_eq!(report.audio_s, 1.23);
        assert!(report.cleanup_ran);
        assert!(tracker.inflight.is_empty());
    }

    #[test]
    fn non_typed_outcome_has_no_pasted_metric() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        transcribed(&mut tracker, base, 0, 1_000, None);
        let done = at(base, 1_300);
        let session = tracker
            .complete(done, Duration::from_millis(300))
            .expect("session completes");
        let report =
            TimingReport::new(&session, TranscriptionOutcome::Empty, 0.0, "m".into(), done);
        assert_eq!(report.release_to_pasted_ms, None);
        assert_eq!(report.cleanup_ms, None);
        assert_eq!(report.release_to_done_ms, Some(300.0));
    }

    #[test]
    fn release_without_press_creates_orphan_session() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        let id = tracker.release(base);
        let session = tracker
            .complete(at(base, 50), Duration::from_millis(50))
            .expect("orphan completes");
        assert_eq!(session.id, id);
        assert_eq!(session.pressed_at, None);
        assert_eq!(span_ms(session.pressed_at, session.mic_ready_at), None);
    }

    #[test]
    fn completion_matches_the_latest_session_released_before_stop() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        let cancelled = transcribed(&mut tracker, base, 0, 1_000, None);
        let live = transcribed(&mut tracker, base, 5_000, 6_000, None);

        let session = tracker
            .complete(at(base, 6_400), Duration::from_millis(400))
            .expect("live session completes");
        assert_eq!(session.id, live);
        assert_eq!(tracker.inflight.len(), 1);
        assert_eq!(tracker.inflight[0].id, cancelled);
    }

    #[test]
    fn completion_without_matching_release_returns_none() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        tracker.press(base);
        tracker.release(at(base, 1_000));
        // The event's stop instant precedes every release mark.
        assert!(tracker
            .complete(at(base, 1_000), Duration::from_millis(500))
            .is_none());
    }

    #[test]
    fn marks_for_an_unknown_session_are_ignored() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        let id = tracker.release(base);
        assert!(!tracker.asr_start(id + 1, at(base, 10), None));
        tracker.paste_start(id + 1, at(base, 20));
        tracker.paste_end(id, at(base, 30));
        let session = &tracker.inflight[0];
        assert_eq!(session.asr_start, None);
        assert_eq!(session.paste_start, None);
        assert_eq!(session.paste_end, None, "no end without a start");
    }

    #[test]
    fn cleanup_attaches_once_and_only_to_its_own_task() {
        let base = Instant::now();
        let tasks = task_ids(2);
        let mut tracker = Tracker::new();
        let id = transcribed(&mut tracker, base, 0, 1_000, Some(tasks[0]));
        assert!(tracker.awaiting_cleanup(None).is_none());
        assert!(tracker.awaiting_cleanup(Some(tasks[1])).is_none());
        let session = tracker.awaiting_cleanup(Some(tasks[0])).expect("own task");
        assert_eq!(session.id, id);
        session.cleanup_start = Some(at(base, 1_300));
        assert!(tracker.awaiting_cleanup(Some(tasks[0])).is_none());
    }

    #[test]
    fn overlapping_sessions_keep_their_own_tone_and_marks() {
        let base = Instant::now();
        let tasks = task_ids(2);
        let (task_a, task_b) = (tasks[0], tasks[1]);
        let mut tracker = Tracker::new();

        // A (Mail) is transcribing when B (Slack) is pressed and released.
        tracker.press(base);
        with_app(&mut tracker, "com.apple.mail");
        let a = tracker.release(at(base, 1_000));
        assert!(tracker.asr_start(a, at(base, 1_010), Some(task_a)));
        tracker.press(at(base, 1_100));
        with_app(&mut tracker, "com.tinyspeck.slackmacgap");
        let b = tracker.release(at(base, 1_500));

        // Formerly A got Default here because B was the newest session.
        tracker.asr_end(a, at(base, 1_600));
        assert_eq!(tracker.choose_tone(Some(task_a), tone_by_app), Tone::Formal);

        // B's ASR and cleanup run while A is still pasting.
        assert!(tracker.asr_start(b, at(base, 1_700), Some(task_b)));
        tracker.asr_end(b, at(base, 1_900));
        tracker.paste_start(a, at(base, 1_950));
        assert_eq!(tracker.choose_tone(Some(task_b), tone_by_app), Tone::Casual);
        tracker.paste_start(b, at(base, 2_000));
        tracker.paste_end(b, at(base, 2_050));
        tracker.paste_end(a, at(base, 2_100));

        let session = |id| {
            tracker
                .inflight
                .iter()
                .find(|s| s.id == id)
                .expect("session")
        };
        let (sa, sb) = (session(a), session(b));
        assert_eq!((sa.tone, sb.tone), (Some(Tone::Formal), Some(Tone::Casual)));
        assert_eq!(sa.asr_end, Some(at(base, 1_600)));
        assert_eq!(sb.asr_start, Some(at(base, 1_700)));
        assert_eq!(span_ms(sa.paste_start, sa.paste_end), Some(150.0));
        assert_eq!(span_ms(sb.paste_start, sb.paste_end), Some(50.0));
    }

    #[test]
    fn retry_outside_a_dictation_task_gets_default_and_leaves_live_session() {
        let base = Instant::now();
        let task = task_ids(1)[0];
        let mut tracker = Tracker::new();
        tracker.press(base);
        with_app(&mut tracker, "com.apple.mail");
        let live = tracker.release(at(base, 1_000));
        assert!(tracker.asr_start(live, at(base, 1_010), Some(task)));
        tracker.asr_end(live, at(base, 1_200));

        let retry = tracker.choose_tone(None, |_| panic!("retry must not see a session"));
        assert_eq!(retry, Tone::Default);
        assert_eq!(tracker.inflight[0].tone, None);
        assert_eq!(tracker.choose_tone(Some(task), tone_by_app), Tone::Formal);
    }

    #[test]
    fn retry_block_on_has_no_task_id_but_a_spawned_dictation_does() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .expect("runtime");
        assert_eq!(runtime.block_on(async { tokio::task::try_id() }), None);
        let spawned = runtime
            .block_on(runtime.spawn(async { tokio::task::try_id() }))
            .expect("task");
        assert!(spawned.is_some());
    }

    #[test]
    fn report_flags_app_capture_without_identity() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        tracker.press(base);
        if let Some(session) = tracker.recording.as_mut() {
            session.app = Some(AppContext {
                app_id: Some("com.tinyspeck.slackmacgap".into()),
                app_name: Some("Slack".into()),
            });
            session.app_capture_us = Some(250);
        }
        tracker.release(at(base, 1_000));
        let done = at(base, 1_100);
        let session = tracker
            .complete(done, Duration::from_millis(100))
            .expect("session completes");
        let report =
            TimingReport::new(&session, TranscriptionOutcome::Empty, 0.0, "m".into(), done);
        assert!(report.app_captured);
        assert_eq!(report.app_capture_us, Some(250));
        let json = serde_json::to_string(&report).expect("serializes");
        assert!(!json.contains("slack"), "identity leaked: {json}");
        assert!(!json.contains("Slack"), "identity leaked: {json}");
    }

    #[test]
    fn new_press_supersedes_unreleased_session() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        let first = tracker.press(base);
        let second = tracker.press(at(base, 100));
        assert_ne!(first, second);
        assert_eq!(tracker.release(at(base, 200)), second);
        assert_eq!(tracker.inflight.len(), 1);
        assert_eq!(tracker.inflight[0].id, second);
    }

    #[test]
    fn inflight_sessions_are_capped_and_pruned() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        for i in 0..(MAX_INFLIGHT as u64 + 2) {
            tracker.release(at(base, i));
        }
        assert_eq!(tracker.inflight.len(), MAX_INFLIGHT);

        tracker.release(base + STALE_AFTER + Duration::from_secs(1));
        assert_eq!(tracker.inflight.len(), 1);
    }

    #[test]
    fn span_rounds_to_tenth_of_a_millisecond() {
        let base = Instant::now();
        let end = base + Duration::from_micros(12_345);
        assert_eq!(span_ms(Some(base), Some(end)), Some(12.3));
        assert_eq!(span_ms(Some(end), Some(base)), None);
    }
}
