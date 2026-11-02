//! libjxl encoding via raw `jpegxl-sys` FFI.

mod boxes;
mod color;
mod encoder;

pub use boxes::{build_jhgm_box, encode_gain_map_gray};
pub use encoder::{Enc, EncodeError};

use self::encoder::{check, err};
use crate::domain::{ColorDesc, IccSpace, ImageFrame, MetaBoxes, Orientation, Pixels};
use boxes::add_metadata_boxes;
use color::{ColorChoice, set_color};
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
        (1..=10)
            .contains(&v)
            .then_some(Self(v))
            .ok_or_else(|| "effort must be 1..=10".into())
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
        (0.0..=25.0)
            .contains(&v)
            .then_some(Self(v))
            .ok_or_else(|| "distance must be 0..=25".into())
    }
}

/// Photon-noise strength: multiplier on the full-frame-equivalent ISO.
#[derive(Debug, Clone, Copy)]
pub struct NoiseStrength(f32);

impl NoiseStrength {
    pub fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for NoiseStrength {
    type Error = String;

    fn try_from(v: f32) -> Result<Self, Self::Error> {
        (v > 0.0 && v <= 100.0)
            .then_some(Self(v))
            .ok_or_else(|| "noise strength must be in (0, 100]".into())
    }
}

/// Encoder knobs threaded through every encode call.
#[derive(Debug, Clone, Copy)]
pub struct EncodeSettings {
    pub effort: Effort,
    pub distance: Distance,
    /// `Some` when grain synthesis is enabled (`--add-noise`).
    pub noise: Option<NoiseStrength>,
    /// libjxl worker threads for this file.
    pub threads: usize,
}

/// Decode-time grain synthesis at the given ISO (full-frame equivalent).
unsafe fn set_photon_noise(
    fs: *mut JxlEncoderFrameSettings,
    iso: Option<f32>,
) -> Result<(), EncodeError> {
    match iso {
        Some(iso) => unsafe {
            check(
                JxlEncoderFrameSettingsSetFloatOption(
                    fs,
                    JxlEncoderFrameSettingId::PhotonNoise,
                    iso,
                ),
                "set photon noise",
            )
        },
        None => Ok(()),
    }
}

/// Lossless JPEG → JXL recompression (jbrd). JPEG bytes must be a complete file.
///
/// `extra_xmp`: `xml ` box for a JPEG without XMP of its own. Reconstruction
/// only restores XMP the JPEG had, so the restored file stays byte-exact.
pub fn encode_jpeg(
    jpeg: &[u8],
    settings: &EncodeSettings,
    extra_xmp: Option<&[u8]>,
) -> Result<Vec<u8>, EncodeError> {
    let e = Enc::new(settings.threads)?;
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
        // Same box policy as the pixel path (see boxes.rs): no `brob`.
        check(
            JxlEncoderFrameSettingsSetOption(
                settings_handle,
                JxlEncoderFrameSettingId::JpegCompressBoxes,
                0,
            ),
            "set jpeg_compress_boxes",
        )?;
        // EXIF/XMP/JUMBF preservation is the default since libjxl 0.10.
        let s = JxlEncoderAddJPEGFrame(settings_handle, jpeg.as_ptr(), jpeg.len());
        if s == JxlEncoderStatus::Error {
            return err("jbrd: unsupported JPEG (CMYK, arithmetic, 12-bit, or corrupt)");
        }
        check(s, "AddJPEGFrame")?;
        if let Some(xmp) = extra_xmp {
            add_metadata_boxes(&e, &MetaBoxes::new(None, Some(xmp.to_vec())))?;
        }
        JxlEncoderCloseInput(e.enc);
    }
    e.collect_output()
}

/// Pixel path: encode one frame with explicit color/metadata/orientation.
pub fn encode_image(
    frame: &ImageFrame,
    color: &ColorDesc,
    meta: &MetaBoxes,
    orientation: Orientation,
    hdr: &crate::domain::Hdr,
    settings: &EncodeSettings,
    photon_noise_iso: Option<f32>,
) -> Result<Vec<u8>, EncodeError> {
    let e = Enc::new(settings.threads)?;
    unsafe {
        check(JxlEncoderUseContainer(e.enc, true.into()), "UseContainer")?;
        let mut bi = std::mem::MaybeUninit::<JxlBasicInfo>::uninit();
        JxlEncoderInitBasicInfo(bi.as_mut_ptr());
        let mut bi = bi.assume_init();
        bi.xsize = frame.width;
        bi.ysize = frame.height;
        let (choice, gray_pixels) = resolve_color(frame, color);
        bi.num_color_channels = if choice.gray { 1 } else { 3 };
        bi.orientation = orientation.jxl();
        let bit_depth = stored_bit_depth(frame);
        if frame.has_alpha {
            bi.num_extra_channels = 1;
            bi.alpha_bits = u32::from(bit_depth);
            bi.alpha_premultiplied = frame.alpha_premultiplied.into();
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
        apply_bit_depth(&mut bi, frame, bit_depth);
        if settings.distance.is_lossless() {
            // Lossless requires keeping the source color profile untouched.
            bi.uses_original_profile = true.into();
        }
        check(JxlEncoderSetBasicInfo(e.enc, &raw const bi), "SetBasicInfo")?;
        set_color(&e, &choice)?;

        let fs = JxlEncoderFrameSettingsCreate(e.enc, null_mut());
        if fs.is_null() {
            return err("FrameSettingsCreate failed");
        }
        check(
            JxlEncoderFrameSettingsSetOption(
                fs,
                JxlEncoderFrameSettingId::Effort,
                settings.effort.value(),
            ),
            "set effort",
        )?;
        if settings.distance.is_lossless() {
            check(
                JxlEncoderSetFrameLossless(fs, true.into()),
                "SetFrameLossless",
            )?;
        } else {
            // Grain is calibrated for XYB only: lossless frames (original
            // color space) get it colored, jbrd frames (YCbCr) only above
            // mid-gray. So lossy pixel frames only.
            set_photon_noise(fs, photon_noise_iso)?;
            check(
                JxlEncoderSetFrameDistance(fs, settings.distance.get()),
                "SetFrameDistance",
            )?;
        }

        let pixels = gray_pixels.as_ref().unwrap_or(&frame.pixels);
        let format = pixel_format(pixels, frame.has_alpha, choice.gray);
        let (ptr, len_bytes) = pixels.as_raw_parts();
        let ptr: *const c_void = ptr.cast();
        check(
            JxlEncoderAddImageFrame(fs, &format, ptr, len_bytes),
            "AddImageFrame",
        )?;
        add_metadata_boxes(&e, meta)?;
        JxlEncoderCloseInput(e.enc);
    }
    e.collect_output()
}

/// Full-range 16-bit value of 8-bit sample 1.
const U8_TO_U16_SCALE: u16 = 257;

/// Bits to declare for this frame. 8-bit content in 16-bit storage is declared
/// as 8 bits: it decodes to the same values and spares libjxl a sparse range.
fn stored_bit_depth(frame: &ImageFrame) -> u8 {
    match &frame.pixels {
        Pixels::U16(v) if frame.bit_depth == 16 && v.iter().all(|s| s % U8_TO_U16_SCALE == 0) => 8,
        _ => frame.bit_depth,
    }
}

fn apply_bit_depth(bi: &mut JxlBasicInfo, frame: &ImageFrame, bit_depth: u8) {
    match &frame.pixels {
        Pixels::U8(_) => {
            bi.bits_per_sample = u32::from(bit_depth.min(8));
            bi.exponent_bits_per_sample = 0;
        }
        Pixels::U16(_) => {
            bi.bits_per_sample = u32::from(bit_depth);
            bi.exponent_bits_per_sample = 0;
        }
        Pixels::F32(_) => {
            bi.bits_per_sample = 32;
            bi.exponent_bits_per_sample = 8;
        }
    }
}

/// Pick the color signal for this frame. Decoders expand gray sources to
/// RGB; a gray ICC profile can only be honored by encoding gray, and a
/// profile of any other space (CMYK after conversion, Lab, ...) does not
/// describe the RGB samples, so it is dropped.
fn resolve_color<'a>(
    frame: &ImageFrame,
    color: &'a ColorDesc,
) -> (ColorChoice<'a>, Option<Pixels>) {
    let mut choice = ColorChoice {
        cicp: color.cicp.as_ref(),
        icc: None,
        custom: color.custom.as_ref(),
        gray: false,
        linear_default: matches!(frame.pixels, Pixels::F32(_)),
    };
    let mut gray_pixels = None;
    match color.icc_space() {
        Some(IccSpace::Rgb) => choice.icc = color.icc.as_deref(),
        Some(IccSpace::Gray) => match collapse_gray(&frame.pixels, frame.has_alpha) {
            Some(px) => {
                choice.icc = color.icc.as_deref();
                choice.gray = true;
                gray_pixels = Some(px);
            }
            None => eprintln!("note: gray ICC profile on color pixels dropped"),
        },
        Some(IccSpace::Other) => {
            eprintln!("note: non-RGB ICC profile (e.g. CMYK) does not match RGB pixels, dropped");
        }
        None => {}
    }
    (choice, gray_pixels)
}

/// Gray-only view of an RGB(A) buffer whose R, G and B are all equal.
fn collapse_gray(pixels: &Pixels, has_alpha: bool) -> Option<Pixels> {
    fn collapse<T: Copy + PartialEq>(v: &[T], has_alpha: bool) -> Option<Vec<T>> {
        let (stride, keep) = if has_alpha { (4, 2) } else { (3, 1) };
        let mut out = Vec::with_capacity(v.len() / stride * keep);
        for px in v.chunks_exact(stride) {
            if px[0] != px[1] || px[0] != px[2] {
                return None;
            }
            out.push(px[0]);
            if has_alpha {
                out.push(px[3]);
            }
        }
        Some(out)
    }
    match pixels {
        Pixels::U8(v) => collapse(v, has_alpha).map(Pixels::U8),
        Pixels::U16(v) => collapse(v, has_alpha).map(Pixels::U16),
        Pixels::F32(v) => collapse(v, has_alpha).map(Pixels::F32),
    }
}

fn pixel_format(pixels: &Pixels, has_alpha: bool, gray: bool) -> JxlPixelFormat {
    let color_channels = if gray { 1 } else { 3 };
    JxlPixelFormat {
        num_channels: color_channels + u32::from(has_alpha),
        data_type: match pixels {
            Pixels::U8(_) => JxlDataType::Uint8,
            Pixels::U16(_) => JxlDataType::Uint16,
            Pixels::F32(_) => JxlDataType::Float,
        },
        endianness: JxlEndianness::Native,
        align: 0,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal header-valid ICC profile of the given data color space.
    fn icc(space: &[u8; 4]) -> Vec<u8> {
        let mut p = vec![0u8; 128];
        p[..4].copy_from_slice(&128u32.to_be_bytes());
        p[16..20].copy_from_slice(space);
        p[36..40].copy_from_slice(b"acsp");
        p
    }

    fn frame(pixels: Pixels, has_alpha: bool) -> ImageFrame {
        ImageFrame {
            width: 2,
            height: 1,
            bit_depth: 8,
            has_alpha,
            alpha_premultiplied: false,
            pixels,
        }
    }

    #[test]
    fn gray_icc_on_expanded_gray_encodes_gray() {
        let f = frame(Pixels::U8(vec![5, 5, 5, 200, 9, 9, 9, 100]), true);
        let color = ColorDesc::new(None, Some(icc(b"GRAY")));
        let (choice, px) = resolve_color(&f, &color);
        assert!(choice.gray);
        assert!(choice.icc.is_some());
        assert!(matches!(px, Some(Pixels::U8(v)) if v == [5, 200, 9, 100]));
    }

    #[test]
    fn gray_icc_on_color_pixels_is_dropped() {
        let f = frame(Pixels::U16(vec![1, 2, 3, 4, 4, 4]), false);
        let color = ColorDesc::new(None, Some(icc(b"GRAY")));
        let (choice, px) = resolve_color(&f, &color);
        assert!(!choice.gray && choice.icc.is_none() && px.is_none());
    }

    #[test]
    fn cmyk_icc_is_dropped_rgb_kept() {
        let f = frame(Pixels::U8(vec![1, 2, 3, 4, 5, 6]), false);
        let cmyk = ColorDesc::new(None, Some(icc(b"CMYK")));
        assert!(resolve_color(&f, &cmyk).0.icc.is_none());
        let rgb = ColorDesc::new(None, Some(icc(b"RGB ")));
        assert!(resolve_color(&f, &rgb).0.icc.is_some());
    }

    /// 16x16 baseline RGB JPEG, optionally with an APP1 segment.
    pub(crate) fn test_jpeg(app1: Option<Vec<u8>>) -> Vec<u8> {
        const SIDE: u16 = 16;
        let pixels: Vec<u8> = (0..usize::from(SIDE) * usize::from(SIDE) * 3)
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        let mut out = Vec::new();
        let mut enc = jpeg_encoder::Encoder::new(&mut out, 90);
        if let Some(body) = app1 {
            enc.add_app_segment(1, body).unwrap();
        }
        enc.encode(&pixels, SIDE, SIDE, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        out
    }

    /// JPEG restored from a jbrd JXL, as `djxl` would produce it.
    fn reconstruct(jxl: &[u8]) -> Vec<u8> {
        use jpegxl_sys::decode::*;
        const CHUNK: usize = 1 << 16;
        unsafe {
            let dec = JxlDecoderCreate(std::ptr::null());
            let events =
                JxlDecoderStatus::JPEGReconstruction as i32 | JxlDecoderStatus::FullImage as i32;
            assert_eq!(
                JxlDecoderSubscribeEvents(dec, events),
                JxlDecoderStatus::Success
            );
            assert_eq!(
                JxlDecoderSetInput(dec, jxl.as_ptr(), jxl.len()),
                JxlDecoderStatus::Success
            );
            JxlDecoderCloseInput(dec);
            let mut out = vec![0u8; CHUNK];
            let mut used;
            loop {
                match JxlDecoderProcessInput(dec) {
                    JxlDecoderStatus::JPEGReconstruction => {
                        JxlDecoderSetJPEGBuffer(dec, out.as_mut_ptr(), out.len());
                    }
                    JxlDecoderStatus::JPEGNeedMoreOutput => {
                        used = out.len() - JxlDecoderReleaseJPEGBuffer(dec);
                        out.resize(out.len() + CHUNK, 0);
                        JxlDecoderSetJPEGBuffer(dec, out[used..].as_mut_ptr(), out.len() - used);
                    }
                    JxlDecoderStatus::FullImage => {}
                    JxlDecoderStatus::Success => {
                        used = out.len() - JxlDecoderReleaseJPEGBuffer(dec);
                        break;
                    }
                    s => panic!("JPEG reconstruction failed: {s:?}"),
                }
            }
            JxlDecoderDestroy(dec);
            out.truncate(used);
            out
        }
    }

    fn settings() -> EncodeSettings {
        EncodeSettings {
            effort: Effort::try_from(1).unwrap(),
            distance: Distance::try_from(0.0).unwrap(),
            noise: None,
            threads: 1,
        }
    }

    /// Decode a JXL to interleaved RGB u16, with the depth the file declares.
    fn decode_u16(jxl: &[u8]) -> (u32, Vec<u16>) {
        use jpegxl_sys::decode::*;
        unsafe {
            let dec = JxlDecoderCreate(std::ptr::null());
            let events = JxlDecoderStatus::BasicInfo as i32 | JxlDecoderStatus::FullImage as i32;
            assert_eq!(
                JxlDecoderSubscribeEvents(dec, events),
                JxlDecoderStatus::Success
            );
            assert_eq!(
                JxlDecoderSetInput(dec, jxl.as_ptr(), jxl.len()),
                JxlDecoderStatus::Success
            );
            JxlDecoderCloseInput(dec);
            let format = JxlPixelFormat {
                num_channels: 3,
                data_type: JxlDataType::Uint16,
                endianness: JxlEndianness::Native,
                align: 0,
            };
            let mut bits = 0;
            let mut out = Vec::new();
            loop {
                match JxlDecoderProcessInput(dec) {
                    JxlDecoderStatus::BasicInfo => {
                        let mut info = std::mem::MaybeUninit::<JxlBasicInfo>::uninit();
                        assert_eq!(
                            JxlDecoderGetBasicInfo(dec, info.as_mut_ptr()),
                            JxlDecoderStatus::Success
                        );
                        let info = info.assume_init();
                        bits = info.bits_per_sample;
                        out = vec![0u16; (info.xsize * info.ysize * 3) as usize];
                    }
                    JxlDecoderStatus::NeedImageOutBuffer => {
                        assert_eq!(
                            JxlDecoderSetImageOutBuffer(
                                dec,
                                &format,
                                out.as_mut_ptr().cast(),
                                out.len() * 2,
                            ),
                            JxlDecoderStatus::Success
                        );
                    }
                    JxlDecoderStatus::FullImage => {}
                    JxlDecoderStatus::Success => break,
                    s => panic!("decode failed: {s:?}"),
                }
            }
            JxlDecoderDestroy(dec);
            (bits, out)
        }
    }

    #[test]
    fn full_range_ten_bit_samples_round_trip_at_ten_bits() {
        const TEN_BIT_MAX: u32 = 1023;
        let pixels: Vec<u16> = (0..=TEN_BIT_MAX)
            .flat_map(|v| [v, TEN_BIT_MAX - v, v / 2])
            .map(|v| ((v * 65535 + 512) / TEN_BIT_MAX) as u16)
            .collect();
        let img = ImageFrame {
            width: TEN_BIT_MAX + 1,
            height: 1,
            bit_depth: 10,
            has_alpha: false,
            alpha_premultiplied: false,
            pixels: Pixels::U16(pixels.clone()),
        };
        let jxl = encode_image(
            &img,
            &ColorDesc::default(),
            &MetaBoxes::default(),
            Orientation::new(1),
            &crate::domain::Hdr::default(),
            &settings(),
            None,
        )
        .unwrap();
        let (bits, decoded) = decode_u16(&jxl);
        assert_eq!(bits, 10);
        assert_eq!(decoded, pixels);
    }

    fn encode_rgb16_frame(pixels: Vec<u16>) -> Vec<u8> {
        let img = ImageFrame {
            width: (pixels.len() / 3) as u32,
            height: 1,
            bit_depth: 16,
            has_alpha: false,
            alpha_premultiplied: false,
            pixels: Pixels::U16(pixels),
        };
        encode_image(
            &img,
            &ColorDesc::default(),
            &MetaBoxes::default(),
            Orientation::new(1),
            &crate::domain::Hdr::default(),
            &settings(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn upsampled_eight_bit_samples_are_stored_at_eight_bits() {
        let pixels: Vec<u16> = (0..=255u16)
            .flat_map(|v| [v, 255 - v, v / 2])
            .map(|v| v * 257)
            .collect();
        let (bits, decoded) = decode_u16(&encode_rgb16_frame(pixels.clone()));
        assert_eq!(bits, 8);
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn one_true_sixteen_bit_sample_keeps_sixteen_bits() {
        let mut pixels: Vec<u16> = (0..=255u16)
            .flat_map(|v| [v, v, v])
            .map(|v| v * 257)
            .collect();
        pixels[100] += 1;
        let (bits, decoded) = decode_u16(&encode_rgb16_frame(pixels.clone()));
        assert_eq!(bits, 16);
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn thread_count_does_not_change_the_output() {
        const SIDE: u32 = 600;
        let pixels: Vec<u8> = (0..SIDE * SIDE * 3).map(|i| (i * 7 % 251) as u8).collect();
        let img = ImageFrame {
            width: SIDE,
            height: SIDE,
            bit_depth: 8,
            has_alpha: false,
            alpha_premultiplied: false,
            pixels: Pixels::U8(pixels),
        };
        let encode = |threads| {
            encode_image(
                &img,
                &ColorDesc::default(),
                &MetaBoxes::default(),
                Orientation::new(1),
                &crate::domain::Hdr::default(),
                &EncodeSettings {
                    threads,
                    ..settings()
                },
                None,
            )
            .unwrap()
        };
        assert_eq!(encode(1), encode(4));
    }

    #[test]
    fn extra_xmp_box_keeps_jpeg_reconstruction_byte_exact() {
        let jpeg = test_jpeg(None);
        let xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>";
        let jxl = encode_jpeg(&jpeg, &settings(), Some(xmp)).unwrap();
        assert!(jxl.windows(xmp.len()).any(|w| w == xmp), "xml box present");
        assert_eq!(reconstruct(&jxl), jpeg);
    }

    #[test]
    fn same_length_xmp_edit_reconstructs_edited_jpeg() {
        use crate::domain::{Rating, Score};
        use crate::format::{jpeg_markers::scan_segments, xmp_rating};
        let packet = format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about=""/></rdf:RDF></x:xmpmeta>{}<?xpacket end="w"?>"#,
            " ".repeat(2048)
        );
        let mut body = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        body.extend_from_slice(packet.as_bytes());
        let mut jpeg = test_jpeg(Some(body));
        let range = scan_segments(&jpeg).xmp_packet.unwrap();
        let score = Score {
            value: 0.5,
            model: "m",
        };
        let rating = Rating {
            technical: score,
            aesthetic: score,
            stars: Some(3),
            overwrite_stars: false,
        };
        let edited = xmp_rating::apply_same_length(&jpeg[range.clone()], &rating).unwrap();
        jpeg[range].copy_from_slice(&edited);
        let jxl = encode_jpeg(&jpeg, &settings(), None).unwrap();
        assert_eq!(reconstruct(&jxl), jpeg);
    }

    #[test]
    fn float_without_tag_defaults_to_linear() {
        let f = frame(Pixels::F32(vec![0.5; 6]), false);
        let color = ColorDesc::default();
        assert!(resolve_color(&f, &color).0.linear_default);
    }
}
