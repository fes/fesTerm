#![cfg(all(windows, feature = "relay"))]

use festerm_tunnel_bridge::setup_storage;

#[path = "../src/test_support.rs"]
mod test_support;

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

#[test]
fn windows_worker_is_windowless_and_preserves_owned_setup_arguments_and_exit_status() {
    let worker = env!("CARGO_BIN_EXE_festerm-tunnel-worker");
    let bytes = std::fs::read(worker).unwrap();
    let pe = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    let subsystem = pe + 24 + 68;
    assert_eq!(
        u16::from_le_bytes(bytes[subsystem..subsystem + 2].try_into().unwrap()),
        2,
        "the Scheduler entrypoint must use the Windows GUI subsystem"
    );
    let root = test_support::private_directory("festerm-windowless-worker");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(root.clone());
    let directory = root.join("release & helpers");
    let state = root.join("state & terminal-\u{1f980}");
    std::fs::create_dir(&directory).unwrap();
    std::fs::create_dir(&state).unwrap();
    let launcher = directory.join("festerm-tunnel-worker.exe");
    std::fs::copy(worker, &launcher).unwrap();
    let setup = directory.join("festerm-tunnel-setup.exe");
    let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let source = r#"
using System;
using System.IO;
using System.Runtime.InteropServices;
public class OwnedSetupProbe {
    [DllImport("kernel32.dll")]
    public static extern IntPtr GetConsoleWindow();
    public static int Main(string[] args) {
        if (args.Length != 3 || args[0] != "--serve" || args[1] != "--state-dir")
            return 99;
        File.WriteAllText(Path.Combine(args[2], "probe.txt"),
            GetConsoleWindow().ToInt64() + "\n" + args[2]);
        return 37;
    }
}
"#;
    let compiler = std::process::Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Add-Type -TypeDefinition $env:FESTERM_PROBE_SOURCE -OutputAssembly $env:FESTERM_PROBE_OUTPUT -OutputType ConsoleApplication",
        ])
        .env("FESTERM_PROBE_SOURCE", source)
        .env("FESTERM_PROBE_OUTPUT", &setup)
        .output()
        .unwrap();
    assert!(compiler.status.success(), "{compiler:?}");
    let mut child = std::process::Command::new(&launcher)
        .arg("--state-dir")
        .arg(&state)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("windowless launcher exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(37));
    assert_eq!(
        std::fs::read_to_string(state.join("probe.txt")).unwrap(),
        format!("0\n{}", state.display())
    );
}
