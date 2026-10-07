const MAX_TEXTURE_DIMENSION: usize = 8_192;
pub(crate) const MAX_TEXTURE_PIXELS: usize = 16_777_216;

/// Checks the native backend's existing single-texture dimension limits.
pub fn texture_dimensions_supported([width, height]: [usize; 2]) -> bool {
    width > 0
        && height > 0
        && width <= MAX_TEXTURE_DIMENSION
        && height <= MAX_TEXTURE_DIMENSION
        && width * height <= MAX_TEXTURE_PIXELS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_texture_dimension_policy_preserves_limits_and_rejects_overflow() {
        for size in [[1, 1], [8_192, 2_048], [4_096, 4_096], [2_048, 8_192]] {
            assert!(texture_dimensions_supported(size), "{size:?}");
        }
        for size in [
            [0, 1],
            [1, 0],
            [8_193, 1],
            [1, 8_193],
            [4_096, 4_097],
            [8_192, 8_192],
            [usize::MAX, 2],
            [2, usize::MAX],
            [usize::MAX, usize::MAX],
        ] {
            assert!(!texture_dimensions_supported(size), "{size:?}");
        }
    }
}
