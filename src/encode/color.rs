//! Color encoding selection: CICP enums → ICC bytes → gAMA/cHRM → default.

use super::encoder::{check, Enc, EncodeError};
use crate::domain::{Cicp, CustomColor};
use jpegxl_sys::color::color_encoding::*;
use jpegxl_sys::encoder::encode::*;

/// CICP code point meaning "unspecified".
const CICP_UNSPECIFIED: u16 = 2;
/// Pure-power transfer functions (CICP TF 4 and 5) as OETF exponents.
const GAMMA_22_OETF: f64 = 1.0 / 2.2;
const GAMMA_28_OETF: f64 = 1.0 / 2.8;

fn rgb_encoding(
    primaries: JxlPrimaries,
    white_point: JxlWhitePoint,
    transfer_function: JxlTransferFunction,
    gamma: f64,
) -> JxlColorEncoding {
    JxlColorEncoding {
        color_space: JxlColorSpace::Rgb,
        white_point,
        white_point_xy: [0.0; 2],
        primaries,
        primaries_red_xy: [0.0; 2],
        primaries_green_xy: [0.0; 2],
        primaries_blue_xy: [0.0; 2],
        transfer_function,
        gamma,
        rendering_intent: JxlRenderingIntent::Perceptual,
    }
}

/// CICP values without a JXL enum counterpart fall back to ICC/sRGB. One
/// unspecified half (primaries or transfer) defaults to its sRGB value.
fn cicp_to_color_encoding(c: &Cicp) -> Option<JxlColorEncoding> {
    if c.color_primaries == CICP_UNSPECIFIED && c.transfer_characteristics == CICP_UNSPECIFIED {
        return None;
    }
    let (primaries, white_point) = match c.color_primaries {
        1 | CICP_UNSPECIFIED => (JxlPrimaries::SRgb, JxlWhitePoint::D65),
        9 => (JxlPrimaries::Rec2100, JxlWhitePoint::D65),
        11 => (JxlPrimaries::P3, JxlWhitePoint::Dci),
        12 => (JxlPrimaries::P3, JxlWhitePoint::D65),
        _ => return None,
    };
    let (tf, gamma) = match c.transfer_characteristics {
        // BT.601 and BT.2020 10/12-bit share the BT.709 curve.
        1 | 6 | 14 | 15 => (JxlTransferFunction::BT709, 0.0),
        4 => (JxlTransferFunction::Gamma, GAMMA_22_OETF),
        5 => (JxlTransferFunction::Gamma, GAMMA_28_OETF),
        8 => (JxlTransferFunction::Linear, 0.0),
        13 | CICP_UNSPECIFIED => (JxlTransferFunction::SRGB, 0.0),
        16 => (JxlTransferFunction::PQ, 0.0),
        17 => (JxlTransferFunction::DCI, 0.0),
        18 => (JxlTransferFunction::HLG, 0.0),
        _ => return None,
    };
    Some(rgb_encoding(primaries, white_point, tf, gamma))
}

/// Tolerance for treating a stored gamma as exactly linear (PNG gAMA is
/// quantized to 1e-5).
const LINEAR_GAMMA_EPS: f64 = 1e-4;

fn custom_to_color_encoding(c: &CustomColor) -> JxlColorEncoding {
    let (tf, gamma) = match c.gamma {
        Some(g) if (g - 1.0).abs() < LINEAR_GAMMA_EPS => (JxlTransferFunction::Linear, 0.0),
        Some(g) => (JxlTransferFunction::Gamma, g),
        None => (JxlTransferFunction::SRGB, 0.0),
    };
    let mut ce = rgb_encoding(JxlPrimaries::SRgb, JxlWhitePoint::D65, tf, gamma);
    if let Some([w, r, g, b]) = c.chromaticities {
        ce.white_point = JxlWhitePoint::Custom;
        ce.white_point_xy = w;
        ce.primaries = JxlPrimaries::Custom;
        ce.primaries_red_xy = r;
        ce.primaries_green_xy = g;
        ce.primaries_blue_xy = b;
    }
    ce
}

/// Explicit color inputs for one encode; `icc` is already vetted against the
/// pixel layout by the caller.
pub struct ColorChoice<'a> {
    pub cicp: Option<&'a Cicp>,
    pub icc: Option<&'a [u8]>,
    pub custom: Option<&'a CustomColor>,
    /// Encode as 1-channel gray; only ever set together with a gray `icc`.
    pub gray: bool,
    /// Float samples without a tag are conventionally scene-linear.
    pub linear_default: bool,
}

pub fn set_color(e: &Enc, c: &ColorChoice) -> Result<(), EncodeError> {
    unsafe {
        // Gray output cannot take an RGB enum encoding; only a gray ICC.
        if !c.gray
            && let Some(ce) = c.cicp.and_then(cicp_to_color_encoding)
        {
            return check(
                JxlEncoderSetColorEncoding(e.enc, &raw const ce),
                "SetColorEncoding",
            );
        }
        if let Some(cicp) = c.cicp
            && c.icc.is_none()
            && (cicp.color_primaries, cicp.transfer_characteristics)
                != (CICP_UNSPECIFIED, CICP_UNSPECIFIED)
        {
            eprintln!(
                "note: CICP {}/{} has no JXL equivalent, encoding with default color",
                cicp.color_primaries, cicp.transfer_characteristics
            );
        }
        if let Some(icc) = c.icc {
            return check(
                JxlEncoderSetICCProfile(e.enc, icc.as_ptr(), icc.len()),
                "SetICCProfile",
            );
        }
        if let Some(custom) = c.custom {
            let ce = custom_to_color_encoding(custom);
            return check(
                JxlEncoderSetColorEncoding(e.enc, &raw const ce),
                "SetColorEncoding(custom)",
            );
        }
        let mut ce = std::mem::MaybeUninit::<JxlColorEncoding>::uninit();
        if c.linear_default {
            JxlColorEncodingSetToLinearSRGB(ce.as_mut_ptr(), false.into());
        } else {
            JxlColorEncodingSetToSRGB(ce.as_mut_ptr(), false.into());
        }
        let ce = ce.assume_init();
        check(
            JxlEncoderSetColorEncoding(e.enc, &raw const ce),
            "SetColorEncoding(default)",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cicp(p: u16, t: u16) -> Cicp {
        Cicp { color_primaries: p, transfer_characteristics: t }
    }

    #[test]
    fn bt2020_sdr_maps_to_rec2100_bt709() {
        let ce = cicp_to_color_encoding(&cicp(9, 14)).expect("mapped");
        assert_eq!(ce.primaries, JxlPrimaries::Rec2100);
        assert_eq!(ce.transfer_function, JxlTransferFunction::BT709);
    }

    #[test]
    fn dci_p3_uses_dci_white_point() {
        let ce = cicp_to_color_encoding(&cicp(11, 17)).expect("mapped");
        assert_eq!(ce.primaries, JxlPrimaries::P3);
        assert_eq!(ce.white_point, JxlWhitePoint::Dci);
        assert_eq!(ce.transfer_function, JxlTransferFunction::DCI);
    }

    #[test]
    fn unspecified_primaries_keep_pq() {
        let ce = cicp_to_color_encoding(&cicp(2, 16)).expect("mapped");
        assert_eq!(ce.primaries, JxlPrimaries::SRgb);
        assert_eq!(ce.transfer_function, JxlTransferFunction::PQ);
        assert!(cicp_to_color_encoding(&cicp(2, 2)).is_none());
    }

    #[test]
    fn gamma_22_is_oetf_exponent() {
        let ce = cicp_to_color_encoding(&cicp(1, 4)).expect("mapped");
        assert_eq!(ce.transfer_function, JxlTransferFunction::Gamma);
        assert!((ce.gamma - 1.0 / 2.2).abs() < 1e-9);
    }

    #[test]
    fn png_linear_gamma_and_chromaticities() {
        let c = CustomColor {
            gamma: Some(1.0),
            chromaticities: Some([[0.3127, 0.329], [0.64, 0.33], [0.3, 0.6], [0.15, 0.06]]),
        };
        let ce = custom_to_color_encoding(&c);
        assert_eq!(ce.transfer_function, JxlTransferFunction::Linear);
        assert_eq!(ce.primaries, JxlPrimaries::Custom);
        assert_eq!(ce.primaries_red_xy, [0.64, 0.33]);
    }
}
