//! CLI entrypoint for `lowyap-eval`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use lowyap_eval::{
    evaluate_all, evaluate_comparison, format_comparison_markdown, format_scorecard_markdown,
    format_wav_report_markdown, inspect_wav_pair, PipelineUnderTest,
};
use lowyap_ports::OutputRouteKind;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SuiteArg {
    Baseline,
    ProductionDsp,
    Production,
    #[value(name = "modular-target")]
    ModularTarget,
    Compare,
    All,
}

impl SuiteArg {
    fn profile(self) -> Option<(&'static str, PipelineUnderTest)> {
        match self {
            Self::Baseline => Some(("baseline", PipelineUnderTest::baseline())),
            Self::ProductionDsp => Some(("production-dsp", PipelineUnderTest::production_dsp())),
            Self::Production => Some(("production", PipelineUnderTest::production())),
            Self::ModularTarget => Some(("modular-target", PipelineUnderTest::modular_target())),
            Self::Compare | Self::All => None,
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
    about = "Deterministic audio pipeline evaluation harness and WAV pair inspector"
)]
struct Cli {
    /// Synthetic benchmark suite to execute.
    #[arg(long, value_enum, default_value_t = SuiteArg::Compare)]
    suite: SuiteArg,

    /// Emit machine-readable JSON to stdout.
    #[arg(long)]
    json: bool,

    /// Inspect a local pair of 16 kHz (or resampled) WAV files in read-only mode.
    #[arg(long, num_args = 2, value_names = ["ME_WAV", "OTHERS_WAV"])]
    wav_pair: Option<Vec<PathBuf>>,

    /// Output route assumed when inspecting a WAV pair.
    #[arg(long, value_enum, default_value_t = RouteArg::Speakers)]
    route: RouteArg,

    /// Minimum required overall score (0.0..=10.0) for the target suite; exits non-zero if missed.
    #[arg(long)]
    min_score: Option<f32>,

    /// Optional file path to write the rendered output (no files written unless explicitly passed).
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Some(pair) = cli.wav_pair {
        return run_wav_mode(
            &pair[0],
            &pair[1],
            cli.route.into(),
            cli.json,
            cli.out.as_deref(),
        );
    }

    run_suite_mode(cli.suite, cli.json, cli.min_score, cli.out.as_deref())
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
