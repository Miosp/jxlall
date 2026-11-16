//! CLI definition; raw clap args validated into a `RunOptions`.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::ai::backend::BackendChoice;
use crate::ai::dejpeg::DejpegOptions;
use crate::ai::iqa::{AestheticModel, IqaOptions, TechnicalModel};
use crate::avif::{AvifSettings, Crf, DEFAULT_CRF, DEFAULT_PRESET, Preset};
use crate::domain::StarThresholds;
use crate::encode::{
    Distance, Effort, EncodeSettings, Epf, MaxBitDepth, NoiseIso, NoiseSettings, NoiseStrength,
};

#[derive(Parser, Debug)]
#[command(name = "jxlall", version, about = "Convert photos to JPEG XL or AVIF")]
struct Args {
    /// Input files and/or directories (directories are scanned; add
    /// --recursive to descend into subdirectories).
    inputs: Vec<PathBuf>,
    /// Output directory (a scanned directory's layout is mirrored)
    /// [default: alongside sources].
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Output format. AVIF is always lossy (see --crf) and stores wide-gamut
    /// sources in a color space Windows, Chrome and Immich all display.
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Jxl)]
    format: OutputFormat,

    /// AVIF quality: SVT-AV1 CRF 0..=70, lower is better, in quarter steps
    /// [default: 23].
    #[arg(long, value_name = "CRF")]
    crf: Option<f32>,

    /// AVIF encoder preset -2..=13, lower is slower and better [default: 5].
    #[arg(long, allow_negative_numbers = true)]
    preset: Option<i32>,

    /// Extra SVT-AV1 option for AVIF as KEY=VALUE, named like the
    /// SvtAv1EncApp long option without dashes (e.g. `sharpness=2`).
    /// Repeatable; applied after jxlall's own settings.
    #[arg(long, value_name = "KEY=VALUE", allow_hyphen_values = true)]
    svt: Vec<String>,

    /// Encoder effort 1..=10 (higher is slower and usually smaller)
    /// [default: 4 with -d 0, otherwise 7].
    #[arg(short, long)]
    effort: Option<i64>,

    /// Lossy target distance for the pixel path (0 = lossless).
    #[arg(short, long, default_value_t = 0.0)]
    distance: f32,

    /// Store integer pixel images deeper than this at this many bits (8..=16),
    /// e.g. for viewers that mishandle 16-bit JXL. With -d 0 the samples are
    /// rounded, so the output is no longer bit-exact. Float images and
    /// recompressed JPEGs are unaffected.
    #[arg(long, value_name = "BITS")]
    max_bit_depth: Option<u8>,

    /// Force the pixel path for JPEG (disable lossless jbrd recompression).
    #[arg(long)]
    force_lossy_jpeg: bool,

    /// Edge-preserving filter strength 0..=3 for lossy pixel encodes. 3 hides
    /// block edges best, at the cost of some softness [default: picked by
    /// libjxl from the distance].
    #[arg(long, value_name = "0..=3")]
    epf: Option<i64>,

    /// Files converted at once, sharing all CPU cores between them
    /// [default: CPU count, at most 128].
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
    #[arg(
        short,
        long,
        default_value = ".jpg|.jpeg|.png|.webp|.heic|.heif|.avif|.tif|.tiff"
    )]
    pattern: String,

    /// Delete source files after successful conversion.
    #[arg(long)]
    delete_originals: bool,

    /// Add decode-time film grain (JXL photon noise) matching the ISO in
    /// the source metadata, for images that were denoised before
    /// conversion. Lossy encodes only (-d > 0; JPEGs also need
    /// --force-lossy-jpeg). With --format avif: AV1 film grain, always.
    #[arg(long)]
    add_noise: bool,

    /// Grain strength: multiplies the full-frame-equivalent ISO (grain
    /// amplitude grows roughly with its square root).
    #[arg(long, default_value_t = DEFAULT_NOISE_STRENGTH)]
    noise_strength: f32,

    /// Grain for every file from this full-frame-equivalent ISO instead of
    /// the source metadata, e.g. for photos with stripped EXIF. Implies
    /// --add-noise.
    #[arg(long, value_name = "ISO")]
    noise_iso: Option<f32>,

    /// Rate each image and store a technical (detail) and an aesthetic
    /// (composition) score, each 0..1, in XMP. Models (about 90 MB each)
    /// are downloaded once to the user cache.
    #[arg(long)]
    iqa: bool,

    /// Model for the technical score [default: spaq].
    #[arg(long, value_enum)]
    iqa_technical: Option<TechnicalModel>,

    /// Model for the aesthetic score [default: para].
    #[arg(long, value_enum)]
    iqa_aesthetic: Option<AestheticModel>,

    /// Also write `xmp:Rating` stars from the mean of both scores: minimum
    /// values for 2, 3, 4 and 5 stars, comma-separated (lower gets 1 star).
    #[arg(long, value_delimiter = ',')]
    iqa_stars: Option<Vec<f32>>,

    /// Replace a star rating the source already has.
    #[arg(long)]
    overwrite_rating: bool,

    /// Remove JPEG artifacts from JPEG inputs with the DeJPEGNet v2 model.
    /// ONNX Runtime, a GPU backend and the model are downloaded on first use.
    /// Other settings are unchanged, so with the default -d 0 restored images
    /// are stored losslessly.
    #[arg(long)]
    dejpeg: bool,

    /// With --dejpeg: process every input, not only JPEGs.
    #[arg(long)]
    dejpeg_all: bool,

    /// Backend for --dejpeg and --iqa. `auto` prefers a GPU and falls back to
    /// the CPU.
    #[arg(long, value_enum, default_value_t = BackendChoice::Auto)]
    ai_backend: BackendChoice,

    /// Log each file's details and timings, one line per step.
    #[arg(short, long)]
    verbose: bool,
}

const DEFAULT_NOISE_STRENGTH: f32 = 1.0;

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Jxl,
    Avif,
}

/// AVIF settings, or a warning for each AVIF option given with JXL output
/// and each JXL option given with AVIF output.
fn resolve_avif(args: &Args) -> Result<Option<AvifSettings>, String> {
    let avif_given = args.crf.is_some() || args.preset.is_some() || !args.svt.is_empty();
    if args.format == OutputFormat::Jxl {
        if avif_given {
            eprintln!("warning: --crf, --preset and --svt have no effect without --format avif");
        }
        return Ok(None);
    }
    let jxl_only = [
        (args.effort.is_some(), "--effort"),
        (args.distance != 0.0, "--distance"),
        (args.max_bit_depth.is_some(), "--max-bit-depth"),
        (args.epf.is_some(), "--epf"),
        (args.force_lossy_jpeg, "--force-lossy-jpeg"),
    ];
    for (_, flag) in jxl_only.iter().filter(|(given, _)| *given) {
        eprintln!("warning: {flag} has no effect with --format avif");
    }
    let svt = args
        .svt
        .iter()
        .map(|kv| {
            kv.split_once('=')
                .filter(|(k, _)| !k.is_empty())
                .map(|(k, v)| (k.trim_start_matches('-').to_owned(), v.to_owned()))
                .ok_or_else(|| format!("--svt expects KEY=VALUE, got `{kv}`"))
        })
        .collect::<Result<_, _>>()?;
    Ok(Some(AvifSettings {
        crf: Crf::try_from(args.crf.unwrap_or(DEFAULT_CRF))?,
        preset: Preset::try_from(args.preset.unwrap_or(DEFAULT_PRESET))?,
        svt,
    }))
}

/// Default effort with `-d 0`. Effort 7 costs 1.5 to 5 times as much for
/// lossless pixels and is often no smaller.
const DEFAULT_LOSSLESS_EFFORT: i64 = 4;
/// Default effort with `-d` above 0, where lower efforts drop perceptual
/// heuristics. JPEG recompression barely changes between efforts 3 and 7, so
/// it simply follows the run's effort.
const DEFAULT_LOSSY_EFFORT: i64 = 7;

fn resolve_effort(explicit: Option<i64>, distance: f32) -> Result<Effort, String> {
    let default = if distance == 0.0 {
        DEFAULT_LOSSLESS_EFFORT
    } else {
        DEFAULT_LOSSY_EFFORT
    };
    Effort::try_from(explicit.unwrap_or(default))
}

/// Upper bound for `--jobs`: each conversion holds one blocking-pool thread
/// and may need another for file I/O on drivers without native async files;
/// compio's pool caps at 256 threads and spins once exhausted.
const MAX_JOBS: usize = 128;

pub fn available_cores() -> usize {
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
    pub iqa: Option<IqaOptions>,
    pub dejpeg: Option<DejpegOptions>,
    pub ai_backend: BackendChoice,
    pub verbose: bool,
}

/// Parse and validate; error text is user-facing.
pub fn parse() -> Result<RunOptions, String> {
    let args = Args::parse();
    if args.inputs.is_empty() {
        return Err("no inputs given".into());
    }
    let avif = resolve_avif(&args)?;
    let effort = resolve_effort(args.effort, args.distance)?;
    let distance = Distance::try_from(args.distance)?;
    let max_bit_depth = args.max_bit_depth.map(MaxBitDepth::try_from).transpose()?;
    let epf = args.epf.map(Epf::try_from).transpose()?;
    if avif.is_some() {
        // JXL-only options were reported by resolve_avif.
    } else if epf.is_some() && distance.is_lossless() {
        eprintln!("warning: --epf has no effect with lossless encoding (-d 0)");
    } else if epf.is_some() && !args.force_lossy_jpeg && !args.dejpeg {
        eprintln!(
            "note: JPEGs recompressed losslessly are not filtered; add --force-lossy-jpeg for them"
        );
    }
    let noise_strength = NoiseStrength::try_from(args.noise_strength)?;
    let noise_iso = args.noise_iso.map(NoiseIso::try_from).transpose()?;
    let add_noise = args.add_noise || noise_iso.is_some();
    if !add_noise && args.noise_strength != DEFAULT_NOISE_STRENGTH {
        eprintln!("warning: --noise-strength has no effect without --add-noise");
    }
    if avif.is_some() {
        // AVIF is always lossy and decodes every input to pixels.
    } else if add_noise && distance.is_lossless() {
        eprintln!(
            "note: grain (--add-noise, --noise-iso) needs lossy encoding (-d > 0); none will be added"
        );
    } else if add_noise && !args.force_lossy_jpeg && !args.dejpeg {
        eprintln!(
            "note: JPEGs recompressed losslessly get no grain; add --force-lossy-jpeg for them"
        );
    }
    let stars = args.iqa_stars.map(StarThresholds::try_from).transpose()?;
    let iqa_extras = args.iqa_technical.is_some() || args.iqa_aesthetic.is_some();
    if !args.iqa && (iqa_extras || stars.is_some() || args.overwrite_rating) {
        eprintln!("warning: --iqa-* and --overwrite-rating options have no effect without --iqa");
    } else if stars.is_none() && args.overwrite_rating {
        eprintln!("warning: --overwrite-rating has no effect without --iqa-stars");
    }
    if !args.dejpeg && args.dejpeg_all {
        eprintln!("warning: --dejpeg-all has no effect without --dejpeg");
    }
    if !args.dejpeg && !args.iqa && args.ai_backend != BackendChoice::Auto {
        eprintln!("warning: --ai-backend has no effect without --dejpeg or --iqa");
    }
    if args.dejpeg && args.delete_originals {
        eprintln!(
            "warning: --dejpeg with --delete-originals: the original JPEGs cannot be restored from the output"
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
            max_bit_depth,
            epf,
            noise: add_noise.then_some(NoiseSettings {
                strength: noise_strength,
                iso_override: noise_iso,
            }),
            // The pipeline splits the cores once it knows the file count.
            threads: 1,
            avif,
        },
        force_lossy_jpeg: args.force_lossy_jpeg,
        jobs: args.jobs.unwrap_or_else(available_cores).clamp(1, MAX_JOBS),
        keep_going: args.keep_going,
        dry_run: args.dry_run,
        recursive: args.recursive,
        pattern: args.pattern,
        delete_originals: args.delete_originals,
        dejpeg: args.dejpeg.then_some(DejpegOptions {
            all: args.dejpeg_all,
        }),
        ai_backend: args.ai_backend,
        iqa: args.iqa.then(|| IqaOptions {
            technical: args.iqa_technical.unwrap_or(TechnicalModel::Spaq),
            aesthetic: args.iqa_aesthetic.unwrap_or(AestheticModel::Para),
            stars,
            overwrite_stars: args.overwrite_rating,
        }),
        verbose: args.verbose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Args {
        Args::try_parse_from(["jxlall", "in.jpg"].iter().chain(extra)).unwrap()
    }

    #[test]
    fn dejpeg_is_off_by_default_and_backend_defaults_to_auto() {
        let parsed = args(&[]);
        assert!(!parsed.dejpeg && !parsed.dejpeg_all);
        assert_eq!(parsed.ai_backend, BackendChoice::Auto);
    }

    #[test]
    fn backend_names_parse() {
        for (name, want) in [
            ("auto", BackendChoice::Auto),
            ("tensorrt-rtx", BackendChoice::TensorrtRtx),
            ("webgpu", BackendChoice::Webgpu),
            ("cpu", BackendChoice::Cpu),
        ] {
            assert_eq!(args(&["--ai-backend", name]).ai_backend, want);
        }
    }

    #[test]
    fn iqa_stars_stops_at_the_next_argument() {
        let parsed =
            Args::try_parse_from(["jxlall", "--iqa-stars", "0.2,0.4,0.6,0.8", "a.png"]).unwrap();
        assert_eq!(parsed.iqa_stars.unwrap(), [0.2, 0.4, 0.6, 0.8]);
        assert_eq!(parsed.inputs, [std::path::PathBuf::from("a.png")]);
    }

    #[test]
    fn default_effort_depends_on_distance_and_an_explicit_one_wins() {
        let effort = |extra: &[&str]| {
            let parsed = args(extra);
            resolve_effort(parsed.effort, parsed.distance)
                .unwrap()
                .value()
        };
        assert_eq!(effort(&[]), 4);
        assert_eq!(effort(&["-d", "1"]), 7);
        assert_eq!(effort(&["-e", "9"]), 9);
        assert_eq!(effort(&["-e", "2", "-d", "1"]), 2);
    }

    #[test]
    fn out_of_range_effort_is_rejected() {
        assert!(resolve_effort(Some(11), 0.0).is_err());
    }

    #[test]
    fn epf_is_unset_by_default_and_out_of_range_is_rejected() {
        assert_eq!(args(&[]).epf, None);
        assert_eq!(args(&["--epf", "3"]).epf, Some(3));
        assert!(Epf::try_from(4).is_err());
        assert!(Epf::try_from(-1).is_err());
    }

    #[test]
    fn dejpeg_leaves_distance_alone() {
        assert_eq!(args(&["--dejpeg"]).distance, 0.0);
    }

    #[test]
    fn jxl_is_default_and_avif_gets_defaults() {
        assert!(resolve_avif(&args(&[])).unwrap().is_none());
        let avif = resolve_avif(&args(&["-f", "avif"])).unwrap().unwrap();
        assert_eq!(avif.crf.get(), DEFAULT_CRF);
        assert_eq!(avif.preset.get(), DEFAULT_PRESET);
        assert!(avif.svt.is_empty());
    }

    #[test]
    fn avif_options_parse_and_validate() {
        let avif = resolve_avif(&args(&[
            "--format",
            "avif",
            "--crf",
            "27.25",
            "--preset",
            "-1",
            "--svt",
            "sharpness=2",
            "--svt",
            "--tune=4",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(avif.crf.get(), 27.25);
        assert_eq!(avif.preset.get(), -1);
        assert_eq!(
            avif.svt,
            [
                ("sharpness".to_owned(), "2".to_owned()),
                ("tune".to_owned(), "4".to_owned())
            ]
        );
        assert!(resolve_avif(&args(&["-f", "avif", "--crf", "71"])).is_err());
        assert!(resolve_avif(&args(&["-f", "avif", "--svt", "novalue"])).is_err());
    }
}
