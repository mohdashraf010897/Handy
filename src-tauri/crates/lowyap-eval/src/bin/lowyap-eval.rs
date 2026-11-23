//! CLI entrypoint for `lowyap-eval`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use lowyap_eval::{
    evaluate_all, evaluate_comparison, format_comparison_markdown, format_dictation_bench_markdown,
    format_real_voice_markdown, format_scorecard_markdown, format_wav_report_markdown,
    inspect_wav_pair, run_dictation_bench, run_real_voice_suite, PipelineUnderTest,
    RealVoiceConfig, RealVoiceStatus,
};
use lowyap_ports::OutputRouteKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SuiteArg {
    Baseline,
    ProductionDsp,
    Production,
    #[value(name = "modular-target")]
    ModularTarget,
    Compare,
    All,
    #[value(name = "real-voice")]
    RealVoice,
}

impl SuiteArg {
    fn profile(self) -> Option<(&'static str, PipelineUnderTest)> {
        match self {
            Self::Baseline => Some(("baseline", PipelineUnderTest::baseline())),
            Self::ProductionDsp => Some(("production-dsp", PipelineUnderTest::production_dsp())),
            Self::Production => Some(("production", PipelineUnderTest::production())),
            Self::ModularTarget => Some(("modular-target", PipelineUnderTest::modular_target())),
            Self::Compare | Self::All | Self::RealVoice => None,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RouteArg {
    Speakers,
    Headphones,
    Unknown,
}

impl From<RouteArg> for OutputRouteKind {
    fn from(value: RouteArg) -> Self {
        match value {
            RouteArg::Speakers => OutputRouteKind::Speakers,
            RouteArg::Headphones => OutputRouteKind::Headphones,
            RouteArg::Unknown => OutputRouteKind::Unknown,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "lowyap-eval",
    version,
    about = "Deterministic synthetic plumbing harness, WAV pair inspector, and real-human-audio WAV benchmark"
)]
struct Cli {
    /// Benchmark suite to execute (`real-voice` evaluates real human WAVs; other suites check synthetic plumbing invariants only).
    #[arg(long, value_enum, default_value_t = SuiteArg::Compare)]
    suite: SuiteArg,

    /// Emit machine-readable JSON to stdout.
    #[arg(long)]
    json: bool,

    /// Replay offline dictation fixtures and simulate decode latency instead of running a suite.
    #[arg(long, conflicts_with_all = ["wav_pair", "min_score"])]
    dictation_bench: bool,

    /// Inspect a local pair of 16 kHz (or resampled) WAV files in read-only mode.
    #[arg(long, num_args = 2, value_names = ["ME_WAV", "OTHERS_WAV"])]
    wav_pair: Option<Vec<PathBuf>>,

    /// Output route assumed when inspecting a WAV pair.
    #[arg(long, value_enum, default_value_t = RouteArg::Speakers)]
    route: RouteArg,

    /// Load the bundled real-human global accent & multilingual corpus (`fixtures/real-voice/manifest.toml`).
    #[arg(long)]
    default_corpus: bool,

    /// Directory of real human `.wav` recordings (and optional `<stem>.txt` sidecars) for `--suite real-voice`.
    #[arg(long, value_name = "DIR")]
    wav_dir: Option<PathBuf>,

    /// JSON or TOML corpus manifest for `--suite real-voice`.
    #[arg(long, value_name = "PATH")]
    manifest: Option<PathBuf>,

    /// External ASR command for `--suite real-voice` (supports `{wav}` and `{lang}` placeholders, e.g. `lowyap --json --simulate-live --transcribe-file {wav}`).
    #[arg(long, value_name = "BIN")]
    asr_cmd: Option<String>,

    /// Emit redacted diagnostic previews on `--suite real-voice` (raw private sentences are never printed).
    #[arg(long)]
    verbose_redacted: bool,

    /// Minimum required synthetic plumbing index (0.0..=10.0) for synthetic suites; exits non-zero if missed.
    #[arg(long)]
    min_score: Option<f32>,

    /// Optional file path to write the rendered output (no files written unless explicitly passed).
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if cli.dictation_bench {
        let report = run_dictation_bench();
        let rendered = if cli.json {
            serde_json::to_string_pretty(&report)
        } else {
            Ok(format_dictation_bench_markdown(&report))
        };
        let passed = report.replays.iter().all(|row| row.output == row.expected);
        return render_report(rendered, passed, cli.out.as_deref());
    }

    if let Some(pair) = cli.wav_pair {
        return run_wav_mode(
            &pair[0],
            &pair[1],
            cli.route.into(),
            cli.json,
            cli.out.as_deref(),
        );
    }

    if cli.suite == SuiteArg::RealVoice {
        return run_real_voice_mode(
            RealVoiceConfig {
                use_default_corpus: cli.default_corpus,
                wav_dir: cli.wav_dir,
                manifest: cli.manifest,
                asr_cmd: cli.asr_cmd,
                verbose_redacted: cli.verbose_redacted,
                ..RealVoiceConfig::default()
            },
            cli.json,
            cli.out.as_deref(),
        );
    }

    run_suite_mode(cli.suite, cli.json, cli.min_score, cli.out.as_deref())
}

fn run_real_voice_mode(config: RealVoiceConfig, json: bool, out_path: Option<&Path>) -> ExitCode {
    let verbose_redacted = config.verbose_redacted;
    let report = match run_real_voice_suite(&config) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let passed = report.status == RealVoiceStatus::Pass;
    let rendered = if json {
        serde_json::to_string_pretty(&report)
    } else {
        Ok(format_real_voice_markdown(&report, verbose_redacted))
    };
    render_report(rendered, passed, out_path)
}

fn run_wav_mode(
    me_wav: &Path,
    others_wav: &Path,
    route: OutputRouteKind,
    json: bool,
    out_path: Option<&Path>,
) -> ExitCode {
    let report = match inspect_wav_pair(me_wav, others_wav, route) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let rendered = if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("error serializing JSON: {err}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        format_wav_report_markdown(&report)
    };

    if let Some(path) = out_path {
        if let Err(err) = fs::write(path, &rendered) {
            eprintln!("error writing '{}': {err}", path.display());
            return ExitCode::FAILURE;
        }
    }

    println!("{rendered}");
    ExitCode::SUCCESS
}

fn run_suite_mode(
    suite: SuiteArg,
    json: bool,
    min_score: Option<f32>,
    out_path: Option<&Path>,
) -> ExitCode {
    let (rendered, passed) = if let Some((name, mut pipeline)) = suite.profile() {
        let card = pipeline.run_suite(name, min_score);
        let rendered = if json {
            serde_json::to_string_pretty(&card)
        } else {
            Ok(format_scorecard_markdown(&card))
        };
        (rendered, card.passed)
    } else {
        let report = match suite {
            SuiteArg::All => evaluate_all(min_score),
            SuiteArg::Compare => evaluate_comparison(min_score),
            _ => unreachable!("single-suite profiles handled above"),
        };
        let passed = report.passed;
        let rendered = if json {
            serde_json::to_string_pretty(&report)
        } else {
            Ok(format_comparison_markdown(&report))
        };
        (rendered, passed)
    };
    render_report(rendered, passed, out_path)
}

fn render_report(
    rendered: Result<String, serde_json::Error>,
    passed: bool,
    out_path: Option<&Path>,
) -> ExitCode {
    let rendered = match rendered {
        Ok(body) => body,
        Err(err) => {
            eprintln!("error serializing JSON: {err}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(path) = out_path {
        if let Err(err) = fs::write(path, &rendered) {
            eprintln!("error writing '{}': {err}", path.display());
            return ExitCode::FAILURE;
        }
    }

    println!("{rendered}");

    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
