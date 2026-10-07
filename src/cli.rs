//! CLI definition; raw clap args validated into a `RunOptions`.

use std::path::PathBuf;

use clap::Parser;

use crate::encode::{Distance, Effort, EncodeSettings};

#[derive(Parser, Debug)]
#[command(name = "photo-convert", version, about = "Convert photos to JPEG XL")]
struct Args {
    /// Input files and/or directories (directories are scanned; add
    /// --recursive to descend into subdirectories).
    inputs: Vec<PathBuf>,
    /// Output directory (a scanned directory's layout is mirrored)
    /// [default: alongside sources].
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Encoder effort 1..=10 (faster → smaller files).
    #[arg(short, long, default_value_t = 7)]
    effort: i64,

    /// Lossy target distance for the pixel path (0 = lossless).
    #[arg(short, long, default_value_t = 0.0)]
    distance: f32,

    /// Force the pixel path for JPEG (disable lossless jbrd recompression).
    #[arg(long)]
    force_lossy_jpeg: bool,

    /// Parallel conversions.
    #[arg(short, long, default_value_t = default_jobs())]
    jobs: usize,

    /// Continue on errors, report at end.
    #[arg(long)]
    keep_going: bool,

    /// Dry run: report what would be converted.
    #[arg(long)]
    dry_run: bool,

    /// Walk subdirectories recursively.
    #[arg(short, long)]
    recursive: bool,

    /// Extension filter for directory scans, `|`-separated.
    #[arg(short, long, default_value = ".jpg|.jpeg|.png|.webp|.heic|.heif|.avif|.tif|.tiff")]
    pattern: String,

    /// Delete source files after successful conversion.
    #[arg(long)]
    delete_originals: bool,

    /// Verbose per-file logging.
    #[arg(short, long)]
    verbose: bool,
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Validated run configuration.
pub struct RunOptions {
    pub inputs: Vec<PathBuf>,
    pub output: Option<PathBuf>,
    pub settings: EncodeSettings,
    pub force_lossy_jpeg: bool,
    pub jobs: usize,
    /// Continue scheduling after a failure instead of stopping.
    pub keep_going: bool,
    pub dry_run: bool,
    pub recursive: bool,
    pub pattern: String,
    pub delete_originals: bool,
    pub verbose: bool,
}

/// Parse and validate; error text is user-facing.
pub fn parse() -> Result<RunOptions, String> {
    let args = Args::parse();
    if args.inputs.is_empty() {
        return Err("no inputs given".into());
    }
    let effort = Effort::try_from(args.effort)?;
    let distance = Distance::try_from(args.distance)?;
    Ok(RunOptions {
        inputs: args.inputs,
        output: args.output,
        settings: EncodeSettings { effort, distance },
        force_lossy_jpeg: args.force_lossy_jpeg,
        jobs: args.jobs,
        keep_going: args.keep_going,
        dry_run: args.dry_run,
        recursive: args.recursive,
        pattern: args.pattern,
        delete_originals: args.delete_originals,
        verbose: args.verbose,
    })
}
