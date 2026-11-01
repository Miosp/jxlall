//! Tile grid and blending for restoring large images piecewise, as in the
//! dejpeg web app: origins stay on a fixed grid, tiles past the image edge are
//! mirrored, overlaps are cosine-feathered.

use std::f32::consts::PI;

/// RGB: the model never sees alpha.
pub const CHANNELS: usize = 3;
const MIN_OVERLAP: usize = 8;
const OVERLAP_DIVISOR: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    pub x0: usize,
    pub y0: usize,
}

/// Square tiles covering an image; the last row and column may overhang it.
pub struct TilePlan {
    pub width: usize,
    pub height: usize,
    pub size: usize,
    pub overlap: usize,
    pub tiles: Vec<Tile>,
}

impl TilePlan {
    pub fn new(width: usize, height: usize, size: usize) -> Self {
        let overlap = MIN_OVERLAP.max(size / OVERLAP_DIVISOR);
        let step = size - overlap;
        let xs = axis_origins(width, size, step);
        let ys = axis_origins(height, size, step);
        let tiles = ys
            .iter()
            .flat_map(|&y0| xs.iter().map(move |&x0| Tile { x0, y0 }))
            .collect();
        Self {
            width,
            height,
            size,
            overlap,
            tiles,
        }
    }
}

fn axis_origins(length: usize, size: usize, step: usize) -> Vec<usize> {
    if length <= size {
        return vec![0];
    }
    let mut origins: Vec<usize> = (0..)
        .map(|n| n * step)
        .take_while(|origin| origin + size <= length)
        .collect();
    let last = *origins.last().expect("length > size admits origin 0");
    if last + size < length {
        origins.push(last + step);
    }
    origins
}

/// Coordinate past the image edge reflected back inside. A black overhang
/// would add a false edge.
pub fn mirror(i: usize, length: usize) -> usize {
    let period = i % (2 * length);
    if period < length {
        period
    } else {
        2 * length - 1 - period
    }
}

/// Feather weights along one axis; margins on an image border stay at 1 so
/// borders are not darkened.
fn axis_weights(origin: usize, length: usize, size: usize, overlap: usize) -> Vec<f32> {
    let ramp = |distance: usize| 0.5 * (1.0 - (PI * distance as f32 / overlap as f32).cos());
    (0..size)
        .map(|pos| {
            if pos < overlap && origin > 0 {
                ramp(pos + 1)
            } else if pos >= size - overlap && origin + size < length {
                ramp(size - pos)
            } else {
                1.0
            }
        })
        .collect()
}

/// Accumulates tile outputs into one interleaved RGB image.
pub struct Blender {
    width: usize,
    height: usize,
    weighted: Vec<f32>,
    weights: Vec<f32>,
}

impl Blender {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            weighted: vec![0.0; width * height * CHANNELS],
            weights: vec![0.0; width * height],
        }
    }

    /// `output`: one tile in CHW layout.
    pub fn add(&mut self, plan: &TilePlan, tile: Tile, output: &[f32]) {
        let size = plan.size;
        let wx = axis_weights(tile.x0, self.width, size, plan.overlap);
        let wy = axis_weights(tile.y0, self.height, size, plan.overlap);
        for dy in 0..size.min(self.height.saturating_sub(tile.y0)) {
            for dx in 0..size.min(self.width.saturating_sub(tile.x0)) {
                let weight = wx[dx] * wy[dy];
                let pixel = (tile.y0 + dy) * self.width + tile.x0 + dx;
                self.weights[pixel] += weight;
                for c in 0..CHANNELS {
                    self.weighted[pixel * CHANNELS + c] +=
                        weight * output[(c * size + dy) * size + dx];
                }
            }
        }
    }

    /// Interleaved RGB in 0..=1.
    pub fn finish(mut self) -> Vec<f32> {
        for (rgb, &weight) in self
            .weighted
            .as_chunks_mut::<CHANNELS>()
            .0
            .iter_mut()
            .zip(&self.weights)
        {
            for v in rgb {
                *v = (*v / weight).clamp(0.0, 1.0);
            }
        }
        self.weighted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_image_gets_one_tile() {
        let plan = TilePlan::new(100, 40, 256);
        assert_eq!(plan.tiles, [Tile { x0: 0, y0: 0 }]);
    }

    #[test]
    fn origins_stay_on_the_step_grid_and_cover_the_image() {
        let plan = TilePlan::new(1000, 700, 256);
        let step = 256 - plan.overlap;
        for tile in &plan.tiles {
            assert_eq!(tile.x0 % step, 0);
            assert_eq!(tile.y0 % step, 0);
        }
        let right = plan.tiles.iter().map(|t| t.x0 + 256).max().unwrap();
        let bottom = plan.tiles.iter().map(|t| t.y0 + 256).max().unwrap();
        assert!(right >= 1000 && bottom >= 700);
    }

    #[test]
    fn overlap_has_a_floor() {
        assert_eq!(TilePlan::new(10, 10, 64).overlap, 8);
        assert_eq!(TilePlan::new(10, 10, 512).overlap, 64);
    }

    #[test]
    fn mirror_reflects_past_the_edge() {
        assert_eq!(mirror(3, 5), 3);
        assert_eq!(mirror(5, 5), 4);
        assert_eq!(mirror(6, 5), 3);
        assert_eq!(mirror(10, 5), 0);
    }

    /// Identity inference: every tile returns its own mirrored input.
    fn blend_identity(width: usize, height: usize, size: usize) -> (Vec<f32>, Vec<f32>) {
        let source: Vec<f32> = (0..width * height * CHANNELS)
            .map(|i| (i % 251) as f32 / 251.0)
            .collect();
        let plan = TilePlan::new(width, height, size);
        let mut blender = Blender::new(width, height);
        let mut tile_chw = vec![0.0; CHANNELS * size * size];
        for &tile in &plan.tiles {
            for c in 0..CHANNELS {
                for dy in 0..size {
                    for dx in 0..size {
                        let x = mirror(tile.x0 + dx, width);
                        let y = mirror(tile.y0 + dy, height);
                        tile_chw[(c * size + dy) * size + dx] =
                            source[(y * width + x) * CHANNELS + c];
                    }
                }
            }
            blender.add(&plan, tile, &tile_chw);
        }
        (source, blender.finish())
    }

    #[test]
    fn blending_identity_output_returns_the_input() {
        const TOLERANCE: f32 = 1e-4;
        for (width, height, size) in [
            (1000, 700, 256),
            (64, 64, 64),
            (17, 300, 64),
            (513, 512, 512),
        ] {
            let (source, blended) = blend_identity(width, height, size);
            assert_eq!(source.len(), blended.len());
            for (a, b) in source.iter().zip(&blended) {
                assert!(
                    (a - b).abs() < TOLERANCE,
                    "{width}x{height}@{size}: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn output_is_clamped_to_unit_range() {
        let plan = TilePlan::new(4, 4, 64);
        let mut blender = Blender::new(4, 4);
        blender.add(&plan, plan.tiles[0], &vec![2.0; CHANNELS * 64 * 64]);
        assert!(blender.finish().iter().all(|&v| v == 1.0));
    }
}
