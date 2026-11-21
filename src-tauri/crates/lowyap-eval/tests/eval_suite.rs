use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use hound::{SampleFormat, WavSpec, WavWriter};
use lowyap_dsp::{EarshotSpeechScorer, FrameSpeechScorer};
use lowyap_eval::reference_adapters::{
    estimate_pitch_hz, rms, ChannelOnlyDiarizer, ChannelPlusEnrollmentDiarizer,
    CorrelationGateEchoCanceller, EnergyVadSegmenter, ExtractiveBudgetSummarizer,
    FixedWindowSegmenter, FlowGuardedAsrAdapter, IdealOracleEchoCanceller, NlmsEchoCanceller,
    SyntheticWhisperAsrSim, SAMPLE_RATE_16K,
};
use lowyap_eval::scenarios::{
    apply_room_echo, long_multilingual_meeting_rollup, open_speaker_echo_bleed,
    run_soft_cancel_undo_invariants, synth_room_noise, synth_voiced_speech,
};
use lowyap_eval::{
    format_comparison_markdown, format_scorecard_markdown, format_wav_report_markdown,
    inspect_wav_pair, ComparisonReport, EvalScorecard, PipelineUnderTest,
};
use lowyap_ports::{
    AsrProvider, AsrResult, EchoCanceller, HybridNotesRequest, MeetingTemplate, NoteSummarizer,
    OutputRouteKind, PortError, SpeakerChannel, SpeakerDiarizer, UtteranceSegmenter, UtteranceSpan,
};

struct TempFixtureDir {
    path: PathBuf,
}

impl TempFixtureDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".eval-test-fixture-{tag}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test fixture dir");
        Self { path }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempFixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_wav_i16(path: &Path, samples: &[f32], sample_rate: u32, channels: u16) {
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).expect("create wav");
    for &s in samples {
        let q = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        for _ in 0..channels {
            writer.write_sample(q).expect("write wav sample");
        }
    }
    writer.finalize().expect("finalize wav");
}

#[test]
fn correlation_gate_passes_headphones_unchanged() {
    let mut aec = CorrelationGateEchoCanceller::new();
    let me = synth_voiced_speech(400, 175.0, 0.5, 1);
    let others = synth_voiced_speech(400, 130.0, 0.5, 2);
    let out = aec.process_16k(&me, &others, OutputRouteKind::Headphones);
    assert_eq!(out, me);
}

#[test]
fn nlms_beats_correlation_gate_on_45ms_nonlinear_bleed() {
    let scenario = open_speaker_echo_bleed();
    let mut base = CorrelationGateEchoCanceller::new();
    let mut nlms = NlmsEchoCanceller::new();

    let mut base_out = Vec::new();
    let mut nlms_out = Vec::new();
    for chunk in &scenario.chunks {
        base_out.extend(base.process_16k(&chunk.me_16k, &chunk.others_16k, chunk.output_route));
        nlms_out.extend(nlms.process_16k(&chunk.me_16k, &chunk.others_16k, chunk.output_route));
    }

    let pre_rms = rms(&scenario.me_bleed_16k);
    let base_leak = rms(&base_out) / pre_rms;
    let nlms_leak = rms(&nlms_out) / pre_rms;

    assert!(
        base_leak > 0.35,
        "expected correlation gate to leak on 45ms nonlinear echo, got {base_leak:.3}"
    );
    assert!(
        nlms_leak < 0.15,
        "expected NLMS to crush 45ms nonlinear echo, got {nlms_leak:.3}"
    );
    assert!(nlms_leak < base_leak * 0.35);
}

#[test]
fn ideal_oracle_echo_canceller_achieves_very_low_leak() {
    let scenario = open_speaker_echo_bleed();
    let mut oracle = IdealOracleEchoCanceller::with_delay_ms(45);
    let mut out = Vec::new();
    for chunk in &scenario.chunks {
        out.extend(oracle.process_16k(&chunk.me_16k, &chunk.others_16k, chunk.output_route));
    }
    let leak = rms(&out) / rms(&scenario.me_bleed_16k);
    assert!(
        leak < 0.10,
        "expected oracle echo leak < 0.10, got {leak:.4}"
    );
}

#[test]
fn fixed_window_segmenter_emits_10s_slices_including_silence() {
    let mut seg = FixedWindowSegmenter::default();
    let silence = synth_room_noise(25 * SAMPLE_RATE_16K, 0.0004, 11);
    let mut spans = seg.push_chunk(SpeakerChannel::Me, &silence, 0);
    spans.extend(seg.flush());

    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].samples_16k.len(), 10 * SAMPLE_RATE_16K);
    assert_eq!(spans[1].samples_16k.len(), 10 * SAMPLE_RATE_16K);
    assert_eq!(spans[2].samples_16k.len(), 5 * SAMPLE_RATE_16K);
}

#[test]
fn fixed_window_segmenter_resets_timestamps_after_exact_multiple_flush() {
    let mut seg = FixedWindowSegmenter::default();
    let exact = synth_room_noise(20 * SAMPLE_RATE_16K, 0.0004, 14);
    let first = seg.push_chunk(SpeakerChannel::Me, &exact, 0);
    assert_eq!(first.len(), 2);
    assert!(seg.flush().is_empty());

    let next = synth_room_noise(10 * SAMPLE_RATE_16K, 0.0004, 15);
    let second = seg.push_chunk(SpeakerChannel::Me, &next, 0);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].start_ms, 0);
    assert_eq!(second[0].end_ms, 10_000);
}

#[test]
fn energy_vad_drops_silence_below_minus_45_dbfs_and_keeps_speech() {
    let mut vad = EnergyVadSegmenter::default();
    let mut track = synth_room_noise(12 * SAMPLE_RATE_16K, 0.0005, 12);
    let speech = synth_voiced_speech(2_000, 170.0, 0.45, 13);
    let offset = 3 * SAMPLE_RATE_16K;
    for (i, &s) in speech.iter().enumerate() {
        track[offset + i] += s;
    }

    let mut spans = vad.push_chunk(SpeakerChannel::Me, &track, 0);
    spans.extend(vad.flush());

    assert_eq!(spans.len(), 1, "expected exactly one speech span");
    assert!(spans[0].start_ms >= 2_900 && spans[0].start_ms <= 3_200);
    assert!(spans[0].end_ms >= 4_900 && spans[0].end_ms <= 5_600);
}

#[test]
fn synthetic_whisper_reproduces_silence_hallucination_and_transcribes_speech() {
    let asr = SyntheticWhisperAsrSim;
    let long_silence = synth_room_noise(6 * SAMPLE_RATE_16K, 0.0005, 21);
    let en_res = asr.transcribe_16k(&long_silence, Some("en")).unwrap();
    assert_eq!(en_res.text, "Thank you.");
    assert!(SyntheticWhisperAsrSim::is_known_hallucination(&en_res.text));

    let ru_res = asr.transcribe_16k(&long_silence, Some("ru")).unwrap();
    assert!(SyntheticWhisperAsrSim::is_known_hallucination(&ru_res.text));

    let short_silence = synth_room_noise(2 * SAMPLE_RATE_16K, 0.0005, 22);
    let short_res = asr.transcribe_16k(&short_silence, Some("en")).unwrap();
    assert!(short_res.text.is_empty());

    let speech = synth_voiced_speech(2_000, 180.0, 0.45, 23);
    let speech_res = asr.transcribe_16k(&speech, Some("en")).unwrap();
    assert!(!speech_res.text.is_empty());
    assert!(!SyntheticWhisperAsrSim::is_known_hallucination(
        &speech_res.text
    ));
}

#[test]
fn pitch_estimator_distinguishes_speaker_profiles() {
    let low = synth_voiced_speech(1_500, 125.0, 0.45, 31);
    let mid = synth_voiced_speech(1_500, 170.0, 0.45, 32);
    let high = synth_voiced_speech(1_500, 215.0, 0.45, 33);

    let p_low = estimate_pitch_hz(&low).expect("low pitch");
    let p_mid = estimate_pitch_hz(&mid).expect("mid pitch");
    let p_high = estimate_pitch_hz(&high).expect("high pitch");

    assert!((p_low - 125.0).abs() < 15.0, "got {p_low}");
    assert!((p_mid - 170.0).abs() < 15.0, "got {p_mid}");
    assert!((p_high - 215.0).abs() < 18.0, "got {p_high}");
}

#[test]
fn enrollment_diarizer_separates_me_and_two_remote_speakers() {
    let mut diarizer = ChannelPlusEnrollmentDiarizer::new();
    let me_audio = synth_voiced_speech(2_000, 170.0, 0.5, 41);
    diarizer.observe_me_clean(&me_audio);
    assert!(diarizer.is_me_enrolled());

    let spk_a1 = UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: synth_voiced_speech(2_000, 125.0, 0.45, 42),
        start_ms: 4_000,
        end_ms: 6_000,
    };
    let spk_b1 = UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: synth_voiced_speech(2_000, 215.0, 0.45, 43),
        start_ms: 7_000,
        end_ms: 9_000,
    };
    let spk_a2 = UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: synth_voiced_speech(2_000, 125.0, 0.45, 44),
        start_ms: 10_000,
        end_ms: 12_000,
    };
    let me_span = UtteranceSpan {
        channel: SpeakerChannel::Me,
        samples_16k: me_audio,
        start_ms: 0,
        end_ms: 2_000,
    };

    let id_me = diarizer.identify(&me_span);
    let id_a1 = diarizer.identify(&spk_a1);
    let id_b1 = diarizer.identify(&spk_b1);
    let id_a2 = diarizer.identify(&spk_a2);

    assert_eq!(id_me.speaker_id, "me");
    assert_eq!(id_a1.speaker_id, "speaker_1");
    assert_eq!(id_b1.speaker_id, "speaker_2");
    assert_eq!(id_a2.speaker_id, "speaker_1");
    assert_eq!(diarizer.remote_cluster_count(), 2);

    diarizer.reset();
    assert!(!diarizer.is_me_enrolled());
    assert_eq!(diarizer.remote_cluster_count(), 0);
}

#[test]
fn channel_only_diarizer_collapses_remote_speakers() {
    let mut diarizer = ChannelOnlyDiarizer;
    let span_a = UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: synth_voiced_speech(1_000, 125.0, 0.45, 51),
        start_ms: 0,
        end_ms: 1_000,
    };
    let span_b = UtteranceSpan {
        channel: SpeakerChannel::Others,
        samples_16k: synth_voiced_speech(1_000, 215.0, 0.45, 52),
        start_ms: 2_000,
        end_ms: 3_000,
    };
    assert_eq!(diarizer.identify(&span_a).speaker_id, "others");
    assert_eq!(diarizer.identify(&span_b).speaker_id, "others");
}

#[test]
fn extractive_summarizer_rejects_budget_overflow_and_preserves_multilingual_scripts() {
    let summarizer = ExtractiveBudgetSummarizer::new(12_000);
    let scenario = long_multilingual_meeting_rollup();

    // Full 65-minute single-shot request must overflow the 12,000-char budget.
    let overflow_req = HybridNotesRequest {
        title: scenario.title.clone(),
        template: MeetingTemplate::General,
        rough_notes: scenario.rough_notes.clone(),
        rollups: Vec::new(),
        tail_segments: scenario.segments.clone(),
        language_instruction: Some(scenario.language_instruction.clone()),
    };
    assert!(summarizer.expand_hybrid_notes(&overflow_req).is_err());

    // Compact request with multilingual tail must succeed and retain scripts.
    let compact_req = HybridNotesRequest {
        title: scenario.title,
        template: MeetingTemplate::ClientCall,
        rough_notes: scenario.rough_notes,
        rollups: vec!["- Action: AEC3 + demo".to_owned()],
        tail_segments: scenario.segments[..6].to_vec(),
        language_instruction: Some(scenario.language_instruction),
    };
    let out = summarizer.expand_hybrid_notes(&compact_req).unwrap();
    for marker in scenario.required_markers {
        assert!(
            out.markdown.contains(marker),
            "expected marker '{marker}' in markdown:\n{}",
            out.markdown
        );
    }
}

#[test]
fn session_bus_invariants_pass_100_percent() {
    let res = run_soft_cancel_undo_invariants();
    assert!(res.total_checks >= 15);
    assert_eq!(
        res.passed_checks, res.total_checks,
        "failed invariants: {:?}",
        res.failures
    );
}

#[test]
fn modular_target_dramatically_outperforms_baseline_suite() {
    let mut base_pipe = PipelineUnderTest::baseline();
    let base = base_pipe.run_suite("baseline", None);

    let mut target_pipe = PipelineUnderTest::modular_target();
    let target = target_pipe.run_suite("modular-target", Some(8.5));

    assert!(target.passed, "modular-target missed 8.5 gate: {target:#?}");
    assert!(
        target.overall_score > base.overall_score + 2.5,
        "expected strong improvement: target={:.2} vs baseline={:.2}",
        target.overall_score,
        base.overall_score
    );

    // Individual dimension assertions
    assert!(
        target.metrics.echo_leak_rate < 0.15,
        "target echo leak too high: {}",
        target.metrics.echo_leak_rate
    );
    assert!(
        base.metrics.echo_leak_rate > target.metrics.echo_leak_rate + 0.25,
        "baseline should leak significantly more than target"
    );
    assert!(
        target.metrics.double_talk_survival_rate >= 0.85,
        "double-talk survival too low: {}",
        target.metrics.double_talk_survival_rate
    );
    assert_eq!(target.metrics.silence_hallucination_count, 0);
    assert!(
        base.metrics.silence_hallucination_count >= 3,
        "baseline should hallucinate on 10s silent slices"
    );
    assert_eq!(target.metrics.speaker_self_recall, 1.0);
    assert_eq!(target.metrics.remote_cluster_purity, 1.0);
    assert_eq!(target.metrics.speaker_attribution_f1, 1.0);
    assert!(base.metrics.speaker_attribution_f1 < 0.8);
    assert_eq!(target.metrics.summary_overflow_count, 0);
    assert_eq!(target.metrics.summary_budget_pass_rate, 1.0);
    assert_eq!(base.metrics.summary_overflow_count, 1);
    assert_eq!(target.metrics.session_bus_invariant_pass_rate, 1.0);
}

#[test]
fn pipeline_suite_is_strictly_deterministic_across_repeated_runs() {
    let mut pipe = PipelineUnderTest::modular_target();
    let run1 = pipe.run_suite("modular-target", None);
    let run2 = pipe.run_suite("modular-target", None);
    assert_eq!(run1, run2);
}

#[test]
fn production_dsp_meets_score_leak_and_double_talk_gates() {
    let mut pipeline = PipelineUnderTest::production_dsp();
    let card = pipeline.run_suite("production-dsp", Some(9.5));
    assert!(card.passed, "{card:#?}");
    assert!(card.metrics.echo_leak_rate <= 0.02, "{:?}", card.metrics);
    assert!(
        card.metrics.double_talk_survival_rate >= 0.88,
        "{:?}",
        card.metrics
    );
    assert_eq!(card.metrics.silence_hallucination_count, 0);
    assert_eq!(pipeline.run_suite("production-dsp", Some(9.5)), card);
}

#[test]
fn earshot_assist_keeps_synthetic_voicing_but_rejects_room_noise() {
    let speech = synth_voiced_speech(2_000, 125.0, 0.45, 122);
    let mut scorer = EarshotSpeechScorer::new();
    let assisted_voiced = speech
        .as_chunks::<256>()
        .0
        .iter()
        .filter(|frame| scorer.score_frame(*frame) >= 0.5)
        .count();
    assert!(assisted_voiced >= 115, "{assisted_voiced}/125 voiced");
    for dbfs in [-45.0_f32, -50.0, -55.0] {
        let noise = synth_room_noise(16_384, 10.0_f32.powf(dbfs / 20.0), 123);
        for frame in noise.as_chunks::<256>().0 {
            assert_eq!(scorer.score_frame(frame), 0.0, "noise at {dbfs} dBFS");
        }
    }
}

#[test]
fn cli_all_lists_production_and_reference_suites_in_order() {
    let output = Command::new(env!("CARGO_BIN_EXE_lowyap-eval"))
        .args(["--suite", "all", "--min-score", "9.9", "--json"])
        .output()
        .expect("run suite");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: ComparisonReport = serde_json::from_slice(&output.stdout).expect("comparison");
    let names: Vec<_> = report
        .scorecards
        .iter()
        .map(|card| card.suite_name.as_str())
        .collect();
    assert_eq!(
        names,
        ["baseline", "production-dsp", "production", "modular-target"]
    );
    assert!(report.passed);
    let prod = &report.scorecards[2];
    assert_eq!(prod.min_score_threshold, Some(9.9));
    assert!(prod.passed);
}

#[test]
fn flow_guarded_asr_strips_silence_music_and_cross_script_phantoms() {
    struct StaticPhantomAsr(&'static str, Option<f32>);

    impl AsrProvider for StaticPhantomAsr {
        fn name(&self) -> &'static str {
            "static_phantom_asr"
        }

        fn transcribe_16k(
            &self,
            _samples_16k: &[f32],
            language_hint: Option<&str>,
        ) -> Result<AsrResult, PortError> {
            Ok(AsrResult {
                text: self.0.to_owned(),
                detected_language: language_hint.map(str::to_owned),
                no_speech_prob: self.1,
            })
        }
    }

    let guarded = FlowGuardedAsrAdapter::new(Box::new(SyntheticWhisperAsrSim::new()));
    assert_eq!(guarded.name(), "flow_guarded_asr");

    let long_silence = synth_room_noise(6 * SAMPLE_RATE_16K, 0.0005, 131);
    for lang in [Some("en"), Some("ru"), Some("ur")] {
        let res = guarded.transcribe_16k(&long_silence, lang).unwrap();
        assert!(
            res.text.is_empty(),
            "expected silence phantom stripped for lang={lang:?}, got {:?}",
            res.text
        );
    }

    let speech = synth_voiced_speech(2_000, 175.0, 0.45, 132);
    let speech_res = guarded.transcribe_16k(&speech, Some("en")).unwrap();
    assert!(!speech_res.text.is_empty());

    let music_guarded = FlowGuardedAsrAdapter::new(Box::new(StaticPhantomAsr(
        "[Music] ご視聴ありがとうございました Привет",
        Some(0.10),
    )));
    let music_res = music_guarded.transcribe_16k(&speech, Some("en")).unwrap();
    assert!(
        music_res.text.is_empty(),
        "expected music/cross-script phantoms stripped, got {:?}",
        music_res.text
    );
}

#[test]
fn production_suite_wires_all_four_crates_and_meets_gates() {
    let mut pipeline = PipelineUnderTest::production();
    let card = pipeline.run_suite("production", Some(9.9));

    assert!(card.passed, "production missed 9.90 gate: {card:#?}");
    assert!(
        card.overall_score >= 9.90,
        "expected overall_score >= 9.90, got {:.2}",
        card.overall_score
    );
    assert_eq!(card.adapters.echo_canceller, "webrtc_aec3");
    assert_eq!(card.adapters.segmenter, "earshot");
    assert_eq!(card.adapters.asr, "flow_guarded_asr");
    assert_eq!(card.adapters.diarizer, "hybrid_voiceprint_diarizer");
    assert_eq!(card.adapters.summarizer, "hybrid_map_reduce_summarizer");
    assert!(card.adapters.rolling_window_summary);

    assert_eq!(card.metrics.silence_hallucination_count, 0);
    assert!(
        card.metrics.speaker_self_recall >= 0.99,
        "{:?}",
        card.metrics
    );
    assert!(
        card.metrics.remote_cluster_purity >= 0.99,
        "{:?}",
        card.metrics
    );
    assert!(
        card.metrics.speaker_attribution_f1 >= 0.99,
        "{:?}",
        card.metrics
    );
    assert_eq!(card.metrics.summary_overflow_count, 0);
    assert!(
        card.metrics.summary_budget_pass_rate >= 0.99,
        "{:?}",
        card.metrics
    );
    assert!(
        card.metrics.multilingual_marker_recall >= 0.99,
        "{:?}",
        card.metrics
    );

    assert_eq!(pipeline.run_suite("production", Some(9.9)), card);
}

#[test]
fn cli_compare_and_production_suites_target_full_production_profile() {
    let output = Command::new(env!("CARGO_BIN_EXE_lowyap-eval"))
        .args(["--suite", "production", "--min-score", "9.9", "--json"])
        .output()
        .expect("run production suite");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let card: EvalScorecard = serde_json::from_slice(&output.stdout).expect("production scorecard");
    assert_eq!(card.suite_name, "production");
    assert!(card.overall_score >= 9.90);
    assert!(card.passed);

    let cmp_output = Command::new(env!("CARGO_BIN_EXE_lowyap-eval"))
        .args(["--suite", "compare", "--min-score", "9.9", "--json"])
        .output()
        .expect("run compare suite");
    assert!(
        cmp_output.status.success(),
        "{}",
        String::from_utf8_lossy(&cmp_output.stderr)
    );
    let cmp_report: ComparisonReport =
        serde_json::from_slice(&cmp_output.stdout).expect("compare report");
    let cli_cmp_names: Vec<_> = cmp_report
        .scorecards
        .iter()
        .map(|c| c.suite_name.as_str())
        .collect();
    assert_eq!(cli_cmp_names, ["baseline", "production"]);
    assert!(cmp_report.passed);
}

#[test]
fn scorecard_and_comparison_json_roundtrip() {
    let mut base_pipe = PipelineUnderTest::baseline();
    let base = base_pipe.run_suite("baseline", None);
    let mut target_pipe = PipelineUnderTest::modular_target();
    let target = target_pipe.run_suite("modular-target", Some(8.0));

    let json = serde_json::to_string(&target).expect("serialize scorecard");
    let decoded: EvalScorecard = serde_json::from_str(&json).expect("deserialize scorecard");
    assert_eq!(decoded, target);

    let cmp = ComparisonReport {
        scorecards: vec![base, target],
        passed: true,
    };
    let cmp_json = serde_json::to_string(&cmp).expect("serialize comparison");
    let cmp_decoded: ComparisonReport =
        serde_json::from_str(&cmp_json).expect("deserialize comparison");
    assert_eq!(cmp_decoded, cmp);
}

#[test]
fn min_score_gate_fails_when_threshold_exceeds_score() {
    let mut base_pipe = PipelineUnderTest::baseline();
    let card = base_pipe.run_suite("baseline", Some(9.5));
    assert!(!card.passed);
    assert_eq!(card.min_score_threshold, Some(9.5));
}

#[test]
fn markdown_formatters_include_expected_headers_and_scores() {
    let mut base_pipe = PipelineUnderTest::baseline();
    let base = base_pipe.run_suite("baseline", None);
    let mut target_pipe = PipelineUnderTest::modular_target();
    let target = target_pipe.run_suite("modular-target", None);

    let single_md = format_scorecard_markdown(&target);
    assert!(single_md.contains("lowyap-eval Scorecard: `modular-target`"));
    assert!(single_md.contains("Echo Suppression"));

    let cmp = ComparisonReport {
        scorecards: vec![base, target],
        passed: true,
    };
    let cmp_md = format_comparison_markdown(&cmp);
    assert!(cmp_md.contains("Delta (Target - Baseline)"));
    assert!(cmp_md.contains("Silence Hallucinations"));
}

#[test]
fn wav_pair_inspector_detects_delay_and_compares_profiles_without_mutating_inputs() {
    let dir = TempFixtureDir::new("wav16k");
    let me_path = dir.file("me_16k.wav");
    let others_path = dir.file("others_16k.wav");

    // 4 s remote speech + 6 s silence so fixed 10s vs VAD behavior is exercised too.
    let mut others = synth_voiced_speech(4_000, 140.0, 0.45, 71);
    others.extend(synth_room_noise(6 * SAMPLE_RATE_16K, 0.0004, 72));
    let mut me = apply_room_echo(&others, 45, 0.55, 0.30, 1.7);
    let ambient = synth_room_noise(me.len(), 0.0005, 73);
    for (s, n) in me.iter_mut().zip(ambient.iter()) {
        *s += *n;
    }

    write_wav_i16(&me_path, &me, 16_000, 1);
    write_wav_i16(&others_path, &others, 16_000, 1);

    let me_bytes_before = fs::read(&me_path).unwrap();
    let others_bytes_before = fs::read(&others_path).unwrap();

    let report = inspect_wav_pair(&me_path, &others_path, OutputRouteKind::Speakers)
        .expect("inspect wav pair");

    // Verify read-only: files untouched.
    assert_eq!(fs::read(&me_path).unwrap(), me_bytes_before);
    assert_eq!(fs::read(&others_path).unwrap(), others_bytes_before);

    let delay = report.estimated_echo_delay_ms.expect("detected delay");
    assert!(
        (delay - 45.0).abs() <= 3.0,
        "expected ~45ms delay, got {delay:.1}ms"
    );
    assert_eq!(report.profiles.len(), 3);
    let base_prof = &report.profiles[0];
    let mod_prof = &report.profiles[1];
    assert!(
        mod_prof.me_attenuation_db > base_prof.me_attenuation_db + 5.0,
        "expected modular AEC attenuation ({:.1} dB) > baseline ({:.1} dB)",
        mod_prof.me_attenuation_db,
        base_prof.me_attenuation_db
    );
    assert_eq!(mod_prof.simulated_hallucination_count, 0);
    let production_prof = &report.profiles[2];
    assert_eq!(production_prof.echo_canceller, "webrtc_aec3");
    assert!(
        production_prof.post_aec_me_rms < report.me_track.rms * 0.04,
        "{production_prof:?}"
    );
    assert!(production_prof.me_attenuation_db >= 28.0);

    let md = format_wav_report_markdown(&report);
    assert!(md.contains("WAV Pair Inspection"));
    assert!(md.contains("modular-target"));
    assert!(md.contains("webrtc_aec3"));
}

#[test]
fn wav_pair_inspector_resamples_stereo_48k_inputs() {
    let dir = TempFixtureDir::new("wav48k");
    let me_path = dir.file("me_48k.wav");
    let others_path = dir.file("others_48k.wav");

    // Synthesize 1 s at 48 kHz (3x oversampled) stereo.
    let n48 = 48_000usize;
    let samples: Vec<f32> = (0..n48)
        .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 160.0 / 48_000.0).sin() * 0.3)
        .collect();
    write_wav_i16(&me_path, &samples, 48_000, 2);
    write_wav_i16(&others_path, &vec![0.0_f32; n48], 48_000, 2);

    let report = inspect_wav_pair(&me_path, &others_path, OutputRouteKind::Headphones)
        .expect("inspect 48k stereo wav");
    assert_eq!(report.me_track.source_sample_rate, 48_000);
    assert_eq!(report.me_track.source_channels, 2);
    assert_eq!(report.me_track.duration_ms, 1_000);
}

#[test]
fn wav_pair_rms_excludes_padding_after_the_shorter_mic_track() {
    let dir = TempFixtureDir::new("unequal");
    let me_path = dir.file("me.wav");
    let others_path = dir.file("others.wav");
    write_wav_i16(&me_path, &[0.2; 173], 16_000, 1);
    write_wav_i16(&others_path, &[0.01; 16_000], 16_000, 1);
    let report = inspect_wav_pair(&me_path, &others_path, OutputRouteKind::Headphones).unwrap();
    for profile in report.profiles {
        assert_eq!(profile.post_aec_me_rms, report.me_track.rms);
        assert_eq!(profile.me_attenuation_db, 0.0);
    }

    let speakers_report =
        inspect_wav_pair(&me_path, &others_path, OutputRouteKind::Speakers).unwrap();
    assert_eq!(speakers_report.profiles.len(), 3);
    for profile in speakers_report.profiles {
        assert!(profile.post_aec_me_rms.is_finite());
        assert!(profile.me_attenuation_db.is_finite());
    }
}

#[test]
fn wav_pair_inspector_sanitizes_non_finite_float_wav_samples() {
    let dir = TempFixtureDir::new("wavfloatnan");
    let me_path = dir.file("me_nan.wav");
    let others_path = dir.file("others_nan.wav");

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut me_writer = hound::WavWriter::create(&me_path, spec).expect("create float me wav");
    let mut oth_writer =
        hound::WavWriter::create(&others_path, spec).expect("create float others wav");
    for i in 0..16_000 {
        let sample = match i {
            100 => f32::NAN,
            200 => f32::INFINITY,
            300 => f32::NEG_INFINITY,
            _ => ((i as f32 * 0.05).sin()) * 0.25,
        };
        me_writer.write_sample(sample).unwrap();
        oth_writer.write_sample(0.0_f32).unwrap();
    }
    me_writer.finalize().unwrap();
    oth_writer.finalize().unwrap();

    let report = inspect_wav_pair(&me_path, &others_path, OutputRouteKind::Speakers)
        .expect("inspect float wav with non-finite samples");
    assert!(report.me_track.rms.is_finite());
    assert!(report.me_track.peak_abs.is_finite());
    assert!(report.me_track.peak_abs <= 1.0);
    for profile in &report.profiles {
        assert!(profile.post_aec_me_rms.is_finite());
        assert!(profile.me_attenuation_db.is_finite());
    }
}
