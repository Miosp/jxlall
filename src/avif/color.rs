//! Color for AVIF output: every source is converted to one of three spaces
//! whose signalling was measured to render correctly in Windows Photos,
//! Chrome and Immich:
//!
//! - sRGB: nclx 1/13/6 plus an sRGB ICC (Immich treats a 10-bit file with
//!   no ICC as P3, oversaturating it);
//! - Display P3: nclx 12/13/6 plus a "Display P3" ICC;
//! - wider sources (Adobe RGB, ProPhoto, Rec.2020, ...): the P3 + Adobe RGB
//!   union gamut, described by the ICC only (nclx primaries unspecified, as
//!   no CICP code exists for it).
//!
//! The source gamut picks the smallest of the three that holds it. PQ/HLG
//! sources keep their CICP and pixels unchanged.

use super::icc::{self, Primaries, Trc};
use crate::domain::{ColorDesc, CustomColor, IccSpace};
use moxcms::{ColorProfile, Layout, RenderingIntent, Transform16BitExecutor, TransformOptions};

pub const SRGB_DESCRIPTION: &str = "sRGB IEC61966-2.1";
pub const P3_DESCRIPTION: &str = "Display P3";
pub const WIDE_DESCRIPTION: &str = "Wide Gamut RGB (P3 + Adobe RGB)";

/// CICP matrix coefficients for SDR output (BT.601, measured fine in WIC).
pub const MATRIX_BT601: u8 = 6;
const MATRIX_BT2020_NCL: u8 = 9;

/// How the output's color is described.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub icc: Option<Vec<u8>>,
    /// Gamut class of SDR output; `None` for HDR passthrough.
    pub space: Option<Space>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    Srgb,
    P3,
    Wide,
}

impl Space {
    fn primaries(self) -> Primaries {
        match self {
            Self::Srgb => icc::SRGB,
            Self::P3 => icc::DISPLAY_P3,
            Self::Wide => icc::WIDE,
        }
    }

    pub fn signal(self) -> Signal {
        let (cp, description) = match self {
            Self::Srgb => (1, SRGB_DESCRIPTION),
            Self::P3 => (12, P3_DESCRIPTION),
            Self::Wide => (2, WIDE_DESCRIPTION),
        };
        Signal {
            primaries: cp,
            transfer: 13,
            matrix: MATRIX_BT601,
            icc: Some(icc::rgb_profile(&self.primaries(), Trc::Srgb, description)),
            space: Some(self),
        }
    }

    pub fn is_wide(self) -> bool {
        self != Self::Srgb
    }
}

/// Interleaved 16-bit RGB or RGBA, full range.
pub struct Rgb16 {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub data: Vec<u16>,
}

impl Rgb16 {
    pub fn channels(&self) -> usize {
        if self.alpha { 4 } else { 3 }
    }
}

/// Where the source color description comes from.
enum Source {
    /// Pixels already in the target space; no conversion.
    Exact(Space),
    Profile(Box<ColorProfile>, Option<Space>),
    Hdr {
        primaries: u8,
        transfer: u8,
    },
}

/// Convert `img` in place to its output space and return the signalling.
/// `linear_default`: untagged samples are linear (float sources). The
/// transform runs on up to `threads` threads.
pub fn convert(
    img: &mut Rgb16,
    color: &ColorDesc,
    linear_default: bool,
    threads: usize,
) -> Result<Signal, String> {
    let (profile, hint) = match source(color, linear_default) {
        Source::Exact(space) => return Ok(space.signal()),
        Source::Hdr {
            primaries,
            transfer,
        } => {
            return Ok(Signal {
                primaries,
                transfer,
                matrix: MATRIX_BT2020_NCL,
                icc: None,
                space: None,
            });
        }
        Source::Profile(p, hint) => (*p, hint),
    };
    let space = hint.unwrap_or_else(|| classify(&profile));
    let signal = space.signal();
    let dst = ColorProfile::new_from_slice(signal.icc.as_deref().expect("SDR output has an ICC"))
        .map_err(|e| format!("internal: output profile: {e:?}"))?;
    let options = TransformOptions {
        rendering_intent: RenderingIntent::RelativeColorimetric,
        ..TransformOptions::default()
    };
    let gray = profile.color_space == moxcms::DataColorSpace::Gray;
    let rgb_layout = if img.alpha { Layout::Rgba } else { Layout::Rgb };
    if gray {
        let src_layout = if img.alpha {
            Layout::GrayAlpha
        } else {
            Layout::Gray
        };
        let t = profile
            .create_transform_16bit(src_layout, &dst, rgb_layout, options)
            .map_err(|e| format!("gray color transform: {e:?}"))?;
        // Decoders expand gray to RGB; feed the gray channel back in.
        let ch = img.channels();
        let gray_px: Vec<u16> = img
            .data
            .chunks_exact(ch)
            .flat_map(|p| {
                if img.alpha {
                    vec![p[0], p[3]]
                } else {
                    vec![p[0]]
                }
            })
            .collect();
        let src_ch = if img.alpha { 2 } else { 1 };
        transform(&*t, &gray_px, src_ch, &mut img.data, ch, threads)
            .map_err(|e| format!("gray color transform: {e:?}"))?;
    } else {
        let t = profile
            .create_transform_16bit(rgb_layout, &dst, rgb_layout, options)
            .map_err(|e| format!("color transform: {e:?}"))?;
        let src = std::mem::take(&mut img.data);
        img.data = vec![0; src.len()];
        let ch = img.channels();
        transform(&*t, &src, ch, &mut img.data, ch, threads)
            .map_err(|e| format!("color transform: {e:?}"))?;
    }
    Ok(signal)
}

/// `t` over `src` into `dst` (`src_ch` and `dst_ch` samples per pixel),
/// in equal pixel ranges on up to `threads` threads.
fn transform(
    t: &Transform16BitExecutor,
    src: &[u16],
    src_ch: usize,
    dst: &mut [u16],
    dst_ch: usize,
    threads: usize,
) -> Result<(), moxcms::CmsError> {
    let pixels = src.len() / src_ch;
    let per_thread = pixels.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = src
            .chunks(per_thread * src_ch)
            .zip(dst.chunks_mut(per_thread * dst_ch))
            .map(|(src, dst)| s.spawn(move || t.transform(src, dst)))
            .collect();
        parts
            .into_iter()
            .try_for_each(|p| p.join().expect("color transform thread panicked"))
    })
}

fn parse(icc: &[u8]) -> Option<ColorProfile> {
    match ColorProfile::new_from_slice(icc) {
        Ok(p) => Some(p),
        Err(e) => {
            notice!("note: ICC profile unreadable ({e:?}), treated as sRGB");
            None
        }
    }
}

fn synthesized(p: &Primaries, trc: Trc) -> ColorProfile {
    ColorProfile::new_from_slice(&icc::rgb_profile(p, trc, "source"))
        .expect("synthesized profiles always parse")
}

fn source(color: &ColorDesc, linear_default: bool) -> Source {
    match color.icc_space() {
        Some(IccSpace::Rgb | IccSpace::Gray) => {
            if let Some(p) = color.icc.as_deref().and_then(parse) {
                return Source::Profile(Box::new(p), None);
            }
            return Source::Exact(Space::Srgb);
        }
        Some(IccSpace::Other) => {
            notice!("note: non-RGB ICC profile does not match RGB pixels, treated as sRGB");
            return Source::Exact(Space::Srgb);
        }
        None => {}
    }
    if let Some(c) = &color.cicp {
        let (cp, tc) = (c.color_primaries, c.transfer_characteristics);
        if matches!(tc, 16 | 18) {
            let cp = if cp == 2 { 9 } else { cp };
            return Source::Hdr {
                primaries: u8::try_from(cp).unwrap_or(9),
                transfer: tc as u8,
            };
        }
        let trc = match tc {
            2 => Some(Trc::Srgb),
            t => icc::cicp_trc(t),
        };
        let prim = match cp {
            2 => Some(icc::SRGB),
            p => icc::cicp_primaries(p),
        };
        match (prim, trc) {
            (Some(p), Some(Trc::Srgb)) if p == icc::SRGB => return Source::Exact(Space::Srgb),
            (Some(p), Some(Trc::Srgb)) if p == icc::DISPLAY_P3 => return Source::Exact(Space::P3),
            (Some(p), Some(t)) => return Source::Profile(Box::new(synthesized(&p, t)), None),
            _ if !(cp == 2 && tc == 2) => {
                notice!("note: CICP {cp}/{tc} not supported for AVIF, treated as sRGB");
                return Source::Exact(Space::Srgb);
            }
            _ => {}
        }
    }
    if let Some(custom) = &color.custom {
        let (p, t) = custom_profile(custom);
        return Source::Profile(Box::new(synthesized(&p, t)), None);
    }
    if linear_default {
        return Source::Profile(
            Box::new(synthesized(&icc::SRGB, Trc::Linear)),
            Some(Space::Srgb),
        );
    }
    Source::Exact(Space::Srgb)
}

const LINEAR_GAMMA_EPS: f64 = 1e-4;

fn custom_profile(c: &CustomColor) -> (Primaries, Trc) {
    let trc = match c.gamma {
        // PNG gAMA stores the encoding exponent.
        Some(g) if (g - 1.0).abs() < LINEAR_GAMMA_EPS => Trc::Linear,
        Some(g) if g > 0.0 => Trc::Gamma(1.0 / g),
        _ => Trc::Srgb,
    };
    let prim = c
        .chromaticities
        .map_or(icc::SRGB, |[w, r, g, b]| Primaries { r, g, b, w });
    (prim, trc)
}

/// Smallest output space holding the source gamut. Profiles without
/// colorants (LUT-based) cannot be measured cheaply and get the widest.
fn classify(p: &ColorProfile) -> Space {
    if p.color_space == moxcms::DataColorSpace::Gray {
        return Space::Srgb;
    }
    if !p.is_matrix_shaper() {
        return Space::Wide;
    }
    let xy =
        [p.red_colorant, p.green_colorant, p.blue_colorant].map(|c| icc::xy_of([c.x, c.y, c.z]));
    [Space::Srgb, Space::P3]
        .into_iter()
        .find(|s| icc::triangle_contains(&icc::pcs_colorant_xy(&s.primaries()), &xy))
        .unwrap_or(Space::Wide)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Cicp;

    fn px(rgb: [u16; 3]) -> Rgb16 {
        Rgb16 {
            width: 1,
            height: 1,
            alpha: false,
            data: rgb.to_vec(),
        }
    }

    fn icc_desc(p: &Primaries, trc: Trc) -> ColorDesc {
        ColorDesc::new(None, Some(icc::rgb_profile(p, trc, "test")))
    }

    #[test]
    fn untagged_and_srgb_cicp_pass_through() {
        let mut img = px([1000, 2000, 3000]);
        let s = convert(&mut img, &ColorDesc::default(), false, 1).unwrap();
        assert_eq!((s.primaries, s.transfer, s.matrix), (1, 13, 6));
        assert_eq!(img.data, [1000, 2000, 3000]);
        let cicp = ColorDesc::new(
            Some(Cicp {
                color_primaries: 12,
                transfer_characteristics: 13,
            }),
            None,
        );
        assert_eq!(
            convert(&mut img, &cicp, false, 1).unwrap().space,
            Some(Space::P3)
        );
        assert_eq!(img.data, [1000, 2000, 3000]);
    }

    #[test]
    fn source_gamut_picks_the_space() {
        let adobe = Primaries {
            r: [0.64, 0.33],
            g: [0.21, 0.71],
            b: [0.15, 0.06],
            w: icc::D65,
        };
        for (p, want) in [
            (icc::SRGB, Space::Srgb),
            (icc::DISPLAY_P3, Space::P3),
            (adobe, Space::Wide),
            (icc::cicp_primaries(9).unwrap(), Space::Wide),
        ] {
            let mut img = px([30000; 3]);
            let s = convert(&mut img, &icc_desc(&p, Trc::Srgb), false, 1).unwrap();
            assert_eq!(s.space, Some(want), "{p:?}");
            // Neutral gray stays neutral and keeps its level.
            assert!(
                img.data.iter().all(|&v| v.abs_diff(30000) < 40),
                "{:?}",
                img.data
            );
        }
    }

    #[test]
    fn threaded_transform_matches_one_thread() {
        let data: Vec<u16> = (0..7 * 5 * 3).map(|i| (i * 7919 % 65536) as u16).collect();
        let run = |threads| {
            let mut img = Rgb16 {
                width: 7,
                height: 5,
                alpha: false,
                data: data.clone(),
            };
            let desc = icc_desc(&icc::cicp_primaries(9).unwrap(), Trc::Srgb);
            convert(&mut img, &desc, false, threads).unwrap();
            img.data
        };
        let one = run(1);
        assert_ne!(one, data);
        for threads in [2, 3, 64] {
            assert_eq!(run(threads), one, "{threads} threads");
        }
    }

    #[test]
    fn threaded_gray_transform_matches_one_thread() {
        let icc = ColorProfile::new_gray_with_gamma(1.8).encode().unwrap();
        let data: Vec<u16> = (0..7 * 5)
            .flat_map(|i| [(i * 1931 % 65536) as u16; 3])
            .collect();
        let run = |threads| {
            let mut img = Rgb16 {
                width: 7,
                height: 5,
                alpha: false,
                data: data.clone(),
            };
            convert(
                &mut img,
                &ColorDesc::new(None, Some(icc.clone())),
                false,
                threads,
            )
            .unwrap();
            img.data
        };
        let one = run(1);
        assert_ne!(one, data);
        for threads in [2, 3, 64] {
            assert_eq!(run(threads), one, "{threads} threads");
        }
    }

    #[test]
    fn p3_red_is_preserved_in_the_wide_space() {
        let mut img = px([65535, 0, 0]);
        convert(
            &mut img,
            &icc_desc(&icc::DISPLAY_P3, Trc::Gamma(2.2)),
            false,
            1,
        )
        .unwrap();
        // P3 with a gamma curve is converted to the P3 output (same primaries).
        assert!(
            img.data[0] > 65000 && img.data[1] < 600 && img.data[2] < 600,
            "{:?}",
            img.data
        );
    }

    #[test]
    fn hdr_keeps_cicp_and_pixels() {
        let mut img = px([1, 2, 3]);
        let pq = ColorDesc::new(
            Some(Cicp {
                color_primaries: 9,
                transfer_characteristics: 16,
            }),
            None,
        );
        let s = convert(&mut img, &pq, false, 1).unwrap();
        assert_eq!((s.primaries, s.transfer, s.matrix, s.icc), (9, 16, 9, None));
        assert_eq!(img.data, [1, 2, 3]);
    }

    #[test]
    fn linear_float_default_is_encoded_to_srgb() {
        let mut img = px([65535 / 2; 3]);
        convert(&mut img, &ColorDesc::default(), true, 1).unwrap();
        // Linear 0.5 is sRGB ~0.735.
        assert!(
            img.data.iter().all(|&v| v.abs_diff(48200) < 300),
            "{:?}",
            img.data
        );
    }
}
