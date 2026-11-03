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

/// Range of shapes a model input will see, for backends that compile the model
/// for a fixed range.
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
pub struct ShapeRange {
    pub input: &'static str,
    pub min: Vec<usize>,
    pub opt: Vec<usize>,
    pub max: Vec<usize>,
}

/// TensorRT RTX provider options. Without them it guesses sizes for dynamic
/// dimensions, and a guess a model's reshapes cannot divide fails the build.
/// Keys carry `ep_name`: ort silently drops options not prefixed with their
/// provider.
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
pub fn tensorrt_profile_options(ep_name: &str, ranges: &[ShapeRange]) -> Vec<(String, String)> {
    let join = |pick: fn(&ShapeRange) -> &[usize]| {
        ranges
            .iter()
            .map(|range| {
                let dims: Vec<String> = pick(range).iter().map(usize::to_string).collect();
                format!("{}:{}", range.input, dims.join("x"))
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let key = |name: &str| format!("{ep_name}.{name}");
    vec![
        (key("nv_profile_min_shapes"), join(|r| &r.min)),
        (key("nv_profile_opt_shapes"), join(|r| &r.opt)),
        (key("nv_profile_max_shapes"), join(|r| &r.max)),
    ]
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn profile_options_use_the_provider_format() {
        let range = |input, n| ShapeRange {
            input,
            min: vec![1, n],
            opt: vec![2, n],
            max: vec![3, n],
        };
        assert_eq!(
            tensorrt_profile_options("Ep", &[range("a", 8), range("b", 9)]),
            [
                (
                    "Ep.nv_profile_min_shapes".to_string(),
                    "a:1x8,b:1x9".to_string()
                ),
                (
                    "Ep.nv_profile_opt_shapes".to_string(),
                    "a:2x8,b:2x9".to_string()
                ),
                (
                    "Ep.nv_profile_max_shapes".to_string(),
                    "a:3x8,b:3x9".to_string()
                ),
            ]
        );
    }
}
