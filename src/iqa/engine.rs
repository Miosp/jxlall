//! ONNX Runtime side: model files from the cache, one session per model.

use std::sync::{Mutex, OnceLock, PoisonError};

use ort::environment::Environment;
use ort::session::Session;
use ort::value::TensorRef;

use super::tokenizer::{PATCH, Tokens};
use super::{ModelSpec, fetch, npy};

const RGB: usize = 3;

/// One loaded checkpoint. ONNX Runtime parallelizes each run itself, so
/// concurrent conversions take turns.
pub struct Model {
    session: Mutex<Session>,
}

/// Session for `spec`, plus its position table (values, embedding dim).
pub fn load(spec: &ModelSpec) -> Result<(Model, Vec<f32>, usize), String> {
    let model_path = fetch::ensure(&spec.model)?;
    let pos_path = fetch::ensure(spec.pos_table)?;
    let pos_bytes = std::fs::read(&pos_path).map_err(|e| format!("{}: {e}", pos_path.display()))?;
    let (pos, shape) =
        npy::read_f32(&pos_bytes).map_err(|e| format!("{}: {e}", pos_path.display()))?;
    let dim = *shape.last().ok_or("empty position table shape")?;

    static ENV: OnceLock<Result<Environment, String>> = OnceLock::new();
    let env = ENV
        .get_or_init(|| ort::init().build().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| format!("ONNX Runtime: {e}"))?;
    let model_err = |e: ort::Error| format!("IQA model {}: {e}", model_path.display());
    let session = Session::builder(env)
        .map_err(model_err)?
        .commit_from_file(&model_path)
        .map_err(model_err)?;
    Ok((
        Model {
            session: Mutex::new(session),
        },
        pos,
        dim,
    ))
}

impl Model {
    /// Raw `score` output for one image.
    pub fn run(&self, tokens: &Tokens) -> Result<f32, String> {
        let n = tokens.scale_ids.len();
        let dim = tokens.pos_embed.len() / n;
        let run = || -> Result<Option<f32>, ort::Error> {
            let patches =
                TensorRef::from_array_view(([1, n, RGB, PATCH, PATCH], tokens.patches.as_slice()))?;
            let pos = TensorRef::from_array_view(([1, n, dim], tokens.pos_embed.as_slice()))?;
            let ids = TensorRef::from_array_view(([1, n], tokens.scale_ids.as_slice()))?;
            let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
            let outputs = session
                .run(ort::inputs!["patches" => patches, "pos_embed" => pos, "scale_ids" => ids])?;
            let (_, score) = outputs["score"].try_extract_tensor::<f32>()?;
            Ok(score.first().copied())
        };
        run()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "model returned no score".into())
    }
}

#[cfg(test)]
mod tests {
    use super::super::MAX_IMAGE_SIDE;
    use super::super::tests::all_specs;
    use super::super::tokenizer::{PosTable, fit, tests::golden_cases, tests::pattern, tokenize};
    use super::*;

    /// Same images and models as `testdata/make_golden.py`; within 1% of
    /// the score range, as the charm-onnx end-to-end tests allow.
    #[test]
    #[ignore = "downloads six 86 MB models"]
    fn scores_match_reference() {
        const TOLERANCE_FRACTION: f32 = 0.01;
        for spec in all_specs() {
            let (model, pos, dim) = load(spec).unwrap();
            let pos = PosTable::new(&pos, dim).unwrap();
            let name = spec.dataset();
            for case in golden_cases() {
                let image = fit(pattern(case.width, case.height), MAX_IMAGE_SIDE);
                let tokens = tokenize(&image, &pos, spec.max_tokens).unwrap();
                let got = model.run(&tokens).unwrap();
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
}
