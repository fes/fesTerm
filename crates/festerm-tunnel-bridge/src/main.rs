use festerm_sessiond::NativeSessionIdentity;
use festerm_tunnel_bridge::{
    protocol::{invalid, Target},
    relay::{self, Destination},
    transport,
};
use std::{io, path::PathBuf, time::Duration};

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
        println!("festerm-tunnel-bridge --experimental --tunnel-id ID --cluster CLUSTER --port PORT --session NAME --destination PATH [--token-stdin] [--allow-file-read]\nPrivate GitHub-authenticated Dev Tunnels provisioning first; runtime takes only a scoped host token. The configured session must already exist and be unattached. Ctrl+C detaches, never terminates. File reads are opt-in and grant remote-user-readable regular files, not a directory sandbox. No create/inventory/takeover/terminate or automatic reconnect.");
        return Ok(());
    }
    let mut tunnel = None;
    let mut cluster = None;
    let mut port = None;
    let mut name = None;
    let mut destination = None;
    let mut experimental = false;
    let mut stdin = false;
    let mut allow_file_read = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--experimental" => experimental = true,
            "--token-stdin" => stdin = true,
            "--allow-file-read" => allow_file_read = true,
            "--tunnel-id" => tunnel = iter.next().cloned(),
            "--cluster" => cluster = iter.next().cloned(),
            "--port" => port = iter.next().and_then(|value| value.parse::<u16>().ok()),
            "--session" => name = iter.next().cloned(),
            "--destination" => destination = iter.next().map(PathBuf::from),
            _ => return Err(invalid()),
        }
    }
    if !experimental {
        return Err(invalid());
    }
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
    let identity = NativeSessionIdentity {
        name: selected.name.clone(),
        pid: selected.pid,
        created_at_unix_ms: selected.created_at_unix_ms,
        endpoint: selected.endpoint,
    };
    let token = transport::read_token(stdin)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async move {
        let mut host = tokio::time::timeout(
            Duration::from_secs(30),
            relay::host(&tunnel, &cluster, port, &token),
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
        // including a previous host binding. Restart needs deliberate refresh.
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
                    let Some(stream) = incoming else { return Err(invalid()); };
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
