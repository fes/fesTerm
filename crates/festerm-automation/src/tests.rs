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

fn metadata_request(targets: &[TargetRef]) -> MetadataGrantRequest {
    MetadataGrantRequest {
        targets: targets.iter().copied().collect(),
    }
}

fn metadata_spec(targets: &[TargetRef], deadline: Instant) -> MetadataRunSpec {
    MetadataRunSpec {
        parameters: digest(99),
        targets: targets.iter().copied().collect(),
        budget: Budget::default(),
        deadline,
    }
}

fn approve_metadata(
    f: &mut Fixture,
    targets: &[TargetRef],
    controls: &[TargetRef],
    lifetime: Duration,
) {
    let request = metadata_request(targets);
    let id = f
        .broker
        .request_metadata_approval(f.connection, request.clone(), f.now)
        .unwrap();
    let mut approval = NativeMetadataApproval::new(request.clone(), lifetime);
    for control in controls {
        approval = approval.accept_control_disclosure(*control, request.targets.clone());
    }
    f.broker
        .approve_metadata(f.connection, id, approval, f.now)
        .unwrap();
}

fn metadata_run(f: &mut Fixture, targets: &[TargetRef], deadline: Instant) -> RunId {
    f.broker
        .admit_metadata_run(f.connection, metadata_spec(targets, deadline), f.now)
        .unwrap()
}

fn shared_control_scope(f: &Fixture) -> Scope {
    Scope {
        discover: BTreeSet::from([f.a]),
        control: BTreeSet::from([f.b]),
        ..Scope::default()
    }
}

fn approve_control(f: &mut Fixture, lifetime: Duration) {
    let scope = shared_control_scope(f);
    let request = GrantRequest {
        scope: scope.clone(),
        bindings: BTreeSet::from([direct_input(f.b, digest(3), 10)]),
        direct_read_calls: BTreeSet::new(),
    };
    let id = f
        .broker
        .request_approval(f.connection, request.clone(), f.now)
        .unwrap();
    f.broker
        .approve(
            f.connection,
            id,
            NativeApproval::new(request, lifetime).accept_control_handoff(f.b, scope.sources()),
            f.now,
        )
        .unwrap();
}

fn committed_discovery(f: &mut Fixture, run: RunId) -> DisclosureLiveness {
    let request = f
        .broker
        .prepare_request(
            f.connection,
            run,
            Operation::Discover { max_bytes: 64 },
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker.discovery_targets(f.connection, request, f.now),
        Ok(vec![f.a])
    );
    f.broker
        .commit_disclosure(f.connection, request, 10, f.now)
        .unwrap();
    let liveness = f
        .broker
        .disclosure_liveness(f.connection, request, f.now)
        .unwrap();
    f.broker.settle_request(f.connection, request).unwrap();
    f.broker
        .complete(f.connection, run, Completion::Returned, f.now)
        .unwrap();
    f.broker.forget_run(f.connection, run).unwrap();
    liveness
}

#[test]
fn metadata_and_control_require_separate_grants_and_reviewed_closure() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let metadata = metadata_request(&[a]);
    let pending = f
        .broker
        .request_metadata_approval(f.connection, metadata.clone(), f.now)
        .unwrap();
    let other = f.broker.accept_connection().unwrap();
    assert_eq!(
        f.broker.approve_metadata(
            other,
            pending,
            NativeMetadataApproval::new(metadata.clone(), MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(metadata, MAX_GRANT_LIFETIME),
            f.now,
        )
        .unwrap();

    let action = direct_input(b, digest(3), 10);
    let request = GrantRequest {
        scope: input_scope(b),
        bindings: BTreeSet::from([action]),
        direct_read_calls: BTreeSet::new(),
    };
    let pending = f
        .broker
        .request_approval(f.connection, request.clone(), f.now)
        .unwrap();
    assert_eq!(
        f.broker.approve(
            f.connection,
            pending,
            NativeApproval::new(request, MAX_GRANT_LIFETIME)
                .accept_control_handoff(b, BTreeSet::new()),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker.reject_approval(f.connection, pending).unwrap();
    approve_control(&mut f, MAX_GRANT_LIFETIME);

    let control = f.run(input_scope(b), action);
    let lease = f.lease(control, b);
    let deadline = f.now + Duration::from_secs(60);
    let metadata = metadata_run(&mut f, &[a], deadline);
    for operation in [
        Operation::Observe {
            target: a,
            max_bytes: 1,
        },
        input_operation(lease, digest(3), 10),
        Operation::Result { max_bytes: 1 },
    ] {
        assert_eq!(
            f.broker
                .prepare_request(f.connection, metadata, operation, f.now),
            Err(Error::Unauthorized)
        );
    }
    assert!(matches!(
        f.broker.prepare_lease(f.connection, metadata, b, f.now),
        Err(Error::Unauthorized)
    ));
    assert_eq!(
        f.broker.start_workflow(
            f.connection,
            metadata,
            StartKey([9; 16]),
            f.spec(Scope::default(), binding(1)),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.status(f.connection, metadata, f.now),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.admit_run(
            f.connection,
            None,
            f.spec(
                Scope {
                    discover: BTreeSet::from([a]),
                    ..Scope::default()
                },
                direct(DirectKind::Discovery),
            ),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.admit_metadata_run(
            f.connection,
            metadata_spec(&[b], f.now + Duration::from_secs(60)),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    let token = committed_discovery(&mut f, metadata);
    assert!(token.is_live(f.connection, f.now));
    assert!(!token.is_live(other, f.now));
    let request = f.write(control, lease);
    f.broker
        .commit_control(f.connection, request, digest(3), 10, f.now)
        .unwrap();
}

#[test]
fn metadata_approval_requires_explicit_current_control_flow_and_ownership() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let action = direct_input(b, digest(3), 10);
    let request = GrantRequest {
        scope: input_scope(b),
        bindings: BTreeSet::from([action]),
        direct_read_calls: BTreeSet::new(),
    };
    let pending = f
        .broker
        .request_approval(f.connection, request.clone(), f.now)
        .unwrap();
    f.broker
        .approve(
            f.connection,
            pending,
            NativeApproval::new(request, MAX_GRANT_LIFETIME)
                .accept_control_handoff(b, BTreeSet::from([a])),
            f.now,
        )
        .unwrap();
    let proposal = metadata_request(&[a]);
    let pending = f
        .broker
        .request_metadata_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(proposal.clone(), MAX_GRANT_LIFETIME)
                .accept_control_disclosure(b, BTreeSet::from([a])),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker.reject_approval(f.connection, pending).unwrap();
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    let pending = f
        .broker
        .request_metadata_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(proposal.clone(), MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(proposal.clone(), MAX_GRANT_LIFETIME)
                .accept_control_disclosure(b, BTreeSet::from([b])),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    let run = f.run(input_scope(b), action);
    f.lease(run, b);
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(proposal.clone(), MAX_GRANT_LIFETIME)
                .accept_control_disclosure(b, BTreeSet::from([a])),
            f.now,
        ),
        Err(Error::Revoked)
    );
    f.broker.reject_approval(f.connection, pending).unwrap();
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
}

#[test]
fn metadata_closure_requires_every_source_at_every_control_sink() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let c = f.broker.attach_target(3, 1).unwrap();
    let sources = BTreeSet::from([a, b]);
    let scope = Scope {
        discover: sources.clone(),
        control: BTreeSet::from([b, c]),
        ..Scope::default()
    };
    let proposal = GrantRequest {
        scope,
        bindings: BTreeSet::from([direct_input(b, digest(3), 10)]),
        direct_read_calls: BTreeSet::new(),
    };
    let pending = f
        .broker
        .request_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    f.broker
        .approve(
            f.connection,
            pending,
            NativeApproval::new(proposal, MAX_GRANT_LIFETIME)
                .accept_control_handoff(b, sources.clone())
                .accept_control_handoff(c, sources.clone()),
            f.now,
        )
        .unwrap();
    let request = metadata_request(&[a, b]);
    let pending = f
        .broker
        .request_metadata_approval(f.connection, request.clone(), f.now)
        .unwrap();
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(request.clone(), MAX_GRANT_LIFETIME)
                .accept_control_disclosure(b, BTreeSet::from([a]))
                .accept_control_disclosure(c, sources.clone()),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(request.clone(), MAX_GRANT_LIFETIME)
                .accept_control_disclosure(b, sources.clone()),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(request, MAX_GRANT_LIFETIME)
                .accept_control_disclosure(b, sources.clone())
                .accept_control_disclosure(c, sources),
            f.now,
        )
        .unwrap();
    let deadline = f.now + Duration::from_secs(60);
    let run = metadata_run(&mut f, &[a], deadline);
    assert!(committed_discovery(&mut f, run).is_live(f.connection, f.now));
}

#[test]
fn metadata_reapproval_and_takeover_preserve_the_other_grant() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let deadline = f.now + Duration::from_secs(60);
    let action = direct_input(b, digest(3), 10);
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    let control = f.run(input_scope(b), action);
    let lease = f.lease(control, b);
    let input = f.write(control, lease);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    let run = metadata_run(&mut f, &[a], deadline);
    let old_delivery = committed_discovery(&mut f, run);
    assert!(old_delivery.is_live(f.connection, f.now));

    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    assert!(!old_delivery.is_live(f.connection, f.now));
    let run = metadata_run(&mut f, &[a], deadline);
    let current_delivery = committed_discovery(&mut f, run);
    f.broker
        .commit_control(f.connection, input, digest(3), 10, f.now)
        .unwrap();

    f.broker.human_takeover(b).unwrap();
    assert!(current_delivery.is_live(f.connection, f.now));
    assert!(f
        .broker
        .prepare_lease(f.connection, control, b, f.now)
        .is_err());
    let run = metadata_run(&mut f, &[a], deadline);
    assert!(committed_discovery(&mut f, run).is_live(f.connection, f.now));

    approve_control(&mut f, MAX_GRANT_LIFETIME);
    assert!(current_delivery.is_live(f.connection, f.now));
    assert!(f
        .broker
        .prepare_lease(f.connection, control, b, f.now)
        .is_err());
    let fresh = f.run(input_scope(b), action);
    f.lease(fresh, b);
}

#[test]
fn metadata_revocation_cuts_delivery_without_interrupting_pending_control() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    let control = f.run(input_scope(b), direct_input(b, digest(3), 10));
    let lease = f.lease(control, b);
    let input = f.write(control, lease);
    let deadline = f.now + Duration::from_secs(60);
    let metadata = metadata_run(&mut f, &[a], deadline);
    let disclosure = f
        .broker
        .prepare_request(
            f.connection,
            metadata,
            Operation::Discover { max_bytes: 64 },
            f.now,
        )
        .unwrap();
    f.broker
        .commit_disclosure(f.connection, disclosure, 10, f.now)
        .unwrap();
    let delivery = f
        .broker
        .disclosure_liveness(f.connection, disclosure, f.now)
        .unwrap();
    assert!(delivery.is_live(f.connection, f.now));

    f.broker.revoke_metadata(f.connection);
    assert!(!delivery.is_live(f.connection, f.now));
    assert_eq!(
        f.broker
            .commit_disclosure(f.connection, disclosure, 10, f.now),
        Err(Error::Closed)
    );
    f.broker
        .commit_control(f.connection, input, digest(3), 10, f.now)
        .unwrap();
    assert_eq!(
        f.broker
            .commit_control(f.connection, input, digest(3), 10, f.now),
        Err(Error::Closed)
    );
    f.broker.settle_request(f.connection, input).unwrap();
    let status = f
        .broker
        .complete(f.connection, control, Completion::Returned, f.now)
        .unwrap();
    assert_eq!(status.state, RunState::Completed);
    assert_eq!(status.admitted_effects, 1);
    assert!(status.execution_outcome_unknown);
}

#[test]
fn metadata_reapproval_preserves_existing_ordinary_workflow_lineage() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let mut scope = f.scope();
    scope.control.insert(b);
    f.approve(scope.clone(), &[binding(1), binding(2)]);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    let parent = f.run(scope.clone(), binding(1));
    let child = f
        .broker
        .admit_run(f.connection, Some(parent), f.spec(scope, binding(2)), f.now)
        .unwrap();
    let lease = f.lease(child, b);
    let request = f.write(child, lease);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    f.broker
        .commit_control(f.connection, request, digest(3), 10, f.now)
        .unwrap();
    f.broker.settle_request(f.connection, request).unwrap();
    assert_eq!(
        f.broker
            .complete(f.connection, child, Completion::Returned, f.now)
            .unwrap()
            .state,
        RunState::Completed
    );
    assert_eq!(
        f.broker.status(f.connection, parent, f.now).unwrap().state,
        RunState::Running
    );
}

#[test]
fn metadata_delivery_liveness_and_routing_never_fall_back_to_another_grant() {
    let mut f = Fixture::new();
    let a = f.a;
    let scope = Scope {
        discover: BTreeSet::from([a]),
        ..Scope::default()
    };
    let deadline = f.now + Duration::from_secs(60);
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);

    let ordinary_run = f.run(scope.clone(), direct(DirectKind::Discovery));
    let ordinary_delivery = committed_discovery(&mut f, ordinary_run);
    let metadata = metadata_run(&mut f, &[a], deadline);
    let old_metadata_delivery = committed_discovery(&mut f, metadata);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    assert!(ordinary_delivery.is_live(f.connection, f.now));
    assert!(!old_metadata_delivery.is_live(f.connection, f.now));

    let metadata = metadata_run(&mut f, &[a], deadline);
    let metadata_delivery = committed_discovery(&mut f, metadata);
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    assert!(!ordinary_delivery.is_live(f.connection, f.now));
    assert!(metadata_delivery.is_live(f.connection, f.now));

    f.broker.revoke_metadata(f.connection);
    assert!(!metadata_delivery.is_live(f.connection, f.now));
    assert_eq!(
        f.broker
            .admit_metadata_run(f.connection, metadata_spec(&[a], deadline), f.now,),
        Err(Error::Revoked)
    );
    let ordinary_run = f.run(scope, direct(DirectKind::Discovery));
    assert!(committed_discovery(&mut f, ordinary_run).is_live(f.connection, f.now));

    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    let metadata = metadata_run(&mut f, &[a], deadline);
    let request = f
        .broker
        .prepare_request(
            f.connection,
            metadata,
            Operation::Discover { max_bytes: 64 },
            f.now,
        )
        .unwrap();
    assert!(matches!(
        f.broker.disclosure_liveness(f.connection, request, f.now),
        Err(Error::InvalidRequest)
    ));
    f.broker
        .commit_disclosure(f.connection, request, 10, f.now)
        .unwrap();
    let delivery = f
        .broker
        .disclosure_liveness(f.connection, request, f.now)
        .unwrap();
    f.broker.cancel_run(f.connection, metadata, f.now).unwrap();
    assert!(!delivery.is_live(f.connection, f.now));
}

#[test]
fn disclosure_liveness_tracks_forgotten_workflow_ancestors() {
    for cancel in [true, false] {
        let mut f = Fixture::new();
        let scope = f.scope();
        f.approve(scope.clone(), &[binding(1), binding(2)]);
        let parent = f.run(scope.clone(), binding(1));
        let parent_request = f
            .broker
            .prepare_request(
                f.connection,
                parent,
                Operation::Discover { max_bytes: 64 },
                f.now,
            )
            .unwrap();
        f.broker
            .commit_disclosure(f.connection, parent_request, 10, f.now)
            .unwrap();
        let parent_delivery = f
            .broker
            .disclosure_liveness(f.connection, parent_request, f.now)
            .unwrap();
        f.broker
            .settle_request(f.connection, parent_request)
            .unwrap();
        let child = f
            .broker
            .admit_run(f.connection, Some(parent), f.spec(scope, binding(2)), f.now)
            .unwrap();
        let delivery = committed_discovery(&mut f, child);
        assert!(delivery.is_live(f.connection, f.now));

        if cancel {
            f.broker.cancel_run(f.connection, parent, f.now).unwrap();
        } else {
            f.broker
                .complete(f.connection, parent, Completion::Returned, f.now)
                .unwrap();
        }
        assert!(!delivery.is_live(f.connection, f.now));
        assert_eq!(parent_delivery.is_live(f.connection, f.now), !cancel);
    }
}

#[test]
fn metadata_and_ordinary_approval_snapshots_reject_slot_aba() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let metadata = metadata_request(&[a]);
    let pending = f
        .broker
        .request_metadata_approval(f.connection, metadata.clone(), f.now)
        .unwrap();
    let action = direct_input(b, digest(3), 10);
    f.approve(input_scope(b), &[action]);
    f.broker.human_takeover(b).unwrap();
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(metadata, MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );

    let mut f = Fixture::new();
    let a = f.a;
    let request = GrantRequest {
        scope: Scope {
            discover: BTreeSet::from([a]),
            ..Scope::default()
        },
        bindings: BTreeSet::from([direct(DirectKind::Discovery)]),
        direct_read_calls: BTreeSet::new(),
    };
    let pending = f
        .broker
        .request_approval(f.connection, request.clone(), f.now)
        .unwrap();
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            pending,
            NativeMetadataApproval::new(metadata_request(&[a]), MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    f.broker.revoke_metadata(f.connection);
    assert_eq!(
        f.broker.approve(
            f.connection,
            pending,
            NativeApproval::new(request, MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
}

#[test]
fn slot_revision_cuts_reclaim_principal_pending_capacity_without_refunds() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let proposal = metadata_request(&[a]);
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    let stale: Vec<_> = (0..2)
        .map(|_| {
            f.broker
                .request_metadata_approval(f.connection, proposal.clone(), f.now)
                .unwrap()
        })
        .collect();
    assert_eq!(
        f.broker
            .request_metadata_approval(f.connection, proposal.clone(), f.now),
        Err(Error::QuotaExceeded)
    );

    f.broker.human_takeover(b).unwrap();
    let fresh: Vec<_> = (0..2)
        .map(|_| {
            f.broker
                .request_metadata_approval(f.connection, proposal.clone(), f.now)
                .unwrap()
        })
        .collect();
    for id in &stale {
        assert_eq!(
            f.broker.approve_metadata(
                f.connection,
                *id,
                NativeMetadataApproval::new(proposal.clone(), MAX_GRANT_LIFETIME),
                f.now,
            ),
            Err(Error::Unauthorized)
        );
    }
    for id in fresh {
        f.broker.reject_approval(f.connection, id).unwrap();
    }

    approve_control(&mut f, MAX_GRANT_LIFETIME);
    for id in stale {
        assert_eq!(
            f.broker.approve_metadata(
                f.connection,
                id,
                NativeMetadataApproval::new(proposal.clone(), MAX_GRANT_LIFETIME)
                    .accept_control_disclosure(b, BTreeSet::from([a])),
                f.now,
            ),
            Err(Error::Unauthorized)
        );
    }
    let eighth = f
        .broker
        .request_metadata_approval(f.connection, proposal.clone(), f.now)
        .unwrap();
    f.broker.reject_approval(f.connection, eighth).unwrap();
    assert_eq!(
        f.broker
            .request_metadata_approval(f.connection, proposal, f.now),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn slot_revision_cuts_reclaim_global_pending_capacity_for_other_principals() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let proposal = metadata_request(&[a]);
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    for _ in 0..2 {
        f.broker
            .request_metadata_approval(f.connection, proposal.clone(), f.now)
            .unwrap();
    }
    let mut unaffected = None;
    for _ in 0..7 {
        let connection = f.broker.accept_connection().unwrap();
        let id = f
            .broker
            .request_metadata_approval(connection, proposal.clone(), f.now)
            .unwrap();
        unaffected.get_or_insert((connection, id));
        f.broker
            .request_metadata_approval(connection, proposal.clone(), f.now)
            .unwrap();
    }
    let first = f.broker.accept_connection().unwrap();
    assert_eq!(
        f.broker
            .request_metadata_approval(first, proposal.clone(), f.now),
        Err(Error::QuotaExceeded)
    );
    f.broker.human_takeover(b).unwrap();
    let first_pending = f
        .broker
        .request_metadata_approval(first, proposal.clone(), f.now)
        .unwrap();
    let second = f.broker.accept_connection().unwrap();
    f.broker
        .request_metadata_approval(second, proposal.clone(), f.now)
        .unwrap();
    let third = f.broker.accept_connection().unwrap();
    assert_eq!(
        f.broker
            .request_metadata_approval(third, proposal.clone(), f.now),
        Err(Error::QuotaExceeded)
    );

    let (owner, pending) = unaffected.unwrap();
    f.broker
        .approve_metadata(
            owner,
            pending,
            NativeMetadataApproval::new(proposal.clone(), MAX_GRANT_LIFETIME),
            f.now,
        )
        .unwrap();
    f.broker
        .request_metadata_approval(third, proposal.clone(), f.now)
        .unwrap();
    f.broker
        .approve_metadata(
            first,
            first_pending,
            NativeMetadataApproval::new(proposal, MAX_GRANT_LIFETIME),
            f.now,
        )
        .unwrap();
}

#[test]
fn slot_installations_reclaim_opposite_slot_proposals() {
    for install_ordinary in [true, false] {
        let mut f = Fixture::new();
        let a = f.a;
        let b = f.b;
        let metadata = metadata_request(&[a]);
        let ordinary = GrantRequest {
            scope: shared_control_scope(&f),
            bindings: BTreeSet::from([direct_input(b, digest(3), 10)]),
            direct_read_calls: BTreeSet::new(),
        };
        if install_ordinary {
            let stale = f
                .broker
                .request_metadata_approval(f.connection, metadata.clone(), f.now)
                .unwrap();
            approve_control(&mut f, MAX_GRANT_LIFETIME);
            for _ in 0..2 {
                f.broker
                    .request_metadata_approval(f.connection, metadata.clone(), f.now)
                    .unwrap();
            }
            assert_eq!(
                f.broker.approve_metadata(
                    f.connection,
                    stale,
                    NativeMetadataApproval::new(metadata, MAX_GRANT_LIFETIME)
                        .accept_control_disclosure(b, BTreeSet::from([a])),
                    f.now,
                ),
                Err(Error::Unauthorized)
            );
        } else {
            let stale = f
                .broker
                .request_approval(f.connection, ordinary.clone(), f.now)
                .unwrap();
            approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
            for _ in 0..2 {
                f.broker
                    .request_approval(f.connection, ordinary.clone(), f.now)
                    .unwrap();
            }
            assert_eq!(
                f.broker.approve(
                    f.connection,
                    stale,
                    NativeApproval::new(ordinary, MAX_GRANT_LIFETIME)
                        .accept_control_handoff(b, BTreeSet::from([a])),
                    f.now,
                ),
                Err(Error::Unauthorized)
            );
        }
    }
}

#[test]
fn slot_expiry_reclaims_stale_opposite_slot_proposals() {
    for metadata_expires_first in [false, true] {
        let mut f = Fixture::new();
        let a = f.a;
        let b = f.b;
        let short = Duration::from_secs(10);
        let long = Duration::from_secs(120);
        approve_control(&mut f, if metadata_expires_first { long } else { short });
        approve_metadata(
            &mut f,
            &[a],
            &[b],
            if metadata_expires_first { short } else { long },
        );
        let metadata = metadata_request(&[a]);
        let ordinary = GrantRequest {
            scope: shared_control_scope(&f),
            bindings: BTreeSet::from([direct_input(b, digest(3), 10)]),
            direct_read_calls: BTreeSet::new(),
        };
        let stale: Vec<_> = (0..2)
            .map(|_| {
                if metadata_expires_first {
                    f.broker
                        .request_approval(f.connection, ordinary.clone(), f.now)
                } else {
                    f.broker
                        .request_metadata_approval(f.connection, metadata.clone(), f.now)
                }
                .unwrap()
            })
            .collect();
        f.now += short;
        f.broker.poll_expiry(f.now);
        for _ in 0..2 {
            if metadata_expires_first {
                f.broker
                    .request_approval(f.connection, ordinary.clone(), f.now)
            } else {
                f.broker
                    .request_metadata_approval(f.connection, metadata.clone(), f.now)
            }
            .unwrap();
        }
        for id in stale {
            let result = if metadata_expires_first {
                f.broker.approve(
                    f.connection,
                    id,
                    NativeApproval::new(ordinary.clone(), MAX_GRANT_LIFETIME)
                        .accept_control_handoff(b, BTreeSet::from([a])),
                    f.now,
                )
            } else {
                f.broker.approve_metadata(
                    f.connection,
                    id,
                    NativeMetadataApproval::new(metadata.clone(), MAX_GRANT_LIFETIME),
                    f.now,
                )
            };
            assert_eq!(result, Err(Error::Unauthorized));
        }
    }
}

#[test]
fn reclaimed_stale_approvals_do_not_refund_global_lifetime_limit() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let proposal = metadata_request(&[a]);
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    for _ in 0..2 {
        f.broker
            .request_metadata_approval(f.connection, proposal.clone(), f.now)
            .unwrap();
    }
    f.broker.human_takeover(b).unwrap();
    for _ in 0..4 {
        let id = f
            .broker
            .request_metadata_approval(f.connection, proposal.clone(), f.now)
            .unwrap();
        f.broker.reject_approval(f.connection, id).unwrap();
    }
    let mut connections = Vec::new();
    for _ in 0..15 {
        let connection = f.broker.accept_connection().unwrap();
        connections.push(connection);
        for _ in 0..8 {
            let id = f
                .broker
                .request_metadata_approval(connection, proposal.clone(), f.now)
                .unwrap();
            f.broker.reject_approval(connection, id).unwrap();
        }
    }
    f.broker.disconnect(connections[0]);
    let new = f.broker.accept_connection().unwrap();
    assert_eq!(
        f.broker.request_metadata_approval(new, proposal, f.now),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn empty_slot_revocation_preserves_unaffected_opposite_proposals() {
    let mut f = Fixture::new();
    let a = f.a;
    let metadata = metadata_request(&[a]);
    let ordinary = GrantRequest {
        scope: Scope {
            discover: BTreeSet::from([a]),
            ..Scope::default()
        },
        bindings: BTreeSet::from([direct(DirectKind::Discovery)]),
        direct_read_calls: BTreeSet::new(),
    };
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    let metadata_pending = f
        .broker
        .request_metadata_approval(f.connection, metadata.clone(), f.now)
        .unwrap();
    let ordinary_pending = f
        .broker
        .request_approval(f.connection, ordinary.clone(), f.now)
        .unwrap();
    f.broker.revoke_ordinary(f.connection);
    assert_eq!(
        f.broker.approve(
            f.connection,
            ordinary_pending,
            NativeApproval::new(ordinary.clone(), MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .approve_metadata(
            f.connection,
            metadata_pending,
            NativeMetadataApproval::new(metadata.clone(), MAX_GRANT_LIFETIME),
            f.now,
        )
        .unwrap();

    let mut f = Fixture::new();
    let a = f.a;
    let metadata = metadata_request(&[a]);
    let ordinary = GrantRequest {
        scope: Scope {
            discover: BTreeSet::from([a]),
            ..Scope::default()
        },
        bindings: BTreeSet::from([direct(DirectKind::Discovery)]),
        direct_read_calls: BTreeSet::new(),
    };
    f.approve(ordinary.scope.clone(), &[direct(DirectKind::Discovery)]);
    let ordinary_pending = f
        .broker
        .request_approval(f.connection, ordinary.clone(), f.now)
        .unwrap();
    let metadata_pending = f
        .broker
        .request_metadata_approval(f.connection, metadata.clone(), f.now)
        .unwrap();
    f.broker.revoke_metadata(f.connection);
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            metadata_pending,
            NativeMetadataApproval::new(metadata, MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .approve(
            f.connection,
            ordinary_pending,
            NativeApproval::new(ordinary, MAX_GRANT_LIFETIME),
            f.now,
        )
        .unwrap();
}

#[test]
fn slot_revocation_isolates_observation_metadata_and_pending_delivery() {
    let mut f = Fixture::new();
    let a = f.a;
    let other = f.broker.accept_connection().unwrap();
    let deadline = f.now + Duration::from_secs(60);
    let scope = Scope {
        observe: BTreeSet::from([a]),
        ..Scope::default()
    };
    let binding = direct(DirectKind::Observation);
    let ordinary = GrantRequest {
        scope: scope.clone(),
        bindings: BTreeSet::from([binding]),
        direct_read_calls: BTreeSet::new(),
    };
    let metadata = metadata_request(&[a]);
    f.approve(scope.clone(), &[binding]);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);

    let observation = f.run(scope.clone(), binding);
    let observe = Operation::Observe {
        target: a,
        max_bytes: 64,
    };
    let committed = f
        .broker
        .prepare_request(f.connection, observation, observe, f.now)
        .unwrap();
    f.broker
        .commit_disclosure(f.connection, committed, 10, f.now)
        .unwrap();
    let ordinary_delivery = f
        .broker
        .disclosure_liveness(f.connection, committed, f.now)
        .unwrap();
    f.broker.settle_request(f.connection, committed).unwrap();
    let ordinary_pending = f
        .broker
        .prepare_request(f.connection, observation, observe, f.now)
        .unwrap();

    let metadata_context = metadata_run(&mut f, &[a], deadline);
    let discover = Operation::Discover { max_bytes: 64 };
    let committed = f
        .broker
        .prepare_request(f.connection, metadata_context, discover, f.now)
        .unwrap();
    f.broker
        .commit_disclosure(f.connection, committed, 10, f.now)
        .unwrap();
    let metadata_delivery = f
        .broker
        .disclosure_liveness(f.connection, committed, f.now)
        .unwrap();
    f.broker.settle_request(f.connection, committed).unwrap();
    let metadata_pending = f
        .broker
        .prepare_request(f.connection, metadata_context, discover, f.now)
        .unwrap();
    let metadata_approval = f
        .broker
        .request_metadata_approval(f.connection, metadata.clone(), f.now)
        .unwrap();
    let ordinary_approval = f
        .broker
        .request_approval(f.connection, ordinary.clone(), f.now)
        .unwrap();

    f.broker.revoke_ordinary(other);
    assert!(ordinary_delivery.is_live(f.connection, f.now));
    assert!(metadata_delivery.is_live(f.connection, f.now));
    f.broker.revoke_ordinary(f.connection);
    assert!(!ordinary_delivery.is_live(f.connection, f.now));
    assert!(metadata_delivery.is_live(f.connection, f.now));
    assert_eq!(
        f.broker
            .commit_disclosure(f.connection, ordinary_pending, 10, f.now),
        Err(Error::Closed)
    );
    assert_eq!(
        f.broker.approve(
            f.connection,
            ordinary_approval,
            NativeApproval::new(ordinary, MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.approve_metadata(
            f.connection,
            metadata_approval,
            NativeMetadataApproval::new(metadata, MAX_GRANT_LIFETIME),
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    f.broker
        .commit_disclosure(f.connection, metadata_pending, 10, f.now)
        .unwrap();
    let current_metadata_delivery = f
        .broker
        .disclosure_liveness(f.connection, metadata_pending, f.now)
        .unwrap();
    f.broker
        .settle_request(f.connection, metadata_pending)
        .unwrap();
    let metadata_late = f
        .broker
        .prepare_request(f.connection, metadata_context, discover, f.now)
        .unwrap();

    f.approve(scope.clone(), &[binding]);
    assert_eq!(
        f.broker
            .prepare_request(f.connection, observation, observe, f.now),
        Err(Error::Revoked)
    );
    let new_observation = f.run(scope, binding);
    let committed = f
        .broker
        .prepare_request(f.connection, new_observation, observe, f.now)
        .unwrap();
    f.broker
        .commit_disclosure(f.connection, committed, 10, f.now)
        .unwrap();
    let new_ordinary_delivery = f
        .broker
        .disclosure_liveness(f.connection, committed, f.now)
        .unwrap();
    f.broker.settle_request(f.connection, committed).unwrap();
    let ordinary_late = f
        .broker
        .prepare_request(f.connection, new_observation, observe, f.now)
        .unwrap();

    f.broker.revoke_metadata(f.connection);
    assert!(!metadata_delivery.is_live(f.connection, f.now));
    assert!(!current_metadata_delivery.is_live(f.connection, f.now));
    assert!(new_ordinary_delivery.is_live(f.connection, f.now));
    assert_eq!(
        f.broker
            .commit_disclosure(f.connection, metadata_late, 10, f.now),
        Err(Error::Closed)
    );
    f.broker
        .commit_disclosure(f.connection, ordinary_late, 10, f.now)
        .unwrap();
    f.broker
        .settle_request(f.connection, ordinary_late)
        .unwrap();
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    let metadata = metadata_run(&mut f, &[a], deadline);
    let final_metadata_delivery = committed_discovery(&mut f, metadata);
    f.broker.revoke_all();
    assert!(!new_ordinary_delivery.is_live(f.connection, f.now));
    assert!(!final_metadata_delivery.is_live(f.connection, f.now));
}

#[test]
fn ordinary_control_revocation_preserves_metadata_and_unknown_outcomes() {
    for admitted in [false, true] {
        let mut f = Fixture::new();
        let a = f.a;
        let b = f.b;
        let deadline = f.now + Duration::from_secs(60);
        let binding = direct_input(b, digest(3), 10);
        let mut scope = shared_control_scope(&f);
        scope.return_results = true;
        f.approve(scope.clone(), &[binding]);
        approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
        let control = f.run(input_scope(b), binding);
        let lease = f.lease(control, b);
        let input = f.write(control, lease);
        if admitted {
            f.broker
                .commit_control(f.connection, input, digest(3), 10, f.now)
                .unwrap();
            let status = f.broker.status(f.connection, control, f.now).unwrap();
            assert_eq!(status.admitted_effects, 1);
            assert!(status.execution_outcome_unknown);
        }

        let metadata = metadata_run(&mut f, &[a], deadline);
        let delivery = committed_discovery(&mut f, metadata);
        let metadata = metadata_run(&mut f, &[a], deadline);
        let pending = f
            .broker
            .prepare_request(
                f.connection,
                metadata,
                Operation::Discover { max_bytes: 64 },
                f.now,
            )
            .unwrap();
        f.broker.revoke_ordinary(f.connection);
        assert!(delivery.is_live(f.connection, f.now));
        assert_eq!(
            f.broker
                .commit_control(f.connection, input, digest(3), 10, f.now),
            Err(Error::Closed)
        );
        assert!(matches!(
            f.broker.prepare_lease(f.connection, control, b, f.now),
            Err(Error::Revoked)
        ));
        f.broker
            .commit_disclosure(f.connection, pending, 10, f.now)
            .unwrap();

        f.approve(scope, &[binding]);
        assert!(delivery.is_live(f.connection, f.now));
        assert!(matches!(
            f.broker.prepare_lease(f.connection, control, b, f.now),
            Err(Error::Revoked)
        ));
        let fresh = f.run(input_scope(b), binding);
        let fresh_lease = f.lease(fresh, b);
        assert_ne!(fresh_lease.ownership_epoch(), lease.ownership_epoch());
    }
}

#[test]
fn metadata_and_control_expire_independently_at_their_exact_deadlines() {
    for metadata_expires_first in [false, true] {
        let mut f = Fixture::new();
        let a = f.a;
        let b = f.b;
        let short = Duration::from_secs(10);
        let long = Duration::from_secs(120);
        let control_lifetime = if metadata_expires_first { long } else { short };
        let metadata_lifetime = if metadata_expires_first { short } else { long };
        approve_control(&mut f, control_lifetime);
        approve_metadata(&mut f, &[a], &[b], metadata_lifetime);
        let action = direct_input(b, digest(3), 10);
        let mut spec = f.spec(input_scope(b), action);
        spec.deadline = f.now + control_lifetime.min(Duration::from_secs(60));
        let control = f.broker.admit_run(f.connection, None, spec, f.now).unwrap();
        let deadline = f.now + metadata_lifetime.min(Duration::from_secs(60));
        let run = metadata_run(&mut f, &[a], deadline);
        let delivery = committed_discovery(&mut f, run);
        let expiry = f.now + short;
        assert_eq!(
            delivery.is_live(f.connection, expiry),
            !metadata_expires_first
        );
        f.broker.poll_expiry(expiry);
        if metadata_expires_first {
            assert!(f
                .broker
                .prepare_lease(f.connection, control, b, expiry)
                .is_ok());
            assert_eq!(
                f.broker.admit_metadata_run(
                    f.connection,
                    metadata_spec(&[a], expiry + short),
                    expiry,
                ),
                Err(Error::Revoked)
            );
        } else {
            assert!(f
                .broker
                .prepare_lease(f.connection, control, b, expiry)
                .is_err());
            assert!(f
                .broker
                .admit_metadata_run(f.connection, metadata_spec(&[a], expiry + short), expiry,)
                .is_ok());
        }
    }
}

#[test]
fn metadata_lifecycle_cuts_affected_generations_privacy_and_connections() {
    for event in 0..5 {
        let mut f = Fixture::new();
        let a = f.a;
        let b = f.b;
        let deadline = f.now + Duration::from_secs(60);
        approve_control(&mut f, MAX_GRANT_LIFETIME);
        approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
        let control = f.run(input_scope(b), direct_input(b, digest(3), 10));
        let run = metadata_run(&mut f, &[a], deadline);
        let delivery = committed_discovery(&mut f, run);
        match event {
            0 => {
                let replacement = f.broker.attach_target(a.session(), 2).unwrap();
                assert_ne!(replacement, a);
                assert!(!delivery.is_live(f.connection, f.now));
                assert_eq!(
                    f.broker
                        .request_metadata_approval(f.connection, metadata_request(&[a]), f.now,),
                    Err(Error::StaleTarget)
                );
                assert!(f
                    .broker
                    .prepare_lease(f.connection, control, b, f.now)
                    .is_err());
            }
            1 => {
                f.broker.attach_target(b.session(), 2).unwrap();
                assert!(delivery.is_live(f.connection, f.now));
                assert!(f
                    .broker
                    .prepare_lease(f.connection, control, b, f.now)
                    .is_err());
                let run = metadata_run(&mut f, &[a], deadline);
                assert!(committed_discovery(&mut f, run).is_live(f.connection, f.now));
            }
            2 => {
                f.broker.revoke_all();
                assert!(!delivery.is_live(f.connection, f.now));
            }
            3 => {
                f.broker.revoke(f.connection);
                assert!(!delivery.is_live(f.connection, f.now));
            }
            _ => {
                let old = f.connection;
                f.broker.disconnect(old);
                let new = f.broker.accept_connection().unwrap();
                assert_ne!(old, new);
                assert!(!delivery.is_live(old, f.now));
                assert!(!delivery.is_live(new, f.now));
                assert_eq!(
                    f.broker
                        .admit_metadata_run(new, metadata_spec(&[a], deadline), f.now),
                    Err(Error::Revoked)
                );
            }
        }
    }

    let mut f = Fixture::new();
    let a = f.a;
    let deadline = f.now + Duration::from_secs(60);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    let run = metadata_run(&mut f, &[a], deadline);
    let delivery = committed_discovery(&mut f, run);
    drop(f.broker);
    assert!(!delivery.is_live(f.connection, f.now));

    let forged = serde_json::json!({"targets": [a], "control": [a]});
    assert!(serde_json::from_value::<MetadataGrantRequest>(forged).is_err());
}

#[test]
fn metadata_takeover_and_principal_revocation_remain_isolated() {
    let mut f = Fixture::new();
    let first = f.connection;
    let second = f.broker.accept_connection().unwrap();
    let a = f.a;
    let b = f.b;
    let deadline = f.now + Duration::from_secs(60);
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    let first_run = metadata_run(&mut f, &[a], deadline);
    let first_delivery = committed_discovery(&mut f, first_run);

    f.connection = second;
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    let second_run = metadata_run(&mut f, &[a], deadline);
    assert_eq!(
        f.broker.prepare_request(
            first,
            second_run,
            Operation::Discover { max_bytes: 1 },
            f.now,
        ),
        Err(Error::Unauthorized)
    );
    let second_delivery = committed_discovery(&mut f, second_run);
    f.broker.human_takeover(b).unwrap();
    assert!(first_delivery.is_live(first, f.now));
    assert!(second_delivery.is_live(second, f.now));

    f.broker.revoke(first);
    assert!(!first_delivery.is_live(first, f.now));
    assert!(second_delivery.is_live(second, f.now));
    let run = metadata_run(&mut f, &[a], deadline);
    assert!(committed_discovery(&mut f, run).is_live(second, f.now));
    f.broker.revoke_all();
    assert!(!second_delivery.is_live(second, f.now));
}

#[test]
fn metadata_and_ordinary_reapproval_cannot_replenish_each_other() {
    let mut f = Fixture::new();
    let a = f.a;
    let b = f.b;
    let deadline = f.now + Duration::from_secs(60);
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    approve_metadata(&mut f, &[a], &[b], MAX_GRANT_LIFETIME);
    for _ in 0..16 {
        let run = metadata_run(&mut f, &[a], deadline);
        let request = f
            .broker
            .prepare_request(
                f.connection,
                run,
                Operation::Discover {
                    max_bytes: MAX_PAYLOAD_BYTES,
                },
                f.now,
            )
            .unwrap();
        f.broker.settle_request(f.connection, request).unwrap();
        f.broker
            .complete(f.connection, run, Completion::Returned, f.now)
            .unwrap();
        f.broker.forget_run(f.connection, run).unwrap();
    }
    approve_control(&mut f, MAX_GRANT_LIFETIME);
    let run = metadata_run(&mut f, &[a], deadline);
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            run,
            Operation::Discover { max_bytes: 1 },
            f.now
        ),
        Err(Error::QuotaExceeded)
    );

    let mut f = Fixture::new();
    let a = f.a;
    let scope = Scope {
        discover: BTreeSet::from([a]),
        ..Scope::default()
    };
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    for _ in 0..16 {
        let run = f.run(scope.clone(), direct(DirectKind::Discovery));
        let request = f
            .broker
            .prepare_request(
                f.connection,
                run,
                Operation::Discover {
                    max_bytes: MAX_PAYLOAD_BYTES,
                },
                f.now,
            )
            .unwrap();
        f.broker.settle_request(f.connection, request).unwrap();
        f.broker
            .complete(f.connection, run, Completion::Returned, f.now)
            .unwrap();
        f.broker.forget_run(f.connection, run).unwrap();
    }
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    let run = f.run(scope.clone(), direct(DirectKind::Discovery));
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            run,
            Operation::Discover { max_bytes: 1 },
            f.now
        ),
        Err(Error::QuotaExceeded)
    );
    f.broker.revoke_metadata(f.connection);
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    let run = f.run(scope, direct(DirectKind::Discovery));
    assert!(f
        .broker
        .prepare_request(
            f.connection,
            run,
            Operation::Discover { max_bytes: 1 },
            f.now
        )
        .is_ok());

    let mut f = Fixture::new();
    let a = f.a;
    let deadline = f.now + Duration::from_secs(60);
    let scope = Scope {
        discover: BTreeSet::from([a]),
        ..Scope::default()
    };
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    for _ in 0..128 {
        let ordinary = f.run(scope.clone(), direct(DirectKind::Discovery));
        let metadata = metadata_run(&mut f, &[a], deadline);
        for run in [ordinary, metadata] {
            f.broker
                .complete(f.connection, run, Completion::Returned, f.now)
                .unwrap();
            f.broker.forget_run(f.connection, run).unwrap();
        }
    }
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    assert_eq!(
        f.broker.admit_run(
            f.connection,
            None,
            f.spec(scope, direct(DirectKind::Discovery)),
            f.now,
        ),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn metadata_shares_pending_and_direct_context_limits() {
    let mut f = Fixture::new();
    let a = f.a;
    let ordinary = GrantRequest {
        scope: Scope {
            discover: BTreeSet::from([a]),
            ..Scope::default()
        },
        bindings: BTreeSet::from([direct(DirectKind::Discovery)]),
        direct_read_calls: BTreeSet::new(),
    };
    let first = f
        .broker
        .request_approval(f.connection, ordinary.clone(), f.now)
        .unwrap();
    let second = f
        .broker
        .request_metadata_approval(f.connection, metadata_request(&[a]), f.now)
        .unwrap();
    assert_eq!(
        f.broker
            .request_metadata_approval(f.connection, metadata_request(&[a]), f.now),
        Err(Error::QuotaExceeded)
    );
    f.broker.reject_approval(f.connection, first).unwrap();
    f.broker.reject_approval(f.connection, second).unwrap();
    for _ in 0..6 {
        let id = f
            .broker
            .request_metadata_approval(f.connection, metadata_request(&[a]), f.now)
            .unwrap();
        f.broker.reject_approval(f.connection, id).unwrap();
    }
    assert_eq!(
        f.broker.request_approval(f.connection, ordinary, f.now),
        Err(Error::QuotaExceeded)
    );

    let mut f = Fixture::new();
    let a = f.a;
    let mut connections = vec![f.connection];
    for _ in 0..7 {
        connections.push(f.broker.accept_connection().unwrap());
    }
    for connection in connections {
        for _ in 0..2 {
            f.broker
                .request_metadata_approval(connection, metadata_request(&[a]), f.now)
                .unwrap();
        }
    }
    let ninth = f.broker.accept_connection().unwrap();
    assert_eq!(
        f.broker
            .request_metadata_approval(ninth, metadata_request(&[a]), f.now),
        Err(Error::QuotaExceeded)
    );

    let mut f = Fixture::new();
    let a = f.a;
    let deadline = f.now + Duration::from_secs(60);
    let scope = Scope {
        discover: BTreeSet::from([a]),
        ..Scope::default()
    };
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    f.run(scope.clone(), direct(DirectKind::Discovery));
    f.run(scope, direct(DirectKind::Discovery));
    metadata_run(&mut f, &[a], deadline);
    metadata_run(&mut f, &[a], deadline);
    assert_eq!(
        f.broker
            .admit_metadata_run(f.connection, metadata_spec(&[a], deadline), f.now),
        Err(Error::QuotaExceeded)
    );

    let mut f = Fixture::new();
    let a = f.a;
    let mut connections = vec![f.connection];
    for _ in 0..15 {
        connections.push(f.broker.accept_connection().unwrap());
    }
    for connection in &connections {
        for _ in 0..8 {
            let id = f
                .broker
                .request_metadata_approval(*connection, metadata_request(&[a]), f.now)
                .unwrap();
            f.broker.reject_approval(*connection, id).unwrap();
        }
    }
    f.broker.disconnect(connections[0]);
    let new = f.broker.accept_connection().unwrap();
    assert_eq!(
        f.broker
            .request_metadata_approval(new, metadata_request(&[a]), f.now),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn metadata_and_ordinary_work_share_global_lifetime_quotas() {
    let limits = Limits {
        global_operations: 4,
        ..Limits::default()
    };
    let mut f = Fixture::with_limits(limits);
    let a = f.a;
    let deadline = f.now + Duration::from_secs(60);
    let scope = Scope {
        discover: BTreeSet::from([a]),
        ..Scope::default()
    };
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    let metadata = metadata_run(&mut f, &[a], deadline);
    committed_discovery(&mut f, metadata);
    let ordinary = f.run(scope.clone(), direct(DirectKind::Discovery));
    committed_discovery(&mut f, ordinary);
    f.approve(scope, &[direct(DirectKind::Discovery)]);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    assert_eq!(
        f.broker
            .admit_metadata_run(f.connection, metadata_spec(&[a], deadline), f.now),
        Err(Error::QuotaExceeded)
    );

    let limits = Limits {
        global_disclosed_bytes: 64,
        ..Limits::default()
    };
    let mut f = Fixture::with_limits(limits);
    let a = f.a;
    let deadline = f.now + Duration::from_secs(60);
    let scope = Scope {
        discover: BTreeSet::from([a]),
        ..Scope::default()
    };
    f.approve(scope.clone(), &[direct(DirectKind::Discovery)]);
    approve_metadata(&mut f, &[a], &[], MAX_GRANT_LIFETIME);
    let metadata = metadata_run(&mut f, &[a], deadline);
    committed_discovery(&mut f, metadata);
    let ordinary = f.run(scope, direct(DirectKind::Discovery));
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            ordinary,
            Operation::Discover { max_bytes: 1 },
            f.now,
        ),
        Err(Error::QuotaExceeded)
    );
}
