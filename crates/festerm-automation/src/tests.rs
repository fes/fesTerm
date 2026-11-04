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

fn enroll_controller(
    broker: &mut Broker,
    connection: ConnectionId,
    registration: ControllerRegistrationId,
    target: TargetRef,
) -> ControllerEnrollmentId {
    let proposal = broker
        .prepare_controller_enrollment(connection, registration, target)
        .unwrap();
    broker
        .approve_controller_enrollment(connection, NativeControllerEnrollment::new(proposal))
        .unwrap()
}

#[test]
fn controller_enrollment_uses_exact_native_selection_not_labels_or_position() {
    let mut f = Fixture::new();
    let first = f
        .broker
        .register_controller_session(f.connection, b"local-agent-1")
        .unwrap();
    let second = f
        .broker
        .register_controller_session(f.connection, b"local-agent-2")
        .unwrap();
    let mut displayed = [("same label", f.a), ("same label", f.b)];
    let proposal = f
        .broker
        .prepare_controller_enrollment(f.connection, first, displayed[1].1)
        .unwrap();
    displayed.swap(0, 1);
    let selected = f
        .broker
        .approve_controller_enrollment(f.connection, NativeControllerEnrollment::new(proposal))
        .unwrap();
    let other = enroll_controller(&mut f.broker, f.connection, second, displayed[1].1);

    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, first, selected),
        Ok(f.b)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, second, other),
        Ok(f.a)
    );
    assert_eq!(
        f.broker
            .register_controller_session(f.connection, b"local-agent-1"),
        Ok(first)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, first, selected),
        Ok(f.b)
    );
}

#[test]
fn controller_registrations_are_connection_owned_and_disconnect_clears_them() {
    let mut f = Fixture::new();
    let other = f.broker.accept_connection().unwrap();
    let first = f
        .broker
        .register_controller_session(f.connection, b"same-local-id")
        .unwrap();
    let second = f
        .broker
        .register_controller_session(other, b"same-local-id")
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        f.broker.controller_registration(f.connection, first),
        Ok(&b"same-local-id"[..])
    );
    assert_eq!(
        f.broker.controller_registration(other, first),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .prepare_controller_enrollment(other, first, f.a)
            .unwrap_err(),
        Error::Unauthorized
    );
    let proposal = f
        .broker
        .prepare_controller_enrollment(f.connection, first, f.a)
        .unwrap();
    assert_eq!(
        f.broker
            .approve_controller_enrollment(other, NativeControllerEnrollment::new(proposal)),
        Err(Error::Unauthorized)
    );
    let first_binding = enroll_controller(&mut f.broker, f.connection, first, f.a);
    let second_binding = enroll_controller(&mut f.broker, other, second, f.b);
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(other, first, first_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(other, second, first_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.unregister_controller_session(other, first),
        Err(Error::Unauthorized)
    );

    let before_disconnect = f
        .broker
        .prepare_controller_enrollment(f.connection, first, f.b)
        .unwrap();
    f.broker.disconnect(f.connection);
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, first, first_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(other, second, second_binding),
        Ok(f.b)
    );
    let reconnected = f.broker.accept_connection().unwrap();
    let new_registration = f
        .broker
        .register_controller_session(reconnected, b"same-local-id")
        .unwrap();
    assert_ne!(new_registration, first);
    assert_eq!(
        f.broker.approve_controller_enrollment(
            reconnected,
            NativeControllerEnrollment::new(before_disconnect)
        ),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(reconnected, new_registration, first_binding),
        Err(Error::Unauthorized)
    );
}

#[test]
fn controller_rebinding_invalidates_old_references_and_review_decisions() {
    let mut f = Fixture::new();
    let registration = f
        .broker
        .register_controller_session(f.connection, b"agent")
        .unwrap();
    let first_binding = enroll_controller(&mut f.broker, f.connection, registration, f.a);
    let stale = f
        .broker
        .prepare_controller_enrollment(f.connection, registration, f.a)
        .unwrap();
    let latest = f
        .broker
        .prepare_controller_enrollment(f.connection, registration, f.b)
        .unwrap();
    assert_eq!(
        f.broker
            .approve_controller_enrollment(f.connection, NativeControllerEnrollment::new(stale)),
        Err(Error::BindingMismatch)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, registration, first_binding),
        Ok(f.a)
    );

    let second_binding = f
        .broker
        .approve_controller_enrollment(f.connection, NativeControllerEnrollment::new(latest))
        .unwrap();
    assert_ne!(first_binding, second_binding);
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, registration, first_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, registration, second_binding),
        Ok(f.b)
    );
    let third_binding = enroll_controller(&mut f.broker, f.connection, registration, f.b);
    assert_ne!(second_binding, third_binding);
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, registration, second_binding),
        Err(Error::Unauthorized)
    );

    let pending = f
        .broker
        .prepare_controller_enrollment(f.connection, registration, f.a)
        .unwrap();
    f.broker
        .unregister_controller_session(f.connection, registration)
        .unwrap();
    let replacement = f
        .broker
        .register_controller_session(f.connection, b"agent")
        .unwrap();
    assert_ne!(registration, replacement);
    assert_eq!(
        f.broker
            .approve_controller_enrollment(f.connection, NativeControllerEnrollment::new(pending)),
        Err(Error::Unauthorized)
    );
}

#[test]
fn controller_enrollment_invalidates_on_replacement_close_and_broker_restart() {
    let mut f = Fixture::new();
    let other_connection = f.broker.accept_connection().unwrap();
    let first = f
        .broker
        .register_controller_session(f.connection, b"first")
        .unwrap();
    let second = f
        .broker
        .register_controller_session(f.connection, b"second")
        .unwrap();
    let first_binding = enroll_controller(&mut f.broker, f.connection, first, f.a);
    let second_binding = enroll_controller(&mut f.broker, f.connection, second, f.b);
    let other_registration = f
        .broker
        .register_controller_session(other_connection, b"other")
        .unwrap();
    let other_binding = enroll_controller(&mut f.broker, other_connection, other_registration, f.a);
    let pending = f
        .broker
        .prepare_controller_enrollment(f.connection, first, f.b)
        .unwrap();
    let replacement = f.broker.attach_target(f.a.session(), 2).unwrap();
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, first, first_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(other_connection, other_registration, other_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .approve_controller_enrollment(f.connection, NativeControllerEnrollment::new(pending)),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, second, second_binding),
        Ok(f.b)
    );
    let fresh = f
        .broker
        .register_controller_session(f.connection, b"first")
        .unwrap();
    assert_ne!(fresh, first);
    let fresh_binding = enroll_controller(&mut f.broker, f.connection, fresh, replacement);
    let pending_closed_target = f
        .broker
        .prepare_controller_enrollment(f.connection, fresh, f.b)
        .unwrap();
    f.broker.close_target(f.b).unwrap();
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, second, second_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker.approve_controller_enrollment(
            f.connection,
            NativeControllerEnrollment::new(pending_closed_target)
        ),
        Err(Error::StaleTarget)
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, fresh, fresh_binding),
        Ok(replacement)
    );
    let session = f.a.session();
    drop(f);

    let mut before = Broker::new([7; 16]);
    let old_connection = before.accept_connection().unwrap();
    let old_target = before.attach_target(session, 2).unwrap();
    let old_registration = before
        .register_controller_session(old_connection, b"first")
        .unwrap();
    let old_binding = enroll_controller(&mut before, old_connection, old_registration, old_target);
    let before_restart = before
        .prepare_controller_enrollment(old_connection, old_registration, old_target)
        .unwrap();
    drop(before);

    let mut restarted = Broker::new([8; 16]);
    let connection = restarted.accept_connection().unwrap();
    let target = restarted.attach_target(session, 2).unwrap();
    let registration = restarted
        .register_controller_session(connection, b"first")
        .unwrap();
    assert_eq!(
        restarted.resolve_controller_enrollment(connection, old_registration, old_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        restarted.approve_controller_enrollment(
            connection,
            NativeControllerEnrollment::new(before_restart)
        ),
        Err(Error::Unauthorized)
    );
    let binding = enroll_controller(&mut restarted, connection, registration, target);
    assert_ne!(old_binding, binding);
    assert_eq!(
        restarted.resolve_controller_enrollment(connection, registration, old_binding),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        restarted.resolve_controller_enrollment(connection, registration, binding),
        Ok(target)
    );
}

#[test]
fn controller_registrations_are_bounded_and_capacity_is_reclaimed() {
    let mut broker = Broker::with_limits(
        [7; 16],
        Limits {
            controller_registrations: 3,
            controller_registrations_per_connection: 2,
            ..Limits::default()
        },
    );
    let first = broker.accept_connection().unwrap();
    let second = broker.accept_connection().unwrap();
    assert_eq!(
        broker.register_controller_session(first, b""),
        Err(Error::InvalidRequest)
    );
    assert_eq!(
        broker.register_controller_session(first, &[0; MAX_CONTROLLER_REGISTRATION_BYTES + 1]),
        Err(Error::InvalidRequest)
    );
    let maximum = [0xff; MAX_CONTROLLER_REGISTRATION_BYTES];
    let first_id = broker.register_controller_session(first, &maximum).unwrap();
    let second_id = broker
        .register_controller_session(first, b"\0opaque")
        .unwrap();
    assert_eq!(
        broker.register_controller_session(first, &maximum),
        Ok(first_id)
    );
    assert_eq!(
        broker.register_controller_session(first, b"over-principal-limit"),
        Err(Error::QuotaExceeded)
    );
    broker
        .register_controller_session(second, b"other")
        .unwrap();
    assert_eq!(
        broker.register_controller_session(second, b"over-global-limit"),
        Err(Error::QuotaExceeded)
    );
    broker
        .unregister_controller_session(first, second_id)
        .unwrap();
    broker
        .register_controller_session(second, b"now-fits")
        .unwrap();
    broker.disconnect(first);
    let third = broker.accept_connection().unwrap();
    assert!(broker.register_controller_session(third, b"freed").is_ok());

    let mut clamped = Broker::with_limits(
        [9; 16],
        Limits {
            controller_registrations: usize::MAX,
            controller_registrations_per_connection: usize::MAX,
            ..Limits::default()
        },
    );
    let connection = clamped.accept_connection().unwrap();
    for id in 0..Limits::default().controller_registrations_per_connection {
        clamped
            .register_controller_session(connection, &[id as u8])
            .unwrap();
    }
    assert_eq!(
        clamped.register_controller_session(connection, b"one-too-many"),
        Err(Error::QuotaExceeded)
    );
    for _ in 1..(Limits::default().controller_registrations
        / Limits::default().controller_registrations_per_connection)
    {
        let connection = clamped.accept_connection().unwrap();
        for id in 0..Limits::default().controller_registrations_per_connection {
            clamped
                .register_controller_session(connection, &[id as u8])
                .unwrap();
        }
    }
    let another = clamped.accept_connection().unwrap();
    assert_eq!(
        clamped.register_controller_session(another, b"over-global-ceiling"),
        Err(Error::QuotaExceeded)
    );
}

#[test]
fn controller_enrollment_never_authorizes_discovery_observation_or_control() {
    let mut f = Fixture::new();
    let registration = f
        .broker
        .register_controller_session(f.connection, b"hardware-gesture=approved")
        .unwrap();
    let mut requested = f.scope();
    requested.discover.insert(f.b);
    requested.observe.insert(f.b);
    requested.control.insert(f.b);
    let spec = f.spec(requested.clone(), binding(1));
    assert_eq!(
        f.broker.admit_run(f.connection, None, spec.clone(), f.now),
        Err(Error::Revoked)
    );
    let enrollment = enroll_controller(&mut f.broker, f.connection, registration, f.b);
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, registration, enrollment),
        Ok(f.b)
    );
    assert_eq!(
        f.broker.admit_run(f.connection, None, spec, f.now),
        Err(Error::Revoked)
    );

    let allowed = Scope {
        discover: BTreeSet::from([f.a]),
        observe: BTreeSet::from([f.a]),
        ..Scope::default()
    };
    f.approve(
        allowed.clone(),
        &[
            direct(DirectKind::Discovery),
            direct(DirectKind::Observation),
        ],
    );
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, registration, enrollment),
        Ok(f.b)
    );
    let discovery = f.run(allowed.clone(), direct(DirectKind::Discovery));
    let request = f
        .broker
        .prepare_request(
            f.connection,
            discovery,
            Operation::Discover { max_bytes: 64 },
            f.now,
        )
        .unwrap();
    assert_eq!(
        f.broker
            .discovery_targets(f.connection, request, f.now)
            .unwrap(),
        vec![f.a]
    );
    let observation = f.run(allowed, direct(DirectKind::Observation));
    assert_eq!(
        f.broker.prepare_request(
            f.connection,
            observation,
            Operation::Observe {
                target: f.b,
                max_bytes: 64,
            },
            f.now
        ),
        Err(Error::Unauthorized)
    );
    assert!(matches!(
        f.broker
            .prepare_lease(f.connection, observation, f.b, f.now),
        Err(Error::Unauthorized)
    ));
    assert_eq!(
        f.broker.admit_run(
            f.connection,
            None,
            f.spec(requested.clone(), binding(1)),
            f.now
        ),
        Err(Error::Unauthorized)
    );
    f.broker.revoke(f.connection);
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, registration, enrollment),
        Ok(f.b)
    );
    assert_eq!(
        f.broker
            .admit_run(f.connection, None, f.spec(requested, binding(1)), f.now),
        Err(Error::Revoked)
    );
}

#[test]
fn global_revocation_discards_controller_enrollments() {
    let mut f = Fixture::new();
    let registration = f
        .broker
        .register_controller_session(f.connection, b"agent")
        .unwrap();
    let enrollment = enroll_controller(&mut f.broker, f.connection, registration, f.a);
    let pending = f
        .broker
        .prepare_controller_enrollment(f.connection, registration, f.b)
        .unwrap();
    f.broker.revoke_all();
    assert_eq!(
        f.broker
            .resolve_controller_enrollment(f.connection, registration, enrollment),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        f.broker
            .approve_controller_enrollment(f.connection, NativeControllerEnrollment::new(pending)),
        Err(Error::Unauthorized)
    );
    let fresh = f
        .broker
        .register_controller_session(f.connection, b"agent")
        .unwrap();
    assert_ne!(registration, fresh);
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
