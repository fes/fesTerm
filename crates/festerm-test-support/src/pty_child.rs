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
        stable_copy(&path)
    });
    CHILD.as_path()
}

/// Copies the child out of Cargo's output directory.
///
/// Cargo replaces the uplifted executable on every build, even a fresh one.
/// When test processes run in parallel (for example under Nextest), another
/// process's build can briefly remove the path this process is about to
/// spawn. A content-addressed copy is never replaced once published.
fn stable_copy(source: &Path) -> PathBuf {
    let mut last_error = None;
    for _ in 0..200 {
        match try_stable_copy(source) {
            Ok(path) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => panic!("copy the PTY child {source:?}: {error}"),
        }
    }
    panic!("Cargo-reported PTY child is missing: {source:?}: {last_error:?}")
}

fn try_stable_copy(source: &Path) -> std::io::Result<PathBuf> {
    let bytes = std::fs::read(source)?;
    let file_name = source.file_name().expect("the PTY child path names a file");
    let directory = source
        .parent()
        .expect("the PTY child has a parent directory")
        .join("festerm-pty-test-child-stable")
        .join(format!("{:016x}", fnv1a(&bytes)));
    let destination = directory.join(file_name);
    if destination.is_file() {
        return Ok(destination);
    }
    std::fs::create_dir_all(&directory)?;
    let staging = directory.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&staging, &bytes)?;
    std::fs::set_permissions(&staging, std::fs::metadata(source)?.permissions())?;
    if let Err(error) = std::fs::rename(&staging, &destination) {
        let _ = std::fs::remove_file(&staging);
        if !destination.is_file() {
            return Err(error);
        }
    }
    Ok(destination)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
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
    fn stable_copy_survives_cargo_replacing_the_uplifted_child() {
        let root =
            std::env::temp_dir().join(format!("festerm-pty-child-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("festerm-pty-test-child");
        std::fs::write(&source, b"child v1").unwrap();

        let first = stable_copy(&source);
        assert_eq!(stable_copy(&source), first);
        std::fs::remove_file(&source).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"child v1");

        std::fs::write(&source, b"child v2").unwrap();
        let second = stable_copy(&source);
        assert_ne!(second, first);
        assert_eq!(std::fs::read(&second).unwrap(), b"child v2");
        std::fs::remove_dir_all(&root).unwrap();
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
