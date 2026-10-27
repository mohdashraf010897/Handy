//! Per-dictation stage timing.
//!
//! Upstream code marks pipeline points through one-line hooks; this module
//! groups the marks into sessions and logs exactly one `flow_timing {json}`
//! info line when a dictation finishes (typed, empty, silent or failed).
//! Cancelled dictations log nothing. The headline metric is
//! `release_to_pasted_ms`: hotkey release until the paste call returned.
//!
//! The line holds durations and flags only — never transcript text or the
//! identity of the frontmost app (that is logged at debug level only).

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

/// Released sessions kept while awaiting completion; older ones were cancelled.
const MAX_INFLIGHT: usize = 4;
/// Released sessions older than this are dropped (cancelled or lost).
const STALE_AFTER: Duration = Duration::from_secs(300);
/// Cleanup only attaches to a session whose ASR ended this recently, which
/// keeps the history "retry" path from being attributed to a live dictation.
const LIVE_WINDOW: Duration = Duration::from_secs(30);
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

    fn release(&mut self, now: Instant) {
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
        self.inflight.push(session);
    }

    fn by_id(&mut self, id: u64) -> Option<&mut Session> {
        self.inflight.iter_mut().find(|s| s.id == id)
    }

    fn awaiting_asr(&mut self) -> Option<&mut Session> {
        self.inflight.last_mut().filter(|s| s.asr_start.is_none())
    }

    /// The just-transcribed live session, if cleanup has not started on it.
    fn awaiting_cleanup(&mut self, now: Instant) -> Option<&mut Session> {
        self.inflight.last_mut().filter(|s| {
            s.cleanup_start.is_none()
                && s.asr_end
                    .is_some_and(|end| now.saturating_duration_since(end) < LIVE_WINDOW)
        })
    }

    fn choose_tone(
        &mut self,
        now: Instant,
        choose: impl FnOnce(Option<&AppContext>) -> Tone,
    ) -> Tone {
        let Some(session) = self.awaiting_cleanup(now) else {
            return Tone::Default;
        };
        let tone = choose(session.app.as_ref());
        session.tone = Some(tone);
        tone
    }

    fn awaiting_paste(&mut self) -> Option<&mut Session> {
        self.inflight
            .iter_mut()
            .rev()
            .find(|s| s.asr_end.is_some() && s.paste_start.is_none())
    }

    fn pasting(&mut self) -> Option<&mut Session> {
        self.inflight
            .iter_mut()
            .find(|s| s.paste_start.is_some() && s.paste_end.is_none())
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
        debug!(
            "flow: session {} frontmost app {:?} ({:?}) captured in {capture_us} us",
            session.id,
            app.as_ref().and_then(|a| a.app_id.as_deref()),
            app.as_ref().and_then(|a| a.app_name.as_deref()),
        );
        session.app = app;
        session.app_capture_us = Some(capture_us);
    }
}

/// Marks the first real microphone samples of the recording session.
pub fn mic_ready() {
    tracker().mic_ready(Instant::now());
}

/// Marks the hotkey release and queues the session for completion.
pub fn release() {
    tracker().release(Instant::now());
}

/// Times the speech-to-text call of the session just released.
pub fn asr<T>(transcribe: impl FnOnce() -> T) -> T {
    let id = tracker().awaiting_asr().map(|session| {
        session.asr_start = Some(Instant::now());
        session.id
    });
    let output = transcribe();
    if let Some(id) = id {
        if let Some(session) = tracker().by_id(id) {
            session.asr_end = Some(Instant::now());
        }
    }
    output
}

/// Picks the cleanup tone from the app captured for the live session about
/// to be cleaned up, and records it. `Default` when there is no live session
/// (e.g. a retry from history), so retries never inherit a dictation's tone.
pub fn choose_tone(choose: impl FnOnce(Option<&AppContext>) -> Tone) -> Tone {
    tracker().choose_tone(Instant::now(), choose)
}

/// Times LLM cleanup of the live session; `Some` output means it ran.
pub async fn cleanup<T>(post_process: impl Future<Output = Option<T>>) -> Option<T> {
    let id = tracker().awaiting_cleanup(Instant::now()).map(|session| {
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

/// Marks the paste start of the newest transcribed session not yet pasted.
pub fn paste_start() {
    if let Some(session) = tracker().awaiting_paste() {
        session.paste_start = Some(Instant::now());
    }
}

/// Marks the paste end of the session currently pasting.
pub fn paste_end() {
    if let Some(session) = tracker().pasting() {
        session.paste_end = Some(Instant::now());
    }
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

    fn transcribed(tracker: &mut Tracker, base: Instant, press: u64, release: u64) -> u64 {
        let id = tracker.press(at(base, press));
        tracker.release(at(base, release));
        let session = tracker.awaiting_asr().expect("released session awaits ASR");
        session.asr_start = Some(at(base, release + 10));
        session.asr_end = Some(at(base, release + 210));
        id
    }

    #[test]
    fn full_typed_session_reports_every_stage() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        tracker.press(base);
        tracker.mic_ready(at(base, 40));
        tracker.release(at(base, 2_000));
        let session = tracker.awaiting_asr().expect("awaiting asr");
        session.asr_start = Some(at(base, 2_005));
        session.asr_end = Some(at(base, 2_305));
        let session = tracker.awaiting_cleanup(at(base, 2_306)).expect("cleanup");
        session.cleanup_start = Some(at(base, 2_306));
        session.cleanup_end = Some(at(base, 2_806));
        session.cleanup_ran = true;
        let session = tracker.awaiting_paste().expect("paste");
        session.paste_start = Some(at(base, 2_810));
        let session = tracker.pasting().expect("pasting");
        session.paste_end = Some(at(base, 2_900));

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
        transcribed(&mut tracker, base, 0, 1_000);
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
        tracker.release(base);
        let session = tracker
            .complete(at(base, 50), Duration::from_millis(50))
            .expect("orphan completes");
        assert_eq!(session.pressed_at, None);
        assert_eq!(span_ms(session.pressed_at, session.mic_ready_at), None);
    }

    #[test]
    fn completion_matches_the_latest_session_released_before_stop() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        let cancelled = transcribed(&mut tracker, base, 0, 1_000);
        let live = transcribed(&mut tracker, base, 5_000, 6_000);

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
    fn cleanup_is_not_attributed_to_stale_or_started_sessions() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        transcribed(&mut tracker, base, 0, 1_000);
        assert!(tracker
            .awaiting_cleanup(at(base, 1_210) + LIVE_WINDOW)
            .is_none());
        let session = tracker
            .awaiting_cleanup(at(base, 1_300))
            .expect("fresh session");
        session.cleanup_start = Some(at(base, 1_300));
        assert!(tracker.awaiting_cleanup(at(base, 1_301)).is_none());
    }

    #[test]
    fn paste_attaches_to_newest_transcribed_session() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        transcribed(&mut tracker, base, 0, 1_000);
        let live = transcribed(&mut tracker, base, 5_000, 6_000);
        let session = tracker.awaiting_paste().expect("paste");
        assert_eq!(session.id, live);
        session.paste_start = Some(at(base, 6_300));
        assert_eq!(tracker.pasting().map(|s| s.id), Some(live));
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
    fn tone_is_chosen_only_for_a_live_session() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        let none = tracker.choose_tone(base, |_| panic!("no live session to choose for"));
        assert_eq!(none, Tone::Default);

        tracker.press(base);
        if let Some(session) = tracker.recording.as_mut() {
            session.app = Some(AppContext {
                app_id: Some("com.apple.mail".into()),
                app_name: None,
            });
        }
        tracker.release(at(base, 1_000));
        let session = tracker.awaiting_asr().expect("awaiting asr");
        session.asr_start = Some(at(base, 1_010));
        session.asr_end = Some(at(base, 1_200));
        let tone = tracker.choose_tone(at(base, 1_201), |app| {
            assert_eq!(
                app.and_then(|a| a.app_id.as_deref()),
                Some("com.apple.mail")
            );
            Tone::Formal
        });
        assert_eq!(tone, Tone::Formal);
        assert_eq!(tracker.inflight[0].tone, Some(Tone::Formal));
    }

    #[test]
    fn new_press_supersedes_unreleased_session() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        let first = tracker.press(base);
        let second = tracker.press(at(base, 100));
        assert_ne!(first, second);
        tracker.release(at(base, 200));
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
