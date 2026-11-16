//! SVT-AV1 (Tritium fork) still-image encoding.
//!
//! Every encoder option is passed by name (the `SvtAv1EncApp` long option
//! without dashes), so this crate never mirrors the fork-specific config
//! struct layout.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::{Mutex, Once};

#[repr(C)]
struct Param {
    key: *const c_char,
    value: *const c_char,
}

/// Luma-only AV1 film grain parameters (`AomFilmGrain` subset).
#[repr(C)]
#[derive(Clone, Debug, PartialEq)]
pub struct Grain {
    pub num_y_points: i32,
    /// `[x, scaling]`, 8-bit scale, increasing x.
    pub scaling_points_y: [[i32; 2]; 14],
    pub scaling_shift: i32,
    pub ar_coeff_shift: i32,
    pub overlap_flag: i32,
    pub chroma_scaling_from_luma: i32,
    pub clip_to_restricted_range: i32,
    pub bit_depth: i32,
    pub random_seed: u16,
}

#[repr(C)]
struct Picture {
    width: u32,
    height: u32,
    bit_depth: u32,
    color_format: u32,
    y: *const c_void,
    u: *const c_void,
    v: *const c_void,
    y_stride: u32,
    uv_stride: u32,
    uv_height: u32,
}

unsafe extern "C" {
    fn jxlall_svt_set_logger(f: extern "C" fn(c_int, *const c_char));
    fn jxlall_svt_free(p: *mut u8);
    fn jxlall_svt_encode(
        params: *const Param,
        n_params: usize,
        grain: *const Grain,
        pic: *const Picture,
        out: *mut *mut u8,
        out_len: *mut usize,
        failed_param: *mut usize,
    ) -> c_int;
}

/// 10-bit 4:2:0 planes, one `u16` per sample.
pub struct Yuv420<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u16],
    pub u: &'a [u16],
    pub v: &'a [u16],
}

impl Yuv420<'_> {
    pub fn chroma_size(width: u32, height: u32) -> (u32, u32) {
        (width.div_ceil(2), height.div_ceil(2))
    }
}

/// Recent SVT error messages; the logger is process-global and encodes may
/// run concurrently, so errors are matched to a failure only loosely.
static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
const MAX_ERRORS: usize = 16;
const LOG_ERROR: c_int = 1;

extern "C" fn log(level: c_int, msg: *const c_char) {
    if level > LOG_ERROR || msg.is_null() {
        return;
    }
    // SAFETY: the shim passes a NUL-terminated buffer valid for this call.
    let msg = unsafe { std::ffi::CStr::from_ptr(msg) }.to_string_lossy();
    let mut errors = ERRORS.lock().unwrap_or_else(|e| e.into_inner());
    if errors.len() == MAX_ERRORS {
        errors.remove(0);
    }
    errors.push(msg.trim().to_owned());
}

fn take_errors() -> String {
    let mut errors = ERRORS.lock().unwrap_or_else(|e| e.into_inner());
    let s = errors.join("; ");
    errors.clear();
    s
}

/// Encode one 10-bit 4:2:0 picture; returns the raw OBU stream.
pub fn encode_yuv420_10(
    params: &[(&str, &str)],
    grain: Option<&Grain>,
    pic: &Yuv420,
) -> Result<Vec<u8>, String> {
    static LOGGER: Once = Once::new();
    // SAFETY: plain function pointer registration, before any encoder exists.
    LOGGER.call_once(|| unsafe { jxlall_svt_set_logger(log) });

    let (cw, ch) = Yuv420::chroma_size(pic.width, pic.height);
    let luma = pic.width as usize * pic.height as usize;
    let chroma = cw as usize * ch as usize;
    if pic.y.len() != luma || pic.u.len() != chroma || pic.v.len() != chroma {
        return Err("internal: plane sizes do not match the picture".into());
    }
    let owned: Vec<(CString, CString)> = params
        .iter()
        .map(|(k, v)| Ok((CString::new(*k)?, CString::new(*v)?)))
        .collect::<Result<_, std::ffi::NulError>>()
        .map_err(|e| format!("encoder option: {e}"))?;
    let raw: Vec<Param> = owned
        .iter()
        .map(|(k, v)| Param {
            key: k.as_ptr(),
            value: v.as_ptr(),
        })
        .collect();
    let picture = Picture {
        width: pic.width,
        height: pic.height,
        bit_depth: 10,
        color_format: 1,
        y: pic.y.as_ptr().cast(),
        u: pic.u.as_ptr().cast(),
        v: pic.v.as_ptr().cast(),
        y_stride: pic.width,
        uv_stride: cw,
        uv_height: ch,
    };
    let mut out = std::ptr::null_mut();
    let mut out_len = 0usize;
    let mut failed = 0usize;
    // SAFETY: all pointers outlive the call; the shim copies nothing it keeps.
    let rc = unsafe {
        jxlall_svt_encode(
            raw.as_ptr(),
            raw.len(),
            grain.map_or(std::ptr::null(), |g| g as *const Grain),
            &picture,
            &mut out,
            &mut out_len,
            &mut failed,
        )
    };
    if rc != 0 {
        let stage = match rc {
            1 => "init_handle".to_owned(),
            2 => {
                let (k, v) = params.get(failed).copied().unwrap_or(("?", ""));
                return Err(format!("SVT-AV1 rejected option {k}={v}"));
            }
            3 => "set_parameter".to_owned(),
            4 => "init".to_owned(),
            5 => "send_picture".to_owned(),
            6 => "get_packet".to_owned(),
            7 => return Err("SVT-AV1: out of memory".into()),
            n => format!("error {n}"),
        };
        let detail = take_errors();
        return Err(if detail.is_empty() {
            format!("SVT-AV1 {stage} failed")
        } else {
            format!("SVT-AV1 {stage} failed: {detail}")
        });
    }
    // SAFETY: on success the shim returns a malloc'd buffer of out_len bytes.
    let bytes = unsafe { std::slice::from_raw_parts(out, out_len) }.to_vec();
    unsafe { jxlall_svt_free(out) };
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_small_gray_picture() {
        let (w, h) = (67, 33); // odd sizes on purpose
        let (cw, ch) = Yuv420::chroma_size(w, h);
        let y: Vec<u16> = (0..w * h).map(|i| (i % 1024) as u16).collect();
        let u = vec![512u16; (cw * ch) as usize];
        let v = u.clone();
        let params = [("avif", "1"), ("preset", "10"), ("crf", "30"), ("lp", "1")];
        let obus = encode_yuv420_10(
            &params,
            None,
            &Yuv420 {
                width: w,
                height: h,
                y: &y,
                u: &u,
                v: &v,
            },
        )
        .unwrap();
        assert!(obus.len() > 16);
    }

    #[test]
    fn unknown_option_is_reported() {
        let y = vec![0u16; 64 * 64];
        let c = vec![512u16; 32 * 32];
        let err = encode_yuv420_10(
            &[("no-such-option", "1")],
            None,
            &Yuv420 {
                width: 64,
                height: 64,
                y: &y,
                u: &c,
                v: &c,
            },
        )
        .unwrap_err();
        assert!(err.contains("no-such-option"), "{err}");
    }
}
