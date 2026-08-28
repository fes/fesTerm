use std::{
    collections::{BTreeMap, VecDeque},
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
};

use festerm_pty::{default_local_profile, EnvironmentPolicy};
use festerm_ssh::PersistentSessionName;
use fs2::FileExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

const CLIENT_POLL_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(windows)]
const WINDOWS_CLIENT_READ_TIMEOUT: Duration = Duration::from_millis(250);
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const CLIENT_QUEUE_CAPACITY: usize = 64;
const CLIENT_FRAME_HEADER_BYTES: usize = 9;
const CLIENT_FRAME_MAGIC: &[u8; 4] = b"FSD1";
const CLIENT_FRAME_INPUT: u8 = 1;
const CLIENT_FRAME_RESIZE: u8 = 2;
const MAX_CLIENT_FRAME_BYTES: usize = 64 * 1024;
const REPLAY_CAPACITY_BYTES: usize = 1024 * 1024;
const REPLAY_RESET_BYTES: &[u8] = b"\x1bc\x1b[3J";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SessionRecord {
    name: String,
    pid: u32,
    socket: String,
    shell: String,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default)]
    working_directory: Option<String>,
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
        shell: ShellSpec,
        cols: u16,
        rows: u16,
    },
    Daemon {
        name: String,
        shell: ShellSpec,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShellSpec {
    executable: String,
    arguments: Vec<String>,
    working_directory: Option<String>,
    environment: EnvironmentSpec,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum EnvironmentSpec {
    #[default]
    Inherit,
    Clear(BTreeMap<String, String>),
    InheritWith(BTreeMap<String, String>),
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
            let name = parse_name_only(&args[1..], "kill")?;
            Ok(CommandSpec::Kill { name })
        }
        "attach" => {
            let name = parse_name_only(&args[1..], "attach")?;
            Ok(CommandSpec::Attach { name })
        }
        other => Err(format!("unknown command: {other}").into()),
    }
}

fn parse_start(args: &[String]) -> Result<CommandSpec, Box<dyn std::error::Error>> {
    let (name, shell, cols, rows) = parse_session_options(args, "start")?;
    Ok(CommandSpec::Start {
        name,
        shell,
        cols,
        rows,
    })
}

fn parse_daemon(args: &[String]) -> Result<CommandSpec, Box<dyn std::error::Error>> {
    let (name, shell, cols, rows) = parse_session_options(args, "daemon")?;
    Ok(CommandSpec::Daemon {
        name,
        shell,
        cols,
        rows,
    })
}

fn parse_name_only(args: &[String], command: &str) -> Result<String, Box<dyn std::error::Error>> {
    if let [flag, name] = args {
        if flag == "--name" {
            return Ok(name.clone());
        }
    }
    Err(format!("usage: festerm-sessiond {command} --name <id>").into())
}

fn parse_session_options(
    args: &[String],
    command: &str,
) -> Result<(String, ShellSpec, u16, u16), Box<dyn std::error::Error>> {
    let mut name = None;
    let mut shell = None;
    let mut shell_arguments = Vec::new();
    let mut working_directory = None;
    let mut environment_policy = None;
    let mut environment = BTreeMap::new();
    let mut cols = None;
    let mut rows = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        if flag == "--env" {
            let key = args
                .get(index + 1)
                .ok_or_else(|| format!("{command} requires a key and value for --env"))?;
            let value = args
                .get(index + 2)
                .ok_or_else(|| format!("{command} requires a key and value for --env"))?;
            if environment.insert(key.clone(), value.clone()).is_some() {
                return Err(
                    format!("{command} received environment key {key} more than once").into(),
                );
            }
            index += 3;
            continue;
        }
        let Some(value) = args.get(index + 1) else {
            return Err(format!("{command} requires a value for {flag}").into());
        };
        match flag {
            "--name" => set_once(&mut name, value.clone(), command, flag)?,
            "--shell" => set_once(&mut shell, value.clone(), command, flag)?,
            "--arg" => shell_arguments.push(value.clone()),
            "--cwd" => set_once(&mut working_directory, value.clone(), command, flag)?,
            "--env-policy" => {
                let policy = match value.as_str() {
                    "clear" => EnvironmentSpec::Clear(BTreeMap::new()),
                    "inherit-with" => EnvironmentSpec::InheritWith(BTreeMap::new()),
                    _ => {
                        return Err(format!(
                            "{command} requires clear or inherit-with for --env-policy"
                        )
                        .into())
                    }
                };
                set_once(&mut environment_policy, policy, command, flag)?;
            }
            "--cols" => {
                let value = value
                    .parse::<u16>()
                    .map_err(|_| format!("{command} requires a valid u16 for {flag}"))?;
                set_once(&mut cols, value, command, flag)?;
            }
            "--rows" => {
                let value = value
                    .parse::<u16>()
                    .map_err(|_| format!("{command} requires a valid u16 for {flag}"))?;
                set_once(&mut rows, value, command, flag)?;
            }
            other => return Err(format!("{command} does not recognize {other}").into()),
        }
        index += 2;
    }
    let environment = match environment_policy {
        Some(EnvironmentSpec::Clear(_)) => EnvironmentSpec::Clear(environment),
        Some(EnvironmentSpec::InheritWith(_)) => EnvironmentSpec::InheritWith(environment),
        Some(EnvironmentSpec::Inherit) => unreachable!("inherit is not accepted by the parser"),
        None if environment.is_empty() => EnvironmentSpec::Inherit,
        None => return Err(format!("{command} requires --env-policy with --env").into()),
    };
    Ok((
        name.ok_or_else(|| format!("{command} requires --name <value>"))?,
        match shell {
            Some(executable) => ShellSpec {
                executable,
                arguments: shell_arguments,
                working_directory,
                environment,
            },
            None => {
                if !shell_arguments.is_empty()
                    || working_directory.is_some()
                    || environment != EnvironmentSpec::Inherit
                {
                    return Err(format!(
                        "{command} requires --shell with --arg, --cwd, or environment options"
                    )
                    .into());
                }
                default_shell()
            }
        },
        cols.unwrap_or(80),
        rows.unwrap_or(24),
    ))
}

fn set_once<T>(
    target: &mut Option<T>,
    value: T,
    command: &str,
    flag: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if target.replace(value).is_some() {
        return Err(format!("{command} received {flag} more than once").into());
    }
    Ok(())
}

fn default_shell() -> ShellSpec {
    default_local_profile()
        .map(|profile| ShellSpec {
            executable: profile.executable().to_string_lossy().into_owned(),
            arguments: profile
                .arguments()
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect(),
            working_directory: profile
                .working_directory()
                .map(|directory| directory.to_string_lossy().into_owned()),
            environment: environment_spec(profile.environment()),
        })
        .unwrap_or_else(|_| {
            #[cfg(unix)]
            {
                ShellSpec {
                    executable: "/bin/sh".to_owned(),
                    arguments: vec!["-l".to_owned()],
                    working_directory: None,
                    environment: EnvironmentSpec::Inherit,
                }
            }
            #[cfg(windows)]
            {
                ShellSpec {
                    executable: "cmd.exe".to_owned(),
                    arguments: vec!["/Q".to_owned()],
                    working_directory: None,
                    environment: EnvironmentSpec::Inherit,
                }
            }
        })
}

fn environment_spec(policy: &EnvironmentPolicy) -> EnvironmentSpec {
    let convert = |environment: &BTreeMap<std::ffi::OsString, std::ffi::OsString>| {
        environment
            .iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect()
    };
    match policy {
        EnvironmentPolicy::Inherit => EnvironmentSpec::Inherit,
        EnvironmentPolicy::Clear(environment) => EnvironmentSpec::Clear(convert(environment)),
        EnvironmentPolicy::InheritWith(environment) => {
            EnvironmentSpec::InheritWith(convert(environment))
        }
    }
}

fn append_shell_options(command: &mut Command, shell: &ShellSpec) {
    for argument in &shell.arguments {
        command.arg("--arg").arg(argument);
    }
    if let Some(working_directory) = &shell.working_directory {
        command.arg("--cwd").arg(working_directory);
    }
    let (policy, environment) = match &shell.environment {
        EnvironmentSpec::Inherit => return,
        EnvironmentSpec::Clear(environment) => ("clear", environment),
        EnvironmentSpec::InheritWith(environment) => ("inherit-with", environment),
    };
    command.arg("--env-policy").arg(policy);
    for (key, value) in environment {
        command.arg("--env").arg(key).arg(value);
    }
}

fn run_start(
    name: String,
    shell: ShellSpec,
    cols: u16,
    rows: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = validate_name(name)?;
    let runtime_root = runtime_root()?;
    fs::create_dir_all(&runtime_root)?;
    set_dir_mode(&runtime_root, 0o700)?;

    let start_lock_path = runtime_root.join(format!("{name}.start.lock"));
    let start_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&start_lock_path)?;
    set_file_mode(&start_lock_path, 0o600)?;
    start_lock.lock_exclusive()?;

    with_registry_lock(|registry| {
        if let Some(record) = registry.sessions.get(&name) {
            if process_alive(record.pid) {
                return Err(format!("session '{name}' is already running").into());
            }
            registry.sessions.remove(&name);
        }
        Ok(())
    })?;

    let exe = env::current_exe()?;

    #[cfg(unix)]
    let mut daemon = {
        let mut command = Command::new(&exe);
        command
            .arg("daemon")
            .arg("--name")
            .arg(&name)
            .arg("--shell")
            .arg(&shell.executable)
            .arg("--cols")
            .arg(cols.to_string())
            .arg("--rows")
            .arg(rows.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        append_shell_options(&mut command, &shell);
        command.spawn()?
    };

    #[cfg(windows)]
    let mut daemon = {
        let mut command = Command::new(&exe);
        command
            .arg("daemon")
            .arg("--name")
            .arg(&name)
            .arg("--shell")
            .arg(&shell.executable)
            .arg("--cols")
            .arg(cols.to_string())
            .arg("--rows")
            .arg(rows.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Failing to break away is preferable to starting a "persistent"
            // daemon that dies when the caller's KILL_ON_JOB_CLOSE job closes.
            .creation_flags(
                CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB,
            );
        append_shell_options(&mut command, &shell);
        command.spawn()?
    };
    let daemon_pid = daemon.id();

    let registration = (|| -> Result<SessionRecord, Box<dyn std::error::Error>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let registry = load_registry()?;
            if let Some(record) = registry.sessions.get(&name) {
                if record.pid == daemon_pid {
                    return Ok(record.clone());
                }
            }
            if let Some(status) = daemon.try_wait()? {
                return Err(format!(
                    "session daemon for '{name}' exited during startup with {status}"
                )
                .into());
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!("timed out waiting for session '{name}' to register").into());
            }
            thread::sleep(Duration::from_millis(25));
        }
    })();
    let record = match registration {
        Ok(record) => record,
        Err(error) => {
            if daemon.try_wait()?.is_none() {
                let _ = terminate_pid(daemon_pid);
                let _ = daemon.wait();
            }
            return Err(error);
        }
    };
    FileExt::unlock(&start_lock)?;

    println!(
        "started {} pid={} socket={} shell={}",
        name, record.pid, record.socket, record.shell
    );
    Ok(())
}

fn run_daemon(
    name: String,
    shell: ShellSpec,
    cols: u16,
    rows: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    nix::unistd::setsid()?;

    let name = validate_name(name)?;
    let runtime_root = runtime_root()?;
    fs::create_dir_all(&runtime_root)?;
    set_dir_mode(&runtime_root, 0o700)?;

    #[cfg(unix)]
    {
        let socket_path = session_socket_path(&runtime_root, &name)?;
        let listener = bind_unix_listener(&socket_path)?;
        set_file_mode(&socket_path, 0o600)?;

        let mut spawned = spawn_shell(&shell, cols, rows)?;
        let record = SessionRecord {
            name: name.clone(),
            pid: process::id(),
            socket: socket_path.to_string_lossy().into_owned(),
            shell: shell.executable.clone(),
            arguments: shell.arguments.clone(),
            working_directory: shell.working_directory.clone(),
            cols,
            rows,
            created_at_unix_ms: now_ms(),
        };
        if let Err(error) = save_registry_record(record) {
            let _ = spawned.child.kill();
            let _ = spawned.child.wait();
            let _ = fs::remove_file(&socket_path);
            return Err(error);
        }

        let reader = spawned.master.try_clone_reader()?;
        let writer = spawned.master.take_writer()?;
        daemon_client_loop(listener, reader, writer, &mut spawned, &name)?;
        let _ = fs::remove_file(socket_path);
    }

    #[cfg(windows)]
    {
        let pipe_name = session_pipe_name(&name);
        let initial_listener = create_secure_pipe_listener(&pipe_name, true)?;
        let mut spawned = spawn_shell(&shell, cols, rows)?;
        let record = SessionRecord {
            name: name.clone(),
            pid: process::id(),
            socket: pipe_name.clone(),
            shell: shell.executable.clone(),
            arguments: shell.arguments.clone(),
            working_directory: shell.working_directory.clone(),
            cols,
            rows,
            created_at_unix_ms: now_ms(),
        };
        if let Err(error) = save_registry_record(record) {
            let _ = spawned.child.kill();
            let _ = spawned.child.wait();
            return Err(error);
        }

        let reader = spawned.master.try_clone_reader()?;
        let writer = spawned.master.take_writer()?;
        daemon_client_loop_windows(
            &pipe_name,
            initial_listener,
            reader,
            writer,
            &mut spawned,
            &name,
        )?;
    }

    Ok(())
}

#[cfg(unix)]
fn bind_unix_listener(path: &Path) -> io::Result<UnixListener> {
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("session socket {} is already active", path.display()),
                ));
            }
            fs::remove_file(path)?;
            UnixListener::bind(path)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn daemon_client_loop<R: Read + Send + 'static>(
    listener: UnixListener,
    reader: R,
    mut writer: Box<dyn Write + Send>,
    spawned: &mut SpawnedShell,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = session_client_loop(
        listener,
        reader,
        None,
        |command| match command {
            ClientCommand::Input(data) => writer.write_all(&data).and_then(|()| writer.flush()),
            ClientCommand::Resize(size) => spawned
                .master
                .resize(size)
                .map_err(|error| io::Error::other(error.to_string())),
        },
        || {
            let _ = spawned.child.kill();
        },
    );
    if result.is_err() {
        let _ = spawned.child.kill();
    }
    let _ = spawned.child.wait();
    drop_registry_record(name, process::id())?;
    Ok(result?)
}

#[cfg(unix)]
fn session_client_loop<R: Read + Send + 'static>(
    listener: UnixListener,
    reader: R,
    observer: Option<mpsc::Sender<ClientLoopEvent>>,
    mut handle_client_command: impl FnMut(ClientCommand) -> io::Result<()>,
    mut shutdown: impl FnMut(),
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let (pty_rx, reader_thread) = spawn_pty_reader(reader);
    let (client_input_tx, client_input_rx) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
    let mut active: Option<ActiveClient> = None;
    let mut retired_clients = Vec::new();
    let mut next_generation = 1u64;
    let mut replay = ReplayBuffer::default();
    let mut pending_output = None;
    let mut shell_exited = false;
    let mut shell_exit_deadline = None;
    let result = loop {
        if let Err(error) = reap_client_threads(&mut retired_clients) {
            shutdown();
            break Err(error);
        }
        if let Err(error) = accept_unix_clients(
            &listener,
            &mut active,
            &mut retired_clients,
            observer.as_ref(),
            &client_input_tx,
            &mut next_generation,
        ) {
            shutdown();
            break Err(error);
        }
        if let Err(error) = handle_pending_client_input(
            &client_input_rx,
            &mut active,
            &mut retired_clients,
            &replay,
            &mut handle_client_command,
        ) {
            shutdown();
            break Err(error);
        }
        if let Some(output) = pending_output.take() {
            pending_output = retry_pending_output(&mut active, &mut retired_clients, output);
            if pending_output.is_some() {
                thread::sleep(CLIENT_POLL_INTERVAL);
                continue;
            }
        }
        if shell_exited {
            if active.as_ref().is_some_and(|client| !client.ready)
                && shell_exit_deadline.is_some_and(|deadline| Instant::now() < deadline)
            {
                thread::sleep(CLIENT_POLL_INTERVAL);
                continue;
            }
            break Ok(());
        }
        match pty_rx.recv_timeout(CLIENT_POLL_INTERVAL) {
            Ok(PtyEvent::Data(data)) => {
                if let Err(error) = accept_unix_clients(
                    &listener,
                    &mut active,
                    &mut retired_clients,
                    observer.as_ref(),
                    &client_input_tx,
                    &mut next_generation,
                ) {
                    shutdown();
                    break Err(error);
                }
                replay.push(&data);
                if let Some(observer) = observer.as_ref() {
                    let _ = observer.send(ClientLoopEvent::OutputBuffered);
                }
                pending_output = send_to_active(&mut active, &mut retired_clients, data);
            }
            Ok(PtyEvent::Eof) => {
                replay.push(EXITED_NOTICE_BYTES);
                pending_output = send_to_active(
                    &mut active,
                    &mut retired_clients,
                    EXITED_NOTICE_BYTES.to_vec(),
                );
                shell_exited = true;
                shell_exit_deadline = Some(Instant::now() + CLIENT_WRITE_TIMEOUT);
            }
            Ok(PtyEvent::Error(error)) => break Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };

    retire_active(&mut active, &mut retired_clients, false);
    for client in retired_clients {
        join_client_thread(client)?;
    }
    drop(pty_rx);
    join_io_thread(reader_thread)?;
    result
}

#[cfg(unix)]
fn accept_unix_clients(
    listener: &UnixListener,
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    observer: Option<&mpsc::Sender<ClientLoopEvent>>,
    client_input_tx: &mpsc::SyncSender<ClientInput>,
    next_generation: &mut u64,
) -> io::Result<bool> {
    let mut attached = false;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(CLIENT_POLL_INTERVAL))?;
                stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))?;
                replace_active(
                    active,
                    retired_clients,
                    stream,
                    client_input_tx.clone(),
                    next_generation,
                )?;
                if let Some(observer) = observer {
                    let _ = observer.send(ClientLoopEvent::ClientAttached);
                }
                attached = true;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(attached),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientLoopEvent {
    ClientAttached,
    OutputBuffered,
}

#[cfg(windows)]
fn daemon_client_loop_windows<R: Read + Send + 'static>(
    pipe_name: &str,
    initial_listener: named_pipe::ConnectingServer,
    reader: R,
    mut writer: Box<dyn Write + Send>,
    spawned: &mut SpawnedShell,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (accept_tx, accept_rx) = mpsc::channel::<io::Result<named_pipe::PipeServer>>();
    let pipe_name = pipe_name.to_owned();
    let accepting = Arc::new(AtomicBool::new(true));
    let accept_running = Arc::clone(&accepting);
    let accept_pipe_name = pipe_name.clone();
    let accept_thread = thread::spawn(move || -> io::Result<()> {
        let mut initial_listener = Some(initial_listener);
        while accept_running.load(Ordering::Acquire) {
            let server = match initial_listener.take() {
                Some(listener) => listener.wait(),
                None => create_secure_pipe_listener(&accept_pipe_name, false)?.wait(),
            };
            let server = match server {
                Ok(server) => server,
                Err(error) => {
                    let forwarded =
                        io::Error::new(error.kind(), format!("named pipe accept failed: {error}"));
                    let _ = accept_tx.send(Err(forwarded));
                    return Err(error);
                }
            };
            if !accept_running.load(Ordering::Acquire) {
                break;
            }
            accept_tx
                .send(Ok(server))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "accept loop closed"))?;
        }
        Ok(())
    });

    let (pty_rx, reader_thread) = spawn_pty_reader(reader);
    let (client_input_tx, client_input_rx) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
    let mut active: Option<ActiveClient> = None;
    let mut retired_clients = Vec::new();
    let mut next_generation = 1u64;
    let mut replay = ReplayBuffer::default();
    let mut pending_output = None;
    let mut shell_exited = false;
    let mut shell_exit_deadline = None;
    let result = loop {
        if let Err(error) = reap_client_threads(&mut retired_clients) {
            let _ = spawned.child.kill();
            break Err(error);
        }
        if let Err(error) = accept_windows_clients(
            &accept_rx,
            &mut active,
            &mut retired_clients,
            &client_input_tx,
            &mut next_generation,
        ) {
            let _ = spawned.child.kill();
            break Err(error);
        }
        if let Err(error) = handle_pending_client_input(
            &client_input_rx,
            &mut active,
            &mut retired_clients,
            &replay,
            &mut |command| match command {
                ClientCommand::Input(data) => writer.write_all(&data).and_then(|()| writer.flush()),
                ClientCommand::Resize(size) => spawned
                    .master
                    .resize(size)
                    .map_err(|error| io::Error::other(error.to_string())),
            },
        ) {
            let _ = spawned.child.kill();
            break Err(error);
        }
        if let Some(output) = pending_output.take() {
            pending_output = retry_pending_output(&mut active, &mut retired_clients, output);
            if pending_output.is_some() {
                thread::sleep(CLIENT_POLL_INTERVAL);
                continue;
            }
        }
        if shell_exited {
            if active.as_ref().is_some_and(|client| !client.ready)
                && shell_exit_deadline.is_some_and(|deadline| Instant::now() < deadline)
            {
                thread::sleep(CLIENT_POLL_INTERVAL);
                continue;
            }
            break Ok(());
        }
        match pty_rx.recv_timeout(CLIENT_POLL_INTERVAL) {
            Ok(PtyEvent::Data(data)) => {
                if let Err(error) = accept_windows_clients(
                    &accept_rx,
                    &mut active,
                    &mut retired_clients,
                    &client_input_tx,
                    &mut next_generation,
                ) {
                    let _ = spawned.child.kill();
                    break Err(error);
                }
                replay.push(&data);
                pending_output = send_to_active(&mut active, &mut retired_clients, data);
            }
            Ok(PtyEvent::Eof) => {
                replay.push(EXITED_NOTICE_BYTES);
                pending_output = send_to_active(
                    &mut active,
                    &mut retired_clients,
                    EXITED_NOTICE_BYTES.to_vec(),
                );
                shell_exited = true;
                shell_exit_deadline = Some(Instant::now() + CLIENT_WRITE_TIMEOUT);
            }
            Ok(PtyEvent::Error(error)) => break Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };

    accepting.store(false, Ordering::Release);
    retire_active(&mut active, &mut retired_clients, false);
    let _ = named_pipe::PipeClient::connect_ms(&pipe_name, 100);
    drop(pty_rx);
    let reader_result = join_io_thread(reader_thread);
    let accept_result = join_io_thread(accept_thread);
    let client_result = retired_clients.into_iter().try_for_each(join_client_thread);
    if result.is_err() {
        let _ = spawned.child.kill();
    }
    let _ = spawned.child.wait();
    drop_registry_record(name, process::id())?;
    reader_result?;
    accept_result?;
    client_result?;
    result.map_err(Into::into)
}

#[cfg(windows)]
fn create_secure_pipe_listener(
    pipe_name: &str,
    first: bool,
) -> io::Result<named_pipe::ConnectingServer> {
    let guard = festerm_windows_security::restrict_default_dacl_to_current_user()?;
    let mut options = named_pipe::PipeOptions::new(pipe_name);
    options.first(first);
    let listener = options.single()?;
    guard.restore()?;
    Ok(listener)
}

#[cfg(windows)]
fn accept_windows_clients(
    accept_rx: &mpsc::Receiver<io::Result<named_pipe::PipeServer>>,
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    client_input_tx: &mpsc::SyncSender<ClientInput>,
    next_generation: &mut u64,
) -> io::Result<bool> {
    let mut attached = false;
    for stream in accept_rx.try_iter() {
        let mut stream = stream?;
        stream.set_read_timeout(Some(WINDOWS_CLIENT_READ_TIMEOUT));
        stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
        replace_active(
            active,
            retired_clients,
            stream,
            client_input_tx.clone(),
            next_generation,
        )?;
        attached = true;
    }
    Ok(attached)
}

fn spawn_pty_reader<R: Read + Send + 'static>(
    mut reader: R,
) -> (mpsc::Receiver<PtyEvent>, thread::JoinHandle<io::Result<()>>) {
    let (sender, receiver) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
    let thread = thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(PtyEvent::Eof);
                    return Ok(());
                }
                Ok(count) => {
                    if sender
                        .send(PtyEvent::Data(buffer[..count].to_vec()))
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let kind = error.kind();
                    let message = error.to_string();
                    let _ = sender.send(PtyEvent::Error(io::Error::new(kind, message)));
                    return Ok(());
                }
            }
        }
    });
    (receiver, thread)
}

#[derive(Debug)]
enum ClientCommand {
    Input(Vec<u8>),
    Resize(PtySize),
}

#[derive(Debug)]
struct ClientInput {
    generation: u64,
    command: ClientCommand,
}

enum ClientOutput {
    Data(Vec<u8>),
}

#[derive(Debug, PartialEq, Eq)]
struct PendingClientOutput {
    generation: u64,
    data: Vec<u8>,
}

struct ActiveClient {
    generation: u64,
    ready: bool,
    output: mpsc::SyncSender<ClientOutput>,
    stolen: Arc<AtomicBool>,
    thread: thread::JoinHandle<io::Result<()>>,
}

fn replace_active<S: Read + Write + Send + 'static>(
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    replacement: S,
    input: mpsc::SyncSender<ClientInput>,
    next_generation: &mut u64,
) -> io::Result<()> {
    retire_active(active, retired_clients, true);

    let generation = *next_generation;
    *next_generation = next_generation.wrapping_add(1);
    let (output, output_rx) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
    let stolen = Arc::new(AtomicBool::new(false));
    let worker_stolen = Arc::clone(&stolen);
    let thread = thread::Builder::new()
        .name(format!("festerm-sessiond-client-{generation}"))
        .spawn(move || client_io_loop(replacement, generation, input, output_rx, worker_stolen))?;
    let client = ActiveClient {
        generation,
        ready: false,
        output,
        stolen,
        thread,
    };
    *active = Some(client);
    Ok(())
}

fn retire_active(
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    stolen: bool,
) {
    if let Some(previous) = active.take() {
        previous.stolen.store(stolen, Ordering::Release);
        drop(previous.output);
        retired_clients.push(previous.thread);
    }
}

fn send_to_active(
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    data: Vec<u8>,
) -> Option<PendingClientOutput> {
    if active.as_ref().is_some_and(|client| !client.ready) {
        return None;
    }
    let client = active.as_ref()?;
    match client.output.try_send(ClientOutput::Data(data)) {
        Ok(()) => None,
        Err(mpsc::TrySendError::Full(ClientOutput::Data(data))) => Some(PendingClientOutput {
            generation: client.generation,
            data,
        }),
        Err(mpsc::TrySendError::Disconnected(_)) => {
            retire_active(active, retired_clients, false);
            None
        }
    }
}

fn retry_pending_output(
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    pending: PendingClientOutput,
) -> Option<PendingClientOutput> {
    if !active
        .as_ref()
        .is_some_and(|client| client.generation == pending.generation)
    {
        return None;
    }
    send_to_active(active, retired_clients, pending.data)
}

fn handle_pending_client_input(
    input: &mpsc::Receiver<ClientInput>,
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    replay: &ReplayBuffer,
    handle: &mut impl FnMut(ClientCommand) -> io::Result<()>,
) -> io::Result<()> {
    for input in input.try_iter() {
        let Some(client) = active
            .as_ref()
            .filter(|client| client.generation == input.generation)
        else {
            continue;
        };
        let activates_client = !client.ready && matches!(input.command, ClientCommand::Resize(_));
        if !client.ready && !activates_client {
            continue;
        }
        handle(input.command)?;
        if activates_client {
            let replay = replay.to_vec();
            if let Some(client) = active.as_mut() {
                client.ready = true;
            }
            if !replay.is_empty() {
                let pending = send_to_active(active, retired_clients, replay);
                debug_assert!(
                    pending.is_none(),
                    "fresh client output queue must accept replay"
                );
            }
        }
    }
    Ok(())
}

fn client_io_loop<S: Read + Write>(
    mut stream: S,
    generation: u64,
    input: mpsc::SyncSender<ClientInput>,
    output: mpsc::Receiver<ClientOutput>,
    stolen: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut parser = ClientFrameParser::default();
    let mut buffer = [0u8; 4096];
    loop {
        if stolen.load(Ordering::Acquire) {
            write_client_bytes(
                &mut stream,
                STOLEN_NOTICE_BYTES,
                None,
                Some(Instant::now() + CLIENT_WRITE_TIMEOUT),
            )?;
            return Ok(());
        }
        loop {
            match output.try_recv() {
                Ok(ClientOutput::Data(data)) => {
                    if !write_client_bytes(&mut stream, &data, Some(&stolen), None)? {
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                parser.push(&buffer[..count])?;
                for command in parser.drain()? {
                    match input.try_send(ClientInput {
                        generation,
                        command,
                    }) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Full(_)) => {
                            return Err(io::Error::new(
                                io::ErrorKind::WouldBlock,
                                "session client input queue is full",
                            ))
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn write_client_bytes<W: Write>(
    writer: &mut W,
    mut bytes: &[u8],
    cancelled: Option<&AtomicBool>,
    deadline: Option<Instant>,
) -> io::Result<bool> {
    while !bytes.is_empty() {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
            return Ok(false);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "session client output deadline elapsed",
            ));
        }
        match writer.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "session client output stream stopped accepting bytes",
                ))
            }
            Ok(count) => bytes = &bytes[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(CLIENT_POLL_INTERVAL);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    loop {
        match writer.flush() {
            Ok(()) => return Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
                    return Ok(false);
                }
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "session client flush deadline elapsed",
                    ));
                }
                thread::sleep(CLIENT_POLL_INTERVAL);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[derive(Default)]
struct ClientFrameParser {
    bytes: Vec<u8>,
}

impl ClientFrameParser {
    fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.bytes.len().saturating_add(bytes.len())
            > MAX_CLIENT_FRAME_BYTES + CLIENT_FRAME_HEADER_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session client frame exceeds the protocol limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn drain(&mut self) -> io::Result<Vec<ClientCommand>> {
        let mut commands = Vec::new();
        loop {
            if self.bytes.len() < CLIENT_FRAME_HEADER_BYTES {
                break;
            }
            if &self.bytes[..CLIENT_FRAME_MAGIC.len()] != CLIENT_FRAME_MAGIC {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "session client frame has an invalid magic value",
                ));
            }
            let kind = self.bytes[4];
            let payload_len =
                u32::from_be_bytes(self.bytes[5..9].try_into().expect("fixed frame header"))
                    as usize;
            if payload_len > MAX_CLIENT_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "session client frame payload exceeds the protocol limit",
                ));
            }
            let frame_len = CLIENT_FRAME_HEADER_BYTES + payload_len;
            if self.bytes.len() < frame_len {
                break;
            }
            let payload = &self.bytes[CLIENT_FRAME_HEADER_BYTES..frame_len];
            let command = match kind {
                CLIENT_FRAME_INPUT => ClientCommand::Input(payload.to_vec()),
                CLIENT_FRAME_RESIZE => ClientCommand::Resize(parse_resize_frame(payload)?),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "session client frame has an unknown command",
                    ))
                }
            };
            self.bytes.drain(..frame_len);
            commands.push(command);
        }
        Ok(commands)
    }
}

fn parse_resize_frame(payload: &[u8]) -> io::Result<PtySize> {
    if payload.len() != 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session resize frame has an invalid length",
        ));
    }
    let value = |offset| u16::from_be_bytes([payload[offset], payload[offset + 1]]);
    let size = PtySize {
        cols: value(0),
        rows: value(2),
        pixel_width: value(4),
        pixel_height: value(6),
    };
    if size.cols < 2 || size.rows == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session resize frame has invalid terminal dimensions",
        ));
    }
    Ok(size)
}

fn join_io_thread(thread: thread::JoinHandle<io::Result<()>>) -> io::Result<()> {
    thread
        .join()
        .map_err(|_| io::Error::other("session daemon worker thread panicked"))?
}

fn join_client_thread(thread: thread::JoinHandle<io::Result<()>>) -> io::Result<()> {
    let _ = thread
        .join()
        .map_err(|_| io::Error::other("session client worker thread panicked"))?;
    Ok(())
}

fn reap_client_threads(clients: &mut Vec<thread::JoinHandle<io::Result<()>>>) -> io::Result<()> {
    let mut index = 0;
    while index < clients.len() {
        if clients[index].is_finished() {
            join_client_thread(clients.swap_remove(index))?;
        } else {
            index += 1;
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ReplayBuffer {
    bytes: VecDeque<u8>,
    capacity: usize,
    truncated: bool,
}

impl Default for ReplayBuffer {
    fn default() -> Self {
        Self {
            bytes: VecDeque::with_capacity(REPLAY_CAPACITY_BYTES),
            capacity: REPLAY_CAPACITY_BYTES,
            truncated: false,
        }
    }
}

impl ReplayBuffer {
    fn to_vec(&self) -> Vec<u8> {
        let reset = if self.truncated {
            REPLAY_RESET_BYTES
        } else {
            &[]
        };
        let mut replay = Vec::with_capacity(reset.len() + self.bytes.len());
        replay.extend_from_slice(reset);
        replay.extend(self.bytes.iter().copied());
        replay
    }

    fn push(&mut self, data: &[u8]) {
        if data.len() >= self.capacity {
            self.bytes.clear();
            self.bytes.extend(
                data[data.len().saturating_sub(self.capacity)..]
                    .iter()
                    .copied(),
            );
            self.truncated = true;
            self.align_truncated_start();
            return;
        }

        let overflow = self
            .bytes
            .len()
            .saturating_add(data.len())
            .saturating_sub(self.capacity);
        if overflow > 0 {
            self.bytes.drain(..overflow);
            self.truncated = true;
        }
        self.bytes.extend(data.iter().copied());
        if overflow > 0 {
            self.align_truncated_start();
        }
    }

    fn align_truncated_start(&mut self) {
        let escape = self.bytes.iter().position(|byte| *byte == b'\x1b');
        let after_newline = self
            .bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position + 1);
        if let Some(boundary) = [escape, after_newline].into_iter().flatten().min() {
            self.bytes.drain(..boundary);
        } else {
            while self
                .bytes
                .front()
                .is_some_and(|byte| byte & 0b1100_0000 == 0b1000_0000)
            {
                self.bytes.pop_front();
            }
        }
    }
}

#[derive(Debug)]
enum PtyEvent {
    Data(Vec<u8>),
    Eof,
    Error(io::Error),
}

const STOLEN_NOTICE_BYTES: &[u8] =
    b"\n[festerm-sessiond] SESSION_STOLEN: reattached from another client\n";
const EXITED_NOTICE_BYTES: &[u8] = b"\n[festerm-sessiond] SESSION_EXITED\n";

fn run_list() -> Result<(), Box<dyn std::error::Error>> {
    let registry = with_registry_lock(|registry| {
        prune_dead_records(registry);
        Ok(registry.clone())
    })?;

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
        let Some(record) = registry.sessions.get(&name).cloned() else {
            return Err(format!("session '{name}' is not registered").into());
        };
        if process_alive(record.pid) && endpoint_reachable(&record) {
            terminate_pid(record.pid)?;
        }
        if registry
            .sessions
            .get(&name)
            .is_some_and(|current| current.pid == record.pid)
        {
            registry.sessions.remove(&name);
        }
        Ok(())
    })
}

fn endpoint_reachable(record: &SessionRecord) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    loop {
        #[cfg(unix)]
        let connected = UnixStream::connect(&record.socket).is_ok();
        #[cfg(windows)]
        let connected = named_pipe::PipeClient::connect_ms(&record.socket, 50).is_ok();

        if connected {
            return true;
        }
        if !process_alive(record.pid) || std::time::Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachOutcome {
    Closed,
    Exited,
    Stolen,
}

#[cfg(test)]
fn forward_attach_stream<R: Read, W: Write>(
    reader: &mut R,
    output: &mut W,
) -> io::Result<AttachOutcome> {
    let mut buffer = [0u8; 4096];
    let mut scanner = AttachOutputScanner::default();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return scanner.close(output),
            Ok(count) => {
                if let Some(outcome) = scanner.push(&buffer[..count], output)? {
                    return Ok(outcome);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                return scanner.close(output)
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(Default)]
struct AttachOutputScanner {
    pending: Vec<u8>,
}

impl AttachOutputScanner {
    fn push<W: Write>(
        &mut self,
        bytes: &[u8],
        output: &mut W,
    ) -> io::Result<Option<AttachOutcome>> {
        self.pending.extend_from_slice(bytes);
        if let Some(position) = find_bytes(&self.pending, STOLEN_NOTICE_BYTES) {
            output.write_all(&self.pending[..position])?;
            output.flush()?;
            self.pending.clear();
            return Ok(Some(AttachOutcome::Stolen));
        }
        if let Some(position) = find_bytes(&self.pending, EXITED_NOTICE_BYTES) {
            output.write_all(&self.pending[..position])?;
            output.flush()?;
            self.pending.clear();
            return Ok(Some(AttachOutcome::Exited));
        }

        let retained = partial_marker_suffix_len(&self.pending);
        let flush_count = self.pending.len() - retained;
        if flush_count > 0 {
            output.write_all(&self.pending[..flush_count])?;
            self.pending.drain(..flush_count);
            output.flush()?;
        }
        Ok(None)
    }

    fn close<W: Write>(&mut self, output: &mut W) -> io::Result<AttachOutcome> {
        output.write_all(&self.pending)?;
        output.flush()?;
        self.pending.clear();
        Ok(AttachOutcome::Closed)
    }
}

fn partial_marker_suffix_len(data: &[u8]) -> usize {
    [STOLEN_NOTICE_BYTES, EXITED_NOTICE_BYTES]
        .into_iter()
        .map(|marker| {
            let maximum = data.len().min(marker.len().saturating_sub(1));
            (1..=maximum)
                .rev()
                .find(|&length| data.ends_with(&marker[..length]))
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn run_attach(name: String) -> Result<(), Box<dyn std::error::Error>> {
    let name = validate_name(name)?;
    let registry = load_registry()?;
    let record = registry
        .sessions
        .get(&name)
        .ok_or_else(|| format!("session '{name}' is not registered"))?;

    #[cfg(unix)]
    let outcome = {
        let mut stream = UnixStream::connect(&record.socket)?;
        stream.set_read_timeout(Some(CLIENT_POLL_INTERVAL))?;
        stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))?;
        write_client_resize(&mut stream, record.cols, record.rows, 0, 0)?;
        forward_attach_duplex(&mut stream, &mut io::stdout())?
    };

    #[cfg(windows)]
    let outcome = {
        let mut stream = named_pipe::PipeClient::connect(&record.socket)?;
        stream.set_read_timeout(Some(CLIENT_POLL_INTERVAL));
        stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
        write_client_resize(&mut stream, record.cols, record.rows, 0, 0)?;
        forward_attach_duplex(&mut stream, &mut io::stdout())?
    };

    if outcome == AttachOutcome::Stolen {
        eprintln!(
            "[festerm-sessiond] session taken over by another client; this attach lost the session"
        );
    }

    Ok(())
}

fn forward_attach_duplex<S: Read + Write, W: Write>(
    stream: &mut S,
    output: &mut W,
) -> io::Result<AttachOutcome> {
    let (input_tx, input_rx) = mpsc::sync_channel::<Vec<u8>>(CLIENT_QUEUE_CAPACITY);
    thread::Builder::new()
        .name("festerm-sessiond-stdin".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin();
            let mut buffer = [0u8; 4096];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(count) => {
                        if input_tx.send(buffer[..count].to_vec()).is_err() {
                            return;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
        })?;

    let mut scanner = AttachOutputScanner::default();
    let mut buffer = [0u8; 4096];
    loop {
        for input in input_rx.try_iter() {
            write_client_frame(stream, CLIENT_FRAME_INPUT, &input)?;
        }
        match stream.read(&mut buffer) {
            Ok(0) => return scanner.close(output),
            Ok(count) => {
                if let Some(outcome) = scanner.push(&buffer[..count], output)? {
                    return Ok(outcome);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                return scanner.close(output)
            }
            Err(error) => return Err(error),
        }
    }
}

fn write_client_frame<W: Write>(writer: &mut W, kind: u8, payload: &[u8]) -> io::Result<()> {
    let payload_len = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "session client frame payload is too large",
        )
    })?;
    if payload.len() > MAX_CLIENT_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session client frame payload exceeds the protocol limit",
        ));
    }
    writer.write_all(CLIENT_FRAME_MAGIC)?;
    writer.write_all(&[kind])?;
    writer.write_all(&payload_len.to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

fn write_client_resize<W: Write>(
    writer: &mut W,
    cols: u16,
    rows: u16,
    pixel_width: u16,
    pixel_height: u16,
) -> io::Result<()> {
    let mut payload = Vec::with_capacity(8);
    for value in [cols, rows, pixel_width, pixel_height] {
        payload.extend_from_slice(&value.to_be_bytes());
    }
    write_client_frame(writer, CLIENT_FRAME_RESIZE, &payload)
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
            Err(
                "neither XDG_STATE_HOME nor HOME is set; refusing an unscoped runtime directory"
                    .into(),
            )
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
            Err(
                "neither LOCALAPPDATA nor USERPROFILE is set; refusing an unscoped runtime directory"
                    .into(),
            )
        }
    }
}

#[cfg(unix)]
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

fn registry_lock_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(runtime_root()?.join("registry.lock"))
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
    let lock_path = registry_lock_path()?;
    let parent = lock_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    set_dir_mode(parent, 0o700)?;
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    set_file_mode(&lock_path, 0o600)?;
    lock_file.lock_exclusive()?;
    let mut registry = read_registry_at(&path)?;
    let result = operation(&mut registry);
    if result.is_ok() {
        write_registry_at(&path, &registry)?;
    }
    FileExt::unlock(&lock_file)?;
    result
}

fn write_registry_at(
    path: &Path,
    registry: &SessionRegistry,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = serde_json::to_string_pretty(registry)?;
    fs::write(path, output)?;
    set_file_mode(path, 0o600)?;
    Ok(())
}

fn load_registry() -> Result<SessionRegistry, Box<dyn std::error::Error>> {
    let path = registry_path()?;
    let lock_path = registry_lock_path()?;
    let parent = lock_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    set_dir_mode(parent, 0o700)?;
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    set_file_mode(&lock_path, 0o600)?;
    FileExt::lock_shared(&lock_file)?;
    let result = read_registry_at(&path);
    FileExt::unlock(&lock_file)?;
    result
}

fn save_registry_record(record: SessionRecord) -> Result<(), Box<dyn std::error::Error>> {
    with_registry_lock(|registry: &mut SessionRegistry| {
        if let Some(existing) = registry.sessions.get(&record.name) {
            if existing.pid != record.pid && process_alive(existing.pid) {
                return Err(format!("session '{}' is already running", record.name).into());
            }
        }
        registry
            .sessions
            .insert(record.name.clone(), record.clone());
        Ok(())
    })
}

fn drop_registry_record(name: &str, pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    with_registry_lock(|registry: &mut SessionRegistry| {
        remove_registry_record_if_pid_matches(registry, name, pid);
        Ok(())
    })
}

fn remove_registry_record_if_pid_matches(registry: &mut SessionRegistry, name: &str, pid: u32) {
    if registry
        .sessions
        .get(name)
        .is_some_and(|record| record.pid == pid)
    {
        registry.sessions.remove(name);
    }
}

fn prune_dead_records(registry: &mut SessionRegistry) {
    registry
        .sessions
        .retain(|_, record| process_alive(record.pid));
}

#[cfg(unix)]
fn set_dir_mode(path: &Path, mode: u32) -> io::Result<()> {
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_dir_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn spawn_shell(
    shell: &ShellSpec,
    cols: u16,
    rows: u16,
) -> Result<SpawnedShell, Box<dyn std::error::Error>> {
    #[cfg(windows)]
    festerm_pty::prepare_windows_conpty_runtime()?;

    let system = native_pty_system();
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = system.openpty(size)?;
    let mut command = CommandBuilder::new(&shell.executable);
    command.args(&shell.arguments);
    command.cwd(match &shell.working_directory {
        Some(working_directory) => PathBuf::from(working_directory),
        None => env::current_dir()?,
    });
    match &shell.environment {
        EnvironmentSpec::Inherit => {}
        EnvironmentSpec::Clear(environment) => {
            command.env_clear();
            for (key, value) in environment {
                command.env(key, value);
            }
        }
        EnvironmentSpec::InheritWith(environment) => {
            for (key, value) in environment {
                command.env(key, value);
            }
        }
    }
    command.env("TERM", "xterm-256color");
    command.env("TERM_PROGRAM", "fesTerm");
    command.env_remove("TERM_SESSION_ID");
    let child = pair.slave.spawn_command(command)?;
    let master = pair.master;
    Ok(SpawnedShell { child, master })
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

        match kill(Pid::from_raw(pid as i32), None) {
            Ok(()) | Err(Errno::EPERM) => true,
            Err(Errno::ESRCH) => false,
            Err(_) => false,
        }
    }

    #[cfg(windows)]
    {
        festerm_windows_job::process_is_alive(pid)
    }
}

fn terminate_pid(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use nix::{
            errno::Errno,
            sys::signal::{kill, Signal},
            unistd::Pid,
        };

        let pid = Pid::from_raw(pid as i32);
        match kill(pid, Signal::SIGTERM) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(250));
        if process_alive(pid.as_raw() as u32) {
            match kill(pid, Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        festerm_windows_job::terminate_process(pid)?;
        Ok(())
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

    #[cfg(unix)]
    #[test]
    fn second_client_replaces_first_client_and_first_receives_stolen_notice() {
        use std::{
            io::{self, Read},
            os::unix::net::{UnixListener, UnixStream},
            sync::mpsc::{self, Receiver, Sender},
        };

        struct ChannelReader {
            receiver: Receiver<Vec<u8>>,
            pending: Vec<u8>,
        }

        impl Read for ChannelReader {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if self.pending.is_empty() {
                    match self.receiver.recv() {
                        Ok(data) => self.pending = data,
                        Err(_) => return Ok(0),
                    }
                }
                if self.pending.is_empty() {
                    return Ok(0);
                }
                let count = output.len().min(self.pending.len());
                output[..count].copy_from_slice(&self.pending[..count]);
                self.pending.drain(..count);
                Ok(count)
            }
        }

        fn send_and_receive(
            pty: &Sender<Vec<u8>>,
            client: &mut UnixStream,
            data: &[u8],
        ) -> Vec<u8> {
            pty.send(data.to_vec()).unwrap();
            let mut received = vec![0; data.len()];
            client.read_exact(&mut received).unwrap();
            received
        }

        let directory = unique_test_directory("steal");
        fs::create_dir_all(&directory).unwrap();
        let socket_path = directory.join("session.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (pty_sender, pty_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let (command_sender, command_receiver) = mpsc::channel();
        let service = thread::spawn(move || {
            session_client_loop(
                listener,
                ChannelReader {
                    receiver: pty_receiver,
                    pending: Vec::new(),
                },
                Some(event_sender),
                move |command| {
                    command_sender
                        .send(command)
                        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "test closed"))
                },
                || {},
            )
        });

        pty_sender.send(b"detached".to_vec()).unwrap();
        assert_eq!(
            event_receiver.recv().unwrap(),
            ClientLoopEvent::OutputBuffered
        );

        let mut first = UnixStream::connect(&socket_path).unwrap();
        assert_eq!(
            event_receiver.recv().unwrap(),
            ClientLoopEvent::ClientAttached
        );
        write_client_resize(&mut first, 80, 24, 0, 0).unwrap();
        match command_receiver.recv().unwrap() {
            ClientCommand::Resize(size) => {
                assert_eq!(size.cols, 80);
                assert_eq!(size.rows, 24);
            }
            command => panic!("expected initial resize command, received {command:?}"),
        }
        let mut replayed = [0u8; 8];
        first.read_exact(&mut replayed).unwrap();
        assert_eq!(&replayed, b"detached");
        write_client_frame(&mut first, CLIENT_FRAME_INPUT, b"typed").unwrap();
        match command_receiver.recv().unwrap() {
            ClientCommand::Input(data) => assert_eq!(data, b"typed"),
            command => panic!("expected input command, received {command:?}"),
        }

        assert_eq!(
            send_and_receive(&pty_sender, &mut first, b"first"),
            b"first"
        );
        assert_eq!(
            event_receiver.recv().unwrap(),
            ClientLoopEvent::OutputBuffered
        );

        let mut second = UnixStream::connect(&socket_path).unwrap();
        assert_eq!(
            event_receiver.recv().unwrap(),
            ClientLoopEvent::ClientAttached
        );
        write_client_resize(&mut second, 120, 40, 1200, 800).unwrap();
        let mut stolen = vec![0; STOLEN_NOTICE_BYTES.len()];
        first.read_exact(&mut stolen).unwrap();
        assert_eq!(stolen, STOLEN_NOTICE_BYTES);
        let mut eof = [0u8; 1];
        assert_eq!(first.read(&mut eof).unwrap(), 0);

        let mut second_replay = [0u8; 13];
        second.read_exact(&mut second_replay).unwrap();
        assert_eq!(&second_replay, b"detachedfirst");
        match command_receiver.recv().unwrap() {
            ClientCommand::Resize(size) => {
                assert_eq!(size.cols, 120);
                assert_eq!(size.rows, 40);
                assert_eq!(size.pixel_width, 1200);
                assert_eq!(size.pixel_height, 800);
            }
            command => panic!("expected resize command, received {command:?}"),
        }

        assert_eq!(
            send_and_receive(&pty_sender, &mut second, b"second"),
            b"second"
        );
        assert_eq!(
            event_receiver.recv().unwrap(),
            ClientLoopEvent::OutputBuffered
        );

        let mut third = UnixStream::connect(&socket_path).unwrap();
        assert_eq!(
            event_receiver.recv().unwrap(),
            ClientLoopEvent::ClientAttached
        );
        let mut stolen = vec![0; STOLEN_NOTICE_BYTES.len()];
        second.read_exact(&mut stolen).unwrap();
        assert_eq!(stolen, STOLEN_NOTICE_BYTES);
        assert_eq!(second.read(&mut eof).unwrap(), 0);

        drop(pty_sender);
        thread::sleep(CLIENT_POLL_INTERVAL * 2);
        write_client_resize(&mut third, 100, 30, 0, 0).unwrap();
        let mut replay_and_exit = vec![0; 19 + EXITED_NOTICE_BYTES.len()];
        third.read_exact(&mut replay_and_exit).unwrap();
        assert_eq!(&replay_and_exit[..19], b"detachedfirstsecond");
        assert_eq!(&replay_and_exit[19..], EXITED_NOTICE_BYTES);
        assert_eq!(third.read(&mut eof).unwrap(), 0);
        service.join().unwrap().unwrap();
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn attach_sentinel_is_detected_across_read_boundaries_without_forwarding_it() {
        struct SplitReader {
            chunks: std::collections::VecDeque<Vec<u8>>,
        }

        impl Read for SplitReader {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let Some(chunk) = self.chunks.pop_front() else {
                    return Ok(0);
                };
                output[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
        }

        let split = STOLEN_NOTICE_BYTES.len() / 2;
        let mut reader = SplitReader {
            chunks: std::collections::VecDeque::from([
                [b"shell output".as_slice(), &STOLEN_NOTICE_BYTES[..split]].concat(),
                STOLEN_NOTICE_BYTES[split..].to_vec(),
            ]),
        };
        let mut output = Vec::new();

        assert_eq!(
            forward_attach_stream(&mut reader, &mut output).unwrap(),
            AttachOutcome::Stolen
        );
        assert_eq!(output, b"shell output");
    }

    #[test]
    fn attach_scanner_does_not_retain_normal_short_output() {
        assert_eq!(partial_marker_suffix_len(b"$ "), 0);
        assert_eq!(partial_marker_suffix_len(&STOLEN_NOTICE_BYTES[..12]), 12);
        assert_eq!(
            partial_marker_suffix_len(
                [b"ordinary output".as_slice(), &STOLEN_NOTICE_BYTES[..9]]
                    .concat()
                    .as_slice()
            ),
            9
        );
    }

    #[test]
    fn parser_rejects_invalid_dimensions_duplicate_and_unknown_flags() {
        let invalid_dimension = vec![
            "--name".to_owned(),
            "demo".to_owned(),
            "--cols".to_owned(),
            "wide".to_owned(),
        ];
        assert!(parse_start(&invalid_dimension)
            .unwrap_err()
            .to_string()
            .contains("valid u16"));

        let duplicate = vec![
            "--name".to_owned(),
            "demo".to_owned(),
            "--name".to_owned(),
            "other".to_owned(),
        ];
        assert!(parse_start(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("more than once"));

        let unknown = vec![
            "--name".to_owned(),
            "demo".to_owned(),
            "--bogus".to_owned(),
            "value".to_owned(),
        ];
        assert!(parse_start(&unknown)
            .unwrap_err()
            .to_string()
            .contains("does not recognize"));
    }

    #[test]
    fn parser_preserves_explicit_environment_policy() {
        let arguments = vec![
            "--name".to_owned(),
            "demo".to_owned(),
            "--shell".to_owned(),
            "zsh".to_owned(),
            "--env-policy".to_owned(),
            "inherit-with".to_owned(),
            "--env".to_owned(),
            "LANG".to_owned(),
            "en_US.UTF-8".to_owned(),
            "--env".to_owned(),
            "PATH".to_owned(),
            "/opt/homebrew/bin:/usr/bin".to_owned(),
        ];

        let CommandSpec::Start { shell, .. } = parse_start(&arguments).unwrap() else {
            unreachable!()
        };
        assert_eq!(
            shell.environment,
            EnvironmentSpec::InheritWith(BTreeMap::from([
                ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
                ("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned()),
            ]))
        );
    }

    #[test]
    fn registry_removal_does_not_delete_a_replacement_daemon() {
        let mut registry = SessionRegistry {
            sessions: BTreeMap::from([(
                "demo".to_owned(),
                SessionRecord {
                    name: "demo".to_owned(),
                    pid: 22,
                    socket: "demo.sock".to_owned(),
                    shell: "shell".to_owned(),
                    arguments: Vec::new(),
                    working_directory: None,
                    cols: 80,
                    rows: 24,
                    created_at_unix_ms: 2,
                },
            )]),
        };

        remove_registry_record_if_pid_matches(&mut registry, "demo", 11);
        assert_eq!(registry.sessions["demo"].pid, 22);
        remove_registry_record_if_pid_matches(&mut registry, "demo", 22);
        assert!(!registry.sessions.contains_key("demo"));
    }

    #[test]
    fn replay_buffer_keeps_only_its_newest_bytes() {
        let mut replay = ReplayBuffer {
            bytes: VecDeque::new(),
            capacity: 5,
            truncated: false,
        };
        replay.push(b"abc");
        replay.push(b"defg");
        assert_eq!(replay.bytes.iter().copied().collect::<Vec<_>>(), b"cdefg");
        replay.push(b"1234567");
        assert_eq!(replay.bytes.iter().copied().collect::<Vec<_>>(), b"34567");
        assert_eq!(replay.to_vec(), [REPLAY_RESET_BYTES, b"34567"].concat());

        let mut control_sequence = ReplayBuffer {
            bytes: VecDeque::from(b"31mcorrupt\x1b[2Jclean".to_vec()),
            capacity: 32,
            truncated: true,
        };
        control_sequence.align_truncated_start();
        assert_eq!(
            control_sequence.to_vec(),
            [REPLAY_RESET_BYTES, b"\x1b[2Jclean"].concat()
        );
    }

    #[test]
    fn truncated_replay_resets_parser_and_renders_only_the_safe_suffix() {
        use festerm_core::{Dimensions, Terminal};

        let mut terminal =
            Terminal::new(Dimensions::new(24, 4).unwrap()).expect("terminal allocation");
        terminal.ingest(b"stale-before-reconnect\x1b[31mBROKEN");

        let mut replay = ReplayBuffer {
            bytes: VecDeque::from(
                b"partial-utf8-\xf0\x9f\n\x1b[2JRECONNECTED\r\nprompt> ".to_vec(),
            ),
            capacity: 64,
            truncated: true,
        };
        replay.align_truncated_start();
        terminal.ingest(&replay.to_vec());

        let history = terminal
            .scrollback_lines()
            .flat_map(|line| line.cells())
            .map(|cell| cell.character())
            .collect::<String>();
        let visible = (0..terminal.dimensions().rows())
            .filter_map(|row| terminal.row_text(row))
            .collect::<String>();
        let rendered = format!("{history}{visible}");
        assert!(rendered.contains("RECONNECTED"));
        assert!(rendered.contains("prompt>"));
        assert!(!rendered.contains("stale-before-reconnect"));
        assert!(!rendered.contains("BROKEN"));
        assert!(!rendered.contains('\u{fffd}'));
    }

    #[test]
    fn failed_client_output_is_retired_for_worker_join() {
        let (output, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let mut active = Some(ActiveClient {
            generation: 1,
            ready: true,
            output,
            stolen: Arc::new(AtomicBool::new(false)),
            thread: thread::spawn(|| -> io::Result<()> {
                panic!("test client worker panic");
            }),
        });
        let mut retired = Vec::new();

        let pending = send_to_active(&mut active, &mut retired, b"output".to_vec());

        assert!(pending.is_none());
        assert!(active.is_none());
        assert_eq!(retired.len(), 1);
        assert!(join_client_thread(retired.pop().unwrap()).is_err());
    }

    #[test]
    fn slow_client_output_is_deferred_instead_of_disconnected() {
        let (output, receiver) = mpsc::sync_channel(1);
        output.send(ClientOutput::Data(b"queued".to_vec())).unwrap();
        let mut active = Some(ActiveClient {
            generation: 1,
            ready: true,
            output,
            stolen: Arc::new(AtomicBool::new(false)),
            thread: thread::spawn(|| Ok(())),
        });
        let mut retired = Vec::new();

        let pending = send_to_active(&mut active, &mut retired, b"next".to_vec());
        assert_eq!(
            pending,
            Some(PendingClientOutput {
                generation: 1,
                data: b"next".to_vec(),
            })
        );
        assert!(active.is_some());
        assert!(retired.is_empty());
        let ClientOutput::Data(first) = receiver.recv().unwrap();
        assert_eq!(first, b"queued");
        let pending = retry_pending_output(
            &mut active,
            &mut retired,
            pending.expect("output was deferred"),
        );
        assert!(pending.is_none());
        assert!(active.is_some());
        assert!(retired.is_empty());
        let ClientOutput::Data(second) = receiver.recv().unwrap();
        assert_eq!(second, b"next");

        retire_active(&mut active, &mut retired, false);
        join_client_thread(retired.pop().unwrap()).unwrap();
    }

    #[test]
    fn deferred_output_is_not_duplicated_into_a_replacement_client() {
        let (output, receiver) = mpsc::sync_channel(1);
        let mut active = Some(ActiveClient {
            generation: 2,
            ready: true,
            output,
            stolen: Arc::new(AtomicBool::new(false)),
            thread: thread::spawn(|| Ok(())),
        });
        let mut retired = Vec::new();
        let pending = PendingClientOutput {
            generation: 1,
            data: b"old-client-output".to_vec(),
        };

        assert!(retry_pending_output(&mut active, &mut retired, pending).is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        retire_active(&mut active, &mut retired, false);
        join_client_thread(retired.pop().unwrap()).unwrap();
    }

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
            arguments: vec!["-l".to_owned()],
            working_directory: Some("/tmp".to_owned()),
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

    #[cfg(unix)]
    #[test]
    fn spawned_shell_honors_the_configured_working_directory() {
        let directory = unique_test_directory("cwd");
        fs::create_dir_all(&directory).unwrap();
        let mut spawned = spawn_shell(
            &ShellSpec {
                executable: "/bin/pwd".to_owned(),
                arguments: Vec::new(),
                working_directory: Some(directory.to_string_lossy().into_owned()),
                environment: EnvironmentSpec::Inherit,
            },
            80,
            24,
        )
        .unwrap();
        let mut reader = spawned.master.try_clone_reader().unwrap();
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        let status = spawned.child.wait().unwrap();
        assert!(status.success());
        assert_eq!(
            Path::new(output.trim()),
            directory.canonicalize().unwrap().as_path()
        );
        fs::remove_dir(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn spawned_shell_honors_a_cleared_explicit_environment() {
        let mut spawned = spawn_shell(
            &ShellSpec {
                executable: "/usr/bin/env".to_owned(),
                arguments: Vec::new(),
                working_directory: None,
                environment: EnvironmentSpec::Clear(BTreeMap::from([(
                    "FESTERM_SESSIOND_TEST".to_owned(),
                    "present".to_owned(),
                )])),
            },
            80,
            24,
        )
        .unwrap();
        let mut reader = spawned.master.try_clone_reader().unwrap();
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        let status = spawned.child.wait().unwrap();
        assert!(status.success());
        let output = output.replace('\r', "");
        assert!(output
            .lines()
            .any(|line| line == "FESTERM_SESSIOND_TEST=present"));
        assert!(output.lines().any(|line| line == "TERM=xterm-256color"));
        assert!(output.lines().any(|line| line == "TERM_PROGRAM=fesTerm"));
        assert!(!output.contains("TERM_SESSION_ID="));
    }

    fn unique_test_directory(label: &str) -> PathBuf {
        env::temp_dir().join(format!("fsd-{label}-{}-{}", process::id(), now_ms()))
    }
}
