#![cfg(all(windows, feature = "relay"))]

#[test]
fn windows_setup_helper_embeds_as_invoker_and_launches_without_elevation() {
    let executable = env!("CARGO_BIN_EXE_festerm-tunnel-setup");
    let bytes = std::fs::read(executable).unwrap();
    let manifest = b"<requestedExecutionLevel level=\"asInvoker\" uiAccess=\"false\"/>";
    assert!(
        bytes.windows(manifest.len()).any(|part| part == manifest),
        "the setup filename must not trigger Windows installer-detection elevation"
    );
    let output = std::process::Command::new(executable)
        .arg("--help")
        .output()
        .expect("setup --help must launch directly without a UAC prompt");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--managed"));
    assert!(output.stderr.is_empty());
}
