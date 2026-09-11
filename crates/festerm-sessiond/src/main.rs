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
use festerm_windows_security::named_pipe::{Pipe, PipeListener};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
};

use festerm_pty::default_local_profile;
use festerm_ssh::PersistentSessionName;
use fs2::FileExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

/// Opt-in diagnostic tracing for debugging the Windows native-smoke daemon
/// path (see issue #71). Writes to the file named by
/// `FESTERM_SESSIOND_TRACE_FILE` if set; otherwise a no-op. A file is used
/// instead of stderr because the native-smoke test spawns the daemon with
/// `Stdio::null()` for its own stdio, which would silently discard
/// `eprintln!` output.
fn trace_file() -> Option<&'static Mutex<fs::File>> {
    static TRACE_FILE: OnceLock<Option<Mutex<fs::File>>> = OnceLock::new();
    TRACE_FILE
        .get_or_init(|| {
            let path = env::var_os("FESTERM_SESSIOND_TRACE_FILE")?;
            if path.is_empty() {
                return None;
            }
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
                .map(Mutex::new)
        })
        .as_ref()
}

fn sessiond_trace(message: impl std::fmt::Display) {
    if let Some(file) = trace_file() {
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(file, "{message}");
            let _ = file.flush();
        }
    }
}

const CLIENT_POLL_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(windows)]
const WINDOWS_CLIENT_READ_TIMEOUT: Duration = Duration::from_millis(250);
/// How long shutdown waits for a worker thread before detaching it.
///
/// The pseudoterminal reader can be parked in a blocking read on a handle that
/// only releases when the pseudoterminal closes, so shutdown must never join a
/// worker unconditionally. The process is exiting either way, so detaching a
/// straggler is strictly better than hanging forever.
const WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
/// How long shutdown waits for the shell to be reaped before giving up on it.
const CHILD_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the daemon keeps draining pseudoterminal output after it observes
/// that the shell exited, so the last screenful still reaches the client.
#[cfg(windows)]
const SHELL_EXIT_DRAIN: Duration = Duration::from_millis(250);
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const CLIENT_QUEUE_CAPACITY: usize = 64;
const CLIENT_FRAME_HEADER_BYTES: usize = 9;
const CLIENT_FRAME_MAGIC: &[u8; 4] = b"FSD1";
const CLIENT_FRAME_INPUT: u8 = 1;
const CLIENT_FRAME_RESIZE: u8 = 2;
const MAX_CLIENT_FRAME_BYTES: usize = 64 * 1024;
const REPLAY_CAPACITY_BYTES: usize = 1024 * 1024;

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
    #[serde(default)]
    attached: bool,
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
}

struct SpawnedShell {
    child: Box<dyn portable_pty::Child + Send>,
    /// The pseudoterminal master, kept in an [`Option`] so shutdown can close
    /// it explicitly. On Windows the reader half of a ConPTY only reports end
    /// of file once the pseudoconsole is closed, so a daemon that never drops
    /// this handle can never observe the reader finishing.
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
}

impl SpawnedShell {
    fn master(&self) -> io::Result<&(dyn portable_pty::MasterPty + Send)> {
        match self.master.as_deref() {
            Some(master) => Ok(master),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the session pseudoterminal is closed",
            )),
        }
    }

    /// Closes the pseudoterminal so blocking readers observe end of file.
    fn close_master(&mut self) {
        self.master = None;
    }

    /// Terminates the shell and reaps it, without blocking forever if the
    /// termination request does not take effect.
    fn terminate(&mut self) {
        let _ = self.child.kill();
        let deadline = Instant::now() + CHILD_EXIT_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if Instant::now() >= deadline => return,
                Ok(None) => thread::sleep(CLIENT_POLL_INTERVAL),
            }
        }
    }
}

/// Resizes `master`, reporting a closed pseudoterminal as a broken pipe.
///
/// This takes the master directly rather than a [`SpawnedShell`] so callers can
/// borrow the pseudoterminal and the child process independently.
fn resize_master(
    master: Option<&(dyn portable_pty::MasterPty + Send)>,
    size: PtySize,
) -> io::Result<()> {
    match master {
        Some(master) => master
            .resize(size)
            .map_err(|error| io::Error::other(error.to_string())),
        None => Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the session pseudoterminal is closed",
        )),
    }
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
    let mut cols = None;
    let mut rows = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let Some(value) = args.get(index + 1) else {
            return Err(format!("{command} requires a value for {flag}").into());
        };
        match flag {
            "--name" => set_once(&mut name, value.clone(), command, flag)?,
            "--shell" => set_once(&mut shell, value.clone(), command, flag)?,
            "--arg" => shell_arguments.push(value.clone()),
            "--cwd" => set_once(&mut working_directory, value.clone(), command, flag)?,
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
    Ok((
        name.ok_or_else(|| format!("{command} requires --name <value>"))?,
        match shell {
            Some(executable) => ShellSpec {
                executable,
                arguments: shell_arguments,
                working_directory,
            },
            None => {
                if !shell_arguments.is_empty() || working_directory.is_some() {
                    return Err(format!("{command} requires --shell with --arg or --cwd").into());
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
        })
        .unwrap_or_else(|_| {
            #[cfg(unix)]
            {
                ShellSpec {
                    executable: "/bin/sh".to_owned(),
                    arguments: vec!["-l".to_owned()],
                    working_directory: None,
                }
            }
            #[cfg(windows)]
            {
                ShellSpec {
                    executable: "cmd.exe".to_owned(),
                    arguments: vec!["/Q".to_owned()],
                    working_directory: None,
                }
            }
        })
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
        for argument in &shell.arguments {
            command.arg("--arg").arg(argument);
        }
        if let Some(working_directory) = &shell.working_directory {
            command.arg("--cwd").arg(working_directory);
        }
        command.spawn()?
    };

    // Redirecting the daemon's own stdio (even to NUL) forces Windows to
    // create it with bInheritHandles = TRUE, which duplicates *every*
    // inheritable handle in this process into the daemon — not just the
    // three we explicitly redirect. When this "start" helper's own stdout
    // or stderr was piped by its caller (see `connect_or_start`, which
    // reads `Command::output()` on this very process), that pipe's write
    // end is inheritable, so the long-lived, deliberately detached daemon
    // grandchild would otherwise inherit a duplicate write handle to it.
    // Since the daemon never exits, that duplicate handle never closes,
    // so the caller's blocking read of the pipe (waiting for EOF) hangs
    // forever. Clearing the inherit flag on our own std handles before
    // spawning the daemon prevents that leak.
    #[cfg(windows)]
    festerm_windows_security::disable_std_handle_inheritance();

    #[cfg(windows)]
    let mut daemon = {
        // `CREATE_BREAKAWAY_FROM_JOB` fails with `ERROR_ACCESS_DENIED` (os
        // error 5) whenever our own process is already a member of a job
        // object that does not grant `JOB_OBJECT_LIMIT_BREAKAWAY_OK`. That is
        // not a hypothetical: Cargo places `cargo run`/`cargo test` child
        // processes in exactly such a job on Windows, and other launchers
        // (sandboxes, some IDEs/service managers) do the same. Without a
        // fallback, starting a persistent session from any of those contexts
        // would fail outright instead of degrading gracefully. So: try to
        // break away first (the common case, e.g. launched from the fesTerm
        // GUI or an ordinary shell), and if that specific error occurs,
        // retry without the flag — the daemon will then share our job and
        // may die when it closes, but that is strictly better than refusing
        // to start at all.
        let build_command = |creation_flags: u32| {
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
                .creation_flags(creation_flags);
            for argument in &shell.arguments {
                command.arg("--arg").arg(argument);
            }
            if let Some(working_directory) = &shell.working_directory {
                command.arg("--cwd").arg(working_directory);
            }
            command
        };

        match build_command(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                eprintln!(
                    "festerm-sessiond: could not break away from the current job object \
                     (it likely disallows JOB_OBJECT_LIMIT_BREAKAWAY_OK); starting session \
                     '{name}' without breakaway, so it may not outlive this process's job"
                );
                build_command(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS).spawn()?
            }
            Err(error) => return Err(error.into()),
        }
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
            attached: false,
        };
        if let Err(error) = save_registry_record(record) {
            let _ = spawned.child.kill();
            let _ = spawned.child.wait();
            let _ = fs::remove_file(&socket_path);
            return Err(error);
        }

        let reader = spawned.master()?.try_clone_reader()?;
        let writer = spawned.master()?.take_writer()?;
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
            attached: false,
        };
        if let Err(error) = save_registry_record(record) {
            let _ = spawned.child.kill();
            let _ = spawned.child.wait();
            return Err(error);
        }

        let reader = spawned.master()?.try_clone_reader()?;
        let writer = spawned.master()?.take_writer()?;
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
    let pid = process::id();
    let name_owned = name.to_owned();
    let result = {
        // Borrow the pseudoterminal and the child process separately so the
        // command and shutdown closures below do not both borrow `spawned`.
        let master = spawned.master.as_deref();
        let child = &mut spawned.child;
        session_client_loop(
            listener,
            reader,
            None,
            |command| match command {
                ClientCommand::Input(data) => writer.write_all(&data).and_then(|()| writer.flush()),
                ClientCommand::Resize(size) => resize_master(master, size),
            },
            || {
                let _ = child.kill();
            },
            move |attached| set_registry_attached(&name_owned, pid, attached),
        )
    };
    // Deregister first: a daemon that has stopped serving its session must
    // never stay advertised as resumable while the rest of shutdown runs.
    let deregistered = drop_registry_record(name, process::id());
    spawned.terminate();
    spawned.close_master();
    deregistered?;
    Ok(result?)
}

#[cfg(unix)]
fn session_client_loop<R: Read + Send + 'static>(
    listener: UnixListener,
    reader: R,
    observer: Option<mpsc::Sender<ClientLoopEvent>>,
    mut handle_client_command: impl FnMut(ClientCommand) -> io::Result<()>,
    mut shutdown: impl FnMut(),
    mut on_attach_changed: impl FnMut(bool),
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let (pty_rx, reader_thread) = spawn_pty_reader(reader);
    let (client_input_tx, client_input_rx) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
    let mut active: Option<ActiveClient> = None;
    let mut retired_clients = Vec::new();
    let mut next_generation = 1u64;
    let mut replay = ReplayBuffer::default();
    let mut attached_reported = false;
    let mut pending = PendingOutput::default();
    let result = loop {
        if let Err(error) = reap_client_threads(&mut retired_clients) {
            shutdown();
            break Err(error);
        }
        retire_active_if_finished(&mut active, &mut retired_clients);
        if let Err(error) = accept_unix_clients(
            &listener,
            &mut active,
            &mut retired_clients,
            &replay,
            observer.as_ref(),
            &client_input_tx,
            &mut next_generation,
        ) {
            shutdown();
            break Err(error);
        }
        report_attach_state_change(
            active.is_some(),
            &mut attached_reported,
            &mut on_attach_changed,
        );
        if let Err(error) = handle_pending_client_input(
            &client_input_rx,
            active.as_ref(),
            &mut handle_client_command,
        ) {
            shutdown();
            break Err(error);
        }
        // Output the client has not taken yet must be delivered before any more
        // is consumed from the reader channel, so delivery stays ordered.
        flush_pending_output(&mut active, &mut retired_clients, &mut pending);
        if pending.is_pending() {
            thread::sleep(CLIENT_POLL_INTERVAL);
            continue;
        }
        match pty_rx.recv_timeout(CLIENT_POLL_INTERVAL) {
            Ok(PtyEvent::Data(data)) => {
                if let Err(error) = accept_unix_clients(
                    &listener,
                    &mut active,
                    &mut retired_clients,
                    &replay,
                    observer.as_ref(),
                    &client_input_tx,
                    &mut next_generation,
                ) {
                    shutdown();
                    break Err(error);
                }
                report_attach_state_change(
                    active.is_some(),
                    &mut attached_reported,
                    &mut on_attach_changed,
                );
                replay.push(&data);
                if let Some(observer) = observer.as_ref() {
                    let _ = observer.send(ClientLoopEvent::OutputBuffered);
                }
                send_to_active(&mut active, &mut retired_clients, &mut pending, data);
                report_attach_state_change(
                    active.is_some(),
                    &mut attached_reported,
                    &mut on_attach_changed,
                );
            }
            Ok(PtyEvent::Eof) => {
                send_to_active(
                    &mut active,
                    &mut retired_clients,
                    &mut pending,
                    EXITED_NOTICE_BYTES.to_vec(),
                );
                break Ok(());
            }
            Ok(PtyEvent::Error(error)) => break Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };

    retire_active(&mut active, &mut retired_clients, false);
    report_attach_state_change(false, &mut attached_reported, &mut on_attach_changed);
    for client in retired_clients {
        join_client_thread_within(client, WORKER_JOIN_TIMEOUT)?;
    }
    join_io_thread_within(reader_thread, WORKER_JOIN_TIMEOUT)?;
    result
}

fn report_attach_state_change(
    currently_attached: bool,
    previously_reported: &mut bool,
    on_change: &mut impl FnMut(bool),
) {
    if currently_attached != *previously_reported {
        *previously_reported = currently_attached;
        on_change(currently_attached);
    }
}

#[cfg(unix)]
fn accept_unix_clients(
    listener: &UnixListener,
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    replay: &ReplayBuffer,
    observer: Option<&mpsc::Sender<ClientLoopEvent>>,
    client_input_tx: &mpsc::SyncSender<ClientInput>,
    next_generation: &mut u64,
) -> io::Result<()> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(CLIENT_POLL_INTERVAL))?;
                stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))?;
                replace_active(
                    active,
                    retired_clients,
                    stream,
                    replay,
                    client_input_tx.clone(),
                    next_generation,
                )?;
                if let Some(observer) = observer {
                    let _ = observer.send(ClientLoopEvent::ClientAttached);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
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

fn set_registry_attached(name: &str, pid: u32, attached: bool) {
    let _ = with_registry_lock(|registry: &mut SessionRegistry| {
        if let Some(record) = registry.sessions.get_mut(name) {
            if record.pid == pid {
                record.attached = attached;
            }
        }
        Ok(())
    });
}

#[cfg(windows)]
fn daemon_client_loop_windows<R: Read + Send + 'static>(
    pipe_name: &str,
    initial_listener: PipeListener,
    reader: R,
    mut writer: Box<dyn Write + Send>,
    spawned: &mut SpawnedShell,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (accept_tx, accept_rx) = mpsc::channel::<io::Result<Pipe>>();
    let pipe_name = pipe_name.to_owned();
    let accept_cancelled = Arc::new(AtomicBool::new(false));
    let accept_stopped = Arc::clone(&accept_cancelled);
    let accept_pipe_name = pipe_name.clone();
    let accept_thread = thread::spawn(move || -> io::Result<()> {
        let mut initial_listener = Some(initial_listener);
        while !accept_stopped.load(Ordering::Acquire) {
            let server = match initial_listener.take() {
                Some(listener) => listener.accept(&accept_stopped),
                None => match create_secure_pipe_listener(&accept_pipe_name, false) {
                    Ok(listener) => listener.accept(&accept_stopped),
                    Err(error) => {
                        // Without this the main loop never learns that the
                        // listener is gone, and the daemon lingers with a
                        // registry record but no way to reach it.
                        let forwarded = io::Error::new(
                            error.kind(),
                            format!("named pipe listener could not be created: {error}"),
                        );
                        let _ = accept_tx.send(Err(forwarded));
                        return Err(error);
                    }
                },
            };
            let server = match server {
                Ok(server) => server,
                Err(_) if accept_stopped.load(Ordering::Acquire) => break,
                Err(error) => {
                    let forwarded =
                        io::Error::new(error.kind(), format!("named pipe accept failed: {error}"));
                    let _ = accept_tx.send(Err(forwarded));
                    return Err(error);
                }
            };
            if accept_stopped.load(Ordering::Acquire) {
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
    let mut attached_reported = false;
    let pid = process::id();
    let name_owned = name.to_owned();
    let mut on_attach_changed =
        move |attached: bool| set_registry_attached(&name_owned, pid, attached);
    let mut shell_exited_at: Option<Instant> = None;
    let mut pending = PendingOutput::default();
    let result = loop {
        // Windows keeps the pseudoconsole open for as long as this process
        // holds the master handle, so the pseudoterminal reader never reports
        // end of file when the shell exits. Poll the child directly instead;
        // without this the daemon outlives its shell forever and the launcher
        // keeps offering a session that can be attached but never responds.
        if shell_exited_at.is_none() {
            match spawned.child.try_wait() {
                Ok(Some(status)) => {
                    sessiond_trace(format_args!("shell exited: {status:?}"));
                    shell_exited_at = Some(Instant::now());
                }
                Ok(None) => {}
                Err(error) => break Err(error),
            }
        }
        if shell_exited_at.is_some_and(|exited_at| exited_at.elapsed() >= SHELL_EXIT_DRAIN) {
            send_to_active(
                &mut active,
                &mut retired_clients,
                &mut pending,
                EXITED_NOTICE_BYTES.to_vec(),
            );
            break Ok(());
        }
        if let Err(error) = reap_client_threads(&mut retired_clients) {
            let _ = spawned.child.kill();
            break Err(error);
        }
        retire_active_if_finished(&mut active, &mut retired_clients);
        if let Err(error) = accept_windows_clients(
            &accept_rx,
            &mut active,
            &mut retired_clients,
            &replay,
            &client_input_tx,
            &mut next_generation,
        ) {
            let _ = spawned.child.kill();
            break Err(error);
        }
        report_attach_state_change(
            active.is_some(),
            &mut attached_reported,
            &mut on_attach_changed,
        );
        if let Err(error) =
            handle_pending_client_input(&client_input_rx, active.as_ref(), &mut |command| {
                match command {
                    ClientCommand::Input(data) => {
                        writer.write_all(&data).and_then(|()| writer.flush())
                    }
                    ClientCommand::Resize(size) => resize_master(spawned.master.as_deref(), size),
                }
            })
        {
            let _ = spawned.child.kill();
            break Err(error);
        }
        // Output the client has not taken yet must be delivered before any more
        // is consumed from the reader channel, so delivery stays ordered.
        flush_pending_output(&mut active, &mut retired_clients, &mut pending);
        if pending.is_pending() {
            thread::sleep(CLIENT_POLL_INTERVAL);
            continue;
        }
        match pty_rx.recv_timeout(CLIENT_POLL_INTERVAL) {
            Ok(PtyEvent::Data(data)) => {
                if let Err(error) = accept_windows_clients(
                    &accept_rx,
                    &mut active,
                    &mut retired_clients,
                    &replay,
                    &client_input_tx,
                    &mut next_generation,
                ) {
                    let _ = spawned.child.kill();
                    break Err(error);
                }
                report_attach_state_change(
                    active.is_some(),
                    &mut attached_reported,
                    &mut on_attach_changed,
                );
                replay.push(&data);
                send_to_active(&mut active, &mut retired_clients, &mut pending, data);
                report_attach_state_change(
                    active.is_some(),
                    &mut attached_reported,
                    &mut on_attach_changed,
                );
            }
            Ok(PtyEvent::Eof) => {
                send_to_active(
                    &mut active,
                    &mut retired_clients,
                    &mut pending,
                    EXITED_NOTICE_BYTES.to_vec(),
                );
                break Ok(());
            }
            Ok(PtyEvent::Error(error)) => break Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };

    accept_cancelled.store(true, Ordering::Release);
    sessiond_trace(format_args!("shutdown: loop ended: {result:?}"));
    retire_active(&mut active, &mut retired_clients, false);
    report_attach_state_change(false, &mut attached_reported, &mut on_attach_changed);
    // Deregister before the joins below. A daemon that has stopped serving its
    // session must never stay advertised as resumable, however slowly the rest
    // of shutdown proceeds.
    let deregistered = drop_registry_record(name, process::id());
    // The shell must not outlive the daemon that owns its pseudoterminal, and
    // closing the pseudoconsole is what finally lets the reader see end of
    // file. Both have to happen before any worker thread is joined.
    spawned.terminate();
    spawned.close_master();
    let reader_result = join_io_thread_within(reader_thread, WORKER_JOIN_TIMEOUT);
    let accept_result = join_io_thread_within(accept_thread, WORKER_JOIN_TIMEOUT);
    let client_result = retired_clients
        .into_iter()
        .try_for_each(|client| join_client_thread_within(client, WORKER_JOIN_TIMEOUT));
    sessiond_trace("shutdown: complete");
    deregistered?;
    reader_result?;
    accept_result?;
    client_result?;
    result.map_err(Into::into)
}

#[cfg(windows)]
fn create_secure_pipe_listener(pipe_name: &str, first: bool) -> io::Result<PipeListener> {
    let guard = festerm_windows_security::restrict_default_dacl_to_current_user()?;
    let listener = PipeListener::bind(pipe_name, first)?;
    guard.restore()?;
    Ok(listener)
}

#[cfg(windows)]
fn accept_windows_clients(
    accept_rx: &mpsc::Receiver<io::Result<Pipe>>,
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    replay: &ReplayBuffer,
    client_input_tx: &mpsc::SyncSender<ClientInput>,
    next_generation: &mut u64,
) -> io::Result<()> {
    for stream in accept_rx.try_iter() {
        let mut stream = stream?;
        sessiond_trace(format_args!(
            "accept_windows_clients: new client, replay_empty={}",
            replay.is_empty()
        ));
        stream.set_read_timeout(WINDOWS_CLIENT_READ_TIMEOUT);
        stream.set_write_timeout(CLIENT_WRITE_TIMEOUT);
        replace_active(
            active,
            retired_clients,
            stream,
            replay,
            client_input_tx.clone(),
            next_generation,
        )?;
    }
    Ok(())
}

fn spawn_pty_reader<R: Read + Send + 'static>(
    mut reader: R,
) -> (mpsc::Receiver<PtyEvent>, thread::JoinHandle<io::Result<()>>) {
    let (sender, receiver) = mpsc::channel();
    let thread = thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    sessiond_trace("pty-reader: eof");
                    let _ = sender.send(PtyEvent::Eof);
                    return Ok(());
                }
                Ok(count) => {
                    sessiond_trace(format_args!("pty-reader: read {count} bytes"));
                    if sender
                        .send(PtyEvent::Data(buffer[..count].to_vec()))
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    sessiond_trace(format_args!("pty-reader: error {error}"));
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

#[derive(Debug, PartialEq, Eq)]
enum ClientOutput {
    Data(Vec<u8>),
}

struct ActiveClient {
    generation: u64,
    output: mpsc::SyncSender<ClientOutput>,
    stolen: Arc<AtomicBool>,
    thread: thread::JoinHandle<io::Result<()>>,
}

fn replace_active<S: Read + Write + Send + 'static>(
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    replacement: S,
    replay: &ReplayBuffer,
    input: mpsc::SyncSender<ClientInput>,
    next_generation: &mut u64,
) -> io::Result<()> {
    retire_active(active, retired_clients, true);

    let generation = *next_generation;
    *next_generation = next_generation.wrapping_add(1);
    sessiond_trace(format_args!(
        "replace_active: generation={generation} replay_empty={}",
        replay.is_empty()
    ));
    let (output, output_rx) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
    let stolen = Arc::new(AtomicBool::new(false));
    let worker_stolen = Arc::clone(&stolen);
    let thread = thread::Builder::new()
        .name(format!("festerm-sessiond-client-{generation}"))
        .spawn(move || {
            let result = client_io_loop(replacement, generation, input, output_rx, worker_stolen);
            if let Err(error) = &result {
                sessiond_trace(format_args!(
                    "client_io_loop[{generation}]: worker exited with error: {error}"
                ));
            }
            result
        })?;
    let client = ActiveClient {
        generation,
        output,
        stolen,
        thread,
    };
    if !replay.is_empty()
        && client
            .output
            .try_send(ClientOutput::Data(replay.to_vec()))
            .is_err()
    {
        client.stolen.store(false, Ordering::Release);
        drop(client.output);
        join_io_thread(client.thread)?;
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "new session client could not accept replay",
        ));
    }
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

/// Retires `active` if its I/O thread has already exited on its own (e.g.
/// the client disconnected after reading `Ok(0)`/EOF from the socket).
///
/// [`send_to_active`] is the *other* place a dead client gets noticed, but
/// it only runs when the pty produces new output to relay. A client whose
/// process was killed while its shell sits idle (no output at all) would
/// otherwise never be detected: the socket has already closed and the I/O
/// thread has returned, but nothing pushes data through `client.output` to
/// surface that via a failed `try_send`. Left unnoticed, `active` (and thus
/// the on-disk registry's `attached` flag surfaced by
/// `list_unattached_local_sessions` in `festerm-sessiond`'s library crate)
/// stays `true` forever, hiding a perfectly resumable session from the
/// Launcher's "resume" list. Since this is checked once per main-loop
/// iteration, which runs at least every `CLIENT_POLL_INTERVAL`, a dead
/// client is noticed within one poll interval regardless of pty activity.
fn retire_active_if_finished(
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
) {
    if active
        .as_ref()
        .is_some_and(|client| client.thread.is_finished())
    {
        retire_active(active, retired_clients, false);
    }
}

fn send_to_active(
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    pending: &mut PendingOutput,
    data: Vec<u8>,
) {
    sessiond_trace(format_args!(
        "send_to_active: {} bytes, active_present={}",
        data.len(),
        active.is_some()
    ));
    pending.hold(active.as_ref().map(|client| client.generation), data);
    flush_pending_output(active, retired_clients, pending);
}

/// Output that has been read from the pseudoterminal but not yet accepted by
/// the attached client.
///
/// A client's queue is bounded, so any client that briefly stops reading will
/// fill it: the GUI drains session output on its frame loop, and one long frame
/// (or a burst larger than the queue) is enough. Dropping the client there
/// turned ordinary backpressure into a lost session, so the main loop parks
/// the chunk here instead and stops consuming its reader channel until the
/// client takes it. The separate PTY-reader channel is still unbounded; this
/// limits client delivery, not the daemon's total buffered PTY output.
#[derive(Default)]
struct PendingOutput {
    /// The chunk awaiting delivery, and the client generation it was produced
    /// for. Output parked for a client that has since been replaced is dropped
    /// rather than delivered, because the replacement already received the
    /// replay buffer containing it.
    held: Option<(u64, Vec<u8>)>,
}

impl PendingOutput {
    fn is_pending(&self) -> bool {
        self.held.is_some()
    }

    fn hold(&mut self, generation: Option<u64>, data: Vec<u8>) {
        let Some(generation) = generation else {
            // With no client attached the replay buffer already holds this
            // output, so there is nothing to deliver.
            return;
        };
        debug_assert!(
            self.held.is_none(),
            "the daemon must not read more pseudoterminal output while a chunk is still pending"
        );
        self.held = Some((generation, data));
    }
}

/// Tries once to hand any parked output to the attached client.
///
/// Only a client that is *gone* is retired here. A client that is merely full
/// keeps its session and its place in the stream.
fn flush_pending_output(
    active: &mut Option<ActiveClient>,
    retired_clients: &mut Vec<thread::JoinHandle<io::Result<()>>>,
    pending: &mut PendingOutput,
) {
    let Some((generation, data)) = pending.held.take() else {
        return;
    };
    let Some(client) = active.as_ref() else {
        return;
    };
    if client.generation != generation {
        return;
    }
    match client.output.try_send(ClientOutput::Data(data)) {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(ClientOutput::Data(data))) => {
            pending.held = Some((generation, data));
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            retire_active(active, retired_clients, false);
        }
    }
}

fn handle_pending_client_input(
    input: &mpsc::Receiver<ClientInput>,
    active: Option<&ActiveClient>,
    handle: &mut impl FnMut(ClientCommand) -> io::Result<()>,
) -> io::Result<()> {
    for input in input.try_iter().take(CLIENT_QUEUE_CAPACITY) {
        if active.is_some_and(|client| client.generation == input.generation) {
            handle(input.command)?;
        }
    }
    Ok(())
}

/// Advances an output chunk without preventing the worker from reading input.
/// A timeout or a completed byte budget yields with the offset intact; neither
/// disconnects the client nor repeats bytes already delivered.
fn write_to_client<S: Write>(
    stream: &mut S,
    data: &[u8],
    written: &mut usize,
    stolen: &AtomicBool,
) -> io::Result<bool> {
    let mut budget = MAX_CLIENT_FRAME_BYTES;
    while *written < data.len() {
        if budget == 0 || stolen.load(Ordering::Acquire) {
            return Ok(false);
        }
        let end = data.len().min(*written + budget);
        match stream.write(&data[*written..end]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "session client accepted no bytes",
                ))
            }
            Ok(count) => {
                *written += count;
                budget -= count;
            }
            Err(error) if is_retryable_client_write(&error) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    // Named-pipe flush waits for the peer to drain, without the write timeout.
    #[cfg(not(windows))]
    match stream.flush() {
        Ok(()) => {}
        Err(error) if is_retryable_client_write(&error) => {}
        Err(error) => return Err(error),
    }
    Ok(true)
}

fn is_retryable_client_write(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn client_io_loop<S: Read + Write>(
    mut stream: S,
    generation: u64,
    input: mpsc::SyncSender<ClientInput>,
    output: mpsc::Receiver<ClientOutput>,
    stolen: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut parser = ClientFrameParser::default();
    let mut pending_input = None;
    let mut pending_output = None;
    let mut buffer = [0u8; 4096];
    'client: loop {
        if stolen.load(Ordering::Acquire) {
            stream.write_all(STOLEN_NOTICE_BYTES)?;
            #[cfg(not(windows))]
            stream.flush()?;
            return Ok(());
        }
        loop {
            if stolen.load(Ordering::Acquire) {
                continue 'client;
            }
            let retrying = pending_input.is_some();
            let command = match pending_input.take() {
                Some(command) => command,
                None => match parser.next_command()? {
                    Some(command) => ClientInput {
                        generation,
                        command,
                    },
                    None => break,
                },
            };
            match input.try_send(command) {
                Ok(()) => {
                    if retrying {
                        sessiond_trace(format_args!(
                            "client_io_loop[{generation}]: input queue resumed"
                        ));
                    }
                }
                Err(mpsc::TrySendError::Full(command)) => {
                    // Keep just one decoded command and the bounded parser
                    // buffer. Do not read ahead or block on send: output and
                    // takeover must still progress while the daemon is busy.
                    if !retrying {
                        sessiond_trace(format_args!(
                            "client_io_loop[{generation}]: input queue full; pausing reads"
                        ));
                    }
                    pending_input = Some(command);
                    break;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
            }
        }
        for _ in 0..CLIENT_QUEUE_CAPACITY {
            if stolen.load(Ordering::Acquire) {
                continue 'client;
            }
            if pending_output.is_none() {
                match output.try_recv() {
                    Ok(ClientOutput::Data(data)) => {
                        sessiond_trace(format_args!(
                            "client_io_loop[{generation}]: writing {} bytes to client",
                            data.len()
                        ));
                        pending_output = Some((data, 0));
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                }
            }
            let (data, written) = pending_output
                .as_mut()
                .expect("output is held until written");
            if !write_to_client(&mut stream, data, written, &stolen)? {
                break;
            }
            pending_output = None;
        }
        if pending_input.is_some() {
            thread::sleep(CLIENT_POLL_INTERVAL);
            continue;
        }
        // A maximum-sized frame may end in the same transport read as the
        // next frame. Limit the read, not the combined frames' validity.
        let read_limit = buffer.len().min(parser.remaining_capacity());
        match stream.read(&mut buffer[..read_limit]) {
            Ok(0) => return Ok(()),
            Ok(count) => parser.push(&buffer[..count])?,
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

#[derive(Default)]
struct ClientFrameParser {
    bytes: Vec<u8>,
}

impl ClientFrameParser {
    fn remaining_capacity(&self) -> usize {
        MAX_CLIENT_FRAME_BYTES + CLIENT_FRAME_HEADER_BYTES - self.bytes.len()
    }

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

    fn next_command(&mut self) -> io::Result<Option<ClientCommand>> {
        if self.bytes.len() < CLIENT_FRAME_HEADER_BYTES {
            return Ok(None);
        }
        if &self.bytes[..CLIENT_FRAME_MAGIC.len()] != CLIENT_FRAME_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session client frame has an invalid magic value",
            ));
        }
        let kind = self.bytes[4];
        let payload_len =
            u32::from_be_bytes(self.bytes[5..9].try_into().expect("fixed frame header")) as usize;
        if payload_len > MAX_CLIENT_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session client frame payload exceeds the protocol limit",
            ));
        }
        let frame_len = CLIENT_FRAME_HEADER_BYTES + payload_len;
        if self.bytes.len() < frame_len {
            return Ok(None);
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
        Ok(Some(command))
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

/// Joins `thread` if it finishes within `timeout`, and otherwise detaches it.
///
/// Shutdown runs immediately before the process exits, so a straggling worker
/// costs nothing once it is abandoned. Blocking on it, by contrast, strands the
/// daemon: it stops serving clients but never deregisters, so the launcher keeps
/// advertising a session that can no longer be attached.
fn join_io_thread_within(
    thread: thread::JoinHandle<io::Result<()>>,
    timeout: Duration,
) -> io::Result<()> {
    if wait_for_thread(&thread, timeout) {
        return join_io_thread(thread);
    }
    sessiond_trace("shutdown: detaching a worker thread that did not finish");
    Ok(())
}

fn join_client_thread_within(
    thread: thread::JoinHandle<io::Result<()>>,
    timeout: Duration,
) -> io::Result<()> {
    if wait_for_thread(&thread, timeout) {
        return join_client_thread(thread);
    }
    sessiond_trace("shutdown: detaching a client thread that did not finish");
    Ok(())
}

fn wait_for_thread(thread: &thread::JoinHandle<io::Result<()>>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(CLIENT_POLL_INTERVAL);
    }
    true
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
}

impl Default for ReplayBuffer {
    fn default() -> Self {
        Self {
            bytes: VecDeque::with_capacity(REPLAY_CAPACITY_BYTES),
            capacity: REPLAY_CAPACITY_BYTES,
        }
    }
}

impl ReplayBuffer {
    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn to_vec(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    fn push(&mut self, data: &[u8]) {
        if data.len() >= self.capacity {
            self.bytes.clear();
            self.bytes.extend(
                data[data.len().saturating_sub(self.capacity)..]
                    .iter()
                    .copied(),
            );
            return;
        }

        let overflow = self
            .bytes
            .len()
            .saturating_add(data.len())
            .saturating_sub(self.capacity);
        self.bytes.drain(..overflow);
        self.bytes.extend(data.iter().copied());
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

    println!("name\tpid\tsocket\tshell\tattached");
    for record in registry.sessions.values() {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            record.name, record.pid, record.socket, record.shell, record.attached
        );
    }
    Ok(())
}

fn run_kill(name: String) -> Result<(), Box<dyn std::error::Error>> {
    let name = validate_name(name)?;
    with_registry_lock(|registry: &mut SessionRegistry| {
        kill_registered_session(registry, &name, terminate_pid)
    })
}

fn kill_registered_session(
    registry: &mut SessionRegistry,
    name: &str,
    terminate: impl FnOnce(u32) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(record) = registry.sessions.get(name).cloned() else {
        return Err(format!("session '{name}' is not registered").into());
    };
    let terminated = if process_alive(record.pid) {
        terminate(record.pid)
    } else {
        Ok(())
    };
    // Drop the record even when termination failed. Leaving it behind is what
    // turns an unresponsive daemon into a session the Launcher keeps offering
    // and no client can ever attach to, with no way back short of editing the
    // registry by hand.
    remove_registry_record_if_pid_matches(registry, name, record.pid);
    terminated
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
        forward_attach_duplex(&mut stream, &mut io::stdout())?
    };

    #[cfg(windows)]
    let outcome = {
        let mut stream =
            Pipe::connect(&record.socket, WORKER_JOIN_TIMEOUT, &AtomicBool::new(false))?;
        stream.set_read_timeout(CLIENT_POLL_INTERVAL);
        stream.set_write_timeout(CLIENT_WRITE_TIMEOUT);
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
    {
        let selection = festerm_pty::prepare_windows_conpty_runtime()?;
        sessiond_trace(format_args!(
            "spawn_shell: conpty runtime selection = {selection:?}"
        ));
    }

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
    command.env("TERM", "xterm-256color");
    let child = pair.slave.spawn_command(command)?;
    let master = pair.master;
    Ok(SpawnedShell {
        child,
        master: Some(master),
    })
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
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct ClientTestStream<F> {
        input: io::Cursor<Vec<u8>>,
        reads: Rc<Cell<usize>>,
        output_on_read: Option<mpsc::SyncSender<ClientOutput>>,
        on_write: F,
    }

    impl<F> Read for ClientTestStream<F> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads.set(self.reads.get() + 1);
            if let Some(output) = self.output_on_read.take() {
                output
                    .try_send(ClientOutput::Data(b"duplex output".to_vec()))
                    .unwrap();
            }
            self.input.read(buffer)
        }
    }

    impl<F: FnMut(&[u8])> Write for ClientTestStream<F> {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            (self.on_write)(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn client_input_burst_waits_in_order_while_output_keeps_flowing() {
        let mut frames = Vec::new();
        let count = CLIENT_QUEUE_CAPACITY + 17;
        for index in 0..count {
            if index % 2 == 0 {
                write_client_frame(&mut frames, CLIENT_FRAME_INPUT, &[index as u8]).unwrap();
            } else {
                let payload: Vec<_> = [80 + index as u16, 24, 800, 600]
                    .into_iter()
                    .flat_map(u16::to_be_bytes)
                    .collect();
                write_client_frame(&mut frames, CLIENT_FRAME_RESIZE, &payload).unwrap();
            }
        }
        assert!(frames.len() < 4096, "the burst must fit in one read");
        let (input, commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let (output, received_output) = mpsc::sync_channel(1);
        let mut accepted = Vec::new();
        let mut written = Vec::new();
        let reads = Rc::new(Cell::new(0));
        let stream = ClientTestStream {
            input: io::Cursor::new(frames),
            reads: Rc::clone(&reads),
            output_on_read: Some(output.clone()),
            on_write: |bytes: &[u8]| {
                // No daemon command is consumed until the full input queue
                // has allowed the worker to return to its output pump.
                written.extend_from_slice(bytes);
                assert_eq!(reads.get(), 1, "a full input queue must stop reads");
                accepted.extend(commands.try_iter());
                assert_eq!(accepted.len(), CLIENT_QUEUE_CAPACITY);
            },
        };

        client_io_loop(
            stream,
            7,
            input,
            received_output,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();

        assert_eq!(written, b"duplex output");
        accepted.extend(commands.try_iter());
        assert_eq!(accepted.len(), count, "no command may be lost");
        for (index, input) in accepted.into_iter().enumerate() {
            assert_eq!(input.generation, 7);
            match input.command {
                ClientCommand::Input(bytes) => {
                    assert_eq!(index % 2, 0);
                    assert_eq!(bytes, [index as u8]);
                }
                ClientCommand::Resize(size) => {
                    assert_eq!(index % 2, 1);
                    assert_eq!(size.cols, 80 + index as u16);
                    assert_eq!(size.rows, 24);
                    assert_eq!(size.pixel_width, 800);
                    assert_eq!(size.pixel_height, 600);
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn client_input_coalesced_burst_keeps_native_socket_attached() {
        let (mut client, worker_socket) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))
            .unwrap();
        worker_socket
            .set_read_timeout(Some(CLIENT_POLL_INTERVAL))
            .unwrap();
        worker_socket
            .set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))
            .unwrap();
        let mut burst = Vec::new();
        for _ in 0..256 {
            write_client_frame(&mut burst, CLIENT_FRAME_INPUT, b"x").unwrap();
        }
        let marker = b"\nINPUT-BURST-COMPLETE\n";
        write_client_frame(&mut burst, CLIENT_FRAME_INPUT, marker).unwrap();
        client.write_all(&burst).unwrap();

        let (input, commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let (output, received_output) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let stolen = Arc::new(AtomicBool::new(false));
        let worker_stolen = Arc::clone(&stolen);
        let worker = thread::spawn(move || {
            client_io_loop(worker_socket, 7, input, received_output, worker_stolen)
        });
        let expect_input = |expected: &[u8]| {
            let input = commands.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(input.generation, 7);
            assert!(matches!(
                input.command,
                ClientCommand::Input(bytes) if bytes == expected
            ));
        };
        expect_input(b"x");
        // Withhold the command consumer until duplex output reaches the
        // client, so a blocking send cannot masquerade as successful retry.
        let duplex = b"output while input is queued";
        output.send(ClientOutput::Data(duplex.to_vec())).unwrap();
        let mut received = vec![0; duplex.len()];
        client.read_exact(&mut received).unwrap();
        assert_eq!(received, duplex);
        for _ in 1..256 {
            expect_input(b"x");
        }
        expect_input(marker);

        write_client_frame(&mut client, CLIENT_FRAME_INPUT, b"still attached").unwrap();
        expect_input(b"still attached");
        stolen.store(true, Ordering::Release);
        let mut notice = vec![0; STOLEN_NOTICE_BYTES.len()];
        client.read_exact(&mut notice).unwrap();
        assert_eq!(notice, STOLEN_NOTICE_BYTES);
        assert!(wait_for_thread(&worker, Duration::from_secs(2)));
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn client_input_is_read_while_client_output_writes_are_blocked() {
        struct DuplexStream {
            input: io::Cursor<Vec<u8>>,
            commands: mpsc::Receiver<ClientInput>,
            written: Vec<u8>,
            stalled_writes: usize,
            input_received: bool,
        }

        impl Read for DuplexStream {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.input.read(buffer)
            }
        }

        impl Write for DuplexStream {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.written.is_empty() {
                    self.written.extend_from_slice(&bytes[..2]);
                    return Ok(2);
                }
                if !self.input_received {
                    match self.commands.try_recv() {
                        Ok(command) => {
                            assert!(matches!(
                                command.command,
                                ClientCommand::Input(input) if input == b"\x03"
                            ));
                            self.input_received = true;
                        }
                        Err(_) => {
                            self.stalled_writes += 1;
                            assert!(
                                self.stalled_writes < 3,
                                "output retry must yield to read the interrupt"
                            );
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                    }
                }
                self.written.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut frames = Vec::new();
        write_client_frame(&mut frames, CLIENT_FRAME_INPUT, b"\x03").unwrap();
        let (input, commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let (output, received_output) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        output
            .send(ClientOutput::Data(b"uninterrupted output".to_vec()))
            .unwrap();
        let mut stream = DuplexStream {
            input: io::Cursor::new(frames),
            commands,
            written: Vec::new(),
            stalled_writes: 0,
            input_received: false,
        };
        client_io_loop(
            &mut stream,
            7,
            input,
            received_output,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(stream.written, b"uninterrupted output");
        assert!(stream.input_received);
    }

    #[test]
    fn client_input_batches_yield_even_when_the_producer_keeps_refilling() {
        let (input, commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let command = || ClientInput {
            generation: 7,
            command: ClientCommand::Input(b"x".to_vec()),
        };
        for _ in 0..CLIENT_QUEUE_CAPACITY {
            input.send(command()).unwrap();
        }
        let (output, _received_output) = mpsc::sync_channel(1);
        let active = ActiveClient {
            generation: 7,
            output,
            stolen: Arc::new(AtomicBool::new(false)),
            thread: thread::spawn(|| Ok(())),
        };
        let mut handled = 0;
        handle_pending_client_input(&commands, Some(&active), &mut |_| {
            handled += 1;
            assert!(handled <= CLIENT_QUEUE_CAPACITY);
            input.try_send(command()).unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(handled, CLIENT_QUEUE_CAPACITY);
        assert_eq!(commands.try_iter().count(), CLIENT_QUEUE_CAPACITY);
        active.thread.join().unwrap().unwrap();
    }

    #[test]
    fn client_input_backpressure_stops_reading_and_allows_takeover() {
        let mut frames = Vec::new();
        for _ in 0..1000 {
            write_client_frame(&mut frames, CLIENT_FRAME_INPUT, b"x").unwrap();
        }
        assert!(frames.len() > 4096);
        let (input, commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let probe = input.clone();
        let (output, received_output) = mpsc::sync_channel(1);
        let stolen = Arc::new(AtomicBool::new(false));
        let reads = Rc::new(Cell::new(0));
        let mut written = Vec::new();
        let stream = ClientTestStream {
            input: io::Cursor::new(frames),
            reads: Rc::clone(&reads),
            output_on_read: Some(output.clone()),
            on_write: |bytes: &[u8]| {
                written.extend_from_slice(bytes);
                assert_eq!(reads.get(), 1, "backpressure must not read ahead");
                assert!(matches!(
                    probe.try_send(ClientInput {
                        generation: 7,
                        command: ClientCommand::Input(b"must not fit".to_vec()),
                    }),
                    Err(mpsc::TrySendError::Full(_))
                ));
                stolen.store(true, Ordering::Release);
            },
        };

        client_io_loop(stream, 7, input, received_output, Arc::clone(&stolen)).unwrap();

        assert_eq!(
            written,
            [b"duplex output".as_slice(), STOLEN_NOTICE_BYTES].concat()
        );
        assert_eq!(commands.try_iter().count(), CLIENT_QUEUE_CAPACITY);
    }

    #[test]
    fn client_input_backpressure_exits_when_daemon_channels_close() {
        for close_input in [false, true] {
            let mut frames = Vec::new();
            for _ in 0..CLIENT_QUEUE_CAPACITY + 1 {
                write_client_frame(&mut frames, CLIENT_FRAME_INPUT, b"x").unwrap();
            }
            let (input, commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
            let (output, received_output) = mpsc::sync_channel(1);
            let mut commands = Some(commands);
            let mut output = Some(output);
            let reads = Rc::new(Cell::new(0));
            let mut written = Vec::new();
            let stream = ClientTestStream {
                input: io::Cursor::new(frames),
                reads: Rc::clone(&reads),
                output_on_read: output.clone(),
                on_write: |bytes: &[u8]| {
                    written.extend_from_slice(bytes);
                    if close_input {
                        commands.take();
                    } else {
                        output.take();
                    }
                },
            };

            client_io_loop(
                stream,
                7,
                input,
                received_output,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();

            assert_eq!(written, b"duplex output");
            assert_eq!(reads.get(), 1);
        }
    }

    #[test]
    fn client_input_retry_is_not_starved_by_continuous_output() {
        let mut frames = Vec::new();
        for _ in 0..CLIENT_QUEUE_CAPACITY + 1 {
            write_client_frame(&mut frames, CLIENT_FRAME_INPUT, b"x").unwrap();
        }
        let (input, commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let (output, received_output) = mpsc::sync_channel(1);
        let reads = Rc::new(Cell::new(0));
        let mut writes = 0;
        let stream = ClientTestStream {
            input: io::Cursor::new(frames),
            reads: Rc::clone(&reads),
            output_on_read: Some(output.clone()),
            on_write: |_: &[u8]| {
                writes += 1;
                assert_eq!(reads.get(), 1);
                if writes == 1 {
                    assert_eq!(commands.try_iter().count(), CLIENT_QUEUE_CAPACITY);
                } else if writes == CLIENT_QUEUE_CAPACITY + 1 {
                    assert_eq!(
                        commands.try_iter().count(),
                        1,
                        "pending input must retry even when output never becomes empty"
                    );
                }
                assert!(writes <= 2 * CLIENT_QUEUE_CAPACITY);
                output
                    .try_send(ClientOutput::Data(b"more output".to_vec()))
                    .unwrap();
            },
        };

        client_io_loop(
            stream,
            7,
            input,
            received_output,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();

        assert_eq!(writes, 2 * CLIENT_QUEUE_CAPACITY);
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn client_input_maximum_frame_accepts_a_coalesced_following_frame() {
        let payload = vec![b'x'; MAX_CLIENT_FRAME_BYTES];
        let mut frames = Vec::new();
        write_client_frame(&mut frames, CLIENT_FRAME_INPUT, &payload).unwrap();
        write_client_frame(&mut frames, CLIENT_FRAME_INPUT, b"next").unwrap();
        let (input, commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let (_output, received_output) = mpsc::sync_channel(1);

        client_io_loop(
            io::Cursor::new(frames),
            7,
            input,
            received_output,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();

        let accepted: Vec<_> = commands.try_iter().collect();
        assert_eq!(accepted.len(), 2);
        for (command, expected) in accepted.into_iter().zip([payload, b"next".to_vec()]) {
            match command.command {
                ClientCommand::Input(bytes) => assert_eq!(bytes, expected),
                other => panic!("expected input, received {other:?}"),
            }
        }
    }

    #[test]
    fn client_input_parser_preserves_fragmented_frames_and_rejects_invalid_frames() {
        let mut frame = Vec::new();
        write_client_frame(&mut frame, CLIENT_FRAME_INPUT, b"typed").unwrap();
        let mut parser = ClientFrameParser::default();
        for byte in &frame[..frame.len() - 1] {
            parser.push(&[*byte]).unwrap();
            assert!(parser.next_command().unwrap().is_none());
        }
        parser.push(&frame[frame.len() - 1..]).unwrap();
        assert!(matches!(
            parser.next_command().unwrap(),
            Some(ClientCommand::Input(bytes)) if bytes == b"typed"
        ));
        assert!(parser.next_command().unwrap().is_none());
        assert_eq!(
            parser.remaining_capacity(),
            MAX_CLIENT_FRAME_BYTES + CLIENT_FRAME_HEADER_BYTES
        );

        let mut invalid_magic = frame.clone();
        invalid_magic[0] = b'?';
        let mut oversized = frame[..CLIENT_FRAME_HEADER_BYTES].to_vec();
        oversized[5..9].copy_from_slice(&((MAX_CLIENT_FRAME_BYTES + 1) as u32).to_be_bytes());
        let mut unknown_kind = frame;
        unknown_kind[4] = 255;
        let mut short_resize = Vec::new();
        write_client_frame(&mut short_resize, CLIENT_FRAME_RESIZE, &[0; 7]).unwrap();
        let mut invalid_resize = Vec::new();
        write_client_frame(&mut invalid_resize, CLIENT_FRAME_RESIZE, &[0; 8]).unwrap();
        for invalid in [
            invalid_magic,
            oversized,
            unknown_kind,
            short_resize,
            invalid_resize,
        ] {
            let mut parser = ClientFrameParser::default();
            parser.push(&invalid).unwrap();
            assert_eq!(
                parser.next_command().unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        let mut parser = ClientFrameParser::default();
        let oversized_buffer = vec![0; MAX_CLIENT_FRAME_BYTES + CLIENT_FRAME_HEADER_BYTES + 1];
        assert_eq!(
            parser.push(&oversized_buffer).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

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
                |_attached| {},
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
        let mut stolen = vec![0; STOLEN_NOTICE_BYTES.len()];
        first.read_exact(&mut stolen).unwrap();
        assert_eq!(stolen, STOLEN_NOTICE_BYTES);
        let mut eof = [0u8; 1];
        assert_eq!(first.read(&mut eof).unwrap(), 0);

        let mut second_replay = [0u8; 13];
        second.read_exact(&mut second_replay).unwrap();
        assert_eq!(&second_replay, b"detachedfirst");
        let mut resize = Vec::new();
        for value in [120u16, 40, 1200, 800] {
            resize.extend_from_slice(&value.to_be_bytes());
        }
        write_client_frame(&mut second, CLIENT_FRAME_RESIZE, &resize).unwrap();
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

        drop(pty_sender);
        let mut exited = vec![0; EXITED_NOTICE_BYTES.len()];
        second.read_exact(&mut exited).unwrap();
        assert_eq!(exited, EXITED_NOTICE_BYTES);
        assert_eq!(second.read(&mut eof).unwrap(), 0);
        service.join().unwrap().unwrap();
        let _ = fs::remove_dir_all(directory);
    }

    /// Regression test for a client that disappears (crashes/is killed)
    /// while its shell sits completely idle, i.e. with no pty output at all
    /// after the disconnect. `send_to_active`'s failed-`try_send` detection
    /// only runs when there is data to relay, so before
    /// `retire_active_if_finished` was added the daemon would keep
    /// reporting `attached: true` forever in this scenario, hiding an
    /// otherwise-resumable session from `list_unattached_local_sessions`.
    #[cfg(unix)]
    #[test]
    fn idle_client_disconnect_is_detected_without_pty_output() {
        use std::{
            io::{self, Read},
            os::unix::net::{UnixListener, UnixStream},
            sync::{mpsc, Mutex},
        };

        let directory = unique_test_directory("idle-disconnect");
        fs::create_dir_all(&directory).unwrap();
        let socket_path = directory.join("session.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (_pty_sender, pty_receiver) = mpsc::channel::<Vec<u8>>();
        let attach_events: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let attach_events_clone = Arc::clone(&attach_events);

        struct ChannelReader {
            receiver: mpsc::Receiver<Vec<u8>>,
        }

        impl Read for ChannelReader {
            fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
                match self.receiver.recv() {
                    Ok(_) => unreachable!("this test never sends pty output"),
                    Err(_) => Ok(0),
                }
            }
        }

        let service = thread::spawn(move || {
            session_client_loop(
                listener,
                ChannelReader {
                    receiver: pty_receiver,
                },
                None,
                |_command| Ok(()),
                || {},
                move |attached| attach_events_clone.lock().unwrap().push(attached),
            )
        });

        let client = UnixStream::connect(&socket_path).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while attach_events.lock().unwrap().as_slice() != [true] {
            if std::time::Instant::now() > deadline {
                panic!("daemon never reported the client as attached");
            }
            thread::sleep(Duration::from_millis(5));
        }

        // Simulate the client process crashing/being killed: the socket
        // closes with no further protocol activity and the shell stays
        // idle (no pty output is ever sent on `_pty_sender`).
        drop(client);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while attach_events.lock().unwrap().as_slice() != [true, false] {
            if std::time::Instant::now() > deadline {
                panic!(
                    "idle client disconnect was never detected; attach events: {:?}",
                    attach_events.lock().unwrap()
                );
            }
            thread::sleep(Duration::from_millis(5));
        }

        drop(_pty_sender);
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
                    attached: false,
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
        };
        replay.push(b"abc");
        replay.push(b"defg");
        assert_eq!(replay.bytes.iter().copied().collect::<Vec<_>>(), b"cdefg");
        replay.push(b"1234567");
        assert_eq!(replay.bytes.iter().copied().collect::<Vec<_>>(), b"34567");
    }

    /// A client that is gone must be retired so its worker can be joined.
    #[test]
    fn output_for_a_departed_client_is_retired_for_worker_join() {
        let (output, receiver) = mpsc::sync_channel(1);
        // The worker has ended, so nothing will ever receive again.
        drop(receiver);
        let mut active = Some(ActiveClient {
            generation: 1,
            output,
            stolen: Arc::new(AtomicBool::new(false)),
            thread: thread::spawn(|| -> io::Result<()> {
                panic!("test client worker panic");
            }),
        });
        let mut retired = Vec::new();
        let mut pending = PendingOutput::default();

        send_to_active(&mut active, &mut retired, &mut pending, b"output".to_vec());

        assert!(active.is_none());
        assert!(!pending.is_pending());
        assert_eq!(retired.len(), 1);
        assert!(join_client_thread(retired.pop().unwrap()).is_err());
    }

    /// A client that is merely slow must keep its session.
    ///
    /// The GUI drains output on its frame loop, so a long frame fills the
    /// bounded queue. Retiring the client there disconnected users in the
    /// middle of an active session; the output has to wait instead.
    #[test]
    fn output_for_a_full_client_waits_instead_of_dropping_the_session() {
        let (output, receiver) = mpsc::sync_channel(1);
        let mut active = Some(ActiveClient {
            generation: 1,
            output,
            stolen: Arc::new(AtomicBool::new(false)),
            thread: thread::spawn(|| -> io::Result<()> { Ok(()) }),
        });
        let mut retired = Vec::new();
        let mut pending = PendingOutput::default();

        send_to_active(&mut active, &mut retired, &mut pending, b"first".to_vec());
        assert!(!pending.is_pending(), "the first chunk fits in the queue");

        // The queue is now full, so this chunk has nowhere to go yet.
        send_to_active(&mut active, &mut retired, &mut pending, b"second".to_vec());
        assert!(pending.is_pending());
        assert!(active.is_some(), "a slow client must keep its session");
        assert!(retired.is_empty());

        // Once the client reads, the parked chunk is delivered in order.
        assert_eq!(
            receiver.recv().unwrap(),
            ClientOutput::Data(b"first".into())
        );
        flush_pending_output(&mut active, &mut retired, &mut pending);
        assert!(!pending.is_pending());
        assert_eq!(
            receiver.recv().unwrap(),
            ClientOutput::Data(b"second".into())
        );
    }

    /// Output parked for a client that has since been replaced must be dropped:
    /// the replacement is sent the replay buffer, which already contains it.
    #[test]
    fn output_parked_for_a_replaced_client_is_not_delivered_twice() {
        let (output, receiver) = mpsc::sync_channel(1);
        let mut active = Some(ActiveClient {
            generation: 1,
            output,
            stolen: Arc::new(AtomicBool::new(false)),
            thread: thread::spawn(|| -> io::Result<()> { Ok(()) }),
        });
        let mut retired = Vec::new();
        let mut pending = PendingOutput::default();

        send_to_active(&mut active, &mut retired, &mut pending, b"first".to_vec());
        send_to_active(&mut active, &mut retired, &mut pending, b"second".to_vec());
        assert!(pending.is_pending());

        let (replacement, replacement_receiver) = mpsc::sync_channel(4);
        active = Some(ActiveClient {
            generation: 2,
            output: replacement,
            stolen: Arc::new(AtomicBool::new(false)),
            thread: thread::spawn(|| -> io::Result<()> { Ok(()) }),
        });

        flush_pending_output(&mut active, &mut retired, &mut pending);

        assert!(!pending.is_pending());
        assert!(replacement_receiver.try_recv().is_err());
        drop(receiver);
    }

    /// A write that times out is backpressure, not a broken client.
    #[test]
    fn a_client_write_that_times_out_resumes_where_it_stopped() {
        #[derive(Default)]
        struct StallingWriter {
            written: Vec<u8>,
            attempts: usize,
        }

        impl Write for StallingWriter {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                self.attempts += 1;
                match self.attempts {
                    // Accept a little, then stall twice, then accept the rest.
                    1 => {
                        self.written.extend_from_slice(&buffer[..2]);
                        Ok(2)
                    }
                    2 => Err(io::Error::new(io::ErrorKind::TimedOut, "slow client")),
                    3 => Err(io::Error::new(io::ErrorKind::WouldBlock, "slow client")),
                    _ => {
                        self.written.extend_from_slice(buffer);
                        Ok(buffer.len())
                    }
                }
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = StallingWriter::default();
        let stolen = AtomicBool::new(false);

        let mut written = 0;
        while !write_to_client(&mut writer, b"abcdef", &mut written, &stolen).unwrap() {}

        assert_eq!(writer.written, b"abcdef", "no byte is written twice");
        assert_eq!(written, 6);
    }

    #[test]
    fn a_client_output_batch_yields_with_its_offset_intact() {
        let data = vec![b'x'; MAX_CLIENT_FRAME_BYTES + 17];
        let mut wire = Vec::new();
        let mut written = 0;
        let stolen = AtomicBool::new(false);
        assert!(!write_to_client(&mut wire, &data, &mut written, &stolen).unwrap());
        assert_eq!(written, MAX_CLIENT_FRAME_BYTES);
        assert!(write_to_client(&mut wire, &data, &mut written, &stolen).unwrap());
        assert_eq!(wire, data);
    }

    #[cfg(windows)]
    #[test]
    fn retired_windows_clients_finish_even_when_their_output_is_never_read() {
        for index in 0..4 {
            let name = format!(
                r"\\.\pipe\festerm-retired-client-{}-{}-{index}",
                process::id(),
                now_ms()
            );
            let listener = create_secure_pipe_listener(&name, true).unwrap();
            let _nonreading_client =
                Pipe::connect(&name, Duration::from_secs(1), &AtomicBool::new(false)).unwrap();
            let mut server = listener.accept(&AtomicBool::new(false)).unwrap();
            server.write_all(b"unread output before takeover").unwrap();
            let (input, _commands) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
            let mut active = None;
            let mut retired = Vec::new();
            replace_active(
                &mut active,
                &mut retired,
                server,
                &ReplayBuffer::default(),
                input,
                &mut 1,
            )
            .unwrap();
            retire_active(&mut active, &mut retired, true);
            assert!(wait_for_thread(&retired[0], WORKER_JOIN_TIMEOUT));
            reap_client_threads(&mut retired).unwrap();
            assert!(retired.is_empty());
        }
    }

    /// A takeover must not be blocked by a client that refuses to read.
    #[test]
    fn a_client_write_is_abandoned_once_the_session_is_stolen() {
        struct NeverAcceptingWriter;

        impl Write for NeverAcceptingWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::TimedOut, "wedged client"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let stolen = AtomicBool::new(true);

        assert!(!write_to_client(&mut NeverAcceptingWriter, b"abcdef", &mut 0, &stolen).unwrap());
    }

    #[test]
    fn valid_names_are_accepted() {
        assert_eq!(validate_name("demo-01".to_owned()).unwrap(), "demo-01");
        assert_eq!(validate_name("session_2".to_owned()).unwrap(), "session_2");
    }

    /// Shutdown used to join the pseudoterminal reader unconditionally. On
    /// Windows that reader is parked in a blocking read on a ConPTY handle
    /// that only releases when the pseudoconsole closes -- which cannot happen
    /// until shutdown returns -- so the daemon hung forever with a live
    /// registry record and no listener, and the Launcher kept offering a
    /// session nothing could attach to.
    #[test]
    fn a_worker_that_never_finishes_is_detached_so_shutdown_cannot_hang() {
        let (release, blocked) = mpsc::channel::<()>();
        let worker = thread::spawn(move || -> io::Result<()> {
            let _ = blocked.recv();
            Ok(())
        });

        let started = Instant::now();
        let result = join_io_thread_within(worker, Duration::from_millis(100));

        assert!(result.is_ok());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "shutdown waited {:?} on a worker that never finishes",
            started.elapsed()
        );
        drop(release);
    }

    #[test]
    fn a_worker_that_finishes_in_time_still_reports_its_failure() {
        let worker = thread::spawn(|| -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "worker failed"))
        });

        let error = join_io_thread_within(worker, Duration::from_secs(5)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn a_client_worker_that_never_finishes_is_detached_so_shutdown_cannot_hang() {
        let (release, blocked) = mpsc::channel::<()>();
        let worker = thread::spawn(move || -> io::Result<()> {
            let _ = blocked.recv();
            Ok(())
        });

        assert!(join_client_thread_within(worker, Duration::from_millis(100)).is_ok());
        drop(release);
    }

    /// A client worker that fails its own I/O is expected: the client simply
    /// went away. Only a panic is a daemon bug worth surfacing.
    #[test]
    fn a_client_worker_that_finishes_in_time_ignores_its_io_failure_but_not_a_panic() {
        let failed = thread::spawn(|| -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "client went away",
            ))
        });
        assert!(join_client_thread_within(failed, Duration::from_secs(5)).is_ok());

        let panicked = thread::spawn(|| -> io::Result<()> {
            panic!("test client worker panic");
        });
        assert!(join_client_thread_within(panicked, Duration::from_secs(5)).is_err());
    }

    #[test]
    fn invalid_names_are_rejected() {
        let err = validate_name("bad/name".to_owned()).unwrap_err();
        assert!(err.to_string().contains("persistent session name"));
    }

    /// `kill` used to abandon the registry entry when terminating the process
    /// failed, which is exactly the case where the entry is most harmful: an
    /// unresponsive daemon stayed advertised with no supported way to clear it.
    #[test]
    fn killing_a_session_drops_its_record_even_when_termination_fails() {
        let mut registry = SessionRegistry::default();
        registry
            .sessions
            .insert("demo".to_owned(), test_record("demo", process::id()));

        let error = kill_registered_session(&mut registry, "demo", |_| {
            Err("access is denied".to_owned().into())
        })
        .unwrap_err();

        assert!(error.to_string().contains("access is denied"));
        assert!(
            registry.sessions.is_empty(),
            "a session that could not be terminated must still be deregistered"
        );
    }

    #[test]
    fn killing_an_unregistered_session_reports_that_it_is_not_registered() {
        let mut registry = SessionRegistry::default();

        let error = kill_registered_session(&mut registry, "missing", |_| {
            panic!("an unregistered session must not be terminated")
        })
        .unwrap_err();

        assert!(error.to_string().contains("is not registered"));
    }

    fn test_record(name: &str, pid: u32) -> SessionRecord {
        SessionRecord {
            name: name.to_owned(),
            pid,
            socket: format!("/tmp/festerm-sessiond/{name}.sock"),
            shell: "/bin/bash".to_owned(),
            arguments: Vec::new(),
            working_directory: None,
            cols: 80,
            rows: 24,
            created_at_unix_ms: 1_700_000_000_000,
            attached: false,
        }
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
            attached: true,
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
            },
            80,
            24,
        )
        .unwrap();
        let mut reader = spawned.master().unwrap().try_clone_reader().unwrap();
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
    fn unique_test_directory(label: &str) -> PathBuf {
        env::temp_dir().join(format!("fsd-{label}-{}-{}", process::id(), now_ms()))
    }

    #[test]
    fn attach_state_change_reports_only_on_transition() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut reported = false;
        let events_clone = Rc::clone(&events);
        let mut on_change = move |attached: bool| events_clone.borrow_mut().push(attached);

        report_attach_state_change(false, &mut reported, &mut on_change);
        assert!(events.borrow().is_empty());

        report_attach_state_change(true, &mut reported, &mut on_change);
        assert_eq!(*events.borrow(), vec![true]);

        report_attach_state_change(true, &mut reported, &mut on_change);
        assert_eq!(*events.borrow(), vec![true]);

        report_attach_state_change(false, &mut reported, &mut on_change);
        assert_eq!(*events.borrow(), vec![true, false]);
    }
}
