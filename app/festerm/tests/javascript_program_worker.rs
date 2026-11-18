#![cfg(feature = "javascript-program-dev")]

use std::process::{Command, Stdio};

#[test]
fn javascript_program_worker_entry_refuses_extra_arguments_exactly() {
    let output = Command::new(env!("CARGO_BIN_EXE_festerm"))
        .args(["--javascript-program-worker", "unexpected"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
