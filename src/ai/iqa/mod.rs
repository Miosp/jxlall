//! No-reference image rating with Charm (CVPR 2025) models: a technical
//! (detail) and an aesthetic (composition) score per image. Charm sees the
//! whole frame at its aspect ratio and keeps the sharpest regions at full
//! resolution, so intentional blur (bokeh) is judged in context. Models are
//! ONNX exports, fetched once into the per-user cache.

mod input;
mod npy;
mod resample;
mod tokenizer;

use std::sync::{Mutex, PoisonError};

use super::ShapeRange;
use super::artifacts::Remote;
use super::backend::{Backend, BackendChoice};
use super::runtime::{self, Model};
use super::{Input, TensorData};
use crate::domain::{ImageFrame, Orientation, Rating, Score, StarThresholds};
use tokenizer::{PATCH, PosTable, Tokens};

/// Model that scores detail and technical quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TechnicalModel {
    /// SPAQ: smartphone photos at high resolution.
    Spaq,
    /// KonIQ-10k: web images at 1024×768.
    Koniq10k,
}

/// Model that scores aesthetics and composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AestheticModel {
    /// PARA: personal photos, scored 1 to 5.
    Para,
    /// AADB: photos rated for aesthetics.
    Aadb,
    /// TAD66k: theme-aware aesthetics.
    Tad66k,
    /// BAID: artistic images.
    Baid,
}

/// One exported checkpoint.
struct ModelSpec {
    /// Written to XMP next to the score. Names the checkpoint's first export:
    /// later exports score the same, so existing ratings stay comparable.
    id: &'static str,
    model: Remote,
    pos_table: &'static Remote,
    /// Token budget the checkpoint was trained with (CLS included).
    max_tokens: usize,
    /// Raw score range, mapped to 0..=1.
    score_range: (f32, f32),
}

/// Larger images are downscaled first, as in the reference for SPAQ and
/// PARA; the token count would otherwise grow with the pixel count.
const MAX_IMAGE_SIDE: usize = 1024;
/// Share of the technical score in the star rating.
const TECHNICAL_WEIGHT: f32 = 0.5;
const UNIT_RANGE: (f32, f32) = (0.0, 1.0);
const RGB: usize = 3;
/// Image for the GPU check at load time: the largest token count real photos
/// reach, so a backend that lacks the memory is rejected up front.
const PROBE_SIDE: usize = MAX_IMAGE_SIDE;
const PROBE_GRAY: u8 = 128;
/// CLS plus one patch: the head averages the patches, so it needs at least one.
const MIN_TOKENS: usize = 2;

/// File URL in the export repository, pinned to a revision.
macro_rules! hf_url {
    ($($part:literal),+) => {
        concat!(
            "https://huggingface.co/Miosp/charm-onnx/resolve/db910932759d0c18954959b69027eaac8f12fe36/",
            $($part),+
        )
    };
}

static DINOV2_SMALL_POS: Remote = Remote {
    file_name: "charm-db91093-dinov2-small-pos.npy",
    url: hf_url!("spaq/pos_embed.npy"),
    sha256: "79732feda3385a28bab9505eb3aacd083fec0789dfd8d35e4e2a7e1f730bb42d",
    size: 2_104_448,
};

macro_rules! small_model {
    ($name:literal, $sha:literal, $size:literal, $max_tokens:literal, $range:expr) => {
        ModelSpec {
            id: concat!("charm-", $name, "@cbcd14f"),
            model: Remote {
                file_name: concat!("charm-db91093-", $name, ".onnx"),
                url: hf_url!($name, "/model.onnx"),
                sha256: $sha,
                size: $size,
            },
            pos_table: &DINOV2_SMALL_POS,
            max_tokens: $max_tokens,
            score_range: $range,
        }
    };
}

static SPAQ: ModelSpec = small_model!(
    "spaq",
    "818a2b0a15effcc0911454b955e0676313c50066b4a2ad040fcbdff0158cbc56",
    86_299_600,
    1025,
    UNIT_RANGE
);
static KONIQ10K: ModelSpec = small_model!(
    "koniq10k",
    "a283e1120233be32aad5610976e4404c23f3b70d7b4d5dba609ff7ae2ae3d708",
    86_299_600,
    1025,
    UNIT_RANGE
);
static PARA: ModelSpec = small_model!(
    "para",
    "67936b0c49e2305f5b8ec5853782a0cd5751fcc6493e074f55a6a9a8e5fa7d12",
    86_324_441,
    1025,
    (1.0, 5.0)
);
static AADB: ModelSpec = small_model!(
    "aadb",
    "1ae118043b3b2602bdff657b420c6c58b09837951740cdc59c0ab459eceee17b",
    86_299_600,
    1025,
    UNIT_RANGE
);
static TAD66K: ModelSpec = small_model!(
    "tad66k",
    "f800873266c0c657eb9fe08ab52138eab80d36554afb0d0068ccd3d71a4ac0a0",
    86_299_594,
    769,
    (1.0, 10.0)
);
static BAID: ModelSpec = small_model!(
    "baid",
    "96059d197e5cff31980eded3b545a4932ac459d340080c189c36cc4ec2ae5bee",
    86_299_600,
    1025,
    UNIT_RANGE
);

impl TechnicalModel {
    fn spec(self) -> &'static ModelSpec {
        match self {
            Self::Spaq => &SPAQ,
            Self::Koniq10k => &KONIQ10K,
        }
    }
}

impl AestheticModel {
    fn spec(self) -> &'static ModelSpec {
        match self {
            Self::Para => &PARA,
            Self::Aadb => &AADB,
            Self::Tad66k => &TAD66K,
            Self::Baid => &BAID,
        }
    }
}

/// User choices for rating.
#[derive(Debug, Clone, Copy)]
pub struct IqaOptions {
    pub technical: TechnicalModel,
    pub aesthetic: AestheticModel,
    /// `Some`: also write `xmp:Rating` stars.
    pub stars: Option<StarThresholds>,
    pub overwrite_stars: bool,
}

/// Loaded models plus the rating policy, shared by all conversions.
pub struct Rater {
    technical: Scorer,
    aesthetic: Scorer,
    stars: Option<StarThresholds>,
    overwrite_stars: bool,
}

struct Scorer {
    spec: &'static ModelSpec,
    pos: PosTable,
    /// Runs take turns across all models, see `Model::run`.
    model: Mutex<Model>,
}

impl Scorer {
    fn load(spec: &'static ModelSpec, backend: BackendChoice) -> Result<Self, String> {
        let pos_path = runtime::ensure_file(spec.pos_table)?;
        let pos_bytes =
            std::fs::read(&pos_path).map_err(|e| format!("{}: {e}", pos_path.display()))?;
        let (values, shape) =
            npy::read_f32(&pos_bytes).map_err(|e| format!("{}: {e}", pos_path.display()))?;
        let dim = *shape.last().ok_or("empty position table shape")?;
        let pos = PosTable::new(&values, dim)?;
        let probe = probe_tokens(&pos, spec.max_tokens)?;
        let model = Model::load(
            spec.id,
            &spec.model,
            backend,
            &token_shape_ranges(probe.scale_ids.len(), dim),
            |model| {
                if model.backend() == Backend::Cpu {
                    return Ok(());
                }
                raw_score(model, &probe).map(|_| ())
            },
        )?;
        Ok(Self {
            spec,
            pos,
            model: Mutex::new(model),
        })
    }

    fn raw(&self, tokens: &Tokens) -> Result<f32, String> {
        let mut model = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        raw_score(&mut model, tokens)
    }

    /// Score mapped to 0..=1.
    fn score(&self, tokens: &Tokens) -> Result<Score, String> {
        let raw = self.raw(tokens)?;
        let (lo, hi) = self.spec.score_range;
        Ok(Score {
            value: ((raw - lo) / (hi - lo)).clamp(0.0, 1.0),
            model: self.spec.id,
        })
    }

    fn tokenize(&self, image: &resample::Rgb8) -> Result<Tokens, String> {
        tokenizer::tokenize(image, &self.pos, self.spec.max_tokens)
    }
}

/// Every token count a photo can reach, for backends that compile for a range.
/// `largest` is the probe image's count.
fn token_shape_ranges(largest: usize, pos_dim: usize) -> [ShapeRange; 3] {
    let range = |input, shape: &dyn Fn(usize) -> Vec<usize>| ShapeRange {
        input,
        min: shape(MIN_TOKENS),
        opt: shape(largest),
        max: shape(largest),
    };
    [
        range("patches", &|n| vec![1, n, RGB, PATCH, PATCH]),
        range("pos_embed", &|n| vec![1, n, pos_dim]),
        range("scale_ids", &|n| vec![1, n]),
    ]
}

fn probe_tokens(pos: &PosTable, max_tokens: usize) -> Result<Tokens, String> {
    let image = resample::Rgb8 {
        width: PROBE_SIDE,
        height: PROBE_SIDE,
        data: vec![PROBE_GRAY; RGB * PROBE_SIDE * PROBE_SIDE],
    };
    tokenizer::tokenize(&image, pos, max_tokens)
}

/// Raw `score` output for one image.
fn raw_score(model: &mut Model, tokens: &Tokens) -> Result<f32, String> {
    let n = tokens.scale_ids.len();
    let dim = tokens.pos_embed.len() / n;
    let inputs = [
        Input {
            name: "patches",
            shape: vec![1, n, RGB, PATCH, PATCH],
            data: TensorData::F32(&tokens.patches),
        },
        Input {
            name: "pos_embed",
            shape: vec![1, n, dim],
            data: TensorData::F32(&tokens.pos_embed),
        },
        Input {
            name: "scale_ids",
            shape: vec![1, n],
            data: TensorData::I64(&tokens.scale_ids),
        },
    ];
    let score = model.run(&inputs, "score")?;
    match score.first() {
        Some(raw) if raw.is_finite() => Ok(*raw),
        Some(raw) => Err(format!("model returned {raw}")),
        None => Err("model returned no score".into()),
    }
}

impl Rater {
    /// Fetch the models if they are not cached yet, then load them.
    pub fn load(opts: IqaOptions, backend: BackendChoice) -> Result<Self, String> {
        Ok(Self {
            technical: Scorer::load(opts.technical.spec(), backend)?,
            aesthetic: Scorer::load(opts.aesthetic.spec(), backend)?,
            stars: opts.stars,
            overwrite_stars: opts.overwrite_stars,
        })
    }

    pub fn rate(&self, frame: &ImageFrame, orientation: Orientation) -> Result<Rating, String> {
        let iqa = |e: String| format!("IQA: {e}");
        let image = tokenizer::fit(input::display_rgb8(frame, orientation), MAX_IMAGE_SIDE);
        let tokens = self.technical.tokenize(&image).map_err(iqa)?;
        let technical = self.technical.score(&tokens).map_err(iqa)?;
        let (t, a) = (self.technical.spec, self.aesthetic.spec);
        let same_tokens = t.max_tokens == a.max_tokens && std::ptr::eq(t.pos_table, a.pos_table);
        let aesthetic = if same_tokens {
            self.aesthetic.score(&tokens)
        } else {
            self.aesthetic
                .score(&self.aesthetic.tokenize(&image).map_err(iqa)?)
        }
        .map_err(iqa)?;
        let combined =
            TECHNICAL_WEIGHT * technical.value + (1.0 - TECHNICAL_WEIGHT) * aesthetic.value;
        Ok(Rating {
            technical,
            aesthetic,
            stars: self.stars.map(|t| t.stars_for(combined)),
            overwrite_stars: self.overwrite_stars,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use clap::ValueEnum;

    use super::tokenizer::tests::golden_cases;
    use super::*;

    impl ModelSpec {
        /// Directory name in the export repository, as in the golden file.
        pub(super) fn dataset(&self) -> &'static str {
            let name = self.id.split('@').next().unwrap();
            name.strip_prefix("charm-").unwrap()
        }
    }

    pub(super) fn all_specs() -> Vec<&'static ModelSpec> {
        let technical = TechnicalModel::value_variants().iter().map(|m| m.spec());
        let aesthetic = AestheticModel::value_variants().iter().map(|m| m.spec());
        technical.chain(aesthetic).collect()
    }

    #[test]
    fn token_ranges_match_the_model_inputs() {
        let [patches, pos_embed, scale_ids] = token_shape_ranges(1562, 384);
        assert_eq!(patches.input, "patches");
        assert_eq!(patches.min, [1, MIN_TOKENS, RGB, PATCH, PATCH]);
        assert_eq!(patches.max, [1, 1562, RGB, PATCH, PATCH]);
        assert_eq!(pos_embed.opt, [1, 1562, 384]);
        assert_eq!(scale_ids.max, [1, 1562]);
    }

    /// The range is taken from the probe, so no photo may exceed the probe.
    #[test]
    fn every_token_count_fits_the_probe_range() {
        const POS_DIM: usize = 4;
        const POS_GRID: usize = 37;
        let values = vec![0.0; (POS_GRID * POS_GRID + 1) * POS_DIM];
        let pos = PosTable::new(&values, POS_DIM).unwrap();
        // One patch; exactly one budget's patches; every common aspect ratio.
        let sizes = [
            (1, 1),
            (350, 574),
            (1024, 1024),
            (1024, 768),
            (1024, 683),
            (1024, 576),
            (768, 1024),
            (1024, 1000),
            (1024, 300),
        ];
        for spec in all_specs() {
            let largest = probe_tokens(&pos, spec.max_tokens).unwrap().scale_ids.len();
            let [_, _, scale_ids] = token_shape_ranges(largest, POS_DIM);
            let (min, max) = (scale_ids.min[1], scale_ids.max[1]);
            for (width, height) in sizes {
                let image = resample::Rgb8 {
                    width,
                    height,
                    data: vec![PROBE_GRAY; RGB * width * height],
                };
                let tokens = tokenizer::tokenize(&image, &pos, spec.max_tokens)
                    .unwrap()
                    .scale_ids
                    .len();
                assert!((min..=max).contains(&tokens), "{width}x{height}: {tokens}");
            }
        }
    }

    #[test]
    fn probe_image_tokenizes_for_every_token_budget() {
        const POS_DIM: usize = 4;
        const POS_GRID: usize = 37;
        let values = vec![0.0; (POS_GRID * POS_GRID + 1) * POS_DIM];
        let pos = PosTable::new(&values, POS_DIM).unwrap();
        for spec in all_specs() {
            assert!(
                !probe_tokens(&pos, spec.max_tokens)
                    .unwrap()
                    .scale_ids
                    .is_empty()
            );
        }
    }

    /// Same images and models as `testdata/make_golden.py`; within 1% of
    /// the score range, as the charm-onnx end-to-end tests allow.
    #[cfg(feature = "ai")]
    fn assert_scores_match_reference(backend: BackendChoice) {
        use super::tokenizer::{fit, tests::pattern, tokenize};
        const TOLERANCE_FRACTION: f32 = 0.01;
        for spec in all_specs() {
            let scorer = Scorer::load(spec, backend).unwrap();
            let name = spec.dataset();
            for case in golden_cases() {
                let image = fit(pattern(case.width, case.height), MAX_IMAGE_SIDE);
                let tokens = tokenize(&image, &scorer.pos, spec.max_tokens).unwrap();
                let got = scorer.raw(&tokens).unwrap();
                let want = case.scores.iter().find(|(m, _)| m == name).unwrap().1;
                let tolerance = TOLERANCE_FRACTION * (spec.score_range.1 - spec.score_range.0);
                assert!(
                    (got - want).abs() < tolerance,
                    "{name} {}x{}: {got} vs {want}",
                    case.width,
                    case.height
                );
            }
        }
    }

    #[cfg(feature = "ai")]
    #[test]
    #[ignore = "downloads ONNX Runtime and six 86 MB models"]
    fn scores_match_reference_on_cpu() {
        assert_scores_match_reference(BackendChoice::Cpu);
    }

    #[cfg(feature = "ai")]
    #[test]
    #[ignore = "downloads ONNX Runtime, the WebGPU plugin and six 86 MB models"]
    fn scores_match_reference_on_webgpu() {
        assert_scores_match_reference(BackendChoice::Webgpu);
    }

    /// The reference outputs of every model lie inside its declared raw
    /// range, so normalization cannot saturate on ordinary photos.
    #[test]
    fn reference_scores_fit_declared_ranges() {
        for spec in all_specs() {
            let name = spec.dataset();
            for case in golden_cases() {
                let (_, raw) = case.scores.iter().find(|(m, _)| m == name).unwrap();
                let (lo, hi) = spec.score_range;
                assert!((lo..=hi).contains(raw), "{name}: {raw} outside {lo}..{hi}");
            }
        }
    }
}
