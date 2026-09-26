use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn child_acknowledgment_requires_completed_controlled_input() {
    for (input, accepted) in [
        (b"os-input-ok\n".as_slice(), true),
        (b"\t\x1b[Aos-input-ok\r\n", true),
        (b"", false),
        (b"\n", false),
        (b"wrong-input\n", false),
        (b"os-input-ok-extra\n", false),
        (b"os-input-ok", false),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_festerm-pty-test-child"))
            .args([
                "read-line",
                "expect-line-suffix:os-input-ok",
                "emit:OS-INPUT-ACK",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.success(),
            accepted,
            "input {input:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            if accepted {
                b"OS-INPUT-ACK\n"
            } else {
                b"".as_slice()
            }
        );
    }
}
