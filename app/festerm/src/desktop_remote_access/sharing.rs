use super::*;
use festerm_host_setup::private_session_sharing::{Consent, Error, Sharing};
use festerm_sessiond::native_binding::{BindingPhase, Receipt};

fn classify(error: Error) -> Failure {
    match error {
        Error::Unsupported => Failure::ShareCapability,
        Error::Denied | Error::Revoked => Failure::OwnerApproval,
        Error::Busy => Failure::Busy,
        Error::Stale | Error::Conflict => Failure::ScopeChanged,
        Error::Invalid => Failure::InvalidDraft,
        Error::Unknown | Error::OutcomeUncertain => Failure::Reconciliation,
        Error::Cancelled => Failure::Cancelled,
    }
}

fn phase(receipt: &Receipt) -> SharePhase {
    match receipt.phase {
        BindingPhase::Prepared => SharePhase::Prepared,
        BindingPhase::Active => SharePhase::Active,
        BindingPhase::Revoked => SharePhase::Revoked,
        BindingPhase::Released => SharePhase::Released,
    }
}

fn exact(receipt: &Receipt, task: &ShareTask) -> Result<(), Failure> {
    if receipt.binding.operation.as_str() != task.operation
        || receipt.binding.identity != task.target.identity
        || task.target.source.as_ref().is_some_and(|source| {
            source.identity() != &receipt.binding.identity
                || source.runtime_root() != receipt.binding.runtime_root.as_path()
        })
    {
        return Err(Failure::SessionChanged);
    }
    Ok(())
}

fn consent(
    store: &Store,
    request: &Request,
    task: &ShareTask,
    cancel: &CancellationToken,
) -> Result<Consent, Failure> {
    if !task.is_current(cancel) {
        return Err(Failure::Cancelled);
    }
    verify(Some(store), &request.expected)?;
    let Scope::Present(expected) = &request.expected else {
        return Err(Failure::ScopeChanged);
    };
    let account = request
        .draft
        .account
        .parse::<u64>()
        .ok()
        .filter(|account| *account != 0)
        .ok_or(Failure::SignIn)?;
    if expected.phase != Phase::Provisioned
        || expected
            .binding
            .as_ref()
            .is_none_or(|binding| binding.account != format!("github:{account}"))
    {
        return Err(Failure::ScopeChanged);
    }
    let owner = store
        .owner_enrollment()
        .and_then(|owner| owner.snapshot())
        .map_err(failure)?;
    if owner.storage != OwnerStorage::NativeVault
        || owner.origin != Origin::Desktop
        || request.owner_revision != Some(owner.revision.get())
    {
        return Err(Failure::OwnerApproval);
    }
    let source = task
        .target
        .source
        .as_ref()
        .ok_or(Failure::ShareCapability)?;
    if source.identity() != &task.target.identity {
        return Err(Failure::SessionChanged);
    }
    Ok(Consent {
        expected: expected.as_ref().clone(),
        owner_revision: owner.revision,
        operation: task.operation.parse().map_err(|_| Failure::InvalidDraft)?,
    })
}

pub(super) fn execute(
    request: Request,
    step: ShareStep,
    cancel: &CancellationToken,
) -> Result<Report, Failure> {
    let task = request.sharing.as_ref().ok_or(Failure::InvalidDraft)?;
    let operation = task.operation.parse().map_err(|_| Failure::InvalidDraft)?;
    let store = observed(&request.root)?.ok_or(Failure::RefreshFirst)?;
    let consent = if step.grants() {
        Some(consent(&store, &request, task, cancel)?)
    } else {
        None
    };
    let sharing = Sharing::current(&store, &request.root).map_err(classify)?;
    let mut status = ShareStatus {
        target: task.target.clone(),
        operation: task.operation.clone(),
        phase: SharePhase::Uncertain,
    };
    let result = (|| {
        let current = sharing.lookup(&operation).map_err(classify)?;
        if let Some(receipt) = &current {
            exact(receipt, task)?;
        }
        if step == ShareStep::Resume {
            return current.ok_or(Failure::Reconciliation);
        }
        if !step.grants() {
            let receipt = current.ok_or(Failure::Reconciliation)?;
            return match step {
                ShareStep::Release => sharing.release(&receipt),
                ShareStep::Rollback => sharing.rollback(&receipt),
                _ => unreachable!(),
            }
            .map_err(classify);
        }
        if !task.is_current(cancel) {
            return Err(Failure::Cancelled);
        }
        let source = task
            .target
            .source
            .as_ref()
            .ok_or(Failure::ShareCapability)?;
        let consent = consent.as_ref().ok_or(Failure::OwnerApproval)?;
        match step {
            ShareStep::Bind => sharing.bind(consent, source, || task.is_current(cancel)),
            ShareStep::Switch => {
                let receipt = current.ok_or(Failure::Reconciliation)?;
                sharing.switch(&receipt, || task.is_current(cancel))
            }
            ShareStep::Resume | ShareStep::Release | ShareStep::Rollback => unreachable!(),
        }
        .map_err(classify)
    })();

    let mut result = result;
    if step.grants() && !task.is_current(cancel) {
        // A cancelled native wait can already have committed. Reduce only this
        // original protected operation; never disable the host or touch its PTY.
        result = match sharing.lookup(&operation).map_err(classify) {
            Ok(Some(receipt)) => {
                exact(&receipt, task)?;
                sharing.release(&receipt).map_err(classify)
            }
            Ok(None) => Err(Failure::Cancelled),
            Err(_) => Err(Failure::Reconciliation),
        };
    }
    let outcome = match result {
        Ok(receipt) => {
            exact(&receipt, task)?;
            status.phase = phase(&receipt);
            if step.grants() && !task.is_current(cancel) {
                Some(Failure::Cancelled)
            } else {
                None
            }
        }
        Err(error) => Some(error),
    };
    let mut observed = report(Some(&store))?;
    if let Ok(owner) = store.owner_enrollment().and_then(|owner| owner.snapshot()) {
        if matches!(&observed.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Provisioned)
        {
            public_owner(&mut observed, owner)?;
        }
    }
    observed.sharing = Some(status);
    observed.failure = outcome;
    Ok(observed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_remote_access_sharing_native_errors_are_fieldless_and_fail_closed() {
        for (error, expected) in [
            (Error::Unsupported, Failure::ShareCapability),
            (Error::Denied, Failure::OwnerApproval),
            (Error::Busy, Failure::Busy),
            (Error::Stale, Failure::ScopeChanged),
            (Error::Invalid, Failure::InvalidDraft),
            (Error::Conflict, Failure::ScopeChanged),
            (Error::Revoked, Failure::OwnerApproval),
            (Error::Unknown, Failure::Reconciliation),
            (Error::OutcomeUncertain, Failure::Reconciliation),
            (Error::Cancelled, Failure::Cancelled),
        ] {
            assert_eq!(classify(error), expected);
            assert!(!classify(error).message().contains("https://"));
        }
    }
}
