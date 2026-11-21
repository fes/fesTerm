#![cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]

use std::{
    env, fs,
    path::Path,
    sync::{atomic::AtomicBool, Arc, Mutex},
};

use festerm_automation_js::{
    platform,
    program::{
        ApprovedProgram, ProgramLaunchMode, ProgramLaunchParams, ProgramSourceSnapshot,
        TerminalSize,
    },
    ProgramDeliveryFence, ProgramStorageCommit, ProgramStorageCommitRequest, ProgramStorageHost,
    ProgramStorageLoad, ProgramSupervisor, QuickJsProgramRuntime, WorkerProgram,
};

fn program(snapshot: &Path) -> ApprovedProgram {
    let snapshot: ProgramSourceSnapshot =
        serde_json::from_slice(&fs::read(snapshot).expect("read program snapshot"))
            .expect("parse program snapshot");
    ApprovedProgram::new(snapshot).expect("approve program")
}

fn output_text(chunks: Vec<Vec<u8>>) -> String {
    String::from_utf8(chunks.concat()).expect("program output is UTF-8")
}

struct Fence;

impl ProgramDeliveryFence for Fence {
    fn is_live(&self, generation: u64) -> bool {
        generation == 19
    }

    fn try_enter(
        &self,
        generation: u64,
    ) -> Result<
        Option<Box<dyn festerm_automation_js::ProgramDeliveryPermit + '_>>,
        festerm_automation_js::Error,
    > {
        if self.is_live(generation) {
            Ok(Some(Box::new(())))
        } else {
            Err(festerm_automation_js::Error::DisclosureRevoked)
        }
    }
}

#[derive(Clone, Default)]
struct TestStorage(Arc<Mutex<Option<SavedState>>>);

type SavedState = (u64, Vec<u8>);

impl TestStorage {
    fn bytes(&self) -> Vec<u8> {
        self.0
            .lock()
            .expect("storage lock")
            .as_ref()
            .map(|(_, bytes)| bytes.clone())
            .unwrap_or_default()
    }
}

impl ProgramStorageHost for TestStorage {
    fn load(&self, slot: &str) -> ProgramStorageLoad {
        if slot != "universe" {
            return ProgramStorageLoad::Unavailable;
        }
        self.0.lock().expect("storage lock").clone().map_or(
            ProgramStorageLoad::Missing,
            |(revision, value)| ProgramStorageLoad::Present {
                schema_version: 2,
                revision,
                value,
            },
        )
    }

    fn commit(&self, slot: &str, request: ProgramStorageCommitRequest) -> ProgramStorageCommit {
        if slot != "universe" || request.schema_version != 2 {
            return ProgramStorageCommit::ResourceLimit;
        }
        let mut saved = self.0.lock().expect("storage lock");
        let actual = saved.as_ref().map(|(revision, _)| *revision);
        if request.expected_revision != actual {
            return ProgramStorageCommit::Conflict {
                actual_revision: actual,
            };
        }
        let revision = actual.unwrap_or(0) + 1;
        *saved = Some((revision, request.value));
        ProgramStorageCommit::Committed { revision }
    }
}

#[test]
#[ignore = "requires a locally built TW2K2 program snapshot"]
fn tw2k2_boots_and_accepts_basic_gameplay() {
    let snapshot = env::var_os("TW2K2_SNAPSHOT").expect("set TW2K2_SNAPSHOT");
    let storage = TestStorage::default();
    let runtime = QuickJsProgramRuntime::new_with_storage(
        program(Path::new(&snapshot)),
        ProgramLaunchParams::new(ProgramLaunchMode::New),
        Arc::new(storage.clone()),
    )
    .expect("build runtime");
    let welcome = output_text(
        runtime
            .start(TerminalSize {
                columns: 80,
                rows: 24,
            })
            .expect("start tw2k2"),
    );
    assert!(welcome.contains("Callsign:"), "{welcome:?}");

    let ship = output_text(runtime.input(b"Test Pilot\r").expect("submit callsign"));
    assert!(ship.contains("Ship") || ship.contains("ship"), "{ship:?}");

    let sector = output_text(runtime.input(b"Test Ship\r").expect("submit ship name"));
    assert!(sector.contains("SECTOR"), "{sector:?}");

    let status = output_text(runtime.input(b"I\r").expect("open ship status"));
    assert!(
        status.contains("Merchant Cruiser") || status.contains("Ship"),
        "{status:?}"
    );

    let moved = output_text(runtime.input(b"M 2\r").expect("move to adjacent sector"));
    assert!(
        moved.contains("Sector: 2") || moved.contains("Sector 2") || moved.contains("SECTOR 2"),
        "{moved:?}"
    );

    let resized = output_text(
        runtime
            .resize(TerminalSize {
                columns: 100,
                rows: 32,
            })
            .expect("resize terminal"),
    );
    assert!(
        resized.contains("Sector 2") || resized.contains("SECTOR 2"),
        "{resized:?}"
    );

    let redisplay = output_text(runtime.input(b"D\r").expect("redisplay sector"));
    assert!(
        redisplay.contains("Sector 2") || redisplay.contains("SECTOR 2"),
        "{redisplay:?}"
    );
    let save = output_text(runtime.input(b"SAVE\r").expect("save game"));
    assert!(save.contains("Checkpoint"), "{save:?}");
    assert!(serde_json::from_slice::<serde_json::Value>(&storage.bytes()).is_ok());
    runtime.close().expect("close runtime");

    let resumed = QuickJsProgramRuntime::new_with_storage(
        program(Path::new(&snapshot)),
        ProgramLaunchParams::new(ProgramLaunchMode::Resume),
        Arc::new(storage),
    )
    .expect("build resumed runtime");
    let welcome = output_text(
        resumed
            .start(TerminalSize {
                columns: 80,
                rows: 24,
            })
            .expect("resume tw2k2"),
    );
    assert!(
        welcome.contains("Welcome back") && welcome.contains("SECTOR 2"),
        "{welcome:?}"
    );
    resumed.close().expect("close resumed runtime");
}

#[test]
#[ignore = "requires a locally built TW2K2 program snapshot"]
fn tw2k2_plays_through_the_confined_program_worker() {
    let snapshot = env::var_os("TW2K2_SNAPSHOT").expect("set TW2K2_SNAPSHOT");
    let worker = WorkerProgram::from_install_owned_absolute(
        &Path::new(env!("CARGO_BIN_EXE_festerm-automation-js-worker"))
            .canonicalize()
            .expect("canonicalize program worker"),
    )
    .expect("review program worker");
    platform::program_availability().expect("program worker confinement is available");
    let storage = TestStorage::default();
    let mut runtime = ProgramSupervisor::spawn_with_launch_and_storage(
        &worker,
        program(Path::new(&snapshot)),
        19,
        TerminalSize {
            columns: 80,
            rows: 24,
        },
        Arc::new(AtomicBool::new(false)),
        Arc::new(Fence),
        Arc::new(storage.clone()),
        ProgramLaunchParams::new(ProgramLaunchMode::New),
    )
    .expect("start confined TW2K2 worker");

    let welcome = output_text(runtime.take_start_output());
    assert!(welcome.contains("Callsign:"), "{welcome:?}");
    runtime.input(b"Confined ").expect("type callsign prefix");
    assert!(output_text(runtime.input(&[0xc3]).expect("accept split UTF-8 lead")).is_empty());
    assert!(output_text(
        runtime
            .input(&[0xa9, b'\r'])
            .expect("complete split UTF-8 callsign"),
    )
    .contains("Ship"));
    assert!(
        output_text(runtime.input(b"Confined Ship\r").expect("submit ship name"))
            .contains("SECTOR")
    );
    let moved = output_text(runtime.input(b"M 2\r").expect("move to adjacent sector"));
    assert!(
        moved.contains("Sector: 2") || moved.contains("Sector 2") || moved.contains("SECTOR 2"),
        "{moved:?}"
    );
    let status = output_text(runtime.input(b"I\r").expect("open ship status"));
    assert!(
        status.contains("Confined Ship") || status.contains("Merchant Cruiser"),
        "{status:?}"
    );
    let save = output_text(runtime.input(b"SAVE\r").expect("save game"));
    assert!(save.contains("Checkpoint"), "{save:?}");
    assert!(serde_json::from_slice::<serde_json::Value>(&storage.bytes()).is_ok());
    runtime.close().expect("close program worker");
    runtime.shutdown().expect("reap program worker");
}
