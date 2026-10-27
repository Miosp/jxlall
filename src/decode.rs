//! Per-format decode dispatch. Multipage sources yield multiple items.

use std::sync::Arc;

use crate::domain::Decoded;
use crate::sniff::Format;

/// `&Arc<Vec<u8>>`: decoders borrow the bytes; the JPEG passthrough shares
/// the whole buffer instead of copying it.
pub fn decode(data: &Arc<Vec<u8>>, format: Format) -> Result<Vec<Decoded>, String> {
    match format {
        Format::Jpeg if crate::format::jpeg_markers::is_hierarchical(data) => {
            Err("hierarchical JPEG not supported (decoders read only the first frame)".into())
        }
        Format::Jpeg if crate::format::jpeg_markers::is_lossless_sof(data) => {
            crate::format::jpeg_lossless::decode(data).map(|d| vec![d])
        }
        Format::Jpeg => Ok(vec![Decoded::Jpeg(Arc::clone(data))]),
        Format::Heif => crate::format::heif::decode(data).map(|d| vec![d]),
        Format::WebP => crate::format::webp::decode(data).map(|d| vec![d]),
        Format::Png => crate::format::png::decode(data).map(|d| vec![d]),
        Format::Tiff => crate::format::tiff::decode(data),
    }
}
