//! Domain model: decoded images and their color/metadata payload.

mod color;
mod gainmap;
mod image;
mod metadata;
mod noise;
mod orientation;

pub use color::{Cicp, ColorDesc, CustomColor, IccSpace};
pub use gainmap::{GainMapData, from_heic};
pub use image::{Decoded, DecodedImage, Hdr, ImageFrame, Pixels};
pub use metadata::{MetaBoxes, reset_exif_orientation, strip_exif_prefix};
pub use noise::iso_from_exif;
pub use orientation::Orientation;
