use std::{
    ffi::c_void,
    fmt,
    marker::PhantomData,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::{
    core::{Interface, HRESULT, PCWSTR},
    Win32::{
        Media::MediaFoundation::{
            IMFAttributes, IMFByteStream, IMFMediaBuffer, IMFMediaType, IMFSample, IMFSinkWriter,
            MFMediaType_Video, MFTranscodeContainerType_MPEG4, MFVideoFormat_H264,
            MFVideoFormat_RGB32, MFVideoInterlace_Progressive, MFSTARTUP_FULL, MF_ACCESSMODE_WRITE,
            MF_FILEFLAGS_NONE, MF_FILE_ACCESSMODE, MF_FILE_FLAGS, MF_FILE_OPENMODE,
            MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE,
            MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
            MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_OPENMODE_FAIL_IF_EXIST,
            MF_SINK_WRITER_DISABLE_THROTTLING, MF_SINK_WRITER_STATISTICS,
            MF_TRANSCODE_CONTAINERTYPE, MF_VERSION,
        },
        System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
    },
};

const MAX_WIDTH: u32 = 3840;
const MAX_HEIGHT: u32 = 2160;
const TICKS_PER_SECOND: u64 = 10_000_000;
const FRAMES_PER_SECOND: u64 = 30;
const MAX_QUEUED_FRAMES: u64 = 16;
const FRAME_WAIT: Duration = Duration::from_secs(30);

type Startup = unsafe extern "system" fn(u32, u32) -> HRESULT;
type Shutdown = unsafe extern "system" fn() -> HRESULT;
type CreateFile = unsafe extern "system" fn(
    MF_FILE_ACCESSMODE,
    MF_FILE_OPENMODE,
    MF_FILE_FLAGS,
    PCWSTR,
    *mut *mut c_void,
) -> HRESULT;
type CreateObject = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type CreateBuffer = unsafe extern "system" fn(u32, *mut *mut c_void) -> HRESULT;
type CreateAttributes = unsafe extern "system" fn(*mut *mut c_void, u32) -> HRESULT;
type CreateWriter =
    unsafe extern "system" fn(PCWSTR, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT;

struct MfApi {
    startup: Startup,
    shutdown: Shutdown,
    create_file: CreateFile,
    create_media_type: CreateObject,
    create_sample: CreateObject,
    create_buffer: CreateBuffer,
    create_attributes: CreateAttributes,
    create_writer: CreateWriter,
    _platform: Library,
    _readwrite: Library,
}

impl MfApi {
    fn load() -> Result<Self, VideoError> {
        let platform = load_library("mfplat.dll")?;
        let readwrite = load_library("mfreadwrite.dll")?;
        Ok(Self {
            startup: symbol(&platform, b"MFStartup\0")?,
            shutdown: symbol(&platform, b"MFShutdown\0")?,
            create_file: symbol(&platform, b"MFCreateFile\0")?,
            create_media_type: symbol(&platform, b"MFCreateMediaType\0")?,
            create_sample: symbol(&platform, b"MFCreateSample\0")?,
            create_buffer: symbol(&platform, b"MFCreateMemoryBuffer\0")?,
            create_attributes: symbol(&platform, b"MFCreateAttributes\0")?,
            create_writer: symbol(&readwrite, b"MFCreateSinkWriterFromURL\0")?,
            _platform: platform,
            _readwrite: readwrite,
        })
    }

    fn media_type(&self) -> Result<IMFMediaType, VideoError> {
        create_object("create a video media type", |out| {
            // The loaded function uses the documented Media Foundation ABI.
            unsafe { (self.create_media_type)(out) }
        })
    }
}

fn load_library(name: &str) -> Result<Library, VideoError> {
    // Load only from System32, and only when an export is requested.
    unsafe { Library::load_with_flags(name, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|error| {
        VideoError::new(format!(
            "Windows Media Foundation is unavailable ({name}: {error}); install the Windows Media Feature Pack"
        ))
    })
}

fn symbol<T: Copy>(library: &Library, name: &'static [u8]) -> Result<T, VideoError> {
    // The function pointer types match the public Media Foundation signatures;
    // the owning libraries remain loaded while any pointer is in use.
    let function = unsafe { library.get::<T>(name) }.map_err(|error| {
        VideoError::new(format!(
            "Windows Media Foundation is missing a required API ({}: {error})",
            String::from_utf8_lossy(&name[..name.len() - 1])
        ))
    })?;
    Ok(*function)
}

fn create_object<T: Interface>(
    action: &str,
    create: impl FnOnce(*mut *mut c_void) -> HRESULT,
) -> Result<T, VideoError> {
    let mut object = ptr::null_mut();
    let result = create(&mut object);
    if result.is_err() {
        if !object.is_null() {
            // An unexpected returned reference still belongs to this caller.
            unsafe { drop(T::from_raw(object)) };
        }
        return Err(VideoError::new(format!(
            "could not {action} (Windows error {result:?}); check that Media Foundation and an H.264 encoder are installed"
        )));
    }
    if object.is_null() {
        return Err(VideoError::new(format!(
            "could not {action}: Windows returned an empty object"
        )));
    }
    // Media Foundation returned one owned reference to the requested interface.
    Ok(unsafe { T::from_raw(object) })
}

fn mf<T>(action: &str, result: windows::core::Result<T>) -> Result<T, VideoError> {
    result.map_err(|error| {
        VideoError::new(format!(
            "could not {action} (Windows error {:#x}); check that Media Foundation and an H.264 encoder are installed",
            error.code().0
        ))
    })
}

struct MfContext {
    api: MfApi,
    active: bool,
    _apartment: PhantomData<Rc<()>>,
}

impl MfContext {
    fn new() -> Result<Self, VideoError> {
        let api = MfApi::load()?;
        // This context and its COM objects remain on the export worker thread.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|error| VideoError::new(format!("could not initialize COM: {error}")))?;
        let result = unsafe { (api.startup)(MF_VERSION, MFSTARTUP_FULL) };
        if result.is_err() {
            unsafe { CoUninitialize() };
            return Err(VideoError::new(format!(
                "could not initialize Windows Media Foundation ({result:?}); install the Windows Media Feature Pack"
            )));
        }
        Ok(Self {
            api,
            active: true,
            _apartment: PhantomData,
        })
    }

    fn shutdown(&mut self) -> Result<(), VideoError> {
        if self.active {
            self.active = false;
            let result = unsafe { (self.api.shutdown)() };
            unsafe { CoUninitialize() };
            if result.is_err() {
                return Err(VideoError::new(format!(
                    "could not shut down Windows Media Foundation ({result:?})"
                )));
            }
        }
        Ok(())
    }
}

impl Drop for MfContext {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("fesTerm: {error}");
        }
    }
}

/// A content-free failure from the operating system's MP4 encoder.
#[derive(Debug)]
pub struct VideoError(String);

impl VideoError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
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
/// The encoder is confined to its export worker thread. Dropping an unfinished
/// encoder releases its writer without publishing; callers retain the source.
pub struct WindowsEncoder {
    writer: Option<IMFSinkWriter>,
    output: Option<IMFByteStream>,
    context: MfContext,
    stream: u32,
    bytes_per_frame: usize,
    next_frame: u64,
    finished: bool,
    failed: bool,
}

impl WindowsEncoder {
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
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| VideoError::new("the private video path has no filename"))?;
        let directory = festerm_windows_security::lock_directory_path_without_delete_sharing(
            parent,
        )
        .map_err(|error| VideoError::new(format!("could not open private staging: {error}")))?;
        let private =
            festerm_windows_security::is_current_user_only(&directory).map_err(|error| {
                VideoError::new(format!("could not verify private staging: {error}"))
            })?;
        if !private {
            return Err(VideoError::new(
                "video staging is not private to the current user",
            ));
        }
        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide_path.contains(&0) {
            return Err(VideoError::new("the private video path contains a NUL"));
        }
        wide_path.push(0);
        let path = PCWSTR(wide_path.as_ptr());
        let context = MfContext::new()?;
        let api = &context.api;
        let output: IMFByteStream = create_object("create a private MP4 file", |out| unsafe {
            (api.create_file)(
                MF_ACCESSMODE_WRITE,
                MF_OPENMODE_FAIL_IF_EXIST,
                MF_FILEFLAGS_NONE,
                path,
                out,
            )
        })?;
        let created =
            festerm_windows_security::open_file_no_reparse_for_capture(&directory, Path::new(name))
                .map_err(|error| {
                    VideoError::new(format!("could not secure the new MP4 file: {error}"))
                })?;
        festerm_windows_security::restrict_to_current_user(&created).map_err(|error| {
            VideoError::new(format!("could not secure the new MP4 file: {error}"))
        })?;
        drop(created);
        let attributes: IMFAttributes =
            create_object("configure the MP4 container", |out| unsafe {
                (api.create_attributes)(out, 2)
            })?;
        mf("select the MP4 container", unsafe {
            attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)
        })?;
        mf("configure offline encoding", unsafe {
            attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)
        })?;
        let writer: IMFSinkWriter = create_object("create the MP4 writer", |out| unsafe {
            (api.create_writer)(path, output.as_raw(), attributes.as_raw(), out)
        })?;
        let output_type = api.media_type()?;
        configure_video_type(&output_type, width, height)?;
        mf("select H.264 video", unsafe {
            output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
        })?;
        let bitrate = (u64::from(width) * u64::from(height) * 4).clamp(2_000_000, 24_000_000);
        mf("set the video bit rate", unsafe {
            output_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate as u32)
        })?;
        let stream = mf("add the video stream", unsafe {
            writer.AddStream(&output_type)
        })?;
        let input_type = api.media_type()?;
        configure_video_type(&input_type, width, height)?;
        mf("select RGB input frames", unsafe {
            input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
        })?;
        mf("set the input frame stride", unsafe {
            input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)
        })?;
        mf("configure independent input frames", unsafe {
            input_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
        })?;
        mf("configure the H.264 encoder", unsafe {
            writer.SetInputMediaType(stream, &input_type, None::<&IMFAttributes>)
        })?;
        mf("start the MP4 writer", unsafe { writer.BeginWriting() })?;
        Ok(Self {
            writer: Some(writer),
            output: Some(output),
            context,
            stream,
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
        let next = self
            .next_frame
            .checked_add(1)
            .ok_or_else(|| VideoError::new("the video frame index overflows"))?;
        let time = frame_time(self.next_frame)?;
        let duration = frame_time(next)? - time;
        let result = self.append_frame(rgba, time, duration);
        if result.is_err() {
            self.failed = true;
        } else {
            self.next_frame = next;
        }
        result
    }

    fn append_frame(&self, rgba: &[u8], time: i64, duration: i64) -> Result<(), VideoError> {
        let api = &self.context.api;
        let buffer: IMFMediaBuffer = create_object("allocate a video frame", |out| unsafe {
            (api.create_buffer)(self.bytes_per_frame as u32, out)
        })?;
        let mut data = ptr::null_mut();
        let mut maximum = 0;
        mf("lock a video frame", unsafe {
            buffer.Lock(&mut data, Some(&mut maximum), None)
        })?;
        if data.is_null() || (maximum as usize) < self.bytes_per_frame {
            mf("unlock an invalid video frame", unsafe { buffer.Unlock() })?;
            return Err(VideoError::new(
                "Windows returned a video frame buffer that is too small",
            ));
        }
        // The locked buffer is writable for `maximum` bytes until Unlock.
        let destination = unsafe { std::slice::from_raw_parts_mut(data, self.bytes_per_frame) };
        for (source, target) in rgba.chunks_exact(4).zip(destination.chunks_exact_mut(4)) {
            target.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
        }
        mf("unlock a video frame", unsafe { buffer.Unlock() })?;
        mf("set the video frame length", unsafe {
            buffer.SetCurrentLength(self.bytes_per_frame as u32)
        })?;
        let sample: IMFSample = create_object("create a video sample", |out| unsafe {
            (api.create_sample)(out)
        })?;
        mf("attach the video frame", unsafe {
            sample.AddBuffer(&buffer)
        })?;
        mf("set the video frame time", unsafe {
            sample.SetSampleTime(time)
        })?;
        mf("set the video frame duration", unsafe {
            sample.SetSampleDuration(duration)
        })?;
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| VideoError::new("the video writer is no longer available"))?;
        wait_for_encoder(writer, self.stream)?;
        mf("write the video frame", unsafe {
            writer.WriteSample(self.stream, &sample)
        })
    }

    pub fn finish(&mut self) -> Result<(), VideoError> {
        if self.finished || self.failed {
            return Err(VideoError::new("the video encoder is no longer active"));
        }
        if self.next_frame == 0 {
            return Err(VideoError::new("the video has no frames"));
        }
        let result = (|| {
            let writer = self
                .writer
                .as_ref()
                .ok_or_else(|| VideoError::new("the video writer is no longer available"))?;
            mf("finalize the MP4", unsafe { writer.Finalize() })?;
            self.writer.take();
            // Finalize closes the sink's byte stream. Staging reopens and
            // syncs the completed file before it can be published.
            self.output.take();
            self.context.shutdown()
        })();
        if result.is_err() {
            self.failed = true;
        } else {
            self.finished = true;
        }
        result
    }
}

impl Drop for WindowsEncoder {
    fn drop(&mut self) {
        // COM objects must be released before Media Foundation is shut down.
        self.writer.take();
        self.output.take();
    }
}

fn configure_video_type(
    media_type: &IMFMediaType,
    width: u32,
    height: u32,
) -> Result<(), VideoError> {
    mf("set the video major type", unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
    })?;
    mf("set the video frame size", unsafe {
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pair(width, height))
    })?;
    mf("set the video frame rate", unsafe {
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pair(FRAMES_PER_SECOND as u32, 1))
    })?;
    mf("set the pixel aspect ratio", unsafe {
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pair(1, 1))
    })?;
    mf("set progressive video", unsafe {
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
    })
}

const fn pair(first: u32, second: u32) -> u64 {
    ((first as u64) << 32) | second as u64
}

fn frame_time(index: u64) -> Result<i64, VideoError> {
    let ticks = u128::from(index) * u128::from(TICKS_PER_SECOND) / u128::from(FRAMES_PER_SECOND);
    i64::try_from(ticks).map_err(|_| VideoError::new("the video duration overflows"))
}

fn wait_for_encoder(writer: &IMFSinkWriter, stream: u32) -> Result<(), VideoError> {
    let deadline = Instant::now() + FRAME_WAIT;
    loop {
        let mut statistics = MF_SINK_WRITER_STATISTICS {
            cb: std::mem::size_of::<MF_SINK_WRITER_STATISTICS>() as u32,
            ..Default::default()
        };
        mf("inspect the video encoder queue", unsafe {
            writer.GetStatistics(stream, &mut statistics)
        })?;
        if statistics
            .qwNumSamplesReceived
            .saturating_sub(statistics.qwNumSamplesProcessed)
            < MAX_QUEUED_FRAMES
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(VideoError::new(
                "the Windows H.264 encoder did not process queued frames in time",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[test]
    fn rejects_invalid_dimensions_without_creating_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.mp4");
        assert!(WindowsEncoder::new(&path, 15, 64).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn frame_times_remain_aligned_at_thirty_fps() {
        assert_eq!(frame_time(0).unwrap(), 0);
        assert_eq!(frame_time(1).unwrap(), 333_333);
        assert_eq!(frame_time(2).unwrap(), 666_666);
        assert_eq!(frame_time(30).unwrap(), 10_000_000);
    }

    #[test]
    #[ignore = "requires Windows Media Foundation and ffmpeg/ffprobe"]
    fn produces_a_silent_mp4_with_sequential_top_down_frames() {
        let directory = tempfile::tempdir().unwrap();
        let parent =
            festerm_windows_security::lock_directory_path_without_delete_sharing(directory.path())
                .unwrap();
        let stage = festerm_windows_security::create_current_user_only_directory(
            &parent,
            Path::new("stage"),
        )
        .unwrap();
        drop((stage, parent));
        let path = directory.path().join("stage").join("video.mp4");
        let mut encoder = WindowsEncoder::new(&path, 320, 180).unwrap();
        let mut frame = vec![255; 320 * 180 * 4];
        for index in 0..90 {
            for (position, pixel) in frame.chunks_exact_mut(4).enumerate() {
                let top = position / 320 < 90;
                pixel[0] = if top && index < 30 { 220 } else { 20 };
                pixel[1] = if top && (30..60).contains(&index) {
                    220
                } else {
                    20
                };
                pixel[2] = if !top || index >= 60 { 220 } else { 20 };
            }
            encoder.append(&frame).unwrap();
        }
        encoder.finish().unwrap();
        assert!(encoder.finish().is_err());
        let parent = festerm_windows_security::lock_directory_path_without_delete_sharing(
            path.parent().unwrap(),
        )
        .unwrap();
        let output = festerm_windows_security::open_file_no_reparse(
            &parent,
            Path::new(path.file_name().unwrap()),
        )
        .unwrap();
        assert!(festerm_windows_security::is_current_user_only(&output).unwrap());
        drop((output, parent));
        assert!(encoder.append(&frame).is_err());
        assert!(WindowsEncoder::new(&path, 320, 180).is_err());
        let bytes = fs::read(&path).unwrap();
        for atom in [b"ftyp", b"mdat", b"moov"] {
            assert!(bytes.windows(atom.len()).any(|window| window == atom));
        }

        let video = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height,avg_frame_rate,nb_frames",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&path)
            .output()
            .expect("ffprobe is required for this native test");
        assert!(
            video.status.success(),
            "{}",
            String::from_utf8_lossy(&video.stderr)
        );
        let properties = String::from_utf8(video.stdout).unwrap();
        for expected in [
            "codec_name=h264",
            "width=320",
            "height=180",
            "avg_frame_rate=30/1",
            "nb_frames=90",
        ] {
            assert!(
                properties.lines().any(|line| line == expected),
                "{properties}"
            );
        }
        let audio = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_entries",
                "stream=codec_type",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(audio.status.success());
        assert!(audio.stdout.is_empty());

        let decoded = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args([
                "-vf",
                "select=eq(n\\,0)+eq(n\\,30)+eq(n\\,60)",
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "pipe:1",
            ])
            .output()
            .expect("ffmpeg is required for this native test");
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let stride = 320 * 180 * 3;
        assert_eq!(decoded.stdout.len(), stride * 3);
        for (index, expected) in [(0, 0), (1, 1), (2, 2)] {
            for (y, channel) in [(45, expected), (135, 2)] {
                let offset = index * stride + (y * 320 + 160) * 3;
                let pixel = &decoded.stdout[offset..offset + 3];
                assert!(pixel[channel] > 180, "{pixel:?}");
                for (other, &value) in pixel.iter().enumerate() {
                    if other != channel {
                        assert!(value < 60, "{pixel:?}");
                    }
                }
            }
        }
    }
}
