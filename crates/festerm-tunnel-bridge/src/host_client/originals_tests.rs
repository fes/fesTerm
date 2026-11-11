use super::super::creation_tests::{caps, client, context, creation, handshake, respond};
use super::*;
use festerm_host_protocol::{EnsureDisposition, Envelope, Epoch, GenerationId, Refusal};
use festerm_secret_store::{MemorySecretStore, SecretStoreError};
use festerm_tunnel_client::host_control::read_frame;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Mutex,
};
use tokio::io::{AsyncReadExt, DuplexStream};

struct Vault {
    inner: MemorySecretStore,
    fault: AtomicU8,
    cancel: Mutex<Option<CancellationToken>>,
}

impl SecretStore for Vault {
    fn create_reserved(
        &self,
        reference: &SecretReference,
        bytes: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        self.inner.create_reserved(reference, bytes)
    }
    fn put(&self, _: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        panic!("no unanchored native reference")
    }
    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        if self.fault.load(Ordering::SeqCst) == 3 {
            return Err(SecretStoreError::LockedOrUnavailable);
        }
        self.inner.get(reference)
    }
    fn update(
        &self,
        reference: &SecretReference,
        bytes: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        let fault = self.fault.swap(0, Ordering::SeqCst);
        if fault == 1 {
            return Err(SecretStoreError::BackendFailure);
        }
        self.inner.update(reference, bytes)?;
        if let Some(cancel) = self.cancel.lock().unwrap().take() {
            cancel.cancel();
        }
        if fault == 2 {
            return Err(SecretStoreError::BackendFailure);
        }
        Ok(())
    }
    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        self.inner.delete(reference)
    }
}

struct Fixture {
    root: festerm_sessiond::private_storage::PrivateTestDirectory,
    vault: Arc<Vault>,
    context: ApprovedContext,
    store: Arc<OriginalStore>,
}

impl Fixture {
    fn new() -> Self {
        let parent = std::env::current_dir().unwrap().join("target");
        std::fs::create_dir_all(&parent).unwrap();
        let root = festerm_sessiond::private_storage::private_test_directory_in(&parent);
        let vault = Arc::new(Vault {
            inner: MemorySecretStore::new(),
            fault: 0.into(),
            cancel: Mutex::new(None),
        });
        let mut context = context();
        let reviewed = approvals::ReviewedOwner::confirm(
            &serde_json::to_string(&context.approved).unwrap(),
            &context.approved.host.host,
            &context.approved.account,
            &context.approved.host.host_key_fingerprint,
        )
        .unwrap();
        let receipt = ReceiptIndex::in_app_sandbox(root.path())
            .unwrap()
            .prepare_with(vault.as_ref(), reviewed)
            .unwrap();
        context.reference = receipt.reference();
        let (store, recovered) =
            OriginalStore::recover_with(root.path(), &context, vault.clone()).unwrap();
        assert!(recovered.is_empty());
        Self {
            root,
            vault,
            context,
            store,
        }
    }

    fn reopen(&self) -> Result<(Arc<OriginalStore>, Vec<RecoveredOriginal>), Failure> {
        OriginalStore::recover_with(self.root.path(), &self.context, self.vault.clone())
    }

    fn reference(&self) -> SecretReference {
        ReceiptIndex::in_app_sandbox(self.root.path())
            .unwrap()
            .with_originals(
                self.vault.as_ref(),
                &self.context.approved,
                &self.context.reference,
                false,
                |reference, _, _| Ok(reference.duplicate_for_transport()),
            )
            .unwrap()
    }

    fn intent(&self) -> Arc<CreationIntent> {
        let intent =
            Arc::new(CreationIntent::new(&self.context, CreationMode::Ensure, creation()).unwrap());
        intent
            .protect_original(self.store.clone(), "fixture review".into())
            .unwrap();
        intent
    }

    fn blob(&self) -> Blob {
        self.vault
            .get(&self.reference())
            .unwrap()
            .with_bytes(|bytes| serde_json::from_slice(bytes).unwrap())
    }

    fn corrupt(&self, mutation: impl FnOnce(&mut Blob)) {
        let mut blob = self.blob();
        mutation(&mut blob);
        self.vault
            .inner
            .update(
                &self.reference(),
                &SecretBytes::copy_from_slice(&serde_json::to_vec(&blob).unwrap()),
            )
            .unwrap();
    }
}

async fn request(peer: &mut DuplexStream) -> Envelope<Request> {
    read_frame(peer, &Budgets::default())
        .await
        .unwrap()
        .decode()
        .unwrap()
}

async fn lost_creation(fixture: &Fixture, intent: Arc<CreationIntent>) -> Envelope<Request> {
    let (stream, mut peer) = tokio::io::duplex(8192);
    let store = fixture.store.clone();
    let server = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        let request = request(&mut peer).await;
        store
            .access(false, |blob| {
                assert!(
                    blob.actions.iter().any(|action| matches!(&action.body,
                Body::Creation { issued, .. } if issued.id == request.operation
                    && Some(issued.epoch) == request.issuance_epoch)),
                    "original durable before wire"
                );
                Ok(((), false))
            })
            .unwrap();
        request
    });
    assert!(matches!(
        create_authenticated(
            &mut client(stream, &fixture.context),
            intent,
            &CancellationToken::new()
        )
        .await,
        Err(Failure::Control(Error::Offline))
    ));
    server.await.unwrap()
}

#[tokio::test]
async fn protected_original_reopen_retains_exact_ticket_body_and_old_epoch_after_lost_reply() {
    let fixture = Fixture::new();
    let intent = fixture.intent();
    let first = lost_creation(&fixture, intent.clone()).await;
    let issued = intent.issued_operation().unwrap();
    drop(intent);
    let (store, mut recovered) = fixture.reopen().unwrap();
    let RecoveredIntent::Creation(restored) = recovered.remove(0).intent else {
        panic!("creation")
    };
    assert_eq!(restored.issued_operation().unwrap(), issued);
    assert!(!restored.is_presented() && !restored.is_resolved());
    assert!(restored
        .protect_original(store, "changed review".into())
        .is_err());
    let expected = first.clone();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        let mut capabilities = caps();
        capabilities.issuance_epoch = Some(Epoch::new(9).unwrap());
        handshake(&mut peer, capabilities).await;
        let retried = request(&mut peer).await;
        assert_eq!(
            retried, expected,
            "new handshake must not mint a new ticket"
        );
        respond(
            &mut peer,
            &retried,
            Response::Refused {
                reason: Refusal::OutcomeUncertain,
            },
        )
        .await;
    });
    assert!(matches!(
        create_authenticated(
            &mut client(stream, &fixture.context),
            restored,
            &CancellationToken::new()
        )
        .await,
        Err(Failure::Control(Error::Refused(Refusal::OutcomeUncertain)))
    ));
    server.await.unwrap();
    assert_eq!(fixture.reopen().unwrap().1.len(), 1);
}

#[tokio::test]
async fn protected_original_checkpoint_faults_and_lock_never_send_or_become_empty_after_uncertainty(
) {
    for fault in [1, 2, 3, 4] {
        let fixture = Fixture::new();
        let intent = fixture.intent();
        let cancel = CancellationToken::new();
        if fault == 4 {
            *fixture.vault.cancel.lock().unwrap() = Some(cancel.clone());
        } else {
            fixture.vault.fault.store(fault, Ordering::SeqCst);
        }
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(&mut peer, caps()).await;
            let mut effectful = Vec::new();
            peer.read_to_end(&mut effectful).await.unwrap();
            assert!(
                effectful.is_empty(),
                "vault failure/cancel sent effectful bytes"
            );
        });
        let result = create_authenticated(
            &mut client(stream, &fixture.context),
            intent.clone(),
            &cancel,
        )
        .await;
        match fault {
            1 | 2 => assert!(matches!(result, Err(Failure::OriginalStorage))),
            3 => assert!(matches!(
                result,
                Err(Failure::OwnerKey(crate::owner_auth::OwnerKeyError::Vault(
                    SecretStoreError::LockedOrUnavailable
                )))
            )),
            4 => assert!(matches!(result, Err(Failure::Control(Error::Cancelled)))),
            _ => unreachable!(),
        }
        let original = intent.issued_operation().unwrap();
        server.await.unwrap();
        fixture.vault.fault.store(0, Ordering::SeqCst);
        match fault {
            1 => assert!(
                fixture.reopen().is_err(),
                "pending uncommitted checkpoint is not absence"
            ),
            2 | 4 => {
                let (_, mut originals) = fixture.reopen().unwrap();
                let RecoveredIntent::Creation(restored) = originals.remove(0).intent else {
                    panic!("creation")
                };
                assert_eq!(restored.issued_operation().unwrap(), original);
            }
            3 => assert!(
                fixture.reopen().unwrap().1.is_empty(),
                "no checkpoint began while locked"
            ),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn protected_original_missing_corrupt_or_scope_changed_never_reinitializes() {
    for corruption in 0..10 {
        let fixture = Fixture::new();
        lost_creation(&fixture, fixture.intent()).await;
        let reference = fixture.reference();
        match corruption {
            0 => {
                fixture.vault.inner.delete(&reference).unwrap();
            }
            1 => {
                fixture
                    .vault
                    .inner
                    .update(&reference, &SecretBytes::copy_from_slice(b"{"))
                    .unwrap();
            }
            2 => fixture.corrupt(|blob| blob.domain.push('!')),
            3 => fixture.corrupt(|blob| blob.owner.account = "github:changed".into()),
            4 => fixture
                .corrupt(|blob| blob.owner.principal = festerm_host_protocol::PrincipalId::new()),
            5 => {
                fixture.corrupt(|blob| blob.key = SecretReference::generate().to_persisted_string())
            }
            6 => fixture.corrupt(|blob| blob.endpoint.push('!')),
            7 => fixture
                .corrupt(|blob| blob.reference = SecretReference::generate().to_persisted_string()),
            8 => fixture.corrupt(|blob| blob.actions.push(blob.actions[0].clone())),
            9 => fixture.corrupt(|blob| {
                let original = blob.actions[0].clone();
                for _ in 1..17 {
                    let mut extra = original.clone();
                    let Body::Creation { creation, .. } = &mut extra.body else {
                        unreachable!()
                    };
                    creation.attachment = AttachmentId::new();
                    extra.id = creation.attachment.to_string();
                    blob.actions.push(extra);
                }
            }),
            _ => unreachable!(),
        }
        assert!(
            fixture.reopen().is_err(),
            "corruption {corruption} became empty"
        );
        if corruption == 0 {
            assert!(matches!(
                fixture.vault.inner.get(&reference),
                Err(SecretStoreError::Missing)
            ));
        }
    }
}

#[tokio::test]
async fn protected_original_end_reopens_original_without_issuance_and_only_exact_ended_prunes() {
    let fixture = Fixture::new();
    let target = Target {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(4).unwrap(),
    };
    let original = Arc::new(EndIntent::new(&fixture.context, target.clone()));
    original
        .protect_original(fixture.store.clone(), "End review".into())
        .unwrap();
    let operation = original.operation().clone();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        handshake(
            &mut peer,
            Capabilities {
                explicit_end: true,
                ..Capabilities::default()
            },
        )
        .await;
        let first = request(&mut peer).await;
        assert!(first.issuance_epoch.is_none());
        first
    });
    assert!(end_authenticated(
        &mut client(stream, &fixture.context),
        original.clone(),
        &CancellationToken::new()
    )
    .await
    .is_err());
    let first = server.await.unwrap();
    drop(original);
    for reason in [Some(Refusal::Busy), Some(Refusal::OutcomeUncertain), None] {
        let (_, mut recovered) = fixture.reopen().unwrap();
        let RecoveredIntent::End(intent) = recovered.remove(0).intent else {
            panic!("End")
        };
        assert_eq!(intent.operation(), &operation);
        let expected = first.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(
                &mut peer,
                Capabilities {
                    explicit_end: true,
                    ..Capabilities::default()
                },
            )
            .await;
            let retry = request(&mut peer).await;
            assert_eq!(retry, expected);
            let Request::ExplicitEnd { target } = retry.body.clone() else {
                panic!("End")
            };
            respond(
                &mut peer,
                &retry,
                match reason {
                    Some(reason) => Response::Refused { reason },
                    None => Response::Ended { target },
                },
            )
            .await;
        });
        let result = end_authenticated(
            &mut client(stream, &fixture.context),
            intent.clone(),
            &CancellationToken::new(),
        )
        .await;
        server.await.unwrap();
        if reason.is_some() {
            assert!(result.is_err() && !intent.is_resolved());
            assert_eq!(fixture.reopen().unwrap().1.len(), 1);
        } else {
            assert!(matches!(result, Ok(Outcome::Ended { .. })));
            assert!(intent.is_resolved());
            assert!(fixture.reopen().unwrap().1.is_empty());
        }
    }
}

#[tokio::test]
async fn protected_original_sixteen_uncertain_actions_cannot_be_pruned_or_replaced() {
    let fixture = Fixture::new();
    for _ in 0..16 {
        lost_creation(&fixture, fixture.intent()).await;
    }
    let (_, originals) = fixture.reopen().unwrap();
    let issued: Vec<_> = originals
        .into_iter()
        .map(|original| match original.intent {
            RecoveredIntent::Creation(intent) => intent.issued_operation().unwrap(),
            _ => panic!("creation"),
        })
        .collect();
    let next = fixture.intent();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        let mut effectful = Vec::new();
        peer.read_to_end(&mut effectful).await.unwrap();
        assert!(effectful.is_empty());
    });
    assert!(matches!(
        create_authenticated(
            &mut client(stream, &fixture.context),
            next,
            &CancellationToken::new()
        )
        .await,
        Err(Failure::OriginalStorage)
    ));
    server.await.unwrap();
    let (_, originals) = fixture.reopen().unwrap();
    let restored: Vec<_> = originals
        .into_iter()
        .map(|original| match original.intent {
            RecoveredIntent::Creation(intent) => intent.issued_operation().unwrap(),
            _ => panic!("creation"),
        })
        .collect();
    assert_eq!(restored, issued);
}

#[tokio::test]
async fn protected_original_forget_preserves_other_records_and_never_settles_end() {
    let fixture = Fixture::new();
    let first = fixture.intent();
    let second = fixture.intent();
    lost_creation(&fixture, first.clone()).await;
    lost_creation(&fixture, second.clone()).await;
    let operation = OperationId::new();
    let target = Target {
        logical: first.creation().logical.clone(),
        generation: GenerationId::new(8).unwrap(),
    };
    fixture
        .store
        .handle(operation.to_string(), "End".into())
        .capture_end(&operation, &target)
        .unwrap();
    let saved = serde_json::to_vec(&fixture.blob().actions[1]).unwrap();
    let cancel = CancellationToken::new();
    let review = fixture
        .store
        .review_forget(
            &fixture.context,
            OriginalId::End(operation.clone()),
            &cancel,
        )
        .unwrap();
    assert_eq!(review.target(), Some(&target));
    assert_eq!(fixture.reopen().unwrap().1.len(), 3);
    let receipt = review.forget(&fixture.context, &cancel).unwrap();
    assert_eq!(receipt.outcome(), ForgetOutcome::Removed);
    assert!(receipt.matches_context(&fixture.context));
    assert_eq!(fixture.reopen().unwrap().1.len(), 2);
    assert!(!first.is_resolved() && !first.is_presented());
    let review = fixture
        .store
        .review_forget(
            &fixture.context,
            OriginalId::Creation(first.creation().attachment.clone()),
            &cancel,
        )
        .unwrap();
    assert_eq!(review.logical(), Some(&first.creation().logical));
    review.forget(&fixture.context, &cancel).unwrap();
    let remaining = fixture.blob().actions;
    assert_eq!(remaining.len(), 1);
    assert!(serde_json::to_vec(&remaining[0]).unwrap() == saved);
    let (_, mut reopened) = fixture.reopen().unwrap();
    let RecoveredIntent::Creation(restored) = reopened.remove(0).intent else {
        panic!("creation")
    };
    assert_eq!(restored.issued_operation(), second.issued_operation());
}

#[tokio::test]
async fn protected_original_forget_refuses_changed_scope_record_and_cancelled_consent() {
    let fixture = Fixture::new();
    let intent = fixture.intent();
    lost_creation(&fixture, intent.clone()).await;
    let id = OriginalId::Creation(intent.creation().attachment.clone());
    let cancel = CancellationToken::new();
    for changed in 0..4 {
        let mut wrong = context();
        wrong.approved = fixture.context.approved.clone();
        wrong.reference = fixture.context.reference.duplicate_for_transport();
        wrong.binding = fixture.context.binding.clone();
        match changed {
            0 => wrong.approved.principal = festerm_host_protocol::PrincipalId::new(),
            1 => wrong.approved.account = "github:8".into(),
            2 => wrong.reference = SecretReference::generate(),
            _ => {
                wrong.binding = Binding::new(
                    wrong.binding.host().clone(),
                    wrong.binding.account().into(),
                    "other.endpoint".into(),
                )
                .unwrap()
            }
        }
        assert!(matches!(
            fixture.store.review_forget(&wrong, id.clone(), &cancel),
            Err(Failure::BindingMismatch)
        ));
        let review = fixture
            .store
            .review_forget(&fixture.context, id.clone(), &cancel)
            .unwrap();
        assert!(matches!(
            review.forget(&wrong, &cancel),
            Err(Failure::BindingMismatch)
        ));
    }
    let review = fixture
        .store
        .review_forget(&fixture.context, id.clone(), &cancel)
        .unwrap();
    cancel.cancel();
    assert!(review.forget(&fixture.context, &cancel).is_err());
    assert_eq!(fixture.reopen().unwrap().1.len(), 1);
    let cancel = CancellationToken::new();
    let review = fixture
        .store
        .review_forget(&fixture.context, id, &cancel)
        .unwrap();
    fixture.corrupt(|blob| blob.actions[0].review.push_str(" changed"));
    assert!(matches!(
        review.forget(&fixture.context, &cancel),
        Err(Failure::BindingMismatch)
    ));
    assert_eq!(fixture.reopen().unwrap().1.len(), 1);
}

#[tokio::test]
async fn protected_original_forget_missing_is_local_only_and_vault_failures_never_succeed() {
    for fault in 0..6 {
        let fixture = Fixture::new();
        let intent = fixture.intent();
        lost_creation(&fixture, intent.clone()).await;
        let id = OriginalId::Creation(intent.creation().attachment.clone());
        let cancel = CancellationToken::new();
        let review = fixture
            .store
            .review_forget(&fixture.context, id.clone(), &cancel)
            .unwrap();
        match fault {
            0 => {
                drop(review);
                assert_eq!(fixture.reopen().unwrap().1.len(), 1);
                fixture
                    .store
                    .review_forget(&fixture.context, id.clone(), &cancel)
                    .unwrap()
                    .forget(&fixture.context, &cancel)
                    .unwrap();
                let receipt = fixture
                    .store
                    .review_forget(&fixture.context, id, &cancel)
                    .unwrap()
                    .forget(&fixture.context, &cancel)
                    .unwrap();
                assert_eq!(receipt.outcome(), ForgetOutcome::AlreadyMissing);
                assert!(!intent.is_resolved());
            }
            1 | 2 => {
                fixture.vault.fault.store(fault, Ordering::SeqCst);
                assert!(review.forget(&fixture.context, &cancel).is_err());
                assert_eq!(fixture.blob().actions.len(), usize::from(fault == 1));
                if fault == 1 {
                    assert!(fixture
                        .store
                        .review_forget(&fixture.context, id, &cancel)
                        .is_err());
                } else {
                    let receipt = fixture
                        .store
                        .review_forget(&fixture.context, id, &cancel)
                        .unwrap()
                        .forget(&fixture.context, &cancel)
                        .unwrap();
                    assert_eq!(receipt.outcome(), ForgetOutcome::AlreadyMissing);
                }
            }
            3 => {
                fixture.vault.fault.store(3, Ordering::SeqCst);
                assert!(review.forget(&fixture.context, &cancel).is_err());
                assert!(fixture
                    .store
                    .review_forget(&fixture.context, id, &cancel)
                    .is_err());
            }
            4 => {
                fixture.vault.inner.delete(&fixture.reference()).unwrap();
                assert!(review.forget(&fixture.context, &cancel).is_err());
                assert!(fixture
                    .store
                    .review_forget(&fixture.context, id, &cancel)
                    .is_err());
            }
            _ => {
                fixture.corrupt(|blob| blob.domain.push_str(" invalid"));
                assert!(review.forget(&fixture.context, &cancel).is_err());
                assert!(fixture
                    .store
                    .review_forget(&fixture.context, id, &cancel)
                    .is_err());
            }
        }
    }
}

#[tokio::test]
async fn protected_original_forget_frees_only_explicitly_selected_capacity() {
    let fixture = Fixture::new();
    for _ in 0..16 {
        lost_creation(&fixture, fixture.intent()).await;
    }
    let target = Target {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(9).unwrap(),
    };
    let mut ends = Vec::new();
    for _ in 0..16 {
        let operation = OperationId::new();
        fixture
            .store
            .handle(operation.to_string(), "End".into())
            .capture_end(&operation, &target)
            .unwrap();
        ends.push(operation);
    }
    let extra_end = OperationId::new();
    assert!(matches!(
        fixture
            .store
            .handle(extra_end.to_string(), "End".into())
            .capture_end(&extra_end, &target),
        Err(Failure::OriginalStorage)
    ));
    let before = fixture.blob().actions;
    let (_, originals) = fixture.reopen().unwrap();
    let RecoveredIntent::Creation(selected) = &originals[0].intent else {
        panic!("creation")
    };
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        let mut effectful = Vec::new();
        peer.read_to_end(&mut effectful).await.unwrap();
        assert!(effectful.is_empty());
    });
    assert!(matches!(
        create_authenticated(
            &mut client(stream, &fixture.context),
            fixture.intent(),
            &CancellationToken::new()
        )
        .await,
        Err(Failure::OriginalStorage)
    ));
    server.await.unwrap();
    let cancel = CancellationToken::new();
    fixture
        .store
        .review_forget(
            &fixture.context,
            OriginalId::Creation(selected.creation().attachment.clone()),
            &cancel,
        )
        .unwrap()
        .forget(&fixture.context, &cancel)
        .unwrap();
    assert!(
        serde_json::to_vec(&fixture.blob().actions).unwrap()
            == serde_json::to_vec(&before[1..]).unwrap()
    );
    lost_creation(&fixture, fixture.intent()).await;
    assert_eq!(fixture.reopen().unwrap().1.len(), 32);
    fixture
        .store
        .review_forget(&fixture.context, OriginalId::End(ends[0].clone()), &cancel)
        .unwrap()
        .forget(&fixture.context, &cancel)
        .unwrap();
    fixture
        .store
        .handle(extra_end.to_string(), "End".into())
        .capture_end(&extra_end, &target)
        .unwrap();
    assert_eq!(fixture.reopen().unwrap().1.len(), 32);
}

#[tokio::test]
async fn protected_original_created_generation_survives_presentation_until_exact_authoritative_end()
{
    let fixture = Fixture::new();
    let intent = fixture.intent();
    let target = Target {
        logical: intent.creation().logical.clone(),
        generation: GenerationId::new(8).unwrap(),
    };
    let expected = target.clone();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        let request = request(&mut peer).await;
        respond(
            &mut peer,
            &request,
            Response::Ensured {
                session: SessionSummary {
                    target: expected,
                    name: "Synthetic journal result".into(),
                    lifetime: festerm_host_protocol::Lifetime::Persistent {},
                    attached: false,
                },
                disposition: EnsureDisposition::Created,
                lease: None,
            },
        )
        .await;
    });
    assert!(matches!(
        create_authenticated(
            &mut client(stream, &fixture.context),
            intent.clone(),
            &CancellationToken::new()
        )
        .await,
        Ok(Outcome::Created(_))
    ));
    server.await.unwrap();
    intent.mark_presented();
    let (_, mut originals) = fixture.reopen().unwrap();
    let RecoveredIntent::Creation(restored) = originals.remove(0).intent else {
        panic!("creation")
    };
    assert_eq!(restored.confirmed_target().as_ref(), Some(&target));
    assert!(
        !restored.is_presented(),
        "a fresh frontend must adopt independently"
    );
    let end = Arc::new(EndIntent::new(&fixture.context, target.clone()));
    end.protect_original(fixture.store.clone(), "End review".into())
        .unwrap();
    for phase in 0..3 {
        let target = target.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(
                &mut peer,
                Capabilities {
                    explicit_end: true,
                    ..Capabilities::default()
                },
            )
            .await;
            let request = request(&mut peer).await;
            let response = match phase {
                0 => Response::Ended {
                    target: Target {
                        generation: GenerationId::new(9).unwrap(),
                        ..target
                    },
                },
                1 => Response::Refused {
                    reason: Refusal::OutcomeUncertain,
                },
                _ => Response::Ended { target },
            };
            respond(&mut peer, &request, response).await;
        });
        let result = end_authenticated(
            &mut client(stream, &fixture.context),
            end.clone(),
            &CancellationToken::new(),
        )
        .await;
        server.await.unwrap();
        if phase < 2 {
            assert!(result.is_err() && !end.definitively_settles(&intent));
            assert_eq!(fixture.reopen().unwrap().1.len(), 2);
        } else {
            assert!(matches!(result, Ok(Outcome::Ended { .. })));
            assert!(end.definitively_settles(&intent));
            assert!(fixture.reopen().unwrap().1.is_empty());
        }
    }
}

#[tokio::test]
async fn protected_original_forged_checkpoint_body_purpose_or_epoch_never_sends_new_authority() {
    for corruption in 0..5 {
        let fixture = Fixture::new();
        let intent = fixture.intent();
        lost_creation(&fixture, intent.clone()).await;
        let issued = intent.issued_operation().unwrap();
        fixture.corrupt(|blob| {
            let Body::Creation { ticket, .. } = &mut blob.actions[0].body else {
                panic!("creation")
            };
            let mut value: serde_json::Value =
                serde_json::from_slice(&STANDARD_NO_PAD.decode(&*ticket).unwrap()).unwrap();
            match corruption {
                0 => {
                    *ticket = "!".into();
                    return;
                }
                1 => value["domain"] = "wrong-purpose".into(),
                2 => value["operation"]["epoch"] = 99.into(),
                3 => value["account"] = "github:changed".into(),
                4 => value["unexpected"] = true.into(),
                _ => unreachable!(),
            }
            *ticket = STANDARD_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
        });
        let (_, mut originals) = fixture.reopen().unwrap();
        let RecoveredIntent::Creation(restored) = originals.remove(0).intent else {
            panic!("creation")
        };
        assert_eq!(restored.issued_operation().unwrap(), issued);
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(&mut peer, caps()).await;
            let mut effectful = Vec::new();
            peer.read_to_end(&mut effectful).await.unwrap();
            assert!(effectful.is_empty(), "invalid original was sent");
        });
        assert!(create_authenticated(
            &mut client(stream, &fixture.context),
            restored,
            &CancellationToken::new()
        )
        .await
        .is_err());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn protected_original_fresh_context_cannot_change_account_key_installation_or_principal() {
    let fixture = Fixture::new();
    lost_creation(&fixture, fixture.intent()).await;
    for field in 0..5 {
        let mut changed = ApprovedContext::from_protected_approval(
            fixture.context.approved.clone(),
            fixture.context.reference.duplicate_for_transport(),
            fixture.context.endpoint.clone(),
            fixture.context.port,
            Zeroizing::new("fresh-synthetic-public-client-token".into()),
            fixture.context.binding.clone(),
        )
        .unwrap();
        match field {
            0 => changed.approved.account = "github:other".into(),
            1 => changed.approved.principal = festerm_host_protocol::PrincipalId::new(),
            2 => changed.approved.installation = festerm_host_protocol::HostId::new().to_string(),
            3 => changed.reference = SecretReference::generate(),
            4 => changed.approved.host.host_key_fingerprint.push('!'),
            _ => unreachable!(),
        }
        assert!(
            OriginalStore::recover_with(fixture.root.path(), &changed, fixture.vault.clone())
                .is_err()
        );
        assert_eq!(fixture.reopen().unwrap().1.len(), 1);
    }
}

#[tokio::test]
async fn protected_original_fresh_process_reopens_native_fixture_and_replays_identical_wire_once() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let fixture = Fixture::new();
    let first = lost_creation(&fixture, fixture.intent()).await;
    let reference = fixture.reference();
    // Only synthetic MemorySecretStore values cross this test-only pipe. This
    // is not a native-service acceptance claim or production file/key exporter.
    let entries: Vec<_> = [&fixture.context.reference, &reference]
        .into_iter()
        .map(|reference| {
            let value = fixture.vault.get(reference).unwrap();
            serde_json::json!({"reference": reference.to_persisted_string(),
            "value": value.with_bytes(|bytes| STANDARD_NO_PAD.encode(bytes))})
        })
        .collect();
    let payload = serde_json::to_vec(&serde_json::json!({
        "root": fixture.root.path(),
        "owner": fixture.context.approved,
        "key": fixture.context.reference.to_persisted_string(),
        "hostKeys": fixture.context.endpoint.host_public_keys,
        "entries": entries,
        "expected": first,
    }))
    .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "host_client::originals::tests::checkpoint_fixture_child",
            "--nocapture",
        ])
        .env("FESTERM_CHECKPOINT_FIXTURE_CHILD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&payload).unwrap();
    let child_id = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "fresh-process checkpoint fixture failed"
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("EXACT_ORIGINAL_REOPENED_ONCE"));
    println!("checkpoint fixture child pid={child_id} exit=0; exactly one identical original wire request");
    assert_eq!(
        fixture.reopen().unwrap().1.len(),
        1,
        "child uncertainty retained the durable original"
    );
}

#[tokio::test]
async fn protected_original_reopen_vault_errors_remain_typed_not_host_denial() {
    for locked in [false, true] {
        let fixture = Fixture::new();
        lost_creation(&fixture, fixture.intent()).await;
        let (_, mut originals) = fixture.reopen().unwrap();
        let RecoveredIntent::Creation(restored) = originals.remove(0).intent else {
            panic!("creation")
        };
        let issued = restored.issued_operation().unwrap();
        if locked {
            fixture.vault.fault.store(3, Ordering::SeqCst);
        } else {
            fixture.vault.inner.delete(&fixture.reference()).unwrap();
        }
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(&mut peer, caps()).await;
            let mut effectful = Vec::new();
            peer.read_to_end(&mut effectful).await.unwrap();
            assert!(effectful.is_empty());
        });
        let result = create_authenticated(
            &mut client(stream, &fixture.context),
            restored.clone(),
            &CancellationToken::new(),
        )
        .await;
        if locked {
            assert!(matches!(
                result,
                Err(Failure::OwnerKey(crate::owner_auth::OwnerKeyError::Vault(
                    SecretStoreError::LockedOrUnavailable
                )))
            ));
        } else {
            assert!(matches!(result, Err(Failure::OriginalStorage)));
        }
        assert_eq!(restored.issued_operation().unwrap(), issued);
        assert!(!restored.is_resolved());
        server.await.unwrap();
    }
}

#[test]
fn checkpoint_fixture_child() {
    use std::io::Read;
    if std::env::var("FESTERM_CHECKPOINT_FIXTURE_CHILD").as_deref() != Ok("1") {
        return;
    }
    let mut payload = Zeroizing::new(Vec::new());
    std::io::stdin()
        .take(200_000)
        .read_to_end(&mut payload)
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    let vault = Arc::new(MemorySecretStore::new());
    for entry in value["entries"].as_array().unwrap() {
        let reference = SecretReference::parse(entry["reference"].as_str().unwrap()).unwrap();
        let bytes = Zeroizing::new(
            STANDARD_NO_PAD
                .decode(entry["value"].as_str().unwrap())
                .unwrap(),
        );
        vault
            .create_reserved(&reference, &SecretBytes::copy_from_slice(&bytes))
            .unwrap();
    }
    let owner: OwnerBinding = serde_json::from_value(value["owner"].clone()).unwrap();
    let binding = Binding::new(
        owner.host.host.clone(),
        owner.account.clone(),
        "fixture".into(),
    )
    .unwrap();
    let context = ApprovedContext::from_protected_approval(
        owner,
        SecretReference::parse(value["key"].as_str().unwrap()).unwrap(),
        serde_json::from_value(serde_json::json!({
            "connectionMode": "TunnelRelay", "hostId": "fixture",
            "hostPublicKeys": value["hostKeys"], "clientRelayUri": "wss://example.invalid/client",
        }))
        .unwrap(),
        22,
        Zeroizing::new("fresh-synthetic-public-client-token".into()),
        binding,
    )
    .unwrap();
    let root = Path::new(value["root"].as_str().unwrap());
    let (_, mut originals) = OriginalStore::recover_with(root, &context, vault).unwrap();
    assert_eq!(originals.len(), 1);
    let RecoveredIntent::Creation(intent) = originals.remove(0).intent else {
        panic!("creation")
    };
    let expected: Envelope<Request> = serde_json::from_value(value["expected"].clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            let mut capabilities = caps();
            capabilities.issuance_epoch = Some(Epoch::new(77).unwrap());
            handshake(&mut peer, capabilities).await;
            let replay = request(&mut peer).await;
            assert_eq!(
                replay, expected,
                "new process substituted original ID, epoch or body"
            );
            respond(
                &mut peer,
                &replay,
                Response::Refused {
                    reason: Refusal::OutcomeUncertain,
                },
            )
            .await;
            let mut extra = Vec::new();
            peer.read_to_end(&mut extra).await.unwrap();
            assert!(extra.is_empty(), "more than one effectful request");
        });
        assert!(matches!(
            create_authenticated(
                &mut client(stream, &context),
                intent,
                &CancellationToken::new()
            )
            .await,
            Err(Failure::Control(Error::Refused(Refusal::OutcomeUncertain)))
        ));
        server.await.unwrap();
    });
    println!("EXACT_ORIGINAL_REOPENED_ONCE");
}
