//! Builds without the `ai` feature: musl and x86_64 macOS.

use super::artifacts::Remote;
use super::backend::{Backend, BackendChoice};

pub enum Model {}

impl Model {
    pub fn load(
        _: &'static Remote,
        _: BackendChoice,
        _: impl Fn(&mut Self) -> Result<(), String>,
    ) -> Result<Self, String> {
        Err("this build has no AI support (built without the `ai` feature)".into())
    }

    pub fn backend(&self) -> Backend {
        match *self {}
    }

    pub fn run_rgb(
        &mut self,
        _: &'static str,
        _: &'static str,
        _: &[f32],
        _: usize,
    ) -> Result<Vec<f32>, String> {
        match *self {}
    }
}
