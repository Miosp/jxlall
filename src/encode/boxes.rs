//! JXL container boxes: metadata transfer and the `jhgm` gain map bundle.

use super::encoder::{check, err, Enc, EncodeError};
use super::EncodeSettings;
use crate::domain::MetaBoxes;
use std::ffi::c_char;
use jpegxl_sys::color::color_encoding::JxlColorEncoding;
use jpegxl_sys::common::types::{JxlBoxType, JxlDataType, JxlEndianness, JxlPixelFormat};
use jpegxl_sys::encoder::encode::*;
use jpegxl_sys::metadata::codestream_header::JxlBasicInfo;
use std::ptr::null_mut;

fn box_type(name: &[u8; 4]) -> JxlBoxType {
    JxlBoxType([
        name[0] as c_char,
        name[1] as c_char,
        name[2] as c_char,
        name[3] as c_char,
    ])
}

pub(super) fn add_metadata_boxes(e: &Enc, meta: &MetaBoxes) -> Result<(), EncodeError> {
    unsafe {
        if meta.is_empty() {
            return Ok(());
        }
        check(JxlEncoderUseBoxes(e.enc), "UseBoxes")?;
        if let Some(exif) = meta.exif.as_deref() {
            // Exif box content starts with a 4-byte offset to the TIFF header;
            // 0 = header follows immediately.
            let mut payload = Vec::with_capacity(4 + exif.len());
            payload.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(exif);
            let t = box_type(b"Exif");
            check(
                JxlEncoderAddBox(e.enc, &t, payload.as_ptr(), payload.len(), true.into()),
                "AddBox(Exif)",
            )?;
        }
        if let Some(xmp) = meta.xmp.as_deref() {
            let t = box_type(b"xml ");
            check(
                JxlEncoderAddBox(e.enc, &t, xmp.as_ptr(), xmp.len(), true.into()),
                "AddBox(XMP)",
            )?;
        }
        if let Some(jhgm) = meta.jhgm.as_deref() {
            // jhgm holds a naked JXL codestream; brotli on top is useless.
            let t = box_type(b"jhgm");
            check(
                JxlEncoderAddBox(e.enc, &t, jhgm.as_ptr(), jhgm.len(), false.into()),
                "AddBox(jhgm)",
            )?;
        }
        JxlEncoderCloseBoxes(e.enc);
        Ok(())
    }
}

/// Encode an 8-bit grayscale image as a NAKED JXL codestream (no container,
/// no boxes) — the gain map image inside `jhgm`.
pub fn encode_gain_map_gray(
    width: u32,
    height: u32,
    gray: &[u8],
    settings: &EncodeSettings,
) -> Result<Vec<u8>, EncodeError> {
    let e = Enc::new()?;
    unsafe {
        let mut bi = std::mem::MaybeUninit::<JxlBasicInfo>::uninit();
        JxlEncoderInitBasicInfo(bi.as_mut_ptr());
        let mut bi = bi.assume_init();
        bi.xsize = width;
        bi.ysize = height;
        bi.num_color_channels = 1;
        bi.bits_per_sample = 8;
        bi.uses_original_profile = true.into();
        check(
            JxlEncoderSetBasicInfo(e.enc, &raw const bi),
            "SetBasicInfo(gain map)",
        )?;
        let mut ce = std::mem::MaybeUninit::<JxlColorEncoding>::uninit();
        JxlColorEncodingSetToSRGB(ce.as_mut_ptr(), true.into());
        let ce = ce.assume_init();
        check(
            JxlEncoderSetColorEncoding(e.enc, &raw const ce),
            "SetColorEncoding(gain map)",
        )?;
        let fs = JxlEncoderFrameSettingsCreate(e.enc, null_mut());
        if fs.is_null() {
            return err("FrameSettingsCreate failed");
        }
        check(
            JxlEncoderFrameSettingsSetOption(fs, JxlEncoderFrameSettingId::Effort, settings.effort.value()),
            "set effort (gain map)",
        )?;
        check(
            JxlEncoderSetFrameLossless(fs, true.into()),
            "SetFrameLossless(gain map)",
        )?;
        let format = JxlPixelFormat {
            num_channels: 1,
            data_type: JxlDataType::Uint8,
            endianness: JxlEndianness::Native,
            align: 0,
        };
        check(
            JxlEncoderAddImageFrame(fs, &format, gray.as_ptr().cast(), gray.len()),
            "AddImageFrame(gain map)",
        )?;
        JxlEncoderCloseInput(e.enc);
    }
    e.collect_output()
}

/// Serialize a `jhgm` box payload (JxlGainMapBundle, ISO/IEC 21496-1).
///
/// Mirrors `JxlGainMapWriteBundle` (libjxl `lib/extras/gain_map.cc`) for the
/// `has_color_encoding = false, alt_icc = null` case — the upstream C symbols
/// live in `jxl_extras`, which the vendored build does not produce:
/// `version u8 | metadata_size BE16 | metadata | color_encoding_size u8 (0)
///  | alt_icc_size BE32 (0) | gain map codestream`.
pub fn build_jhgm_box(
    metadata_iso: &[u8],
    gain_codestream: &[u8],
) -> Result<Vec<u8>, EncodeError> {
    if metadata_iso.len() > u16::MAX as usize {
        return err("gain map ISO metadata exceeds 64 KiB");
    }
    let mut out = Vec::with_capacity(1 + 2 + metadata_iso.len() + 1 + 4 + gain_codestream.len());
    out.push(0); // jhgm_version
    out.extend_from_slice(&(metadata_iso.len() as u16).to_be_bytes());
    out.extend_from_slice(metadata_iso);
    out.push(0); // color_encoding_size: none
    out.extend_from_slice(&0u32.to_be_bytes()); // alt_icc_size: none
    out.extend_from_slice(gain_codestream);
    Ok(out)
}
