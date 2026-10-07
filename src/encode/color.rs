//! Color encoding selection: CICP enums → ICC bytes → sRGB default.

use super::encoder::{check, Enc, EncodeError};
use crate::domain::{Cicp, ColorDesc};
use jpegxl_sys::color::color_encoding::*;
use jpegxl_sys::encoder::encode::*;

/// CICP values without a JXL enum counterpart fall back to ICC/sRGB.
fn cicp_to_color_encoding(c: &Cicp) -> Option<JxlColorEncoding> {
    let primaries = match c.color_primaries {
        1 => JxlPrimaries::SRgb,
        9 => JxlPrimaries::Rec2100,
        12 => JxlPrimaries::P3,
        _ => return None,
    };
    let tf = match c.transfer_characteristics {
        1 => JxlTransferFunction::BT709,
        8 => JxlTransferFunction::Linear,
        13 => JxlTransferFunction::SRGB,
        16 => JxlTransferFunction::PQ,
        18 => JxlTransferFunction::HLG,
        _ => return None,
    };
    Some(JxlColorEncoding {
        color_space: JxlColorSpace::Rgb,
        white_point: JxlWhitePoint::D65,
        white_point_xy: [0.0; 2],
        primaries,
        primaries_red_xy: [0.0; 2],
        primaries_green_xy: [0.0; 2],
        primaries_blue_xy: [0.0; 2],
        transfer_function: tf,
        gamma: 0.0,
        rendering_intent: JxlRenderingIntent::Perceptual,
    })
}

pub fn set_color(e: &Enc, color: &ColorDesc) -> Result<(), EncodeError> {
    unsafe {
        if let Some(ce) = color.cicp.as_ref().and_then(cicp_to_color_encoding) {
            return check(
                JxlEncoderSetColorEncoding(e.enc, &raw const ce),
                "SetColorEncoding",
            );
        }
        if let Some(icc) = color.icc.as_deref() {
            return check(
                JxlEncoderSetICCProfile(e.enc, icc.as_ptr(), icc.len()),
                "SetICCProfile",
            );
        }
        let mut ce = std::mem::MaybeUninit::<JxlColorEncoding>::uninit();
        JxlColorEncodingSetToSRGB(ce.as_mut_ptr(), false.into());
        let ce = ce.assume_init();
        check(
            JxlEncoderSetColorEncoding(e.enc, &raw const ce),
            "SetColorEncoding(srgb)",
        )
    }
}
