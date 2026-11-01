//! Optional neural image processing. ONNX Runtime, its GPU backends and the
//! models are downloaded on first use, pinned by content.

#[cfg_attr(not(feature = "ai"), allow(dead_code))]
mod artifacts;
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
pub mod backend;
pub mod dejpeg;
#[cfg(feature = "ai")]
mod fetch;
pub mod iqa;
#[cfg(feature = "ai")]
mod runtime;
#[cfg(not(feature = "ai"))]
#[path = "runtime_stub.rs"]
mod runtime;
mod tiling;

#[cfg_attr(not(feature = "ai"), allow(dead_code))]
pub enum TensorData<'a> {
    F32(&'a [f32]),
    I64(&'a [i64]),
}

/// One named input tensor of a model.
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
pub struct Input<'a> {
    pub name: &'static str,
    pub shape: Vec<usize>,
    pub data: TensorData<'a>,
}
