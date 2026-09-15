//! Screen socket timestamps/inodes can change without restarting the shell.
//! A bounded, batched ps query supplies process-generation identity instead.

use std::collections::BTreeMap;

#[cfg(unix)]
pub(super) fn client_is_attached(
    server: u32,
    socket: &std::path::Path,
    client: u32,
    device: &std::path::Path,
) -> Result<bool, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let terminal = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOCTTY | nix::libc::O_NONBLOCK)
        .open(device)
    {
        Ok(terminal) => terminal,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Could not open owned screen client terminal: {error}"
            ))
        }
    };
    // stdin selects an exact display. TERM expands before Screen's fallback to
    // another display; %+p expands afterward. Require both context and PID.
    // @ keeps successful echo replies out of other displays.
    let mut namespace = QueryNamespace::new(socket, server)?;
    let mut command = super::provider_command(
        "screen",
        &["-S", &namespace.target, "-Q", "@echo", "-p", "${TERM}|%+p"],
    );
    command.env("SCREENDIR", &namespace.root).env_remove("STY");
    let result =
        crate::local_command::output_with_stdin(command, super::COMMAND_TIMEOUT, terminal.into());
    let cleanup = namespace.cleanup();
    let output = match (result, cleanup) {
        (Ok(output), Ok(())) => output.ok_or("screen is no longer available")?,
        (Err(error), Ok(())) => return Err(format!(
            "Screen attachment query failed: {error}. If Screen requires authentication, attach with Screen directly."
        )),
        (Ok(_), Err(error)) => return Err(error),
        (Err(error), Err(cleanup)) => return Err(format!("{error}; {cleanup}")),
    };
    confirm_query(&output, client, || {
        legacy_server_has_terminal(server, device)
    })
}

/// Screen -Q leaves a reply socket when killed while waiting for an uninitialized
/// display. Isolate those sockets instead of accumulating files in user inventory.
#[cfg(unix)]
struct QueryNamespace {
    root: std::path::PathBuf,
    target: String,
    cleaned: bool,
}

#[cfg(unix)]
impl QueryNamespace {
    fn new(socket: &std::path::Path, server: u32) -> Result<Self, String> {
        use std::{
            os::unix::fs::DirBuilderExt,
            sync::atomic::{AtomicU64, Ordering},
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let socket = if socket.is_absolute() {
            socket.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|error| error.to_string())?
                .join(socket)
        };
        for _ in 0..128 {
            // macOS's default TMPDIR may exceed the Unix socket pathname limit.
            let root = std::path::PathBuf::from(format!(
                "/tmp/fsq-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => {
                    let namespace = Self {
                        root,
                        target: format!("{server}.q"),
                        cleaned: false,
                    };
                    std::os::unix::fs::symlink(&socket, namespace.root.join(&namespace.target))
                        .map_err(|error| {
                            format!("Could not reference selected Screen socket: {error}")
                        })?;
                    return Ok(namespace);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "Could not create private Screen query directory: {error}"
                    ))
                }
            }
        }
        Err("Could not allocate private Screen query directory after 128 collisions".into())
    }

    fn cleanup(&mut self) -> Result<(), String> {
        use std::os::unix::fs::FileTypeExt;
        if self.cleaned {
            return Ok(());
        }
        let root_type = std::fs::symlink_metadata(&self.root)
            .map_err(|error| error.to_string())?
            .file_type();
        if !root_type.is_dir() || root_type.is_symlink() {
            return Err(format!(
                "Refusing redirected Screen query directory {}",
                self.root.display()
            ));
        }
        let entries = std::fs::read_dir(&self.root)
            .map_err(|error| format!("Could not inspect owned Screen query directory: {error}"))?;
        for (index, entry) in entries.enumerate() {
            if index >= 64 {
                return Err(format!(
                    "Too many artifacts in owned Screen query {}",
                    self.root.display()
                ));
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if !kind.is_socket()
                && !(kind.is_symlink() && entry.file_name() == self.target.as_str())
            {
                return Err(format!(
                    "Unexpected artifact in owned Screen query {}",
                    self.root.display()
                ));
            }
            let path = entry.path();
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "Could not clean owned Screen query {}: {error}",
                        path.display()
                    ))
                }
            }
        }
        std::fs::remove_dir(&self.root).map_err(|error| {
            format!(
                "Could not clean owned Screen query {}: {error}",
                self.root.display()
            )
        })?;
        self.cleaned = true;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for QueryNamespace {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("{error}");
        }
    }
}

#[cfg(unix)]
fn confirm_query(
    output: &std::process::Output,
    client: u32,
    legacy_inspection: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success()
        && stderr.is_empty()
        && stdout.trim_end().ends_with("Error: Unknown option -Q")
    {
        return legacy_inspection();
    }
    if !output.status.success() || !stderr.is_empty() {
        return Err(format!(
            "Could not confirm screen client: {} {}",
            stdout.trim(),
            stderr.trim()
        ));
    }
    let (terminal, pid) = stdout
        .trim()
        .split_once('|')
        .ok_or("Unrecognized Screen client query response; attachment not confirmed")?;
    let pid = pid
        .parse::<u32>()
        .map_err(|_| "Invalid Screen frontend PID; attachment not confirmed")?;
    // This is the TERM set by LocalPtySession, not the queried window's TERM.
    Ok(terminal == "xterm-256color" && pid == client)
}

/// Apple Screen 4.00 lacks -Q. Never use this compatibility path after a
/// supported query fails, or interpret denied inspection as attachment success.
#[cfg(unix)]
fn legacy_server_has_terminal(pid: u32, device: &std::path::Path) -> Result<bool, String> {
    #[cfg(test)]
    if std::env::var_os("FESTERM_TEST_SCREEN_INSPECTION_DENIED").is_some() {
        return Err("Screen process inspection denied by validation fixture".into());
    }
    let device = device
        .to_str()
        .ok_or("Screen client terminal path is not UTF-8")?;
    let output = super::bounded_output("lsof", &["-nP", "-p", &pid.to_string(), "-Fpn"])?
        .ok_or("lsof is required to confirm this screen client's terminal attachment")?;
    if !output.stderr.is_empty() || (!output.status.success() && output.status.code() != Some(1)) {
        return Err(format!(
            "Could not confirm screen client terminal: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(terminal_is_open_by(
        &String::from_utf8_lossy(&output.stdout),
        pid,
        device,
    ))
}

#[cfg(any(test, unix))]
fn terminal_is_open_by(output: &str, pid: u32, device: &str) -> bool {
    let mut owner = None;
    output.lines().any(|line| {
        if let Some(value) = line.strip_prefix('p') {
            owner = value.parse::<u32>().ok();
        }
        owner == Some(pid) && line.strip_prefix('n') == Some(device)
    })
}

#[cfg(unix)]
pub(super) fn add_identity(sessions: &mut Vec<super::MultiplexerSession>) -> Result<(), String> {
    if sessions.is_empty() {
        return Ok(());
    }
    if sessions.len() > 4096 {
        return Err("screen inventory exceeds 4096 sessions".into());
    }
    let pids = sessions
        .iter()
        .filter_map(|session| session.match_key.split_once('.'))
        .filter(|(pid, _)| pid.parse::<u32>().is_ok())
        .map(|(pid, _)| pid)
        .collect::<Vec<_>>()
        .join(",");
    if pids.is_empty() {
        sessions.clear();
        return Ok(());
    }
    let output = super::bounded_output("ps", &["-p", &pids, "-o", "pid=,lstart="])?
        .ok_or("ps is required to validate GNU screen process identities")?;
    // ps returns 1 if all selected processes exited during discovery.
    if !output.stderr.is_empty() || (!output.status.success() && output.status.code() != Some(1)) {
        return Err(format!(
            "could not inspect screen processes: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let starts = parse_starts(&String::from_utf8_lossy(&output.stdout))?;
    sessions.retain_mut(|session| {
        let pid = session
            .match_key
            .split_once('.')
            .and_then(|(pid, _)| pid.parse::<u32>().ok());
        let Some(start) = pid.and_then(|pid| starts.get(&pid)).copied() else {
            return false;
        };
        session.match_key = format!("{}|{start}", session.match_key);
        session.started_at_unix_seconds = Some(start);
        true
    });
    Ok(())
}

fn parse_starts(output: &str) -> Result<BTreeMap<u32, u64>, String> {
    let mut starts = BTreeMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let (pid, start) = parse_start(line)
            .ok_or("unrecognized ps process start time; cannot safely identify screen sessions")?;
        starts.insert(pid, start);
    }
    Ok(starts)
}

fn parse_start(line: &str) -> Option<(u32, u64)> {
    // bounded_output fixes LC_ALL=C and TZ=UTC0 on both BSD and Linux.
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() != 6 {
        return None;
    }
    let pid = fields[0].parse::<u32>().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|month| *month == fields[2])?;
    let day = fields[3].parse::<u64>().ok()?;
    let year = fields[5].parse::<u64>().ok()?;
    if !(1970..=9999).contains(&year) {
        return None;
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day == 0 || day > months[month] {
        return None;
    }
    let time: Vec<u64> = fields[4]
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if time.len() != 3 || time[0] > 23 || time[1] > 59 || time[2] > 59 {
        return None;
    }
    let days = 365 * (year - 1970) + (year - 1) / 4 - 1969 / 4 - ((year - 1) / 100 - 1969 / 100)
        + (year - 1) / 400
        - 1969 / 400
        + months[..month].iter().sum::<u64>()
        + day
        - 1;
    Some((pid, days * 86400 + time[0] * 3600 + time[1] * 60 + time[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn timed_out_screen_query_cleans_only_its_private_reply_sockets() {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        let other = QueryNamespace::new(std::path::Path::new("/unused-test-socket"), 42).unwrap();
        let original = other.root.join("original-query");
        let _server = UnixListener::bind(&original).unwrap();
        let mut query = QueryNamespace::new(&original, 42).unwrap();
        let root = query.root.clone();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _reply = UnixListener::bind(root.join("42.q-queryA")).unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 5"]);
        assert!(
            crate::local_command::output(command, std::time::Duration::from_millis(30))
                .unwrap_err()
                .contains("timed out")
        );
        let held = other.root.join("held-query");
        std::fs::rename(&root, &held).unwrap();
        std::os::unix::fs::symlink(&other.root, &root).unwrap();
        assert!(query.cleanup().unwrap_err().contains("redirected"));
        assert!(original.exists());
        std::fs::remove_file(&root).unwrap();
        std::fs::rename(held, &root).unwrap();
        query.cleanup().unwrap();
        assert!(!root.exists());
        assert!(
            original.exists(),
            "query cleanup must not follow the server symlink"
        );
        assert!(other.root.exists());
    }

    #[cfg(unix)]
    fn query_output(code: i32, stdout: &str, stderr: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn screen_query_confirms_exact_client_without_process_inspection() {
        for (text, expected) in [
            ("xterm-256color|42", true),
            ("xterm-256color|43", false),
            ("unknown|42", false),
            ("unknown|43", false),
        ] {
            let inspected = std::cell::Cell::new(false);
            let result = confirm_query(&query_output(0, text, ""), 42, || {
                inspected.set(true);
                Err("Permission denied: nondumpable Screen server".into())
            });
            assert_eq!(result.unwrap(), expected, "{text}");
            assert!(
                !inspected.get(),
                "supported queries never inspect server descriptors"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn screen_query_errors_never_fall_back_to_another_clients_attachment() {
        for output in [
            query_output(1, "", "Permission denied"),
            query_output(0, "unexpected", ""),
            query_output(0, "xterm-256color|not-a-pid", ""),
            query_output(0, "xterm-256color|42", "query failed"),
        ] {
            assert!(confirm_query(&output, 42, || {
                panic!("a failed/malformed query must not fall back to inspection")
            })
            .is_err());
        }
        let old = query_output(1, "Usage...\nError: Unknown option -Q\n", "");
        assert!(confirm_query(&old, 42, || Ok(true)).unwrap());
        assert!(confirm_query(&old, 42, || Err("Permission denied".into())).is_err());
    }

    #[test]
    fn screen_confirmation_requires_the_selected_server_and_exact_client_terminal() {
        let output = "p42\nn/dev/ttys001\nn/dev/ttys002\np43\nn/dev/ttys003\n";
        assert!(terminal_is_open_by(output, 42, "/dev/ttys002"));
        assert!(!terminal_is_open_by(output, 42, "/dev/ttys003"));
        assert!(!terminal_is_open_by(output, 42, "/dev/ttys00"));
        assert!(!terminal_is_open_by("", 42, "/dev/ttys002"));
    }

    #[test]
    fn screen_process_start_is_stable_utc_generation_metadata() {
        assert_eq!(parse_start("42 Thu Jan 1 00:00:00 1970"), Some((42, 0)));
        assert_eq!(
            parse_start("42 Sat Jan 1 00:00:00 2000"),
            Some((42, 946684800))
        );
        assert_eq!(
            parse_start("42 Tue Feb 29 12:34:56 2000"),
            Some((42, 951827696))
        );
        assert!(parse_start("42 Mon Feb 29 00:00:00 2100").is_none());
        assert!(parse_starts("unrecognized date").is_err());
        assert!(parse_starts("").unwrap().is_empty());
    }
}
