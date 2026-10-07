//! EXIF orientation as a domain value. Stored as metadata only — pixels are
//! never rotated.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Orientation(u8);

impl Orientation {
    /// Out-of-range values fall back to 1 (identity): a missing rotation
    /// is recoverable, a wrong one is silent corruption.
    pub fn new(v: u8) -> Self {
        Self(if (1..=8).contains(&v) { v } else { 1 })
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// Parse from a raw EXIF/TIFF payload (no 4-byte prefix).
    pub fn from_exif(exif: Option<&[u8]>) -> Option<Self> {
        let exif = exif::Reader::new().read_raw(exif?.to_vec()).ok()?;
        let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
        field
            .value
            .get_uint(0)
            .and_then(|v| u8::try_from(v).ok())
            .filter(|v| (1..=8).contains(v))
            .map(Self)
    }
}
