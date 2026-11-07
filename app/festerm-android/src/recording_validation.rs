//! Available only in explicitly requested native-validation builds.

use std::{
    fs::{self, DirBuilder, Permissions},
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
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
    for path in [&source_directory, &staging_directory] {
        DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|error| error.to_string())?;
    }
    let dimensions = Dimensions::new(30, 8).map_err(|error| error.to_string())?;
    let mut red = Terminal::new(dimensions).map_err(|error| error.to_string())?;
    let mut green = Terminal::new(dimensions).map_err(|error| error.to_string())?;
    verify_collision(&source_directory, &red)?;
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
    let source = match await_completion(&mut recorder)? {
        Completion::Complete { path, .. } => path,
        Completion::Incomplete { .. } => return Err("capture was incomplete".to_owned()),
        Completion::Failed { message, .. } => return Err(message),
    };
    let mut factory = AndroidEncoderFactory;
    let options = ExportOptions {
        pacing: Pacing::default(),
        size: RecordingFrameSize::new(256, 128).map_err(|error| error.to_string())?,
        font_size: 16.0,
    };
    let stage = |source: &Path, factory: &mut AndroidEncoderFactory| {
        stage_recording(
            source,
            &staging_directory,
            options,
            factory,
            &AtomicBool::new(false),
            |_| {},
        )
    };
    fs::set_permissions(&staging_directory, Permissions::from_mode(0o755))
        .map_err(|error| error.to_string())?;
    if stage(&source, &mut factory).is_ok() {
        return Err("non-private video staging was accepted".to_owned());
    }
    fs::set_permissions(&staging_directory, Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    fs::set_permissions(&source, Permissions::from_mode(0o644))
        .map_err(|error| error.to_string())?;
    if stage(&source, &mut factory).is_ok() {
        return Err("a non-private recording source was accepted".to_owned());
    }
    fs::set_permissions(&source, Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    let link = source_directory.join("linked.fstrec");
    symlink(&source, &link).map_err(|error| error.to_string())?;
    if stage(&link, &mut factory).is_ok() {
        return Err("a symlink recording source was accepted".to_owned());
    }
    fs::remove_file(link).map_err(|error| error.to_string())?;
    let staged = stage(&source, &mut factory).map_err(|error| error.to_string())?;
    if staged.videos().len() != 2 || !source.is_file() {
        return Err("the staged video set or retained source is missing".to_owned());
    }
    Ok(())
}

fn verify_collision(directory: &Path, terminal: &Terminal) -> Result<(), String> {
    let (mut recorder, _) =
        Recorder::start(directory, &[terminal], || {}).map_err(|error| error.to_string())?;
    let existing = directory.join(format!("recording-{}.fstrec", recorder.id()));
    fs::write(&existing, b"existing recording").map_err(|error| error.to_string())?;
    recorder
        .stop(FinishReason::User)
        .map_err(|error| error.to_string())?;
    let partial = match await_completion(&mut recorder)? {
        Completion::Failed { path, .. } => path,
        _ => return Err("a source collision did not fail".to_owned()),
    };
    if !partial.is_file()
        || fs::read(&existing).map_err(|error| error.to_string())? != b"existing recording"
    {
        return Err("a source collision did not preserve both files".to_owned());
    }
    fs::remove_file(partial).map_err(|error| error.to_string())?;
    fs::remove_file(existing).map_err(|error| error.to_string())?;
    Ok(())
}

fn await_completion(recorder: &mut Recorder) -> Result<Completion, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match recorder.poll_completion() {
            Some(completion) => return Ok(completion),
            None if Instant::now() >= deadline => return Err("capture timed out".to_owned()),
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}
