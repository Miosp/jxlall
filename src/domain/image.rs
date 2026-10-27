//! Pixel buffers and decoded-image aggregates.

/// Interleaved pixel buffer, RGB or RGBA.
pub enum Pixels {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl Pixels {
    /// Raw buffer parts: `(pointer, length in bytes)`, ready for FFI.
    pub fn as_raw_parts(&self) -> (*const u8, usize) {
        match self {
            Self::U8(v) => (v.as_ptr(), v.len()),
            Self::U16(v) => (v.as_ptr().cast(), v.len() * 2),
            Self::F32(v) => (v.as_ptr().cast(), v.len() * 4),
        }
    }
}

/// One decoded frame ready for encoding.
pub struct ImageFrame {
    pub width: u32,
    pub height: u32,
    /// Significant bits per sample (8, 10, 12, 16, 32). N-bit samples are
    /// left-aligned in their storage type per JXL convention.
    pub bit_depth: u8,
    pub has_alpha: bool,
    /// Color samples are premultiplied by alpha (TIFF associated alpha).
    pub alpha_premultiplied: bool,
    pub pixels: Pixels,
}

/// HDR luminance hints. libjxl 0.12 C API exposes no CLLI setter, so content
/// light level is not preserved on the pixel path.
#[derive(Default)]
pub struct Hdr {
    pub intensity_target: Option<f32>,
}

impl Hdr {
    /// Reference display luminance for a CICP transfer function.
    pub fn for_transfer(transfer_characteristics: u16) -> Self {
        let intensity_target = match transfer_characteristics {
            16 => Some(10_000.0), // PQ
            18 => Some(1_000.0),  // HLG
            _ => None,
        };
        Self { intensity_target }
    }
}

/// One page of a decoded source, ready for JXL encoding.
pub struct DecodedImage {
    pub frame: ImageFrame,
    pub color: super::ColorDesc,
    pub meta: super::MetaBoxes,
    pub orientation: super::Orientation,
    pub hdr: Hdr,
    /// ISO 21496-1 gain map, preserved as a `jhgm` box.
    pub gain_map: Option<super::GainMapData>,
}

impl DecodedImage {
    pub fn new(frame: ImageFrame, color: super::ColorDesc, meta: super::MetaBoxes) -> Self {
        Self {
            frame,
            color,
            meta,
            orientation: super::Orientation::default(),
            hdr: Hdr::default(),
            gain_map: None,
        }
    }

    pub fn orientation(mut self, o: super::Orientation) -> Self {
        self.orientation = o;
        self
    }

    pub fn hdr(mut self, h: Hdr) -> Self {
        self.hdr = h;
        self
    }

    pub fn gain_map(mut self, g: Option<super::GainMapData>) -> Self {
        self.gain_map = g;
        self
    }
}

/// Decoder output: a JPEG eligible for jbrd, or decoded pixels. The JPEG
/// variant shares the file buffer (`Arc`), never copies it.
pub enum Decoded {
    Jpeg(std::sync::Arc<Vec<u8>>),
    Image(Box<DecodedImage>),
}
