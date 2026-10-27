//! CLI entry: discover inputs, convert to JXL, print summary.

mod cli;
mod decode;
mod domain;
mod encode;
mod format;
mod inputs;
mod iqa;
mod pipeline;
mod sniff;
mod stats;

use std::process::ExitCode;
use std::sync::Arc;

use stats::{Bytes, Stats};

fn main() -> ExitCode {
    let opts = match cli::parse() {
        Ok(opts) => opts,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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

    let rater = match opts.iqa.map(iqa::Rater::load).transpose() {
        Ok(rater) => rater.map(Arc::new),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let total = files.len() as u64;
    let stats = Arc::new(Stats::default());
    if let Err(e) = pipeline::run(opts, files, Arc::clone(&stats), rater) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    // Without --keep-going a failure stops scheduling the rest.
    let skipped = total - stats.done();
    // Counted only now: a failure stops scheduling without --keep-going,
    // but a missing input must not prevent converting the others.
    stats.record_fails(input_errors);

    let s = stats.snapshot();
    println!(
        "{ok} converted, {failed} failed{skipped}, {in_b} -> {out_b} ({pct:.1}%)",
        ok = s.ok,
        failed = s.failed,
        skipped = if skipped > 0 {
            format!(", {skipped} skipped")
        } else {
            String::new()
        },
        in_b = Bytes(s.bytes_in),
        out_b = Bytes(s.bytes_out),
        pct = if s.bytes_in > 0 {
            s.bytes_out as f64 / s.bytes_in as f64 * 100.0
        } else {
            0.0
        },
    );
    if s.failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
