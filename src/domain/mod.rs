//! Domain model: decoded images and their color/metadata payload.

mod color;
mod gainmap;
mod image;
mod metadata;
mod orientation;

pub use color::{Cicp, ColorDesc};
pub use gainmap::{from_heic, GainMapData};
pub use image::{Decoded, DecodedImage, Hdr, ImageFrame, Pixels};
pub use metadata::MetaBoxes;
pub use orientation::Orientation;
