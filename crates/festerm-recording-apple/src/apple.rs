use std::{
    ffi::{c_char, c_void, CStr, CString},
    fmt,
    path::Path,
    ptr::NonNull,
};

const ERROR_BYTES: usize = 1024;
const MAX_WIDTH: u32 = 3840;
const MAX_HEIGHT: u32 = 2160;

unsafe extern "C" {
    fn festerm_apple_video_create(
        path: *const c_char,
        width: u32,
        height: u32,
        error: *mut c_char,
        error_size: usize,
    ) -> *mut c_void;
    fn festerm_apple_video_append(
        encoder: *mut c_void,
        rgba: *const u8,
        length: usize,
        index: u64,
        error: *mut c_char,
        error_size: usize,
    ) -> bool;
    fn festerm_apple_video_finish(
        encoder: *mut c_void,
        error: *mut c_char,
        error_size: usize,
    ) -> bool;
    fn festerm_apple_video_destroy(encoder: *mut c_void);
}

/// A content-free failure from the operating system's MP4 encoder.
#[derive(Debug)]
pub struct VideoError(String);

impl VideoError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn native(buffer: &[c_char; ERROR_BYTES]) -> Self {
        // The native boundary always NUL-terminates its fixed-size error buffer.
        let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
        if text.is_empty() {
            Self::new("the Apple video encoder failed without an error message")
        } else {
            Self::new(text)
        }
    }
}

impl fmt::Display for VideoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for VideoError {}

/// One silent H.264 MP4, written to a new file in caller-owned private staging.
///
/// The encoder must run on an export worker, not a live-session or UI thread.
/// It accepts sequential opaque RGBA frames at a fixed 30 fps. Dropping an
/// unfinished encoder cancels the writer; callers retain the recording source.
pub struct AppleEncoder {
    native: NonNull<c_void>,
    bytes_per_frame: usize,
    next_frame: u64,
    finished: bool,
    failed: bool,
}

impl AppleEncoder {
    pub fn new(path: &Path, width: u32, height: u32) -> Result<Self, VideoError> {
        if width < 16
            || height < 16
            || width > MAX_WIDTH
            || height > MAX_HEIGHT
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
        {
            return Err(VideoError::new(
                "video dimensions must be even and between 16x16 and 3840x2160",
            ));
        }
        let path = path
            .to_str()
            .ok_or_else(|| VideoError::new("the private video path is not valid UTF-8"))?;
        let path = CString::new(path)
            .map_err(|_| VideoError::new("the private video path contains a NUL byte"))?;
        let mut error = [0; ERROR_BYTES];
        // The native constructor copies the path and returns an owned writer.
        let native = unsafe {
            festerm_apple_video_create(
                path.as_ptr(),
                width,
                height,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let native = NonNull::new(native).ok_or_else(|| VideoError::native(&error))?;
        Ok(Self {
            native,
            bytes_per_frame: width as usize * height as usize * 4,
            next_frame: 0,
            finished: false,
            failed: false,
        })
    }

    pub fn append(&mut self, rgba: &[u8]) -> Result<(), VideoError> {
        if self.finished || self.failed {
            return Err(VideoError::new("the video encoder is no longer active"));
        }
        if rgba.len() != self.bytes_per_frame {
            return Err(VideoError::new("the video frame has the wrong dimensions"));
        }
        if rgba.chunks_exact(4).any(|pixel| pixel[3] != 255) {
            return Err(VideoError::new("video frames must be opaque RGBA"));
        }
        let mut error = [0; ERROR_BYTES];
        // The native method consumes no more than `rgba.len()` and returns
        // before the borrowed frame is released.
        let ok = unsafe {
            festerm_apple_video_append(
                self.native.as_ptr(),
                rgba.as_ptr(),
                rgba.len(),
                self.next_frame,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if !ok {
            self.failed = true;
            return Err(VideoError::native(&error));
        }
        self.next_frame = self
            .next_frame
            .checked_add(1)
            .ok_or_else(|| VideoError::new("the video frame index overflows"))?;
        Ok(())
    }

    /// Completes the MP4 container. The caller must sync and publish it only
    /// after every stream in the synchronized export has finished successfully.
    pub fn finish(&mut self) -> Result<(), VideoError> {
        if self.finished || self.failed {
            return Err(VideoError::new("the video encoder is no longer active"));
        }
        if self.next_frame == 0 {
            return Err(VideoError::new("the video has no frames"));
        }
        let mut error = [0; ERROR_BYTES];
        // This call is serialized with append and returns after native
        // completion or an explicit failure.
        let ok = unsafe {
            festerm_apple_video_finish(self.native.as_ptr(), error.as_mut_ptr(), error.len())
        };
        if !ok {
            self.failed = true;
            return Err(VideoError::native(&error));
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for AppleEncoder {
    fn drop(&mut self) {
        // `native` was returned by the constructor and is destroyed exactly once.
        unsafe { festerm_apple_video_destroy(self.native.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;

    #[test]
    fn rejects_invalid_frames_without_creating_a_video() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.mp4");
        assert!(AppleEncoder::new(&path, 15, 64).is_err());
        assert!(!path.exists());
        let mut encoder = AppleEncoder::new(&path, 64, 64).unwrap();
        assert!(encoder.append(&[0; 3]).is_err());
        assert!(encoder.append(&vec![0; 64 * 64 * 4]).is_err());
        assert!(encoder.finish().is_err());
    }

    #[test]
    fn produces_a_finalized_mp4_with_sequential_frames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("video.mp4");
        encode_fixture(&path);
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.len() > 1000);
        for atom in [b"ftyp", b"mdat", b"moov"] {
            assert!(bytes.windows(atom.len()).any(|window| window == atom));
        }
        assert!(AppleEncoder::new(&path, 64, 64).is_err());
    }

    fn encode_fixture(path: &Path) {
        let mut encoder = AppleEncoder::new(path, 64, 64).unwrap();
        let mut frame = vec![255; 64 * 64 * 4];
        for index in 0..30 {
            for pixel in frame.chunks_exact_mut(4) {
                pixel[0] = index * 6;
                pixel[1] = 60;
                pixel[2] = 180;
            }
            encoder.append(&frame).unwrap();
        }
        encoder.finish().unwrap();
        assert!(encoder.finish().is_err());
        assert!(encoder.append(&frame).is_err());
    }
}
