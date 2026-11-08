use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    fmt,
    fs::{File, OpenOptions},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    path::Path,
    ptr::{self, NonNull},
    thread,
    time::{Duration, Instant},
};

use libloading::Library;

const MAX_WIDTH: u32 = 3840;
const MAX_HEIGHT: u32 = 2160;
const FRAME_WAIT: Duration = Duration::from_secs(30);
const FINISH_WAIT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const GST_STATE_NULL: c_int = 1;
const GST_STATE_PLAYING: c_int = 4;
const GST_STATE_CHANGE_FAILURE: c_int = 0;
const GST_FLOW_OK: c_int = 0;
const GST_MESSAGE_EOS: u32 = 1;
const GST_MESSAGE_ERROR: u32 = 2;

#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *mut c_char,
}

// These public prefixes are the GStreamer 1.x ABI used by GST_MESSAGE_TYPE.
#[repr(C)]
struct GstMiniObject {
    type_id: usize,
    refcount: c_int,
    lockstate: c_int,
    flags: u32,
    copy: *mut c_void,
    dispose: *mut c_void,
    free: *mut c_void,
    private_uint: u32,
    private_pointer: *mut c_void,
}

#[repr(C)]
struct GstMessage {
    mini_object: GstMiniObject,
    message_type: u32,
}

type InitCheck = unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> c_int;
type ParseLaunch = unsafe extern "C" fn(*const c_char, *mut *mut GError) -> *mut c_void;
type FindFactory = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type GetByName = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type GetBus = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type SetState = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type ObjectUnref = unsafe extern "C" fn(*mut c_void);
type BufferNew = unsafe extern "C" fn(*mut c_void, usize, *mut c_void) -> *mut c_void;
type BufferFill = unsafe extern "C" fn(*mut c_void, usize, *const c_void, usize) -> usize;
type MiniObjectUnref = unsafe extern "C" fn(*mut c_void);
type PushBuffer = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type EndStream = unsafe extern "C" fn(*mut c_void) -> c_int;
type QueuedBytes = unsafe extern "C" fn(*mut c_void) -> u64;
type PopMessage = unsafe extern "C" fn(*mut c_void, u64, u32) -> *mut GstMessage;
type ParseError = unsafe extern "C" fn(*mut GstMessage, *mut *mut GError, *mut *mut c_char);
type ErrorFree = unsafe extern "C" fn(*mut GError);
type Free = unsafe extern "C" fn(*mut c_void);

struct GstApi {
    init_check: InitCheck,
    parse_launch: ParseLaunch,
    find_factory: FindFactory,
    get_by_name: GetByName,
    get_bus: GetBus,
    set_state: SetState,
    object_unref: ObjectUnref,
    buffer_new: BufferNew,
    buffer_fill: BufferFill,
    mini_object_unref: MiniObjectUnref,
    push_buffer: PushBuffer,
    end_stream: EndStream,
    queued_bytes: QueuedBytes,
    pop_message: PopMessage,
    parse_error: ParseError,
    error_free: ErrorFree,
    free: Free,
    _core: Library,
    _app: Library,
    _glib: Library,
}

impl GstApi {
    fn load() -> Result<Self, VideoError> {
        let core = load_library("libgstreamer-1.0.so.0")?;
        let app = load_library("libgstapp-1.0.so.0")?;
        let glib = load_library("libglib-2.0.so.0")?;
        let api = Self {
            init_check: symbol(&core, b"gst_init_check\0")?,
            parse_launch: symbol(&core, b"gst_parse_launch\0")?,
            find_factory: symbol(&core, b"gst_element_factory_find\0")?,
            get_by_name: symbol(&core, b"gst_bin_get_by_name\0")?,
            get_bus: symbol(&core, b"gst_element_get_bus\0")?,
            set_state: symbol(&core, b"gst_element_set_state\0")?,
            object_unref: symbol(&core, b"gst_object_unref\0")?,
            buffer_new: symbol(&core, b"gst_buffer_new_allocate\0")?,
            buffer_fill: symbol(&core, b"gst_buffer_fill\0")?,
            mini_object_unref: symbol(&core, b"gst_mini_object_unref\0")?,
            push_buffer: symbol(&app, b"gst_app_src_push_buffer\0")?,
            end_stream: symbol(&app, b"gst_app_src_end_of_stream\0")?,
            queued_bytes: symbol(&app, b"gst_app_src_get_current_level_bytes\0")?,
            pop_message: symbol(&core, b"gst_bus_timed_pop_filtered\0")?,
            parse_error: symbol(&core, b"gst_message_parse_error\0")?,
            error_free: symbol(&glib, b"g_error_free\0")?,
            free: symbol(&glib, b"g_free\0")?,
            _core: core,
            _app: app,
            _glib: glib,
        };
        let mut error = ptr::null_mut();
        // GStreamer accepts null argc/argv and owns the returned error.
        let initialized =
            unsafe { (api.init_check)(ptr::null_mut(), ptr::null_mut(), &mut error) != 0 };
        if !initialized || !error.is_null() {
            return Err(VideoError::new(format!(
                "could not initialize system GStreamer: {}",
                api.take_error(error, "initialization failed")
            )));
        }
        Ok(api)
    }

    fn has_factory(&self, name: &CStr) -> bool {
        // The returned factory is an owned GstObject reference.
        let factory = unsafe { (self.find_factory)(name.as_ptr()) };
        if factory.is_null() {
            return false;
        }
        unsafe { (self.object_unref)(factory) };
        true
    }

    fn require_factory(&self, name: &CStr) -> Result<(), VideoError> {
        if self.has_factory(name) {
            return Ok(());
        }
        Err(VideoError::new(format!(
            "system GStreamer is missing the {} plugin; install the GStreamer base, good and bad plugins",
            name.to_string_lossy()
        )))
    }

    fn take_error(&self, error: *mut GError, fallback: &str) -> String {
        if error.is_null() {
            return fallback.to_owned();
        }
        // GError and its message remain valid until g_error_free.
        let message = unsafe {
            let text = (*error).message;
            if text.is_null() {
                fallback.to_owned()
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            }
        };
        unsafe { (self.error_free)(error) };
        message
    }
}

fn load_library(name: &str) -> Result<Library, VideoError> {
    // The system runtime is loaded only when an export is requested.
    unsafe { Library::new(name) }.map_err(|error| {
        VideoError::new(format!(
            "system GStreamer is unavailable ({name}: {error}); install the GStreamer runtime and H.264/MP4 plugins"
        ))
    })
}

fn symbol<T: Copy>(library: &Library, name: &'static [u8]) -> Result<T, VideoError> {
    // Each function pointer above matches the public GStreamer/GLib 1.x ABI;
    // the owning libraries outlive every use of the copied pointer.
    let function = unsafe { library.get::<T>(name) }.map_err(|error| {
        VideoError::new(format!(
            "system GStreamer is missing a required API ({}: {error})",
            String::from_utf8_lossy(&name[..name.len() - 1])
        ))
    })?;
    Ok(*function)
}

/// A content-free failure from the optional system video encoder.
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

/// A silent H.264 MP4 using only installed GStreamer libraries and plugins.
///
/// The encoder accepts sequential opaque RGBA frames at 30 fps on an export
/// worker. It exclusively creates its private destination; dropping an
/// unfinished encoder stops the pipeline.
pub struct LinuxEncoder {
    api: GstApi,
    file: File,
    pipeline: NonNull<c_void>,
    source: NonNull<c_void>,
    bus: NonNull<c_void>,
    bytes_per_frame: usize,
    next_frame: u64,
    finished: bool,
    failed: bool,
    stopped: bool,
}

impl LinuxEncoder {
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
        let api = GstApi::load()?;
        for plugin in [
            c"appsrc",
            c"rawvideoparse",
            c"videoconvert",
            c"h264parse",
            c"mp4mux",
            c"fdsink",
        ] {
            api.require_factory(plugin)?;
        }
        let bitrate = (u64::from(width) * u64::from(height) * 6).clamp(1_000_000, 50_000_000);
        let encoder = if api.has_factory(c"x264enc") {
            format!(
                "video/x-raw,format=Y444 ! x264enc bitrate={} key-int-max=30 bframes=0 tune=zerolatency",
                bitrate / 1_000
            )
        } else if api.has_factory(c"openh264enc") {
            format!("video/x-raw,format=I420 ! openh264enc bitrate={bitrate} gop-size=30")
        } else if api.has_factory(c"avenc_h264") {
            format!("video/x-raw,format=I420 ! avenc_h264 bitrate={bitrate}")
        } else {
            return Err(VideoError::new(
                "system GStreamer has no supported H.264 encoder; install an x264, OpenH264 or libav GStreamer encoder plugin",
            ));
        };
        let bytes_per_frame = width as usize * height as usize * 4;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| VideoError::new(format!("could not create private MP4: {error}")))?;
        let description = CString::new(format!(
            "appsrc name=frames is-live=false format=bytes block=false max-bytes={bytes_per_frame} \
             ! rawvideoparse use-sink-caps=false format=rgba width={width} height={height} framerate=30/1 \
             ! videoconvert ! {encoder} ! h264parse ! mp4mux ! fdsink fd={} sync=false",
            file.as_raw_fd()
        ))
        .expect("the generated pipeline contains only ASCII and integers");
        let mut error = ptr::null_mut();
        // The description contains no filename or shell input. A partial
        // pipeline returned alongside an error must still be released.
        let raw_pipeline = unsafe { (api.parse_launch)(description.as_ptr(), &mut error) };
        if raw_pipeline.is_null() || !error.is_null() {
            if !raw_pipeline.is_null() {
                unsafe { (api.object_unref)(raw_pipeline) };
            }
            return Err(VideoError::new(format!(
                "could not construct the GStreamer pipeline: {}",
                api.take_error(error, "pipeline construction failed")
            )));
        }
        let pipeline = NonNull::new(raw_pipeline).expect("pipeline is non-null");
        let raw_source = unsafe { (api.get_by_name)(pipeline.as_ptr(), c"frames".as_ptr()) };
        let raw_bus = unsafe { (api.get_bus)(pipeline.as_ptr()) };
        let (Some(source), Some(bus)) = (NonNull::new(raw_source), NonNull::new(raw_bus)) else {
            if !raw_source.is_null() {
                unsafe { (api.object_unref)(raw_source) };
            }
            if !raw_bus.is_null() {
                unsafe { (api.object_unref)(raw_bus) };
            }
            unsafe { (api.object_unref)(pipeline.as_ptr()) };
            return Err(VideoError::new(
                "GStreamer did not create its source or bus",
            ));
        };
        let encoder = Self {
            api,
            file,
            pipeline,
            source,
            bus,
            bytes_per_frame,
            next_frame: 0,
            finished: false,
            failed: false,
            stopped: false,
        };
        let state =
            unsafe { (encoder.api.set_state)(encoder.pipeline.as_ptr(), GST_STATE_PLAYING) };
        if state == GST_STATE_CHANGE_FAILURE {
            return Err(VideoError::new(
                "GStreamer could not start the MP4 pipeline",
            ));
        }
        Ok(encoder)
    }

    pub fn append(&mut self, rgba: &[u8]) -> Result<(), VideoError> {
        self.require_active()?;
        if rgba.len() != self.bytes_per_frame {
            return Err(VideoError::new("the video frame has the wrong dimensions"));
        }
        if rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 255) {
            return Err(VideoError::new("video frames must be opaque RGBA"));
        }
        let next_frame = self
            .next_frame
            .checked_add(1)
            .ok_or_else(|| VideoError::new("the video frame index overflows"))?;
        let deadline = Instant::now() + FRAME_WAIT;
        loop {
            match self.poll_bus(Duration::ZERO) {
                Ok(Some(BusEvent::End)) => {
                    self.failed = true;
                    return Err(VideoError::new("the GStreamer pipeline ended unexpectedly"));
                }
                Ok(None) => {}
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }
            let queued = unsafe { (self.api.queued_bytes)(self.source.as_ptr()) };
            if queued < self.bytes_per_frame as u64 {
                break;
            }
            if Instant::now() >= deadline {
                self.failed = true;
                return Err(VideoError::new(
                    "the GStreamer pipeline timed out accepting a frame",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
        // The source takes ownership of the buffer when it is pushed.
        let buffer = unsafe { (self.api.buffer_new)(ptr::null_mut(), rgba.len(), ptr::null_mut()) };
        if buffer.is_null() {
            self.failed = true;
            return Err(VideoError::new("could not allocate a GStreamer frame"));
        }
        let written =
            unsafe { (self.api.buffer_fill)(buffer, 0, rgba.as_ptr().cast(), rgba.len()) };
        if written != rgba.len() {
            unsafe { (self.api.mini_object_unref)(buffer) };
            self.failed = true;
            return Err(VideoError::new("could not fill a GStreamer frame"));
        }
        let flow = unsafe { (self.api.push_buffer)(self.source.as_ptr(), buffer) };
        if flow != GST_FLOW_OK {
            self.failed = true;
            self.poll_bus(Duration::ZERO)?;
            return Err(VideoError::new(format!(
                "GStreamer rejected a video frame (flow {flow})"
            )));
        }
        self.next_frame = next_frame;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), VideoError> {
        self.require_active()?;
        if self.next_frame == 0 {
            return Err(VideoError::new("the video has no frames"));
        }
        let flow = unsafe { (self.api.end_stream)(self.source.as_ptr()) };
        if flow != GST_FLOW_OK {
            self.failed = true;
            return Err(VideoError::new(format!(
                "GStreamer could not finish the video stream (flow {flow})"
            )));
        }
        let deadline = Instant::now() + FINISH_WAIT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.failed = true;
                return Err(VideoError::new("GStreamer timed out finalizing the MP4"));
            }
            match self.poll_bus(remaining.min(Duration::from_millis(250))) {
                Ok(Some(BusEvent::End)) => break,
                Ok(None) => {}
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        let state = unsafe { (self.api.set_state)(self.pipeline.as_ptr(), GST_STATE_NULL) };
        if state == GST_STATE_CHANGE_FAILURE {
            self.failed = true;
            return Err(VideoError::new(
                "GStreamer could not close the MP4 pipeline",
            ));
        }
        self.stopped = true;
        if let Err(error) = self.file.sync_all() {
            self.failed = true;
            return Err(VideoError::new(format!(
                "could not sync the private MP4: {error}"
            )));
        }
        self.finished = true;
        Ok(())
    }

    fn require_active(&self) -> Result<(), VideoError> {
        if self.finished || self.failed {
            Err(VideoError::new("the video encoder is no longer active"))
        } else {
            Ok(())
        }
    }

    fn poll_bus(&self, timeout: Duration) -> Result<Option<BusEvent>, VideoError> {
        let timeout_ns = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX - 1);
        // Only EOS and error messages are returned. The message's public type
        // prefix is immutable while this owned reference is held.
        let message = unsafe {
            (self.api.pop_message)(
                self.bus.as_ptr(),
                timeout_ns,
                GST_MESSAGE_EOS | GST_MESSAGE_ERROR,
            )
        };
        if message.is_null() {
            return Ok(None);
        }
        let kind = unsafe { ptr::addr_of!((*message).message_type).read() };
        let result = match kind {
            GST_MESSAGE_EOS => Ok(Some(BusEvent::End)),
            GST_MESSAGE_ERROR => {
                let mut error = ptr::null_mut();
                let mut debug = ptr::null_mut();
                unsafe { (self.api.parse_error)(message, &mut error, &mut debug) };
                if !debug.is_null() {
                    unsafe { (self.api.free)(debug.cast()) };
                }
                Err(VideoError::new(format!(
                    "GStreamer could not encode the video: {}",
                    self.api.take_error(error, "pipeline failed")
                )))
            }
            _ => Err(VideoError::new("GStreamer returned an unexpected message")),
        };
        unsafe { (self.api.mini_object_unref)(message.cast()) };
        result
    }
}

enum BusEvent {
    End,
}

impl Drop for LinuxEncoder {
    fn drop(&mut self) {
        if !self.stopped {
            unsafe { (self.api.set_state)(self.pipeline.as_ptr(), GST_STATE_NULL) };
        }
        unsafe {
            (self.api.object_unref)(self.bus.as_ptr());
            (self.api.object_unref)(self.source.as_ptr());
            (self.api.object_unref)(self.pipeline.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[test]
    fn invalid_dimensions_do_not_create_a_video() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.mp4");
        assert!(LinuxEncoder::new(&path, 15, 64).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn missing_plugins_are_actionable_and_do_not_create_a_video() {
        const CHILD: &str = "FESTERM_TEST_EMPTY_GSTREAMER_PLUGINS";
        const VIDEO: &str = "FESTERM_TEST_EMPTY_GSTREAMER_VIDEO";
        if std::env::var_os(CHILD).is_some() {
            let path = std::env::var_os(VIDEO).expect("the parent supplies a private path");
            let path = Path::new(&path);
            let error = match LinuxEncoder::new(path, 64, 64) {
                Ok(_) => panic!("an empty plugin search path must fail"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("GStreamer"));
            assert!(error.to_string().contains("install"));
            assert!(!path.exists());
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "linux::tests::missing_plugins_are_actionable_and_do_not_create_a_video",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(VIDEO, directory.path().join("video.mp4"))
            .env("GST_PLUGIN_SYSTEM_PATH", "")
            .env("GST_PLUGIN_SYSTEM_PATH_1_0", "")
            .env("GST_PLUGIN_PATH", "")
            .env("GST_PLUGIN_PATH_1_0", "")
            .env(
                "GST_REGISTRY_1_0",
                directory.path().join("empty-registry.bin"),
            )
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "isolated missing-plugin test failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            String::from_utf8_lossy(&result.stdout).contains("1 passed"),
            "the isolated missing-plugin test did not run"
        );
    }

    #[test]
    #[ignore = "requires system GStreamer H.264/MP4 plugins and ffmpeg/ffprobe"]
    fn produces_a_silent_mp4_with_sequential_frames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("video.mp4");
        let mut encoder = LinuxEncoder::new(&path, 320, 180).unwrap();
        let mut frame = vec![255; 320 * 180 * 4];
        for index in 0..90 {
            for pixel in frame.as_chunks_mut::<4>().0 {
                pixel[0] = if index < 30 { 220 } else { 20 };
                pixel[1] = if (30..60).contains(&index) { 220 } else { 20 };
                pixel[2] = if index >= 60 { 220 } else { 20 };
            }
            encoder.append(&frame).unwrap();
        }
        encoder.finish().unwrap();
        assert!(encoder.finish().is_err());
        assert!(encoder.append(&frame).is_err());
        assert!(LinuxEncoder::new(&path, 320, 180).is_err());
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
            let offset = index * stride + (90 * 320 + 160) * 3;
            let pixel = &decoded.stdout[offset..offset + 3];
            assert!(pixel[expected] > 180, "{pixel:?}");
            for (channel, &value) in pixel.iter().enumerate() {
                if channel != expected {
                    assert!(value < 60, "{pixel:?}");
                }
            }
        }
    }
}
