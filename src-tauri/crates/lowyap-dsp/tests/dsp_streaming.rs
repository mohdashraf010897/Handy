use std::f32::consts::TAU;

use lowyap_dsp::{
    EarshotSpeechScorer, FrameSpeechScorer, VadSegmenterConfig, VadUtteranceSegmenter,
    WebRtcAec3EchoCanceller,
};
use lowyap_ports::{EchoCanceller, OutputRouteKind, SpeakerChannel, UtteranceSegmenter};

const ME: SpeakerChannel = SpeakerChannel::Me;
const OTHERS: SpeakerChannel = SpeakerChannel::Others;
const SPEAKERS: OutputRouteKind = OutputRouteKind::Speakers;
const HEADPHONES: OutputRouteKind = OutputRouteKind::Headphones;

fn voice(n: usize, pitch: f32, amplitude: f32) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            let phase = TAU * pitch * t + 0.6 * (TAU * 1.3 * t).sin();
            let envelope = (0.65 + 0.35 * (TAU * 3.1 * t).sin()) * (i as f32 / 480.0).min(1.0);
            amplitude * envelope * (phase.sin() + 0.5 * (2.0 * phase).sin()) / 1.5
        })
        .collect()
}

fn rms(pcm: &[f32]) -> f64 {
    (pcm.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt()
}

fn room_pair() -> (Vec<f32>, Vec<f32>) {
    let remote = voice(48_000, 157.0, 0.40);
    let echo = (0..remote.len())
        .map(|i: usize| {
            let at = |lag| i.checked_sub(lag).map_or(0.0, |j| remote[j]);
            // Fractional path and nonlinear reflection, independent of eval fixtures.
            let direct = 0.65 * at(515) + 0.35 * at(516);
            let acoustic = direct + 0.11 * at(549);
            (1.8 * acoustic).tanh() * 0.34 + 0.16 * acoustic.powi(2)
        })
        .collect();
    (remote, echo)
}

fn aec_stream(
    aec: &mut WebRtcAec3EchoCanceller,
    me: &[f32],
    remote: &[f32],
    size: usize,
    route: OutputRouteKind,
) -> Vec<f32> {
    me.chunks(size)
        .zip(remote.chunks(size))
        .flat_map(|(capture, render)| aec.process_16k(capture, render, route))
        .collect()
}

#[test]
fn aec_returns_exact_length_for_mixed_chunk_sizes_and_short_render() {
    let mut aec = WebRtcAec3EchoCanceller::new();
    for len in [0, 1, 25, 159, 160, 161, 256, 320, 3_200, 3_203] {
        let output = aec.process_16k(&vec![0.4; len], &[0.2; 3], SPEAKERS);
        assert_eq!(output.len(), len);
    }
}

#[test]
fn headphones_preserve_all_normalized_samples_even_at_odd_boundaries() {
    let me = voice(7_999, 185.0, 0.7);
    let remote = voice(7_999, 135.0, 0.75);
    let out = aec_stream(
        &mut WebRtcAec3EchoCanceller::new(),
        &me,
        &remote,
        257,
        HEADPHONES,
    );
    assert_eq!(out, me);
}

#[test]
fn headphones_sanitize_and_clamp_before_passthrough() {
    let out = WebRtcAec3EchoCanceller::new().process_16k(
        &[f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -2.0, 3.0],
        &[f32::NAN; 8],
        HEADPHONES,
    );
    assert_eq!(out, [0.0, 0.0, 0.0, -1.0, 1.0]);
}

#[test]
fn invalid_render_and_capture_never_poison_following_frames() {
    let mut aec = WebRtcAec3EchoCanceller::new();
    let out = aec.process_16k(&[f32::INFINITY; 161], &[f32::NAN; 161], SPEAKERS);
    assert_eq!(out, vec![0.0; 161]);
    let out = aec.process_16k(&[0.2; 161], &[f32::NEG_INFINITY; 161], SPEAKERS);
    assert_eq!(out, vec![0.2; 161]);
}

#[test]
fn absent_reference_keeps_near_end_samples_on_the_capture_clock() {
    let me = voice(797, 191.0, 0.7);
    let out = WebRtcAec3EchoCanceller::new().process_16k(&me, &[], OutputRouteKind::Unknown);
    assert_eq!(out, me);
}

#[test]
fn speakers_cancel_fractional_nonlinear_bleed_with_odd_stream_chunks() {
    let (remote, echo) = room_pair();
    let out = aec_stream(
        &mut WebRtcAec3EchoCanceller::new(),
        &echo,
        &remote,
        257,
        SPEAKERS,
    );
    let leak = rms(&out) / rms(&echo);
    assert!(
        leak <= 0.02,
        "acoustic leak {leak:.3}; {}",
        leak_windows(&out)
    );
}

#[test]
fn unknown_route_cancels_when_reference_is_active() {
    let (remote, echo) = room_pair();
    let out = aec_stream(
        &mut WebRtcAec3EchoCanceller::new(),
        &echo,
        &remote,
        320,
        OutputRouteKind::Unknown,
    );
    let leak = rms(&out) / rms(&echo);
    assert!(
        leak <= 0.02,
        "acoustic leak {leak:.3}; {}",
        leak_windows(&out)
    );
}

fn leak_windows(pcm: &[f32]) -> String {
    let mut windows: Vec<_> = pcm.chunks(160).map(rms).enumerate().collect();
    windows.sort_by(|a, b| b.1.total_cmp(&a.1));
    format!("worst 10 ms windows: {:?}", &windows[..8])
}

#[test]
fn double_talk_preserves_the_near_end_waveform() {
    let (remote, echo) = room_pair();
    let start = 16_000;
    let near = voice(echo.len() - start, 218.0, 0.60);
    let mut mic = echo;
    for (sample, speech) in mic[start..].iter_mut().zip(&near) {
        *sample += speech;
    }
    let out = aec_stream(
        &mut WebRtcAec3EchoCanceller::new(),
        &mic,
        &remote,
        256,
        SPEAKERS,
    );
    let projection: f32 = out[start..].iter().zip(&near).map(|(x, y)| x * y).sum();
    let energy: f32 = near.iter().map(|x| x * x).sum();
    assert!(
        projection / energy >= 0.88,
        "survival {}",
        projection / energy
    );
}

#[test]
fn aec_reset_discards_partial_frames_and_learned_path() {
    let (remote, echo) = room_pair();
    let mut aec = WebRtcAec3EchoCanceller::new();
    aec.process_16k(&echo[..3_201], &remote[..3_201], SPEAKERS);
    aec.reset();
    let after_reset = aec_stream(&mut aec, &echo[..4_001], &remote[..4_001], 257, SPEAKERS);
    let fresh = aec_stream(
        &mut WebRtcAec3EchoCanceller::new(),
        &echo[..4_001],
        &remote[..4_001],
        257,
        SPEAKERS,
    );
    assert_eq!(after_reset, fresh);
}

#[test]
fn headphones_after_speakers_do_not_inherit_attenuation() {
    let (remote, echo) = room_pair();
    let mut aec = WebRtcAec3EchoCanceller::new();
    aec.process_16k(&echo[..3_203], &remote[..3_203], SPEAKERS);
    let mic = &echo[3_203..3_706];
    assert_eq!(aec.process_16k(mic, mic, HEADPHONES), mic);
}

#[test]
fn earshot_scores_voiced_frames_and_rejects_dc_and_invalid_pcm() {
    let mut scorer = EarshotSpeechScorer::new();
    let audio = voice(16_384, 135.0, 0.5);
    let voiced = audio
        .as_chunks::<256>()
        .0
        .iter()
        .filter(|chunk| scorer.score_frame(*chunk) > 0.5)
        .count();
    assert!(voiced >= 58, "only {voiced}/64 voiced");
    for dc in [0.0, 0.45, 1.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(scorer.score_frame(&[dc; 256]), 0.0);
    }
}

#[test]
fn earshot_rejects_single_impulses_and_recovers_after_reset() {
    let mut scorer = EarshotSpeechScorer::new();
    let mut impulse = [0.0; 256];
    impulse[128] = 4.0;
    assert_eq!(scorer.score_frame(&impulse), 0.0);
    let speech = voice(1_024, 190.0, 0.6);
    for frame in speech.as_chunks::<256>().0 {
        scorer.score_frame(frame);
    }
    scorer.reset();
    assert_eq!(
        scorer.score_frame(&speech[..256]),
        EarshotSpeechScorer::new().score_frame(&speech[..256])
    );
}

// Use a deterministic scorer to isolate segmentation from model decisions.
struct SignScorer;

impl FrameSpeechScorer for SignScorer {
    fn name(&self) -> &'static str {
        "sign"
    }

    fn frame_samples(&self) -> usize {
        256
    }

    fn score_frame(&mut self, pcm: &[f32]) -> f32 {
        if pcm[0] > 0.1 {
            1.0
        } else {
            0.0
        }
    }

    fn reset(&mut self) {}
}

fn fixture() -> VadUtteranceSegmenter<SignScorer> {
    VadUtteranceSegmenter::with_scorers(VadSegmenterConfig::default(), SignScorer, SignScorer)
}

fn scored_voice(ms: usize) -> Vec<f32> {
    [1.25, -1.25].repeat(ms * 8)
}

#[test]
fn vad_keeps_raw_preroll_and_uses_actual_sample_times() {
    let mut segmenter = fixture();
    let mut pcm = vec![-0.03; 8_192];
    pcm.extend(scored_voice(320));
    for (i, chunk) in pcm.chunks(257).enumerate() {
        assert!(segmenter
            .push_chunk(ME, chunk, 700 + (i * 257 / 16) as u64)
            .is_empty());
    }
    let spans = segmenter.flush();
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].start_ms, spans[0].end_ms), (892, 1_532));
    assert_eq!(spans[0].samples_16k, pcm[8_192 - 5_120..]);
}

#[test]
fn vad_rejects_bursts_below_minimum_voiced_time() {
    let mut segmenter = fixture();
    assert!(segmenter.push_chunk(ME, &scored_voice(224), 0).is_empty());
    assert!(segmenter.flush().is_empty());
}

#[test]
fn vad_needs_consecutive_onset_not_just_total_positive_frames() {
    let mut segmenter = fixture();
    let mut block = scored_voice(48);
    block.extend([0.0; 256]);
    assert!(segmenter.push_chunk(ME, &block.repeat(10), 0).is_empty());
    assert!(segmenter.flush().is_empty());
}

#[test]
fn vad_energy_gate_rejects_dc_even_when_scorer_calls_it_speech() {
    let mut segmenter = fixture();
    assert!(segmenter.push_chunk(ME, &[0.4; 4_096], 0).is_empty());
    assert!(segmenter.flush().is_empty());
}

#[test]
fn vad_splits_natural_pauses_but_not_short_pauses() {
    let mut segmenter = fixture();
    let mut pcm = scored_voice(320);
    pcm.extend(vec![0.0; 160 * 16]);
    pcm.extend(scored_voice(320));
    pcm.extend(vec![0.0; 480 * 16]);
    pcm.extend(scored_voice(320));
    let mut spans = segmenter.push_chunk(ME, &pcm, 0);
    spans.extend(segmenter.flush());
    let times: Vec<_> = spans
        .iter()
        .map(|span| (span.start_ms, span.end_ms))
        .collect();
    assert_eq!(times, [(0, 1_280), (1_280, 1_600)]);
}

#[test]
fn vad_soft_splits_at_brief_pause_after_ten_seconds() {
    let mut segmenter = fixture();
    let mut pcm = scored_voice(10_000);
    pcm.extend([0.0; 64 * 16]);
    pcm.extend(scored_voice(320));
    let spans = segmenter.push_chunk(ME, &pcm, 0);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].end_ms, 10_064);
    assert_eq!(segmenter.flush()[0].start_ms, 10_064);
}

#[test]
fn vad_twenty_second_cap_splits_continuous_speech_without_sample_loss() {
    let mut segmenter = fixture();
    let pcm = scored_voice(20_512);
    let first = segmenter.push_chunk(ME, &pcm, 30);
    let second = segmenter.flush();
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].end_ms, 20_030);
    assert_eq!(second[0].start_ms, 20_030);
    assert_eq!(second[0].end_ms, 20_542);
    assert_eq!(first[0].samples_16k, pcm[..320_000]);
    assert_eq!(second[0].samples_16k, pcm[320_000..]);
}

#[test]
fn vad_flush_keeps_partial_tail_and_is_idempotent() {
    let mut segmenter = fixture();
    let mut pcm = scored_voice(320);
    pcm.extend([0.2; 27]);
    assert!(segmenter.push_chunk(ME, &pcm, 1_000).is_empty());
    let spans = segmenter.flush();
    assert_eq!(spans[0].samples_16k, pcm);
    assert_eq!(spans[0].end_ms, 1_321);
    assert!(segmenter.flush().is_empty());
}

#[test]
fn vad_tracks_independent_channel_clocks_and_flushes_in_time_order() {
    let mut segmenter = fixture();
    segmenter.push_chunk(ME, &scored_voice(320), 3_700);
    segmenter.push_chunk(OTHERS, &scored_voice(320), 400);
    let spans = segmenter.flush();
    assert_eq!(spans.len(), 2);
    assert_eq!((spans[0].channel, spans[0].start_ms), (OTHERS, 400));
    assert_eq!((spans[1].channel, spans[1].start_ms), (ME, 3_700));
}

#[test]
fn vad_does_not_bundle_a_timestamp_gap_into_one_utterance() {
    let mut segmenter = fixture();
    let pcm = scored_voice(320);
    segmenter.push_chunk(ME, &pcm, 0);
    let spans = segmenter.push_chunk(ME, &pcm, 900);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].end_ms, 320);
    assert_eq!(segmenter.flush()[0].start_ms, 900);
}

#[test]
fn vad_reset_discards_both_channels_and_restarts_at_new_timestamp() {
    let mut segmenter = fixture();
    let pcm = scored_voice(320);
    segmenter.push_chunk(ME, &pcm, 900);
    segmenter.push_chunk(OTHERS, &pcm, 1_000);
    segmenter.reset();
    assert!(segmenter.flush().is_empty());
    segmenter.push_chunk(OTHERS, &pcm, 0);
    assert_eq!(segmenter.flush()[0].start_ms, 0);
}
