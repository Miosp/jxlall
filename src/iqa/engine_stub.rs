//! Builds without ONNX Runtime (no `iqa` feature): rating is unavailable.

use super::ModelSpec;
use super::tokenizer::Tokens;

pub enum Model {}

pub fn load(_: &ModelSpec) -> Result<(Model, Vec<f32>, usize), String> {
    Err("this build has no quality rating (built without the `iqa` feature)".into())
}

impl Model {
    pub fn run(&self, _: &Tokens) -> Result<f32, String> {
        match *self {}
    }
}
