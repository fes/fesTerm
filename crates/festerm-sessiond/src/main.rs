use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use festerm_pty::default_local_profile;
use festerm_ssh::PersistentSessionName;
use fs2::FileExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x00000008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SessionRecord {
    name: String,
    pid: u32,
    socket: String,
    shell: String,
    cols: u16,
    rows: u16,
    created_at_unix_ms: u128,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SessionRegistry {
    #[serde(default)]
    sessions: BTreeMap<String, SessionRecord>,
}

#[derive(Clone, Debug)]
enum CommandSpec {
    Start {
        name: String,
        shell: String,
        cols: u16,
        rows: u16,
    },
    Daemon {
        name: String,
        shell: String,
        cols: u16,
        rows: u16,
    },
    List,
    Kill {
        name: String,
    },
    Attach {
        name: String,
    },
}

struct SpawnedShell {
    child: Box<dyn portable_pty::Child + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("festerm-sessiond: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let spec = parse_args(env::args().skip(1).collect())?;
    match spec {
        CommandSpec::Start {
            name,
            shell,
            cols,
            rows,
        } => run_start(name, shell, cols, rows),
        CommandSpec::Daemon {
            name,
            shell,
            cols,
            rows,
        } => run_daemon(name, shell, cols, rows),
        CommandSpec::List => run_list(),
        CommandSpec::Kill { name } => run_kill(name),
        CommandSpec::Attach { name } => run_attach(name),
    }
}

fn parse_args(args: Vec<String>) -> Result<CommandSpec, Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("usage: festerm-sessiond <start|daemon|list|kill|attach> ...".into());
    }

    match args[0].as_str() {
        "start" => parse_start(&args[1..]),
        "daemon" => parse_daemon(&args[1..]),
        "list" => {
            if !args[1..].is_empty() {
                return Err("list takes no additional arguments".into());
            }
            Ok(CommandSpec::List)
        }
        "kill" => {
            let name = take_value(&args[1..], "kill", "--name")?;
            Ok(CommandSpec::Kill { name })
        }
        "attach" => {
            let name = take_value(&args[1..], "attach", "--name")?;
            Ok(CommandSpec::Attach { name })
        }
        other => Err(format!("unknown command: {other}").into()),
    }
}

fn parse_start(args: &[String]) -> Result<CommandSpec, Box<dyn std::error::Error>> {
    let name = take_value(args, "start", "--name")?;
    let shell = take_value(args, "start", "--shell").unwrap_or_else(|_| default_shell());
    let cols = take_u16(args, "--cols").unwrap_or(80);
    let rows = take_u16(args, "--rows").unwrap_or(24);
    Ok(CommandSpec::Start {
        name,
        shell,
        cols,
        rows,
    })
}

fn parse_daemon(args: &[String]) -> Result<CommandSpec, Box<dyn std::error::Error>> {
    let name = take_value(args, "daemon", "--name")?;
    let shell = take_value(args, "daemon", "--shell").unwrap_or_else(|_| default_shell());
    let cols = take_u16(args, "--cols").unwrap_or(80);
    let rows = take_u16(args, "--rows").unwrap_or(24);
    Ok(CommandSpec::Daemon {
        name,
        shell,
        cols,
        rows,
    })
}

fn take_value(
    args: &[String],
    command: &str,
    flag: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == flag {
            if index + 1 >= args.len() {
                return Err(format!("{command} requires a value for {flag}").into());
            }
            return Ok(args[index + 1].clone());
        }
        index += 1;
    }
    Err(format!("{command} requires {flag} <value>").into())
}

fn take_u16(args: &[String], flag: &str) -> Option<u16> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == flag {
            if index + 1 >= args.len() {
                return None;
            }
            return args[index + 1].parse().ok();
        }
        index += 1;
    }
    None
}

fn default_shell() -> String {
    default_local_profile()
        .map(|profile| profile.executable().to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            #[cfg(unix)]
            {
                "/bin/sh".to_owned()
            }
            #[cfg(windows)]
            {
                "cmd.exe".to_owned()
            }
        })
}

fn run_start(
    name: String,
    shell: String,
    cols: u16,
    rows: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = validate_name(name)?;
    let registry = load_registry()?;
    if registry.sessions.contains_key(&name) {
        let record = registry.sessions.get(&name).unwrap();
        if process_alive(record.pid) {
            return Err(format!("session '{name}' is already running").into());
        }
    }

    let exe = env::current_exe()?;

    #[cfg(unix)]
    {
        let mut command = Command::new("setsid");
        command
            .arg(&exe)
            .arg("daemon")
            .arg("--name")
            .arg(&name)
            .arg("--shell")
            .arg(&shell)
            .arg("--cols")
            .arg(cols.to_string())
            .arg("--rows")
            .arg(rows.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.spawn()?;
    }

    #[cfg(windows)]
    {
        let mut command = Command::new(&exe);
        command
            .arg("daemon")
            .arg("--name")
            .arg(&name)
            .arg("--shell")
            .arg(&shell)
            .arg("--cols")
            .arg(cols.to_string())
            .arg("--rows")
            .arg(rows.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
        command.spawn()?;
    }

    thread::sleep(Duration::from_millis(250));
    let registry = load_registry()?;
    let record = registry
        .sessions
        .get(&name)
        .ok_or_else(|| format!("failed to register session '{name}'"))?;

    println!(
        "started {} pid={} socket={} shell={}",
        name, record.pid, record.socket, record.shell
    );
    Ok(())
}

fn run_daemon(
    name: String,
    shell: String,
    cols: u16,
    rows: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = validate_name(name)?;
    let runtime_root = runtime_root()?;
    fs::create_dir_all(&runtime_root)?;
    set_dir_mode(&runtime_root, 0o700);

    #[cfg(unix)]
    {
        let socket_path = session_socket_path(&runtime_root, &name)?;
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }
        let listener = UnixListener::bind(&socket_path)?;
        set_file_mode(&socket_path, 0o600);

        let record = SessionRecord {
            name: name.clone(),
            pid: process::id(),
            socket: socket_path.to_string_lossy().into_owned(),
            shell: shell.clone(),
            cols,
            rows,
            created_at_unix_ms: now_ms(),
        };
        save_registry_record(record)?;

        let mut spawned = spawn_shell(&shell, cols, rows)?;
        let mut reader = spawned.master.try_clone_reader()?;
        let (mut stream, _) = listener.accept()?;
        io_loop(&mut reader, &mut stream, &mut spawned, &name)?;
    }

    #[cfg(windows)]
    {
        let pipe_name = session_pipe_name(&name);
        let record = SessionRecord {
            name: name.clone(),
            pid: process::id(),
            socket: pipe_name.clone(),
            shell: shell.clone(),
            cols,
            rows,
            created_at_unix_ms: now_ms(),
        };
        save_registry_record(record)?;

        let mut spawned = spawn_shell(&shell, cols, rows)?;
        let mut reader = spawned.master.try_clone_reader()?;
        let mut server = named_pipe::PipeOptions::new(&pipe_name).single()?.wait()?;
        io_loop(&mut reader, &mut server, &mut spawned, &name)?;
    }

    Ok(())
}

fn io_loop<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    spawned: &mut SpawnedShell,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                writer.write_all(&buffer[..count])?;
                writer.flush()?;
            }
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }

    let _ = spawned.child.wait();
    drop_registry_record(name)?;
    Ok(())
}

fn run_list() -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = load_registry()?;
    prune_dead_records(&mut registry)?;
    save_registry(&registry)?;

    if registry.sessions.is_empty() {
        println!("no live sessions");
        return Ok(());
    }

    println!("name\tpid\tsocket\tshell");
    for record in registry.sessions.values() {
        println!(
            "{}\t{}\t{}\t{}",
            record.name, record.pid, record.socket, record.shell
        );
    }
    Ok(())
}

fn run_kill(name: String) -> Result<(), Box<dyn std::error::Error>> {
    let name = validate_name(name)?;
    with_registry_lock(|registry: &mut SessionRegistry| {
        let Some(record) = registry.sessions.remove(&name) else {
            return Err(format!("session '{name}' is not registered").into());
        };
        terminate_pid(record.pid);
        Ok(())
    })
}

fn run_attach(name: String) -> Result<(), Box<dyn std::error::Error>> {
    let name = validate_name(name)?;
    let registry = load_registry()?;
    let record = registry
        .sessions
        .get(&name)
        .ok_or_else(|| format!("session '{name}' is not registered"))?;

    #[cfg(unix)]
    {
        let mut stream = UnixStream::connect(&record.socket)?;
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    io::stdout().write_all(&buffer[..count])?;
                    io::stdout().flush()?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                Err(error) => return Err(error.into()),
            }
        }
    }

    #[cfg(windows)]
    {
        let mut stream = named_pipe::PipeClient::connect(&record.socket)?;
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    io::stdout().write_all(&buffer[..count])?;
                    io::stdout().flush()?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                Err(error) => return Err(error.into()),
            }
        }
    }

    Ok(())
}

fn validate_name(name: String) -> Result<String, Box<dyn std::error::Error>> {
    let session_name = PersistentSessionName::new(name)?;
    Ok(session_name.as_str().to_owned())
}

fn runtime_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        if let Some(root) = env::var_os("XDG_STATE_HOME") {
            Ok(PathBuf::from(root).join("festerm").join("sessiond"))
        } else if let Some(home) = env::var_os("HOME") {
            Ok(PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("festerm")
                .join("sessiond"))
        } else {
            Ok(PathBuf::from("/tmp").join("festerm-sessiond"))
        }
    }

    #[cfg(windows)]
    {
        if let Some(root) = env::var_os("LOCALAPPDATA") {
            Ok(PathBuf::from(root).join("fesTerm").join("sessiond"))
        } else if let Some(root) = env::var_os("USERPROFILE") {
            Ok(PathBuf::from(root)
                .join("AppData")
                .join("Local")
                .join("fesTerm")
                .join("sessiond"))
        } else {
            Ok(PathBuf::from("C:\\Temp\\festerm-sessiond"))
        }
    }
}

fn session_socket_path(
    runtime_root: &Path,
    name: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(runtime_root.join(format!("{name}.sock")))
}

#[cfg(windows)]
fn session_pipe_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|character| match character {
            ch if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') => ch,
            _ => '_',
        })
        .collect();
    format!(r"\\.\pipe\festerm-sessiond-{sanitized}")
}

fn registry_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(runtime_root()?.join("registry.json"))
}

fn read_registry_at(path: &Path) -> Result<SessionRegistry, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(SessionRegistry::default());
    }
    let bytes = fs::read(path)?;
    if bytes.is_empty() {
        return Ok(SessionRegistry::default());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn with_registry_lock<T>(
    mut operation: impl FnMut(&mut SessionRegistry) -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    let path = registry_path()?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)?;
    file.lock_exclusive()?;
    let mut registry = read_registry_at(&path)?;
    let result = operation(&mut registry);
    if result.is_ok() {
        write_registry_at(&path, &registry)?;
    }
    file.unlock()?;
    result
}

fn write_registry_at(
    path: &Path,
    registry: &SessionRegistry,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = serde_json::to_string_pretty(registry)?;
    fs::write(path, output)?;
    set_file_mode(path, 0o600);
    Ok(())
}

fn load_registry() -> Result<SessionRegistry, Box<dyn std::error::Error>> {
    let path = registry_path()?;
    let _file = OpenOptions::new().read(true).open(&path).ok();
    let _ = _file;
    read_registry_at(&path)
}

fn save_registry(registry: &SessionRegistry) -> Result<(), Box<dyn std::error::Error>> {
    let path = registry_path()?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    write_registry_at(&path, registry)
}

fn save_registry_record(record: SessionRecord) -> Result<(), Box<dyn std::error::Error>> {
    with_registry_lock(|registry: &mut SessionRegistry| {
        registry
            .sessions
            .insert(record.name.clone(), record.clone());
        Ok(())
    })
}

fn drop_registry_record(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    with_registry_lock(|registry: &mut SessionRegistry| {
        registry.sessions.remove(name);
        Ok(())
    })
}

fn prune_dead_records(registry: &mut SessionRegistry) -> Result<(), Box<dyn std::error::Error>> {
    let names: Vec<String> = registry.sessions.keys().cloned().collect();
    for name in names {
        if let Some(record) = registry.sessions.get(&name) {
            if !process_alive(record.pid) {
                registry.sessions.remove(&name);
            }
        }
    }
    Ok(())
}

fn set_dir_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(mode);
        let _ = fs::set_permissions(path, permissions);
    }
}

fn set_file_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(mode);
        let _ = fs::set_permissions(path, permissions);
    }
}

fn spawn_shell(
    shell: &str,
    cols: u16,
    rows: u16,
) -> Result<SpawnedShell, Box<dyn std::error::Error>> {
    let system = native_pty_system();
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = system.openpty(size)?;
    let mut command = CommandBuilder::new(shell);
    command.cwd(env::current_dir()?);
    command.env("TERM", "xterm-256color");
    let child = pair.slave.spawn_command(command)?;
    let master = pair.master;
    Ok(SpawnedShell { child, master })
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid="])
            .output();
        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                !stdout.trim().is_empty()
            }
            Err(_) => false,
        }
    }

    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .arg("/FO")
            .arg("CSV")
            .arg("/NH")
            .output();
        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.contains(&format!("\"{pid}\""))
            }
            Err(_) => false,
        }
    }
}

fn terminate_pid(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-- -{pid}")])
            .status();
        thread::sleep(Duration::from_millis(250));
        if process_alive(pid) {
            let _ = Command::new("kill")
                .args(["-KILL", &format!("-- -{pid}")])
                .status();
        }
    }

    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_are_accepted() {
        assert_eq!(validate_name("demo-01".to_owned()).unwrap(), "demo-01");
        assert_eq!(validate_name("session_2".to_owned()).unwrap(), "session_2");
    }

    #[test]
    fn invalid_names_are_rejected() {
        let err = validate_name("bad/name".to_owned()).unwrap_err();
        assert!(err.to_string().contains("persistent session name"));
    }

    #[test]
    fn registry_round_trip_is_stable() {
        let record = SessionRecord {
            name: "demo".to_owned(),
            pid: 1234,
            socket: "/tmp/festerm-sessiond/demo.sock".to_owned(),
            shell: "/bin/bash".to_owned(),
            cols: 80,
            rows: 24,
            created_at_unix_ms: 1_700_000_000_000,
        };
        let registry = SessionRegistry {
            sessions: BTreeMap::from([(record.name.clone(), record.clone())]),
        };
        let serialized = serde_json::to_string(&registry).unwrap();
        let parsed: SessionRegistry = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.sessions.get("demo"), Some(&record));
    }
}
