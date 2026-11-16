//! Photon-noise film grain for AV1: the libaom/av1-grain sensor model
//! (shot noise, read noise and photo-response non-uniformity of a
//! full-frame sensor at a given ISO), evaluated on the sRGB curve the
//! output uses. Luma only; the decoder synthesizes it.
//!
//! Ported from `generate_photon_noise_params` in rav1e's `av1-grain` crate
//! (BSD-2-Clause), which follows libaom's photon noise table generator.

use svt_av1_tritium_sys::Grain;

const NUM_POINTS: usize = 14;
const PHOTONS_PER_SQ_MICRON_PER_LUX_SECOND: f32 = 11260.0;
const EFFECTIVE_QUANTUM_EFFICIENCY: f32 = 0.2;
const PHOTO_RESPONSE_NON_UNIFORMITY: f32 = 0.005;
const INPUT_REFERRED_READ_NOISE: f32 = 1.5;
/// 36 x 24 mm, in square microns.
const SENSOR_AREA: f32 = 36_000.0 * 24_000.0;
/// libaom's empirical factor from noise sigma to scaling-function value.
const SIGMA_TO_SCALING: f32 = 7.88;
const RAMP_OFFSET: f32 = 3.0;
/// Fixed seed: identical inputs give identical files.
const SEED: u16 = 7391;

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Grain for a full-frame-equivalent `iso` on a `width` x `height` image
/// (the whole image, also when it is encoded as grid tiles). `tile` varies
/// the seed so tiles do not repeat one pattern.
pub fn photon_noise(iso: f32, width: u32, height: u32, tile: u16) -> Grain {
    let mid_tone_exposure = 10.0 / iso.max(1.0);
    let pixel_area = SENSOR_AREA / (width as f32 * height as f32);
    let mid_tone_electrons = EFFECTIVE_QUANTUM_EFFICIENCY
        * PHOTONS_PER_SQ_MICRON_PER_LUX_SECOND
        * mid_tone_exposure
        * pixel_area;
    let max_electrons = mid_tone_electrons / srgb_to_linear(0.5);
    let range = 255.0f32;
    let mut points = [[0i32; 2]; NUM_POINTS];
    for (i, point) in points.iter_mut().enumerate() {
        let x = match i {
            0 => 0.0,
            i if i == NUM_POINTS - 1 => 1.0,
            i => {
                (RAMP_OFFSET
                    + (range - 2.0 * RAMP_OFFSET) * (i - 1) as f32 / (NUM_POINTS - 3) as f32)
                    / range
            }
        };
        let linear = srgb_to_linear(x);
        let electrons = max_electrons * linear;
        let noise_electrons =
            (PHOTO_RESPONSE_NON_UNIFORMITY * PHOTO_RESPONSE_NON_UNIFORMITY * electrons * electrons
                + INPUT_REFERRED_READ_NOISE * INPUT_REFERRED_READ_NOISE
                + electrons)
                .sqrt();
        let linear_noise = noise_electrons / max_electrons;
        let start = (linear - 2.0 * linear_noise).max(0.0);
        let end = (linear + 2.0 * linear_noise).min(1.0);
        let slope = (linear_to_srgb(end) - linear_to_srgb(start)) / (end - start);
        let encoded_noise = linear_noise * slope;
        let mut scaling = (range * SIGMA_TO_SCALING * encoded_noise)
            .round()
            .min(range) as i32;
        // Full-strength grain at the very ends brightens blacks and darkens whites.
        if i == 0 || i == NUM_POINTS - 1 {
            scaling = scaling.min(1);
        }
        *point = [(range * x).round() as i32, scaling];
    }
    Grain {
        num_y_points: NUM_POINTS as i32,
        scaling_points_y: points,
        scaling_shift: 8,
        ar_coeff_shift: 6,
        overlap_flag: 1,
        chroma_scaling_from_luma: 0,
        clip_to_restricted_range: 0,
        bit_depth: 10,
        random_seed: SEED.wrapping_add(tile.wrapping_mul(97)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grain_grows_with_iso_and_points_increase() {
        let low = photon_noise(100.0, 6000, 4000, 0);
        let high = photon_noise(6400.0, 6000, 4000, 0);
        let mid = |g: &Grain| g.scaling_points_y[6][1];
        assert!(mid(&high) > mid(&low), "{} vs {}", mid(&high), mid(&low));
        let xs: Vec<i32> = high.scaling_points_y.iter().map(|p| p[0]).collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
        assert_eq!((xs[0], xs[13]), (0, 255));
        assert!(
            high.scaling_points_y
                .iter()
                .all(|p| (0..=255).contains(&p[1]))
        );
    }

    #[test]
    fn tiles_get_distinct_seeds() {
        assert_ne!(
            photon_noise(800.0, 100, 100, 0).random_seed,
            photon_noise(800.0, 100, 100, 1).random_seed
        );
    }
}
