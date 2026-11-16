//! CLI definition; raw clap args validated into a `RunOptions`.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::ai::backend::BackendChoice;
use crate::ai::dejpeg::DejpegOptions;
use crate::ai::iqa::{AestheticModel, IqaOptions, TechnicalModel};
use crate::avif::{AvifSettings, Crf, Preset};
use crate::domain::StarThresholds;
use crate::encode::{
    Distance, Effort, EncodeSettings, JxlOption, MaxBitDepth, NoiseIso, NoiseSettings,
    NoiseStrength,
};
use crate::quality::Quality;

const OUTPUT: &str = "Output";
const ENCODING: &str = "Encoding";
const AI: &str = "AI";
const RUN: &str = "Run";

#[derive(Parser, Debug)]
#[command(
    name = "jxlall",
    version,
    about = "Convert photos to JPEG XL or AVIF",
    after_help = "`--help` shows all options and details.",
    after_long_help = ""
)]
struct Args {
    /// Files and/or directories to convert.
    ///
    /// Directories are scanned for the extensions in --pattern; add -r to
    /// descend into subdirectories.
    inputs: Vec<PathBuf>,

    /// Output directory [default: alongside sources].
    ///
    /// A scanned directory's layout is mirrored inside it.
    #[arg(short, long, help_heading = OUTPUT)]
    output: Option<PathBuf>,

    /// Output format.
    ///
    /// AVIF is always lossy (10-bit 4:2:0) and stores wide-gamut sources in a
    /// color space Windows, Chrome and Immich all display.
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Jxl, help_heading = OUTPUT)]
    format: OutputFormat,

    /// Delete source files after successful conversion.
    #[arg(long, help_heading = OUTPUT)]
    delete_originals: bool,

    /// Quality, same scale for both formats [default: 100 (lossless) for JPEG
    /// XL, 85 for AVIF].
    ///
    /// Equal values score alike on SSIMULACRA2. JPEG XL: libjxl's scale, 90 =
    /// distance 1, 100 = lossless. AVIF: mapped to an SVT-AV1 CRF, 85 = CRF
    /// 23, 100 = CRF 2.75. JPEGs are still recompressed losslessly into JPEG
    /// XL whatever the quality (see --reencode-jpeg).
    #[arg(short, long, value_name = "0..=100", help_heading = ENCODING)]
    quality: Option<f32>,

    /// Higher is slower, smaller or better [default: 7; 4 for lossless JPEG
    /// XL].
    ///
    /// AVIF maps it to SVT-AV1 presets 11..=2 (7 = preset 5); `-x preset=N`
    /// reaches the others.
    #[arg(short, long, value_name = "1..=10", help_heading = ENCODING)]
    effort: Option<i64>,

    /// Add film grain at decode time: from the ISO in each file's metadata,
    /// times N (`-g=x1.5`), or a fixed ISO (`-g=3200`).
    ///
    /// For photos that were denoised. A fixed ISO is full-frame equivalent and
    /// applies to every file. Lossy only: JPEG XL needs -q below 100 and
    /// leaves losslessly recompressed JPEGs alone.
    #[arg(
        short,
        long,
        value_name = "xN|ISO",
        num_args = 0..=1,
        require_equals = true,
        help_heading = ENCODING
    )]
    grain: Option<Option<String>>,

    /// Re-encode JPEGs from pixels instead of recompressing them losslessly.
    ///
    /// JPEG XL only; AVIF always re-encodes.
    #[arg(long, help_heading = ENCODING)]
    reencode_jpeg: bool,

    /// Raw encoder setting, repeatable, applied after jxlall's own.
    ///
    /// AVIF: any SvtAv1EncApp long option without dashes (`-x sharpness=2`,
    /// `-x preset=-1`, `-x crf=20.5`). JPEG XL: a libjxl frame setting by its
    /// cjxl name (`-x epf=3`, `-x gaborish=0`, `-x modular=1`, ...).
    #[arg(
        short = 'x',
        value_name = "KEY=VALUE",
        allow_hyphen_values = true,
        hide_short_help = true,
        help_heading = ENCODING
    )]
    encoder_option: Vec<String>,

    /// Store deeper integer images at this many bits (8..=16).
    ///
    /// E.g. for viewers that mishandle 16-bit JXL. Lossless output is then
    /// exact only up to that depth. JPEG XL only.
    #[arg(long, value_name = "BITS", hide_short_help = true, help_heading = ENCODING)]
    max_bit_depth: Option<u8>,

    /// Remove JPEG artifacts first; `--dejpeg=all` treats every input.
    ///
    /// Uses the DeJPEGNet v2 model. ONNX Runtime, a GPU backend and the model
    /// are downloaded on first use.
    #[arg(
        long,
        value_name = "all",
        num_args = 0..=1,
        require_equals = true,
        value_parser = ["all"],
        hide_possible_values = true,
        help_heading = AI
    )]
    dejpeg: Option<Option<String>>,

    /// Store a technical and an aesthetic score (0..1) in XMP.
    ///
    /// The technical score rates detail, the aesthetic one composition.
    /// Optionally pick the models, e.g. `--iqa=koniq10k,aadb`: technical spaq
    /// (default) or koniq10k; aesthetic para (default), aadb, tad66k or
    /// baid. Models (about 90 MB each) are downloaded once.
    #[arg(
        long,
        value_name = "MODEL,MODEL",
        num_args = 0..,
        value_delimiter = ',',
        require_equals = true,
        help_heading = AI
    )]
    iqa: Option<Vec<String>>,

    /// Also write 1..=5 stars from the mean score (implies --iqa).
    ///
    /// Written to `xmp:Rating`. Optionally the minimum mean scores for 2, 3, 4
    /// and 5 stars [default: 0.37,0.48,0.59,0.64].
    #[arg(
        long,
        value_name = "T2,T3,T4,T5",
        num_args = 0..,
        value_delimiter = ',',
        require_equals = true,
        help_heading = AI
    )]
    stars: Option<Vec<f32>>,

    /// Replace a star rating the source already has.
    #[arg(long, hide_short_help = true, help_heading = AI)]
    overwrite_rating: bool,

    /// Backend for --dejpeg and --iqa.
    ///
    /// `auto` prefers a GPU and falls back to the CPU.
    #[arg(
        long,
        value_enum,
        default_value_t = BackendChoice::Auto,
        hide_short_help = true,
        help_heading = AI
    )]
    ai_backend: BackendChoice,

    /// Walk subdirectories recursively.
    #[arg(short, long, help_heading = RUN)]
    recursive: bool,

    /// Extension filter for directory scans, `|`-separated.
    #[arg(
        short,
        long,
        default_value = ".jpg|.jpeg|.png|.webp|.heic|.heif|.avif|.tif|.tiff",
        hide_short_help = true,
        help_heading = RUN
    )]
    pattern: String,

    /// Files converted at once [default: CPU count, at most 128].
    ///
    /// They share all CPU cores between them.
    #[arg(short, long, help_heading = RUN)]
    jobs: Option<usize>,

    /// Report what would be converted, convert nothing.
    #[arg(short = 'n', long, help_heading = RUN)]
    dry_run: bool,

    /// Stop at the first failed file.
    ///
    /// By default the rest is converted and failures are listed at the end.
    #[arg(long, hide_short_help = true, help_heading = RUN)]
    fail_fast: bool,

    /// Log each file's details and timings, one line per step.
    #[arg(short, long, help_heading = RUN)]
    verbose: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Jxl,
    Avif,
}

/// Default star thresholds: minimum mean score for 2, 3, 4 and 5 stars.
const DEFAULT_STARS: [f32; 4] = [0.37, 0.48, 0.59, 0.64];

/// Default effort for lossless JPEG XL. Effort 7 costs 1.5 to 5 times as much
/// for lossless pixels and is often no smaller.
const DEFAULT_LOSSLESS_EFFORT: i64 = 4;
/// Default effort otherwise: lower efforts drop libjxl's perceptual
/// heuristics; for AVIF it is SVT-AV1 preset 5. JPEG recompression barely
/// changes between efforts 3 and 7, so it simply follows the run's effort.
const DEFAULT_EFFORT: i64 = 7;

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
    pub reencode_jpeg: bool,
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

/// `KEY=VALUE` with leading dashes on the key dropped.
fn split_option(kv: &str) -> Result<(String, String), String> {
    kv.split_once('=')
        .map(|(k, v)| (k.trim_start_matches('-'), v))
        .filter(|(k, _)| !k.is_empty())
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| format!("-x expects KEY=VALUE, got `{kv}`"))
}

fn resolve_effort(explicit: Option<i64>, lossless_jxl: bool) -> Result<Effort, String> {
    let default = if lossless_jxl {
        DEFAULT_LOSSLESS_EFFORT
    } else {
        DEFAULT_EFFORT
    };
    Effort::try_from(explicit.unwrap_or(default))
}

/// `-g`: bare (metadata ISO), `xN` (metadata ISO times N) or an ISO.
fn resolve_grain(spec: Option<Option<String>>) -> Result<Option<NoiseSettings>, String> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let bad = |s: &str| format!("-g expects xN (e.g. x1.5) or an ISO (e.g. 3200), got `{s}`");
    let (strength, iso_override) = match spec.as_deref().map(str::trim) {
        None => (1.0, None),
        Some(s) => match s.strip_prefix(['x', 'X', '*']) {
            Some(n) => (n.parse::<f32>().map_err(|_| bad(s))?, None),
            None => (1.0, Some(s.parse::<f32>().map_err(|_| bad(s))?)),
        },
    };
    Ok(Some(NoiseSettings {
        strength: NoiseStrength::try_from(strength)?,
        iso_override: iso_override.map(NoiseIso::try_from).transpose()?,
    }))
}

/// `--iqa` model names: each picks the technical or the aesthetic model.
fn resolve_models(names: &[String]) -> Result<(TechnicalModel, AestheticModel), String> {
    let (mut technical, mut aesthetic) = (None, None);
    for name in names {
        if let Ok(m) = TechnicalModel::from_str(name, true) {
            if technical.replace(m).is_some() {
                return Err(format!("--iqa: more than one technical model (`{name}`)"));
            }
        } else if let Ok(m) = AestheticModel::from_str(name, true) {
            if aesthetic.replace(m).is_some() {
                return Err(format!("--iqa: more than one aesthetic model (`{name}`)"));
            }
        } else {
            return Err(format!(
                "--iqa: unknown model `{name}` (technical: spaq, koniq10k; aesthetic: para, aadb, tad66k, baid)"
            ));
        }
    }
    Ok((
        technical.unwrap_or(TechnicalModel::Spaq),
        aesthetic.unwrap_or(AestheticModel::Para),
    ))
}

fn resolve_iqa(args: &Args) -> Result<Option<IqaOptions>, String> {
    if args.iqa.is_none() && args.stars.is_none() {
        if args.overwrite_rating {
            eprintln!("warning: --overwrite-rating has no effect without --stars");
        }
        return Ok(None);
    }
    let (technical, aesthetic) = resolve_models(args.iqa.as_deref().unwrap_or_default())?;
    let stars = args
        .stars
        .as_ref()
        .map(|t| match t.as_slice() {
            [] => StarThresholds::try_from(DEFAULT_STARS.to_vec()),
            t => StarThresholds::try_from(t.to_vec()),
        })
        .transpose()?;
    if stars.is_none() && args.overwrite_rating {
        eprintln!("warning: --overwrite-rating has no effect without --stars");
    }
    Ok(Some(IqaOptions {
        technical,
        aesthetic,
        stars,
        overwrite_stars: args.overwrite_rating,
    }))
}

/// Settings for one output format; warns about options it ignores.
fn resolve_settings(args: &Args) -> Result<EncodeSettings, String> {
    let avif = args.format == OutputFormat::Avif;
    let quality = Quality::try_from(args.quality.unwrap_or(if avif {
        Quality::AVIF_DEFAULT.get()
    } else {
        Quality::JXL_DEFAULT.get()
    }))?;
    let distance = Distance::try_from(quality.jxl_distance())?;
    let lossless_jxl = !avif && distance.is_lossless();
    let effort = resolve_effort(args.effort, lossless_jxl)?;
    let noise = resolve_grain(args.grain.clone())?;
    let max_bit_depth = args.max_bit_depth.map(MaxBitDepth::try_from).transpose()?;
    let raw = args
        .encoder_option
        .iter()
        .map(|kv| split_option(kv))
        .collect::<Result<Vec<_>, _>>()?;

    let (options, avif) = if avif {
        if max_bit_depth.is_some() {
            eprintln!("warning: --max-bit-depth has no effect with -f avif (always 10-bit)");
        }
        if args.reencode_jpeg {
            eprintln!("warning: --reencode-jpeg has no effect with -f avif (always re-encodes)");
        }
        let settings = AvifSettings {
            crf: Crf::try_from(quality.avif_crf())?,
            preset: Preset::from_effort(effort),
            svt: raw,
        };
        crate::avif::validate_options(&settings)?;
        (Vec::new(), Some(settings))
    } else {
        let options = raw
            .iter()
            .map(|(k, v)| JxlOption::parse(k, v))
            .collect::<Result<Vec<_>, _>>()?;
        crate::encode::validate_options(&options)?;
        if noise.is_some() && distance.is_lossless() {
            eprintln!("warning: -g has no effect on lossless JPEG XL (-q 100)");
        }
        (options, None)
    };
    Ok(EncodeSettings {
        effort,
        quality,
        distance,
        max_bit_depth,
        options,
        noise,
        // The pipeline splits the cores once it knows the file count.
        threads: 1,
        avif,
    })
}

/// Parse and validate; error text is user-facing.
pub fn parse() -> Result<RunOptions, String> {
    let args = Args::parse();
    if args.inputs.is_empty() {
        return Err("no inputs given (see -h)".into());
    }
    let settings = resolve_settings(&args)?;
    let iqa = resolve_iqa(&args)?;
    let dejpeg = args.dejpeg.as_ref().map(|scope| DejpegOptions {
        all: scope.is_some(),
    });
    if dejpeg.is_none() && iqa.is_none() && args.ai_backend != BackendChoice::Auto {
        eprintln!("warning: --ai-backend has no effect without --dejpeg, --iqa or --stars");
    }
    if dejpeg.is_some() && args.delete_originals {
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
        settings,
        reencode_jpeg: args.reencode_jpeg,
        jobs: args.jobs.unwrap_or_else(available_cores).clamp(1, MAX_JOBS),
        keep_going: !args.fail_fast,
        dry_run: args.dry_run,
        recursive: args.recursive,
        pattern: args.pattern,
        delete_originals: args.delete_originals,
        dejpeg,
        ai_backend: args.ai_backend,
        iqa,
        verbose: args.verbose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Args {
        Args::try_parse_from(["jxlall", "in.jpg"].iter().chain(extra)).unwrap()
    }

    fn settings(extra: &[&str]) -> EncodeSettings {
        resolve_settings(&args(extra)).unwrap()
    }

    #[test]
    fn jxl_defaults_to_lossless_effort_4_and_lossy_to_effort_7() {
        let s = settings(&[]);
        assert!(s.avif.is_none() && s.distance.is_lossless());
        assert_eq!(s.effort.value(), 4);
        let s = settings(&["-q", "90"]);
        assert!((s.distance.get() - 1.0).abs() < 1e-6);
        assert_eq!(s.effort.value(), 7);
        assert_eq!(settings(&["-e", "2", "-q", "90"]).effort.value(), 2);
        assert!(resolve_settings(&args(&["-e", "11"])).is_err());
        assert!(resolve_settings(&args(&["-q", "101"])).is_err());
    }

    #[test]
    fn avif_maps_quality_and_effort() {
        let a = settings(&["-f", "avif"]).avif.unwrap();
        assert_eq!(a.crf.get(), 23.0);
        assert_eq!(a.preset.get(), 5);
        let a = settings(&["-f", "avif", "-q", "90", "-e", "10"])
            .avif
            .unwrap();
        assert_eq!(a.crf.get(), 16.25);
        assert_eq!(a.preset.get(), 2);
        assert_eq!(
            settings(&["-f", "avif", "-e", "1"])
                .avif
                .unwrap()
                .preset
                .get(),
            11
        );
    }

    #[test]
    fn encoder_options_go_to_the_chosen_encoder_and_are_validated() {
        let a = settings(&["-f", "avif", "-x", "sharpness=2", "-x", "--tune=4"])
            .avif
            .unwrap();
        assert_eq!(
            a.svt,
            [
                ("sharpness".to_owned(), "2".to_owned()),
                ("tune".to_owned(), "4".to_owned())
            ]
        );
        assert!(resolve_settings(&args(&["-f", "avif", "-x", "no-such-option=1"])).is_err());
        assert!(resolve_settings(&args(&["-f", "avif", "-x", "novalue"])).is_err());
        assert_eq!(settings(&["-q", "80", "-x", "epf=3"]).options.len(), 1);
        assert!(resolve_settings(&args(&["-x", "epf=9"])).is_err());
        assert!(resolve_settings(&args(&["-x", "sharpness=2"])).is_err());
    }

    #[test]
    fn grain_forms() {
        let g = |extra: &[&str]| settings(extra).noise;
        assert!(g(&[]).is_none());
        let n = g(&["-q", "90", "-g"]).unwrap();
        assert_eq!(n.strength.get(), 1.0);
        assert!(n.iso_override.is_none());
        let n = g(&["-q", "90", "--grain=x1.5"]).unwrap();
        assert_eq!(n.strength.get(), 1.5);
        let n = g(&["-q", "90", "-g=3200"]).unwrap();
        assert_eq!(n.iso_override.unwrap().get(), 3200.0);
        assert!(resolve_settings(&args(&["-g=lots"])).is_err());
        assert!(resolve_settings(&args(&["-g=x0"])).is_err());
    }

    #[test]
    fn optional_values_never_swallow_an_input() {
        let parsed = Args::try_parse_from(["jxlall", "-g", "--dejpeg", "--iqa", "a.png"]).unwrap();
        assert_eq!(parsed.inputs, [PathBuf::from("a.png")]);
        assert_eq!(parsed.grain, Some(None));
        assert_eq!(parsed.dejpeg, Some(None));
        assert_eq!(parsed.iqa, Some(Vec::new()));
        let parsed = Args::try_parse_from(["jxlall", "--dejpeg=all", "a.png"]).unwrap();
        assert_eq!(parsed.dejpeg, Some(Some("all".into())));
        assert!(Args::try_parse_from(["jxlall", "--dejpeg=some", "a.png"]).is_err());
    }

    #[test]
    fn iqa_models_and_stars() {
        let iqa = |extra: &[&str]| resolve_iqa(&args(extra)).unwrap();
        assert!(iqa(&[]).is_none());
        let o = iqa(&["--iqa"]).unwrap();
        assert_eq!(
            (o.technical, o.aesthetic),
            (TechnicalModel::Spaq, AestheticModel::Para)
        );
        assert!(o.stars.is_none());
        let o = iqa(&["--iqa=aadb,koniq10k"]).unwrap();
        assert_eq!(
            (o.technical, o.aesthetic),
            (TechnicalModel::Koniq10k, AestheticModel::Aadb)
        );
        assert!(resolve_iqa(&args(&["--iqa=spaq,koniq10k"])).is_err());
        assert!(resolve_iqa(&args(&["--iqa=nope"])).is_err());
        // --stars alone turns rating on, with default or given thresholds.
        assert!(iqa(&["--stars"]).unwrap().stars.is_some());
        assert!(iqa(&["--stars=0.2,0.4,0.6,0.8"]).unwrap().stars.is_some());
        assert!(resolve_iqa(&args(&["--stars=0.8,0.4,0.6,0.2"])).is_err());
    }

    #[test]
    fn dejpeg_and_backend_parse() {
        let parsed = args(&[]);
        assert!(parsed.dejpeg.is_none());
        assert_eq!(parsed.ai_backend, BackendChoice::Auto);
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
    fn short_flags() {
        let parsed = args(&["-n", "-r", "-v", "-j", "3"]);
        assert!(parsed.dry_run && parsed.recursive && parsed.verbose);
        assert_eq!(parsed.jobs, Some(3));
        assert!(!parsed.fail_fast);
    }
}
