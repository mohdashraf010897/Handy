use std::collections::{HashMap, HashSet};
use std::f32::consts::TAU;

use lowyap_ports::{
    DualStreamChunk16k, OutputRouteKind, SpeakerChannel, SpeakerDiarizer, UtteranceSpan,
};

use crate::{
    AcousticMelVoiceEmbedder, DiarizerConfig, HybridSpeakerDiarizer, NameCueResolver,
    VoiceEmbedder, SAMPLE_RATE_16K,
};

const STREAM_CHUNK_SAMPLES: usize = SAMPLE_RATE_16K * 200 / 1_000;
const FRAME_SAMPLES_16K: usize = 320;

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

    fn next_signed_f32(&mut self) -> f32 {
        let bits = (self.next_u64() >> 40) as u32;
        (bits as f32 / ((1_u32 << 24) as f32)) * 2.0 - 1.0
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

fn synth_room_noise(num_samples: usize, target_rms: f32, seed: u64) -> Vec<f32> {
    let mut rng = DeterministicRng::new(seed);
    let scale = target_rms * 3.0_f32.sqrt();
    (0..num_samples)
        .map(|_| rng.next_signed_f32() * scale)
        .collect()
}

/// Exact reproduction of `lowyap_eval::scenarios::synth_voiced_speech`.
fn synth_voiced_speech(duration_ms: usize, base_f0_hz: f32, amp: f32, seed: u64) -> Vec<f32> {
    let num_samples = duration_ms * SAMPLE_RATE_16K / 1_000;
    let sr = SAMPLE_RATE_16K as f32;
    let mut out = Vec::with_capacity(num_samples);
    let mut rng = DeterministicRng::new(seed);
    let mut phase = 0.0_f32;

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
        let inst_f0 = base_f0_hz * (1.0 + 0.025 * (TAU * 1.4 * t).sin());
        phase += TAU * inst_f0 / sr;

        let mut voiced = 0.0_f32;
        for (h_idx, &weight) in harmonics.iter().enumerate() {
            let k = (h_idx + 1) as f32;
            voiced += weight * (phase * k).sin();
        }
        voiced /= norm;

        let syllable = 0.35 + 0.65 * (0.5 + 0.5 * (TAU * 3.2 * t - 0.5).sin()).powf(0.8);
        let fade_in = (i as f32 / 800.0).min(1.0);
        let fade_out = ((num_samples.saturating_sub(i)) as f32 / 800.0).min(1.0);
        let noise = rng.next_signed_f32() * 0.002;
        out.push(voiced * syllable * fade_in * fade_out * amp + noise);
    }

    out
}

/// Synthesizes a multi-harmonic human-like voice at `base_f0_hz` shaped by two
/// Gaussian vocal-tract formant resonances (`f1_hz`, `f2_hz`) and configurable syllable rate.
fn synth_formant_voice(
    duration_ms: usize,
    base_f0_hz: f32,
    formants_hz: [(f32, f32, f32); 3],
    syllable_hz: f32,
    amp: f32,
    seed: u64,
) -> Vec<f32> {
    let num_samples = duration_ms * SAMPLE_RATE_16K / 1_000;
    let sr = SAMPLE_RATE_16K as f32;
    let mut out = Vec::with_capacity(num_samples);
    let mut rng = DeterministicRng::new(seed);
    let mut phase = 0.0_f32;

    let max_harmonic = ((7_200.0 / base_f0_hz).floor() as usize).clamp(6, 36);
    let mut weights = Vec::with_capacity(max_harmonic);
    for h in 1..=max_harmonic {
        let freq = h as f32 * base_f0_hz;
        // Glottal source -6 dB/oct rolloff times vocal-tract formant transfer function.
        let glottal = 1.0 / (1.0 + (freq / 300.0).powi(2)).sqrt();
        let mut tract = 0.08_f32;
        for &(center, bw, gain) in &formants_hz {
            let z = (freq - center) / bw;
            tract += gain * (-0.5 * z * z).exp();
        }
        weights.push(glottal * tract);
    }
    let norm: f32 = weights.iter().sum::<f32>().max(1e-6);

    for i in 0..num_samples {
        let t = i as f32 / sr;
        let inst_f0 = base_f0_hz * (1.0 + 0.02 * (TAU * 1.6 * t).sin());
        phase += TAU * inst_f0 / sr;

        let mut voiced = 0.0_f32;
        for (idx, &w) in weights.iter().enumerate() {
            let k = (idx + 1) as f32;
            voiced += w * (phase * k).sin();
        }
        voiced /= norm;

        let syllable = 0.35 + 0.65 * (0.5 + 0.5 * (TAU * syllable_hz * t).sin()).powf(0.9);
        let fade_in = (i as f32 / 600.0).min(1.0);
        let fade_out = ((num_samples.saturating_sub(i)) as f32 / 600.0).min(1.0);
        let noise = rng.next_signed_f32() * 0.0015;
        out.push(voiced * syllable * fade_in * fade_out * amp + noise);
    }

    out
}

#[derive(Default)]
struct SimpleVadChannel {
    in_speech: bool,
    active_run: usize,
    hangover: usize,
    pending: Vec<f32>,
    pending_start_ms: Option<u64>,
    samples: Vec<f32>,
    start_ms: u64,
    rem: Vec<f32>,
    rem_ms: Option<u64>,
}

/// Exact energy + hangover segmenter mirroring `lowyap_eval::EnergyVadSegmenter::default()`.
fn segment_channel_chunks(channel: SpeakerChannel, chunks: &[(&[f32], u64)]) -> Vec<UtteranceSpan> {
    let threshold_rms = 10.0_f32.powf(-45.0 / 20.0);
    let onset_frames = 3usize;
    let hangover_frames = 20usize;
    let mut st = SimpleVadChannel::default();
    let mut out = Vec::new();

    for &(samples, chunk_start_ms) in chunks {
        if samples.is_empty() {
            continue;
        }
        if st.rem_ms.is_none() {
            st.rem_ms = Some(chunk_start_ms);
        }
        st.rem.extend_from_slice(samples);
        let mut cursor_ms = st.rem_ms.unwrap_or(chunk_start_ms);

        while st.rem.len() >= FRAME_SAMPLES_16K {
            let frame: Vec<f32> = st.rem.drain(..FRAME_SAMPLES_16K).collect();
            let frame_ms = cursor_ms;
            cursor_ms += 20;
            let active = rms(&frame) >= threshold_rms;

            if !st.in_speech {
                if active {
                    if st.pending_start_ms.is_none() {
                        st.pending_start_ms = Some(frame_ms);
                    }
                    st.pending.extend_from_slice(&frame);
                    st.active_run += 1;
                    if st.active_run >= onset_frames {
                        st.in_speech = true;
                        st.start_ms = st.pending_start_ms.unwrap_or(frame_ms);
                        st.samples.append(&mut st.pending);
                        st.pending_start_ms = None;
                        st.hangover = 0;
                    }
                } else {
                    st.pending.clear();
                    st.pending_start_ms = None;
                    st.active_run = 0;
                }
            } else {
                st.samples.extend_from_slice(&frame);
                if active {
                    st.hangover = 0;
                } else {
                    st.hangover += 1;
                }
                if st.hangover >= hangover_frames {
                    let samples_16k = std::mem::take(&mut st.samples);
                    let dur_ms = samples_16k.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;
                    out.push(UtteranceSpan {
                        channel,
                        samples_16k,
                        start_ms: st.start_ms,
                        end_ms: st.start_ms + dur_ms,
                    });
                    st.in_speech = false;
                    st.active_run = 0;
                    st.hangover = 0;
                }
            }
        }
        st.rem_ms = if st.rem.is_empty() {
            None
        } else {
            Some(cursor_ms)
        };
    }

    if st.in_speech && !st.samples.is_empty() {
        let samples_16k = std::mem::take(&mut st.samples);
        let dur_ms = samples_16k.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;
        out.push(UtteranceSpan {
            channel,
            samples_16k,
            start_ms: st.start_ms,
            end_ms: st.start_ms + dur_ms,
        });
    }

    out
}

fn interval_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> u64 {
    let start = a_start.max(b_start);
    let end = a_end.min(b_end);
    end.saturating_sub(start)
}

#[test]
fn trait_bounds_send_and_sync_hold() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>() {}

    assert_send_sync::<AcousticMelVoiceEmbedder>();
    assert_send_sync::<HybridSpeakerDiarizer>();
    assert_send::<Box<dyn SpeakerDiarizer>>();
    assert_send_sync::<Box<dyn VoiceEmbedder>>();
}

#[test]
fn embedder_separates_scenarios_pitches_and_matches_repeated_turns() {
    let mut embedder = AcousticMelVoiceEmbedder::new();
    assert_eq!(embedder.name(), "acoustic_mel_mfcc_f0_v1");
    assert_eq!(embedder.embedding_dim(), 48);

    let me_a = embedder
        .embed_16k(&synth_voiced_speech(3_000, 170.0, 0.48, 410))
        .expect("me turn 1 embedding");
    let me_b = embedder
        .embed_16k(&synth_voiced_speech(2_500, 170.0, 0.22, 415))
        .expect("me turn 2 embedding at different gain");
    let rem_low_a = embedder
        .embed_16k(&synth_voiced_speech(3_500, 125.0, 0.48, 411))
        .expect("125 Hz turn 1");
    let rem_low_b = embedder
        .embed_16k(&synth_voiced_speech(3_000, 125.0, 0.48, 413))
        .expect("125 Hz turn 2");
    let rem_high_a = embedder
        .embed_16k(&synth_voiced_speech(3_500, 215.0, 0.48, 412))
        .expect("215 Hz turn 1");
    let rem_high_b = embedder
        .embed_16k(&synth_voiced_speech(3_000, 215.0, 0.48, 414))
        .expect("215 Hz turn 2");

    // Unit L2-norm check.
    let norm: f32 = me_a.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "expected unit norm, got {norm}");

    // Pitch accuracy check.
    let f0_me = me_a.median_f0_hz.expect("voiced me f0");
    let f0_low = rem_low_a.median_f0_hz.expect("voiced 125 f0");
    let f0_high = rem_high_a.median_f0_hz.expect("voiced 215 f0");
    assert!((f0_me - 170.0).abs() < 5.0, "got me f0={f0_me}");
    assert!((f0_low - 125.0).abs() < 5.0, "got low f0={f0_low}");
    assert!((f0_high - 215.0).abs() < 6.0, "got high f0={f0_high}");

    // Same-speaker turns (> 0.90).
    let sim_me = me_a.cosine_similarity(&me_b.vector);
    let sim_low = rem_low_a.cosine_similarity(&rem_low_b.vector);
    let sim_high = rem_high_a.cosine_similarity(&rem_high_b.vector);
    assert!(
        sim_me > 0.90,
        "expected same-speaker me > 0.90, got {sim_me}"
    );
    assert!(
        sim_low > 0.90,
        "expected same-speaker 125 Hz > 0.90, got {sim_low}"
    );
    assert!(
        sim_high > 0.90,
        "expected same-speaker 215 Hz > 0.90, got {sim_high}"
    );

    // Distinct speakers (< 0.80).
    let sim_125_170 = rem_low_a.cosine_similarity(&me_a.vector);
    let sim_170_215 = me_a.cosine_similarity(&rem_high_a.vector);
    let sim_125_215 = rem_low_a.cosine_similarity(&rem_high_a.vector);
    assert!(
        sim_125_170 < 0.80,
        "expected 125 vs 170 Hz < 0.80, got {sim_125_170}"
    );
    assert!(
        sim_170_215 < 0.80,
        "expected 170 vs 215 Hz < 0.80, got {sim_170_215}"
    );
    assert!(
        sim_125_215 < 0.80,
        "expected 125 vs 215 Hz < 0.80, got {sim_125_215}"
    );
}

#[test]
fn scenario_4_exact_reproduction_achieves_perfect_recall_purity_and_f1() {
    let total_ms = 24_000usize;
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

    for &(_, channel, start_ms, end_ms, f0, seed) in &turn_specs {
        let audio = synth_voiced_speech(end_ms - start_ms, f0, 0.48, seed);
        let start_idx = start_ms * SAMPLE_RATE_16K / 1_000;
        let target = match channel {
            SpeakerChannel::Me => &mut me_16k,
            SpeakerChannel::Others => &mut others_16k,
        };
        for (offset, &s) in audio.iter().enumerate() {
            target[start_idx + offset] += s;
        }
    }

    let mut chunks = Vec::new();
    for start in (0..total_samples).step_by(STREAM_CHUNK_SAMPLES) {
        let end = (start + STREAM_CHUNK_SAMPLES).min(total_samples);
        chunks.push(DualStreamChunk16k {
            me_16k: me_16k[start..end].to_vec(),
            others_16k: others_16k[start..end].to_vec(),
            output_route: OutputRouteKind::Headphones,
            start_ms: start as u64 * 1_000 / SAMPLE_RATE_16K as u64,
        });
    }

    let mut diarizer = HybridSpeakerDiarizer::default();
    for chunk in &chunks {
        if rms(&chunk.me_16k) >= 0.02 && rms(&chunk.others_16k) < 0.005 {
            diarizer.observe_me_clean(&chunk.me_16k);
        }
    }
    assert!(diarizer.is_me_enrolled());
    assert!(diarizer.me_observation_count() >= 15);

    let me_chunk_refs: Vec<(&[f32], u64)> = chunks
        .iter()
        .map(|c| (c.me_16k.as_slice(), c.start_ms))
        .collect();
    let others_chunk_refs: Vec<(&[f32], u64)> = chunks
        .iter()
        .map(|c| (c.others_16k.as_slice(), c.start_ms))
        .collect();

    let mut spans = segment_channel_chunks(SpeakerChannel::Me, &me_chunk_refs);
    spans.extend(segment_channel_chunks(
        SpeakerChannel::Others,
        &others_chunk_refs,
    ));
    spans.sort_by_key(|s| s.start_ms);

    let attributed: Vec<(&UtteranceSpan, String)> = spans
        .iter()
        .filter(|s| rms(&s.samples_16k) >= 0.01)
        .map(|span| {
            let id = diarizer.identify(span);
            (span, id.speaker_id)
        })
        .collect();

    let mut turn_predictions: Vec<(&'static str, String)> = Vec::new();
    let mut me_correct = 0usize;
    let mut me_total = 0usize;
    let mut remote_votes: HashMap<String, HashMap<&'static str, usize>> = HashMap::new();
    let mut remote_total = 0usize;

    for &(role, channel, start_ms, end_ms, _, _) in &turn_specs {
        let best_pred = attributed
            .iter()
            .filter(|(span, _)| span.channel == channel)
            .map(|(span, spk_id)| {
                (
                    interval_overlap(span.start_ms, span.end_ms, start_ms as u64, end_ms as u64),
                    spk_id.clone(),
                )
            })
            .filter(|(ov, _)| *ov > 0)
            .max_by_key(|(ov, _)| *ov)
            .map(|(_, spk_id)| spk_id)
            .unwrap_or_else(|| "<missed>".to_owned());

        if role == "me" {
            me_total += 1;
            if best_pred == "me" {
                me_correct += 1;
            }
        } else {
            remote_total += 1;
            *remote_votes
                .entry(best_pred.clone())
                .or_default()
                .entry(role)
                .or_insert(0) += 1;
        }
        turn_predictions.push((role, best_pred));
    }

    assert_eq!(me_total, 2);
    assert_eq!(me_correct, 2, "expected 2/2 Me recall");

    let pure_hits: usize = remote_votes
        .values()
        .map(|m| m.values().copied().max().unwrap_or(0))
        .sum();
    assert_eq!(remote_total, 4);
    assert_eq!(pure_hits, 4, "expected 4/4 remote cluster purity");
    assert_eq!(diarizer.remote_speaker_count(), 2);

    let distinct_remote: HashSet<&str> = remote_votes.keys().map(String::as_str).collect();
    assert_eq!(distinct_remote, HashSet::from(["speaker_1", "speaker_2"]));

    // Exact macro-F1 across {"me", "remote_a" -> "speaker_1", "remote_b" -> "speaker_2"}.
    let mapping = HashMap::from([
        ("me", "me"),
        ("remote_a", "speaker_1"),
        ("remote_b", "speaker_2"),
    ]);
    let mut f1_sum = 0.0_f32;
    for role in ["me", "remote_a", "remote_b"] {
        let expected_id = mapping[role];
        let mut tp = 0usize;
        let mut fp = 0usize;
        let mut fn_ = 0usize;
        for (gt_role, pred_id) in &turn_predictions {
            match (*gt_role == role, pred_id == expected_id) {
                (true, true) => tp += 1,
                (false, true) => fp += 1,
                (true, false) => fn_ += 1,
                (false, false) => {}
            }
        }
        f1_sum += (2 * tp) as f32 / (2 * tp + fp + fn_) as f32;
    }
    let f1 = f1_sum / 3.0;
    assert!((f1 - 1.0).abs() < 1e-6, "expected F1 == 1.0, got {f1}");
}

#[test]
fn same_f0_150hz_with_distinct_formant_envelopes_separates_cleanly() {
    // Both speakers share identical F0 = 150 Hz!
    // Speaker Warm: chest/back-vowel formant profile (F1=320 Hz, F2=880 Hz, F3=2100 Hz).
    let warm_formants = [
        (320.0, 90.0, 1.4),
        (880.0, 140.0, 0.9),
        (2_100.0, 220.0, 0.15),
    ];
    // Speaker Bright: forward/palatal formant profile (F1=680 Hz, F2=2250 Hz, F3=3400 Hz).
    let bright_formants = [
        (680.0, 120.0, 0.45),
        (2_250.0, 240.0, 1.35),
        (3_400.0, 320.0, 0.95),
    ];

    let warm_turn1 = synth_formant_voice(2_200, 150.0, warm_formants, 3.0, 0.45, 701);
    let warm_turn2 = synth_formant_voice(1_800, 150.0, warm_formants, 4.3, 0.30, 702);
    let bright_turn1 = synth_formant_voice(2_200, 150.0, bright_formants, 3.4, 0.45, 703);
    let bright_turn2 = synth_formant_voice(1_900, 150.0, bright_formants, 2.6, 0.35, 704);

    let mut embedder = AcousticMelVoiceEmbedder::new();
    let e_w1 = embedder.embed_16k(&warm_turn1).expect("warm 1");
    let e_w2 = embedder.embed_16k(&warm_turn2).expect("warm 2");
    let e_b1 = embedder.embed_16k(&bright_turn1).expect("bright 1");
    let e_b2 = embedder.embed_16k(&bright_turn2).expect("bright 2");

    // Verify both have F0 ~ 150 Hz.
    assert!((e_w1.median_f0_hz.unwrap() - 150.0).abs() < 4.0);
    assert!((e_b1.median_f0_hz.unwrap() - 150.0).abs() < 4.0);

    let same_warm = e_w1.cosine_similarity(&e_w2.vector);
    let same_bright = e_b1.cosine_similarity(&e_b2.vector);
    let cross = e_w1.cosine_similarity(&e_b1.vector);
    assert!(same_warm > 0.90, "same warm speaker sim={same_warm}");
    assert!(same_bright > 0.90, "same bright speaker sim={same_bright}");
    assert!(
        cross < 0.80,
        "distinct formant envelopes at identical 150 Hz must have cos < 0.80, got {cross}"
    );

    let mut diarizer = HybridSpeakerDiarizer::default();
    let id1 = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: warm_turn1,
        start_ms: 0,
        end_ms: 2_200,
    });
    let id2 = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: bright_turn1,
        start_ms: 2_500,
        end_ms: 4_700,
    });
    let id3 = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: warm_turn2,
        start_ms: 5_000,
        end_ms: 6_800,
    });
    let id4 = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: bright_turn2,
        start_ms: 7_000,
        end_ms: 8_900,
    });

    assert_eq!(id1.speaker_id, "speaker_1");
    assert_eq!(id2.speaker_id, "speaker_2");
    assert_eq!(id3.speaker_id, "speaker_1");
    assert_eq!(id4.speaker_id, "speaker_2");
    assert_eq!(diarizer.remote_speaker_count(), 2);
}

#[test]
fn short_30ms_click_burst_on_others_does_not_spawn_phantom_speaker() {
    let mut diarizer = HybridSpeakerDiarizer::default();

    let spk1 = synth_voiced_speech(2_000, 125.0, 0.45, 801);
    let spk2 = synth_voiced_speech(2_000, 215.0, 0.45, 802);

    let a1 = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: spk1,
        start_ms: 0,
        end_ms: 2_000,
    });
    let a2 = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: spk2,
        start_ms: 2_500,
        end_ms: 4_500,
    });
    assert_eq!(a1.speaker_id, "speaker_1");
    assert_eq!(a2.speaker_id, "speaker_2");
    assert_eq!(diarizer.remote_speaker_count(), 2);

    // 30 ms = 480 samples loud click/noise burst on Others.
    let click_30ms = synth_room_noise(480, 0.25, 803);
    let click_id = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: click_30ms,
        start_ms: 4_800,
        end_ms: 4_830,
    });

    assert_eq!(
        diarizer.remote_speaker_count(),
        2,
        "30 ms click must NOT spawn Speaker 3"
    );
    assert_eq!(click_id.speaker_id, "speaker_2");
    assert_eq!(click_id.confidence, Some(0.40));

    // Also verify an 80 ms unvoiced noise burst (active_frames >= 4, voiced_frames == 0 < 12) does not spawn Speaker 3.
    let burst_80ms = synth_room_noise(1_280, 0.12, 804);
    let _ = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: burst_80ms,
        start_ms: 5_000,
        end_ms: 5_080,
    });
    assert_eq!(diarizer.remote_speaker_count(), 2);
}

#[test]
fn max_remote_speakers_cap_is_strictly_respected() {
    let config = DiarizerConfig {
        max_remote_speakers: 2,
        ..DiarizerConfig::default()
    };
    let mut diarizer = HybridSpeakerDiarizer::new(config);

    for (idx, f0) in [105.0_f32, 165.0, 235.0, 310.0].into_iter().enumerate() {
        let audio = synth_voiced_speech(2_000, f0, 0.45, 900 + idx as u64);
        let id = diarizer.identify(&UtteranceSpan {
            channel: SpeakerChannel::Others,
            samples_16k: audio,
            start_ms: (idx as u64) * 2_500,
            end_ms: (idx as u64) * 2_500 + 2_000,
        });
        assert!(
            matches!(id.speaker_id.as_str(), "speaker_1" | "speaker_2"),
            "unexpected speaker_id beyond cap: {}",
            id.speaker_id
        );
        assert!(diarizer.remote_speaker_count() <= 2);
    }
    assert_eq!(diarizer.remote_speaker_count(), 2);
}

#[test]
fn single_mic_fallback_separates_in_room_speaker_and_reset_clears_state() {
    let config = DiarizerConfig {
        separate_single_mic_speakers: true,
        ..DiarizerConfig::default()
    };
    let mut diarizer = HybridSpeakerDiarizer::new(config);

    // Pre-enrollment Me turn gets confidence 0.85 and "me".
    let pre = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Me,
        samples_16k: synth_voiced_speech(1_500, 175.0, 0.45, 1001),
        start_ms: 0,
        end_ms: 1_500,
    });
    assert_eq!(pre.speaker_id, "me");
    assert_eq!(pre.confidence, Some(0.85));

    // Enroll Me (175 Hz).
    diarizer.observe_me_clean(&synth_voiced_speech(1_500, 175.0, 0.45, 1002));
    assert!(diarizer.is_me_enrolled());

    // Divergent voice on Me channel (110 Hz in-room colleague) -> speaker_1 on Me channel!
    let room_turn = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Me,
        samples_16k: synth_voiced_speech(2_000, 110.0, 0.45, 1003),
        start_ms: 2_000,
        end_ms: 4_000,
    });
    assert_eq!(room_turn.channel, SpeakerChannel::Me);
    assert_eq!(room_turn.speaker_id, "speaker_1");
    assert_eq!(room_turn.display_label, "Speaker 1");

    // Subsequent Me voice -> still "me"!
    let me_again = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Me,
        samples_16k: synth_voiced_speech(1_800, 175.0, 0.45, 1004),
        start_ms: 4_500,
        end_ms: 6_300,
    });
    assert_eq!(me_again.speaker_id, "me");

    diarizer.observe_transcript_turn(
        SpeakerChannel::Me,
        "speaker_1",
        2_000,
        4_000,
        "Hi, I'm Rahul",
    );
    assert_eq!(
        diarizer
            .name_cues()
            .display_label_for("speaker_1", "Speaker 1"),
        "Speaker 1 (Rahul)"
    );

    // Subsequent turn by that in-room speaker directly resolves to "Speaker 1 (Rahul)".
    let room_turn_2 = diarizer.identify(&UtteranceSpan {
        channel: SpeakerChannel::Me,
        samples_16k: synth_voiced_speech(1_800, 110.0, 0.42, 1005),
        start_ms: 6_500,
        end_ms: 8_300,
    });
    assert_eq!(room_turn_2.speaker_id, "speaker_1");
    assert_eq!(room_turn_2.display_label, "Speaker 1 (Rahul)");

    diarizer.reset();
    assert!(!diarizer.is_me_enrolled());
    assert_eq!(diarizer.me_observation_count(), 0);
    assert_eq!(diarizer.remote_speaker_count(), 0);
    assert_eq!(
        diarizer
            .name_cues()
            .display_label_for("speaker_1", "Speaker 1"),
        "Speaker 1"
    );
}

#[test]
fn name_cue_resolver_binds_valid_intros_and_vocatives_and_rejects_noise() {
    let mut resolver = NameCueResolver::new();

    // 1. Self-intro: "Hi, I'm Rahul"
    let bound = resolver.observe_transcript_turn(
        SpeakerChannel::Others,
        "speaker_1",
        1_000,
        3_000,
        "Hi, I'm Rahul",
    );
    assert_eq!(bound.as_deref(), Some("Rahul"));
    assert_eq!(
        resolver.display_label_for("speaker_1", "Speaker 1"),
        "Speaker 1 (Rahul)"
    );
    assert_eq!(resolver.display_label_for("me", "Me"), "Me");

    // 2. Vocative handoff from Me -> next Others turn within <= 8_000 ms:
    assert_eq!(
        resolver.observe_transcript_turn(
            SpeakerChannel::Me,
            "me",
            4_000,
            6_000,
            "Hey Priya, can you review the migration plan?",
        ),
        None
    );
    let bound_priya = resolver.observe_transcript_turn(
        SpeakerChannel::Others,
        "speaker_2",
        7_500,
        10_000,
        "Sure, looking at the schema right now.",
    );
    assert_eq!(bound_priya.as_deref(), Some("Priya"));
    assert_eq!(
        resolver.display_label_for("speaker_2", "Speaker 2"),
        "Speaker 2 (Priya)"
    );

    // 3. Roman Hindi/Urdu self-intro & additional intro forms:
    assert_eq!(
        resolver
            .observe_transcript_turn(
                SpeakerChannel::Others,
                "speaker_3",
                11_000,
                13_000,
                "Hello, mera naam Kabir hai.",
            )
            .as_deref(),
        Some("Kabir")
    );
    assert_eq!(
        resolver
            .observe_transcript_turn(
                SpeakerChannel::Others,
                "speaker_4",
                14_000,
                16_000,
                "This is Ananya from security.",
            )
            .as_deref(),
        Some("Ananya")
    );

    // 4. Rejections: "Hi, I'm ready", "Hey everyone, let's start", "Hey team, what do you think?"
    let mut clean = NameCueResolver::new();
    assert_eq!(
        clean.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            0,
            1_500,
            "Hi, I'm ready",
        ),
        None
    );
    assert_eq!(
        clean.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            1_600,
            3_000,
            "Hi, I'm Ready",
        ),
        None
    );

    // "Hey everyone, let's start" followed immediately by Others turn -> must NOT bind!
    let _ = clean.observe_transcript_turn(
        SpeakerChannel::Me,
        "me",
        4_000,
        5_500,
        "Hey everyone, let's start",
    );
    assert_eq!(
        clean.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            6_000,
            7_500,
            "Sounds good.",
        ),
        None
    );

    // "Hey team, what do you think?" (and capitalized "Hey Team, what do you think?") -> must NOT bind!
    let _ = clean.observe_transcript_turn(
        SpeakerChannel::Me,
        "me",
        8_000,
        9_500,
        "Hey team, what do you think?",
    );
    assert_eq!(
        clean.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            10_000,
            11_500,
            "Let's ship it.",
        ),
        None
    );
    let _ = clean.observe_transcript_turn(
        SpeakerChannel::Me,
        "me",
        12_000,
        13_500,
        "Hey Team, what do you think?",
    );
    assert_eq!(
        clean.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            14_000,
            15_500,
            "Agreed.",
        ),
        None
    );

    // Expired vocative (> 8_000 ms gap) -> must NOT bind!
    let _ = clean.observe_transcript_turn(
        SpeakerChannel::Me,
        "me",
        20_000,
        22_000,
        "Vikram, can you take this?",
    );
    assert_eq!(
        clean.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            30_001,
            32_000,
            "Sorry I was on mute.",
        ),
        None
    );

    // Exhaustive blocklist check across all mandated categories.
    for blocked in [
        "Everyone",
        "Team",
        "Guys",
        "Folks",
        "All",
        "There",
        "Here",
        "Sure",
        "Yeah",
        "Yes",
        "Well",
        "Okay",
        "Right",
        "Thanks",
        "Sorry",
        "Ready",
        "Going",
        "Doing",
        "Trying",
        "Looking",
        "Working",
        "Speaking",
        "Calling",
        "Bhai",
        "Yaar",
        "Bro",
        "Sir",
        "Madam",
        "Boss",
        "Man",
        "Buddy",
        "Mate",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
        "Today",
        "Tomorrow",
        "Google",
        "Zoom",
        "Slack",
        "Teams",
    ] {
        assert_eq!(
            clean.observe_transcript_turn(
                SpeakerChannel::Others,
                "speaker_9",
                0,
                1_000,
                &format!("Hi, I'm {blocked}"),
            ),
            None,
            "blocklisted token {blocked} must be rejected"
        );
    }

    // Existing binding on speaker_1 ("Rahul") must never be overwritten by a subsequent vocative.
    let _ = resolver.observe_transcript_turn(
        SpeakerChannel::Me,
        "me",
        20_000,
        21_000,
        "Hey Vikram, can you confirm?",
    );
    assert_eq!(
        resolver.observe_transcript_turn(
            SpeakerChannel::Others,
            "speaker_1",
            21_500,
            23_000,
            "Actually Vikram stepped away.",
        ),
        None
    );
    assert_eq!(
        resolver.display_label_for("speaker_1", "Speaker 1"),
        "Speaker 1 (Rahul)"
    );
}

#[test]
fn nearby_pitch_distinct_vocal_tracts_split_while_multi_syllable_phrases_match() {
    let mut embedder = AcousticMelVoiceEmbedder::new();

    // Speaker A (152 Hz, longer vocal tract) speaking two different multi-syllable phrases.
    let mut phrase_a1 = synth_formant_voice(
        900,
        152.0,
        [
            (310.0, 85.0, 1.3),
            (920.0, 140.0, 1.0),
            (2_150.0, 220.0, 0.25),
        ],
        3.1,
        0.44,
        1101,
    );
    phrase_a1.extend_from_slice(&synth_formant_voice(
        900,
        154.0,
        [
            (360.0, 95.0, 1.2),
            (1_020.0, 150.0, 0.9),
            (2_200.0, 230.0, 0.30),
        ],
        4.2,
        0.40,
        1102,
    ));

    let mut phrase_a2 = synth_formant_voice(
        850,
        153.0,
        [
            (340.0, 90.0, 1.25),
            (970.0, 145.0, 0.95),
            (2_180.0, 225.0, 0.28),
        ],
        2.7,
        0.26,
        1103,
    );
    phrase_a2.extend_from_slice(&synth_formant_voice(
        950,
        151.0,
        [
            (320.0, 88.0, 1.3),
            (940.0, 140.0, 1.0),
            (2_160.0, 220.0, 0.26),
        ],
        3.8,
        0.34,
        1104,
    ));

    // Speaker B at nearby pitch (162 Hz, only ~6.5% higher F0, shorter brighter vocal tract).
    let phrase_b = synth_formant_voice(
        1_800,
        162.0,
        [
            (640.0, 115.0, 0.5),
            (2_180.0, 230.0, 1.3),
            (3_350.0, 300.0, 0.9),
        ],
        3.5,
        0.42,
        1105,
    );

    let ea1 = embedder.embed_16k(&phrase_a1).expect("a1");
    let ea2 = embedder.embed_16k(&phrase_a2).expect("a2");
    let eb = embedder.embed_16k(&phrase_b).expect("b");

    let within_a = ea1.cosine_similarity(&ea2.vector);
    let across_ab = ea1.cosine_similarity(&eb.vector);
    assert!(
        within_a > 0.90,
        "expected multi-syllable phrases of same speaker > 0.90, got {within_a}"
    );
    assert!(
        across_ab < 0.80,
        "expected nearby-pitch distinct vocal tract < 0.80, got {across_ab}"
    );
}
