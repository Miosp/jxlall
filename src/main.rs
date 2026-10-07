//! CLI entry: discover inputs, convert to JXL, print summary.

mod cli;
mod decode;
mod domain;
mod encode;
mod format;
mod inputs;
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

    let files = inputs::collect(&opts.inputs, opts.recursive, &opts.pattern);
    if files.is_empty() {
        eprintln!("error: no convertible input files found");
        return ExitCode::FAILURE;
    }
    if opts.dry_run {
        for f in &files {
            println!("{}", f.path.display());
        }
        println!("{} file(s)", files.len());
        return ExitCode::SUCCESS;
    }

    let stats = Arc::new(Stats::default());
    if let Err(e) = pipeline::run(opts, files, Arc::clone(&stats)) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    let s = stats.snapshot();
    println!(
        "{ok} converted, {failed} failed, {in_b} -> {out_b} bytes ({pct:.1}%)",
        ok = s.ok,
        failed = s.failed,
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
