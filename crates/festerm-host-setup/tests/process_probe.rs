#![cfg(feature = "native-service-fixtures")]

use festerm_host_setup::services::native::{
    CommandRunner, NativeOutput, NativeRequest, NativeRunner,
};
use festerm_sessiond::private_storage as storage;
use std::{
    fs::File,
    io,
    path::Path,
    thread::JoinHandle,
    time::{Duration, Instant},
};

struct OwnedProbe {
    root: storage::PrivateTestDirectory,
    directory: File,
    worker: Option<JoinHandle<io::Result<NativeOutput>>>,
}

impl OwnedProbe {
    fn start(mode: &str, read_only: bool) -> Self {
        let root = storage::private_test_directory();
        let directory = storage::private_directory(root.path(), false).unwrap();
        let request = NativeRequest::Process {
            tool: env!("CARGO_BIN_EXE_festerm-host-service-test-child").into(),
            args: vec![
                mode.into(),
                "--state-dir".into(),
                root.path().as_os_str().into(),
            ],
        };
        let worker = std::thread::spawn(move || {
            if read_only {
                NativeRunner.execute_read_only(&request)
            } else {
                NativeRunner.execute(&request)
            }
        });
        let owned = Self {
            root,
            directory,
            worker: Some(worker),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while storage::open_private_file(&owned.directory, Path::new("probe-ready"), false).is_err()
        {
            assert!(
                Instant::now() < deadline,
                "exact owned child did not become ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // Overflow can already have been refused/reaped by the time ready is
        // observed; the child writes ready only after acquiring this lease.
        if mode == "probe-stall" {
            assert!(
                !owned.quiescent(),
                "ready stalled child must hold its exact fixture lease"
            );
        }
        owned
    }

    fn finish(&mut self) -> io::Result<NativeOutput> {
        self.worker.take().unwrap().join().unwrap()
    }

    fn release(&self) {
        let _ = storage::write_new_private(&self.directory, Path::new("probe-release"), b"release");
    }

    fn quiescent(&self) -> bool {
        let Ok(lease) =
            storage::open_private_file(&self.directory, Path::new("probe-lease"), false)
        else {
            return false;
        };
        fs2::FileExt::try_lock_exclusive(&lease).is_ok()
    }

    fn wait_quiescent(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.quiescent() {
            assert!(
                Instant::now() < deadline,
                "owned child was not reaped after refusal/release"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for OwnedProbe {
    fn drop(&mut self) {
        // Also release the exact fixture on assertion failure. No PID guessing,
        // native manager, unrelated process or recursive cleanup is involved.
        self.release();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.quiescent() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn native_read_only_probe_timeout_reaps_after_the_caller_deadline() {
    let mut owned = OwnedProbe::start("probe-stall", true);
    assert_eq!(
        owned.finish().err().unwrap().to_string(),
        "host.service_native_timeout"
    );
    owned.wait_quiescent();
    assert!(
        !owned.root.path().join("probe-finished").exists(),
        "refused probe must be killed, not naturally completed"
    );
}

#[test]
fn native_read_only_probe_overflow_reaps_without_waiting_for_completion() {
    let mut owned = OwnedProbe::start("probe-overflow", true);
    assert_eq!(
        owned.finish().err().unwrap().to_string(),
        "host.service_native_output_limit"
    );
    owned.wait_quiescent();
    assert!(!owned.root.path().join("probe-finished").exists());
}

#[test]
fn native_manager_deadline_retains_work_until_explicit_owned_release() {
    let mut owned = OwnedProbe::start("probe-stall", false);
    assert_eq!(
        owned.finish().err().unwrap().to_string(),
        "host.service_native_timeout"
    );
    assert!(
        !owned.quiescent(),
        "manager deadline must not kill submitted work"
    );
    assert!(!owned.root.path().join("probe-finished").exists());
    owned.release();
    owned.wait_quiescent();
    assert!(owned.root.path().join("probe-finished").exists());
}
