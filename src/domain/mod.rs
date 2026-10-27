//! Domain model: decoded images and their color/metadata payload.

mod color;
mod gainmap;
mod image;
mod metadata;
mod noise;
mod orientation;
mod rating;

pub use color::{Cicp, ColorDesc, CustomColor, IccSpace};
pub use gainmap::{from_heic, GainMapData};
pub use image::{Decoded, DecodedImage, Hdr, ImageFrame, Pixels};
pub use metadata::{reset_exif_orientation, strip_exif_prefix, MetaBoxes};
pub use noise::iso_from_exif;
pub use orientation::Orientation;
pub use rating::{Rating, StarThresholds};
