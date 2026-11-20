#![cfg(windows)]

#[test]
fn windows_worker_is_windowless_and_preserves_owned_setup_arguments_and_exit_status() {
    let worker = env!("CARGO_BIN_EXE_festerm-tunnel-worker");
    let bytes = std::fs::read(worker).unwrap();
    let manifest = b"<requestedExecutionLevel level=\"asInvoker\" uiAccess=\"false\"/>";
    assert!(
        bytes.windows(manifest.len()).any(|part| part == manifest),
        "relocated worker must retain asInvoker manifest"
    );
    let pe = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    let subsystem = pe + 24 + 68;
    assert_eq!(
        u16::from_le_bytes(bytes[subsystem..subsystem + 2].try_into().unwrap()),
        2,
        "the Scheduler entrypoint must use the Windows GUI subsystem"
    );
    let private = festerm_sessiond::private_storage::private_test_directory();
    let root = private.path();
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
    let managed_state = root.join("managed Desktop state");
    let store = festerm_host_setup::lifecycle::Store::create(
        &managed_state,
        festerm_host_setup::lifecycle::Origin::Desktop,
        festerm_host_setup::lifecycle::Settings {
            filesystem: None,
            friendly_name: "windowless refusal fixture".into(),
            allow_creation: false,
            startup: festerm_host_setup::lifecycle::Startup::Manual,
            ordinary_access: Default::default(),
        },
    )
    .unwrap();
    let before = store.snapshot().unwrap();
    let output = std::process::Command::new(&launcher)
        .args(["run-desktop-managed", "--state-dir"])
        .arg(&managed_state)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert!(
        !managed_state.join("probe.txt").exists(),
        "managed route must not spawn the sibling legacy setup"
    );
    assert!(
        !managed_state.join("host-managed-result.json").exists(),
        "unregistered refusal is not managed runtime completion"
    );
    assert_eq!(store.snapshot().unwrap(), before);
}
