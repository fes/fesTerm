//! HOST-only rooted service composition. Authentication and protected file
//! consent are independent of daemon/session grants and terminal admission.
use crate::{
    file_protocol::{self as rpc, HostPolicy},
    filesystem::{self, Filesystem},
    host_control::{
        AuthorityKind, ConnectionAuthority, PolicyGuard, PolicyView, ProtectedPolicySource,
    },
    owner_auth::ConnectionGuard,
    wire::{read_client_frame, write_frame, DEADLINE},
};
use festerm_host_protocol::ContractError;
use festerm_tunnel_client::host_control::ConnectionPermit;
use std::{io, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

pub(crate) struct Prepared {
    policy: HostPolicy,
    filesystem: Arc<Filesystem>,
}
impl Prepared {
    /// Native setup only, outside the protected policy lease. Preparing a root
    /// grants no authority: every use must still match current protected consent.
    pub(crate) fn open(policy: HostPolicy) -> io::Result<Arc<Self>> {
        policy.scope.validate()?;
        let root = policy.filesystem.as_ref().ok_or_else(denied)?;
        root.validate()?;
        let filesystem = Filesystem::open(root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "host.filesystem_root_unavailable",
            )
        })?;
        Ok(Arc::new(Self { policy, filesystem }))
    }
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "HOST file access is unavailable; review current remote-access settings",
    )
}
fn authorization(error: ContractError) -> io::Error {
    if error == ContractError::Busy {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "HOST file-access settings are busy",
        )
    } else {
        denied()
    }
}
type Current = Arc<dyn Fn() -> io::Result<HostPolicy> + Send + Sync>;
async fn current(read: Current, cancel: &CancellationToken) -> io::Result<HostPolicy> {
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(denied()),
        result = tokio::time::timeout(DEADLINE, tokio::task::spawn_blocking(move || read())) =>
            result.map_err(|_| denied())?.map_err(|_| denied())?,
    }
}

pub(crate) async fn serve<A: ProtectedPolicySource>(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    permit: ConnectionPermit,
    owner: ConnectionAuthority,
    guard: ConnectionGuard,
    source: Arc<A>,
    prepared: Option<Arc<Prepared>>,
    cancel: CancellationToken,
) -> io::Result<()> {
    let _connection_guard = guard;
    let ConnectionAuthority::Ordinary(capability) = owner else {
        return Err(denied());
    };
    // The current-policy closure is invoked only on existing blocking workers.
    // Capturing this permit keeps abandoned admission jobs inside HOST budgets.
    let permit = Arc::new(permit);
    let stopped = cancel.clone();
    let read: Current = Arc::new(move || {
        let _charge = &permit;
        if stopped.is_cancelled() {
            return Err(denied());
        }
        let current = source
            .current_policy_for(AuthorityKind::Ordinary)
            .map_err(authorization)?;
        let PolicyView::Ordinary(policy) = current.view() else {
            return Err(denied());
        };
        let policy = capability
            .authorize_files(policy)
            .map_err(authorization)?
            .clone();
        Ok(policy) // current guard/lease drops before any filesystem/network I/O
    });
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(denied()),
        result = serve_inner(stream, read, prepared, cancel.clone()) => result,
    }
}

async fn serve_inner(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    read: Current,
    prepared: Option<Arc<Prepared>>,
    cancel: CancellationToken,
) -> io::Result<()> {
    let captured = current(read.clone(), &cancel).await?;
    let filesystem = match (&captured.filesystem, prepared) {
        (None, _) => None,
        (Some(_), Some(prepared)) if captured == prepared.policy => {
            Some(prepared.filesystem.clone())
        }
        _ => return Err(denied()),
    };
    write_frame(
        &mut stream,
        rpc::HOST_DESCRIPTION,
        &serde_json::to_vec(&rpc::HostDescription {
            version: if filesystem.as_ref().is_some_and(|fs| fs.has_locations()) {
                rpc::HOST_LOCATIONS_VERSION
            } else {
                rpc::HOST_VERSION
            },
            scope: captured.scope.clone(),
            filesystem: filesystem.as_ref().map(|root| root.capabilities()),
        })?,
    )
    .await?;
    let Some(filesystem) = filesystem else {
        return Ok(());
    };
    let (kind, bytes) = tokio::time::timeout(DEADLINE, read_client_frame(&mut stream))
        .await
        .map_err(|_| denied())??;
    if kind != rpc::OPEN {
        return Err(denied());
    }
    let open: rpc::HostOpen = serde_json::from_slice(&bytes).map_err(|_| denied())?;
    open.scope.validate()?;
    if (if filesystem.has_locations() {
        open.version != rpc::HOST_LOCATIONS_VERSION
    } else {
        !matches!(open.version, rpc::HOST_VERSION | rpc::HOST_REVIEWED_VERSION)
    }) || open.scope != captured.scope
    {
        return Err(denied());
    }
    let admission: filesystem::Admission = Arc::new(move |_| {
        if read()? != captured {
            return Err(denied());
        }
        Ok(())
    });
    if matches!(
        open.version,
        rpc::HOST_REVIEWED_VERSION | rpc::HOST_LOCATIONS_VERSION
    ) {
        filesystem::serve_authorized_reviewed(stream, filesystem, admission, cancel).await
    } else {
        filesystem::serve_authorized(stream, filesystem, admission, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn location_channel(
        prepared: Arc<Prepared>,
        reviewed: bool,
    ) -> (
        Result<
            Option<(
                crate::file_client::Client<tokio::io::DuplexStream>,
                rpc::HostScope,
            )>,
            crate::file_client::Error,
        >,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let (stream, peer) = tokio::io::duplex(8192);
        let scope = prepared.policy.scope.clone();
        let captured = prepared.policy.clone();
        let server = tokio::spawn(serve_inner(
            peer,
            Arc::new(move || Ok(captured.clone())),
            Some(prepared),
            CancellationToken::new(),
        ));
        let opened = if reviewed {
            crate::file_client::Client::open_host_reviewed(
                stream,
                &scope.host,
                &scope.installation,
                CancellationToken::new(),
            )
            .await
        } else {
            let guard = crate::host_client::ordinary::OrdinaryAccountGuard::new(
                CancellationToken::new(),
                Arc::new(|| Ok(())),
            );
            crate::file_client::Client::open_host_guarded(
                stream,
                &scope.host,
                &scope.installation,
                Some(&scope),
                guard,
                CancellationToken::new(),
            )
            .await
        };
        (opened, server)
    }
    async fn drain_location_channel(mut server: tokio::task::JoinHandle<io::Result<()>>) {
        if tokio::time::timeout(std::time::Duration::from_secs(3), &mut server)
            .await
            .is_err()
        {
            server.abort();
            let _ = server.await;
            panic!("owned Files channel did not drain")
        }
    }
    #[tokio::test]
    async fn allowed_locations_current_transfer_guard_retirement_sends_no_open_or_metadata_request()
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for before_description in [true, false] {
            let scope = rpc::HostScope {
                host: Default::default(),
                installation: Default::default(),
                epoch: "1".into(),
            };
            let (stream, mut peer) = tokio::io::duplex(8192);
            let description = rpc::HostDescription {
                version: rpc::HOST_LOCATIONS_VERSION,
                scope: scope.clone(),
                filesystem: Some(rpc::Capabilities {
                    version: rpc::VERSION,
                    write: true,
                    chunk_limit: rpc::CHUNK_LIMIT,
                    page_limit: rpc::PAGE_LIMIT,
                    handle_limit: rpc::HANDLE_LIMIT,
                }),
            };
            write_frame(
                &mut peer,
                rpc::HOST_DESCRIPTION,
                &serde_json::to_vec(&description).unwrap(),
            )
            .await
            .unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let guard = crate::host_client::ordinary::OrdinaryAccountGuard::new(
                CancellationToken::new(),
                Arc::new(move || {
                    let index = observed.fetch_add(1, Ordering::SeqCst);
                    if before_description || index > 0 {
                        Err(festerm_host_protocol::ContractError::Stale)
                    } else {
                        Ok(())
                    }
                }),
            );
            assert!(matches!(
                crate::file_client::Client::open_host_guarded(
                    stream,
                    &scope.host,
                    &scope.installation,
                    Some(&scope),
                    guard,
                    CancellationToken::new()
                )
                .await,
                Err(crate::file_client::Error::Cancelled)
            ));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                if before_description { 1 } else { 2 }
            );
            assert!(
                crate::wire::read_frame(&mut peer).await.is_err(),
                "retirement must emit no Open4 or locations/start request"
            );
        }
    }
    #[tokio::test]
    async fn allowed_locations_two_channels_share_selectors_captured_paths_and_one_budget() {
        #[cfg(unix)]
        let home_fixture = tempfile::Builder::new()
            .prefix(".festerm-locations-channels-")
            .tempdir_in(festerm_unix_security::current_user_home().unwrap())
            .unwrap();
        #[cfg(unix)]
        let base = home_fixture.path().to_owned();
        #[cfg(windows)]
        let base = crate::test_support::private_directory("festerm-locations-channels");
        let first = base.join("first");
        let second = base.join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(second.join("saved.txt"), b"second original").unwrap();
        let policy = rpc::Policy {
            root: first.clone(),
            start_in: None,
            locations: Some(rpc::AllowedLocations {
                roots: vec![first.clone(), second.clone()],
                start: None,
            }),
            write: true,
            delete: true,
        };
        let prepared = Prepared::open(HostPolicy {
            scope: rpc::HostScope {
                host: Default::default(),
                installation: Default::default(),
                epoch: "1".into(),
            },
            filesystem: Some(policy),
        })
        .unwrap();
        // A historical opening mode must refuse before interpreting selectors.
        let (old_stream, old_peer) = tokio::io::duplex(8192);
        let old_prepared = prepared.clone();
        let old_policy = prepared.policy.clone();
        let old_server = tokio::spawn(serve_inner(
            old_peer,
            Arc::new(move || Ok(old_policy.clone())),
            Some(old_prepared),
            CancellationToken::new(),
        ));
        assert!(crate::file_client::Client::open_host(
            old_stream,
            &prepared.policy.scope.host,
            &prepared.policy.scope.installation,
            CancellationToken::new()
        )
        .await
        .is_err());
        drain_location_channel(old_server).await;
        let (browse, server1) = location_channel(prepared.clone(), true).await;
        let (mut browse, _) = browse.unwrap().unwrap();
        let roots = browse.locations().await.unwrap().unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(browse.start_directory().await.unwrap(), "/");
        let captured = format!("{}/saved.txt", roots[0].virtual_root());
        let (transfer, server2) = location_channel(prepared.clone(), false).await;
        let (mut transfer, _) = transfer.unwrap().unwrap();
        assert!(
            transfer.location_metadata().is_none(),
            "transfer never re-queries or remaps browsing selectors"
        );
        let (third, server3) = location_channel(prepared.clone(), false).await;
        assert!(
            third.is_err(),
            "all roots share the original two-channel pool"
        );
        drain_location_channel(server3).await;
        if roots.iter().all(|root| root.available) {
            let changed = browse
                .open_directory(&roots[1].virtual_root())
                .await
                .unwrap();
            let _ = browse
                .read_directory(&changed, rpc::PAGE_LIMIT)
                .await
                .unwrap();
            browse.close(&changed).await.unwrap();
            let payload = b"captured first root";
            let upload = transfer
                .begin_upload(&captured, payload.len() as u64, None)
                .await
                .unwrap();
            transfer.write(&upload, payload).await.unwrap();
            transfer.finish_upload(&upload).await.unwrap();
            let (read, metadata) = transfer.open_read(&captured).await.unwrap();
            assert_eq!(metadata.size, payload.len().to_string());
            assert_eq!(transfer.read(&read, 0, 64).await.unwrap().bytes, payload);
            transfer.close(&read).await.unwrap();
            assert_eq!(std::fs::read(first.join("saved.txt")).unwrap(), payload);
            assert_eq!(
                std::fs::read(second.join("saved.txt")).unwrap(),
                b"second original"
            );
        } else {
            #[cfg(target_os = "macos")]
            {
                let root = festerm_unix_security::open_absolute_directory_read_nofollow(
                    std::path::Path::new("/"),
                )
                .unwrap();
                assert_eq!(
                    festerm_unix_security::open_contained_data_directory(
                        &root,
                        first.strip_prefix("/").unwrap()
                    )
                    .unwrap_err()
                    .kind(),
                    io::ErrorKind::Unsupported
                );
            }
            #[cfg(not(target_os = "macos"))]
            panic!("supported local fixture unexpectedly unavailable");
            assert!(transfer.open_read(&captured).await.is_err());
            assert!(!first.join("saved.txt").exists());
            assert_eq!(
                std::fs::read(second.join("saved.txt")).unwrap(),
                b"second original"
            );
            eprintln!("allowlist native prerequisite unavailable; new-scope refused without effects, shared namespace/budget still checked");
        }
        browse.cancel();
        transfer.cancel();
        drain_location_channel(server1).await;
        drain_location_channel(server2).await;
        std::fs::remove_dir_all(base).unwrap();
    }
    #[tokio::test]
    async fn prepared_root_survives_missing_start_directory_and_serves_root() {
        let path = crate::test_support::private_directory("festerm-start-prepared");
        std::fs::write(path.join("visible.txt"), b"public").unwrap();
        let scope = rpc::HostScope {
            host: Default::default(),
            installation: Default::default(),
            epoch: "1".into(),
        };
        // The same preparation path used by HOST runtime must retain only root
        // admission: losing a navigation preference cannot disable shells/HOST.
        let prepared = Prepared::open(HostPolicy {
            scope: scope.clone(),
            filesystem: Some(rpc::Policy {
                root: path.clone(),
                start_in: Some("/missing".into()),
                locations: None,
                write: false,
                delete: false,
            }),
        })
        .unwrap();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let mut server = tokio::spawn(async move {
            write_frame(
                &mut peer,
                rpc::HOST_DESCRIPTION,
                &serde_json::to_vec(&rpc::HostDescription {
                    version: rpc::HOST_VERSION,
                    scope: prepared.policy.scope.clone(),
                    filesystem: Some(prepared.filesystem.capabilities()),
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let (kind, bytes) = crate::wire::read_frame(&mut peer).await.unwrap();
            assert_eq!(kind, rpc::OPEN);
            let open: rpc::HostOpen = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(open.scope, prepared.policy.scope);
            let _ = filesystem::serve_authorized_reviewed(
                peer,
                prepared.filesystem.clone(),
                Arc::new(|_| Ok(())),
                stop,
            )
            .await;
        });
        let (mut client, _) = crate::file_client::Client::open_host_reviewed(
            stream,
            &scope.host,
            &scope.installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            client.start_directory().await,
            Err(crate::file_client::Error::Remote {
                code: rpc::ErrorCode::NotFound,
                ..
            })
        ));
        let directory = client.open_directory("/").await.unwrap();
        let page = client
            .read_directory(&directory, rpc::PAGE_LIMIT)
            .await
            .unwrap();
        assert!(page.entries.iter().any(|entry| entry.name == "visible.txt"));
        client.close(&directory).await.unwrap();
        client.cancel();
        cancel.cancel();
        match tokio::time::timeout(std::time::Duration::from_secs(3), &mut server).await {
            Ok(result) => result.unwrap(),
            Err(error) => {
                server.abort();
                let _ = server.await;
                panic!("{error}");
            }
        }
        std::fs::remove_dir_all(path).unwrap();
    }
}
