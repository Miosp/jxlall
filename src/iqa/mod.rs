//! No-reference image rating with Charm (CVPR 2025) models: a technical
//! (detail) and an aesthetic (composition) score per image. Charm sees the
//! whole frame at its aspect ratio and keeps the sharpest regions at full
//! resolution, so intentional blur (bokeh) is judged in context. Models are
//! ONNX exports, fetched once into the per-user cache.

#[cfg(feature = "iqa")]
mod engine;
#[cfg(not(feature = "iqa"))]
#[path = "engine_stub.rs"]
mod engine;
#[cfg(feature = "iqa")]
mod fetch;
mod input;
#[cfg(feature = "iqa")]
mod npy;
mod resample;
mod tokenizer;

use crate::domain::{ImageFrame, Orientation, Rating, Score, StarThresholds};
use tokenizer::{PosTable, Tokens};

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

/// A downloadable file, pinned by content.
#[cfg_attr(not(feature = "iqa"), allow(dead_code))]
struct Remote {
    /// Cache file name; changes whenever the content does.
    file_name: &'static str,
    url: &'static str,
    sha256: &'static str,
    size: u64,
}

/// One exported checkpoint.
#[cfg_attr(not(feature = "iqa"), allow(dead_code))]
struct ModelSpec {
    /// Written to XMP next to the score.
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

/// File URL in the export repository, pinned to a revision.
macro_rules! hf_url {
    ($($part:literal),+) => {
        concat!(
            "https://huggingface.co/Miosp/charm-onnx/resolve/cbcd14f8e00381f4227d2c48a0ffaa532b6a7555/",
            $($part),+
        )
    };
}

static DINOV2_SMALL_POS: Remote = Remote {
    file_name: "charm-cbcd14f-dinov2-small-pos.npy",
    url: hf_url!("spaq/pos_embed.npy"),
    sha256: "79732feda3385a28bab9505eb3aacd083fec0789dfd8d35e4e2a7e1f730bb42d",
    size: 2_104_448,
};

macro_rules! small_model {
    ($name:literal, $sha:literal, $size:literal, $max_tokens:literal, $range:expr) => {
        ModelSpec {
            id: concat!("charm-", $name, "@cbcd14f"),
            model: Remote {
                file_name: concat!("charm-cbcd14f-", $name, ".onnx"),
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
    "604382aeba2f232b883e9822378b33a53bd1820a9335be71bc7126cdc7b65eeb",
    86_122_102,
    1025,
    UNIT_RANGE
);
static KONIQ10K: ModelSpec = small_model!(
    "koniq10k",
    "c96dbb2d3575d181b4de31444a8d57743365c3ade0e30da4ed4e944ece77c54b",
    86_122_102,
    1025,
    UNIT_RANGE
);
static PARA: ModelSpec = small_model!(
    "para",
    "68d4b38b64e2965a3de2e10148e0c45b69b6fddebac4282a0f7c84d07d8401b5",
    86_146_942,
    1025,
    (1.0, 5.0)
);
static AADB: ModelSpec = small_model!(
    "aadb",
    "9b03c35ed14171ad3838d8b604e50457c06de2ed3b7526306775388c635123f1",
    86_122_102,
    1025,
    UNIT_RANGE
);
static TAD66K: ModelSpec = small_model!(
    "tad66k",
    "308f501f42d1c55b62e967184f905418e8a1ef3e44c2a7116e034d12b33d718e",
    86_122_096,
    769,
    (1.0, 10.0)
);
static BAID: ModelSpec = small_model!(
    "baid",
    "bae7f688e19ad56f8beea729f764e3a14dafae2a1c91f9ca81b4ae84a1be4e8f",
    86_122_102,
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
    model: engine::Model,
}

impl Scorer {
    fn load(spec: &'static ModelSpec) -> Result<Self, String> {
        let (model, pos_values, dim) = engine::load(spec)?;
        let pos = PosTable::new(&pos_values, dim)?;
        Ok(Self { spec, pos, model })
    }

    /// Score mapped to 0..=1.
    fn score(&self, tokens: &Tokens) -> Result<Score, String> {
        let raw = self.model.run(tokens)?;
        if !raw.is_finite() {
            return Err(format!("{} returned {raw}", self.spec.id));
        }
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

impl Rater {
    /// Fetch the models if they are not cached yet, then load them.
    pub fn load(opts: IqaOptions) -> Result<Self, String> {
        Ok(Self {
            technical: Scorer::load(opts.technical.spec())?,
            aesthetic: Scorer::load(opts.aesthetic.spec())?,
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
