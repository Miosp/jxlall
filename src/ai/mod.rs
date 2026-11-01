//! Optional neural image processing. ONNX Runtime, its GPU backends and the
//! models are downloaded on first use, pinned by content.

#[allow(dead_code)]
pub mod backend;
#[allow(dead_code)]
pub mod dejpeg;
#[allow(dead_code)]
mod tiling;
