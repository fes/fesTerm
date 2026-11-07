//! Available only in explicitly requested native-validation builds.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};

use festerm_core::{Dimensions, Terminal};
use festerm_recording::{Completion, FinishReason, Pacing, Recorder};
use festerm_recording_export::{stage_recording, AndroidEncoderFactory, ExportOptions};
use festerm_ui_egui::RecordingFrameSize;
use jni_legacy::{
    objects::{JClass, JString},
    sys::jint,
    JNIEnv,
};

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_RecordingNativeTest_encode(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    directory: JString<'_>,
) -> jint {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let directory: String = env
            .get_string(&directory)
            .map_err(|error| error.to_string())?
            .into();
        encode(Path::new(&directory))
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("fesTerm: Android native recording validation failed: {error}");
            1
        }
        Err(_) => {
            eprintln!("fesTerm: Android native recording validation panicked");
            2
        }
    }
}

fn encode(directory: &Path) -> Result<(), String> {
    let source_directory = directory.join("sources");
    let staging_directory = directory.join("staging");
    std::fs::create_dir(&source_directory).map_err(|error| error.to_string())?;
    std::fs::create_dir(&staging_directory).map_err(|error| error.to_string())?;
    let dimensions = Dimensions::new(30, 8).map_err(|error| error.to_string())?;
    let mut red = Terminal::new(dimensions).map_err(|error| error.to_string())?;
    let mut green = Terminal::new(dimensions).map_err(|error| error.to_string())?;
    let (mut recorder, streams) = Recorder::start(&source_directory, &[&red, &green], || {})
        .map_err(|error| error.to_string())?;
    for (terminal, stream, bytes) in [
        (&mut red, streams[0], b"\x1b[31mRED ONE\x1b[0m".as_slice()),
        (
            &mut green,
            streams[1],
            b"\x1b[32mGREEN TWO\x1b[0m".as_slice(),
        ),
    ] {
        let generation = terminal.screen_generation();
        terminal.ingest(bytes);
        recorder
            .observe_output(stream, bytes, generation, terminal)
            .map_err(|error| error.to_string())?;
    }
    thread::sleep(Duration::from_millis(150));
    recorder
        .stop(FinishReason::User)
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let source = loop {
        match recorder.poll_completion() {
            Some(Completion::Complete { path, .. }) => break path,
            Some(Completion::Incomplete { .. }) => return Err("capture was incomplete".to_owned()),
            Some(Completion::Failed { message, .. }) => return Err(message),
            None if Instant::now() >= deadline => return Err("capture timed out".to_owned()),
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    let mut factory = AndroidEncoderFactory;
    let staged = stage_recording(
        &source,
        &staging_directory,
        ExportOptions {
            pacing: Pacing::default(),
            size: RecordingFrameSize::new(256, 128).map_err(|error| error.to_string())?,
            font_size: 16.0,
        },
        &mut factory,
        &AtomicBool::new(false),
        |_| {},
    )
    .map_err(|error| error.to_string())?;
    if staged.videos().len() != 2 || !source.is_file() {
        return Err("the staged video set or retained source is missing".to_owned());
    }
    Ok(())
}
