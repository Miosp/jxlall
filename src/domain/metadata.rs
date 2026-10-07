//! JXL metadata boxes assembled from source containers.

use std::sync::Arc;

#[derive(Debug, Default, Clone)]
pub struct MetaBoxes {
    /// Raw EXIF payload (TIFF structure, no 4-byte offset prefix).
    /// `Arc`: TIFF passes the whole file, shared with the decoder input.
    pub exif: Option<Arc<Vec<u8>>>,
    pub xmp: Option<Arc<Vec<u8>>>,
    /// Assembled `jhgm` gain map payload (JxlGainMapBundle bytes).
    pub jhgm: Option<Vec<u8>>,
}

impl MetaBoxes {
    /// Takes owned payloads; the `Vec`s move into their `Arc`, no byte copy.
    pub fn new(exif: Option<Vec<u8>>, xmp: Option<Vec<u8>>) -> Self {
        Self {
            exif: exif.map(Arc::new),
            xmp: xmp.map(Arc::new),
            jhgm: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.exif.is_none() && self.xmp.is_none() && self.jhgm.is_none()
    }
}
