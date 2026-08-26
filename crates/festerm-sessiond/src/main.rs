use std::{
    collections::{BTreeMap, VecDeque},
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
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

use festerm_pty::default_local_profile;
use festerm_ssh::PersistentSessionName;
use fs2::FileExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

const CLIENT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const REPLAY_CAPACITY_BYTES: usize = 1024 * 1024;

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
) -> Result<(String, String, u16, u16), Box<dyn std::error::Error>> {
    let mut name = None;
    let mut shell = None;
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
        shell.unwrap_or_else(default_shell),
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
    let daemon_pid = {
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
            .stderr(Stdio::null());
        command.spawn()?.id()
    };

    #[cfg(windows)]
    let daemon_pid = {
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
            // Failing to break away is preferable to starting a "persistent"
            // daemon that dies when the caller's KILL_ON_JOB_CLOSE job closes.
            .creation_flags(
                CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB,
            );
        command.spawn()?.id()
    };

    let registration = (|| -> Result<SessionRecord, Box<dyn std::error::Error>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let registry = load_registry()?;
            if let Some(record) = registry.sessions.get(&name) {
                if record.pid == daemon_pid {
                    return Ok(record.clone());
                }
            }
            if !process_alive(daemon_pid) {
                return Err(format!("session daemon for '{name}' exited during startup").into());
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
            let _ = terminate_pid(daemon_pid);
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
    shell: String,
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
            shell: shell.clone(),
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
        daemon_client_loop(listener, reader, &mut spawned, &name)?;
        let _ = fs::remove_file(socket_path);
    }

    #[cfg(windows)]
    {
        let pipe_name = session_pipe_name(&name);
        let initial_listener = named_pipe::PipeOptions::new(&pipe_name).single()?;
        let mut spawned = spawn_shell(&shell, cols, rows)?;
        let record = SessionRecord {
            name: name.clone(),
            pid: process::id(),
            socket: pipe_name.clone(),
            shell: shell.clone(),
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
        daemon_client_loop_windows(&pipe_name, initial_listener, reader, &mut spawned, &name)?;
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
    spawned: &mut SpawnedShell,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = session_client_loop(listener, reader, None, || {
        let _ = spawned.child.kill();
    });
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
    mut shutdown: impl FnMut(),
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let (pty_rx, reader_thread) = spawn_pty_reader(reader);
    let mut active: Option<UnixStream> = None;
    let mut replay = ReplayBuffer::default();
    let result = loop {
        if let Err(error) = accept_unix_clients(&listener, &mut active, &replay, observer.as_ref())
        {
            shutdown();
            break Err(error);
        }
        match pty_rx.recv_timeout(CLIENT_POLL_INTERVAL) {
            Ok(PtyEvent::Data(data)) => {
                if let Err(error) =
                    accept_unix_clients(&listener, &mut active, &replay, observer.as_ref())
                {
                    shutdown();
                    break Err(error);
                }
                replay.push(&data);
                if let Some(observer) = observer.as_ref() {
                    let _ = observer.send(ClientLoopEvent::OutputBuffered);
                }
                write_to_active(&mut active, &data);
            }
            Ok(PtyEvent::Eof) => break Ok(()),
            Ok(PtyEvent::Error(error)) => break Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };

    join_io_thread(reader_thread)?;
    result
}

#[cfg(unix)]
fn accept_unix_clients(
    listener: &UnixListener,
    active: &mut Option<UnixStream>,
    replay: &ReplayBuffer,
    observer: Option<&mpsc::Sender<ClientLoopEvent>>,
) -> io::Result<()> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))?;
                replace_active(active, stream, replay);
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

#[cfg(windows)]
fn daemon_client_loop_windows<R: Read + Send + 'static>(
    pipe_name: &str,
    initial_listener: named_pipe::ConnectingServer,
    reader: R,
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
                None => {
                    let mut options = named_pipe::PipeOptions::new(&accept_pipe_name);
                    options.first(false);
                    options.single()?.wait()
                }
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
    let mut active: Option<named_pipe::PipeServer> = None;
    let mut replay = ReplayBuffer::default();
    let result = loop {
        if let Err(error) = accept_windows_clients(&accept_rx, &mut active, &replay) {
            let _ = spawned.child.kill();
            break Err(error);
        }
        match pty_rx.recv_timeout(CLIENT_POLL_INTERVAL) {
            Ok(PtyEvent::Data(data)) => {
                if let Err(error) = accept_windows_clients(&accept_rx, &mut active, &replay) {
                    let _ = spawned.child.kill();
                    break Err(error);
                }
                replay.push(&data);
                write_to_active(&mut active, &data);
            }
            Ok(PtyEvent::Eof) => break Ok(()),
            Ok(PtyEvent::Error(error)) => break Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };

    accepting.store(false, Ordering::Release);
    drop(active);
    let _ = named_pipe::PipeClient::connect_ms(&pipe_name, 100);
    let reader_result = join_io_thread(reader_thread);
    let accept_result = join_io_thread(accept_thread);
    if result.is_err() {
        let _ = spawned.child.kill();
    }
    let _ = spawned.child.wait();
    drop_registry_record(name, process::id())?;
    reader_result?;
    accept_result?;
    result.map_err(Into::into)
}

#[cfg(windows)]
fn accept_windows_clients(
    accept_rx: &mpsc::Receiver<io::Result<named_pipe::PipeServer>>,
    active: &mut Option<named_pipe::PipeServer>,
    replay: &ReplayBuffer,
) -> io::Result<()> {
    for stream in accept_rx.try_iter() {
        let mut stream = stream?;
        stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
        replace_active(active, stream, replay);
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

fn replace_active<W: Write>(active: &mut Option<W>, replacement: W, replay: &ReplayBuffer) {
    if let Some(mut previous) = active.take() {
        let _ = send_steal_notice(&mut previous);
    }
    *active = Some(replacement);
    replay.write_to(active);
}

fn write_to_active<W: Write>(active: &mut Option<W>, data: &[u8]) {
    let failed = active.as_mut().is_some_and(|stream| {
        stream
            .write_all(data)
            .and_then(|()| stream.flush())
            .is_err()
    });
    if failed {
        *active = None;
    }
}

fn join_io_thread(thread: thread::JoinHandle<io::Result<()>>) -> io::Result<()> {
    thread
        .join()
        .map_err(|_| io::Error::other("session daemon worker thread panicked"))?
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

    fn write_to<W: Write>(&self, active: &mut Option<W>) {
        let failed = active.as_mut().is_some_and(|stream| {
            let (first, second) = self.bytes.as_slices();
            stream
                .write_all(first)
                .and_then(|()| stream.write_all(second))
                .and_then(|()| stream.flush())
                .is_err()
        });
        if failed {
            *active = None;
        }
    }
}

#[derive(Debug)]
enum PtyEvent {
    Data(Vec<u8>),
    Eof,
    Error(io::Error),
}

fn send_steal_notice<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(STOLEN_NOTICE_BYTES)?;
    writer.flush()
}

const STOLEN_NOTICE_BYTES: &[u8] =
    b"\n[festerm-sessiond] SESSION_STOLEN: reattached from another client\n";

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
    Stolen,
}

fn forward_attach_stream<R: Read, W: Write>(
    reader: &mut R,
    output: &mut W,
) -> io::Result<AttachOutcome> {
    let mut buffer = [0u8; 4096];
    let mut pending = Vec::with_capacity(STOLEN_NOTICE_BYTES.len() + buffer.len());
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                output.write_all(&pending)?;
                output.flush()?;
                return Ok(AttachOutcome::Closed);
            }
            Ok(count) => {
                pending.extend_from_slice(&buffer[..count]);
                if let Some(position) = find_bytes(&pending, STOLEN_NOTICE_BYTES) {
                    output.write_all(&pending[..position])?;
                    output.flush()?;
                    return Ok(AttachOutcome::Stolen);
                }

                let retained = partial_marker_suffix_len(&pending);
                let flush_count = pending.len() - retained;
                if flush_count > 0 {
                    output.write_all(&pending[..flush_count])?;
                    pending.drain(..flush_count);
                    output.flush()?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                output.write_all(&pending)?;
                output.flush()?;
                return Ok(AttachOutcome::Closed);
            }
            Err(error) => return Err(error),
        }
    }
}

fn partial_marker_suffix_len(data: &[u8]) -> usize {
    let maximum = data.len().min(STOLEN_NOTICE_BYTES.len().saturating_sub(1));
    (1..=maximum)
        .rev()
        .find(|&length| data.ends_with(&STOLEN_NOTICE_BYTES[..length]))
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
        forward_attach_stream(&mut stream, &mut io::stdout())?
    };

    #[cfg(windows)]
    let outcome = {
        let mut stream = named_pipe::PipeClient::connect(&record.socket)?;
        forward_attach_stream(&mut stream, &mut io::stdout())?
    };

    if outcome == AttachOutcome::Stolen {
        eprintln!(
            "[festerm-sessiond] session taken over by another client; this attach lost the session"
        );
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

fn set_dir_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
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
        use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

        match kill(Pid::from_raw(pid as i32), None) {
            Ok(()) | Err(Errno::EPERM) => true,
            Err(Errno::ESRCH) => false,
            Err(_) => false,
        }
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, BOOL};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, STILL_ACTIVE,
        };

        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle == 0 {
            return false;
        }

        let mut exit_code = 0u32;
        let alive = unsafe { GetExitCodeProcess(handle, &mut exit_code) != BOOL(0) }
            && exit_code == STILL_ACTIVE;
        unsafe {
            let _ = CloseHandle(handle);
        }
        alive
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
        use windows_sys::Win32::Foundation::{CloseHandle, BOOL};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };

        // Minimal pass: direct Win32 APIs avoid shelling out to taskkill while
        // keeping the local session daemon small and dependency-light.
        if !process_alive(pid) {
            return Ok(());
        }
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle == 0 {
            return Err(io::Error::last_os_error().into());
        }

        let terminated = unsafe { TerminateProcess(handle, 1) != BOOL(0) };
        let error = (!terminated).then(io::Error::last_os_error);
        unsafe {
            let _ = CloseHandle(handle);
        }
        match error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
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
        let service = thread::spawn(move || {
            session_client_loop(
                listener,
                ChannelReader {
                    receiver: pty_receiver,
                    pending: Vec::new(),
                },
                Some(event_sender),
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
        let mut replayed = [0u8; 8];
        first.read_exact(&mut replayed).unwrap();
        assert_eq!(&replayed, b"detached");

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

        assert_eq!(
            send_and_receive(&pty_sender, &mut second, b"second"),
            b"second"
        );
        assert_eq!(
            event_receiver.recv().unwrap(),
            ClientLoopEvent::OutputBuffered
        );

        drop(pty_sender);
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
        };
        replay.push(b"abc");
        replay.push(b"defg");
        assert_eq!(replay.bytes.iter().copied().collect::<Vec<_>>(), b"cdefg");
        replay.push(b"1234567");
        assert_eq!(replay.bytes.iter().copied().collect::<Vec<_>>(), b"34567");
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

    fn unique_test_directory(label: &str) -> PathBuf {
        env::temp_dir().join(format!("fsd-{label}-{}-{}", process::id(), now_ms()))
    }
}
