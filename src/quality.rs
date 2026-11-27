//! `-q`: one 0..=100 quality scale for JPEG XL and AVIF.
//!
//! JPEG XL uses libjxl's own quality → distance mapping (as `cjxl -q`).
//! The AVIF CRF for each quality was matched to it by median SSIMULACRA2
//! over 2048² crops of 11 camera photos and film scans (JXL effort 7, AVIF
//! preset 5); points outside q 45..=97 are extrapolated.

/// Quality 0..=100; 100 is lossless for JPEG XL.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quality(f32);

impl TryFrom<f32> for Quality {
    type Error = String;

    fn try_from(v: f32) -> Result<Self, Self::Error> {
        (0.0..=100.0)
            .contains(&v)
            .then_some(Self(v))
            .ok_or_else(|| "quality must be 0..=100".into())
    }
}

/// (quality, CRF) control points, ascending quality.
const AVIF_CRF: [(f32, f32); 17] = [
    (0.0, 70.0),
    (30.0, 48.0),
    (45.0, 43.25),
    (50.0, 42.0),
    (55.0, 40.5),
    (60.0, 39.0),
    (65.0, 37.0),
    (70.0, 35.0),
    (75.0, 32.0),
    (80.0, 28.25),
    (85.0, 23.0),
    (88.0, 18.75),
    (90.0, 16.25),
    (93.0, 12.75),
    (95.0, 9.75),
    (97.0, 7.0),
    (100.0, 2.75),
];

impl Quality {
    /// JPEG XL default: lossless.
    pub const JXL_DEFAULT: Self = Self(100.0);
    /// AVIF default: CRF 23.
    pub const AVIF_DEFAULT: Self = Self(85.0);

    pub fn get(self) -> f32 {
        self.0
    }

    /// libjxl `JxlEncoderDistanceFromQuality`; 0 means lossless.
    pub fn jxl_distance(self) -> f32 {
        let q = self.0;
        if q >= 100.0 {
            0.0
        } else if q >= 30.0 {
            0.1 + (100.0 - q) * 0.09
        } else {
            53.0 / 3000.0 * q * q - 23.0 / 20.0 * q + 25.0
        }
    }

    /// SVT-AV1 CRF, rounded to the encoder's quarter steps (it would
    /// otherwise truncate).
    pub fn avif_crf(self) -> f32 {
        let q = self.0;
        let crf =
            AVIF_CRF
                .windows(2)
                .find(|w| q <= w[1].0)
                .map_or(AVIF_CRF[AVIF_CRF.len() - 1].1, |w| {
                    let ((q0, c0), (q1, c1)) = (w[0], w[1]);
                    c0 + (c1 - c0) * (q - q0) / (q1 - q0)
                });
        (crf * 4.0).round() / 4.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(v: f32) -> Quality {
        Quality::try_from(v).unwrap()
    }

    #[test]
    fn jxl_distance_matches_libjxl() {
        assert_eq!(q(100.0).jxl_distance(), 0.0);
        assert!((q(90.0).jxl_distance() - 1.0).abs() < 1e-6);
        assert!((q(0.0).jxl_distance() - 25.0).abs() < 1e-6);
        // Continuous at the formula switch.
        assert!((q(30.0).jxl_distance() - 6.4).abs() < 1e-4);
    }

    #[test]
    fn avif_crf_hits_the_control_points_and_quarter_steps() {
        assert_eq!(Quality::AVIF_DEFAULT.avif_crf(), 23.0);
        assert_eq!(q(90.0).avif_crf(), 16.25);
        assert_eq!(q(0.0).avif_crf(), 70.0);
        assert_eq!(q(100.0).avif_crf(), 2.75);
        // Between 85 (23) and 88 (18.75): 21.583 rounds to 21.5.
        assert_eq!(q(86.0).avif_crf(), 21.5);
        let mut last = f32::MAX;
        for i in 0..=1000 {
            let crf = q(i as f32 / 10.0).avif_crf();
            assert!(crf <= last && (crf * 4.0).fract() == 0.0);
            last = crf;
        }
    }

    #[test]
    fn out_of_range_is_rejected() {
        assert!(Quality::try_from(100.5).is_err());
        assert!(Quality::try_from(-1.0).is_err());
        assert!(Quality::try_from(f32::NAN).is_err());
    }
}
