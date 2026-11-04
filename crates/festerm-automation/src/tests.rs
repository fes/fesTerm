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

fn direct_input(target: TargetRef, input: Digest, bytes: u32) -> InvocationBinding {
    InvocationBinding {
        api_version: 1,
        invocation: Invocation::DirectInput {
            target,
            input,
            bytes,
        },
        parameters: digest(99),
    }
}

fn input_scope(target: TargetRef) -> Scope {
    Scope {
        control: BTreeSet::from([target]),
        ..Scope::default()
    }
}

fn input_operation(lease: LeaseRef, input: Digest, bytes: u32) -> Operation {
    Operation::Control {
        lease,
        input,
        bytes,
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
        Self::with_limits(Limits::default())
    }

    fn with_limits(limits: Limits) -> Self {
        let mut broker = Broker::with_limits([7; 16], limits);
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
            direct_read_calls: BTreeSet::new(),
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

    fn lease(&mut self, run: RunId, target: TargetRef) -> LeaseRef {
        let transition = self
            .broker
            .prepare_lease(self.connection, run, target, self.now)
            .unwrap();
        self.broker.commit_lease(transition, self.now).unwrap()
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
        direct_read_calls: BTreeSet::new(),
    })
    .unwrap();
    malicious["approved"] = true.into();
    assert!(serde_json::from_value::<GrantRequest>(malicious).is_err());
}

#[test]
fn closed_targets_can_be_forgotten_without_resurrecting_old_references() {
    let mut broker = Broker::with_limits(
        [7; 16],
        Limits {
            targets: 1,
            ..Limits::default()
        },
    );
    let first = broker.attach_target(1, 1).unwrap();
    assert_eq!(broker.forget_closed_target(first), Err(Error::StaleTarget));
    broker.close_target(first).unwrap();
    broker.forget_closed_target(first).unwrap();
    assert_eq!(broker.close_target(first), Err(Error::StaleTarget));
    assert_eq!(broker.attach_target(1, 1), Err(Error::StaleTarget));
    assert_eq!(broker.attach_target(2, 1), Err(Error::StaleTarget));

    for generation in 2..=130 {
        let target = broker.attach_target(generation, generation).unwrap();
        assert_ne!(target, first);
        broker.close_target(target).unwrap();
        broker.forget_closed_target(target).unwrap();
    }
    let replacement = broker.attach_target(1, 131).unwrap();
    assert_ne!(replacement, first);
    assert_eq!(broker.close_target(first), Err(Error::StaleTarget));
}

#[test]
fn flexible_direct_reads_require_native_approval_and_never_grant_effects() {
    let mut f = Fixture::new();
    let proposal = GrantRequest {
        scope: f.scope(),
        bindings: BTreeSet::new(),
        direct_read_calls: BTreeSet::from([DirectKind::Observation]),
    };
    let pending = f
        .broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    let mut widened = proposal.clone();
    widened.direct_read_calls.insert(DirectKind::Discovery);
    assert_eq!(
        f.broker.approve(
            f.connection,
            pending,
            NativeApproval::new(widened, MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .approve(
            f.connection,
            pending,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME),
            f.now,
        )
        .unwrap();

    let mut first = direct(DirectKind::Observation);
    first.parameters = digest(1);
    let mut second = first;
    second.parameters = digest(2);
    let run = f.run(f.scope(), first);
    assert!(f
        .broker
        .admit_run(f.connection, None, f.spec(f.scope(), second), f.now)
        .is_ok());
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            run,
            Operation::Observe {
                target: f.b,
                max_bytes: 10,
            },
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.admit_run(
            f.connection,
            None,
            f.spec(f.scope(), direct(DirectKind::WorkflowStart)),
            f.now,
        ),
        Err(Error::Unauthorized)
    );

    let invalid = GrantRequest {
        scope: f.scope(),
        bindings: BTreeSet::new(),
        direct_read_calls: BTreeSet::from([DirectKind::WorkflowStart]),
    };
    assert_eq!(
        f.broker.request_approval(f.connection, invalid, f.now),
        Err(Error::InvalidRequest)
    );
}

#[test]
fn exact_direct_bindings_remain_parameter_specific() {
    let mut f = Fixture::new();
    let approved = direct(DirectKind::Observation);
    f.approve(f.scope(), &[approved]);
    let mut changed = approved;
    changed.parameters = digest(1);
    assert_eq!(
        f.broker
            .admit_run(f.connection, None, f.spec(f.scope(), changed), f.now),
        Err(Error::Unauthorized)
    );
}

#[test]
fn direct_input_requires_exact_native_handoff_without_workflow_or_observation() {
    let mut f = Fixture::new();
    let scope = input_scope(f.b);
    let action = direct_input(f.b, digest(3), 10);
    let proposal = GrantRequest {
        scope: scope.clone(),
        bindings: BTreeSet::from([action]),
        direct_read_calls: BTreeSet::new(),
    };
    let mut invalid = proposal.clone();
    invalid.scope.control.clear();
    assert_eq!(
        f.broker.request_approval(f.connection, invalid, f.now),
        Err(Error::InvalidRequest)
    );
    for bytes in [0, MAX_INPUT_BYTES + 1] {
        let mut invalid = proposal.clone();
        invalid.bindings = BTreeSet::from([direct_input(f.b, digest(3), bytes)]);
        assert_eq!(
            f.broker.request_approval(f.connection, invalid, f.now),
            Err(Error::InvalidRequest)
        );
    }
    let pending = f
        .broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    assert_eq!(
        f.broker.approve(
            f.connection,
            pending,
            NativeApproval::new(proposal.clone(), MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .approve(
            f.connection,
            pending,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::new()),
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker
            .admit_run(f.connection, None, f.spec(Scope::default(), action), f.now),
        Err(Error::Unauthorized)
    );
    let mut expired = f.spec(scope.clone(), action);
    expired.deadline = f.now;
    assert_eq!(
        f.broker.admit_run(f.connection, None, expired, f.now),
        Err(Error::InvalidRequest)
    );
    let mut too_late = f.spec(scope.clone(), action);
    too_late.deadline = f.now + MAX_GRANT_LIFETIME + Duration::from_secs(1);
    assert_eq!(
        f.broker.admit_run(f.connection, None, too_late, f.now),
        Err(Error::InvalidRequest)
    );

    let run = f.run(scope, action);
    let lease = f.lease(run, f.b);
    let request = f
        .broker
        .prepare_request(
            f.connection,
            run,
            input_operation(lease, digest(3), 10),
            f.now,
        )
        .unwrap();
    f.broker
        .commit_control(f.connection, request, digest(3), 10, f.now)
        .unwrap();
    f.broker.settle_request(f.connection, request).unwrap();
    let status = f
        .broker
        .complete(f.connection, run, Completion::Returned, f.now)
        .unwrap();
    assert_eq!(status.state, RunState::Completed);
    assert_eq!(status.admitted_effects, 1);
    assert!(status.execution_outcome_unknown);
}

#[test]
fn direct_input_binds_parameters_target_lease_and_exact_committed_input() {
    let mut f = Fixture::new();
    let mut scope = f.scope();
    scope.control = BTreeSet::from([f.a, f.b]);
    let action = direct_input(f.b, digest(3), 10);
    f.approve(scope.clone(), &[action, binding(1)]);

    let mut changed_parameters = action;
    changed_parameters.parameters = digest(100);
    for changed in [
        changed_parameters,
        direct_input(f.a, digest(3), 10),
        direct_input(f.b, digest(4), 10),
        direct_input(f.b, digest(3), 11),
    ] {
        assert_eq!(
            f.broker
                .admit_run(f.connection, None, f.spec(scope.clone(), changed), f.now),
            Err(Error::Unauthorized)
        );
    }
    let run = f.run(scope.clone(), action);
    assert!(matches!(
        f.broker.prepare_lease(f.connection, run, f.a, f.now),
        Err(Error::BindingMismatch)
    ));
    let replaced = TargetRef {
        generation: f.b.generation + 1,
        ..f.b
    };
    assert!(matches!(
        f.broker.prepare_lease(f.connection, run, replaced, f.now),
        Err(Error::BindingMismatch)
    ));
    let lease = f.lease(run, f.b);
    let another_run = f.run(scope.clone(), action);
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            another_run,
            input_operation(lease, digest(3), 10),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    let other = f.broker.accept_connection().unwrap();
    assert!(matches!(
        f.broker.prepare_lease(other, run, f.b, f.now),
        Err(Error::Unauthorized)
    ));
    for operation in [
        Operation::Discover { max_bytes: 10 },
        Operation::Observe {
            target: f.a,
            max_bytes: 10,
        },
        Operation::Result { max_bytes: 10 },
    ] {
        assert_eq!(
            f.broker
                .prepare_request(f.connection, run, operation, f.now),
            Err(Error::Unauthorized)
        );
    }
    assert_eq!(
        f.broker.start_workflow(
            f.connection,
            run,
            StartKey([1; 16]),
            f.spec(scope.clone(), binding(1)),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .admit_run(f.connection, Some(run), f.spec(scope, binding(1)), f.now,),
        Err(Error::Unauthorized)
    );
    for operation in [
        input_operation(
            LeaseRef {
                target: f.a,
                ..lease
            },
            digest(3),
            10,
        ),
        input_operation(lease, digest(4), 10),
        input_operation(lease, digest(3), 11),
    ] {
        assert_eq!(
            f.broker
                .prepare_request(f.connection, run, operation, f.now),
            Err(Error::BindingMismatch)
        );
    }
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            run,
            input_operation(
                LeaseRef {
                    epoch: lease.epoch + 1,
                    ..lease
                },
                digest(3),
                10
            ),
            f.now,
        ),
        Err(Error::Revoked)
    );
    let request = f
        .broker
        .prepare_request(
            f.connection,
            run,
            input_operation(lease, digest(3), 10),
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker
            .commit_control(other, request, digest(3), 10, f.now),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.settle_request(other, request),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.commit_disclosure(f.connection, request, 0, f.now),
        Err(Error::InvalidRequest)
    );
    assert_eq!(
        f.broker
            .complete_with_result(f.connection, run, request, 0, f.now),
        Err(Error::InvalidRequest)
    );
    for (input, bytes) in [(digest(4), 10), (digest(3), 11)] {
        assert_eq!(
            f.broker
                .commit_control(f.connection, request, input, bytes, f.now),
            Err(Error::BindingMismatch)
        );
    }
    f.broker
        .commit_control(f.connection, request, digest(3), 10, f.now)
        .unwrap();
}

#[test]
fn direct_read_policy_never_authorizes_a_direct_input_action() {
    let mut f = Fixture::new();
    let mut scope = input_scope(f.b);
    scope.observe.insert(f.a);
    let proposal = GrantRequest {
        scope: scope.clone(),
        bindings: BTreeSet::new(),
        direct_read_calls: BTreeSet::from([DirectKind::Observation]),
    };
    let mut wire = serde_json::to_value(&proposal).unwrap();
    wire["direct_read_calls"] = serde_json::json!(["control"]);
    assert!(serde_json::from_value::<GrantRequest>(wire).is_err());
    let pending = f
        .broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    f.broker
        .approve(
            f.connection,
            pending,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::from([f.a])),
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker.admit_run(
            f.connection,
            None,
            f.spec(scope.clone(), direct_input(f.b, digest(3), 10)),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    let read = f.run(scope, direct(DirectKind::Observation));
    assert!(matches!(
        f.broker.prepare_lease(f.connection, read, f.b, f.now),
        Err(Error::Unauthorized)
    ));
}

#[test]
fn direct_input_is_one_request_even_after_abort_settlement_or_lost_response() {
    let mut f = Fixture::new();
    let scope = input_scope(f.b);
    let action = direct_input(f.b, digest(3), 10);
    f.approve(scope.clone(), &[action]);

    for disposition in 0..3 {
        let run = f.run(scope.clone(), action);
        let lease = f.lease(run, f.b);
        let operation = input_operation(lease, digest(3), 10);
        let request = f
            .broker
            .prepare_request(f.connection, run, operation, f.now)
            .unwrap();
        assert_eq!(
            f.broker
                .prepare_request(f.connection, run, operation, f.now),
            Err(Error::Closed)
        );
        if disposition == 0 {
            f.broker.settle_request(f.connection, request).unwrap();
            assert_eq!(
                f.broker
                    .commit_control(f.connection, request, digest(3), 10, f.now),
                Err(Error::Closed)
            );
        } else {
            f.broker
                .commit_control(f.connection, request, digest(3), 10, f.now)
                .unwrap();
            assert_eq!(
                f.broker
                    .commit_control(f.connection, request, digest(3), 10, f.now),
                Err(Error::Closed)
            );
            if disposition == 2 {
                f.broker.settle_request(f.connection, request).unwrap();
            }
        }
        assert_eq!(
            f.broker
                .prepare_request(f.connection, run, operation, f.now),
            Err(Error::Closed)
        );
        assert!(matches!(
            f.broker.prepare_lease(f.connection, run, f.b, f.now),
            Err(Error::Closed)
        ));
        let status = f
            .broker
            .complete(f.connection, run, Completion::Returned, f.now)
            .unwrap();
        assert_eq!(status.admitted_effects, u32::from(disposition != 0));
        assert_eq!(status.execution_outcome_unknown, disposition != 0);
        assert_eq!(
            status.failure,
            (disposition == 1).then_some(Error::UnsettledHostCalls)
        );
        assert_eq!(
            f.broker
                .commit_control(f.connection, request, digest(3), 10, f.now),
            Err(Error::Closed)
        );
    }
}

#[test]
fn control_approval_does_not_transfer_an_existing_writer() {
    for owner_is_workflow in [false, true] {
        let mut f = Fixture::new();
        let mut scope = input_scope(f.b);
        scope.invoke_workflows = true;
        let direct_action = direct_input(f.b, digest(3), 10);
        let workflow_action = binding(1);
        let (owner_action, other_action) = if owner_is_workflow {
            (workflow_action, direct_action)
        } else {
            (direct_action, workflow_action)
        };
        f.approve(scope.clone(), &[owner_action]);
        let owner = f.run(scope.clone(), owner_action);
        let lease = f.lease(owner, f.b);
        let request = f
            .broker
            .prepare_request(
                f.connection,
                owner,
                input_operation(lease, digest(3), 10),
                f.now,
            )
            .unwrap();
        let other = f.broker.accept_connection().unwrap();
        let proposal = GrantRequest {
            scope: scope.clone(),
            bindings: BTreeSet::from([other_action]),
            direct_read_calls: BTreeSet::new(),
        };
        let pending = f
            .broker
            .request_approval(other, proposal.clone(), f.now)
            .unwrap();
        f.broker
            .approve(
                other,
                pending,
                NativeApproval::new(proposal, MAX_GRANT_LIFETIME)
                    .accept_control_handoff(f.b, BTreeSet::new()),
                f.now,
            )
            .unwrap();
        let competing = f
            .broker
            .admit_run(other, None, f.spec(scope, other_action), f.now)
            .unwrap();
        assert!(matches!(
            f.broker.prepare_lease(other, competing, f.b, f.now),
            Err(Error::Busy)
        ));
        f.broker
            .commit_control(f.connection, request, digest(3), 10, f.now)
            .unwrap();
        f.broker.settle_request(f.connection, request).unwrap();
        let status = f
            .broker
            .complete(f.connection, owner, Completion::Returned, f.now)
            .unwrap();
        assert_eq!(status.admitted_effects, 1);
        assert!(status.execution_outcome_unknown);
    }
}

#[test]
fn control_approval_preserves_other_pending_snapshots_without_a_writer() {
    let mut f = Fixture::new();
    let scope = input_scope(f.b);
    let action = direct_input(f.b, digest(3), 10);
    let proposal = GrantRequest {
        scope: scope.clone(),
        bindings: BTreeSet::from([action]),
        direct_read_calls: BTreeSet::new(),
    };
    let other = f.broker.accept_connection().unwrap();
    let first = f
        .broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    let second = f
        .broker
        .request_approval(other, proposal.clone(), f.now)
        .unwrap();
    f.broker
        .approve(
            f.connection,
            first,
            NativeApproval::new(proposal.clone(), MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::new()),
            f.now,
        )
        .unwrap();
    f.broker
        .approve(
            other,
            second,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::new()),
            f.now,
        )
        .unwrap();
    let first_run = f.run(scope.clone(), action);
    let second_run = f
        .broker
        .admit_run(other, None, f.spec(scope, action), f.now)
        .unwrap();
    let first_lease = f
        .broker
        .prepare_lease(f.connection, first_run, f.b, f.now)
        .unwrap();
    let second_lease = f
        .broker
        .prepare_lease(other, second_run, f.b, f.now)
        .unwrap();
    f.broker.commit_lease(first_lease, f.now).unwrap();
    assert_eq!(f.broker.commit_lease(second_lease, f.now), Err(Error::Busy));
}

#[test]
fn explicit_takeover_cuts_cross_principal_control_and_stale_approvals() {
    let mut f = Fixture::new();
    let scope = input_scope(f.b);
    let action = direct_input(f.b, digest(3), 10);
    f.approve(scope.clone(), &[action]);
    let old = f.run(scope.clone(), action);
    let lease = f.lease(old, f.b);
    let request = f
        .broker
        .prepare_request(
            f.connection,
            old,
            input_operation(lease, digest(3), 10),
            f.now,
        )
        .unwrap();
    let second = f.broker.accept_connection().unwrap();
    let third = f.broker.accept_connection().unwrap();
    let mut workflow_scope = scope.clone();
    workflow_scope.invoke_workflows = true;
    let proposal = GrantRequest {
        scope: workflow_scope.clone(),
        bindings: BTreeSet::from([binding(1)]),
        direct_read_calls: BTreeSet::new(),
    };
    let next = f
        .broker
        .request_approval(second, proposal.clone(), f.now)
        .unwrap();
    f.broker
        .approve(
            second,
            next,
            NativeApproval::new(proposal.clone(), MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::new()),
            f.now,
        )
        .unwrap();
    let waiting = f
        .broker
        .admit_run(
            second,
            None,
            f.spec(workflow_scope.clone(), binding(1)),
            f.now,
        )
        .unwrap();
    let stale = f
        .broker
        .request_approval(third, proposal.clone(), f.now)
        .unwrap();
    f.broker.human_takeover(f.b).unwrap();
    assert_eq!(
        f.broker.approve(
            third,
            stale,
            NativeApproval::new(proposal.clone(), MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::new()),
            f.now,
        ),
        Err(Error::Revoked)
    );
    assert_eq!(
        f.broker
            .commit_control(f.connection, request, digest(3), 10, f.now),
        Err(Error::Closed)
    );
    assert!(f
        .broker
        .prepare_lease(f.connection, old, f.b, f.now)
        .is_err());
    assert!(f.broker.prepare_lease(second, waiting, f.b, f.now).is_err());
    let next = f
        .broker
        .request_approval(second, proposal.clone(), f.now)
        .unwrap();
    f.broker
        .approve(
            second,
            next,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME)
                .accept_control_handoff(f.b, BTreeSet::new()),
            f.now,
        )
        .unwrap();
    assert!(f.broker.prepare_lease(second, waiting, f.b, f.now).is_err());
    let next_run = f
        .broker
        .admit_run(second, None, f.spec(workflow_scope, binding(1)), f.now)
        .unwrap();
    let transition = f
        .broker
        .prepare_lease(second, next_run, f.b, f.now)
        .unwrap();
    let next_lease = f.broker.commit_lease(transition, f.now).unwrap();
    assert_ne!(lease, next_lease);
    f.approve(scope, &[action]);
    assert!(f
        .broker
        .prepare_lease(f.connection, old, f.b, f.now)
        .is_err());
    let request = f
        .broker
        .prepare_request(
            second,
            next_run,
            input_operation(next_lease, digest(3), 10),
            f.now,
        )
        .unwrap();
    f.broker
        .commit_control(second, request, digest(3), 10, f.now)
        .unwrap();
}

#[test]
fn direct_input_lifecycle_revocation_never_restores_old_runs() {
    for event in 0..8 {
        let mut f = Fixture::new();
        let scope = input_scope(f.b);
        let action = direct_input(f.b, digest(3), 10);
        f.approve(scope.clone(), &[action]);
        let mut spec = f.spec(scope, action);
        spec.deadline = if event == 6 {
            f.now + Duration::from_secs(1)
        } else {
            f.now + MAX_GRANT_LIFETIME
        };
        let run = f.broker.admit_run(f.connection, None, spec, f.now).unwrap();
        let lease = f.lease(run, f.b);
        let request = f
            .broker
            .prepare_request(
                f.connection,
                run,
                input_operation(lease, digest(3), 10),
                f.now,
            )
            .unwrap();
        match event {
            0 => f.broker.human_takeover(f.b).unwrap(),
            1 => f.broker.revoke(f.connection),
            2 => f.broker.revoke_all(),
            3 => {
                f.broker.close_target(f.b).unwrap();
                f.b = f.broker.attach_target(2, 2).unwrap();
            }
            4 => f.b = f.broker.attach_target(2, 2).unwrap(),
            5 => f.broker.disconnect(f.connection),
            6 => f.now += Duration::from_secs(1),
            7 => f.now += MAX_GRANT_LIFETIME,
            _ => unreachable!(),
        }
        assert!(f
            .broker
            .commit_control(f.connection, request, digest(3), 10, f.now)
            .is_err());
        assert!(f
            .broker
            .prepare_lease(f.connection, run, lease.target(), f.now)
            .is_err());
        if event == 5 {
            f.connection = f.broker.accept_connection().unwrap();
        }
        let new_action = direct_input(f.b, digest(3), 10);
        f.approve(input_scope(f.b), &[new_action]);
        assert!(f
            .broker
            .prepare_lease(f.connection, run, lease.target(), f.now)
            .is_err());
    }
}

#[test]
fn direct_input_charges_run_principal_and_global_budgets_across_invocations() {
    let mut f = Fixture::with_limits(Limits {
        global_operations: 3,
        ..Limits::default()
    });
    let scope = input_scope(f.b);
    let action = direct_input(f.b, digest(3), 10);
    f.approve(scope.clone(), &[action]);
    let first = f.run(scope.clone(), action);
    let lease = f.lease(first, f.b);
    let request = f
        .broker
        .prepare_request(
            f.connection,
            first,
            input_operation(lease, digest(3), 10),
            f.now,
        )
        .unwrap();
    f.broker.settle_request(f.connection, request).unwrap();
    f.broker
        .complete(f.connection, first, Completion::Returned, f.now)
        .unwrap();
    let second = f.run(scope.clone(), action);
    let lease = f.lease(second, f.b);
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            second,
            input_operation(lease, digest(3), 10),
            f.now,
        ),
        Err(Error::QuotaExceeded)
    );
    f.approve(scope.clone(), &[action]);
    assert_eq!(
        f.broker
            .admit_run(f.connection, None, f.spec(scope, action), f.now),
        Err(Error::QuotaExceeded)
    );

    let mut f = Fixture::new();
    let scope = input_scope(f.b);
    let action = direct_input(f.b, digest(3), 10);
    f.approve(scope.clone(), &[action]);
    let mut spec = f.spec(scope.clone(), action);
    spec.budget.operations = 1;
    let run = f.broker.admit_run(f.connection, None, spec, f.now).unwrap();
    let lease = f.lease(run, f.b);
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            run,
            input_operation(lease, digest(3), 10),
            f.now,
        ),
        Err(Error::QuotaExceeded)
    );
    f.broker.cancel_run(f.connection, run, f.now).unwrap();
    f.broker.forget_run(f.connection, run).unwrap();
    for _ in 0..127 {
        let run = f.run(scope.clone(), action);
        let lease = f.lease(run, f.b);
        let request = f
            .broker
            .prepare_request(
                f.connection,
                run,
                input_operation(lease, digest(3), 10),
                f.now,
            )
            .unwrap();
        f.broker.settle_request(f.connection, request).unwrap();
        f.broker
            .complete(f.connection, run, Completion::Returned, f.now)
            .unwrap();
        f.broker.forget_run(f.connection, run).unwrap();
    }
    let final_run = f.run(scope.clone(), action);
    let lease = f.lease(final_run, f.b);
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            final_run,
            input_operation(lease, digest(3), 10),
            f.now,
        ),
        Err(Error::QuotaExceeded)
    );
    f.approve(scope.clone(), &[action]);
    assert!(f
        .broker
        .admit_run(f.connection, None, f.spec(scope, action), f.now)
        .is_ok());
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
        direct_read_calls: BTreeSet::new(),
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
        direct_read_calls: BTreeSet::new(),
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
        direct_read_calls: BTreeSet::new(),
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
