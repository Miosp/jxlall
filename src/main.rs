//! CLI entry: discover inputs, convert to JXL, print summary.

#[macro_use]
mod progress;

mod ai;
mod cli;
mod decode;
mod domain;
mod encode;
mod format;
mod inputs;
mod pipeline;
mod sniff;
mod stats;

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use ai::dejpeg::Restorer;
use ai::iqa::Rater;
use stats::{Report, Stats};

fn main() -> ExitCode {
    let opts = match cli::parse() {
        Ok(opts) => opts,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    progress::set_verbose(opts.verbose);
    let (files, input_errors) = inputs::collect(&opts.inputs, opts.recursive, &opts.pattern);
    if files.is_empty() {
        eprintln!("error: no convertible input files found");
        return ExitCode::FAILURE;
    }
    if opts.dry_run {
        for f in &files {
            println!("{}", f.path.display());
        }
        println!("{} file(s)", files.len());
        return if input_errors > 0 {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }

    let backend = opts.ai_backend;
    let load_error = |e: String| {
        eprintln!("error: {e}");
        ExitCode::FAILURE
    };
    let restorer = match opts.dejpeg.map(|o| Restorer::load(o, backend)).transpose() {
        Ok(restorer) => restorer.map(Arc::new),
        Err(e) => return load_error(e),
    };
    let rater = match opts.iqa.map(|o| Rater::load(o, backend)).transpose() {
        Ok(rater) => rater.map(Arc::new),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let total_files = files.len() as u64;
    let stats = Arc::new(Stats::default());
    let summary = match pipeline::run(opts, files, Arc::clone(&stats), restorer, rater) {
        Ok(summary) => summary,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let report = Report {
        stats: &stats,
        total_files,
        elapsed: summary.elapsed,
        settings: summary.settings,
        notes: progress::notes(),
        input_errors,
    };
    // A closed stdout must not turn a finished run into a panic.
    let _ = write!(std::io::stdout(), "{report}");
    if report.succeeded() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
