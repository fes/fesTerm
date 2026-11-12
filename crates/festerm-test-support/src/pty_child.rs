use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

/// Builds the owned PTY fixture once and uses Cargo's reported executable path.
pub fn executable() -> &'static Path {
    static CHILD: LazyLock<PathBuf> = LazyLock::new(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = Command::new(cargo)
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .args([
                "build",
                "--locked",
                "--package",
                "festerm-pty-test-child",
                "--message-format=json",
            ])
            .output()
            .expect("start the owned PTY child build");
        assert!(
            output.status.success(),
            "owned PTY child build failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = artifact_path(&output.stdout);
        assert!(
            path.is_file(),
            "Cargo-reported PTY child is missing: {path:?}"
        );
        path
    });
    CHILD.as_path()
}

fn artifact_path(messages: &[u8]) -> PathBuf {
    let messages = std::str::from_utf8(messages).expect("Cargo JSON messages are UTF-8");
    let mut executable = None;
    for line in messages.lines() {
        let message: serde_json::Value =
            serde_json::from_str(line).expect("Cargo emits valid JSON messages");
        if message["reason"] == "compiler-artifact"
            && message["target"]["name"] == "festerm-pty-test-child"
            && message["target"]["kind"] == serde_json::json!(["bin"])
        {
            let path = message["executable"]
                .as_str()
                .expect("the built PTY child has an executable path");
            assert!(
                executable.is_none(),
                "Cargo reported duplicate PTY children"
            );
            executable = Some(PathBuf::from(path));
        }
    }
    executable.expect("Cargo reported the owned PTY child executable")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(path: &str) -> String {
        serde_json::json!({
            "reason": "compiler-artifact",
            "target": {"name": "festerm-pty-test-child", "kind": ["bin"]},
            "executable": path
        })
        .to_string()
    }

    #[test]
    fn child_path_uses_reported_artifact_without_assuming_cargo_layout() {
        for path in [
            "/custom-target/debug/festerm-pty-test-child",
            "/custom-build/debug/build/festerm-pty-test-child/hash/out/festerm-pty-test-child",
            r"C:\custom-target\debug\festerm-pty-test-child.exe",
        ] {
            let messages = format!(
                "{{\"reason\":\"build-script-executed\"}}\n{}\n{{\"reason\":\"build-finished\",\"success\":true}}\n",
                artifact(path)
            );
            assert_eq!(artifact_path(messages.as_bytes()), PathBuf::from(path));
        }
    }

    #[test]
    #[should_panic(expected = "Cargo reported the owned PTY child executable")]
    fn missing_child_artifact_fails_explicitly() {
        artifact_path(b"{\"reason\":\"build-finished\",\"success\":true}\n");
    }

    #[test]
    #[should_panic(expected = "Cargo reported duplicate PTY children")]
    fn duplicate_child_artifacts_fail_explicitly() {
        artifact_path(format!("{}\n{}\n", artifact("/one"), artifact("/two")).as_bytes());
    }

    #[test]
    #[should_panic(expected = "Cargo emits valid JSON messages")]
    fn invalid_cargo_metadata_fails_explicitly() {
        artifact_path(b"not JSON\n");
    }
}
