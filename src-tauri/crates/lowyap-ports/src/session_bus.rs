//! Session lifecycle state and projections shared by desktop and frontend.
//!
//! The caller supplies millisecond timestamps from one timeline and schedules
//! any timers. This module has no side effects; even an overdue soft cancel
//! remains pending until the caller applies the expiration transition.

use serde::{Deserialize, Serialize};

use crate::domain::{AudioCaptureMode, OutputRouteKind};

/// Default undo window for a softly cancelled dictation take.
pub const DEFAULT_SOFT_CANCEL_GRACE_MS: u64 = 5_000;

/// Current dictation lifecycle state, discriminated by `phase` on the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "phase",
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
)]
pub enum DictationPhase {
    /// There is no active dictation take.
    #[default]
    Idle,
    /// Capturing dictation or temporarily paused.
    Recording {
        /// Identity of the take.
        take_id: u64,
        /// When recording started, or the cancellation time after an undo.
        started_at_ms: u64,
        /// Whether recording stays active when the start shortcut is released.
        locked: bool,
        /// Whether capture is currently paused.
        paused: bool,
    },
    /// Capture is suspended and the take can still be restored before its deadline.
    SoftCancelled {
        /// Identity of the retained take.
        take_id: u64,
        /// When the take was softly cancelled.
        cancelled_at_ms: u64,
        /// First time at which undo is no longer allowed.
        expires_at_ms: u64,
        /// Requested undo-window duration.
        grace_ms: u64,
    },
    /// Speech recognition is in progress.
    Transcribing {
        /// Identity of the take being transcribed.
        take_id: u64,
    },
    /// The transcript is being processed for delivery.
    PostProcessing {
        /// Identity of the take being processed.
        take_id: u64,
    },
}

impl DictationPhase {
    fn take_id(&self) -> Option<u64> {
        match *self {
            Self::Idle => None,
            Self::Recording { take_id, .. }
            | Self::SoftCancelled { take_id, .. }
            | Self::Transcribing { take_id }
            | Self::PostProcessing { take_id } => Some(take_id),
        }
    }
}

/// A detected call that can be offered for meeting capture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DetectedCallBanner {
    /// Identifier of this detected call.
    pub call_id: String,
    /// Display name of the source application.
    pub app_name: String,
    /// Detected audio output route.
    pub output_route: OutputRouteKind,
}

/// Current meeting lifecycle state, discriminated by `phase` on the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "phase",
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
)]
pub enum MeetingPhase {
    /// No meeting is in progress.
    #[default]
    Idle,
    /// Meeting capture is running.
    Recording {
        /// Identifier of this meeting.
        meeting_id: String,
        /// Display title.
        title: String,
        /// Original recording start, unchanged across pauses.
        started_at_ms: u64,
        /// Duration of all completed pauses.
        accumulated_paused_ms: u64,
        /// Source application, when known.
        app_name: Option<String>,
        /// Available capture streams.
        capture_mode: AudioCaptureMode,
    },
    /// Meeting capture is suspended.
    Paused {
        /// Identifier of this meeting.
        meeting_id: String,
        /// Display title.
        title: String,
        /// Original recording start.
        started_at_ms: u64,
        /// Start of the current pause.
        paused_at_ms: u64,
        /// Duration of completed pauses, excluding the current pause.
        accumulated_paused_ms: u64,
        /// Source application, when known.
        app_name: Option<String>,
        /// Available capture streams.
        capture_mode: AudioCaptureMode,
    },
    /// Capture has ended and notes are being generated.
    Summarizing {
        /// Identifier of the meeting being summarized.
        meeting_id: String,
        /// Display title.
        title: String,
    },
}

impl MeetingPhase {
    /// Returns active recording time in milliseconds, frozen while paused.
    ///
    /// On resume, the caller adds the completed pause duration to
    /// `accumulated_paused_ms`. Inconsistent timing clamps to zero. `Idle` and
    /// `Summarizing` return zero because neither phase carries timing fields.
    ///
    /// ```
    /// use lowyap_ports::{AudioCaptureMode, MeetingPhase};
    ///
    /// let paused = MeetingPhase::Paused {
    ///     meeting_id: "m1".into(),
    ///     title: "Planning".into(),
    ///     started_at_ms: 100,
    ///     paused_at_ms: 500,
    ///     accumulated_paused_ms: 100,
    ///     app_name: None,
    ///     capture_mode: AudioCaptureMode::DualStream,
    /// };
    /// assert_eq!(paused.elapsed_ms(900), 300);
    /// ```
    pub fn elapsed_ms(&self, now_ms: u64) -> u64 {
        let (end_ms, started_at_ms, paused_ms) = match *self {
            Self::Recording {
                started_at_ms,
                accumulated_paused_ms,
                ..
            } => (now_ms, started_at_ms, accumulated_paused_ms),
            Self::Paused {
                started_at_ms,
                paused_at_ms,
                accumulated_paused_ms,
                ..
            } => (paused_at_ms, started_at_ms, accumulated_paused_ms),
            Self::Idle | Self::Summarizing { .. } => return 0,
        };
        end_ms
            .saturating_sub(started_at_ms)
            .saturating_sub(paused_ms)
    }

    fn meeting_id(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Recording { meeting_id, .. }
            | Self::Paused { meeting_id, .. }
            | Self::Summarizing { meeting_id, .. } => Some(meeting_id),
        }
    }
}

/// What to render in the floating pill, discriminated by `mode` on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
)]
pub enum OverlayProjection {
    /// No pill should be displayed.
    Hidden,
    /// Dictation recording controls.
    DictationRecording {
        /// Take controlled by this pill.
        take_id: u64,
        /// Whether recording is locked on.
        locked: bool,
        /// Whether capture is paused.
        paused: bool,
    },
    /// Dictation undo controls and countdown.
    DictationSoftCancelled {
        /// Take that can be restored.
        take_id: u64,
        /// Time at which the countdown started.
        cancelled_at_ms: u64,
        /// Time at which the countdown ends.
        expires_at_ms: u64,
        /// Requested countdown duration.
        grace_ms: u64,
    },
    /// Dictation speech recognition indicator.
    DictationTranscribing {
        /// Take being transcribed.
        take_id: u64,
    },
    /// Dictation transcript processing indicator.
    DictationPostProcessing {
        /// Take being processed.
        take_id: u64,
    },
    /// Controls for a recording or paused meeting.
    MeetingActive {
        /// Meeting controlled by this pill.
        meeting_id: String,
        /// Display title.
        title: String,
        /// Whether meeting capture is paused.
        paused: bool,
        /// Active recording time at snapshot time.
        elapsed_ms: u64,
        /// Original recording start.
        started_at_ms: u64,
        /// Total duration of completed pauses.
        accumulated_paused_ms: u64,
        /// Start of the current pause, if paused.
        paused_at_ms: Option<u64>,
        /// Source application, when known.
        app_name: Option<String>,
    },
    /// Meeting note generation indicator.
    MeetingSummarizing {
        /// Meeting being summarized.
        meeting_id: String,
        /// Display title.
        title: String,
    },
    /// Offer to start capturing a detected call.
    CallDetected {
        /// Identifier of this detected call.
        call_id: String,
        /// Display name of the source application.
        app_name: String,
        /// Detected audio output route.
        output_route: OutputRouteKind,
    },
    /// A temporary notice not tied to an active session.
    TransientNotice {
        /// Notice identifier interpreted by the client.
        notice_kind: String,
    },
}

/// Meeting state for the tray, independent of which pill is visible.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TrayMeetingProjection {
    /// Whether a meeting is recording or paused.
    pub active: bool,
    /// Whether meeting capture is paused.
    pub paused: bool,
    /// Whether notes are being generated after capture ended.
    pub summarizing: bool,
    /// Current meeting identifier, including while summarizing.
    pub meeting_id: Option<String>,
    /// Current meeting title, including while summarizing.
    pub title: Option<String>,
    /// Active recording time; zero when no capture timing is available.
    pub elapsed_ms: u64,
}

impl TrayMeetingProjection {
    fn from_phase(meeting: &MeetingPhase, now_ms: u64) -> Self {
        match meeting {
            MeetingPhase::Idle => Self::default(),
            MeetingPhase::Recording {
                meeting_id, title, ..
            }
            | MeetingPhase::Paused {
                meeting_id, title, ..
            } => Self {
                active: true,
                paused: matches!(meeting, MeetingPhase::Paused { .. }),
                summarizing: false,
                meeting_id: Some(meeting_id.clone()),
                title: Some(title.clone()),
                elapsed_ms: meeting.elapsed_ms(now_ms),
            },
            MeetingPhase::Summarizing { meeting_id, title } => Self {
                summarizing: true,
                meeting_id: Some(meeting_id.clone()),
                title: Some(title.clone()),
                ..Self::default()
            },
        }
    }
}

/// A consistent view of the session phases and their UI projections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionBusSnapshot {
    /// State revision; reading a snapshot does not advance it.
    pub seq: u64,
    /// Time at which the projections were computed.
    pub server_now_ms: u64,
    /// Dictation lifecycle, regardless of pill visibility.
    pub dictation: DictationPhase,
    /// Meeting lifecycle, regardless of pill visibility.
    pub meeting: MeetingPhase,
    /// Current call detection, even if a banner is not visible.
    pub detected_call: Option<DetectedCallBanner>,
    /// Current transient notice, even if dictation takes precedence.
    pub transient_notice: Option<String>,
    /// Foreground pill selected from the current inputs.
    pub overlay: OverlayProjection,
    /// Menu-bar meeting state.
    pub tray_meeting: TrayMeetingProjection,
}

/// Selects the foreground pill: dictation, notice, meeting, then detected call.
///
/// A dismissed recording or paused meeting yields `Hidden`, not a call banner.
/// Dismissal does not hide the summarizing pill or affect the tray.
pub fn resolve_overlay_priority(
    dictation: &DictationPhase,
    meeting: &MeetingPhase,
    detected_call: Option<&DetectedCallBanner>,
    transient_notice: Option<&str>,
    meeting_pill_dismissed: bool,
    now_ms: u64,
) -> OverlayProjection {
    match *dictation {
        DictationPhase::Idle => {}
        DictationPhase::Recording {
            take_id,
            locked,
            paused,
            ..
        } => {
            return OverlayProjection::DictationRecording {
                take_id,
                locked,
                paused,
            };
        }
        DictationPhase::SoftCancelled {
            take_id,
            cancelled_at_ms,
            expires_at_ms,
            grace_ms,
        } => {
            return OverlayProjection::DictationSoftCancelled {
                take_id,
                cancelled_at_ms,
                expires_at_ms,
                grace_ms,
            };
        }
        DictationPhase::Transcribing { take_id } => {
            return OverlayProjection::DictationTranscribing { take_id };
        }
        DictationPhase::PostProcessing { take_id } => {
            return OverlayProjection::DictationPostProcessing { take_id };
        }
    }

    if let Some(notice_kind) = transient_notice {
        return OverlayProjection::TransientNotice {
            notice_kind: notice_kind.to_owned(),
        };
    }

    match meeting {
        MeetingPhase::Idle => match detected_call {
            Some(banner) => OverlayProjection::CallDetected {
                call_id: banner.call_id.clone(),
                app_name: banner.app_name.clone(),
                output_route: banner.output_route,
            },
            None => OverlayProjection::Hidden,
        },
        MeetingPhase::Recording { .. } | MeetingPhase::Paused { .. } if meeting_pill_dismissed => {
            OverlayProjection::Hidden
        }
        MeetingPhase::Recording {
            meeting_id,
            title,
            started_at_ms,
            accumulated_paused_ms,
            app_name,
            ..
        }
        | MeetingPhase::Paused {
            meeting_id,
            title,
            started_at_ms,
            accumulated_paused_ms,
            app_name,
            ..
        } => {
            let paused_at_ms = match meeting {
                MeetingPhase::Paused { paused_at_ms, .. } => Some(*paused_at_ms),
                _ => None,
            };
            OverlayProjection::MeetingActive {
                meeting_id: meeting_id.clone(),
                title: title.clone(),
                paused: paused_at_ms.is_some(),
                elapsed_ms: meeting.elapsed_ms(now_ms),
                started_at_ms: *started_at_ms,
                accumulated_paused_ms: *accumulated_paused_ms,
                paused_at_ms,
                app_name: app_name.clone(),
            }
        }
        MeetingPhase::Summarizing { meeting_id, title } => OverlayProjection::MeetingSummarizing {
            meeting_id: meeting_id.clone(),
            title: title.clone(),
        },
    }
}

/// Pure coordinator for session state and foreground priority.
///
/// Serialize calls through a single owner or a lock. Use a fresh `take_id` for
/// each new take and pass both the ID and epoch to scheduled cancel callbacks.
#[derive(Debug, Default)]
pub struct SessionStateMachine {
    seq: u64,
    dictation: DictationPhase,
    meeting: MeetingPhase,
    detected_call: Option<DetectedCallBanner>,
    transient_notice: Option<String>,
    meeting_pill_dismissed: bool,
    cancel_epoch: u64,
}

impl SessionStateMachine {
    /// Creates an idle state machine at revision zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the user has minimized the floating active-meeting pill for the current meeting.
    pub fn is_meeting_pill_dismissed(&self) -> bool {
        self.meeting_pill_dismissed
    }

    /// Starts an unpaused dictation take, superseding any previous take.
    pub fn start_dictation(
        &mut self,
        take_id: u64,
        now_ms: u64,
        locked: bool,
    ) -> SessionBusSnapshot {
        self.dictation = DictationPhase::Recording {
            take_id,
            started_at_ms: now_ms,
            locked,
            paused: false,
        };
        self.advance(now_ms)
    }

    /// Updates pause and lock flags only for the matching recording take.
    pub fn set_dictation_paused(
        &mut self,
        take_id: u64,
        paused: bool,
        locked: bool,
        now_ms: u64,
    ) -> Option<SessionBusSnapshot> {
        let DictationPhase::Recording {
            take_id: current,
            paused: current_paused,
            locked: current_locked,
            ..
        } = &mut self.dictation
        else {
            return None;
        };
        if *current != take_id {
            return None;
        }
        *current_paused = paused;
        *current_locked = locked;
        Some(self.advance(now_ms))
    }

    /// Softly cancels the matching recording take and returns its timer epoch.
    pub fn soft_cancel_dictation(
        &mut self,
        take_id: u64,
        now_ms: u64,
        grace_ms: u64,
    ) -> Option<(u64, SessionBusSnapshot)> {
        if !matches!(self.dictation, DictationPhase::Recording { take_id: current, .. } if current == take_id)
        {
            return None;
        }
        // Never reuse an epoch, even for successive cancellations of one take.
        self.cancel_epoch = self.cancel_epoch.checked_add(1)?;
        self.dictation = DictationPhase::SoftCancelled {
            take_id,
            cancelled_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(grace_ms),
            grace_ms,
        };
        Some((self.cancel_epoch, self.advance(now_ms)))
    }

    /// Restores the matching take to locked, unpaused recording before the deadline.
    pub fn undo_soft_cancel(&mut self, take_id: u64, now_ms: u64) -> Option<SessionBusSnapshot> {
        let DictationPhase::SoftCancelled {
            take_id: current,
            cancelled_at_ms,
            expires_at_ms,
            ..
        } = self.dictation
        else {
            return None;
        };
        if take_id != current || now_ms >= expires_at_ms {
            return None;
        }
        self.dictation = DictationPhase::Recording {
            take_id,
            started_at_ms: cancelled_at_ms,
            locked: true,
            paused: false,
        };
        Some(self.advance(now_ms))
    }

    /// Finalizes a matching soft cancel. A timer (`Some(epoch)`) must match the
    /// current epoch and reach the deadline; explicit confirmation (`None`) need
    /// not wait. Neither can cancel a take that has already resumed.
    pub fn expire_or_confirm_cancel(
        &mut self,
        take_id: u64,
        expected_epoch: Option<u64>,
        now_ms: u64,
    ) -> Option<SessionBusSnapshot> {
        let DictationPhase::SoftCancelled {
            take_id: current,
            expires_at_ms,
            ..
        } = self.dictation
        else {
            return None;
        };
        if take_id != current
            || expected_epoch
                .is_some_and(|epoch| epoch != self.cancel_epoch || now_ms < expires_at_ms)
        {
            return None;
        }
        self.dictation = DictationPhase::Idle;
        Some(self.advance(now_ms))
    }

    /// Advances the matching recording take to transcription.
    pub fn finish_dictation_to_transcribing(
        &mut self,
        take_id: u64,
        now_ms: u64,
    ) -> Option<SessionBusSnapshot> {
        if !matches!(self.dictation, DictationPhase::Recording { take_id: current, .. } if current == take_id)
        {
            return None;
        }
        self.dictation = DictationPhase::Transcribing { take_id };
        Some(self.advance(now_ms))
    }

    /// Advances the matching transcribing take to post-processing.
    pub fn finish_dictation_to_post_processing(
        &mut self,
        take_id: u64,
        now_ms: u64,
    ) -> Option<SessionBusSnapshot> {
        if self.dictation != (DictationPhase::Transcribing { take_id }) {
            return None;
        }
        self.dictation = DictationPhase::PostProcessing { take_id };
        Some(self.advance(now_ms))
    }

    /// Clears the matching active take, or any active take when no ID is supplied.
    /// If there is no matching take, returns a fresh snapshot at the same revision.
    pub fn complete_or_abort_dictation(
        &mut self,
        take_id: Option<u64>,
        now_ms: u64,
    ) -> SessionBusSnapshot {
        if let Some(current) = self.dictation.take_id() {
            if take_id.is_none() || take_id == Some(current) {
                self.dictation = DictationPhase::Idle;
                return self.advance(now_ms);
            }
        }
        self.snapshot(now_ms)
    }

    /// Replaces meeting and call state together. A different meeting ID clears
    /// the dismissal; updates to the same meeting preserve it.
    pub fn sync_meeting(
        &mut self,
        meeting: MeetingPhase,
        detected_call: Option<DetectedCallBanner>,
        now_ms: u64,
    ) -> SessionBusSnapshot {
        if meeting.meeting_id() != self.meeting.meeting_id() {
            self.meeting_pill_dismissed = false;
        }
        self.meeting = meeting;
        self.detected_call = detected_call;
        self.advance(now_ms)
    }

    /// Hides or restores the current meeting's recording or paused pill.
    pub fn set_meeting_pill_dismissed(
        &mut self,
        dismissed: bool,
        now_ms: u64,
    ) -> SessionBusSnapshot {
        self.meeting_pill_dismissed = dismissed;
        self.advance(now_ms)
    }

    /// Sets or clears a transient notice; its lifetime is owned by the caller.
    pub fn set_transient_notice(
        &mut self,
        notice: Option<String>,
        now_ms: u64,
    ) -> SessionBusSnapshot {
        self.transient_notice = notice;
        self.advance(now_ms)
    }

    /// Projects current state at `now_ms` without applying transitions or mutating state.
    pub fn snapshot(&self, now_ms: u64) -> SessionBusSnapshot {
        SessionBusSnapshot {
            seq: self.seq,
            server_now_ms: now_ms,
            dictation: self.dictation.clone(),
            meeting: self.meeting.clone(),
            detected_call: self.detected_call.clone(),
            transient_notice: self.transient_notice.clone(),
            overlay: resolve_overlay_priority(
                &self.dictation,
                &self.meeting,
                self.detected_call.as_ref(),
                self.transient_notice.as_deref(),
                self.meeting_pill_dismissed,
                now_ms,
            ),
            tray_meeting: TrayMeetingProjection::from_phase(&self.meeting, now_ms),
        }
    }

    fn advance(&mut self, now_ms: u64) -> SessionBusSnapshot {
        self.seq = self.seq.saturating_add(1);
        self.snapshot(now_ms)
    }
}

#[cfg(test)]
mod tests;
