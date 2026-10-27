//! No-reference quality rating of one image, as stored in XMP.

/// Minimum score for 2, 3, 4 and 5 stars; anything lower gets 1 star.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StarThresholds([f32; 4]);

impl StarThresholds {
    pub fn stars_for(&self, score: f32) -> u8 {
        1 + self.0.iter().filter(|&&t| score >= t).count() as u8
    }
}

impl TryFrom<Vec<f32>> for StarThresholds {
    type Error = String;

    fn try_from(v: Vec<f32>) -> Result<Self, Self::Error> {
        let t: [f32; 4] = v
            .try_into()
            .map_err(|_| "star thresholds: give exactly 4 values (minimum score for 2..5 stars)")?;
        if t.iter().any(|x| !x.is_finite()) || !t.is_sorted_by(|a, b| a < b) {
            return Err("star thresholds must be finite and strictly ascending".into());
        }
        Ok(Self(t))
    }
}

/// What gets written for one image.
#[derive(Debug, Clone, PartialEq)]
pub struct Rating {
    /// Raw model score; only comparable between images rated by one model.
    pub score: f32,
    /// Model identifier including its version.
    pub model: &'static str,
    /// `xmp:Rating` stars (1..=5), when star output is enabled.
    pub stars: Option<u8>,
    /// Replace an `xmp:Rating` the source already has.
    pub overwrite_stars: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_follow_thresholds_inclusively() {
        let t = StarThresholds::try_from(vec![0.2, 0.4, 0.6, 0.8]).unwrap();
        assert_eq!(t.stars_for(0.1), 1);
        assert_eq!(t.stars_for(0.2), 2);
        assert_eq!(t.stars_for(0.79), 4);
        assert_eq!(t.stars_for(0.95), 5);
    }

    #[test]
    fn thresholds_must_be_four_ascending_finite_values() {
        assert!(StarThresholds::try_from(vec![0.2, 0.4, 0.6]).is_err());
        assert!(StarThresholds::try_from(vec![0.2, 0.2, 0.6, 0.8]).is_err());
        assert!(StarThresholds::try_from(vec![0.2, 0.4, f32::NAN, 0.8]).is_err());
    }
}
