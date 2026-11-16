//! Minimal ICC v4 matrix/TRC profiles: the output profiles AVIF files carry,
//! and synthesized source profiles for CICP and PNG gAMA/cHRM color.
//!
//! Output profile descriptions never contain "sRGB" unless the space is
//! sRGB: Immich forces an sRGB preview for any profile whose description
//! does.

/// CIE 1931 xy chromaticity.
pub type Xy = [f64; 2];

pub const D65: Xy = [0.3127, 0.3290];
const D50_XYZ: [f64; 3] = [0.9642, 1.0, 0.8249];

/// RGB primaries and white point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Primaries {
    pub r: Xy,
    pub g: Xy,
    pub b: Xy,
    pub w: Xy,
}

pub const SRGB: Primaries = Primaries {
    r: [0.64, 0.33],
    g: [0.30, 0.60],
    b: [0.15, 0.06],
    w: D65,
};
pub const DISPLAY_P3: Primaries = Primaries {
    r: [0.680, 0.320],
    g: [0.265, 0.690],
    b: [0.150, 0.060],
    w: D65,
};
/// Smallest triangle (with the P3 red and blue) holding both Display P3
/// and Adobe RGB. Measured to render correctly in Windows Photos, Chrome
/// and Immich, unlike Rec.2020 primaries, which Windows washes out.
pub const WIDE: Primaries = Primaries {
    r: [0.680, 0.320],
    g: [0.208, 0.745],
    b: [0.150, 0.060],
    w: D65,
};

/// Decoding curve (encoded value -> linear light).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trc {
    Srgb,
    /// BT.709/601/2020 camera curve (inverse OETF).
    Bt709,
    /// Pure power law with this exponent.
    Gamma(f64),
    Linear,
}

type M3 = [[f64; 3]; 3];

fn xyz(xy: Xy) -> [f64; 3] {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn mul_v(a: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| (0..3).map(|k| a[i][k] * v[k]).sum())
}

fn inv(m: &M3) -> M3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    [
        [
            (e * i - f * h) / det,
            (c * h - b * i) / det,
            (b * f - c * e) / det,
        ],
        [
            (f * g - d * i) / det,
            (a * i - c * g) / det,
            (c * d - a * f) / det,
        ],
        [
            (d * h - e * g) / det,
            (b * g - a * h) / det,
            (a * e - b * d) / det,
        ],
    ]
}

/// Linear RGB -> XYZ for these primaries (white Y = 1).
fn rgb_to_xyz(p: &Primaries) -> M3 {
    let cols = [xyz(p.r), xyz(p.g), xyz(p.b)];
    let m: M3 = [0, 1, 2].map(|i| [cols[0][i], cols[1][i], cols[2][i]]);
    let s = mul_v(&inv(&m), xyz(p.w));
    [0, 1, 2].map(|i| [m[i][0] * s[0], m[i][1] * s[1], m[i][2] * s[2]])
}

const BRADFORD: M3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// Bradford adaptation from `white` to the D50 PCS white.
fn chad(white: Xy) -> M3 {
    let src = mul_v(&BRADFORD, xyz(white));
    let dst = mul_v(&BRADFORD, D50_XYZ);
    let scale = [
        [dst[0] / src[0], 0.0, 0.0],
        [0.0, dst[1] / src[1], 0.0],
        [0.0, 0.0, dst[2] / src[2]],
    ];
    mul(&inv(&BRADFORD), &mul(&scale, &BRADFORD))
}

/// D50-adapted colorant chromaticities, as an ICC profile stores them; the
/// space in which source and target gamuts are compared.
pub fn pcs_colorant_xy(p: &Primaries) -> [Xy; 3] {
    let m = mul(&chad(p.w), &rgb_to_xyz(p));
    [0, 1, 2].map(|c| xy_of([m[0][c], m[1][c], m[2][c]]))
}

pub fn xy_of(v: [f64; 3]) -> Xy {
    let s = v[0] + v[1] + v[2];
    if s.abs() < 1e-12 {
        [0.0, 0.0]
    } else {
        [v[0] / s, v[1] / s]
    }
}

/// Whether every point lies inside the triangle (with a small tolerance in
/// xy units for profile rounding).
pub fn triangle_contains(tri: &[Xy; 3], points: &[Xy]) -> bool {
    const TOLERANCE: f64 = 0.002;
    let edge = |a: Xy, b: Xy, p: Xy| {
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        // Signed distance; positive on the inner side of a counter-clockwise triangle.
        (ex * (p[1] - a[1]) - ey * (p[0] - a[0])) / ex.hypot(ey)
    };
    let ccw = edge(tri[0], tri[1], tri[2]) > 0.0;
    points.iter().all(|&p| {
        (0..3).all(|i| {
            let d = edge(tri[i], tri[(i + 1) % 3], p);
            (if ccw { d } else { -d }) >= -TOLERANCE
        })
    })
}

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn mluc(text: &str) -> Vec<u8> {
    let s: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut v = b"mluc\0\0\0\0".to_vec();
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&12u32.to_be_bytes());
    v.extend_from_slice(b"enUS");
    v.extend_from_slice(&(s.len() as u32).to_be_bytes());
    v.extend_from_slice(&28u32.to_be_bytes());
    v.extend(s);
    v
}

fn xyz_tag(v: [f64; 3]) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    v.iter().for_each(|c| t.extend_from_slice(&s15(*c)));
    t
}

fn para(trc: Trc) -> Vec<u8> {
    let (kind, params): (u16, Vec<f64>) = match trc {
        Trc::Srgb => (
            3,
            vec![2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045],
        ),
        Trc::Bt709 => (
            3,
            vec![1.0 / 0.45, 1.0 / 1.099, 0.099 / 1.099, 1.0 / 4.5, 0.081],
        ),
        Trc::Gamma(g) => (0, vec![g]),
        Trc::Linear => (0, vec![1.0]),
    };
    let mut t = b"para\0\0\0\0".to_vec();
    t.extend_from_slice(&kind.to_be_bytes());
    t.extend_from_slice(&[0, 0]);
    params.iter().for_each(|p| t.extend_from_slice(&s15(*p)));
    t
}

/// A v4 display-class RGB matrix/TRC profile.
pub fn rgb_profile(p: &Primaries, trc: Trc, description: &str) -> Vec<u8> {
    let adapt = chad(p.w);
    let m = mul(&adapt, &rgb_to_xyz(p));
    let column = |c: usize| [m[0][c], m[1][c], m[2][c]];
    let curve = para(trc);
    let mut chad_tag = b"sf32\0\0\0\0".to_vec();
    adapt
        .iter()
        .flatten()
        .for_each(|c| chad_tag.extend_from_slice(&s15(*c)));
    let tags: [(&[u8; 4], Vec<u8>); 10] = [
        (b"desc", mluc(description)),
        (b"cprt", mluc("No copyright, use freely")),
        (b"wtpt", xyz_tag(D50_XYZ)),
        (b"chad", chad_tag),
        (b"rXYZ", xyz_tag(column(0))),
        (b"gXYZ", xyz_tag(column(1))),
        (b"bXYZ", xyz_tag(column(2))),
        (b"rTRC", curve.clone()),
        (b"gTRC", curve.clone()),
        (b"bTRC", curve),
    ];
    let header_and_table = 128 + 4 + 12 * tags.len();
    let mut table = Vec::new();
    let mut data: Vec<u8> = Vec::new();
    let mut written: Vec<(Vec<u8>, usize)> = Vec::new();
    for (sig, body) in &tags {
        let offset = match written.iter().find(|(b, _)| b == body) {
            Some((_, o)) => *o,
            None => {
                while !data.len().is_multiple_of(4) {
                    data.push(0);
                }
                let o = header_and_table + data.len();
                data.extend_from_slice(body);
                written.push((body.clone(), o));
                o
            }
        };
        table.extend_from_slice(*sig);
        table.extend_from_slice(&(offset as u32).to_be_bytes());
        table.extend_from_slice(&(body.len() as u32).to_be_bytes());
    }
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let size = header_and_table + data.len();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(size as u32).to_be_bytes());
    out.extend_from_slice(&[0; 4]); // preferred CMM
    out.extend_from_slice(&[4, 0x30, 0, 0]); // v4.3
    out.extend_from_slice(b"mntrRGB XYZ ");
    for v in [2026u16, 1, 1, 0, 0, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(b"acsp");
    out.extend_from_slice(&[0; 4 + 4 + 4 + 4 + 8 + 4]); // platform, flags, manufacturer, model, attributes, intent
    D50_XYZ.iter().for_each(|c| out.extend_from_slice(&s15(*c)));
    out.extend_from_slice(&[0; 4 + 16 + 28]); // creator, profile ID, reserved
    debug_assert_eq!(out.len(), 128);
    out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    out.extend(table);
    out.extend(data);
    out
}

/// Primaries of a CICP `colour_primaries` code, when known.
pub fn cicp_primaries(code: u16) -> Option<Primaries> {
    let p = |r: Xy, g: Xy, b: Xy, w: Xy| Primaries { r, g, b, w };
    Some(match code {
        1 => SRGB,
        4 => p([0.67, 0.33], [0.21, 0.71], [0.14, 0.08], [0.310, 0.316]),
        5 => p([0.64, 0.33], [0.29, 0.60], [0.15, 0.06], D65),
        6 | 7 => p([0.630, 0.340], [0.310, 0.595], [0.155, 0.070], D65),
        8 => p(
            [0.681, 0.319],
            [0.243, 0.692],
            [0.145, 0.049],
            [0.310, 0.316],
        ),
        9 => p([0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65),
        11 => p(
            [0.680, 0.320],
            [0.265, 0.690],
            [0.150, 0.060],
            [0.314, 0.351],
        ),
        12 => DISPLAY_P3,
        _ => return None,
    })
}

/// Curve of a CICP `transfer_characteristics` code, for SDR codes.
pub fn cicp_trc(code: u16) -> Option<Trc> {
    Some(match code {
        1 | 6 | 14 | 15 => Trc::Bt709,
        4 => Trc::Gamma(2.2),
        5 => Trc::Gamma(2.8),
        8 => Trc::Linear,
        13 => Trc::Srgb,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_parse_and_round_trip_their_colorants() {
        let icc = rgb_profile(&DISPLAY_P3, Trc::Srgb, "Display P3");
        assert_eq!(&icc[36..40], b"acsp");
        assert_eq!(
            u32::from_be_bytes(icc[..4].try_into().unwrap()) as usize,
            icc.len()
        );
        let p = moxcms::ColorProfile::new_from_slice(&icc).unwrap();
        assert!(p.is_matrix_shaper());
        let red = xy_of([p.red_colorant.x, p.red_colorant.y, p.red_colorant.z]);
        let want = pcs_colorant_xy(&DISPLAY_P3)[0];
        assert!((red[0] - want[0]).abs() < 1e-4 && (red[1] - want[1]).abs() < 1e-4);
    }

    #[test]
    fn gamut_containment() {
        let srgb = pcs_colorant_xy(&SRGB);
        let p3 = pcs_colorant_xy(&DISPLAY_P3);
        let wide = pcs_colorant_xy(&WIDE);
        let adobe = pcs_colorant_xy(&Primaries {
            r: [0.64, 0.33],
            g: [0.21, 0.71],
            b: [0.15, 0.06],
            w: D65,
        });
        assert!(triangle_contains(&p3, &srgb));
        assert!(triangle_contains(&srgb, &srgb));
        assert!(!triangle_contains(&srgb, &p3));
        assert!(!triangle_contains(&p3, &adobe));
        assert!(triangle_contains(&wide, &adobe));
        assert!(triangle_contains(&wide, &p3));
    }

    #[test]
    fn descriptions_avoid_srgb_for_wide_spaces() {
        // Immich matches "srgb" anywhere in the description.
        for d in [
            super::super::color::P3_DESCRIPTION,
            super::super::color::WIDE_DESCRIPTION,
        ] {
            assert!(!d.to_ascii_lowercase().contains("srgb"), "{d}");
        }
    }
}
