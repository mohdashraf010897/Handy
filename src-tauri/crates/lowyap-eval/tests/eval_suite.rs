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
    append_trailing_breath_tail, attenuate_quiet_mic, compute_cer, compute_script_preservation,
    compute_wer, detect_brand_token_hallucination, detect_phantom_hallucinations,
    evaluate_real_voice_with_asr, extract_edge_silence_slices, format_comparison_markdown,
    format_dictation_bench_markdown, format_real_voice_markdown, format_scorecard_markdown,
    format_wav_report_markdown, inject_pink_fan_noise, inspect_wav_pair, load_manifest,
    redact_transcript_preview, run_dictation_bench, run_real_voice_suite, BrandLeakReason,
    ClipCategory, ComparisonReport, EvalScorecard, EvaluationTier, PerturbationKind,
    PhantomFlagKind, PipelineUnderTest, RealVoiceConfig, RealVoiceReport, RealVoiceStatus,
    ScriptKind, DEFAULT_PERTURBATION_SEED, DEFAULT_PINK_FAN_SNR_DB, DEFAULT_QUIET_MIC_GAIN,
    DEFAULT_TRAILING_BREATH_TAIL_MS, SYNTHETIC_HONESTY_BANNER, UNVERIFIED_NO_ASR_STATUS,
    UNVERIFIED_NO_WAVS_STATUS,
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
        evaluation_tier: EvaluationTier::SyntheticPlumbingOnly,
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
    assert!(single_md.contains("PLUMBING GATE: PASS (6/6 deterministic invariants)"));
    assert!(single_md.contains(SYNTHETIC_HONESTY_BANNER));
    assert!(single_md.contains("Echo Suppression"));
    assert!(!single_md.contains("/ 10"));

    let cmp = ComparisonReport {
        evaluation_tier: EvaluationTier::SyntheticPlumbingOnly,
        scorecards: vec![base, target],
        passed: true,
    };
    let cmp_md = format_comparison_markdown(&cmp);
    assert!(cmp_md.contains("PLUMBING GATE:"));
    assert!(cmp_md.contains(SYNTHETIC_HONESTY_BANNER));
    assert!(cmp_md.contains("Delta (Target - Baseline)"));
    assert!(cmp_md.contains("Silence Hallucinations"));
    assert!(!cmp_md.contains("/ 10"));
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

// ============================================================================
// Anti-Euphoria QA Reform + Real-Voice Benchmark Tests
// ============================================================================

#[test]
fn synthetic_suites_tag_synthetic_plumbing_only_and_abolish_fake_out_of_ten_headlines() {
    for (name, mut pipe) in [
        ("baseline", PipelineUnderTest::baseline()),
        ("modular-target", PipelineUnderTest::modular_target()),
        ("ideal-oracle", PipelineUnderTest::ideal_oracle()),
        ("production", PipelineUnderTest::production()),
    ] {
        let card = pipe.run_suite(name, None);
        assert_eq!(card.evaluation_tier, EvaluationTier::SyntheticPlumbingOnly);
        let md = format_scorecard_markdown(&card);
        assert!(
            md.contains("PLUMBING GATE:"),
            "{name} missing gate header:\n{md}"
        );
        assert!(
            md.contains(SYNTHETIC_HONESTY_BANNER),
            "{name} missing honesty banner:\n{md}"
        );
        assert!(
            !md.contains("/ 10") && !md.contains("/10"),
            "{name} leaked fake /10 score:\n{md}"
        );
    }

    let dict = run_dictation_bench();
    assert_eq!(dict.evaluation_tier, EvaluationTier::SyntheticPlumbingOnly);
    let dict_md = format_dictation_bench_markdown(&dict);
    assert!(dict_md.contains("PLUMBING GATE: PASS (5/5 deterministic invariants)"));
    assert!(dict_md.contains(SYNTHETIC_HONESTY_BANNER));
}

#[test]
fn wer_and_cer_math_handles_english_urdu_devanagari_and_punctuation_normalization() {
    // English punctuation & case normalization -> 0.0 WER/CER.
    let exact = compute_wer(
        "Hello, world! Let's ship the release.",
        "hello world let's ship the release",
    );
    assert_eq!(exact.edits, 0);
    assert_eq!(exact.rate, 0.0);

    // 1 substitution out of 4 words -> 0.25 WER.
    let sub = compute_wer("one two three four", "one two tree four");
    assert_eq!(sub.edits, 1);
    assert_eq!(sub.reference_len, 4);
    assert!((sub.rate - 0.25).abs() < 1e-6);

    // Urdu script with Urdu punctuation (۔ / ،) -> 0.0 WER & CER when matching.
    let urdu_exact = compute_cer(
        "آج کی میٹنگ، وقت پر شروع ہوئی۔",
        "آج کی میٹنگ وقت پر شروع ہوئی",
    );
    assert_eq!(urdu_exact.edits, 0);
    assert_eq!(urdu_exact.rate, 0.0);

    // Devanagari script preserves combining vowel matras (`ा`, `े`, `ी`) so changing a matra is counted!
    let dev_exact = compute_cer("नमस्ते दुनिया।", "नमस्ते दुनिया");
    assert_eq!(dev_exact.edits, 0);
    assert_eq!(dev_exact.rate, 0.0);

    let dev_matra_diff = compute_cer("नमस्ते", "नमस्त");
    assert_eq!(
        dev_matra_diff.edits, 1,
        "Devanagari matra drop must be counted in CER"
    );
    assert!(dev_matra_diff.rate > 0.0);

    // Code-switch script preservation (Latin + Urdu + Devanagari).
    let preserved = compute_script_preservation(
        "Release status: آج کا ڈیمو तैयार है",
        "release status آج کا ڈیمو तैयार है",
    );
    assert!(preserved.is_code_switched);
    assert_eq!(preserved.preserved_script_ratio, 1.0);
    assert!(preserved.unexpected_scripts.is_empty());
    assert!(preserved.expected_scripts.contains(&ScriptKind::Latin));
    assert!(preserved.expected_scripts.contains(&ScriptKind::ArabicUrdu));
    assert!(preserved.expected_scripts.contains(&ScriptKind::Devanagari));

    // Cross-script hallucination (Cyrillic injected when reference is English) is flagged.
    let drifted = compute_script_preservation("ship the build", "ship the build Привет");
    assert_eq!(drifted.unexpected_scripts, vec![ScriptKind::Cyrillic]);
}

#[test]
fn silence_phantom_and_brand_injection_detectors_catch_real_failure_patterns() {
    let yt = detect_phantom_hallucinations("Thank you for watching!", false);
    assert!(
        yt.iter()
            .any(|f| f.kind == PhantomFlagKind::KnownPhantomPhrase),
        "expected YouTube phantom phrase caught: {yt:?}"
    );

    let subs = detect_phantom_hallucinations("Subtitles by the Amara.org community", false);
    assert!(
        subs.iter()
            .any(|f| f.kind == PhantomFlagKind::KnownPhantomPhrase),
        "expected subtitle phantom caught: {subs:?}"
    );

    let tri_loop =
        detect_phantom_hallucinations("check the mic check the mic check the mic", false);
    assert!(
        tri_loop
            .iter()
            .any(|f| f.kind == PhantomFlagKind::RepeatedNgramLoop),
        "expected 3-gram loop caught: {tri_loop:?}"
    );

    let silence_out = detect_phantom_hallucinations("Thank you.", true);
    assert!(silence_out
        .iter()
        .any(|f| f.kind == PhantomFlagKind::SilenceSliceHallucination));

    let clean_speech = detect_phantom_hallucinations(
        "Please send the updated calendar invite after lunch.",
        false,
    );
    assert!(clean_speech.is_empty(), "false positive: {clean_speech:?}");

    // SV-95: standalone ALL-CAPS LOWYAP over "Transcription" (unlabeled and labeled).
    let sv95_unlabeled =
        detect_brand_token_hallucination("Open the LOWYAP settings panel", None, 4.0)
            .expect("ALL-CAPS LOWYAP must be flagged");
    assert_eq!(sv95_unlabeled.reason, BrandLeakReason::AllCapsBrandToken);
    assert_eq!(sv95_unlabeled.all_caps_count, 1);

    let sv95_labeled = detect_brand_token_hallucination(
        "Open the lowyap settings panel",
        Some("Open the transcription settings panel"),
        4.0,
    )
    .expect("unreferenced brand injection must be flagged");
    assert_eq!(
        sv95_labeled.reason,
        BrandLeakReason::UnreferencedBrandInjection
    );

    // Disproportionate density without reference justification.
    let dense = detect_brand_token_hallucination("lowyap lowyap test", None, 3.0)
        .expect("disproportionate density must be flagged");
    assert_eq!(dense.reason, BrandLeakReason::DisproportionateBrandDensity);

    // Legitimate single lowercase mention in longer unlabeled utterance or referenced utterance passes.
    assert!(detect_brand_token_hallucination(
        "We tested lowyap on a real microphone during the team call today.",
        None,
        6.0
    )
    .is_none());
    assert!(detect_brand_token_hallucination(
        "We tested LowYap today.",
        Some("We tested LowYap today."),
        2.5
    )
    .is_none());
}

#[test]
fn acoustic_perturbations_preserve_length_invariants_and_deterministic_seeds() {
    let speech = synth_voiced_speech(2_000, 165.0, 0.40, 201);
    assert_eq!(speech.len(), 2 * SAMPLE_RATE_16K);

    // 1. +12 dB SNR pink/fan noise: exact length + deterministic + measurable SNR.
    let noisy_a =
        inject_pink_fan_noise(&speech, DEFAULT_PINK_FAN_SNR_DB, DEFAULT_PERTURBATION_SEED);
    let noisy_b =
        inject_pink_fan_noise(&speech, DEFAULT_PINK_FAN_SNR_DB, DEFAULT_PERTURBATION_SEED);
    let noisy_c = inject_pink_fan_noise(
        &speech,
        DEFAULT_PINK_FAN_SNR_DB,
        DEFAULT_PERTURBATION_SEED + 1,
    );
    assert_eq!(noisy_a.len(), speech.len());
    assert_eq!(
        noisy_a, noisy_b,
        "pink/fan noise must be deterministic for same seed"
    );
    assert_ne!(
        noisy_a, noisy_c,
        "different seeds must produce distinct noise"
    );
    let residual: Vec<f32> = noisy_a
        .iter()
        .zip(speech.iter())
        .map(|(n, s)| n - s)
        .collect();
    let snr_db = 20.0 * (rms(&speech) / rms(&residual)).log10();
    assert!(
        (snr_db - 12.0).abs() < 0.75,
        "expected ~12 dB SNR, got {snr_db:.2} dB"
    );

    // 2. 0.25x (-12 dB) quiet-mic attenuation: exact length + exact 0.25x RMS.
    let quiet = attenuate_quiet_mic(&speech, DEFAULT_QUIET_MIC_GAIN);
    assert_eq!(quiet.len(), speech.len());
    let ratio = rms(&quiet) / rms(&speech);
    assert!(
        (ratio - 0.25).abs() < 1e-4,
        "expected 0.25x RMS attenuation, got {ratio:.5}"
    );

    // 3. +1.5s trailing breath + room-tone tail: exact +24,000 samples + deterministic + low-level tail.
    let tail_a = append_trailing_breath_tail(
        &speech,
        DEFAULT_TRAILING_BREATH_TAIL_MS,
        DEFAULT_PERTURBATION_SEED,
    );
    let tail_b = append_trailing_breath_tail(
        &speech,
        DEFAULT_TRAILING_BREATH_TAIL_MS,
        DEFAULT_PERTURBATION_SEED,
    );
    assert_eq!(tail_a.len(), speech.len() + 24_000);
    assert_eq!(tail_a, tail_b);
    assert_eq!(&tail_a[..speech.len()], speech.as_slice());
    let tail_rms = rms(&tail_a[speech.len()..]);
    assert!(
        tail_rms > 0.002 && tail_rms < 0.02,
        "expected realistic room-tone/breath tail RMS, got {tail_rms:.5}"
    );
}

#[test]
fn real_voice_zero_wavs_reports_unverified_and_refuses_score() {
    let report = run_real_voice_suite(&RealVoiceConfig::default()).expect("run zero-wav suite");
    assert_eq!(report.evaluation_tier, EvaluationTier::EmpiricalRealAudio);
    assert_eq!(report.status, RealVoiceStatus::Unverified);
    assert_eq!(report.status_banner, UNVERIFIED_NO_WAVS_STATUS);
    assert_eq!(report.clips_evaluated, 0);
    assert!(report.aggregate_wer.is_none());

    let md = format_real_voice_markdown(&report, false);
    assert!(md.contains(UNVERIFIED_NO_WAVS_STATUS));

    // CLI invocation with 0 WAVs exits non-zero and emits UNVERIFIED in JSON & Markdown.
    let cli_json = Command::new(env!("CARGO_BIN_EXE_lowyap-eval"))
        .args(["--suite", "real-voice", "--json"])
        .output()
        .expect("run real-voice cli json");
    assert!(!cli_json.status.success(), "0-WAV real-voice must not pass");
    let decoded: RealVoiceReport =
        serde_json::from_slice(&cli_json.stdout).expect("parse real-voice json");
    assert_eq!(decoded.status, RealVoiceStatus::Unverified);
    assert_eq!(decoded.status_banner, UNVERIFIED_NO_WAVS_STATUS);

    let cli_md = Command::new(env!("CARGO_BIN_EXE_lowyap-eval"))
        .args(["--suite", "real-voice"])
        .output()
        .expect("run real-voice cli markdown");
    assert!(!cli_md.status.success());
    let stdout = String::from_utf8_lossy(&cli_md.stdout);
    assert!(stdout.contains(UNVERIFIED_NO_WAVS_STATUS));
}

#[test]
fn real_voice_evaluates_manifests_sidecars_privacy_and_external_asr_cmd() {
    let dir = TempFixtureDir::new("realvoice");

    // Clip 1: 1.0s leading silence + 1.5s voiced speech + 1.0s trailing silence, with sidecar `.txt`.
    let mut clip1_audio = synth_room_noise(SAMPLE_RATE_16K, 0.0005, 301);
    clip1_audio.extend(synth_voiced_speech(1_500, 170.0, 0.40, 302));
    clip1_audio.extend(synth_room_noise(SAMPLE_RATE_16K, 0.0005, 303));
    let clip1_wav = dir.file("clip_alpha.wav");
    write_wav_i16(&clip1_wav, &clip1_audio, 16_000, 1);
    let private_sentence = "My confidential salary number is ninety thousand dollars";
    fs::write(dir.file("clip_alpha.txt"), private_sentence).expect("write sidecar");

    // Verify edge silence slicing extracts both 1.0s silence margins.
    let (lead, trail) = extract_edge_silence_slices(&clip1_audio);
    assert!(lead.len() >= 15_000 && trail.len() >= 15_000);

    // Without --asr-cmd, WAVs are inspected but status stays UNVERIFIED.
    let no_asr_report = run_real_voice_suite(&RealVoiceConfig {
        wav_dir: Some(dir.path.clone()),
        ..RealVoiceConfig::default()
    })
    .expect("run no-asr suite");
    assert_eq!(no_asr_report.status, RealVoiceStatus::Unverified);
    assert_eq!(no_asr_report.status_banner, UNVERIFIED_NO_ASR_STATUS);
    assert_eq!(no_asr_report.clips_evaluated, 1);

    // With callback ASR that returns silence on quiet slices and exact transcript on speech.
    let cfg = RealVoiceConfig {
        wav_dir: Some(dir.path.clone()),
        verbose_redacted: false,
        ..RealVoiceConfig::default()
    };
    let pass_report = evaluate_real_voice_with_asr(
        &cfg,
        Some(|samples: &[f32]| {
            if rms(samples) < 0.015 {
                Ok(String::new())
            } else {
                Ok(private_sentence.to_owned())
            }
        }),
    )
    .expect("evaluate pass real voice");

    assert_eq!(pass_report.status, RealVoiceStatus::Pass);
    assert_eq!(pass_report.clips_evaluated, 1);
    assert_eq!(pass_report.labeled_clips, 1);
    assert_eq!(pass_report.aggregate_wer, Some(0.0));
    assert_eq!(pass_report.aggregate_cer, Some(0.0));
    assert_eq!(pass_report.silence_hallucination_clips, 0);
    assert_eq!(pass_report.brand_leak_clips, 0);
    assert_eq!(pass_report.perturbation_passed, 3);
    assert_eq!(pass_report.perturbation_total, 3);
    assert!(pass_report.rtf_p50.is_some());
    assert!(pass_report.rtf_p95.is_some());
    assert!(pass_report.latency_ms_p50.is_some());
    assert!(pass_report.latency_ms_p95.is_some());

    // Privacy check (D-05): default Markdown and JSON must NOT contain the private sentence!
    let md_private = format_real_voice_markdown(&pass_report, false);
    assert!(
        !md_private.contains("confidential") && !md_private.contains("ninety thousand"),
        "default real-voice Markdown leaked private transcript:\n{md_private}"
    );
    let json_private = serde_json::to_string(&pass_report).expect("serialize pass report");
    assert!(
        !json_private.contains("confidential") && !json_private.contains("ninety thousand"),
        "default real-voice JSON leaked private transcript"
    );

    // Even redacted preview masks words and never prints the raw private sentence.
    let redacted = redact_transcript_preview(private_sentence);
    assert!(!redacted.contains("confidential"));
    assert!(!redacted.contains("ninety"));
    assert!(redacted.contains("[redacted 8w:"));

    // Failing callback: hallucinates "Thank you for watching" on silence and "LOWYAP" on speech.
    let fail_report = evaluate_real_voice_with_asr(
        &cfg,
        Some(|samples: &[f32]| {
            if rms(samples) < 0.015 {
                Ok("Thank you for watching.".to_owned())
            } else {
                Ok("My LOWYAP salary number is ninety thousand dollars".to_owned())
            }
        }),
    )
    .expect("evaluate failing real voice");
    assert_eq!(fail_report.status, RealVoiceStatus::Fail);
    assert_eq!(fail_report.silence_hallucination_clips, 1);
    assert_eq!(fail_report.brand_leak_clips, 1);
    assert!(!fail_report.clips[0].passed);

    // JSON & TOML manifest loaders both resolve relative paths and categories.
    let json_manifest = dir.file("corpus.json");
    fs::write(
        &json_manifest,
        r#"{ "clips": [ { "wav": "clip_alpha.wav", "category": "noisy_room", "clip_id": "json_alpha" } ] }"#,
    )
    .expect("write json manifest");
    let loaded_json = load_manifest(&json_manifest).expect("load json manifest");
    assert_eq!(loaded_json.len(), 1);
    assert_eq!(loaded_json[0].category, ClipCategory::NoisyRoom);
    assert_eq!(loaded_json[0].clip_id.as_deref(), Some("json_alpha"));
    assert_eq!(
        loaded_json[0].reference_text.as_deref(),
        Some(private_sentence)
    );

    let toml_manifest = dir.file("corpus.toml");
    fs::write(
        &toml_manifest,
        r#"
# Real human corpus manifest
[[clips]]
wav = "clip_alpha.wav"
category = "accent_multilingual"
clip_id = "toml_alpha"
reference_text = "آج کا ڈیمو ready hai"
"#,
    )
    .expect("write toml manifest");
    let loaded_toml = load_manifest(&toml_manifest).expect("load toml manifest");
    assert_eq!(loaded_toml.len(), 1);
    assert_eq!(loaded_toml[0].category, ClipCategory::AccentMultilingual);
    assert_eq!(loaded_toml[0].clip_id.as_deref(), Some("toml_alpha"));
    assert_eq!(
        loaded_toml[0].reference_text.as_deref(),
        Some("آج کا ڈیمو ready hai")
    );
    assert_eq!(
        pass_report.clips[0].perturbations[0].kind,
        PerturbationKind::PinkFanNoise12DbSnr
    );

    // External `--asr-cmd` CLI test using a simple clip without silence edges.
    let short_speech_wav = dir.file("short_speech.wav");
    write_wav_i16(
        &short_speech_wav,
        &synth_voiced_speech(1_000, 175.0, 0.4, 304),
        16_000,
        1,
    );
    let cli_manifest = dir.file("cli_manifest.json");
    fs::write(
        &cli_manifest,
        r#"[{ "wav": "short_speech.wav", "reference_text": "hello world", "category": "clean" }]"#,
    )
    .expect("write cli manifest");

    let cli_out = Command::new(env!("CARGO_BIN_EXE_lowyap-eval"))
        .args([
            "--suite",
            "real-voice",
            "--manifest",
            cli_manifest.to_str().unwrap(),
            "--asr-cmd",
            "/bin/sh -c 'test -s \"$1\" && printf \"hello world\"' _ {wav}",
            "--json",
        ])
        .output()
        .expect("run real-voice with external asr-cmd");
    assert!(
        cli_out.status.success(),
        "expected PASS from external asr-cmd, stderr={}",
        String::from_utf8_lossy(&cli_out.stderr)
    );
    let cli_report: RealVoiceReport =
        serde_json::from_slice(&cli_out.stdout).expect("parse external asr-cmd json");
    assert_eq!(cli_report.status, RealVoiceStatus::Pass);
    assert_eq!(cli_report.aggregate_wer, Some(0.0));
}

#[test]
fn default_real_voice_corpus_manifest_loads_all_human_clips_and_provenance() {
    use lowyap_eval::real_voice::{
        default_corpus_manifest_path, discover_clips, LIVE_CHUNKER_THRESHOLD_MS,
    };
    use lowyap_eval::wav_replay::load_wav_mono_16k;

    let manifest_path = default_corpus_manifest_path();
    assert!(
        manifest_path.exists(),
        "default corpus manifest missing at {}",
        manifest_path.display()
    );
    let corpus_dir = manifest_path
        .parent()
        .expect("default corpus manifest parent");
    let provenance_path = corpus_dir.join("PROVENANCE.md");
    let provenance = fs::read_to_string(&provenance_path).expect("read PROVENANCE.md");
    assert!(provenance.contains("CC-BY-4.0"));
    assert!(provenance.contains("CC0-1.0"));

    let entries = discover_clips(&RealVoiceConfig {
        use_default_corpus: true,
        ..RealVoiceConfig::default()
    })
    .expect("discover default corpus clips");
    assert!(
        (15..=20).contains(&entries.len()),
        "expected 15-20 clips in default corpus, got {}",
        entries.len()
    );

    let mut total_bytes = 0u64;
    let mut clean_en = 0usize;
    let mut accented_en = 0usize;
    let mut multilingual = 0usize;
    let mut silence_breath = 0usize;
    let mut long_chunked = 0usize;
    let mut langs = std::collections::BTreeSet::new();
    let mut has_devanagari = false;
    let mut has_arabic_urdu = false;

    for entry in &entries {
        let wav_name = entry
            .wav
            .file_name()
            .and_then(|n| n.to_str())
            .expect("wav filename");
        assert!(
            provenance.contains(wav_name),
            "PROVENANCE.md must document {wav_name}"
        );
        let meta = fs::metadata(&entry.wav).expect("stat corpus wav");
        total_bytes += meta.len();

        let (samples, summary) = load_wav_mono_16k(&entry.wav).expect("decode corpus wav");
        assert_eq!(summary.source_sample_rate, 16_000);
        assert_eq!(summary.source_channels, 1);
        assert!(!samples.is_empty());
        assert!(samples.iter().all(|s| s.is_finite()));

        if summary.duration_ms >= LIVE_CHUNKER_THRESHOLD_MS {
            assert!(
                summary.duration_ms <= 22_500,
                "long chunked clip should be 14-22s, got {} ms",
                summary.duration_ms
            );
            long_chunked += 1;
        }

        if let Some(ref lang) = entry.language_hint {
            langs.insert(lang.clone());
        }

        match entry.category {
            ClipCategory::CleanEnglish => {
                clean_en += 1;
                assert!(entry
                    .reference_text
                    .as_deref()
                    .is_some_and(|t| !t.trim().is_empty()));
            }
            ClipCategory::AccentedEnglish => {
                accented_en += 1;
                assert!(entry
                    .reference_text
                    .as_deref()
                    .is_some_and(|t| !t.trim().is_empty()));
            }
            ClipCategory::Multilingual | ClipCategory::CodeSwitch => {
                multilingual += 1;
                let text = entry
                    .reference_text
                    .as_deref()
                    .expect("multilingual clip must have reference_text");
                let sp = compute_script_preservation(text, text);
                if sp.expected_scripts.contains(&ScriptKind::Devanagari) {
                    has_devanagari = true;
                }
                if sp.expected_scripts.contains(&ScriptKind::ArabicUrdu) {
                    has_arabic_urdu = true;
                }
            }
            ClipCategory::SilenceBreath => {
                silence_breath += 1;
                assert!(
                    entry.reference_text.is_none(),
                    "silence_breath clip must omit reference_text"
                );
                assert!(
                    summary.rms < 0.02,
                    "silence_breath clip RMS must stay room-tone quiet, got {}",
                    summary.rms
                );
            }
            other => panic!("unexpected category in curated default corpus: {other:?}"),
        }
    }

    assert!(
        total_bytes < 10 * 1024 * 1024,
        "default corpus folder must stay < 10 MiB, got {total_bytes} bytes"
    );
    assert!(
        clean_en >= 3,
        "expected >=3 clean_english clips, got {clean_en}"
    );
    assert!(
        accented_en >= 4,
        "expected >=4 accented_english clips, got {accented_en}"
    );
    assert!(
        multilingual >= 5,
        "expected >=5 multilingual clips, got {multilingual}"
    );
    assert_eq!(silence_breath, 2, "expected 2 silence_breath probes");
    assert!(
        long_chunked >= 2,
        "expected >=2 >=14s live-chunker clips, got {long_chunked}"
    );
    for expected_lang in ["en", "hi", "ur", "es", "de"] {
        assert!(
            langs.contains(expected_lang),
            "default corpus missing language_hint '{expected_lang}' (saw {langs:?})"
        );
    }
    assert!(
        has_devanagari,
        "default corpus must include Devanagari reference"
    );
    assert!(
        has_arabic_urdu,
        "default corpus must include Perso-Arabic Urdu reference"
    );
}

#[test]
fn real_voice_default_corpus_reports_category_breakdown_and_live_chunker_gate() {
    use lowyap_eval::real_voice::{
        discover_clips, evaluate_real_voice_with_timed_asr, AsrStdoutOutput,
    };

    let config = RealVoiceConfig {
        use_default_corpus: true,
        ..RealVoiceConfig::default()
    };
    let entries = discover_clips(&config).expect("discover default corpus");
    let refs_with_audio: Vec<(Vec<f32>, Option<String>, Option<String>)> = entries
        .iter()
        .map(|e| {
            let (samples, _) =
                lowyap_eval::wav_replay::load_wav_mono_16k(&e.wav).expect("load wav");
            (samples, e.language_hint.clone(), e.reference_text.clone())
        })
        .collect();

    let report = evaluate_real_voice_with_timed_asr(
        &config,
        Some(
            |samples: &[f32], lang: Option<&str>| -> Result<AsrStdoutOutput, String> {
                // Pure `silence_breath` probes (`lang == None`) and short edge silence slices.
                if lang.is_none() || (rms(samples) < 0.0085 && samples.len() < 48_000) {
                    return Ok(AsrStdoutOutput {
                        text: String::new(),
                        decode_ms: Some(18.0),
                        ..AsrStdoutOutput::default()
                    });
                }
                // Match clip by language + highest dot-product correlation on prefix samples.
                let matched = refs_with_audio
                    .iter()
                    .filter(|(orig, l, r)| {
                        r.is_some()
                            && l.as_deref() == lang
                            && (orig.len() == samples.len() || orig.len() + 24_000 == samples.len())
                    })
                    .max_by(|(a, _, _), (b, _, _)| {
                        let corr_a: f32 = a
                            .iter()
                            .zip(samples.iter())
                            .take(8_000)
                            .map(|(x, y)| x * y)
                            .sum();
                        let corr_b: f32 = b
                            .iter()
                            .zip(samples.iter())
                            .take(8_000)
                            .map(|(x, y)| x * y)
                            .sum();
                        corr_a.total_cmp(&corr_b)
                    })
                    .and_then(|(_, _, r)| r.clone())
                    .unwrap_or_else(|| "fallback transcript".to_owned());
                Ok(AsrStdoutOutput {
                    text: matched,
                    decode_ms: Some(120.0),
                    simulate_live: Some(true),
                    chunk_count: Some(if samples.len() >= 14 * 16_000 { 2 } else { 1 }),
                })
            },
        ),
    )
    .expect("evaluate default corpus");

    let failed: Vec<_> = report
        .clips
        .iter()
        .filter(|c| !c.passed)
        .map(|c| {
            (
                &c.clip_id,
                c.rms,
                c.wer.as_ref().map(|w| w.rate),
                &c.silence_slice_hallucinations,
                &c.phantom_flags,
                c.perturbations
                    .iter()
                    .filter(|p| !p.passed)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        report.status,
        RealVoiceStatus::Pass,
        "failed clips: {failed:?}"
    );
    assert_eq!(report.clips_evaluated, 18);
    assert_eq!(report.labeled_clips, 16);
    assert_eq!(report.unlabeled_clips, 2);
    assert_eq!(report.long_chunked_clips, 2);
    assert_eq!(report.long_chunked_passed, 2);
    assert_eq!(report.silence_hallucination_clips, 0);
    assert_eq!(report.brand_leak_clips, 0);
    assert_eq!(report.wer_gate_failed_clips, 0);
    assert!(report.clean_english_wer.is_some_and(|w| w <= 0.35));
    assert!(report.accented_english_wer.is_some_and(|w| w <= 0.55));
    assert!(report.multilingual_wer.is_some_and(|w| w <= 0.75));
    assert_eq!(report.latency_ms_p50, Some(120.0));

    let md = format_real_voice_markdown(&report, false);
    assert!(md.contains("### Category Breakdown"));
    assert!(md.contains("`clean_english`"));
    assert!(md.contains("`accented_english`"));
    assert!(md.contains("`multilingual_code_switch`"));
    assert!(md.contains("`silence_breath`"));
    assert!(md.contains("Live-chunker (`>= 14 s` `LiveDecode::Chunked`) clips:** 2/2 passed"));
    assert!(md.contains("(chunked)"));
}

#[test]
fn real_voice_asr_cmd_supports_lang_placeholder_best_ms_and_honest_wer_ceilings() {
    let dir = TempFixtureDir::new("sv98_lang_and_timing");
    let en_wav = dir.file("en_clip.wav");
    let ur_wav = dir.file("ur_clip.wav");
    let auto_wav = dir.file("auto_clip.wav");
    let speech = synth_voiced_speech(1_200, 175.0, 0.42, 701);
    write_wav_i16(&en_wav, &speech, 16_000, 1);
    write_wav_i16(&ur_wav, &speech, 16_000, 1);
    write_wav_i16(&auto_wav, &speech, 16_000, 1);

    // Case 1: `{lang}` + `{wav}` substitution + `best_ms` / `transcribe_ms` extraction + relaxed multilingual WER pass.
    // Reference has 4 Urdu words; ASR returns 2 matching + 2 slightly altered words (WER = 0.50 <= 0.75 multilingual ceiling).
    let pass_manifest = dir.file("pass_manifest.toml");
    fs::write(
        &pass_manifest,
        r#"
[[clips]]
clip_id = "en_clean"
wav = "en_clip.wav"
category = "clean_english"
language_hint = "en"
reference_text = "ship the release today"

[[clips]]
clip_id = "ur_multi"
wav = "ur_clip.wav"
category = "multilingual"
language_hint = "ur"
reference_text = "آج موسم بہت اچھا"

[[clips]]
clip_id = "auto_accented"
wav = "auto_clip.wav"
category = "accented_english"
reference_text = "auto language fallback works"
"#,
    )
    .expect("write pass_manifest");

    let mock_script = dir.file("mock_asr.sh");
    fs::write(
        &mock_script,
        r#"#!/bin/sh
WAV="$1"
LANG_HINT="$2"
test -s "$WAV" || exit 2
echo "[2026-10-27][12:00:00][lowyap_app_lib::cli][INFO] Loaded settings"
case "$LANG_HINT" in
  en)
    printf '{"audio_secs":1.2,"load_ms":900,"transcribe_ms":[55,42],"best_ms":42,"text":"ship the release today"}\n'
    ;;
  ur)
    printf '{"audio_secs":1.2,"transcribe_ms":[30.5,44.5],"text":"آج موسم کافی بہتر"}\n'
    ;;
  auto)
    printf '{"audio_secs":1.2,"best_ms":60,"text":"auto language fallback works"}\n'
    ;;
  *)
    exit 3
    ;;
esac
"#,
    )
    .expect("write mock_asr.sh");

    let config = RealVoiceConfig {
        use_default_corpus: false,
        wav_dir: None,
        manifest: Some(pass_manifest),
        asr_cmd: Some(format!(
            "/bin/sh '{}' {{wav}} {{lang}}",
            mock_script.display()
        )),
        verbose_redacted: false,
        perturbation_seed: DEFAULT_PERTURBATION_SEED,
    };
    let report = run_real_voice_suite(&config).expect("run real-voice suite with lang+json");
    assert_eq!(report.status, RealVoiceStatus::Pass);
    assert_eq!(report.wer_gate_failed_clips, 0);
    assert_eq!(report.clips[0].latency_ms, Some(42.0));
    assert_eq!(report.clips[0].rtf, Some(0.035));
    // Clip 1 omitted `best_ms`, so `transcribe_ms` array sum (30.5 + 44.5 = 75.0 ms) is used.
    assert_eq!(report.clips[1].latency_ms, Some(75.0));
    assert_eq!(report.clips[1].wer_gate_passed, Some(true));
    assert_eq!(report.clips[2].latency_ms, Some(60.0));
    assert_eq!(report.clean_english_wer, Some(0.0));
    assert_eq!(report.accented_english_wer, Some(0.0));
    assert_eq!(report.multilingual_wer, Some(0.5));

    // Case 2: 0.50 WER on `clean_english` exceeds the strict 0.35 ceiling and fails the gate.
    let fail_manifest = dir.file("fail_manifest.toml");
    fs::write(
        &fail_manifest,
        r#"
[[clips]]
clip_id = "en_regressed"
wav = "en_clip.wav"
category = "clean_english"
language_hint = "en"
reference_text = "ship the release today"
"#,
    )
    .expect("write fail_manifest");
    let fail_report = evaluate_real_voice_with_asr(
        &RealVoiceConfig {
            manifest: Some(fail_manifest),
            ..RealVoiceConfig::default()
        },
        Some(|_samples: &[f32]| Ok("ship wrong words now".to_owned())),
    )
    .expect("evaluate regressed clean_english clip");
    assert_eq!(fail_report.status, RealVoiceStatus::Fail);
    assert_eq!(fail_report.wer_gate_failed_clips, 1);
    assert_eq!(fail_report.clips[0].wer_gate_passed, Some(false));

    // Case 3: CLI `--suite real-voice --default-corpus` discovers all 18 bundled WAVs out of the box
    // (exits non-zero when `--asr-cmd` is omitted so CI cannot pass on `Unverified`).
    let cli_default = Command::new(env!("CARGO_BIN_EXE_lowyap-eval"))
        .args(["--suite", "real-voice", "--default-corpus", "--json"])
        .output()
        .expect("run lowyap-eval --default-corpus");
    assert!(
        !cli_default.status.success(),
        "Unverified run without --asr-cmd must exit non-zero"
    );
    let default_report: RealVoiceReport =
        serde_json::from_slice(&cli_default.stdout).expect("parse --default-corpus json");
    assert_eq!(default_report.status, RealVoiceStatus::Unverified);
    assert_eq!(default_report.clips_evaluated, 18);
    assert_eq!(default_report.long_chunked_clips, 2);
}
