//! Lock-free Pause ⇄ Resume session timeline and crash-safe `0600` PCM Write-Ahead Log.
//!
//! Real-time audio callbacks query [`PauseController::is_paused`] via a single
//! `Acquire` atomic load (`< 1 µs`, zero mutex contention), while [`RecordingWal`]
//! bounds in-memory buffering to one 10-second PCM segment (`160_000` samples at
//! 16 kHz = 320 KB) and flushes segments + `manifest.json` atomically with `0700`
//! directory and `0600` file permissions on Unix.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use hound::{SampleFormat, WavSpec, WavWriter};
use log::{debug, warn};
use serde::{Deserialize, Serialize};

/// Default recording sample rate in Hz used when recovering sessions whose
/// `manifest.json` is missing or unreadable.
pub const DEFAULT_SAMPLE_RATE: u32 = 16_000;

/// Segment rotation window in seconds (`10 * 16_000 = 160_000` samples at 16 kHz).
pub const WAL_SEGMENT_SECONDS: u32 = 10;

/// Minimum speech duration (`0.5s` = `8_000` samples at 16 kHz) required to
/// recover an orphaned WAL session into a `.wav` file on startup.
pub const MIN_RECOVERY_SAMPLES_16K: usize = 8_000;

/// Sentinel file written into `<recordings_dir>/active/<session_id>/` while a
/// dictation take is inside its 5-second Escape soft-cancel grace window. If
/// the process crashes or loses power before `[Undo]` clears this marker,
/// startup recovery discards the cancelled audio rather than transcribing it.
pub const SOFT_CANCELLED_SENTINEL: &str = ".soft_cancelled";

/// Marks or unmarks an active WAL directory as soft-cancelled (`0600` sentinel).
pub fn set_wal_soft_cancelled(session_dir: &Path, soft_cancelled: bool) -> io::Result<()> {
    if !session_dir.is_dir() {
        return Ok(());
    }
    let sentinel = session_dir.join(SOFT_CANCELLED_SENTINEL);
    if soft_cancelled {
        write_bytes_atomic_0600(&sentinel, b"1", true)
    } else {
        match fs::remove_file(&sentinel) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

/// Records a completed pause interval relative to active (non-paused) session time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct PauseMarker {
    /// Active (non-paused) milliseconds recorded when the pause began.
    pub at_recorded_ms: u64,
    /// Wall-clock milliseconds spent paused before resuming (or finishing).
    pub paused_duration_ms: u64,
}

#[derive(Debug, Default)]
struct PauseTimeline {
    started_at: Option<Instant>,
    total_paused: Duration,
    /// Present while paused: `(pause_started_instant, active_recorded_ms_at_pause)`.
    pause_started: Option<(Instant, u64)>,
    /// Finalized active duration preserved after [`PauseController::finish_session`].
    completed_active_ms: u64,
    markers: Vec<PauseMarker>,
}

/// Lock-free hot-path Pause ⇄ Resume state machine for recording sessions.
///
/// Audio capture callbacks only call [`Self::is_paused`], which performs a single
/// `Ordering::Acquire` atomic load without touching the internal `Mutex`.
#[derive(Debug)]
pub struct PauseController {
    paused: AtomicBool,
    active: AtomicBool,
    timeline: Mutex<PauseTimeline>,
}

impl Default for PauseController {
    fn default() -> Self {
        Self::new()
    }
}

impl PauseController {
    /// Creates an inactive, unpaused controller.
    pub fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            active: AtomicBool::new(false),
            timeline: Mutex::new(PauseTimeline::default()),
        }
    }

    /// Resets the session timeline and marks the session active and unpaused.
    pub fn start_session(&self) {
        self.start_session_at(Instant::now());
    }

    /// Lock-free pause check (`< 1 µs`) safe for real-time CoreAudio threads.
    #[inline]
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Lock-free check indicating whether a recording session is currently active.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Transitions an active running session into the paused state.
    ///
    /// Returns `true` when the transition occurred, or `false` if the session
    /// was inactive or already paused.
    pub fn pause(&self) -> bool {
        self.pause_at(Instant::now())
    }

    /// Resumes a paused active session and records a [`PauseMarker`].
    ///
    /// Returns `true` when the transition occurred, or `false` if the session
    /// was inactive or not paused.
    pub fn resume(&self) -> bool {
        self.resume_at(Instant::now())
    }

    /// Finalizes any open pause interval, marks the session inactive, and
    /// returns all [`PauseMarker`]s accumulated during the session.
    pub fn finish_session(&self) -> Vec<PauseMarker> {
        self.finish_session_at(Instant::now())
    }

    /// Total active (non-paused) milliseconds recorded in the current or just-finished session.
    pub fn active_elapsed_ms(&self) -> u64 {
        self.active_elapsed_ms_at(Instant::now())
    }

    /// Snapshot of completed [`PauseMarker`]s recorded so far in the session.
    pub fn pause_markers(&self) -> Vec<PauseMarker> {
        self.lock_timeline().markers.clone()
    }

    /// Formats an active timestamp in milliseconds as `"[Paused MM:SS]"`
    /// (for example `75_000` -> `"[Paused 01:15]"`).
    pub fn format_pause_marker(at_recorded_ms: u64) -> String {
        let total_seconds = at_recorded_ms / 1_000;
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        format!("[Paused {minutes:02}:{seconds:02}]")
    }

    fn lock_timeline(&self) -> MutexGuard<'_, PauseTimeline> {
        match self.timeline.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn start_session_at(&self, now: Instant) {
        let mut timeline = self.lock_timeline();
        *timeline = PauseTimeline {
            started_at: Some(now),
            total_paused: Duration::ZERO,
            pause_started: None,
            completed_active_ms: 0,
            markers: Vec::new(),
        };
        self.paused.store(false, Ordering::Release);
        self.active.store(true, Ordering::Release);
    }

    fn pause_at(&self, now: Instant) -> bool {
        let mut timeline = self.lock_timeline();
        if !self.active.load(Ordering::Acquire) || self.paused.load(Ordering::Acquire) {
            return false;
        }
        let Some(started_at) = timeline.started_at else {
            return false;
        };

        let active_duration = now
            .saturating_duration_since(started_at)
            .saturating_sub(timeline.total_paused);
        let at_recorded_ms = duration_to_ms(active_duration);

        self.paused.store(true, Ordering::Release);
        timeline.pause_started = Some((now, at_recorded_ms));
        true
    }

    fn resume_at(&self, now: Instant) -> bool {
        let mut timeline = self.lock_timeline();
        if !self.active.load(Ordering::Acquire) || !self.paused.load(Ordering::Acquire) {
            return false;
        }
        let Some((pause_instant, at_recorded_ms)) = timeline.pause_started.take() else {
            return false;
        };

        let paused_duration = now.saturating_duration_since(pause_instant);
        timeline.total_paused = timeline.total_paused.saturating_add(paused_duration);
        timeline.markers.push(PauseMarker {
            at_recorded_ms,
            paused_duration_ms: duration_to_ms(paused_duration),
        });
        self.paused.store(false, Ordering::Release);
        true
    }

    fn finish_session_at(&self, now: Instant) -> Vec<PauseMarker> {
        let mut timeline = self.lock_timeline();
        if let Some((pause_instant, at_recorded_ms)) = timeline.pause_started.take() {
            let paused_duration = now.saturating_duration_since(pause_instant);
            timeline.total_paused = timeline.total_paused.saturating_add(paused_duration);
            timeline.markers.push(PauseMarker {
                at_recorded_ms,
                paused_duration_ms: duration_to_ms(paused_duration),
            });
            timeline.completed_active_ms = at_recorded_ms;
        } else if let Some(started_at) = timeline.started_at {
            let active_duration = now
                .saturating_duration_since(started_at)
                .saturating_sub(timeline.total_paused);
            timeline.completed_active_ms = duration_to_ms(active_duration);
        }

        timeline.started_at = None;
        self.paused.store(false, Ordering::Release);
        self.active.store(false, Ordering::Release);
        std::mem::take(&mut timeline.markers)
    }

    fn active_elapsed_ms_at(&self, now: Instant) -> u64 {
        let timeline = self.lock_timeline();
        if let Some((_, at_recorded_ms)) = timeline.pause_started {
            return at_recorded_ms;
        }
        if let Some(started_at) = timeline.started_at {
            let active_duration = now
                .saturating_duration_since(started_at)
                .saturating_sub(timeline.total_paused);
            return duration_to_ms(active_duration);
        }
        timeline.completed_active_ms
    }
}

/// On-disk metadata persisted in `<recordings_dir>/active/<session_id>/manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalManifest {
    pub session_id: String,
    pub started_unix_ms: u64,
    pub sample_rate: u32,
    pub segment_count: usize,
    pub total_samples: u64,
    #[serde(default)]
    pub pause_markers: Vec<PauseMarker>,
}

/// Crash-safe 10-second `i16` little-endian PCM Write-Ahead Log (`0700` dir / `0600` files).
#[derive(Debug)]
pub struct RecordingWal {
    active_dir: PathBuf,
    session_dir: PathBuf,
    segment_samples: usize,
    buffer: Vec<i16>,
    byte_scratch: Vec<u8>,
    manifest: WalManifest,
}

impl RecordingWal {
    /// Creates `<recordings_dir>/active/<session_id>/` (`0700` on Unix) and
    /// writes the initial `manifest.json` (`0600`).
    pub fn create(recordings_dir: &Path, session_id: &str, sample_rate: u32) -> io::Result<Self> {
        if !is_safe_session_id(session_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session_id must be a single safe directory component",
            ));
        }
        if sample_rate == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sample_rate must be greater than zero",
            ));
        }

        let segment_samples = (sample_rate as usize).saturating_mul(WAL_SEGMENT_SECONDS as usize);
        if segment_samples == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "segment_samples overflowed",
            ));
        }

        create_dir_0700(recordings_dir)?;
        let active_dir = recordings_dir.join("active");
        create_dir_0700(&active_dir)?;
        let session_dir = active_dir.join(session_id);
        create_dir_0700(&session_dir)?;

        let manifest = WalManifest {
            session_id: session_id.to_string(),
            started_unix_ms: unix_epoch_ms(),
            sample_rate,
            segment_count: 0,
            total_samples: 0,
            pause_markers: Vec::new(),
        };

        let wal = Self {
            active_dir,
            session_dir,
            segment_samples,
            buffer: Vec::with_capacity(segment_samples),
            byte_scratch: Vec::with_capacity(segment_samples.saturating_mul(2)),
            manifest,
        };
        wal.write_manifest(false)?;
        Ok(wal)
    }

    /// Buffers incoming `f32` audio samples and flushes each full 10-second
    /// segment (`sample_rate * 10`) to `seg_NNNNN.pcm` (`0600`) plus `manifest.json`.
    ///
    /// Uses atomic `.tmp` + `rename` without blocking hardware `fsync` during
    /// active capture so consumer frame delivery stays stall-free; `flush_partial`
    /// syncs to stable storage on pause/stop.
    pub fn push_samples(&mut self, samples: &[f32]) -> io::Result<usize> {
        let mut remaining = samples;
        let mut flushed = 0usize;

        while !remaining.is_empty() {
            let available_capacity = self.segment_samples.saturating_sub(self.buffer.len());
            let take = remaining.len().min(available_capacity);
            for &sample in &remaining[..take] {
                self.buffer.push(sample_to_i16(sample));
            }
            remaining = &remaining[take..];

            if self.buffer.len() == self.segment_samples {
                self.flush_buffer_to_segment(false)?;
                self.write_manifest(false)?;
                flushed += 1;
            }
        }

        Ok(flushed)
    }

    /// Flushes any remaining buffered `< 10s` samples (for example on pause or
    /// stop) to the next `seg_NNNNN.pcm` (`0600`) and updates `manifest.json`.
    pub fn flush_partial(&mut self, pause_markers: &[PauseMarker]) -> io::Result<()> {
        if !self.buffer.is_empty() {
            self.flush_buffer_to_segment(true)?;
        }
        self.manifest.pause_markers = pause_markers.to_vec();
        self.write_manifest(true)
    }

    /// Deletes `<recordings_dir>/active/<session_id>/` (and `active/` if empty)
    /// once normal `.wav` + history persistence has succeeded.
    pub fn commit_and_remove(self) -> io::Result<()> {
        if self.session_dir.exists() {
            fs::remove_dir_all(&self.session_dir)?;
        }
        // Best-effort cleanup of the parent `active/` directory when no sibling sessions remain.
        let _ = fs::remove_dir(&self.active_dir);
        Ok(())
    }

    /// Scans `<recordings_dir>/active/*` and recovers orphaned WAL sessions into `.wav` files.
    pub fn recover_orphaned_sessions(recordings_dir: &Path) -> Vec<RecoveredRecording> {
        recover_orphaned_sessions(recordings_dir)
    }

    /// Session identifier associated with this WAL.
    pub fn session_id(&self) -> &str {
        &self.manifest.session_id
    }

    /// Path to `<recordings_dir>/active/<session_id>/`.
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Sample rate in Hz configured for this WAL.
    pub fn sample_rate(&self) -> u32 {
        self.manifest.sample_rate
    }

    /// Number of `.pcm` segments flushed to disk so far.
    pub fn segment_count(&self) -> usize {
        self.manifest.segment_count
    }

    /// Total number of samples already persisted across flushed segments.
    pub fn total_samples(&self) -> u64 {
        self.manifest.total_samples
    }

    fn flush_buffer_to_segment(&mut self, sync_disk: bool) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let seg_name = format!("seg_{:05}.pcm", self.manifest.segment_count);
        let seg_path = self.session_dir.join(seg_name);

        self.byte_scratch.clear();
        self.byte_scratch
            .reserve(self.buffer.len().saturating_mul(2));
        for &sample in &self.buffer {
            self.byte_scratch.extend_from_slice(&sample.to_le_bytes());
        }

        write_bytes_atomic_0600(&seg_path, &self.byte_scratch, sync_disk)?;

        self.manifest.segment_count = self.manifest.segment_count.saturating_add(1);
        self.manifest.total_samples = self
            .manifest
            .total_samples
            .saturating_add(self.buffer.len() as u64);
        self.buffer.clear();
        Ok(())
    }

    fn write_manifest(&self, sync_disk: bool) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(&self.manifest)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let manifest_path = self.session_dir.join("manifest.json");
        write_bytes_atomic_0600(&manifest_path, &json, sync_disk)
    }
}

/// Metadata describing an orphaned WAL session recovered on startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct RecoveredRecording {
    pub session_id: String,
    pub wav_path: PathBuf,
    pub duration_ms: u64,
    pub pause_markers: Vec<PauseMarker>,
}

/// Scans `<recordings_dir>/active/*`, stitches valid orphaned sessions (`>= 0.5s`)
/// into `<recordings_dir>/recovered-<session_id>.wav` (`0600`), and removes empty
/// or corrupt orphan directories cleanly without failing startup.
pub fn recover_orphaned_sessions(recordings_dir: &Path) -> Vec<RecoveredRecording> {
    let active_dir = recordings_dir.join("active");
    let read_dir = match fs::read_dir(&active_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            warn!(
                "flow: failed to read WAL active directory {}: {err}",
                active_dir.display()
            );
            return Vec::new();
        }
    };

    let mut session_dirs: Vec<(String, PathBuf)> = Vec::new();
    for entry_res in read_dir {
        let entry = match entry_res {
            Ok(e) => e,
            Err(err) => {
                warn!("flow: skipping unreadable WAL directory entry: {err}");
                continue;
            }
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(err) => {
                warn!("flow: cannot inspect WAL entry {}: {err}", path.display());
                continue;
            }
        };

        // Never follow symlinks inside `active/`; remove stray non-directories cleanly.
        if !file_type.is_dir() {
            let _ = fs::remove_file(&path);
            continue;
        }

        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            let _ = fs::remove_dir_all(&path);
            continue;
        };

        if !is_safe_session_id(&name) {
            let _ = fs::remove_dir_all(&path);
            continue;
        }

        session_dirs.push((name, path));
    }

    session_dirs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut recovered = Vec::new();
    for (session_id, session_dir) in session_dirs {
        match recover_single_session(recordings_dir, &session_id, &session_dir) {
            SessionRecoveryOutcome::Recovered(item) => {
                if let Err(err) = fs::remove_dir_all(&session_dir) {
                    warn!(
                        "flow: recovered {} but failed to remove WAL dir {}: {err}",
                        item.wav_path.display(),
                        session_dir.display()
                    );
                }
                recovered.push(item);
            }
            SessionRecoveryOutcome::Discard => {
                if let Err(err) = fs::remove_dir_all(&session_dir) {
                    warn!(
                        "flow: failed to clean up empty/corrupt WAL dir {}: {err}",
                        session_dir.display()
                    );
                }
            }
            SessionRecoveryOutcome::KeepForRetry => {
                // Writing the output WAV failed (e.g. disk full); preserve the WAL segments.
            }
        }
    }

    let _ = fs::remove_dir(&active_dir);
    recovered
}

enum SessionRecoveryOutcome {
    Recovered(RecoveredRecording),
    Discard,
    KeepForRetry,
}

fn recover_single_session(
    recordings_dir: &Path,
    session_id: &str,
    session_dir: &Path,
) -> SessionRecoveryOutcome {
    if session_dir.join(SOFT_CANCELLED_SENTINEL).exists() {
        debug!(
            "flow: discarding soft-cancelled WAL session {} without recovery",
            session_dir.display()
        );
        return SessionRecoveryOutcome::Discard;
    }

    let manifest = read_manifest_lenient(&session_dir.join("manifest.json"));
    let sample_rate = manifest
        .as_ref()
        .map(|m| m.sample_rate)
        .filter(|&sr| sr > 0)
        .map_or(DEFAULT_SAMPLE_RATE, |sr| sr);
    let pause_markers = manifest
        .map(|m| m.pause_markers)
        .map_or_else(Vec::new, |markers| markers);

    let segment_paths = match collect_sorted_segments(session_dir) {
        Ok(paths) => paths,
        Err(err) => {
            warn!(
                "flow: corrupt WAL directory {}: {err}; removing",
                session_dir.display()
            );
            return SessionRecoveryOutcome::Discard;
        }
    };

    if segment_paths.is_empty() {
        debug!(
            "flow: removing empty WAL session directory {}",
            session_dir.display()
        );
        return SessionRecoveryOutcome::Discard;
    }

    let mut samples: Vec<i16> = Vec::new();
    let mut non_empty_segments = 0usize;

    for seg_path in &segment_paths {
        let bytes = match fs::read(seg_path) {
            Ok(b) => b,
            Err(err) => {
                warn!(
                    "flow: unreadable WAL segment {}: {err}; discarding session",
                    seg_path.display()
                );
                return SessionRecoveryOutcome::Discard;
            }
        };

        if bytes.len() % 2 != 0 {
            warn!(
                "flow: corrupt odd-byte WAL segment {}; discarding session",
                seg_path.display()
            );
            return SessionRecoveryOutcome::Discard;
        }

        let seg_samples = bytes.len() / 2;
        if seg_samples > 0 {
            non_empty_segments += 1;
            samples.reserve(seg_samples);
            for chunk in bytes.chunks_exact(2) {
                samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
            }
        }
    }

    // Require >= 1 non-empty segment and >= 0.5s of speech (8,000 samples at 16 kHz).
    let min_samples = if sample_rate == DEFAULT_SAMPLE_RATE {
        MIN_RECOVERY_SAMPLES_16K
    } else {
        ((sample_rate as usize) / 2).max(1)
    };

    if non_empty_segments == 0 || samples.len() < min_samples {
        debug!(
            "flow: discarding short/empty orphan WAL session {} ({} samples < {})",
            session_id,
            samples.len(),
            min_samples
        );
        return SessionRecoveryOutcome::Discard;
    }

    let wav_path = recordings_dir.join(format!("recovered-{session_id}.wav"));
    if let Err(err) = write_recovered_wav(&wav_path, sample_rate, &samples) {
        warn!(
            "flow: failed to write recovered WAV {}: {err}",
            wav_path.display()
        );
        return SessionRecoveryOutcome::KeepForRetry;
    }

    let duration_ms = (samples.len() as u64).saturating_mul(1_000) / u64::from(sample_rate);

    SessionRecoveryOutcome::Recovered(RecoveredRecording {
        session_id: session_id.to_string(),
        wav_path,
        duration_ms,
        pause_markers,
    })
}

fn read_manifest_lenient(path: &Path) -> Option<WalManifest> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn collect_sorted_segments(session_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut indexed: Vec<(bool, usize, String, PathBuf)> = Vec::new();
    for entry_res in fs::read_dir(session_dir)? {
        let entry = entry_res?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with("seg_") || !name.ends_with(".pcm") {
            continue;
        }
        let (unnumbered, idx) = match parse_segment_index(&name) {
            Some(i) => (false, i),
            None => (true, usize::MAX),
        };
        indexed.push((unnumbered, idx, name, entry.path()));
    }

    indexed.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    Ok(indexed.into_iter().map(|(_, _, _, path)| path).collect())
}

fn parse_segment_index(file_name: &str) -> Option<usize> {
    let stem = file_name.strip_prefix("seg_")?.strip_suffix(".pcm")?;
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse::<usize>().ok()
}

fn write_recovered_wav(path: &Path, sample_rate: u32, samples: &[i16]) -> io::Result<()> {
    let max_wav_samples = (u32::MAX as usize - 44) / std::mem::size_of::<i16>();
    if samples.len() > max_wav_samples {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PCM stream exceeds maximum 32-bit RIFF WAV capacity",
        ));
    }

    let tmp_path = temp_path_for(path);
    let write_res = (|| -> io::Result<()> {
        let file = open_write_0600(&tmp_path)?;
        let buf_writer = BufWriter::with_capacity(64 * 1024, file);
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut writer = WavWriter::new(buf_writer, spec).map_err(hound_to_io_error)?;
        for &sample in samples {
            writer.write_sample(sample).map_err(hound_to_io_error)?;
        }
        writer.finalize().map_err(hound_to_io_error)?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

fn hound_to_io_error(err: hound::Error) -> io::Error {
    match err {
        hound::Error::IoError(io_err) => io_err,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

#[inline]
fn sample_to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

fn duration_to_ms(duration: Duration) -> u64 {
    match u64::try_from(duration.as_millis()) {
        Ok(ms) => ms,
        Err(_) => u64::MAX,
    }
}

fn unix_epoch_ms() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration_to_ms(duration),
        Err(_) => 0,
    }
}

fn is_safe_session_id(session_id: &str) -> bool {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains('\0')
    {
        return false;
    }
    let mut components = Path::new(session_id).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".tmp");
    PathBuf::from(os)
}

fn create_dir_0700(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

fn open_write_0600(path: &Path) -> io::Result<File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    #[cfg(unix)]
    {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn write_bytes_atomic_0600(path: &Path, bytes: &[u8], sync_disk: bool) -> io::Result<()> {
    let tmp_path = temp_path_for(path);
    let write_res = (|| -> io::Result<()> {
        let mut file = open_write_0600(&tmp_path)?;
        file.write_all(bytes)?;
        if sync_disk {
            file.sync_data()?;
        }
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hound::WavReader;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;

    #[test]
    fn pause_controller_tracks_active_vs_paused_time_and_markers() {
        let controller = PauseController::new();
        assert!(!controller.is_active());
        assert!(!controller.is_paused());
        assert!(!controller.pause(), "cannot pause while inactive");
        assert!(!controller.resume(), "cannot resume while inactive");

        // Deterministic timeline check: start -> pause at 01:05 -> resume -> pause -> finish while paused.
        let t0 = Instant::now();
        controller.start_session_at(t0);
        assert!(controller.is_active());
        assert!(!controller.is_paused());

        let t_pause_1 = t0 + Duration::from_millis(65_000);
        assert!(controller.pause_at(t_pause_1));
        assert!(controller.is_paused());
        assert!(!controller.pause_at(t_pause_1 + Duration::from_millis(500)));
        assert_eq!(
            controller.active_elapsed_ms_at(t_pause_1 + Duration::from_millis(12_000)),
            65_000,
            "active_elapsed_ms must stay frozen while paused"
        );

        let t_resume_1 = t_pause_1 + Duration::from_millis(20_000);
        assert!(controller.resume_at(t_resume_1));
        assert!(!controller.is_paused());
        assert!(!controller.resume_at(t_resume_1 + Duration::from_millis(100)));

        // Record 10 more active seconds (total active = 75_000 ms), then pause and finish while still paused.
        let t_pause_2 = t_resume_1 + Duration::from_millis(10_000);
        assert_eq!(controller.active_elapsed_ms_at(t_pause_2), 75_000);
        assert!(controller.pause_at(t_pause_2));
        assert!(controller.is_paused());

        let t_finish = t_pause_2 + Duration::from_millis(5_500);
        let markers = controller.finish_session_at(t_finish);
        assert!(!controller.is_active());
        assert!(!controller.is_paused());
        assert_eq!(controller.active_elapsed_ms_at(t_finish), 75_000);
        assert_eq!(
            markers,
            vec![
                PauseMarker {
                    at_recorded_ms: 65_000,
                    paused_duration_ms: 20_000,
                },
                PauseMarker {
                    at_recorded_ms: 75_000,
                    paused_duration_ms: 5_500,
                },
            ]
        );
        assert_eq!(
            PauseController::format_pause_marker(markers[0].at_recorded_ms),
            "[Paused 01:05]"
        );
        assert_eq!(
            PauseController::format_pause_marker(markers[1].at_recorded_ms),
            "[Paused 01:15]"
        );

        // Exercise public wall-clock wrappers end-to-end as well.
        controller.start_session();
        assert!(controller.is_active());
        assert!(!controller.is_paused());
        assert!(controller.pause());
        assert!(controller.is_paused());
        assert!(controller.resume());
        assert!(!controller.is_paused());
        let live_markers = controller.finish_session();
        assert_eq!(live_markers.len(), 1);
        assert!(!controller.is_active());
    }

    #[test]
    fn wal_rotates_every_10_seconds_with_0600_permissions() {
        let temp = TempDir::new().expect("tempdir");
        let recordings_dir = temp.path().join("recordings");
        let mut wal =
            RecordingWal::create(&recordings_dir, "session-rotate-01", 16_000).expect("create wal");

        // 2.5 * 10s at 16,000 Hz = 400,000 samples -> 2 full 10s segments + 80,000 buffered samples.
        let samples: Vec<f32> = (0..400_000)
            .map(|i| ((i % 200) as f32 - 100.0) / 100.0)
            .collect();
        let flushed = wal.push_samples(&samples).expect("push samples");
        assert_eq!(flushed, 2);
        assert_eq!(wal.segment_count(), 2);
        assert_eq!(wal.total_samples(), 320_000);

        let session_dir = recordings_dir.join("active").join("session-rotate-01");
        let seg0 = session_dir.join("seg_00000.pcm");
        let seg1 = session_dir.join("seg_00001.pcm");
        let seg2 = session_dir.join("seg_00002.pcm");
        let manifest = session_dir.join("manifest.json");

        assert!(seg0.exists(), "seg_00000.pcm must exist");
        assert!(seg1.exists(), "seg_00001.pcm must exist");
        assert!(!seg2.exists(), "partial 0.5x segment must stay buffered");
        assert!(manifest.exists(), "manifest.json must exist");
        assert_eq!(fs::metadata(&seg0).expect("seg0 meta").len(), 320_000);
        assert_eq!(fs::metadata(&seg1).expect("seg1 meta").len(), 320_000);

        #[cfg(unix)]
        {
            let active_mode = fs::metadata(recordings_dir.join("active"))
                .expect("active meta")
                .mode()
                & 0o777;
            let dir_mode = fs::metadata(&session_dir).expect("dir meta").mode() & 0o777;
            let seg0_mode = fs::metadata(&seg0).expect("seg0 meta").mode() & 0o777;
            let seg1_mode = fs::metadata(&seg1).expect("seg1 meta").mode() & 0o777;
            let manifest_mode = fs::metadata(&manifest).expect("manifest meta").mode() & 0o777;

            assert_eq!(active_mode, 0o700);
            assert_eq!(dir_mode, 0o700);
            assert_eq!(seg0_mode, 0o600);
            assert_eq!(seg1_mode, 0o600);
            assert_eq!(manifest_mode, 0o600);
        }

        wal.commit_and_remove().expect("commit and remove");
        assert!(!session_dir.exists(), "session WAL dir must be removed");
        assert!(
            !recordings_dir.join("active").exists(),
            "empty active dir must be removed"
        );
        let remaining_entries: Vec<_> = fs::read_dir(&recordings_dir)
            .expect("read recordings_dir")
            .collect();
        assert!(
            remaining_entries.is_empty(),
            "commit_and_remove must leave zero files behind"
        );
    }

    #[test]
    fn wal_recovers_orphaned_session_into_valid_wav() {
        let temp = TempDir::new().expect("tempdir");
        let recordings_dir = temp.path().join("recordings");

        let expected_markers = vec![PauseMarker {
            at_recorded_ms: 65_000,
            paused_duration_ms: 3_400,
        }];

        // 12.5 seconds at 16 kHz = 200,000 samples (1 full 10s segment + 2.5s partial segment).
        let input_samples: Vec<f32> = (0..200_000)
            .map(|i| (((i % 64) as f32) - 32.0) / 32.0)
            .collect();

        {
            let mut crashed_wal = RecordingWal::create(&recordings_dir, "lesson-crash-42", 16_000)
                .expect("create crashed wal");
            assert_eq!(
                crashed_wal
                    .push_samples(&input_samples)
                    .expect("push samples"),
                1
            );
            crashed_wal
                .flush_partial(&expected_markers)
                .expect("flush partial before simulated crash");
            // Drop without `commit_and_remove` to simulate mid-recording crash.
        }

        // Also create an accidental < 0.5s tap (2,000 samples = 0.125s) that must be discarded cleanly.
        {
            let mut tap_wal =
                RecordingWal::create(&recordings_dir, "accidental-tap", 16_000).expect("tap wal");
            tap_wal
                .push_samples(&vec![0.25_f32; 2_000])
                .expect("push short tap");
            tap_wal.flush_partial(&[]).expect("flush short tap");
        }

        let recovered = recover_orphaned_sessions(&recordings_dir);
        assert_eq!(recovered.len(), 1);

        let item = &recovered[0];
        assert_eq!(item.session_id, "lesson-crash-42");
        assert_eq!(item.duration_ms, 12_500);
        assert_eq!(item.pause_markers, expected_markers);

        let expected_wav = recordings_dir.join("recovered-lesson-crash-42.wav");
        assert_eq!(item.wav_path, expected_wav);
        assert!(expected_wav.exists(), "recovered WAV file must exist");
        assert!(
            !recordings_dir.join("recovered-accidental-tap.wav").exists(),
            "< 0.5s orphan tap must not produce a WAV"
        );
        assert!(
            !recordings_dir.join("active").exists(),
            "active/ directory must be cleaned up after recovery"
        );

        #[cfg(unix)]
        {
            let wav_mode = fs::metadata(&expected_wav).expect("wav meta").mode() & 0o777;
            assert_eq!(wav_mode, 0o600);
        }

        let mut reader = WavReader::open(&expected_wav).expect("open recovered wav");
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, SampleFormat::Int);
        assert_eq!(reader.len(), 200_000);

        let decoded: Vec<i16> = reader
            .samples::<i16>()
            .map(|s| s.expect("valid sample"))
            .collect();
        let expected_pcm: Vec<i16> = input_samples.iter().copied().map(sample_to_i16).collect();
        assert_eq!(decoded, expected_pcm);
    }

    #[test]
    fn wal_discards_soft_cancelled_session_unless_undo_clears_sentinel() {
        let temp = TempDir::new().expect("tempdir");
        let recordings_dir = temp.path().join("recordings");
        let speech_samples: Vec<f32> = (0..24_000).map(|i| ((i % 32) as f32) / 32.0).collect();

        // Session 1: soft-cancelled and never undone before crash -> must be discarded.
        {
            let mut cancelled_wal =
                RecordingWal::create(&recordings_dir, "soft-cancelled-take", 16_000)
                    .expect("cancelled wal");
            cancelled_wal
                .push_samples(&speech_samples)
                .expect("push cancelled speech");
            cancelled_wal
                .flush_partial(&[])
                .expect("flush cancelled speech");
            set_wal_soft_cancelled(cancelled_wal.session_dir(), true).expect("mark soft cancelled");
        }

        // Session 2: soft-cancelled, then `[Undo]` cleared sentinel before crash -> must recover.
        {
            let mut undone_wal =
                RecordingWal::create(&recordings_dir, "undone-take", 16_000).expect("undone wal");
            undone_wal
                .push_samples(&speech_samples)
                .expect("push undone speech");
            undone_wal.flush_partial(&[]).expect("flush undone speech");
            set_wal_soft_cancelled(undone_wal.session_dir(), true).expect("mark soft cancelled");
            set_wal_soft_cancelled(undone_wal.session_dir(), false)
                .expect("clear soft cancelled on undo");
        }

        let recovered = recover_orphaned_sessions(&recordings_dir);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].session_id, "undone-take");
        assert!(
            !recordings_dir
                .join("recovered-soft-cancelled-take.wav")
                .exists(),
            "soft-cancelled WAL must never be recovered after crash"
        );
    }
}
