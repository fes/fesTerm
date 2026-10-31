#![cfg(unix)]

//! Integration tests against a real OpenSSH `sshd` + `sftp-server`.
//!
//! Run them with `scripts/run-sftp-integration.sh`, which starts a throwaway
//! user-level server and exports the `FESTERM_SFTP_TEST_*` variables. They are
//! `#[ignore]`d so a plain `cargo test` never needs a server.
//!
//! Connections go through `festerm-ssh`'s real GUI SFTP connect path, so the
//! `into_transport` hand-off used by the iOS host is exercised too.

use festerm_session::TerminalSize;
use festerm_sftp::{
    CancelToken, ClientConfig, ConflictPolicy, Direction, EntryKind, ListOptions, LocalFs, PaneFs,
    SftpClient, SftpError, SortKey, SortOrder, TransferConfig, TransferEvent, TransferInfo,
    TransferManager, TransferRequest, TransferState,
};
use festerm_ssh::{
    connect_gui_sftp_session, GuiSftpSessionConnectOutcome, HostIdentity, HostTrustDecision,
    SshAuthentication, SshConnectionProfile, SshPrivateKey,
};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

struct Env {
    port: u16,
    user: String,
    key: String,
    dir: PathBuf,
}

fn env() -> Env {
    let var = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| {
            panic!("{name} is not set; run these tests via scripts/run-sftp-integration.sh")
        })
    };
    Env {
        port: var("FESTERM_SFTP_TEST_PORT").parse().unwrap(),
        user: var("FESTERM_SFTP_TEST_USER"),
        key: fs::read_to_string(var("FESTERM_SFTP_TEST_KEY")).unwrap(),
        dir: fs::canonicalize(var("FESTERM_SFTP_TEST_DIR")).unwrap(),
    }
}

fn connect_via(port: u16) -> SftpClient {
    let env = env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let session = runtime.block_on(async {
        let profile = SshConnectionProfile::new(
            HostIdentity::new("127.0.0.1", port).unwrap(),
            env.user.clone(),
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap();
        let authentication =
            SshAuthentication::public_key(SshPrivateKey::from_openssh(&env.key).unwrap());
        match connect_gui_sftp_session(profile, authentication, None, None)
            .await
            .unwrap()
        {
            GuiSftpSessionConnectOutcome::Connected(session) => session,
            GuiSftpSessionConnectOutcome::NeedsHostKeyDecision {
                prompt,
                resolver,
                completion,
            } => {
                resolver
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .unwrap();
                completion.wait().await.unwrap()
            }
        }
    });
    let (client, keepalive) = session.into_transport();
    SftpClient::from_russh(
        client,
        keepalive,
        ClientConfig {
            request_timeout: Duration::from_secs(10),
            operation_timeout: Duration::from_secs(30),
        },
    )
    .unwrap()
}

fn connect() -> SftpClient {
    connect_via(env().port)
}

/// A scratch area: `server` is served by sshd, `sandbox` is the "iPhone" side.
struct Scratch {
    server: PathBuf,
    sandbox: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let env = env();
        let base = env.dir.parent().unwrap().join("scratch");
        let server = env.dir.join(format!("{name}-{}", std::process::id()));
        let sandbox = base.join(format!("{name}-{}", std::process::id()));
        for path in [&server, &sandbox] {
            let _ = fs::remove_dir_all(path);
            fs::create_dir_all(path).unwrap();
        }
        Self {
            server,
            sandbox: fs::canonicalize(sandbox).unwrap(),
        }
    }

    fn remote(&self, name: &str) -> String {
        self.server.join(name).to_str().unwrap().to_owned()
    }

    fn remote_dir(&self) -> String {
        self.server.to_str().unwrap().to_owned()
    }

    fn local_fs(&self) -> LocalFs {
        LocalFs::new(&self.sandbox).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for path in [&self.server, &self.sandbox] {
            // Restore access so cleanup works after permission tests.
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn write_pattern_file(path: &Path, len: usize, seed: u64) -> u64 {
    let mut file = fs::File::create(path).unwrap();
    let mut written = 0usize;
    let mut hash = fnv_start();
    let mut chunk_seed = seed;
    while written < len {
        let size = (len - written).min(1 << 20);
        let chunk = pattern(size, chunk_seed);
        hash = fnv_update(hash, &chunk);
        file.write_all(&chunk).unwrap();
        written += size;
        chunk_seed += 1;
    }
    hash
}

fn fnv_start() -> u64 {
    0xcbf29ce484222325
}

fn fnv_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn file_hash(path: &Path) -> u64 {
    let mut file = fs::File::open(path).unwrap();
    let mut buffer = vec![0u8; 1 << 20];
    let mut hash = fnv_start();
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            return hash;
        }
        hash = fnv_update(hash, &buffer[..read]);
    }
}

fn manager(client: &SftpClient, scratch: &Scratch) -> TransferManager {
    TransferManager::new(
        client.clone(),
        scratch.local_fs(),
        TransferConfig::default(),
    )
}

fn wait_until(
    manager: &TransferManager,
    id: u64,
    what: &str,
    done: impl Fn(&TransferInfo) -> bool,
) -> TransferInfo {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let info = manager.info(id).expect("transfer retained");
        if done(&info) {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {info:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_finished(manager: &TransferManager, id: u64) -> TransferInfo {
    wait_until(manager, id, "completion", |i| i.state.is_finished())
}

fn wait_conflict(manager: &TransferManager, id: u64) -> festerm_sftp::Conflict {
    match wait_until(manager, id, "conflict", |i| {
        matches!(i.state, TransferState::AwaitingConflict(_)) || i.state.is_finished()
    })
    .state
    {
        TransferState::AwaitingConflict(conflict) => conflict,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

fn partials(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".festerm-") && n.ends_with(".part"))
        .collect()
}

/// TCP relay that can be cut and optionally throttled (server -> client).
struct Relay {
    port: u16,
    cut: Arc<AtomicBool>,
    connections: Arc<Mutex<Vec<TcpStream>>>,
}

impl Relay {
    fn start(upstream: u16, throttle: Option<Duration>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cut = Arc::new(AtomicBool::new(false));
        let connections: Arc<Mutex<Vec<TcpStream>>> = Arc::default();
        let (cut_flag, kept) = (Arc::clone(&cut), Arc::clone(&connections));
        thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let Ok(server) = TcpStream::connect(("127.0.0.1", upstream)) else {
                    continue;
                };
                kept.lock().unwrap().push(client.try_clone().unwrap());
                kept.lock().unwrap().push(server.try_clone().unwrap());
                let pairs = [
                    (
                        client.try_clone().unwrap(),
                        server.try_clone().unwrap(),
                        None,
                    ),
                    (server, client, throttle),
                ];
                for (mut from, mut to, delay) in pairs {
                    let cut = Arc::clone(&cut_flag);
                    thread::spawn(move || {
                        let mut buffer = [0u8; 16 * 1024];
                        while !cut.load(Ordering::SeqCst) {
                            match from.read(&mut buffer) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    if let Some(delay) = delay {
                                        thread::sleep(delay);
                                    }
                                    if to.write_all(&buffer[..n]).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        let _ = to.shutdown(Shutdown::Both);
                        let _ = from.shutdown(Shutdown::Both);
                    });
                }
            }
        });
        Self {
            port,
            cut,
            connections,
        }
    }

    fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
        for stream in self.connections.lock().unwrap().iter() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn listing_sorts_filters_bounds_and_reports_symlinks() {
    let scratch = Scratch::new("listing");
    let dir = &scratch.server;
    fs::write(dir.join("b.txt"), b"bb").unwrap();
    fs::write(dir.join("A.log"), b"a").unwrap();
    fs::write(dir.join(".hidden"), b"h").unwrap();
    fs::create_dir(dir.join("sub")).unwrap();
    symlink("b.txt", dir.join("link-file")).unwrap();
    symlink("sub", dir.join("link-dir")).unwrap();
    symlink("missing", dir.join("link-dangling")).unwrap();
    let client = connect();

    let listing = client
        .list(&scratch.remote_dir(), &ListOptions::default())
        .unwrap();
    let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    // Directories (including links to them) first, then case-insensitive names.
    assert_eq!(
        names,
        [
            "link-dir",
            "sub",
            "A.log",
            "b.txt",
            "link-dangling",
            "link-file"
        ]
    );
    assert_eq!(listing.hidden_count, 1);
    assert!(!listing.truncated);

    let by_name: std::collections::HashMap<_, _> = listing
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();
    let link_file = by_name["link-file"];
    assert_eq!(link_file.kind, EntryKind::Symlink);
    assert_eq!(link_file.effective_kind(), EntryKind::File);
    assert_eq!(link_file.size, Some(2));
    assert_eq!(
        link_file.symlink.as_ref().unwrap().target.as_deref(),
        Some("b.txt")
    );
    assert!(by_name["link-dir"].is_dir());
    let dangling = by_name["link-dangling"];
    assert_eq!(dangling.symlink.as_ref().unwrap().target_kind, None);
    assert_eq!(by_name["sub"].kind, EntryKind::Directory);
    assert!(by_name["b.txt"].modified.is_some() && by_name["b.txt"].permissions.is_some());

    let options = ListOptions {
        show_hidden: true,
        filter: Some("A.L".into()),
        ..ListOptions::default()
    };
    let filtered = client.list(&scratch.remote_dir(), &options).unwrap();
    assert_eq!(filtered.entries.len(), 1);

    let options = ListOptions {
        sort: SortKey::Size,
        order: SortOrder::Descending,
        directories_first: false,
        show_hidden: true,
        max_entries: 3,
        ..ListOptions::default()
    };
    let bounded = client.list(&scratch.remote_dir(), &options).unwrap();
    assert_eq!(bounded.entries.len(), 3);
    assert!(bounded.truncated);

    let options = ListOptions {
        max_scan: 2,
        show_hidden: true,
        ..ListOptions::default()
    };
    let scanned = client.list(&scratch.remote_dir(), &options).unwrap();
    assert_eq!(scanned.scanned, 2);
    assert!(scanned.truncated);

    assert!(matches!(
        client.list(&scratch.remote("nope"), &ListOptions::default()),
        Err(SftpError::NotFound(_))
    ));
    assert!(client
        .list(&scratch.remote("b.txt"), &ListOptions::default())
        .is_err());
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn large_directory_listing_is_bounded_and_cancellable() {
    let scratch = Scratch::new("bigdir");
    for index in 0..3000 {
        fs::write(scratch.server.join(format!("f{index:05}")), b"").unwrap();
    }
    let client = connect();
    let options = ListOptions {
        max_scan: 1000,
        max_entries: 500,
        ..ListOptions::default()
    };
    let listing = client.list(&scratch.remote_dir(), &options).unwrap();
    assert_eq!(listing.entries.len(), 500);
    assert_eq!(listing.scanned, 1000);
    assert!(listing.truncated);

    let cancel = CancelToken::new();
    cancel.cancel();
    let options = ListOptions {
        cancel: Some(cancel),
        ..ListOptions::default()
    };
    assert_eq!(
        client.list(&scratch.remote_dir(), &options).unwrap_err(),
        SftpError::Cancelled
    );
    // The connection stays usable after a cancelled listing.
    assert!(client
        .list(&scratch.remote_dir(), &ListOptions::default())
        .is_ok());
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn mkdir_rename_delete_and_their_errors() {
    let scratch = Scratch::new("mutations");
    let client = connect();
    let d = scratch.remote("d");
    client.mkdir(&d).unwrap();
    assert!(matches!(client.mkdir(&d), Err(SftpError::AlreadyExists(_))));
    assert!(matches!(
        client.mkdir(&scratch.remote("missing/child")),
        Err(SftpError::NotFound(_) | SftpError::Remote(_))
    ));
    assert!(matches!(
        client.mkdir(&format!("{}/bad\u{202E}name", scratch.remote_dir())),
        Ok(()) | Err(_)
    ));

    fs::write(scratch.server.join("d/file"), b"x").unwrap();
    fs::write(scratch.server.join("other"), b"y").unwrap();
    assert!(matches!(
        client.rename(&scratch.remote("d/file"), &scratch.remote("other")),
        Err(SftpError::AlreadyExists(_))
    ));
    client
        .rename(&scratch.remote("d/file"), &scratch.remote("d/renamed"))
        .unwrap();
    assert!(scratch.server.join("d/renamed").exists());
    assert_eq!(fs::read(scratch.server.join("other")).unwrap(), b"y");

    assert!(matches!(
        client.delete(&d, false),
        Err(SftpError::DirectoryNotEmpty(_))
    ));
    fs::create_dir_all(scratch.server.join("d/deep/er")).unwrap();
    symlink(&scratch.server, scratch.server.join("d/deep/loop")).unwrap();
    client.delete(&d, true).unwrap();
    assert!(!scratch.server.join("d").exists());
    // The link inside was unlinked, not followed.
    assert!(scratch.server.join("other").exists());
    assert!(matches!(
        client.delete(&d, false),
        Err(SftpError::NotFound(_))
    ));
    assert!(matches!(
        client.delete("/", true),
        Err(SftpError::InvalidPath(_))
    ));

    // Deleting a link removes the link only.
    fs::create_dir(scratch.server.join("real")).unwrap();
    fs::write(scratch.server.join("real/keep"), b"k").unwrap();
    symlink("real", scratch.server.join("alias")).unwrap();
    client.delete(&scratch.remote("alias"), true).unwrap();
    assert!(scratch.server.join("real/keep").exists());

    let entry = client.stat(&scratch.remote("other")).unwrap();
    assert_eq!((entry.kind, entry.size), (EntryKind::File, Some(1)));
    assert!(matches!(
        client.stat(&scratch.remote("gone")),
        Err(SftpError::NotFound(_))
    ));
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn download_and_upload_round_trip_with_progress_and_mtime() {
    let scratch = Scratch::new("roundtrip");
    let client = connect();
    let manager = manager(&client, &scratch);
    let events = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&events);
    manager.set_event_callback(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    let size = 3 * 1024 * 1024 + 123;
    let hash = write_pattern_file(&scratch.server.join("data.bin"), size, 7);
    let old = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(scratch.server.join("data.bin"))
        .unwrap()
        .set_modified(old)
        .unwrap();

    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("data.bin"), ""))
        .unwrap();
    let info = wait_finished(&manager, id);
    assert_eq!(info.state, TransferState::Completed, "{info:?}");
    assert_eq!(info.progress.bytes_done, size as u64);
    assert_eq!(info.progress.bytes_total, Some(size as u64));
    assert_eq!(file_hash(&scratch.sandbox.join("data.bin")), hash);
    let local_mtime = fs::metadata(scratch.sandbox.join("data.bin"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(local_mtime, old, "download keeps the remote mtime");
    assert!(partials(&scratch.sandbox).is_empty());

    fs::create_dir(scratch.sandbox.join("up")).unwrap();
    fs::copy(
        scratch.sandbox.join("data.bin"),
        scratch.sandbox.join("up/copy.bin"),
    )
    .unwrap();
    fs::create_dir(scratch.server.join("incoming")).unwrap();
    let id = manager
        .enqueue(TransferRequest::upload(
            "up/copy.bin",
            scratch.remote("incoming"),
        ))
        .unwrap();
    let info = wait_finished(&manager, id);
    assert_eq!(info.state, TransferState::Completed, "{info:?}");
    assert_eq!(info.progress.bytes_done, size as u64);
    assert_eq!(file_hash(&scratch.server.join("incoming/copy.bin")), hash);
    assert!(partials(&scratch.server.join("incoming")).is_empty());

    let polled = manager.poll_events();
    let progress: Vec<u64> = polled
        .iter()
        .filter_map(|e| match e {
            TransferEvent::Progress { progress, .. } => Some(progress.bytes_done),
            _ => None,
        })
        .collect();
    assert!(!progress.is_empty());
    assert!(progress.windows(2).all(|w| w[0] <= w[1]));
    assert!(polled.iter().any(|e| matches!(
        e,
        TransferEvent::StateChanged {
            state: TransferState::Completed,
            ..
        }
    )));
    assert!(events.load(Ordering::SeqCst) >= 4);
    assert_eq!(manager.snapshot().len(), 2);
    assert_eq!(manager.snapshot()[0].direction, Direction::Download);
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn sixty_four_megabyte_files_stream_in_both_directions() {
    let scratch = Scratch::new("large");
    let client = connect();
    let manager = manager(&client, &scratch);
    let size = 64 * 1024 * 1024;
    let hash = write_pattern_file(&scratch.server.join("big.bin"), size, 99);

    let started = Instant::now();
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("big.bin"), ""))
        .unwrap();
    let info = wait_finished(&manager, id);
    assert_eq!(info.state, TransferState::Completed, "{info:?}");
    let down = started.elapsed();
    assert_eq!(
        fs::metadata(scratch.sandbox.join("big.bin")).unwrap().len(),
        size as u64
    );
    assert_eq!(file_hash(&scratch.sandbox.join("big.bin")), hash);
    assert!(info.progress.rate_bytes_per_sec > 0.0);

    fs::create_dir(scratch.server.join("out")).unwrap();
    let started = Instant::now();
    let id = manager
        .enqueue(TransferRequest::upload("big.bin", scratch.remote("out")))
        .unwrap();
    let info = wait_finished(&manager, id);
    assert_eq!(info.state, TransferState::Completed, "{info:?}");
    let up = started.elapsed();
    assert_eq!(file_hash(&scratch.server.join("out/big.bin")), hash);
    eprintln!("64 MiB download {down:?}, upload {up:?}");
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn download_conflicts_keep_both_skip_replace_and_remember() {
    let scratch = Scratch::new("conflicts-down");
    let client = connect();
    let manager = manager(&client, &scratch);
    fs::write(scratch.server.join("report.pdf"), b"remote-new").unwrap();
    fs::write(scratch.sandbox.join("report.pdf"), b"local-old").unwrap();

    // Keep both: lands as "report (1).pdf" next to the original.
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("report.pdf"), ""))
        .unwrap();
    let conflict = wait_conflict(&manager, id);
    assert_eq!(conflict.suggested_name, "report (1).pdf");
    assert_eq!(conflict.existing.size, Some(9));
    assert_eq!(conflict.incoming.size, Some(10));
    assert!(!conflict.likely_identical);
    manager
        .resolve_conflict(id, ConflictPolicy::KeepBoth, false)
        .unwrap();
    let info = wait_finished(&manager, id);
    assert_eq!(info.state, TransferState::Completed);
    assert_eq!(info.name, "report (1).pdf");
    assert_eq!(
        fs::read(scratch.sandbox.join("report.pdf")).unwrap(),
        b"local-old"
    );
    assert_eq!(
        fs::read(scratch.sandbox.join("report (1).pdf")).unwrap(),
        b"remote-new"
    );

    // Second keep-both picks the next free number.
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("report.pdf"), ""))
        .unwrap();
    assert_eq!(wait_conflict(&manager, id).suggested_name, "report (2).pdf");
    manager
        .resolve_conflict(id, ConflictPolicy::KeepBoth, false)
        .unwrap();
    assert_eq!(wait_finished(&manager, id).name, "report (2).pdf");

    // Skip leaves the target alone and moves no data.
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("report.pdf"), ""))
        .unwrap();
    wait_conflict(&manager, id);
    manager
        .resolve_conflict(id, ConflictPolicy::Skip, false)
        .unwrap();
    let info = wait_finished(&manager, id);
    assert_eq!(info.state, TransferState::Skipped);
    assert_eq!(
        fs::read(scratch.sandbox.join("report.pdf")).unwrap(),
        b"local-old"
    );

    // Replace swaps atomically.
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("report.pdf"), ""))
        .unwrap();
    wait_conflict(&manager, id);
    manager
        .resolve_conflict(id, ConflictPolicy::Replace, false)
        .unwrap();
    assert_eq!(wait_finished(&manager, id).state, TransferState::Completed);
    assert_eq!(
        fs::read(scratch.sandbox.join("report.pdf")).unwrap(),
        b"remote-new"
    );
    assert!(partials(&scratch.sandbox).is_empty());

    // "Use this choice for remaining conflicts" resolves queued peers too.
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(scratch.server.join(name), format!("remote {name}")).unwrap();
        fs::write(scratch.sandbox.join(name), b"local").unwrap();
    }
    let ids: Vec<_> = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|n| {
            manager
                .enqueue(TransferRequest::download(scratch.remote(n), ""))
                .unwrap()
        })
        .collect();
    let first = ids
        .iter()
        .copied()
        .find(|id| {
            wait_until(&manager, *id, "first conflict", |i| {
                matches!(i.state, TransferState::AwaitingConflict(_)) || i.state.is_finished()
            });
            matches!(
                manager.info(*id).unwrap().state,
                TransferState::AwaitingConflict(_)
            )
        })
        .unwrap();
    manager
        .resolve_conflict(first, ConflictPolicy::Skip, true)
        .unwrap();
    for id in &ids {
        assert_eq!(wait_finished(&manager, *id).state, TransferState::Skipped);
    }
    assert_eq!(manager.default_policy(), Some(ConflictPolicy::Skip));
    assert_eq!(fs::read(scratch.sandbox.join("b.txt")).unwrap(), b"local");
    // Later conflicts do not ask again.
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("a.txt"), ""))
        .unwrap();
    assert_eq!(wait_finished(&manager, id).state, TransferState::Skipped);
    manager.set_default_policy(None);

    // Directories are never replaced.
    fs::create_dir(scratch.sandbox.join("dirname")).unwrap();
    fs::write(scratch.server.join("dirname"), b"file").unwrap();
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("dirname"), ""))
        .unwrap();
    assert!(wait_conflict(&manager, id).existing_is_directory);
    manager
        .resolve_conflict(id, ConflictPolicy::Replace, false)
        .unwrap();
    assert!(matches!(
        wait_finished(&manager, id).state,
        TransferState::Failed(SftpError::IsADirectory(_))
    ));
    assert!(scratch.sandbox.join("dirname").is_dir());
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn upload_conflicts_keep_both_skip_replace() {
    let scratch = Scratch::new("conflicts-up");
    let client = connect();
    let manager = manager(&client, &scratch);
    fs::write(scratch.sandbox.join("notes.txt"), b"phone-new").unwrap();
    fs::write(scratch.server.join("notes.txt"), b"server-old").unwrap();
    let dir = scratch.remote_dir();

    let id = manager
        .enqueue(TransferRequest::upload("notes.txt", &dir))
        .unwrap();
    let conflict = wait_conflict(&manager, id);
    assert_eq!(conflict.suggested_name, "notes (1).txt");
    manager
        .resolve_conflict(id, ConflictPolicy::KeepBoth, false)
        .unwrap();
    assert_eq!(wait_finished(&manager, id).name, "notes (1).txt");
    assert_eq!(
        fs::read(scratch.server.join("notes.txt")).unwrap(),
        b"server-old"
    );
    assert_eq!(
        fs::read(scratch.server.join("notes (1).txt")).unwrap(),
        b"phone-new"
    );

    let id = manager
        .enqueue(TransferRequest::upload("notes.txt", &dir))
        .unwrap();
    wait_conflict(&manager, id);
    manager
        .resolve_conflict(id, ConflictPolicy::Skip, false)
        .unwrap();
    assert_eq!(wait_finished(&manager, id).state, TransferState::Skipped);
    assert_eq!(
        fs::read(scratch.server.join("notes.txt")).unwrap(),
        b"server-old"
    );

    let id = manager
        .enqueue(TransferRequest::upload("notes.txt", &dir))
        .unwrap();
    wait_conflict(&manager, id);
    manager
        .resolve_conflict(id, ConflictPolicy::Replace, false)
        .unwrap();
    assert_eq!(wait_finished(&manager, id).state, TransferState::Completed);
    assert_eq!(
        fs::read(scratch.server.join("notes.txt")).unwrap(),
        b"phone-new"
    );
    assert!(partials(&scratch.server).is_empty());

    // Replacing a remote symlink replaces the link, not what it points to.
    fs::write(scratch.server.join("target"), b"precious").unwrap();
    symlink("target", scratch.server.join("linked.txt")).unwrap();
    fs::write(scratch.sandbox.join("linked.txt"), b"fresh").unwrap();
    let id = manager
        .enqueue(TransferRequest::upload("linked.txt", &dir))
        .unwrap();
    wait_conflict(&manager, id);
    manager
        .resolve_conflict(id, ConflictPolicy::Replace, false)
        .unwrap();
    assert_eq!(wait_finished(&manager, id).state, TransferState::Completed);
    assert_eq!(
        fs::read(scratch.server.join("target")).unwrap(),
        b"precious"
    );
    assert_eq!(
        fs::read(scratch.server.join("linked.txt")).unwrap(),
        b"fresh"
    );
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn cancelling_mid_transfer_leaves_no_final_or_partial_file() {
    let scratch = Scratch::new("cancel");
    // Throttled relay so there is time to cancel mid-stream.
    let relay = Relay::start(env().port, Some(Duration::from_millis(1)));
    let client = connect_via(relay.port);
    let manager = manager(&client, &scratch);
    write_pattern_file(&scratch.server.join("slow.bin"), 24 * 1024 * 1024, 5);

    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("slow.bin"), ""))
        .unwrap();
    wait_until(&manager, id, "bytes flowing", |i| {
        i.progress.bytes_done > 512 * 1024
    });
    manager.cancel(id);
    let info = wait_finished(&manager, id);
    assert_eq!(info.state, TransferState::Cancelled);
    assert!(!scratch.sandbox.join("slow.bin").exists());
    assert!(partials(&scratch.sandbox).is_empty());

    write_pattern_file(&scratch.sandbox.join("up.bin"), 24 * 1024 * 1024, 6);
    fs::create_dir(scratch.server.join("in")).unwrap();
    let id = manager
        .enqueue(TransferRequest::upload("up.bin", scratch.remote("in")))
        .unwrap();
    wait_until(&manager, id, "bytes flowing", |i| {
        i.progress.bytes_done > 512 * 1024
    });
    manager.cancel(id);
    assert_eq!(wait_finished(&manager, id).state, TransferState::Cancelled);
    assert_eq!(fs::read_dir(scratch.server.join("in")).unwrap().count(), 0);

    // Cancelling while queued (concurrency 1 behind a running job).
    let single = TransferManager::new(
        client.clone(),
        scratch.local_fs(),
        TransferConfig {
            max_concurrent: 1,
            ..TransferConfig::default()
        },
    );
    let first = single
        .enqueue(TransferRequest::download(scratch.remote("slow.bin"), "").clone())
        .unwrap();
    let second = single
        .enqueue(TransferRequest {
            name: Some("second.bin".into()),
            ..TransferRequest::download(scratch.remote("slow.bin"), "")
        })
        .unwrap();
    single.cancel(second);
    assert_eq!(
        wait_finished(&single, second).state,
        TransferState::Cancelled
    );
    single.cancel(first);
    assert_eq!(
        wait_finished(&single, first).state,
        TransferState::Cancelled
    );
    assert!(partials(&scratch.sandbox).is_empty());

    // Cancelling while waiting on a conflict.
    fs::write(scratch.sandbox.join("slow.bin"), b"exists").unwrap();
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("slow.bin"), ""))
        .unwrap();
    wait_conflict(&manager, id);
    manager.cancel(id);
    assert_eq!(wait_finished(&manager, id).state, TransferState::Cancelled);
    assert_eq!(
        fs::read(scratch.sandbox.join("slow.bin")).unwrap(),
        b"exists"
    );
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn server_disconnect_mid_transfer_fails_cleanly() {
    for direction in [Direction::Download, Direction::Upload] {
        let scratch = Scratch::new(&format!("disconnect-{direction:?}").to_lowercase());
        let relay = Arc::new(Relay::start(env().port, Some(Duration::from_millis(1))));
        let client = connect_via(relay.port);
        let manager = manager(&client, &scratch);
        let cutter = Arc::clone(&relay);
        manager.set_event_callback(move |event| {
            if let TransferEvent::Progress { progress, .. } = event {
                if progress.bytes_done > 1024 * 1024 {
                    cutter.cut();
                }
            }
        });
        write_pattern_file(&scratch.server.join("f.bin"), 24 * 1024 * 1024, 11);
        write_pattern_file(&scratch.sandbox.join("g.bin"), 24 * 1024 * 1024, 12);
        fs::create_dir(scratch.server.join("in")).unwrap();
        let request = match direction {
            Direction::Download => TransferRequest::download(scratch.remote("f.bin"), ""),
            Direction::Upload => TransferRequest::upload("g.bin", scratch.remote("in")),
        };
        let id = manager.enqueue(request).unwrap();
        let info = wait_finished(&manager, id);
        match &info.state {
            TransferState::Failed(error) => {
                assert!(error.is_connection_lost(), "{direction:?}: {error:?}");
            }
            other => panic!("{direction:?}: expected a connection failure, got {other:?}"),
        }
        assert!(!scratch.sandbox.join("f.bin").exists());
        assert!(!scratch.server.join("in/g.bin").exists());
        assert!(partials(&scratch.sandbox).is_empty());
        assert!(!client.is_connected());
        // The pane reports the dead connection promptly instead of hanging.
        let started = Instant::now();
        let error = client
            .list(&scratch.remote_dir(), &ListOptions::default())
            .unwrap_err();
        assert!(error.is_connection_lost(), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        // Server side: only an abandoned temp (if any) may remain, never the final name.
        let _ = fs::remove_dir_all(scratch.server.join("in"));
    }
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn permission_errors_are_classified_and_leave_nothing_behind() {
    let scratch = Scratch::new("perms");
    let client = connect();
    let manager = manager(&client, &scratch);

    fs::write(scratch.server.join("secret"), b"nope").unwrap();
    fs::set_permissions(
        scratch.server.join("secret"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("secret"), ""))
        .unwrap();
    assert!(matches!(
        wait_finished(&manager, id).state,
        TransferState::Failed(SftpError::PermissionDenied(_))
    ));
    assert!(!scratch.sandbox.join("secret").exists());
    assert!(partials(&scratch.sandbox).is_empty());

    fs::create_dir(scratch.server.join("locked")).unwrap();
    fs::write(scratch.server.join("locked/x"), b"x").unwrap();
    fs::set_permissions(
        scratch.server.join("locked"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    assert!(matches!(
        client.list(&scratch.remote("locked"), &ListOptions::default()),
        Err(SftpError::PermissionDenied(_))
    ));

    fs::create_dir(scratch.server.join("readonly")).unwrap();
    fs::set_permissions(
        scratch.server.join("readonly"),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    fs::write(scratch.sandbox.join("up.txt"), b"data").unwrap();
    let id = manager
        .enqueue(TransferRequest::upload(
            "up.txt",
            scratch.remote("readonly"),
        ))
        .unwrap();
    assert!(matches!(
        wait_finished(&manager, id).state,
        TransferState::Failed(SftpError::PermissionDenied(_))
    ));
    assert!(matches!(
        client.mkdir(&scratch.remote("readonly/new")),
        Err(SftpError::PermissionDenied(_))
    ));
    assert_eq!(
        fs::read_dir(scratch.server.join("readonly"))
            .unwrap()
            .count(),
        0
    );
    // A failed transfer does not poison the connection.
    assert!(client.is_connected());
    fs::set_permissions(
        scratch.server.join("locked"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn symlinks_and_odd_names_are_handled_by_policy() {
    let scratch = Scratch::new("odd");
    let client = connect();
    let manager = manager(&client, &scratch);
    fs::write(scratch.server.join("real.txt"), b"real contents").unwrap();
    symlink("real.txt", scratch.server.join("alias.txt")).unwrap();
    fs::create_dir(scratch.server.join("adir")).unwrap();
    symlink("adir", scratch.server.join("dirlink")).unwrap();
    symlink("nowhere", scratch.server.join("dangling")).unwrap();
    let odd_newline = "line\nbreak.txt";
    let odd_bidi = "evil\u{202E}txt.exe";
    let odd_backslash = "back\\slash";
    for name in [odd_newline, odd_bidi, odd_backslash] {
        fs::write(scratch.server.join(name), b"odd").unwrap();
    }

    // Links to regular files download as the target's contents under the link's name.
    let id = manager
        .enqueue(TransferRequest::download(scratch.remote("alias.txt"), ""))
        .unwrap();
    assert_eq!(wait_finished(&manager, id).state, TransferState::Completed);
    assert_eq!(
        fs::read(scratch.sandbox.join("alias.txt")).unwrap(),
        b"real contents"
    );
    assert!(fs::symlink_metadata(scratch.sandbox.join("alias.txt"))
        .unwrap()
        .is_file());

    for (source, check) in [("dirlink", "dir"), ("adir", "dir"), ("dangling", "missing")] {
        let id = manager
            .enqueue(TransferRequest::download(scratch.remote(source), ""))
            .unwrap();
        match (wait_finished(&manager, id).state, check) {
            (TransferState::Failed(SftpError::IsADirectory(_)), "dir") => {}
            (TransferState::Failed(SftpError::NotFound(_)), "missing") => {}
            (other, _) => panic!("{source}: unexpected {other:?}"),
        }
    }

    // Odd remote names list safely flagged and never become local names.
    let listing = client
        .list(
            &scratch.remote_dir(),
            &ListOptions {
                show_hidden: true,
                ..ListOptions::default()
            },
        )
        .unwrap();
    for name in [odd_newline, odd_bidi, odd_backslash] {
        let entry = listing.entries.iter().find(|e| e.name == name).unwrap();
        assert!(!entry.name_safe, "{name:?}");
        assert!(!entry.display_name.contains(['\n', '\u{202E}']));
    }
    for name in [odd_newline, odd_bidi, odd_backslash] {
        assert!(matches!(
            manager.enqueue(TransferRequest::download(scratch.remote(name), "")),
            Err(SftpError::InvalidPath(_))
        ));
    }
    // ...unless the UI supplies a safe destination name.
    let id = manager
        .enqueue(TransferRequest {
            name: Some("renamed.txt".into()),
            ..TransferRequest::download(scratch.remote(odd_newline), "")
        })
        .unwrap();
    assert_eq!(wait_finished(&manager, id).state, TransferState::Completed);
    assert_eq!(
        fs::read(scratch.sandbox.join("renamed.txt")).unwrap(),
        b"odd"
    );

    // Traversal and NUL are rejected before anything runs.
    for request in [
        TransferRequest::download(scratch.remote("real.txt"), "../outside"),
        TransferRequest::download(scratch.remote("real.txt"), "a/../../b"),
        TransferRequest {
            name: Some("../escape".into()),
            ..TransferRequest::download(scratch.remote("real.txt"), "")
        },
        TransferRequest {
            name: Some("nul\0.txt".into()),
            ..TransferRequest::download(scratch.remote("real.txt"), "")
        },
        TransferRequest::upload("../etc/passwd", scratch.remote_dir()),
        TransferRequest {
            name: Some(".festerm-0000.part".into()),
            ..TransferRequest::upload("x", scratch.remote_dir())
        },
    ] {
        assert!(
            matches!(
                manager.enqueue(request.clone()),
                Err(SftpError::InvalidPath(_))
            ),
            "{request:?}"
        );
    }

    // A local link pointing outside the sandbox is refused for upload and download targets.
    let outside = scratch
        .sandbox
        .parent()
        .unwrap()
        .join(format!("outside-{}", std::process::id()));
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"host secret").unwrap();
    symlink(outside.join("secret.txt"), scratch.sandbox.join("leak.txt")).unwrap();
    symlink(&outside, scratch.sandbox.join("leakdir")).unwrap();
    let id = manager
        .enqueue(TransferRequest::upload("leak.txt", scratch.remote_dir()))
        .unwrap();
    assert!(matches!(
        wait_finished(&manager, id).state,
        TransferState::Failed(SftpError::SymlinkRefused(_))
    ));
    assert!(!scratch.server.join("leak.txt").exists());
    let id = manager
        .enqueue(TransferRequest::download(
            scratch.remote("real.txt"),
            "leakdir",
        ))
        .unwrap();
    assert!(matches!(
        wait_finished(&manager, id).state,
        TransferState::Failed(SftpError::SymlinkRefused(_))
    ));
    assert!(!outside.join("real.txt").exists());
    // A local link inside the sandbox is followed.
    fs::write(scratch.sandbox.join("inside.txt"), b"inside").unwrap();
    symlink("inside.txt", scratch.sandbox.join("inside-link.txt")).unwrap();
    let id = manager
        .enqueue(TransferRequest::upload(
            "inside-link.txt",
            scratch.remote_dir(),
        ))
        .unwrap();
    assert_eq!(wait_finished(&manager, id).state, TransferState::Completed);
    assert_eq!(
        fs::read(scratch.server.join("inside-link.txt")).unwrap(),
        b"inside"
    );
    let _ = fs::remove_dir_all(&outside);
}

#[test]
#[ignore = "needs scripts/run-sftp-integration.sh"]
fn queue_bounds_history_and_closed_client() {
    let scratch = Scratch::new("bounds");
    let client = connect();
    let manager = TransferManager::new(
        client.clone(),
        scratch.local_fs(),
        TransferConfig {
            max_queued: 3,
            max_concurrent: 1,
            max_history: 2,
            ..TransferConfig::default()
        },
    );
    write_pattern_file(&scratch.server.join("q.bin"), 8 * 1024 * 1024, 3);
    let mut ids = Vec::new();
    for index in 0..3 {
        ids.push(
            manager
                .enqueue(TransferRequest {
                    name: Some(format!("q{index}.bin")),
                    ..TransferRequest::download(scratch.remote("q.bin"), "")
                })
                .unwrap(),
        );
    }
    assert!(matches!(
        manager.enqueue(TransferRequest::download(scratch.remote("q.bin"), "")),
        Err(SftpError::LimitExceeded(_))
    ));
    for id in &ids {
        assert_eq!(wait_finished(&manager, *id).state, TransferState::Completed);
    }
    manager
        .enqueue(TransferRequest {
            name: Some("q0.bin".into()),
            ..TransferRequest::download(scratch.remote("q.bin"), "")
        })
        .map(|id| {
            wait_until(&manager, id, "conflict", |i| {
                matches!(i.state, TransferState::AwaitingConflict(_))
            });
            manager.cancel(id);
        })
        .unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(manager.snapshot().len() <= 3, "history is pruned");
    manager.clear_finished();
    assert!(manager.snapshot().is_empty());

    client.close();
    assert!(matches!(
        client.list(&scratch.remote_dir(), &ListOptions::default()),
        Err(SftpError::Disconnected(_))
    ));
    let id = manager.enqueue(TransferRequest::download(scratch.remote("q.bin"), "x"));
    if let Ok(id) = id {
        assert!(matches!(
            wait_finished(&manager, id).state,
            TransferState::Failed(_)
        ));
    }
}
