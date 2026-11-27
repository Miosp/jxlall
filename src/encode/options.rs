//! Raw libjxl frame settings given as `-x KEY=VALUE`.

use super::encoder::{Enc, EncodeError, check, err};
use jpegxl_sys::encoder::encode::{
    JxlEncoderFrameSettingId as Id, JxlEncoderFrameSettings, JxlEncoderFrameSettingsCreate,
    JxlEncoderFrameSettingsSetFloatOption, JxlEncoderFrameSettingsSetOption,
};
use std::ptr::null_mut;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Float,
}

/// Which encodes a setting reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Pixel frames only; JPEG recompression ignores it.
    Pixels,
    /// Lossless JPEG recompression only.
    Jpeg,
}

/// cjxl-style names. Settings jxlall controls itself (effort, photon noise,
/// metadata boxes, buffering) are left out.
const TABLE: &[(&str, Id, Kind, Scope)] = &[
    (
        "decoding-speed",
        Id::DecodingSpeed,
        Kind::Int,
        Scope::Pixels,
    ),
    ("resampling", Id::Resampling, Kind::Int, Scope::Pixels),
    (
        "ec-resampling",
        Id::ExtraChannelResampling,
        Kind::Int,
        Scope::Pixels,
    ),
    ("dots", Id::Dots, Kind::Int, Scope::Pixels),
    ("patches", Id::Patches, Kind::Int, Scope::Pixels),
    ("epf", Id::Epf, Kind::Int, Scope::Pixels),
    ("gaborish", Id::Gaborish, Kind::Int, Scope::Pixels),
    ("modular", Id::Modular, Kind::Int, Scope::Pixels),
    ("group-order", Id::GroupOrder, Kind::Int, Scope::Pixels),
    ("center-x", Id::GroupOrderCenterX, Kind::Int, Scope::Pixels),
    ("center-y", Id::GroupOrderCenterY, Kind::Int, Scope::Pixels),
    ("responsive", Id::Responsive, Kind::Int, Scope::Pixels),
    (
        "progressive-ac",
        Id::ProgressiveAc,
        Kind::Int,
        Scope::Pixels,
    ),
    (
        "qprogressive-ac",
        Id::QprogressiveAc,
        Kind::Int,
        Scope::Pixels,
    ),
    (
        "progressive-dc",
        Id::ProgressiveDc,
        Kind::Int,
        Scope::Pixels,
    ),
    (
        "channel-colors-global-percent",
        Id::ChannelColorsGlobalPercent,
        Kind::Float,
        Scope::Pixels,
    ),
    (
        "channel-colors-group-percent",
        Id::ChannelColorsGroupPercent,
        Kind::Float,
        Scope::Pixels,
    ),
    (
        "palette-colors",
        Id::PaletteColors,
        Kind::Int,
        Scope::Pixels,
    ),
    ("lossy-palette", Id::LossyPalette, Kind::Int, Scope::Pixels),
    (
        "color-transform",
        Id::ColorTransform,
        Kind::Int,
        Scope::Pixels,
    ),
    (
        "modular-colorspace",
        Id::ModularColorSpace,
        Kind::Int,
        Scope::Pixels,
    ),
    (
        "modular-group-size",
        Id::ModularGroupSize,
        Kind::Int,
        Scope::Pixels,
    ),
    (
        "modular-predictor",
        Id::ModularPredictor,
        Kind::Int,
        Scope::Pixels,
    ),
    (
        "modular-ma-tree-learning-percent",
        Id::ModularMaTreeLearningPercent,
        Kind::Float,
        Scope::Pixels,
    ),
    (
        "modular-nb-prev-channels",
        Id::ModularNbPrevChannels,
        Kind::Int,
        Scope::Pixels,
    ),
    ("jpeg-recon-cfl", Id::JpegReconCfl, Kind::Int, Scope::Jpeg),
    ("brotli-effort", Id::BrotliEffort, Kind::Int, Scope::Jpeg),
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    Int(i64),
    Float(f32),
}

/// One validated libjxl frame setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JxlOption {
    name: &'static str,
    id: Id,
    scope: Scope,
    value: Value,
}

impl JxlOption {
    /// Parse `key`/`value`; the value range is checked by [`validate`].
    pub fn parse(key: &str, value: &str) -> Result<Self, String> {
        let &(name, id, kind, scope) = TABLE.iter().find(|(n, ..)| *n == key).ok_or_else(|| {
            format!(
                "unknown JPEG XL option `{key}` (known: {})",
                TABLE
                    .iter()
                    .map(|(n, ..)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let bad = || format!("-x {key}: `{value}` is not a valid number");
        let value = match kind {
            Kind::Int => Value::Int(value.parse().map_err(|_| bad())?),
            Kind::Float => Value::Float(
                value
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or_else(bad)?,
            ),
        };
        Ok(Self {
            name,
            id,
            scope,
            value,
        })
    }

    /// Whether lossless JPEG recompression ignores this setting.
    pub fn pixels_only(self) -> bool {
        self.scope == Scope::Pixels
    }

    fn describe(self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for JxlOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.value {
            Value::Int(v) => write!(f, "{}={v}", self.name),
            Value::Float(v) => write!(f, "{}={v}", self.name),
        }
    }
}

/// Apply the settings meant for this kind of encode.
///
/// # Safety
/// `fs` must be live frame settings of a live encoder.
pub(super) unsafe fn apply(
    fs: *mut JxlEncoderFrameSettings,
    options: &[JxlOption],
    jpeg: bool,
) -> Result<(), EncodeError> {
    let wanted = if jpeg { Scope::Jpeg } else { Scope::Pixels };
    for o in options.iter().filter(|o| o.scope == wanted) {
        let status = unsafe {
            match o.value {
                Value::Int(v) => JxlEncoderFrameSettingsSetOption(fs, o.id, v),
                Value::Float(v) => JxlEncoderFrameSettingsSetFloatOption(fs, o.id, v),
            }
        };
        check(status, &format!("-x {}", o.describe()))?;
    }
    Ok(())
}

/// libjxl checks ranges when a setting is applied: try them once up front
/// so a bad value is one error, not one per file.
pub fn validate(options: &[JxlOption]) -> Result<(), String> {
    let run = || -> Result<(), EncodeError> {
        let e = Enc::new(1)?;
        for o in options {
            // SAFETY: the frame settings belong to `e`, alive for the loop.
            unsafe {
                let fs = JxlEncoderFrameSettingsCreate(e.enc, null_mut());
                if fs.is_null() {
                    return err("FrameSettingsCreate failed");
                }
                let status = match o.value {
                    Value::Int(v) => JxlEncoderFrameSettingsSetOption(fs, o.id, v),
                    Value::Float(v) => JxlEncoderFrameSettingsSetFloatOption(fs, o.id, v),
                };
                if check(status, "").is_err() {
                    return err(format!("libjxl rejected -x {}", o.describe()));
                }
            }
        }
        Ok(())
    };
    run().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_options_parse_and_unknown_ones_list_the_names() {
        let o = JxlOption::parse("epf", "3").unwrap();
        assert_eq!(o.value, Value::Int(3));
        assert!(o.pixels_only());
        assert!(
            !JxlOption::parse("brotli-effort", "9")
                .unwrap()
                .pixels_only()
        );
        let f = JxlOption::parse("channel-colors-global-percent", "12.5").unwrap();
        assert_eq!(f.value, Value::Float(12.5));
        let e = JxlOption::parse("sharpness", "2").unwrap_err();
        assert!(e.contains("epf") && e.contains("gaborish"), "{e}");
        assert!(JxlOption::parse("epf", "high").is_err());
    }

    #[test]
    fn libjxl_range_errors_are_caught_up_front() {
        assert!(validate(&[JxlOption::parse("epf", "3").unwrap()]).is_ok());
        let e = validate(&[JxlOption::parse("epf", "4").unwrap()]).unwrap_err();
        assert!(e.contains("epf=4"), "{e}");
    }
}
