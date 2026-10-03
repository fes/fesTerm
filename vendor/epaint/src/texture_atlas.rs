use ecolor::Color32;
use emath::{Rect, remap_clamp};
use std::sync::Arc;

use crate::{ColorImage, ImageDelta, TextOptions};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rectu {
    /// inclusive
    min_x: usize,

    /// inclusive
    min_y: usize,

    /// exclusive
    max_x: usize,

    /// exclusive
    max_y: usize,
}

impl Rectu {
    const NOTHING: Self = Self {
        min_x: usize::MAX,
        min_y: usize::MAX,
        max_x: 0,
        max_y: 0,
    };
    const EVERYTHING: Self = Self {
        min_x: 0,
        min_y: 0,
        max_x: usize::MAX,
        max_y: usize::MAX,
    };
}

#[derive(Copy, Clone, Debug)]
struct PrerasterizedDisc {
    r: f32,
    uv: Rectu,
}

/// A pre-rasterized disc (filled circle), somewhere in the texture atlas.
#[derive(Copy, Clone, Debug)]
pub struct PreparedDisc {
    /// The radius of this disc in texels.
    pub r: f32,

    /// Width in texels.
    pub w: f32,

    /// Where in the texture atlas the disc is.
    /// Normalized in 0-1 range.
    pub uv: Rect,
}

/// Opaque identity for an unchanged font image; not a content hash.
///
/// Equal revisions guarantee the image has not been exposed for mutation.
/// Different revisions may still describe equal pixels.
#[derive(Clone, Debug)]
pub struct FontImageRevision(Arc<()>);

impl PartialEq for FontImageRevision {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for FontImageRevision {}

/// Contains font data in an atlas, where each character occupied a small rectangle.
///
/// More characters can be added, possibly expanding the texture.
#[derive(Clone)]
pub struct TextureAtlas {
    image: ColorImage,
    image_revision: Option<FontImageRevision>,

    /// What part of the image that is dirty
    dirty: Rectu,

    /// Used for when allocating new rectangles.
    cursor: (usize, usize),

    row_height: usize,

    /// Set when someone requested more space than was available.
    overflowed: bool,

    /// pre-rasterized discs of radii `2^i`, where `i` is the index.
    discs: Vec<PrerasterizedDisc>,

    /// Controls how to convert glyph coverage to alpha.
    options: TextOptions,
}

impl TextureAtlas {
    pub fn new(size: [usize; 2], options: TextOptions) -> Self {
        assert!(size[0] >= 1024, "Tiny texture atlas");
        let mut atlas = Self {
            image: ColorImage::filled(size, Color32::TRANSPARENT),
            image_revision: None,
            dirty: Rectu::EVERYTHING,
            cursor: (0, 0),
            row_height: 0,
            overflowed: false,
            discs: vec![], // will be filled in below
            options,
        };

        // Make the top left pixel fully white for `WHITE_UV`, i.e. painting something with solid color:
        let (pos, image) = atlas.allocate((1, 1));
        assert_eq!(
            pos,
            (0, 0),
            "Expected the first allocation to be at (0, 0), but was at {pos:?}"
        );
        image[pos] = Color32::WHITE;

        // Allocate a series of anti-aliased discs used to render small filled circles:
        // TODO(emilk): these circles can be packed A LOT better.
        // In fact, the whole texture atlas could be packed a lot better.
        // for r in [1, 2, 4, 8, 16, 32, 64] {
        //     let w = 2 * r + 3;
        //     let hw = w as i32 / 2;
        const LARGEST_CIRCLE_RADIUS: f32 = 8.0; // keep small so that the initial texture atlas is small
        for i in 0.. {
            let r = 2.0_f32.powf(i as f32 / 2.0 - 1.0);
            if r > LARGEST_CIRCLE_RADIUS {
                break;
            }
            let hw = (r + 0.5).ceil() as i32;
            let w = (2 * hw + 1) as usize;
            let ((x, y), image) = atlas.allocate((w, w));
            for dx in -hw..=hw {
                for dy in -hw..=hw {
                    let distance_to_center = ((dx * dx + dy * dy) as f32).sqrt();
                    let coverage =
                        remap_clamp(distance_to_center, (r - 0.5)..=(r + 0.5), 1.0..=0.0);
                    image[((x as i32 + hw + dx) as usize, (y as i32 + hw + dy) as usize)] = options
                        .color_transfer_function
                        .color_from_coverage(coverage);
                }
            }
            atlas.discs.push(PrerasterizedDisc {
                r,
                uv: Rectu {
                    min_x: x,
                    min_y: y,
                    max_x: x + w,
                    max_y: y + w,
                },
            });
        }

        atlas
    }

    pub fn options(&self) -> &TextOptions {
        &self.options
    }

    pub fn size(&self) -> [usize; 2] {
        self.image.size
    }

    /// Returns the locations and sizes of pre-rasterized discs (filled circles) in this atlas.
    pub fn prepared_discs(&self) -> Vec<PreparedDisc> {
        let size = self.size();
        let inv_w = 1.0 / size[0] as f32;
        let inv_h = 1.0 / size[1] as f32;
        self.discs
            .iter()
            .map(|disc| {
                let r = disc.r;
                let Rectu {
                    min_x,
                    min_y,
                    max_x,
                    max_y,
                } = disc.uv;
                let w = max_x - min_x;
                let uv = Rect::from_min_max(
                    emath::pos2(min_x as f32 * inv_w, min_y as f32 * inv_h),
                    emath::pos2(max_x as f32 * inv_w, max_y as f32 * inv_h),
                );
                PreparedDisc { r, w: w as f32, uv }
            })
            .collect()
    }

    fn max_height(&self) -> usize {
        // the initial width is set to the max size
        self.image.height().max(self.image.width())
    }

    /// When this get high, it might be time to clear and start over!
    pub fn fill_ratio(&self) -> f32 {
        if self.overflowed {
            1.0
        } else {
            (self.cursor.1 + self.row_height) as f32 / self.max_height() as f32
        }
    }

    /// The texture options suitable for a font texture
    #[inline]
    pub fn texture_options() -> crate::textures::TextureOptions {
        crate::textures::TextureOptions::LINEAR
    }

    /// The full font atlas image.
    #[inline]
    pub fn image(&self) -> &ColorImage {
        &self.image
    }

    /// Non-consuming identity for the current image.
    ///
    /// Tracking is lazy: consumers that never request a revision allocate no
    /// identities. Cloned atlases share an identity only until either changes.
    pub fn image_revision(&mut self) -> FontImageRevision {
        self.image_revision
            .get_or_insert_with(|| FontImageRevision(Arc::new(())))
            .clone()
    }

    /// Call to get the change to the image since last call.
    pub fn take_delta(&mut self) -> Option<ImageDelta> {
        let texture_options = Self::texture_options();

        let dirty = std::mem::replace(&mut self.dirty, Rectu::NOTHING);
        if dirty == Rectu::NOTHING {
            None
        } else if dirty == Rectu::EVERYTHING {
            Some(ImageDelta::full(self.image.clone(), texture_options))
        } else {
            let pos = [dirty.min_x, dirty.min_y];
            let size = [dirty.max_x - dirty.min_x, dirty.max_y - dirty.min_y];
            let region = self.image.region_by_pixels(pos, size);
            Some(ImageDelta::partial(pos, region, texture_options))
        }
    }

    /// Returns the coordinates of where the rect ended up,
    /// and invalidates the region.
    pub fn allocate(&mut self, (w, h): (usize, usize)) -> ((usize, usize), &mut ColorImage) {
        /// On some low-precision GPUs (my old iPad) characters get muddled up
        /// if we don't add some empty pixels between the characters.
        /// On modern high-precision GPUs this is not needed.
        const PADDING: usize = 1;

        assert!(
            w <= self.image.width(),
            "Tried to allocate a {} wide glyph in a {} wide texture atlas",
            w,
            self.image.width()
        );
        // Even zero-sized allocation exposes the whole image for mutation.
        self.image_revision = None;
        if self.cursor.0 + w > self.image.width() {
            // New row:
            self.cursor.0 = 0;
            self.cursor.1 += self.row_height + PADDING;
            self.row_height = 0;
        }

        self.row_height = self.row_height.max(h);

        let required_height = self.cursor.1 + self.row_height;

        if required_height > self.max_height() {
            // This is a bad place to be - we need to start reusing space :/

            log::warn!("epaint texture atlas overflowed!");

            self.cursor = (0, self.image.height() / 3); // Restart a bit down - the top of the atlas has too many important things in it
            self.overflowed = true; // this will signal the user that we need to recreate the texture atlas next frame.
        } else if resize_to_min_height(&mut self.image, required_height) {
            self.dirty = Rectu::EVERYTHING;
        }

        let pos = self.cursor;
        self.cursor.0 += w + PADDING;

        self.dirty.min_x = self.dirty.min_x.min(pos.0);
        self.dirty.min_y = self.dirty.min_y.min(pos.1);
        self.dirty.max_x = self.dirty.max_x.max(pos.0 + w);
        self.dirty.max_y = self.dirty.max_y.max(pos.1 + h);

        (pos, &mut self.image)
    }
}

fn resize_to_min_height(image: &mut ColorImage, required_height: usize) -> bool {
    while required_height >= image.height() {
        image.size[1] *= 2; // double the height
    }

    if image.width() * image.height() > image.pixels.len() {
        image
            .pixels
            .resize(image.width() * image.height(), Color32::TRANSPARENT);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_image_revision_is_stable_and_does_not_consume_deltas() {
        let mut atlas = TextureAtlas::new([1024, 128], Default::default());
        let revision = atlas.image_revision();
        assert_eq!(revision, atlas.image_revision());
        assert!(atlas.take_delta().is_some());
        assert_eq!(revision, atlas.image_revision());
        assert!(atlas.take_delta().is_none());
    }

    #[test]
    fn same_size_and_zero_sized_mutable_exposure_invalidate_revision() {
        let mut atlas = TextureAtlas::new([1024, 128], Default::default());
        let original = atlas.image().clone();
        let revision = atlas.image_revision();
        let (pos, image) = atlas.allocate((1, 1));
        image[pos] = Color32::RED;
        assert_eq!(atlas.size(), original.size);
        assert_ne!(atlas.image(), &original);
        assert_ne!(revision, atlas.image_revision());
        let revision = atlas.image_revision();
        let (_, image) = atlas.allocate((0, 0));
        image[(0, 0)] = Color32::BLUE;
        assert_ne!(revision, atlas.image_revision());
    }

    #[test]
    fn growth_overflow_and_reset_never_reuse_an_old_revision() {
        let mut atlas = TextureAtlas::new([1024, 128], Default::default());
        let original = atlas.image().clone();
        let revision = atlas.image_revision();
        atlas.allocate((1024, 256));
        assert!(atlas.size()[1] > original.size[1]);
        assert_ne!(revision, atlas.image_revision());
        assert_eq!(original.size, [1024, 128]);
        let revision = atlas.image_revision();
        atlas.allocate((1024, 768));
        assert!(atlas.overflowed);
        assert_ne!(revision, atlas.image_revision());
        let mut reset = TextureAtlas::new(atlas.size(), Default::default());
        assert_ne!(atlas.image_revision(), reset.image_revision());
    }

    #[test]
    fn diverging_clones_cannot_have_equal_revisions() {
        let mut left = TextureAtlas::new([1024, 128], Default::default());
        let revision = left.image_revision();
        let mut right = left.clone();
        assert_eq!(revision, right.image_revision());
        let (pos, image) = left.allocate((1, 1));
        image[pos] = Color32::RED;
        let (pos, image) = right.allocate((1, 1));
        image[pos] = Color32::BLUE;
        assert_eq!(left.size(), right.size());
        assert_ne!(left.image_revision(), right.image_revision());
        assert_ne!(left.image(), right.image());
    }

    #[test]
    fn revision_tracking_allocates_nothing_until_requested() {
        let mut atlas = TextureAtlas::new([1024, 128], Default::default());
        atlas.allocate((1, 1));
        assert!(atlas.image_revision.is_none());
        atlas.image_revision();
        assert!(atlas.image_revision.is_some());
        atlas.allocate((1, 1));
        assert!(atlas.image_revision.is_none());
    }
}
