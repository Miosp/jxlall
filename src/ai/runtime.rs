//! ONNX Runtime side: the core is loaded once from the cache, backends are tried
//! in order, each model gets one session.

use std::env::consts::{ARCH, OS};
use std::path::Path;
use std::sync::OnceLock;

use ort::environment::Environment;
use ort::session::Session;
use ort::value::TensorRef;

use super::artifacts::{self, DIRECTML_DLL, Plugin, Remote};
use super::backend::{self, Backend, BackendChoice};
use super::fetch;
use super::tiling::CHANNELS;

/// One loaded model on the backend that accepted it.
pub struct Model {
    session: Session,
    backend: Backend,
}

impl Model {
    /// Load `spec` on the first working backend for `choice`. `validate` runs
    /// on each candidate, so a backend that loads but cannot run the model is
    /// rejected up front. An explicit backend that fails is an error; `auto`
    /// moves on with a note.
    pub fn load(
        spec: &'static Remote,
        choice: BackendChoice,
        validate: impl Fn(&mut Self) -> Result<(), String>,
    ) -> Result<Self, String> {
        let nvidia = backend::nvidia_gpu_present();
        let attempts = backend::attempts(choice, OS, ARCH, nvidia)?;
        let path = fetch::ensure(spec)?;
        let env = environment()?;
        let mut last_error = String::new();
        for backend in attempts {
            let candidate = open(env, &path, backend).map(|session| Self { session, backend });
            let result = candidate.and_then(|mut model| validate(&mut model).map(|()| model));
            match result {
                Ok(model) => {
                    eprintln!("using {} backend", backend.name());
                    return Ok(model);
                }
                Err(e) => {
                    if choice == BackendChoice::Auto {
                        eprintln!("note: {} backend unavailable: {e}", backend.name());
                    }
                    last_error = format!("{} backend: {e}", backend.name());
                }
            }
        }
        Err(last_error)
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// One forward pass on a single square RGB image in CHW layout.
    pub fn run_rgb(
        &mut self,
        input: &'static str,
        output: &'static str,
        data: &[f32],
        size: usize,
    ) -> Result<Vec<f32>, String> {
        let mut run = || -> ort::Result<Vec<f32>> {
            let tensor = TensorRef::from_array_view(([1usize, CHANNELS, size, size], data))?;
            let outputs = self.session.run(ort::inputs![input => tensor])?;
            let (_, values) = outputs[output].try_extract_tensor::<f32>()?;
            Ok(values.to_vec())
        };
        run().map_err(|e| e.to_string())
    }
}

fn environment() -> Result<&'static Environment, String> {
    static ENV: OnceLock<Result<Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| {
        let core = artifacts::core(OS, ARCH).ok_or("no ONNX Runtime build for this platform")?;
        let dir = fetch::ensure_archive(core)?;
        let library = dir.join(core.files[0].name);
        ort::init_from(&library)
            .map_err(|e| e.to_string())?
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| format!("ONNX Runtime: {e}"))
}

fn open(env: &Environment, model: &Path, backend: Backend) -> Result<Session, String> {
    match backend {
        Backend::Cpu => open_cpu(env, model).map_err(|e| e.to_string()),
        Backend::Directml => {
            let archive = artifacts::directml(ARCH).ok_or("no DirectML build for this platform")?;
            let dir = fetch::ensure_archive(archive)?;
            preload(&dir.join(DIRECTML_DLL))?;
            open_directml(env, model).map_err(|e| e.to_string())
        }
        Backend::Webgpu | Backend::TensorrtRtx => {
            let plugin =
                artifacts::plugin(backend, OS, ARCH).ok_or("no plugin for this platform")?;
            open_plugin(env, model, plugin)
        }
    }
}

fn open_cpu(env: &Environment, model: &Path) -> ort::Result<Session> {
    Session::builder(env)?.commit_from_file(model)
}

fn open_directml(env: &Environment, model: &Path) -> ort::Result<Session> {
    Session::builder(env)?
        // DirectML rejects parallel execution and memory patterns.
        .with_parallel_execution(false)?
        .with_memory_pattern(false)?
        .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])?
        .commit_from_file(model)
}

fn open_plugin(env: &Environment, model: &Path, plugin: &Plugin) -> Result<Session, String> {
    let dir = fetch::ensure_archive(plugin.archive)?;
    for name in plugin.preload {
        preload(&dir.join(name))?;
    }
    env.register_ep_library(plugin.ep_name, dir.join(plugin.library))
        .map_err(|e| e.to_string())?;
    let ep_devices = || {
        env.devices()
            .filter(|device| device.ep().is_ok_and(|ep| ep == plugin.ep_name))
    };
    if ep_devices().next().is_none() {
        return Err("no compatible device".into());
    }
    let session = || -> ort::Result<Session> {
        Session::builder(env)?
            .with_devices(ep_devices(), None)?
            .commit_from_file(model)
    };
    session().map_err(|e| e.to_string())
}

/// Load a library by absolute path and keep it loaded, so dependents resolve to
/// this copy and not to one on the system search path.
fn preload(path: &Path) -> Result<(), String> {
    // SAFETY: the file's SHA-256 was verified against the pinned value just
    // before this call; its initializers are from the pinned release.
    let library = unsafe { libloading::Library::new(path) }
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::mem::forget(library);
    Ok(())
}
