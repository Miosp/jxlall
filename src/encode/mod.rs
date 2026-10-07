//! libjxl encoding via raw `jpegxl-sys` FFI.

mod boxes;
mod color;
mod encoder;

pub use boxes::{build_jhgm_box, encode_gain_map_gray};
pub use encoder::{Enc, EncodeError};

use self::encoder::{check, err};
use boxes::add_metadata_boxes;
use color::set_color;
use crate::domain::{ImageFrame, MetaBoxes, Orientation, Pixels};
use jpegxl_sys::common::types::{JxlDataType, JxlEndianness, JxlPixelFormat};
use jpegxl_sys::encoder::encode::*;
use jpegxl_sys::metadata::codestream_header::{JxlBasicInfo, JxlOrientation};
use std::ffi::c_void;
use std::ptr::null_mut;
/// Encoder effort 1..=10 (lightning → glacier).
#[derive(Debug, Clone, Copy)]
pub struct Effort(i64);

impl TryFrom<i64> for Effort {
    type Error = String;

    fn try_from(v: i64) -> Result<Self, Self::Error> {
        (1..=10).contains(&v).then_some(Self(v)).ok_or_else(|| "effort must be 1..=10".into())
    }
}

impl Effort {
    pub(crate) fn value(self) -> i64 {
        self.0
    }
}

/// Lossy target distance; 0 = lossless.
#[derive(Debug, Clone, Copy)]
pub struct Distance(f32);

impl Distance {
    pub fn is_lossless(self) -> bool {
        self.0 == 0.0
    }

    fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for Distance {
    type Error = String;

    fn try_from(v: f32) -> Result<Self, Self::Error> {
        (0.0..=25.0).contains(&v).then_some(Self(v)).ok_or_else(|| "distance must be 0..=25".into())
    }
}

/// Encoder knobs threaded through every encode call.
#[derive(Debug, Clone, Copy)]
pub struct EncodeSettings {
    pub effort: Effort,
    pub distance: Distance,
}

/// Lossless JPEG → JXL recompression (jbrd). JPEG bytes must be a complete file.
pub fn encode_jpeg(jpeg: &[u8], settings: &EncodeSettings) -> Result<Vec<u8>, EncodeError> {
    let e = Enc::new()?;
    unsafe {
        check(
            JxlEncoderStoreJPEGMetadata(e.enc, true.into()),
            "StoreJPEGMetadata",
        )?;
        check(JxlEncoderUseContainer(e.enc, true.into()), "UseContainer")?;
        let settings_handle = JxlEncoderFrameSettingsCreate(e.enc, null_mut());
        if settings_handle.is_null() {
            return err("FrameSettingsCreate failed");
        }
        check(
            JxlEncoderFrameSettingsSetOption(
                settings_handle,
                JxlEncoderFrameSettingId::Effort,
                settings.effort.value(),
            ),
            "set effort",
        )?;
        // EXIF/XMP/JUMBF preservation is the default since libjxl 0.10.
        let s = JxlEncoderAddJPEGFrame(settings_handle, jpeg.as_ptr(), jpeg.len());
        if s == JxlEncoderStatus::Error {
            return err("jbrd: unsupported JPEG (CMYK, arithmetic, 12-bit, or corrupt)");
        }
        check(s, "AddJPEGFrame")?;
        JxlEncoderCloseInput(e.enc);
    }
    e.collect_output()
}

/// Pixel path: encode one frame with explicit color/metadata/orientation.
pub fn encode_image(
    frame: &ImageFrame,
    color: &crate::domain::ColorDesc,
    meta: &MetaBoxes,
    orientation: Orientation,
    hdr: &crate::domain::Hdr,
    settings: &EncodeSettings,
) -> Result<Vec<u8>, EncodeError> {
    let e = Enc::new()?;
    unsafe {
        check(JxlEncoderUseContainer(e.enc, true.into()), "UseContainer")?;
        let mut bi = std::mem::MaybeUninit::<JxlBasicInfo>::uninit();
        JxlEncoderInitBasicInfo(bi.as_mut_ptr());
        let mut bi = bi.assume_init();
        bi.xsize = frame.width;
        bi.ysize = frame.height;
        bi.num_color_channels = 3;
        bi.orientation = orientation.jxl();
        if frame.has_alpha {
            bi.num_extra_channels = 1;
            bi.alpha_bits = u32::from(frame.bit_depth);
            // Float samples use an 8-bit exponent in the alpha channel too.
            bi.alpha_exponent_bits = if matches!(frame.pixels, Pixels::F32(_)) {
                8
            } else {
                0
            };
        }
        if let Some(nits) = hdr.intensity_target {
            bi.intensity_target = nits;
        }
        apply_bit_depth(&mut bi, frame);
        if settings.distance.is_lossless() {
            // Lossless requires keeping the source color profile untouched.
            bi.uses_original_profile = true.into();
        }
        check(JxlEncoderSetBasicInfo(e.enc, &raw const bi), "SetBasicInfo")?;
        set_color(&e, color)?;

        let fs = JxlEncoderFrameSettingsCreate(e.enc, null_mut());
        if fs.is_null() {
            return err("FrameSettingsCreate failed");
        }
        check(
            JxlEncoderFrameSettingsSetOption(fs, JxlEncoderFrameSettingId::Effort, settings.effort.value()),
            "set effort",
        )?;
        if settings.distance.is_lossless() {
            check(JxlEncoderSetFrameLossless(fs, true.into()), "SetFrameLossless")?;
        } else {
            check(
                JxlEncoderSetFrameDistance(fs, settings.distance.get()),
                "SetFrameDistance",
            )?;
        }

        let format = pixel_format(frame);
        let (ptr, len_bytes) = pixel_ptr(frame);
        check(
            JxlEncoderAddImageFrame(fs, &format, ptr, len_bytes),
            "AddImageFrame",
        )?;
        add_metadata_boxes(&e, meta)?;
        JxlEncoderCloseInput(e.enc);
    }
    e.collect_output()
}

fn apply_bit_depth(bi: &mut JxlBasicInfo, frame: &ImageFrame) {
    match &frame.pixels {
        Pixels::U8(_) => {
            bi.bits_per_sample = u32::from(frame.bit_depth.min(8));
            bi.exponent_bits_per_sample = 0;
        }
        Pixels::U16(_) => {
            bi.bits_per_sample = u32::from(frame.bit_depth);
            bi.exponent_bits_per_sample = 0;
        }
        Pixels::F32(_) => {
            bi.bits_per_sample = 32;
            bi.exponent_bits_per_sample = 8;
        }
    }
}

fn pixel_format(frame: &ImageFrame) -> JxlPixelFormat {
    JxlPixelFormat {
        num_channels: if frame.has_alpha { 4 } else { 3 },
        data_type: match &frame.pixels {
            Pixels::U8(_) => JxlDataType::Uint8,
            Pixels::U16(_) => JxlDataType::Uint16,
            Pixels::F32(_) => JxlDataType::Float,
        },
        endianness: JxlEndianness::Native,
        align: 0,
    }
}

fn pixel_ptr(frame: &ImageFrame) -> (*const c_void, usize) {
    let (ptr, len_bytes) = frame.pixels.as_raw_parts();
    (ptr.cast(), len_bytes)
}

impl Orientation {
    /// EXIF orientation values map 1:1 onto `JxlOrientation` (1..=8).
    fn jxl(self) -> JxlOrientation {
        match self.get() {
            2 => JxlOrientation::FlipHorizontal,
            3 => JxlOrientation::Rotate180,
            4 => JxlOrientation::FlipVertical,
            5 => JxlOrientation::Transpose,
            6 => JxlOrientation::Rotate90Cw,
            7 => JxlOrientation::AntiTranspose,
            8 => JxlOrientation::Rotate90Ccw,
            _ => JxlOrientation::Identity,
        }
    }
}
