//! Fixed validation composition, not a selectable application storage backend.
use super::*;
use festerm_host_protocol::{Envelope, GenerationId, HostId, PrincipalId, Refusal};
use festerm_tunnel_client::host_control::{read_frame, write_frame};
use russh::keys::{Algorithm, HashAlg, PrivateKey};

const REVIEW: &str = "Synthetic native-index validation only; no remote authority";

fn require(condition: bool) -> Result<(), Failure> {
    if condition {
        Ok(())
    } else {
        Err(Failure::OriginalStorage)
    }
}

fn prerequisite() -> Result<(approvals::ReviewedOwner, TunnelEndpoint), Failure> {
    let key = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519)
        .map_err(|_| Failure::OriginalStorage)?;
    let public = key
        .public_key()
        .to_openssh()
        .map_err(|_| Failure::OriginalStorage)?;
    let owner = OwnerBinding {
        installation: HostId::new().to_string(),
        host: crate::owner_auth::PinnedHost {
            host: HostId::new(),
            host_key_fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string(),
        },
        account: "github:1234".into(),
        principal: PrincipalId::new(),
    };
    let reviewed = approvals::ReviewedOwner::confirm(
        &serde_json::to_string(&owner).map_err(|_| Failure::OriginalStorage)?,
        &owner.host.host,
        &owner.account,
        &owner.host.host_key_fingerprint,
    )
    .map_err(failed)?;
    let endpoint = serde_json::from_value(serde_json::json!({
        "connectionMode": "TunnelRelay", "hostId": "synthetic-index-fixture",
        "hostPublicKeys": [public.split_whitespace().nth(1).ok_or(Failure::OriginalStorage)?],
        "clientRelayUri": "wss://example.invalid/never-connect"
    }))
    .map_err(|_| Failure::OriginalStorage)?;
    Ok((reviewed, endpoint))
}

fn context(
    receipt: &approvals::Receipt,
    endpoint: TunnelEndpoint,
) -> Result<ApprovedContext, Failure> {
    let owner = receipt.binding().clone();
    let binding = Binding::new(
        owner.host.host.clone(),
        owner.account.clone(),
        "synthetic-index-fixture".into(),
    )?;
    ApprovedContext::from_protected_approval(
        owner,
        receipt.reference(),
        endpoint,
        22,
        Zeroizing::new("not-a-service-token".into()),
        binding,
    )
}

#[cfg(all(target_os = "android", debug_assertions))]
pub(super) fn run_native() -> Result<(), Failure> {
    use festerm_secret_store::NativeAppPrivateRoot;
    let root = Arc::new(NativeAppPrivateRoot::current().map_err(|_| Failure::OriginalStorage)?);
    let index = ReceiptIndex::in_native_app_sandbox(root.clone());
    require(index.validation_has_native_root())?;
    let (reviewed, endpoint) = prerequisite()?;
    let receipt = index.prepare(reviewed).map_err(failed)?;
    let context = context(&receipt, endpoint)?;
    let (store, originals) = OriginalStore::recover_in_native_app_sandbox(root.clone(), &context)?;
    require(originals.is_empty() && store.index.validation_has_native_root())?;
    let reopen = |context: &ApprovedContext| {
        let fresh =
            Arc::new(NativeAppPrivateRoot::current().map_err(|_| Failure::OriginalStorage)?);
        require(!Arc::ptr_eq(&root, &fresh) && root.same_root(&fresh))?;
        let (store, originals) = OriginalStore::recover_in_native_app_sandbox(fresh, context)?;
        require(store.index.validation_has_native_root())?;
        Ok((store, originals))
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|_| Failure::WorkerUnavailable)?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(20), exercise(store, context, reopen))
            .await
            .map_err(|_| Failure::OriginalStorage)?
    })
}

async fn capture_uncertain(
    store: &Arc<OriginalStore>,
    context: &ApprovedContext,
    generation: u64,
) -> Result<Arc<EndIntent>, Failure> {
    let target = Target {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(generation)?,
    };
    let intent = Arc::new(EndIntent::new(context, target.clone()));
    intent.protect_original(store.clone(), REVIEW.into())?;
    let operation = intent.operation().clone();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let budgets = Budgets::default();
    let permit = ConnectionPermit::reserve(&budgets)?;
    let server = async {
        let request: Envelope<Request> = read_frame(&mut peer, &budgets).await?.decode()?;
        require(matches!(request.body, Request::Capabilities {}))?;
        let response = Envelope::new(
            request.host,
            request.operation,
            Response::Capabilities {
                capabilities: Capabilities {
                    explicit_end: true,
                    ..Capabilities::default()
                },
            },
        );
        write_frame(&mut peer, &budgets, &response).await?;
        let request: Envelope<Request> = read_frame(&mut peer, &budgets).await?.decode()?;
        require(
            request.host == *context.binding.host()
                && request.operation == operation
                && request.issuance_epoch.is_none()
                && request.body
                    == Request::ExplicitEnd {
                        target: target.clone(),
                    },
        )?;
        store.access(false, |blob| {
            if !blob.actions.iter().any(|original| {
                matches!(&original.body,
                Body::End { operation: op, target: saved } if op == &operation && saved == &target)
            }) {
                return Err(StorageError::Storage);
            }
            Ok(((), false))
        })?;
        let response = Envelope::new(
            request.host,
            request.operation,
            Response::Refused {
                reason: Refusal::OutcomeUncertain,
            },
        );
        write_frame(&mut peer, &budgets, &response).await?;
        Ok::<(), Failure>(())
    };
    let cancel = CancellationToken::new();
    let client = super::super::testing::end_from_stream(
        stream,
        permit,
        context.binding.clone(),
        intent.clone(),
        &cancel,
    );
    let (result, server) = tokio::join!(client, server);
    server?;
    require(matches!(
        result,
        Err(Failure::Control(Error::Refused(Refusal::OutcomeUncertain)))
    ))?;
    require(intent.is_issued() && !intent.is_resolved())?;
    Ok(intent)
}

async fn exercise(
    store: Arc<OriginalStore>,
    context: ApprovedContext,
    reopen: impl Fn(&ApprovedContext) -> Result<(Arc<OriginalStore>, Vec<RecoveredOriginal>), Failure>,
) -> Result<(), Failure> {
    let first = capture_uncertain(&store, &context, 7).await?;
    let second = capture_uncertain(&store, &context, 8).await?;
    let saved = store.access(false, |blob| {
        Ok((
            Zeroizing::new(
                serde_json::to_vec(&blob.actions[1]).map_err(|_| StorageError::Storage)?,
            ),
            false,
        ))
    })?;
    let before_review = store.access(false, |blob| {
        Ok((
            Zeroizing::new(serde_json::to_vec(blob).map_err(|_| StorageError::Storage)?),
            false,
        ))
    })?;
    let id = OriginalId::End(first.operation().clone());
    let cancel = CancellationToken::new();
    let stale = store.review_forget(&context, id.clone(), &cancel)?;
    store.access(false, |blob| {
        let unchanged =
            Zeroizing::new(serde_json::to_vec(blob).map_err(|_| StorageError::Storage)?);
        if *unchanged != *before_review {
            return Err(StorageError::Storage);
        }
        Ok(((), false))
    })?;
    let (reopened, originals) = reopen(&context)?;
    require(originals.len() == 2 && store.index.same_storage(&reopened.index))?;
    let handle = store.handle(first.operation().to_string(), REVIEW.into());
    require(handle.agrees(&reopened, REVIEW))?;
    drop(handle);
    reopened.access(false, |blob| {
        blob.actions[0]
            .review
            .push_str(" changed after native index reopen");
        Ok(((), true))
    })?;
    let changed = reopened.access(false, |blob| {
        Ok((
            Zeroizing::new(serde_json::to_vec(blob).map_err(|_| StorageError::Storage)?),
            false,
        ))
    })?;
    require(matches!(
        stale.forget(&context, &cancel),
        Err(Failure::BindingMismatch)
    ))?;
    reopened.access(false, |blob| {
        let after = Zeroizing::new(serde_json::to_vec(blob).map_err(|_| StorageError::Storage)?);
        if *after != *changed || blob.actions.len() != 2 {
            return Err(StorageError::Storage);
        }
        Ok(((), false))
    })?;
    let current = reopened.review_forget(&context, id.clone(), &cancel)?;
    let unrelated = second.operation().clone();
    let unrelated_target = second.target().clone();
    drop(originals);
    drop(first);
    drop(second);
    drop(reopened);
    drop(store);
    // Only the explicit review retains the OriginalStore at confirmation.
    let forgotten = current.forget(&context, &cancel)?;
    require(forgotten.id() == &id && forgotten.outcome() == ForgetOutcome::Removed)?;
    let (final_store, remaining) = reopen(&context)?;
    require(remaining.len() == 1)?;
    let RecoveredIntent::End(restored) = &remaining[0].intent else {
        return Err(Failure::OriginalStorage);
    };
    require(
        restored.operation() == &unrelated
            && restored.target() == &unrelated_target
            && restored.is_issued()
            && !restored.is_resolved(),
    )?;
    final_store.access(false, |blob| {
        if blob.actions.len() != 1 {
            return Err(StorageError::Storage);
        }
        let after = Zeroizing::new(
            serde_json::to_vec(&blob.actions[0]).map_err(|_| StorageError::Storage)?,
        );
        if *after != *saved {
            return Err(StorageError::Storage);
        }
        Ok(((), false))
    })
}

#[cfg(test)]
#[tokio::test]
async fn protected_original_forget_native_index_oracles_with_explicit_memory_storage() {
    let parent = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&parent).unwrap();
    let root = festerm_sessiond::private_storage::private_test_directory_in(&parent);
    let index = ReceiptIndex::in_app_sandbox(root.path()).unwrap();
    let vault = Arc::new(festerm_secret_store::MemorySecretStore::new());
    let (reviewed, endpoint) = prerequisite().unwrap();
    let receipt = index.prepare_with(vault.as_ref(), reviewed).unwrap();
    let context = context(&receipt, endpoint).unwrap();
    let (store, originals) =
        OriginalStore::recover_index(index.clone(), &context, vault.clone()).unwrap();
    assert!(originals.is_empty());
    let reopen = |context: &ApprovedContext| {
        OriginalStore::recover_index(index.clone(), context, vault.clone())
    };
    tokio::time::timeout(Duration::from_secs(20), exercise(store, context, reopen))
        .await
        .unwrap()
        .unwrap();
}
