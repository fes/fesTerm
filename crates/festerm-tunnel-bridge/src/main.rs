use festerm_sessiond::NativeSessionIdentity;
use festerm_tunnel_bridge::{
    host_identity::HostIdentity,
    protocol::{invalid, Target},
    relay::{self, Destination},
    transport,
};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

fn main() {
    if run().is_err() {
        eprintln!("Experimental Dev Tunnel bridge stopped/refused. Verify the exact unattached protocol-3 session, dedicated host token, preprovisioned port and service access. No session was created or terminated.");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--version"] {
        println!("festerm-tunnel-bridge {} experimental bridge protocol 1 / daemon protocol 3; Dev Tunnels SDK b73b0eb774b86a5b519c300f2f41c9072a66f663", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--help") {
        println!("festerm-tunnel-bridge --experimental --tunnel-id ID --cluster CLUSTER --port PORT --session NAME --destination PATH [--token-stdin] [--allow-file-read] [--host-identity ABSOLUTE_PRIVATE_PATH [--existing-host-identity]] [--session-pid PID --session-generation U128]\nPrivate GitHub-authenticated Dev Tunnels provisioning first; runtime takes only a scoped host token. The configured session must already exist and be unattached. Optional exact pid/generation pins refuse name reuse. A host identity file contains a dedicated unencrypted signing key protected for this OS user, never a bearer token. --existing-host-identity requires --host-identity and refuses a missing key instead of creating one. Without a host identity path identity remains ephemeral. Ctrl+C detaches, never terminates. File reads are opt-in and grant remote-user-readable regular files, not a directory sandbox. No create/inventory/takeover/terminate or automatic reconnect.");
        return Ok(());
    }
    let mut tunnel = None;
    let mut cluster = None;
    let mut port = None;
    let mut name = None;
    let mut destination = None;
    let mut host_identity = None;
    let mut existing_host_identity = false;
    let mut expected_pid = None;
    let mut expected_generation = None;
    let mut experimental = false;
    let mut stdin = false;
    let mut allow_file_read = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--experimental" => experimental = true,
            "--token-stdin" => stdin = true,
            "--allow-file-read" => allow_file_read = true,
            "--existing-host-identity" => existing_host_identity = true,
            "--tunnel-id" => tunnel = iter.next().cloned(),
            "--cluster" => cluster = iter.next().cloned(),
            "--port" => port = iter.next().and_then(|value| value.parse::<u16>().ok()),
            "--session" => name = iter.next().cloned(),
            "--destination" => destination = iter.next().map(PathBuf::from),
            "--host-identity" => {
                if host_identity.is_some() {
                    return Err(invalid());
                }
                host_identity = Some(PathBuf::from(iter.next().ok_or_else(invalid)?));
            }
            "--session-pid" => {
                if expected_pid.is_some() {
                    return Err(invalid());
                }
                expected_pid = Some(
                    iter.next()
                        .ok_or_else(invalid)?
                        .parse::<u32>()
                        .map_err(|_| invalid())?,
                );
            }
            "--session-generation" => {
                if expected_generation.is_some() {
                    return Err(invalid());
                }
                expected_generation = Some(
                    iter.next()
                        .ok_or_else(invalid)?
                        .parse::<u128>()
                        .map_err(|_| invalid())?,
                );
            }
            _ => return Err(invalid()),
        }
    }
    if !experimental {
        return Err(invalid());
    }
    validate_host_identity_options(host_identity.as_deref(), existing_host_identity)?;
    let expected_session = exact_session_pin(expected_pid, expected_generation)?;
    let (tunnel, cluster, port, name, path) = (
        tunnel.ok_or_else(invalid)?,
        cluster.ok_or_else(invalid)?,
        port.ok_or_else(invalid)?,
        name.ok_or_else(invalid)?,
        destination.ok_or_else(invalid)?,
    );
    let candidates = festerm_sessiond::list_unattached_local_sessions().map_err(|_| invalid())?;
    let selected = candidates
        .into_iter()
        .find(|session| session.name == name)
        .ok_or_else(invalid)?;
    verify_session_pin(expected_session, selected.pid, selected.created_at_unix_ms)?;
    let identity = NativeSessionIdentity {
        name: selected.name.clone(),
        pid: selected.pid,
        created_at_unix_ms: selected.created_at_unix_ms,
        endpoint: selected.endpoint,
    };
    let token = transport::read_token(stdin)?;
    let host_identity = load_host_identity(host_identity.as_deref(), existing_host_identity)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async move {
        let mut host = tokio::time::timeout(
            Duration::from_secs(30),
            relay::host_with_identity(&tunnel, &cluster, port, &token, host_identity),
        )
        .await
        .map_err(|_| std::io::Error::other("host.establishment_timeout"))??;
        drop(token);
        let metadata = Destination {
            file_read: allow_file_read,
            path_style: festerm_tunnel_bridge::files::PathStyle::native(),
            version: 1,
            tunnel_id: tunnel,
            cluster,
            port,
            target: Target {
                name: selected.name,
                pid: identity.pid,
                generation: identity.created_at_unix_ms.to_string(),
            },
            endpoint: host.endpoint.clone(),
        };
        // create_new avoids overwriting an operator's existing destination,
        // including a previous host binding. Managed setup supplies a fresh
        // per-run destination then checkpoints its public metadata separately.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                std::io::Error::other(if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "host.metadata_create.already_exists"
                } else {
                    "host.metadata_create"
                })
            })?;
        serde_json::to_writer_pretty(file, &metadata)
            .map_err(|_| std::io::Error::other("host.metadata_encode"))?;
        eprintln!("Experimental bridge ready: one configured session; attach/detach only.");
        if allow_file_read { eprintln!("Read-only file capability enabled: connect credentials also authorize bounded regular-file reads as this remote OS user."); }
        let files = festerm_tunnel_bridge::files::FileService::new(allow_file_read);
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                incoming = host.connections.recv() => {
                    let Some(stream) = incoming else {
                        return Err(std::io::Error::other(
                            host.failure_category().unwrap_or("host.outer_task_closed"),
                        ));
                    };
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => break,
                        _ = transport::serve_with_files(stream, identity.clone(), files.clone()) => {}
                    }
                }
            }
        }
        Ok(())
    });
    // Native filesystem I/O cannot be forcibly cancelled. A held reservation
    // prevents another job; shutdown must not wait indefinitely for that job.
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

fn validate_host_identity_options(path: Option<&Path>, existing_only: bool) -> io::Result<()> {
    if (existing_only && path.is_none()) || path.is_some_and(|path| !path.is_absolute()) {
        return Err(invalid());
    }
    Ok(())
}

fn load_host_identity(path: Option<&Path>, existing_only: bool) -> io::Result<HostIdentity> {
    validate_host_identity_options(path, existing_only)?;
    match (path, existing_only) {
        (Some(path), true) => HostIdentity::load(path),
        (Some(path), false) => HostIdentity::load_or_create(path),
        (None, false) => HostIdentity::ephemeral(),
        (None, true) => Err(invalid()),
    }
}

fn exact_session_pin(
    pid: Option<u32>,
    generation: Option<u128>,
) -> io::Result<Option<(u32, u128)>> {
    match (pid, generation) {
        (None, None) => Ok(None),
        (Some(pid), Some(generation)) if pid > 0 && generation > 0 => Ok(Some((pid, generation))),
        _ => Err(invalid()),
    }
}

fn verify_session_pin(expected: Option<(u32, u128)>, pid: u32, generation: u128) -> io::Result<()> {
    if expected.is_some_and(|pin| pin != (pid, generation)) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_host_identity_requires_explicit_absolute_path() {
        assert!(validate_host_identity_options(None, true).is_err());
        assert!(validate_host_identity_options(Some(Path::new("relative.key")), true).is_err());
        assert!(validate_host_identity_options(None, false).is_ok());
        let absolute = std::env::current_dir().unwrap().join("host-identity.key");
        assert!(validate_host_identity_options(Some(&absolute), true).is_ok());
        assert!(validate_host_identity_options(Some(&absolute), false).is_ok());
    }

    #[test]
    fn existing_host_identity_refuses_missing_key_without_regeneration() {
        struct OwnedDirectory(PathBuf);
        impl Drop for OwnedDirectory {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let path = std::env::current_dir().unwrap().join(format!(
            ".bridge-existing-identity-test-{}",
            uuid::Uuid::new_v4()
        ));
        let directory =
            festerm_tunnel_bridge::setup_storage::private_directory(&path, true).unwrap();
        let owned = OwnedDirectory(path);
        let key = owned.0.join("host-identity.key");
        assert!(load_host_identity(Some(&key), true).is_err());
        assert!(!key.exists());
        let created = load_host_identity(Some(&key), false).unwrap();
        let reloaded = load_host_identity(Some(&key), true).unwrap();
        assert_eq!(created.host_id(), reloaded.host_id());
        assert_eq!(
            created.fingerprint().unwrap(),
            reloaded.fingerprint().unwrap()
        );
        std::fs::remove_file(&key).unwrap();
        assert!(load_host_identity(Some(&key), true).is_err());
        assert!(!key.exists());
        drop(directory);
    }

    #[test]
    fn managed_session_pin_requires_complete_nonzero_exact_identity() {
        assert_eq!(exact_session_pin(None, None).unwrap(), None);
        assert_eq!(
            exact_session_pin(Some(42), Some(123)).unwrap(),
            Some((42, 123))
        );
        assert!(exact_session_pin(Some(42), None).is_err());
        assert!(exact_session_pin(None, Some(123)).is_err());
        assert!(exact_session_pin(Some(0), Some(123)).is_err());
        assert!(exact_session_pin(Some(42), Some(0)).is_err());
        assert!(verify_session_pin(Some((42, 123)), 42, 123).is_ok());
        assert!(verify_session_pin(Some((42, 123)), 42, 124).is_err());
        assert!(verify_session_pin(Some((42, 123)), 43, 123).is_err());
        assert!(verify_session_pin(None, 42, 124).is_ok());
    }
}
