//! Deterministic synthetic multi-stream audio and session benchmark scenarios.
//!
//! All audio waveforms are synthesized mathematically at 16 kHz mono without
//! external audio files or private recordings.

use std::f32::consts::TAU;

use lowyap_ports::{
    AudioCaptureMode, DetectedCallBanner, DictationPhase, DualStreamChunk16k, MeetingPhase,
    OutputRouteKind, OverlayProjection, SessionStateMachine, SpeakerChannel, SpeakerIdentity,
    TranscriptSegment, DEFAULT_SOFT_CANCEL_GRACE_MS,
};

use crate::reference_adapters::SAMPLE_RATE_16K;

/// Chunk duration used when streaming synthetic dual-stream scenarios.
pub const STREAM_CHUNK_MS: usize = 200;
/// Samples per synthetic streaming chunk (`3_200` samples = 200 ms at 16 kHz).
pub const STREAM_CHUNK_SAMPLES: usize = SAMPLE_RATE_16K * STREAM_CHUNK_MS / 1_000;

/// Simple deterministic xorshift64 PRNG so synthetic noise is 100% reproducible.
#[derive(Debug, Clone)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Returns a deterministic value uniformly distributed in `[-1.0, 1.0]`.
    fn next_signed_f32(&mut self) -> f32 {
        let bits = (self.next_u64() >> 40) as u32;
        (bits as f32 / ((1_u32 << 24) as f32)) * 2.0 - 1.0
    }
}

/// Synthesizes deterministic ambient room-tone noise with target RMS amplitude.
pub fn synth_room_noise(num_samples: usize, target_rms: f32, seed: u64) -> Vec<f32> {
    let mut rng = DeterministicRng::new(seed);
    // Uniform [-1, 1] has RMS = 1 / sqrt(3) ≈ 0.57735.
    let scale = target_rms * 3.0_f32.sqrt();
    (0..num_samples)
        .map(|_| rng.next_signed_f32() * scale)
        .collect()
}

/// Synthesizes band-limited voiced speech-like audio with natural harmonic
/// rolloff, subtle vibrato, and syllabic amplitude modulation.
pub fn synth_voiced_speech(duration_ms: usize, base_f0_hz: f32, amp: f32, seed: u64) -> Vec<f32> {
    let num_samples = duration_ms * SAMPLE_RATE_16K / 1_000;
    let sr = SAMPLE_RATE_16K as f32;
    let mut out = Vec::with_capacity(num_samples);
    let mut rng = DeterministicRng::new(seed);
    let mut phase = 0.0_f32;

    // Harmonic weights with formant-like shaping depending on speaker pitch.
    let harmonics: [f32; 6] = if base_f0_hz < 150.0 {
        [1.0, 0.72, 0.48, 0.28, 0.16, 0.09]
    } else if base_f0_hz < 190.0 {
        [0.95, 0.80, 0.52, 0.34, 0.22, 0.12]
    } else {
        [0.85, 0.90, 0.62, 0.42, 0.26, 0.16]
    };
    let norm: f32 = harmonics.iter().sum();

    for i in 0..num_samples {
        let t = i as f32 / sr;
        // Subtle pitch vibrato (±2.5% at 1.4 Hz) to resemble voiced prosody.
        let inst_f0 = base_f0_hz * (1.0 + 0.025 * (TAU * 1.4 * t).sin());
        phase += TAU * inst_f0 / sr;

        let mut voiced = 0.0_f32;
        for (h_idx, &weight) in harmonics.iter().enumerate() {
            let k = (h_idx + 1) as f32;
            voiced += weight * (phase * k).sin();
        }
        voiced /= norm;

        // Syllabic envelope (3.2 Hz cadence with smooth attack/release).
        let syllable = 0.35 + 0.65 * (0.5 + 0.5 * (TAU * 3.2 * t - 0.5).sin()).powf(0.8);
        let fade_in = (i as f32 / 800.0).min(1.0);
        let fade_out = ((num_samples.saturating_sub(i)) as f32 / 800.0).min(1.0);
        let noise = rng.next_signed_f32() * 0.002;
        out.push(voiced * syllable * fade_in * fade_out * amp + noise);
    }

    out
}

/// Simulates loudspeaker-to-microphone acoustic bleed with bulk delay, early
/// room reflection, soft clipping (`tanh`), and 2nd-harmonic distortion.
pub fn apply_room_echo(
    remote: &[f32],
    delay_ms: usize,
    gain: f32,
    second_harmonic_mix: f32,
    clip_drive: f32,
) -> Vec<f32> {
    let delay_samples = delay_ms * SAMPLE_RATE_16K / 1_000;
    let early_reflection_offset = 40; // +2.5 ms early desk reflection
    let mut out = vec![0.0_f32; remote.len()];

    for i in 0..remote.len() {
        let direct = if i >= delay_samples {
            remote[i - delay_samples]
        } else {
            0.0
        };
        let early = if i >= delay_samples + early_reflection_offset {
            remote[i - delay_samples - early_reflection_offset] * 0.14
        } else {
            0.0
        };
        let acoustic = direct + early;
        // Mild loudspeaker soft-clipping + 2nd-harmonic nonlinearity.
        let clipped = (acoustic * clip_drive).tanh() / clip_drive;
        let harmonic2 = second_harmonic_mix * (acoustic * acoustic);
        out[i] = (clipped + harmonic2) * gain;
    }

    out
}

/// Splits full `me` and `others` tracks into streaming `DualStreamChunk16k` blocks.
pub fn chunk_dual_stream(
    me_16k: &[f32],
    others_16k: &[f32],
    output_route: OutputRouteKind,
    chunk_samples: usize,
) -> Vec<DualStreamChunk16k> {
    let total = me_16k.len().max(others_16k.len());
    let mut chunks = Vec::new();
    for start in (0..total).step_by(chunk_samples) {
        let end = (start + chunk_samples).min(total);
        let me_slice = if start < me_16k.len() {
            me_16k[start..end.min(me_16k.len())].to_vec()
        } else {
            vec![0.0; end - start]
        };
        let others_slice = if start < others_16k.len() {
            others_16k[start..end.min(others_16k.len())].to_vec()
        } else {
            vec![0.0; end - start]
        };
        chunks.push(DualStreamChunk16k {
            me_16k: me_slice,
            others_16k: others_slice,
            output_route,
            start_ms: start as u64 * 1_000 / SAMPLE_RATE_16K as u64,
        });
    }
    chunks
}

/// Scenario 1: Open-speaker echo bleed (12 s remote speech, silent local user).
#[derive(Debug, Clone)]
pub struct EchoBleedScenario {
    /// Streaming dual-channel chunks.
    pub chunks: Vec<DualStreamChunk16k>,
    /// Pre-AEC mic bleed signal (`Me` track).
    pub me_bleed_16k: Vec<f32>,
    /// Remote speech signal (`Others` track).
    pub others_16k: Vec<f32>,
    /// Total active remote speech duration in milliseconds.
    pub remote_speech_ms: u64,
}

/// Generates Scenario 1 (`open_speaker_echo_bleed`): 12 s remote speech with
/// 45 ms delayed + mildly clipped + 2nd-harmonic loudspeaker bleed into silent `Me`.
pub fn open_speaker_echo_bleed() -> EchoBleedScenario {
    let duration_ms = 12_000;
    let others_16k = synth_voiced_speech(duration_ms, 140.0, 0.45, 101);
    let mut me_bleed_16k = apply_room_echo(&others_16k, 45, 0.55, 0.35, 1.8);
    let ambient = synth_room_noise(me_bleed_16k.len(), 0.0007, 102);
    for (s, n) in me_bleed_16k.iter_mut().zip(ambient.iter()) {
        *s += *n;
    }
    let chunks = chunk_dual_stream(
        &me_bleed_16k,
        &others_16k,
        OutputRouteKind::Speakers,
        STREAM_CHUNK_SAMPLES,
    );
    EchoBleedScenario {
        chunks,
        me_bleed_16k,
        others_16k,
        remote_speech_ms: duration_ms as u64,
    }
}

/// Scenario 2: Simultaneous remote + near-end double-talk overlap.
#[derive(Debug, Clone)]
pub struct DoubleTalkScenario {
    /// Streaming dual-channel chunks.
    pub chunks: Vec<DualStreamChunk16k>,
    /// Ground-truth clean near-end `Me` speech track (zero during warmup).
    pub clean_me_16k: Vec<f32>,
    /// Sample range `[start, end)` where both speakers overlap.
    pub overlap_sample_range: (usize, usize),
    /// Millisecond range `[start_ms, end_ms)` where both speakers overlap.
    pub overlap_ms_range: (u64, u64),
}

/// Generates Scenario 2 (`double_talk_overlap`): 2 s remote warmup followed by
/// 6 s simultaneous `Others` (`130 Hz`) and higher-energy `Me` (`210 Hz`) with
/// 45 ms nonlinear speaker bleed.
pub fn double_talk_overlap() -> DoubleTalkScenario {
    let warmup_ms = 2_000;
    let overlap_ms = 6_000;
    let total_ms = warmup_ms + overlap_ms;
    let total_samples = total_ms * SAMPLE_RATE_16K / 1_000;
    let overlap_start_sample = warmup_ms * SAMPLE_RATE_16K / 1_000;

    let others_16k = synth_voiced_speech(total_ms, 130.0, 0.34, 201);
    let echo = apply_room_echo(&others_16k, 45, 0.50, 0.30, 1.6);

    let near_end_speech = synth_voiced_speech(overlap_ms, 210.0, 0.62, 202);
    let mut clean_me_16k = vec![0.0_f32; total_samples];
    clean_me_16k[overlap_start_sample..total_samples].copy_from_slice(&near_end_speech);

    let ambient = synth_room_noise(total_samples, 0.0007, 203);
    let mut mixed_me = vec![0.0_f32; total_samples];
    for i in 0..total_samples {
        mixed_me[i] = clean_me_16k[i] + echo[i] + ambient[i];
    }

    let chunks = chunk_dual_stream(
        &mixed_me,
        &others_16k,
        OutputRouteKind::Speakers,
        STREAM_CHUNK_SAMPLES,
    );

    DoubleTalkScenario {
        chunks,
        clean_me_16k,
        overlap_sample_range: (overlap_start_sample, total_samples),
        overlap_ms_range: (warmup_ms as u64, total_ms as u64),
    }
}

/// Scenario 3: 30 s call with a single 3 s utterance and 27 s room-tone silence.
#[derive(Debug, Clone)]
pub struct SilenceHallucinationScenario {
    /// Streaming dual-channel chunks.
    pub chunks: Vec<DualStreamChunk16k>,
    /// Ground-truth speech span `(start_ms, end_ms)` on `Me`.
    pub speech_window_ms: (u64, u64),
}

/// Generates Scenario 3 (`long_silence_hallucination_trap`): 30 s call with one
/// 3 s utterance (`2 s..5 s`) and 27 s of `< 0.002` RMS room-tone silence.
pub fn long_silence_hallucination_trap() -> SilenceHallucinationScenario {
    let total_ms = 30_000;
    let total_samples = total_ms * SAMPLE_RATE_16K / 1_000;
    let speech_start_ms = 2_000;
    let speech_duration_ms = 3_000;

    let mut me_16k = synth_room_noise(total_samples, 0.0005, 301);
    let others_16k = synth_room_noise(total_samples, 0.0004, 302);

    let utterance = synth_voiced_speech(speech_duration_ms, 165.0, 0.48, 303);
    let start_idx = speech_start_ms * SAMPLE_RATE_16K / 1_000;
    for (offset, &s) in utterance.iter().enumerate() {
        me_16k[start_idx + offset] += s;
    }

    let chunks = chunk_dual_stream(
        &me_16k,
        &others_16k,
        OutputRouteKind::Headphones,
        STREAM_CHUNK_SAMPLES,
    );

    SilenceHallucinationScenario {
        chunks,
        speech_window_ms: (
            speech_start_ms as u64,
            (speech_start_ms + speech_duration_ms) as u64,
        ),
    }
}

/// Ground-truth speaker turn for Scenario 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundTruthTurn {
    /// Expected abstract speaker role (`"me"`, `"remote_a"`, or `"remote_b"`).
    pub role: &'static str,
    /// Capture channel on which the turn occurred.
    pub channel: SpeakerChannel,
    /// Turn start time in milliseconds.
    pub start_ms: u64,
    /// Turn end time in milliseconds.
    pub end_ms: u64,
}

/// Scenario 4: Clean `ME_ONLY` enrollment followed by two distinct remote speakers.
#[derive(Debug, Clone)]
pub struct DiarizationScenario {
    /// Streaming dual-channel chunks.
    pub chunks: Vec<DualStreamChunk16k>,
    /// Ground-truth speaker turns.
    pub turns: Vec<GroundTruthTurn>,
}

/// Generates Scenario 4 (`me_only_enrollment_and_two_remote_speakers`): 4 s clean
/// `ME_ONLY` enrollment (`170 Hz`) followed by two distinct remote speakers
/// (`125 Hz` vs `215 Hz`) on `Others` plus repeated turns to test cluster purity.
pub fn me_only_enrollment_and_two_remote_speakers() -> DiarizationScenario {
    let total_ms = 24_000;
    let total_samples = total_ms * SAMPLE_RATE_16K / 1_000;
    let mut me_16k = synth_room_noise(total_samples, 0.0004, 401);
    let mut others_16k = synth_room_noise(total_samples, 0.0004, 402);

    let turn_specs: [(&'static str, SpeakerChannel, usize, usize, f32, u64); 6] = [
        ("me", SpeakerChannel::Me, 0, 4_000, 170.0, 410),
        ("remote_a", SpeakerChannel::Others, 4_800, 8_300, 125.0, 411),
        (
            "remote_b",
            SpeakerChannel::Others,
            9_100,
            12_600,
            215.0,
            412,
        ),
        (
            "remote_a",
            SpeakerChannel::Others,
            13_400,
            16_400,
            125.0,
            413,
        ),
        (
            "remote_b",
            SpeakerChannel::Others,
            17_200,
            20_200,
            215.0,
            414,
        ),
        ("me", SpeakerChannel::Me, 21_000, 23_500, 170.0, 415),
    ];

    let mut turns = Vec::with_capacity(turn_specs.len());
    for (role, channel, start_ms, end_ms, f0, seed) in turn_specs {
        let audio = synth_voiced_speech(end_ms - start_ms, f0, 0.48, seed);
        let start_idx = start_ms * SAMPLE_RATE_16K / 1_000;
        let target = match channel {
            SpeakerChannel::Me => &mut me_16k,
            SpeakerChannel::Others => &mut others_16k,
        };
        for (offset, &s) in audio.iter().enumerate() {
            target[start_idx + offset] += s;
        }
        turns.push(GroundTruthTurn {
            role,
            channel,
            start_ms: start_ms as u64,
            end_ms: end_ms as u64,
        });
    }

    let chunks = chunk_dual_stream(
        &me_16k,
        &others_16k,
        OutputRouteKind::Headphones,
        STREAM_CHUNK_SAMPLES,
    );

    DiarizationScenario { chunks, turns }
}

/// Scenario 5: 65-minute multilingual transcript stream exceeding single-shot limits.
#[derive(Debug, Clone)]
pub struct MultilingualRollupScenario {
    /// Meeting title.
    pub title: String,
    /// User rough notes.
    pub rough_notes: String,
    /// Language instruction for note generation.
    pub language_instruction: String,
    /// Full 65-minute segment stream.
    pub segments: Vec<TranscriptSegment>,
    /// Required multilingual probe substrings expected to survive summarization.
    pub required_markers: Vec<&'static str>,
}

/// Generates Scenario 5 (`long_multilingual_meeting_rollup`): 65-minute simulated
/// meeting (`195` segments) mixing English, Roman Hindi/Urdu, and Arabic/Urdu script.
pub fn long_multilingual_meeting_rollup() -> MultilingualRollupScenario {
    let templates: [(&str, SpeakerChannel, &str, &str); 6] = [
        (
            "me",
            SpeakerChannel::Me,
            "en",
            "Action item: ship the AEC3 adapter and keep end-to-end echo leak under five percent before Friday's milestone review.",
        ),
        (
            "speaker_1",
            SpeakerChannel::Others,
            "hi-Latn",
            "Bhai kal ka demo bilkul ready rakhna hai, pipeline smooth chal rahi hai aur latency target forty milliseconds se kam hai.",
        ),
        (
            "speaker_2",
            SpeakerChannel::Others,
            "ur",
            "اگلا قدم: میٹنگ نوٹس کا معیار بہتر بنانا اور اردو و انگریزی اصطلاحات کو محفوظ رکھنا ہے تاکہ کوئی تفصیل ضائع نہ ہو۔",
        ),
        (
            "me",
            SpeakerChannel::Me,
            "en",
            "Decision: VAD gating must drop silent spans below minus forty-five dBFS so Whisper never hallucinates phantom Thank you lines.",
        ),
        (
            "speaker_1",
            SpeakerChannel::Others,
            "ar",
            "مراجعة خطة الإطلاق الأسبوعية مع التأكد من استقرار تسجيل الصوت المزدوج وعدم تجاوز حد السياق للنموذج المحلي.",
        ),
        (
            "speaker_2",
            SpeakerChannel::Others,
            "en",
            "Follow up on speaker enrollment: clean ME_ONLY windows should anchor the local centroid while separating remote voices.",
        ),
    ];

    let total_segments = 195usize; // 195 * 20s = 3,900s = 65 minutes
    let mut segments = Vec::with_capacity(total_segments);
    for idx in 0..total_segments {
        let (speaker_id, channel, lang, base_text) = templates[idx % templates.len()];
        let start_ms = (idx as u64) * 20_000;
        let end_ms = start_ms + 18_000;
        let display_label = match speaker_id {
            "me" => "Me",
            "speaker_1" => "Speaker 1",
            _ => "Speaker 2",
        };
        segments.push(TranscriptSegment {
            speaker: SpeakerIdentity {
                channel,
                speaker_id: speaker_id.to_owned(),
                display_label: display_label.to_owned(),
                confidence: Some(0.92),
            },
            start_ms,
            end_ms,
            text: format!("[min {:02}] {base_text}", start_ms / 60_000),
            language: Some(lang.to_owned()),
        });
    }

    MultilingualRollupScenario {
        title: "Q4 Audio Modularity & Multilingual Rollout".to_owned(),
        rough_notes: "Check AEC3 ship date, demo readiness, and Urdu/Arabic preservation."
            .to_owned(),
        language_instruction: "Preserve English, Roman Hindi/Urdu, and Arabic/Urdu terms verbatim."
            .to_owned(),
        segments,
        required_markers: vec!["AEC3", "demo", "میٹنگ نوٹس", "مراجعة خطة الإطلاق"],
    }
}

/// Outcome of Scenario 6 (`soft_cancel_undo_state_invariants`).
#[derive(Debug, Clone)]
pub struct SessionBusInvariantResult {
    /// Number of invariant assertions that passed.
    pub passed_checks: usize,
    /// Total number of invariant assertions evaluated.
    pub total_checks: usize,
    /// Human-readable descriptions of any failed checks.
    pub failures: Vec<String>,
}

/// Exercises Scenario 6 (`soft_cancel_undo_state_invariants`) directly against
/// `lowyap_ports::SessionStateMachine`.
pub fn run_soft_cancel_undo_invariants() -> SessionBusInvariantResult {
    let mut passed = 0usize;
    let mut total = 0usize;
    let mut failures = Vec::new();

    let mut check = |name: &str, cond: bool| {
        total += 1;
        if cond {
            passed += 1;
        } else {
            failures.push(name.to_owned());
        }
    };

    let mut sm = SessionStateMachine::new();
    let initial = sm.snapshot(0);
    check("initial_seq_zero", initial.seq == 0);
    check(
        "initial_overlay_hidden",
        initial.overlay == OverlayProjection::Hidden,
    );

    // 1. Escape -> 5s Undo -> resume
    let s1 = sm.start_dictation(1, 1_000, false);
    check(
        "start_dictation_overlay",
        s1.overlay
            == OverlayProjection::DictationRecording {
                take_id: 1,
                locked: false,
                paused: false,
            },
    );

    let cancel1 = sm.soft_cancel_dictation(1, 2_000, DEFAULT_SOFT_CANCEL_GRACE_MS);
    check("soft_cancel_accepted", cancel1.is_some());
    let (epoch1, s2) = cancel1.unwrap();
    check(
        "soft_cancel_deadline_5s",
        s2.dictation
            == DictationPhase::SoftCancelled {
                take_id: 1,
                cancelled_at_ms: 2_000,
                expires_at_ms: 7_000,
                grace_ms: 5_000,
            }
            && s2.overlay
                == OverlayProjection::DictationSoftCancelled {
                    take_id: 1,
                    cancelled_at_ms: 2_000,
                    expires_at_ms: 7_000,
                    grace_ms: 5_000,
                },
    );

    let undo1 = sm.undo_soft_cancel(1, 4_500);
    check(
        "undo_before_deadline_resumes_locked",
        matches!(
            undo1.as_ref().map(|s| &s.dictation),
            Some(DictationPhase::Recording {
                take_id: 1,
                locked: true,
                paused: false,
                ..
            })
        ),
    );

    // 2. Stale timer after Undo must be ignored and must not drop active recording
    let stale_after_undo = sm.expire_or_confirm_cancel(1, Some(epoch1), 7_500);
    check(
        "stale_timer_after_undo_rejected",
        stale_after_undo.is_none(),
    );
    check(
        "recording_still_active_after_stale_timer",
        matches!(
            sm.snapshot(7_500).dictation,
            DictationPhase::Recording { take_id: 1, .. }
        ),
    );

    // Second cancel on the same take produces a strictly higher epoch; old epoch cannot expire it.
    let cancel1_again = sm.soft_cancel_dictation(1, 8_000, DEFAULT_SOFT_CANCEL_GRACE_MS);
    check("second_soft_cancel_accepted", cancel1_again.is_some());
    let (epoch2, _) = cancel1_again.unwrap();
    check("cancel_epoch_strictly_monotonic", epoch2 > epoch1);
    check(
        "stale_epoch_cannot_expire_new_cancel",
        sm.expire_or_confirm_cancel(1, Some(epoch1), 13_500)
            .is_none(),
    );
    check(
        "explicit_confirm_clears_cancel",
        matches!(
            sm.expire_or_confirm_cancel(1, None, 9_000)
                .map(|s| s.dictation),
            Some(DictationPhase::Idle)
        ),
    );

    // 3. Escape -> 0% expiry
    let _ = sm.start_dictation(2, 10_000, false);
    let (epoch3, _) = sm
        .soft_cancel_dictation(2, 11_000, DEFAULT_SOFT_CANCEL_GRACE_MS)
        .expect("soft cancel take 2");
    check(
        "early_timer_before_deadline_rejected",
        sm.expire_or_confirm_cancel(2, Some(epoch3), 15_999)
            .is_none(),
    );
    check(
        "undo_at_exact_deadline_rejected",
        sm.undo_soft_cancel(2, 16_000).is_none(),
    );
    let expired = sm.expire_or_confirm_cancel(2, Some(epoch3), 16_000);
    check(
        "expiry_at_deadline_returns_idle",
        expired.as_ref().is_some_and(|s| {
            s.dictation == DictationPhase::Idle && s.overlay == OverlayProjection::Hidden
        }),
    );

    // 4. Dictation-over-Active-Meeting restoration + elapsed pause accounting
    let meeting = MeetingPhase::Recording {
        meeting_id: "mtg-42".to_owned(),
        title: "Architecture Sync".to_owned(),
        started_at_ms: 20_000,
        accumulated_paused_ms: 0,
        app_name: Some("Zoom".to_owned()),
        capture_mode: AudioCaptureMode::DualStream,
    };
    let s_mtg = sm.sync_meeting(
        meeting,
        Some(DetectedCallBanner {
            call_id: "call-1".to_owned(),
            app_name: "Zoom".to_owned(),
            output_route: OutputRouteKind::Speakers,
        }),
        20_000,
    );
    check(
        "meeting_active_overlay_and_tray",
        matches!(s_mtg.overlay, OverlayProjection::MeetingActive { .. })
            && s_mtg.tray_meeting.active,
    );

    let s_dict_over = sm.start_dictation(3, 22_000, false);
    check(
        "dictation_takes_foreground_over_meeting_while_tray_stays_active",
        matches!(
            s_dict_over.overlay,
            OverlayProjection::DictationRecording { take_id: 3, .. }
        ) && s_dict_over.tray_meeting.active,
    );

    let s_tx = sm.finish_dictation_to_transcribing(3, 24_000);
    check(
        "transcribing_foreground_over_meeting",
        s_tx.as_ref().is_some_and(|s| {
            s.overlay == OverlayProjection::DictationTranscribing { take_id: 3 }
                && s.tray_meeting.active
        }),
    );

    let s_pp = sm.finish_dictation_to_post_processing(3, 25_000);
    check(
        "post_processing_foreground_over_meeting",
        s_pp.as_ref().is_some_and(|s| {
            s.overlay == OverlayProjection::DictationPostProcessing { take_id: 3 }
                && s.tray_meeting.active
        }),
    );

    let s_restored = sm.complete_or_abort_dictation(Some(3), 26_000);
    check(
        "meeting_overlay_restored_after_dictation_with_accurate_elapsed",
        matches!(
            &s_restored.overlay,
            OverlayProjection::MeetingActive {
                meeting_id,
                elapsed_ms,
                paused: false,
                ..
            } if meeting_id == "mtg-42" && *elapsed_ms == 6_000
        ),
    );

    let seq_before_snapshot = s_restored.seq;
    let snap = sm.snapshot(27_000);
    check(
        "snapshot_does_not_advance_seq",
        snap.seq == seq_before_snapshot,
    );

    // Pause freeze check
    let paused_meeting = MeetingPhase::Paused {
        meeting_id: "mtg-42".to_owned(),
        title: "Architecture Sync".to_owned(),
        started_at_ms: 20_000,
        paused_at_ms: 28_000,
        accumulated_paused_ms: 0,
        app_name: Some("Zoom".to_owned()),
        capture_mode: AudioCaptureMode::DualStream,
    };
    let s_paused = sm.sync_meeting(paused_meeting, None, 32_000);
    check(
        "paused_meeting_freezes_elapsed",
        s_paused.tray_meeting.paused && s_paused.tray_meeting.elapsed_ms == 8_000,
    );

    SessionBusInvariantResult {
        passed_checks: passed,
        total_checks: total,
        failures,
    }
}
