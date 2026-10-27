//! No-reference image quality assessment (NR-IQA): an ONNX model, fetched
//! once into the per-user cache, scores each image for its XMP rating.

mod fetch;
mod input;

use std::sync::{Mutex, PoisonError};

use ort::environment::Environment;
use ort::session::Session;
use ort::value::TensorRef;

use crate::domain::{ImageFrame, Orientation, Rating, StarThresholds};

/// Selectable quality model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum IqaModel {
    /// TOPIQ-NR (ResNet-50, trained on KonIQ-10k). Weights are CC BY-NC-SA
    /// 4.0: non-commercial use only.
    TopiqNr,
}

/// Where a model comes from and how it is fed.
struct ModelSpec {
    /// Written to XMP; changes whenever the weights do.
    id: &'static str,
    file_name: &'static str,
    /// Pinned to a repository revision; `sha256` guards the content.
    url: &'static str,
    sha256: &'static str,
    size: u64,
    license: &'static str,
    /// Square input side; the image is resized to it, aspect not kept.
    input_side: usize,
    input_name: &'static str,
    output_name: &'static str,
}

const TOPIQ_NR: ModelSpec = ModelSpec {
    id: "topiq-nr-koniq@3dab0f8",
    file_name: "topiq-nr-koniq-3dab0f8.onnx",
    url: "https://huggingface.co/nocturna/topiq-nr-koniq-onnx/resolve/3dab0f80c8a3afa0356c763b0e741564096783a8/model.onnx",
    sha256: "9aa87559a8b8f6084ed5ab5eb0d489e1351ec9ad7e733213cdff14a5dccf34c2",
    size: 176_803_037,
    license: "TOPIQ by Chaofeng Chen et al. (IQA-PyTorch), CC BY-NC-SA 4.0: non-commercial use only",
    input_side: 384,
    input_name: "image",
    output_name: "score",
};

impl IqaModel {
    fn spec(self) -> &'static ModelSpec {
        match self {
            Self::TopiqNr => &TOPIQ_NR,
        }
    }
}

/// User choices for rating.
#[derive(Debug, Clone, Copy)]
pub struct IqaOptions {
    pub model: IqaModel,
    /// `Some`: also write `xmp:Rating` stars.
    pub stars: Option<StarThresholds>,
    pub overwrite_stars: bool,
}

/// Loaded model plus the rating policy. Shared by all conversions; ONNX
/// Runtime parallelizes each inference itself, so runs are serialized.
pub struct Rater {
    spec: &'static ModelSpec,
    session: Mutex<Session>,
    stars: Option<StarThresholds>,
    overwrite_stars: bool,
    // Kept alive for the session's lifetime.
    _env: Environment,
}

impl Rater {
    /// Fetch the model if it is not cached yet, then load it.
    pub fn load(opts: IqaOptions) -> Result<Self, String> {
        let spec = opts.model.spec();
        let path = fetch::ensure(spec)?;
        let model_err = |e: ort::Error| format!("IQA model {}: {e}", path.display());
        let env = ort::init().build().map_err(model_err)?;
        let session = Session::builder(&env)
            .map_err(model_err)?
            .commit_from_file(&path)
            .map_err(model_err)?;
        Ok(Self {
            spec,
            session: Mutex::new(session),
            stars: opts.stars,
            overwrite_stars: opts.overwrite_stars,
            _env: env,
        })
    }

    pub fn rate(&self, frame: &ImageFrame, orientation: Orientation) -> Result<Rating, String> {
        let score = self.score(frame, orientation).map_err(|e| format!("IQA: {e}"))?;
        Ok(Rating {
            score,
            model: self.spec.id,
            stars: self.stars.map(|t| t.stars_for(score)),
            overwrite_stars: self.overwrite_stars,
        })
    }

    fn score(&self, frame: &ImageFrame, orientation: Orientation) -> Result<f32, ort::Error> {
        let side = self.spec.input_side;
        let pixels = input::prepare(frame, orientation, side);
        let tensor = TensorRef::from_array_view(([1usize, 3, side, side], pixels.as_slice()))?;
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let outputs = session.run(ort::inputs![self.spec.input_name => tensor])?;
        let (_, score) = outputs[self.spec.output_name].try_extract_tensor::<f32>()?;
        Ok(score.first().copied().unwrap_or(f32::NAN))
    }
}
