//! Optional neural image processing. ONNX Runtime, its GPU backends and the
//! models are downloaded on first use, pinned by content.

#[allow(dead_code)]
mod artifacts;
#[allow(dead_code)]
pub mod backend;
#[allow(dead_code)]
pub mod dejpeg;
#[cfg(feature = "ai")]
#[allow(dead_code)]
mod fetch;
#[cfg(feature = "ai")]
#[allow(dead_code)]
mod runtime;
#[cfg(not(feature = "ai"))]
#[path = "runtime_stub.rs"]
mod runtime;
#[allow(dead_code)]
mod tiling;
