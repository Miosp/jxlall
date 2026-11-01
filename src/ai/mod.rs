//! Optional neural image processing. ONNX Runtime, its GPU backends and the
//! models are downloaded on first use, pinned by content.

#[cfg_attr(not(feature = "ai"), allow(dead_code))]
mod artifacts;
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
pub mod backend;
pub mod dejpeg;
#[cfg(feature = "ai")]
mod fetch;
#[cfg(feature = "ai")]
mod runtime;
#[cfg(not(feature = "ai"))]
#[path = "runtime_stub.rs"]
mod runtime;
mod tiling;
