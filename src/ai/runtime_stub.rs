//! Builds without the `ai` feature: musl and x86_64 macOS.

use std::path::PathBuf;

use super::Input;
use super::artifacts::Remote;
use super::backend::{Backend, BackendChoice};

const NO_AI_SUPPORT: &str = "this build has no AI support (built without the `ai` feature)";

pub fn ensure_file(_: &Remote) -> Result<PathBuf, String> {
    Err(NO_AI_SUPPORT.into())
}

pub enum Model {}

impl Model {
    pub fn load(
        _: &str,
        _: &'static Remote,
        _: BackendChoice,
        _: impl Fn(&mut Self) -> Result<(), String>,
    ) -> Result<Self, String> {
        Err(NO_AI_SUPPORT.into())
    }

    pub fn backend(&self) -> Backend {
        match *self {}
    }

    pub fn run(&mut self, _: &[Input], _: &'static str) -> Result<Vec<f32>, String> {
        match *self {}
    }
}
