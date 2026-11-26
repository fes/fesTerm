//! Mobile review transport over the shared retained-handle deletion owner.
use super::*;
use crate::files::deletion::{Outcome, Review};
use festerm_ssh::RemoteTransferEndpoint;
use festerm_tunnel_bridge::files_endpoint::{DeleteObservation, FilesEndpoint};

type Sender<T> = mpsc::SyncSender<Result<T, String>>;
pub(super) enum Command {
    Prepare(Vec<String>, CancellationToken, Sender<Review>),
    Commit(u64, Sender<Outcome>),
    Cancel,
}

/// False retires this stream. A timed-out mutation is never replayed.
pub(super) async fn execute<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    endpoint: &mut FilesEndpoint<S>,
    command: Command,
) -> bool {
    match command {
        Command::Prepare(paths, stop, reply) => {
            if paths.is_empty() || paths.len() > 8 || paths.iter().any(|p| rooted(p).is_err()) {
                let _ = reply.send(Err("Select up to eight files or empty folders".into()));
                return true;
            }
            let result =
                tokio::time::timeout(DEADLINE, endpoint.prepare_delete(&paths, stop.clone())).await;
            match result {
                Ok(Ok((id, items))) => {
                    let review = Review {
                        id,
                        local_details: None,
                        names: items.into_iter().map(|item| item.name).collect(),
                    };
                    if reply.send(Ok(review)).is_err() || stop.is_cancelled() {
                        stop.cancel();
                        return matches!(
                            tokio::time::timeout(DEADLINE, endpoint.cancel_delete_review()).await,
                            Ok(Ok(()))
                        );
                    }
                    true
                }
                Ok(Err(_)) => {
                    let _ = reply.send(Err(
                        "Could not review these entries. Refresh and select again.".into(),
                    ));
                    endpoint.check_current().is_ok()
                }
                Err(_) => {
                    let _ = reply.send(Err(
                        "Deletion review timed out. Reconnect before trying again.".into(),
                    ));
                    false
                }
            }
        }
        Command::Commit(id, reply) => {
            let observation = DeleteObservation::default();
            let result =
                tokio::time::timeout(DEADLINE, endpoint.commit_delete(id, &observation)).await;
            let (outcome, live) = match result {
                Ok(result) => (
                    Outcome {
                        confirmed: result.confirmed,
                        total: result.total,
                        unknown: result.failure.as_ref().is_some_and(|(unknown, _)| *unknown),
                        failed: result.failure.is_some(),
                        recovery: None,
                    },
                    endpoint.check_current().is_ok(),
                ),
                Err(_) => {
                    let counts = observation.confirmed_before_interruption();
                    (
                        Outcome {
                            confirmed: counts.map_or(0, |(confirmed, _)| confirmed),
                            total: counts.map_or(0, |(_, total)| total),
                            unknown: observation.result_is_uncertain(),
                            failed: true,
                            recovery: None,
                        },
                        false,
                    )
                }
            };
            let _ = reply.send(Ok(outcome));
            live
        }
        Command::Cancel => matches!(
            tokio::time::timeout(DEADLINE, endpoint.cancel_delete_review()).await,
            Ok(Ok(()))
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_tunnel_bridge::{
        files_endpoint::{FilesChannel, FirstFailure},
        wire::{read_frame, write_frame},
    };
    use tokio::io::DuplexStream;

    fn scope() -> rpc::HostScope {
        rpc::HostScope {
            host: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap(),
            installation: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
            epoch: "7".into(),
        }
    }
    async fn send(peer: &mut DuplexStream, id: u32, result: rpc::Result) {
        write_frame(
            peer,
            rpc::RESPONSE,
            &serde_json::to_vec(&rpc::Response {
                id,
                result: Some(result),
                error: None,
                recovery_path: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    }
    async fn request(peer: &mut DuplexStream) -> rpc::Request {
        let (kind, data) = read_frame(peer).await.unwrap();
        assert_eq!(kind, rpc::REQUEST);
        serde_json::from_slice(&data).unwrap()
    }
    async fn handshake(peer: &mut DuplexStream) {
        write_frame(
            peer,
            rpc::HOST_DESCRIPTION,
            &serde_json::to_vec(&rpc::HostDescription {
                version: rpc::HOST_VERSION,
                scope: scope(),
                filesystem: Some(rpc::Capabilities {
                    version: rpc::VERSION,
                    write: true,
                    chunk_limit: rpc::CHUNK_LIMIT,
                    page_limit: rpc::PAGE_LIMIT,
                    handle_limit: rpc::HANDLE_LIMIT,
                }),
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let (kind, data) = read_frame(peer).await.unwrap();
        assert_eq!(kind, rpc::OPEN);
        let open: rpc::HostOpen = serde_json::from_slice(&data).unwrap();
        assert_eq!(open.version, rpc::HOST_REVIEWED_VERSION);
        assert_eq!(open.scope, scope());
        send(
            peer,
            0,
            rpc::Result::ReadyV3 {
                capabilities: rpc::OperationCapabilities {
                    version: rpc::HOST_REVIEWED_VERSION,
                    write: true,
                    operations: vec!["delete".into()],
                    chunk_limit: rpc::CHUNK_LIMIT,
                    page_limit: rpc::PAGE_LIMIT,
                    handle_limit: rpc::HANDLE_LIMIT,
                },
            },
        )
        .await;
    }
    async fn endpoint(stream: DuplexStream) -> FilesEndpoint<DuplexStream> {
        let (client, _) = file_client::Client::open_host_reviewed(
            stream,
            &scope().host,
            &scope().installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        FilesEndpoint::from_client(
            client,
            CancellationToken::new(),
            FirstFailure::default(),
            FilesChannel::Browsing,
        )
    }
    async fn review(peer: &mut DuplexStream, handle: u32) {
        let call = request(peer).await;
        assert!(matches!(
            call.operation,
            rpc::Operation::ReviewEntry {
                purpose: rpc::ReviewPurpose::Delete,
                ..
            }
        ));
        send(
            peer,
            call.id,
            rpc::Result::EntryReview {
                handle,
                metadata: rpc::Metadata {
                    kind: rpc::Kind::File,
                    size: "13".into(),
                    modified_ms: Some("1".into()),
                },
            },
        )
        .await;
    }
    #[tokio::test]
    async fn host_files_delete_cancel_and_stale_review_preserve_stream_without_mutation() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(&mut peer).await;
            review(&mut peer, 19).await;
            let close = request(&mut peer).await;
            assert!(matches!(
                close.operation,
                rpc::Operation::Close { handle: 19 }
            ));
            send(&mut peer, close.id, rpc::Result::Closed).await;
            let ping = request(&mut peer).await;
            assert!(matches!(ping.operation, rpc::Operation::Ping {}));
            send(&mut peer, ping.id, rpc::Result::Pong).await;
        });
        let mut endpoint = endpoint(stream).await;
        let stop = CancellationToken::new();
        let (send, receive) = mpsc::sync_channel(1);
        assert!(
            execute(
                &mut endpoint,
                Command::Prepare(vec!["/public.txt".into()], stop.clone(), send)
            )
            .await
        );
        let review = receive.recv().unwrap().unwrap();
        assert_eq!(review.names, ["public.txt"]);
        stop.cancel();
        assert!(execute(&mut endpoint, Command::Cancel).await);
        let (send, receive) = mpsc::sync_channel(1);
        assert!(execute(&mut endpoint, Command::Commit(review.id, send)).await);
        let outcome = receive.recv().unwrap().unwrap();
        assert!(outcome.failed && !outcome.unknown);
        assert_eq!((outcome.confirmed, outcome.total), (0, 0));
        endpoint.ping().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn host_files_delete_partial_unobserved_reply_keeps_counts_and_never_replays() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(&mut peer).await;
            for handle in 20..23 {
                review(&mut peer, handle).await;
            }
            let first = request(&mut peer).await;
            assert!(matches!(
                first.operation,
                rpc::Operation::Delete { review: 20 }
            ));
            send(&mut peer, first.id, rpc::Result::Deleted).await;
            let second = request(&mut peer).await;
            assert!(matches!(
                second.operation,
                rpc::Operation::Delete { review: 21 }
            ));
            // A received mutation without its result; dropping the peer must
            // not cause a third request or an automatic reconnect/replay.
        });
        let mut endpoint = endpoint(stream).await;
        let (send, receive) = mpsc::sync_channel(1);
        assert!(
            execute(
                &mut endpoint,
                Command::Prepare(
                    vec!["/one".into(), "/two".into(), "/untouched".into()],
                    CancellationToken::new(),
                    send,
                )
            )
            .await
        );
        let review = receive.recv().unwrap().unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        assert!(!execute(&mut endpoint, Command::Commit(review.id, send)).await);
        let outcome = receive.recv().unwrap().unwrap();
        assert!(outcome.unknown && outcome.failed);
        assert_eq!((outcome.confirmed, outcome.total), (1, 3));
        assert!(endpoint.client_without_reviews().is_err());
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}
