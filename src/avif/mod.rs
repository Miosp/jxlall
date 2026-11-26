//! AVIF output: SVT-AV1 (Tritium fork) and an own HEIF muxer.
//!
//! Always 10-bit 4:2:0 full range. Color is converted to sRGB, Display P3
//! or a P3 + Adobe RGB union gamut (see `color`), orientation is baked into
//! the pixels, and frames beyond SVT-AV1's size limit become grids.

mod color;
mod grain;
mod icc;
mod mux;
mod obu;
mod orient;
mod yuv;

use crate::domain::{ColorDesc, GainMapData, ImageFrame, MetaBoxes, Orientation, Pixels};
use color::Rgb16;
use yuv::Planes;

/// SVT-AV1 frame size limits.
const MAX_WIDTH: u32 = 16384;
const MAX_HEIGHT: u32 = 8704;
const BIT_DEPTH: u8 = 10;

/// Quality target, SVT-AV1 CRF 0..=70 (lower is better; quarter steps).
#[derive(Debug, Clone, Copy)]
pub struct Crf(f32);

impl TryFrom<f32> for Crf {
    type Error = String;

    fn try_from(v: f32) -> Result<Self, Self::Error> {
        (0.0..=70.0)
            .contains(&v)
            .then_some(Self(v))
            .ok_or_else(|| "CRF must be 0..=70".into())
    }
}

impl Crf {
    pub fn get(self) -> f32 {
        self.0
    }
}

/// Encoder speed preset (lower is slower and better).
#[derive(Debug, Clone, Copy)]
pub struct Preset(i32);

impl Preset {
    /// `-e` 1..=10 maps onto presets 11..=2: the default effort 7 is preset
    /// 5. Other presets are reachable with `-x preset=N`.
    pub fn from_effort(effort: crate::encode::Effort) -> Self {
        Self(12 - effort.value() as i32)
    }

    pub fn get(self) -> i32 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct AvifSettings {
    pub crf: Crf,
    pub preset: Preset,
    /// Raw SVT-AV1 options (`key`, `value`), applied after ours.
    pub svt: Vec<(String, String)>,
}

/// Still-image tuning carried over from the avif-conv-test prototype.
const TUNING: [(&str, &str); 5] = [
    ("tune", "3"),
    ("tx-bias", "1"),
    ("hbd-mds", "1"),
    ("complex-hvs", "1"),
    ("noise-adaptive-filtering", "1"),
];

/// SVT-AV1 checks option names and values only when encoding: encode a
/// tiny picture once so a bad `-x` is one error, not one per file.
pub fn validate_options(settings: &AvifSettings) -> Result<(), String> {
    if settings.svt.is_empty() {
        return Ok(());
    }
    let crf = settings.crf.get().to_string();
    let preset = settings.preset.get().to_string();
    let params: Vec<(&str, &str)> = [("avif", "1"), ("preset", &preset), ("crf", &crf)]
        .into_iter()
        .chain(TUNING)
        .chain(settings.svt.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .collect();
    const SIDE: u32 = 16;
    let (cw, ch) = svt_av1_tritium_sys::Yuv420::chroma_size(SIDE, SIDE);
    let luma = vec![512u16; (SIDE * SIDE) as usize];
    let chroma = vec![512u16; (cw * ch) as usize];
    svt_av1_tritium_sys::encode_yuv420_10(
        &params,
        None,
        &svt_av1_tritium_sys::Yuv420 {
            width: SIDE,
            height: SIDE,
            y: &luma,
            u: &chroma,
            v: &chroma,
        },
    )
    .map(drop)
    .map_err(|e| format!("-x: {e}"))
}

/// SVT-AV1 parallelism level (1..=6) for a thread budget, matching the
/// encoder's own core-count mapping.
fn parallelism_level(threads: usize) -> u32 {
    match threads {
        0 | 1 => 1,
        2 => 2,
        3..=5 => 3,
        6..=11 => 4,
        12..=23 => 5,
        _ => 6,
    }
}

/// Everything the AVIF encoder needs from a decoded page.
pub struct Input {
    pub frame: ImageFrame,
    pub color: ColorDesc,
    pub meta: MetaBoxes,
    pub orientation: Orientation,
    pub gain_map: Option<GainMapData>,
}

/// What the caller may log about an encode.
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub space: &'static str,
    pub tiles: usize,
}

pub fn encode(
    input: Input,
    settings: &AvifSettings,
    threads: usize,
    grain_iso: Option<f32>,
) -> Result<Encoded, String> {
    let Input {
        frame,
        color,
        meta,
        orientation,
        gain_map,
    } = input;
    let linear_default = matches!(frame.pixels, Pixels::F32(_));
    let mut img = to_rgb16(frame);
    if img.alpha && img.data.as_chunks::<4>().0.iter().all(|p| p[3] == u16::MAX) {
        img = drop_alpha(img);
    }
    let o = orientation.get();
    if o != 1 {
        let (data, w, h) = orient::apply(&img.data, img.width, img.height, img.channels(), o);
        (img.data, img.width, img.height) = (data, w, h);
    }
    let signal = color::convert(&mut img, &color, linear_default, threads)?;
    let space = match signal.space {
        Some(color::Space::Srgb) => "sRGB",
        Some(color::Space::P3) => "Display P3",
        Some(color::Space::Wide) => "wide gamut",
        None => "HDR (CICP)",
    };
    let wide = signal.space.is_some_and(color::Space::is_wide);
    let lp = parallelism_level(threads).to_string();
    let crf = settings.crf.get().to_string();
    let preset = settings.preset.get().to_string();
    let base: Vec<(&str, &str)> = [
        ("avif", "1"),
        ("preset", &preset),
        ("crf", &crf),
        ("lp", &lp),
    ]
    .into_iter()
    .chain(TUNING)
    .collect();
    let extra: Vec<(&str, &str)> = settings
        .svt
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let (cp, tc, mc) = (
        signal.primaries.to_string(),
        signal.transfer.to_string(),
        signal.matrix.to_string(),
    );
    let color_params: Vec<(&str, &str)> = base
        .iter()
        .copied()
        .chain([
            ("color-primaries", cp.as_str()),
            ("transfer-characteristics", tc.as_str()),
            ("matrix-coefficients", mc.as_str()),
            ("color-range", "1"),
        ])
        .chain(extra.iter().copied())
        .collect();
    // Grain is calibrated for the SDR sRGB curve.
    let grain_iso = grain_iso.filter(|_| signal.space.is_some());
    let (width, height) = (img.width, img.height);
    let planes = yuv::from_rgb(&img, signal.matrix, threads);
    let alpha_planes = img.alpha.then(|| yuv::from_alpha(&img));
    // The 16-bit RGB copy is as large as the planes: free it before SVT-AV1
    // allocates its own buffers.
    drop(img);
    let color_image = encode_image(&planes, 3, &color_params, |tile| {
        grain_iso.map(|iso| grain::photon_noise(iso, width, height, tile))
    })?;
    let tiles = color_image.tiles.len();
    drop(planes);

    // Alpha and gain maps are not color: unspecified CICP, no grain.
    let plain_params: Vec<(&str, &str)> = base
        .iter()
        .copied()
        .chain([("color-range", "1")])
        .chain(extra.iter().copied())
        .collect();
    let alpha = alpha_planes
        .map(|p| encode_image(&p, 1, &plain_params, |_| None))
        .transpose()?;
    let gain_map = match gain_map {
        Some(gm) => encode_gain_map(&gm, o, &plain_params)?,
        None => None,
    };

    let exif = meta.exif.as_deref().map(|e| {
        let mut e = e.clone();
        crate::domain::reset_exif_orientation(&mut e);
        if wide {
            orient::mark_exif_uncalibrated(&mut e);
        }
        e
    });
    let xmp = meta.xmp.as_deref().map(|x| {
        let mut x = x.clone();
        orient::reset_xmp_orientation(&mut x);
        x
    });
    let bytes = mux::write(mux::File {
        color: color_image,
        nclx: mux::Nclx {
            primaries: signal.primaries,
            transfer: signal.transfer,
            matrix: signal.matrix,
            full_range: true,
        },
        icc: signal.icc.as_deref(),
        alpha,
        gain_map,
        exif: exif.as_deref(),
        xmp: xmp.as_deref(),
    });
    Ok(Encoded {
        bytes,
        space,
        tiles,
    })
}

/// Tile grid for a `w` x `h` image: one tile when it fits the encoder,
/// otherwise equal even-sized tiles covering it.
fn layout(w: u32, h: u32) -> (u32, u32, u32, u32) {
    if w <= MAX_WIDTH && h <= MAX_HEIGHT {
        return (1, 1, w, h);
    }
    let (cols, rows) = (w.div_ceil(MAX_WIDTH), h.div_ceil(MAX_HEIGHT));
    let even = |v: u32| v + (v & 1);
    (cols, rows, even(w.div_ceil(cols)), even(h.div_ceil(rows)))
}

fn encode_image(
    planes: &Planes,
    channels: u8,
    params: &[(&str, &str)],
    grain: impl Fn(u16) -> Option<svt_av1_tritium_sys::Grain>,
) -> Result<mux::Image, String> {
    let (cols, rows, tw, th) = layout(planes.width, planes.height);
    let mut tiles = Vec::with_capacity((cols * rows) as usize);
    for r in 0..rows {
        for c in 0..cols {
            let tile = if cols * rows == 1 {
                None
            } else {
                Some(planes.tile(c * tw, r * th, tw, th))
            };
            let p = tile.as_ref().unwrap_or(planes);
            let g = grain((r * cols + c) as u16);
            let stream = svt_av1_tritium_sys::encode_yuv420_10(
                params,
                g.as_ref(),
                &svt_av1_tritium_sys::Yuv420 {
                    width: p.width,
                    height: p.height,
                    y: &p.y,
                    u: &p.u,
                    v: &p.v,
                },
            )?;
            let item = obu::to_item(&stream)?;
            tiles.push(mux::Coded {
                data: item.data,
                av1c: item.av1c,
                level: item.level,
            });
        }
    }
    Ok(mux::Image {
        width: planes.width,
        height: planes.height,
        tiles,
        cols,
        rows,
        tile_width: tw,
        tile_height: th,
        channels,
        depth: BIT_DEPTH,
    })
}

fn encode_gain_map(
    gm: &GainMapData,
    orientation: u8,
    params: &[(&str, &str)],
) -> Result<Option<mux::GainMap>, String> {
    let (w, h, gray) = gm.gray();
    let (gray, w, h) = orient::apply(gray, w, h, 1, orientation);
    let image = encode_image(&yuv::from_gray8(w, h, &gray), 1, params, |_| None)?;
    Ok(Some(mux::GainMap {
        image,
        metadata: gm.tmap_payload(),
    }))
}

/// Full-range 16-bit RGB(A), straight alpha.
fn to_rgb16(frame: ImageFrame) -> Rgb16 {
    let ImageFrame {
        width,
        height,
        has_alpha,
        alpha_premultiplied,
        pixels,
        ..
    } = frame;
    let mut data: Vec<u16> = match pixels {
        Pixels::U8(v) => v.into_iter().map(|s| u16::from(s) * 257).collect(),
        Pixels::U16(v) => v,
        Pixels::F32(v) => {
            if v.iter().any(|s| *s > 1.0) {
                notice!("note: float samples above 1.0 clipped for AVIF");
            }
            v.into_iter()
                .map(|s| (s.clamp(0.0, 1.0) * 65535.0).round() as u16)
                .collect()
        }
    };
    if has_alpha && alpha_premultiplied {
        for p in data.as_chunks_mut::<4>().0 {
            let a = u32::from(p[3]);
            for c in &mut p[..3] {
                if let Some(v) = (u32::from(*c) * 65535 + a / 2).checked_div(a) {
                    *c = v.min(65535) as u16;
                }
            }
        }
    }
    Rgb16 {
        width,
        height,
        alpha: has_alpha,
        data,
    }
}

fn drop_alpha(img: Rgb16) -> Rgb16 {
    Rgb16 {
        data: img
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        alpha: false,
        ..img
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_splits_only_oversized_images() {
        assert_eq!(layout(6000, 4000), (1, 1, 6000, 4000));
        assert_eq!(layout(6336, 9504), (1, 2, 6336, 4752));
        assert_eq!(layout(16385, 3), (2, 1, 8194, 4));
        let (c, r, tw, th) = layout(11648, 8737);
        assert!(c * tw >= 11648 && r * th >= 8737 && tw <= MAX_WIDTH && th <= MAX_HEIGHT);
    }

    #[test]
    fn parallelism_levels() {
        assert_eq!(parallelism_level(1), 1);
        assert_eq!(parallelism_level(8), 4);
        assert_eq!(parallelism_level(64), 6);
    }
}
