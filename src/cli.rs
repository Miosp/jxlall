//! CLI definition; raw clap args validated into a `RunOptions`.

use std::path::PathBuf;

use clap::Parser;

use crate::encode::{Distance, Effort, EncodeSettings, NoiseStrength};

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

    /// Parallel conversions [default: CPU count, at most 128].
    #[arg(short, long)]
    jobs: Option<usize>,

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

    /// Add decode-time film grain (JXL photon noise) matching the ISO in
    /// the source metadata, for images that were denoised before
    /// conversion. Lossy encodes only (-d > 0; JPEGs also need
    /// --force-lossy-jpeg).
    #[arg(long)]
    add_noise: bool,

    /// Grain strength: multiplies the full-frame-equivalent ISO (grain
    /// amplitude grows roughly with its square root).
    #[arg(long, default_value_t = DEFAULT_NOISE_STRENGTH)]
    noise_strength: f32,

    /// Verbose per-file logging.
    #[arg(short, long)]
    verbose: bool,
}

const DEFAULT_NOISE_STRENGTH: f32 = 1.0;

/// Upper bound for `--jobs`: each conversion holds one blocking-pool thread
/// and may need another for file I/O on drivers without native async files;
/// compio's pool caps at 256 threads and spins once exhausted.
const MAX_JOBS: usize = 128;

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
    let noise_strength = NoiseStrength::try_from(args.noise_strength)?;
    if !args.add_noise && args.noise_strength != DEFAULT_NOISE_STRENGTH {
        eprintln!("warning: --noise-strength has no effect without --add-noise");
    }
    if args.add_noise && distance.is_lossless() {
        eprintln!("note: --add-noise needs lossy encoding (-d > 0); no grain will be added");
    } else if args.add_noise && !args.force_lossy_jpeg {
        eprintln!(
            "note: JPEGs recompressed losslessly get no grain; add --force-lossy-jpeg for them"
        );
    }
    if args.jobs.is_some_and(|j| j > MAX_JOBS) {
        eprintln!("note: --jobs capped at {MAX_JOBS}");
    }
    Ok(RunOptions {
        inputs: args.inputs,
        output: args.output,
        settings: EncodeSettings {
            effort,
            distance,
            noise: args.add_noise.then_some(noise_strength),
        },
        force_lossy_jpeg: args.force_lossy_jpeg,
        jobs: args.jobs.unwrap_or_else(default_jobs).clamp(1, MAX_JOBS),
        keep_going: args.keep_going,
        dry_run: args.dry_run,
        recursive: args.recursive,
        pattern: args.pattern,
        delete_originals: args.delete_originals,
        verbose: args.verbose,
    })
}
