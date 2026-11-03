use crate::*;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

fn digest(value: u8) -> Digest {
    Digest([value; 32])
}

fn binding(value: u8) -> InvocationBinding {
    InvocationBinding {
        api_version: 1,
        invocation: Invocation::Workflow {
            bundle: digest(value),
            manifest: digest(42),
        },
        parameters: digest(99),
    }
}

fn direct(kind: DirectKind) -> InvocationBinding {
    InvocationBinding {
        api_version: 1,
        invocation: Invocation::Direct { operation: kind },
        parameters: digest(99),
    }
}

struct Fixture {
    broker: Broker,
    connection: ConnectionId,
    a: TargetRef,
    b: TargetRef,
    now: Instant,
}

impl Fixture {
    fn new() -> Self {
        let mut broker = Broker::new([7; 16]);
        let connection = broker.accept_connection().unwrap();
        let a = broker.attach_target(1, 1).unwrap();
        let b = broker.attach_target(2, 1).unwrap();
        Self {
            broker,
            connection,
            a,
            b,
            now: Instant::now(),
        }
    }

    fn scope(&self) -> Scope {
        Scope {
            discover: BTreeSet::from([self.a]),
            observe: BTreeSet::from([self.a]),
            control: BTreeSet::new(),
            invoke_workflows: true,
            return_results: true,
        }
    }

    fn approve(&mut self, scope: Scope, bindings: &[InvocationBinding]) {
        let request = GrantRequest {
            scope,
            bindings: bindings.iter().copied().collect(),
        };
        let id = self
            .broker
            .request_approval(self.connection, request.clone(), self.now)
            .unwrap();
        let mut native = NativeApproval::new(request.clone(), MAX_GRANT_LIFETIME);
        for target in &request.scope.control {
            native = native.accept_control_handoff(*target, request.scope.sources());
        }
        self.broker
            .approve(self.connection, id, native, self.now)
            .unwrap();
    }

    fn spec(&self, scope: Scope, binding: InvocationBinding) -> RunSpec {
        RunSpec {
            binding,
            scope,
            budget: Budget::default(),
            deadline: self.now + Duration::from_secs(60),
        }
    }

    fn run(&mut self, scope: Scope, binding: InvocationBinding) -> RunId {
        self.broker
            .admit_run(self.connection, None, self.spec(scope, binding), self.now)
            .unwrap()
    }

    fn controlled(&mut self) -> (RunId, LeaseRef) {
        let mut scope = self.scope();
        scope.control.insert(self.b);
        self.approve(scope.clone(), &[binding(1), binding(2)]);
        let run = self.run(scope, binding(1));
        let transition = self
            .broker
            .prepare_lease(self.connection, run, self.b, self.now)
            .unwrap();
        let lease = self.broker.commit_lease(transition, self.now).unwrap();
        (run, lease)
    }

    fn write(&mut self, run: RunId, lease: LeaseRef) -> RequestId {
        self.broker
            .prepare_request(
                self.connection,
                run,
                Operation::Control {
                    lease,
                    input: digest(3),
                    bytes: 10,
                },
                self.now,
            )
            .unwrap()
    }
}

#[test]
fn connection_and_wire_ids_never_grant_authority() {
    let mut f = Fixture::new();
    assert_eq!(
        f.broker
            .admit_run(f.connection, None, f.spec(f.scope(), binding(1)), f.now),
        Err(Error::Revoked)
    );
    f.approve(f.scope(), &[binding(1)]);
    let run = f.run(f.scope(), binding(1));
    let other = f.broker.accept_connection().unwrap();
    assert_eq!(f.broker.status(other, run, f.now), Err(Error::Unauthorized));
    assert_eq!(
        f.broker.prepare_request(
            other,
            run,
            Operation::Observe {
                target: f.a,
                max_bytes: 10,
            },
            f.now
        ),
        Err(Error::Unauthorized)
    );
    let encoded = serde_json::to_string(&run).unwrap();
    let copied: RunId = serde_json::from_str(&encoded).unwrap();
    assert_eq!(copied, run);
    assert_eq!(
        f.broker.status(other, copied, f.now),
        Err(Error::Unauthorized)
    );
    let mut malicious = serde_json::to_value(GrantRequest {
        scope: f.scope(),
        bindings: BTreeSet::from([binding(1)]),
    })
    .unwrap();
    malicious["approved"] = true.into();
    assert!(serde_json::from_value::<GrantRequest>(malicious).is_err());
}

#[test]
fn metadata_filtering_is_separate_from_observation_and_control() {
    let mut f = Fixture::new();
    let mut scope = f.scope();
    scope.observe.clear();
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    let run = f.run(scope, direct(DirectKind::Discovery));
    let request = f
        .broker
        .prepare_request(
            f.connection,
            run,
            Operation::Discover { max_bytes: 1024 },
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker.discovery_targets(f.connection, request, f.now),
        Ok(vec![f.a])
    );
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            run,
            Operation::Observe {
                target: f.a,
                max_bytes: 10,
            },
            f.now
        ),
        Err(Error::Unauthorized)
    );
    assert!(matches!(
        f.broker.prepare_lease(f.connection, run, f.a, f.now),
        Err(Error::Unauthorized)
    ));
}

#[test]
fn observation_scope_does_not_implicitly_discover_a_target() {
    let mut f = Fixture::new();
    let mut scope = f.scope();
    scope.discover.clear();
    f.approve(scope.clone(), &[binding(1)]);
    let run = f.run(scope, binding(1));
    let request = f
        .broker
        .prepare_request(
            f.connection,
            run,
            Operation::Discover { max_bytes: 32 },
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker.discovery_targets(f.connection, request, f.now),
        Ok(vec![])
    );
}

#[test]
fn binding_changes_and_child_scope_expansion_refuse() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[binding(1), binding(2)]);
    let mut altered = f.spec(f.scope(), binding(1));
    altered.binding.parameters = digest(100);
    assert_eq!(
        f.broker.admit_run(f.connection, None, altered, f.now),
        Err(Error::Unauthorized)
    );
    let mut altered = f.spec(f.scope(), binding(1));
    altered.binding.invocation = Invocation::Workflow {
        bundle: digest(1),
        manifest: digest(43),
    };
    assert_eq!(
        f.broker.admit_run(f.connection, None, altered, f.now),
        Err(Error::Unauthorized)
    );
    let mut parent_scope = f.scope();
    parent_scope.observe.clear();
    let parent = f.run(parent_scope, binding(1));
    assert_eq!(
        f.broker.admit_run(
            f.connection,
            Some(parent),
            f.spec(f.scope(), binding(2)),
            f.now
        ),
        Err(Error::Unauthorized)
    );
}

#[test]
fn control_requires_native_remote_handoff_and_all_source_disclosure() {
    let mut f = Fixture::new();
    let mut scope = f.scope();
    scope.control.insert(f.b);
    let proposal = GrantRequest {
        scope,
        bindings: BTreeSet::from([binding(1)]),
    };
    let pending = f
        .broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    assert_eq!(
        f.broker.approve(
            f.connection,
            pending,
            NativeApproval::new(proposal.clone(), MAX_GRANT_LIFETIME),
            f.now
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.approve(
            f.connection,
            pending,
            NativeApproval::new(proposal.clone(), MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::new()),
            f.now
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .approve(
            f.connection,
            pending,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::from([f.a])),
            f.now,
        )
        .unwrap();
}

#[test]
fn approvals_are_channel_bound_bounded_and_not_silently_renewed() {
    let mut f = Fixture::new();
    let proposal = GrantRequest {
        scope: f.scope(),
        bindings: BTreeSet::from([binding(1)]),
    };
    let pending = f
        .broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    let other = f.broker.accept_connection().unwrap();
    assert_eq!(
        f.broker.approve(
            other,
            pending,
            NativeApproval::new(proposal.clone(), MAX_GRANT_LIFETIME),
            f.now
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    assert_eq!(
        f.broker
            .request_approval(f.connection, proposal.clone(), f.now),
        Err(Error::QuotaExceeded)
    );
    assert_eq!(
        f.broker.approve(
            f.connection,
            pending,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME + Duration::from_secs(1)),
            f.now
        ),
        Err(Error::InvalidRequest)
    );
    for _ in 2..16 {
        f.broker.accept_connection().unwrap();
    }
    assert_eq!(f.broker.accept_connection(), Err(Error::QuotaExceeded));
}

#[test]
fn replacement_and_revocation_win_over_pending_disclosure() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[binding(1)]);
    let run = f.run(f.scope(), binding(1));
    let request = f
        .broker
        .prepare_request(
            f.connection,
            run,
            Operation::Observe {
                target: f.a,
                max_bytes: 1024,
            },
            f.now,
        )
        .unwrap();
    let replacement = f.broker.attach_target(f.a.session(), 2).unwrap();
    assert_ne!(replacement, f.a);
    assert!(f
        .broker
        .commit_disclosure(f.connection, request, 10, f.now)
        .is_err());
    assert_eq!(
        f.broker.attach_target(f.a.session(), 1),
        Err(Error::StaleTarget)
    );
    assert_eq!(
        f.broker
            .admit_run(f.connection, None, f.spec(f.scope(), binding(1)), f.now),
        Err(Error::StaleTarget)
    );
}

#[test]
fn expiry_at_the_exact_deadline_prevents_late_delivery_and_completion() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[binding(1)]);
    let run = f.run(f.scope(), binding(1));
    let request = f
        .broker
        .prepare_request(
            f.connection,
            run,
            Operation::Result { max_bytes: 100 },
            f.now,
        )
        .unwrap();
    let deadline = f.now + Duration::from_secs(60);
    assert_eq!(
        f.broker
            .commit_disclosure(f.connection, request, 1, deadline),
        Err(Error::Expired)
    );
    assert_eq!(
        f.broker
            .complete(f.connection, run, Completion::Returned, deadline),
        Err(Error::Expired)
    );
    assert_eq!(
        f.broker.status(f.connection, run, deadline).unwrap().state,
        RunState::TimedOut
    );
}

#[test]
fn shared_budget_does_not_reset_with_many_short_requests_or_runs() {
    let mut f = Fixture::new();
    let limits = Limits {
        global_operations: 6,
        ..Limits::default()
    };
    f.broker = Broker::with_limits([7; 16], limits);
    f.connection = f.broker.accept_connection().unwrap();
    f.a = f.broker.attach_target(1, 1).unwrap();
    f.b = f.broker.attach_target(2, 1).unwrap();
    f.approve(f.scope(), &[binding(1)]);
    for _ in 0..3 {
        let run = f.run(f.scope(), binding(1));
        let request = f
            .broker
            .prepare_request(f.connection, run, Operation::Result { max_bytes: 1 }, f.now)
            .unwrap();
        f.broker.settle_request(f.connection, request).unwrap();
        f.broker
            .complete(f.connection, run, Completion::Returned, f.now)
            .unwrap();
        f.broker.forget_run(f.connection, run).unwrap();
    }
    f.approve(f.scope(), &[binding(1)]);
    assert_eq!(
        f.broker
            .admit_run(f.connection, None, f.spec(f.scope(), binding(1)), f.now),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn principal_budget_survives_run_forgetting() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[direct(DirectKind::Observation)]);
    for _ in 0..32 {
        let run = f.run(f.scope(), direct(DirectKind::Observation));
        let request = f
            .broker
            .prepare_request(
                f.connection,
                run,
                Operation::Observe {
                    target: f.a,
                    max_bytes: MAX_OBSERVATION_BYTES,
                },
                f.now,
            )
            .unwrap();
        // Even aborting does not refund a disclosure reservation.
        f.broker.settle_request(f.connection, request).unwrap();
        f.broker
            .complete(f.connection, run, Completion::Returned, f.now)
            .unwrap();
        f.broker.forget_run(f.connection, run).unwrap();
    }
    let run = f.run(f.scope(), direct(DirectKind::Observation));
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            run,
            Operation::Observe {
                target: f.a,
                max_bytes: 1,
            },
            f.now
        ),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn child_deadline_and_cumulative_ancestor_budget_are_enforced() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[binding(1), binding(2)]);
    let mut parent_spec = f.spec(f.scope(), binding(1));
    parent_spec.budget.operations = 3;
    parent_spec.budget.disclosed_bytes = 10;
    let parent = f
        .broker
        .admit_run(f.connection, None, parent_spec.clone(), f.now)
        .unwrap();
    let mut child_spec = parent_spec;
    child_spec.binding = binding(2);
    child_spec.deadline += Duration::from_secs(1);
    assert_eq!(
        f.broker
            .admit_run(f.connection, Some(parent), child_spec.clone(), f.now),
        Err(Error::Unauthorized)
    );
    child_spec.deadline -= Duration::from_secs(1);
    let child = f
        .broker
        .admit_run(f.connection, Some(parent), child_spec, f.now)
        .unwrap();
    let request = f
        .broker
        .prepare_request(
            f.connection,
            child,
            Operation::Result { max_bytes: 10 },
            f.now,
        )
        .unwrap();
    f.broker.settle_request(f.connection, request).unwrap();
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            child,
            Operation::Result { max_bytes: 1 },
            f.now
        ),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn pending_host_calls_and_late_callbacks_cannot_cross_the_completion_cut() {
    let mut f = Fixture::new();
    let (run, lease) = f.controlled();
    let pending = f.write(run, lease);
    let status = f
        .broker
        .complete(f.connection, run, Completion::Returned, f.now)
        .unwrap();
    assert_eq!(status.state, RunState::Failed);
    assert_eq!(status.failure, Some(Error::UnsettledHostCalls));
    assert_eq!(status.admitted_effects, 0);
    assert_eq!(
        f.broker
            .commit_control(f.connection, pending, digest(3), 10, f.now),
        Err(Error::Closed)
    );
    let mut scope = f.scope();
    scope.control.insert(f.b);
    let fresh = f.run(scope, binding(2));
    let transition = f
        .broker
        .prepare_lease(f.connection, fresh, f.b, f.now)
        .unwrap();
    let fresh_lease = f.broker.commit_lease(transition, f.now).unwrap();
    assert_ne!(fresh_lease, lease);
}

#[test]
fn admitted_effects_survive_cancellation_as_unknown_not_rollback() {
    let mut f = Fixture::new();
    let (run, lease) = f.controlled();
    let request = f.write(run, lease);
    assert_eq!(
        f.broker
            .commit_control(f.connection, request, digest(4), 10, f.now),
        Err(Error::BindingMismatch)
    );
    assert_eq!(
        f.broker
            .commit_control(f.connection, request, digest(3), 11, f.now),
        Err(Error::BindingMismatch)
    );
    f.broker
        .commit_control(f.connection, request, digest(3), 10, f.now)
        .unwrap();
    assert_eq!(
        f.broker
            .commit_control(f.connection, request, digest(3), 10, f.now),
        Err(Error::Closed)
    );
    f.broker.cancel_run(f.connection, run, f.now).unwrap();
    let status = f.broker.status(f.connection, run, f.now).unwrap();
    assert_eq!(status.state, RunState::Cancelled);
    assert_eq!(status.admitted_effects, 1);
    assert!(status.execution_outcome_unknown);
}

#[test]
fn takeover_invalidates_queued_writes_and_permanently_disqualifies_old_runs() {
    let mut f = Fixture::new();
    let (run, lease) = f.controlled();
    let request = f.write(run, lease);
    f.broker.human_takeover(f.b).unwrap();
    assert!(f
        .broker
        .commit_control(f.connection, request, digest(3), 10, f.now)
        .is_err());
    assert!(f
        .broker
        .prepare_lease(f.connection, run, f.b, f.now)
        .is_err());
    let mut scope = f.scope();
    scope.control.insert(f.b);
    f.approve(scope.clone(), &[binding(1)]);
    assert!(f
        .broker
        .prepare_lease(f.connection, run, f.b, f.now)
        .is_err());
    let fresh = f.run(scope, binding(1));
    assert!(f
        .broker
        .prepare_lease(f.connection, fresh, f.b, f.now)
        .is_ok());
}

#[test]
fn lease_ownership_is_exclusive_and_pending_transition_does_not_authorize() {
    let mut f = Fixture::new();
    let mut scope = f.scope();
    scope.control.insert(f.b);
    f.approve(scope.clone(), &[binding(1), binding(2)]);
    let first = f.run(scope.clone(), binding(1));
    let second = f.run(scope, binding(2));
    let first_transition = f
        .broker
        .prepare_lease(f.connection, first, f.b, f.now)
        .unwrap();
    let second_transition = f
        .broker
        .prepare_lease(f.connection, second, f.b, f.now)
        .unwrap();
    let lease = f.broker.commit_lease(first_transition, f.now).unwrap();
    assert_eq!(
        f.broker.commit_lease(second_transition, f.now),
        Err(Error::Busy)
    );
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            second,
            Operation::Control {
                lease,
                input: digest(3),
                bytes: 10,
            },
            f.now
        ),
        Err(Error::Unauthorized)
    );
}

#[test]
fn human_input_before_a_pending_lease_commit_requires_a_new_native_grant() {
    let mut f = Fixture::new();
    let mut scope = f.scope();
    scope.control.insert(f.b);
    f.approve(scope.clone(), &[binding(1)]);
    let run = f.run(scope, binding(1));
    let transition = f
        .broker
        .prepare_lease(f.connection, run, f.b, f.now)
        .unwrap();
    f.broker.human_takeover(f.b).unwrap();
    assert!(f.broker.commit_lease(transition, f.now).is_err());
    assert!(f
        .broker
        .prepare_lease(f.connection, run, f.b, f.now)
        .is_err());
}

#[test]
fn parent_return_cancels_children_and_releases_their_writer_leases() {
    let mut f = Fixture::new();
    let mut scope = f.scope();
    scope.control.insert(f.b);
    f.approve(scope.clone(), &[binding(1), binding(2)]);
    let parent = f.run(scope.clone(), binding(1));
    let child = f
        .broker
        .admit_run(f.connection, Some(parent), f.spec(scope, binding(2)), f.now)
        .unwrap();
    let transition = f
        .broker
        .prepare_lease(f.connection, child, f.b, f.now)
        .unwrap();
    f.broker.commit_lease(transition, f.now).unwrap();
    let status = f
        .broker
        .complete(f.connection, parent, Completion::Returned, f.now)
        .unwrap();
    assert_eq!(status.failure, Some(Error::UnsettledChildRuns));
    assert_eq!(
        f.broker.status(f.connection, child, f.now).unwrap().state,
        RunState::Cancelled
    );
    assert!(f
        .broker
        .prepare_lease(f.connection, child, f.b, f.now)
        .is_err());
}

#[test]
fn child_inherits_sources_even_when_its_observe_scope_is_empty() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[binding(1), binding(2)]);
    let parent = f.run(f.scope(), binding(1));
    let mut child_scope = f.scope();
    child_scope.observe.clear();
    child_scope.discover.clear();
    let child = f
        .broker
        .admit_run(
            f.connection,
            Some(parent),
            f.spec(child_scope, binding(2)),
            f.now,
        )
        .unwrap();
    let request = f
        .broker
        .prepare_request(
            f.connection,
            child,
            Operation::Result { max_bytes: 10 },
            f.now,
        )
        .unwrap();
    f.broker.close_target(f.a).unwrap();
    assert!(f
        .broker
        .commit_disclosure(f.connection, request, 1, f.now)
        .is_err());
}

#[test]
fn request_and_response_bounds_refuse_before_disclosure() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[binding(1)]);
    let run = f.run(f.scope(), binding(1));
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            run,
            Operation::Observe {
                target: f.a,
                max_bytes: MAX_OBSERVATION_BYTES + 1,
            },
            f.now
        ),
        Err(Error::InvalidRequest)
    );
    let request = f
        .broker
        .prepare_request(f.connection, run, Operation::Result { max_bytes: 1 }, f.now)
        .unwrap();
    assert_eq!(
        f.broker.commit_disclosure(f.connection, request, 2, f.now),
        Err(Error::InvalidRequest)
    );
    f.broker
        .commit_disclosure(f.connection, request, 1, f.now)
        .unwrap();
    assert_eq!(
        f.broker.commit_disclosure(f.connection, request, 1, f.now),
        Err(Error::InvalidRequest)
    );
    for _ in 1..16 {
        f.broker
            .prepare_request(f.connection, run, Operation::Result { max_bytes: 1 }, f.now)
            .unwrap();
    }
    assert_eq!(
        f.broker
            .prepare_request(f.connection, run, Operation::Result { max_bytes: 1 }, f.now),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn normal_start_response_does_not_orphan_or_cancel_its_distinct_workflow() {
    let mut f = Fixture::new();
    let scope = f.scope();
    f.approve(
        scope.clone(),
        &[direct(DirectKind::WorkflowStart), binding(1)],
    );
    let context = f.run(scope.clone(), direct(DirectKind::WorkflowStart));
    let spec = f.spec(scope, binding(1));
    let key = StartKey([1; 16]);
    let workflow = f
        .broker
        .start_workflow(f.connection, context, key, spec.clone(), f.now)
        .unwrap();
    f.broker
        .complete(f.connection, context, Completion::Returned, f.now)
        .unwrap();
    assert_eq!(
        f.broker
            .status(f.connection, workflow, f.now)
            .unwrap()
            .state,
        RunState::Running
    );
    let retry = f.run(f.scope(), direct(DirectKind::WorkflowStart));
    assert_eq!(
        f.broker
            .start_workflow(f.connection, retry, key, spec.clone(), f.now),
        Ok(workflow)
    );
    let mut mismatch = spec.clone();
    mismatch.binding.parameters = digest(7);
    assert_eq!(
        f.broker
            .start_workflow(f.connection, retry, key, mismatch, f.now),
        Err(Error::BindingMismatch)
    );
    // A delayed cancellation of the already completed start context is
    // explicitly stronger than its earlier normal response.
    f.broker.cancel_run(f.connection, context, f.now).unwrap();
    f.broker.cancel_start(f.connection, key, f.now).unwrap();
    assert_eq!(
        f.broker
            .start_workflow(f.connection, retry, key, spec, f.now),
        Ok(workflow)
    );
    assert_eq!(
        f.broker
            .status(f.connection, workflow, f.now)
            .unwrap()
            .state,
        RunState::Cancelled
    );
}

#[test]
fn lock_and_disconnect_close_all_admission_without_restoring_grants() {
    let mut f = Fixture::new();
    let (run, lease) = f.controlled();
    let request = f.write(run, lease);
    f.broker.revoke_all();
    assert!(f
        .broker
        .commit_control(f.connection, request, digest(3), 10, f.now)
        .is_err());
    assert!(f.broker.status(f.connection, run, f.now).is_err());
    f.broker.disconnect(f.connection);
    let replacement = f.broker.accept_connection().unwrap();
    assert_ne!(replacement, f.connection);
    assert_eq!(
        f.broker.status(replacement, run, f.now),
        Err(Error::Unauthorized)
    );
}

#[test]
fn pending_native_control_approval_is_invalidated_by_an_ownership_change() {
    let mut f = Fixture::new();
    let mut scope = f.scope();
    scope.control.insert(f.b);
    let proposal = GrantRequest {
        scope,
        bindings: BTreeSet::from([binding(1)]),
    };
    let pending = f
        .broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    // No grant or lease exists yet; human input must still invalidate the
    // ownership snapshot reviewed by a pending approval.
    f.broker.human_takeover(f.b).unwrap();
    assert_eq!(
        f.broker.approve(
            f.connection,
            pending,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::from([f.a])),
            f.now
        ),
        Err(Error::Revoked)
    );
}

#[test]
fn successful_and_exceptional_completion_release_the_invocations_own_lease() {
    for completion in [Completion::Returned, Completion::Failed] {
        let mut f = Fixture::new();
        let (run, lease) = f.controlled();
        let status = f
            .broker
            .complete(f.connection, run, completion, f.now)
            .unwrap();
        assert_eq!(
            status.state,
            if completion == Completion::Returned {
                RunState::Completed
            } else {
                RunState::Failed
            }
        );
        let mut scope = f.scope();
        scope.control.insert(f.b);
        let next = f.run(scope, binding(2));
        let transition = f
            .broker
            .prepare_lease(f.connection, next, f.b, f.now)
            .unwrap();
        assert_ne!(f.broker.commit_lease(transition, f.now).unwrap(), lease);
    }
}

#[test]
fn final_result_admission_is_atomic_with_the_successful_completion_cut() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[binding(1), binding(2)]);
    let run = f.run(f.scope(), binding(1));
    f.broker
        .prepare_request(
            f.connection,
            run,
            Operation::Observe {
                target: f.a,
                max_bytes: 10,
            },
            f.now,
        )
        .unwrap();
    let result = f
        .broker
        .prepare_request(
            f.connection,
            run,
            Operation::Result { max_bytes: 10 },
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker
            .complete_with_result(f.connection, run, result, 10, f.now),
        Err(Error::UnsettledHostCalls)
    );
    assert_eq!(
        f.broker.status(f.connection, run, f.now).unwrap().state,
        RunState::Failed
    );
    let next = f.run(f.scope(), binding(2));
    let result = f
        .broker
        .prepare_request(
            f.connection,
            next,
            Operation::Result { max_bytes: 10 },
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker
            .complete_with_result(f.connection, next, result, 10, f.now)
            .unwrap()
            .state,
        RunState::Completed
    );
    assert_eq!(
        f.broker.commit_disclosure(f.connection, result, 1, f.now),
        Err(Error::Closed)
    );
}

#[test]
fn explicit_start_context_cancellation_before_and_after_admission_stops_the_run() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[direct(DirectKind::WorkflowStart), binding(1)]);
    let context = f.run(f.scope(), direct(DirectKind::WorkflowStart));
    f.broker.cancel_run(f.connection, context, f.now).unwrap();
    assert_eq!(
        f.broker.start_workflow(
            f.connection,
            context,
            StartKey([1; 16]),
            f.spec(f.scope(), binding(1)),
            f.now
        ),
        Err(Error::Closed)
    );
    let context = f.run(f.scope(), direct(DirectKind::WorkflowStart));
    let workflow = f
        .broker
        .start_workflow(
            f.connection,
            context,
            StartKey([2; 16]),
            f.spec(f.scope(), binding(1)),
            f.now,
        )
        .unwrap();
    f.broker.cancel_run(f.connection, context, f.now).unwrap();
    assert_eq!(
        f.broker
            .status(f.connection, workflow, f.now)
            .unwrap()
            .state,
        RunState::Cancelled
    );
}

#[test]
fn workflow_concurrency_is_bounded_per_principal_and_globally() {
    let mut f = Fixture::new();
    for principal in 0..3 {
        if principal > 0 {
            f.connection = f.broker.accept_connection().unwrap();
        }
        f.approve(f.scope(), &[binding(1)]);
        if principal < 2 {
            f.run(f.scope(), binding(1));
            f.run(f.scope(), binding(1));
        }
        assert_eq!(
            f.broker
                .admit_run(f.connection, None, f.spec(f.scope(), binding(1)), f.now),
            Err(Error::QuotaExceeded)
        );
    }
}

#[test]
fn exact_five_minute_grant_expiry_never_restores_a_run_on_reapproval() {
    let mut f = Fixture::new();
    f.approve(f.scope(), &[binding(1)]);
    let mut spec = f.spec(f.scope(), binding(1));
    spec.deadline = f.now + MAX_GRANT_LIFETIME;
    let run = f.broker.admit_run(f.connection, None, spec, f.now).unwrap();
    let expiry = f.now + MAX_GRANT_LIFETIME;
    assert_eq!(
        f.broker.status(f.connection, run, expiry),
        Err(Error::Revoked)
    );
    f.now = expiry;
    f.approve(f.scope(), &[binding(1)]);
    assert_eq!(
        f.broker
            .prepare_request(f.connection, run, Operation::Result { max_bytes: 1 }, f.now),
        Err(Error::Revoked)
    );
}
