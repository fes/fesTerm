//! Frozen workflow inputs and manifest attenuation for the application.
//!
//! This crate has no broker, native approval, worker, or session authority.
//! The application must project every worker call before using its one live
//! broker; the broker's coarser scopes do not enforce individual manifest
//! capabilities. Recheck lifecycle and recipients at the admission boundary.

pub mod intake;
pub mod terminal_input;

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use festerm_automation::{
    Budget, Digest, GrantRequest, Invocation, InvocationBinding, RunSpec, Scope, TargetRef,
    MAX_SCOPE_TARGETS,
};
use festerm_automation_js::{
    protocol::{HostRequest, Target, MAX_SAFE_INTEGER},
    snapshot::{ApprovedBundle, BoundParameters, Capability, ParameterValue, Parameters},
};
use sha2::{Digest as _, Sha256};

const PARAMETER_DOMAIN: &[u8] = b"fesTerm workflow parameters and targets v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidInput,
    Denied,
    Unsupported,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "invalid frozen workflow input",
            Self::Denied => "workflow capability or target denied",
            Self::Unsupported => "workflow operation is not yet supported",
        })
    }
}

impl std::error::Error for Error {}

/// A checked projection only; it is not permission to access a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectedCall {
    Discover,
    Observe(TargetRef),
    Control(TargetRef),
}

/// Owns the exact source, parameters, opaque target mapping, and requested
/// scopes shown to the native approver. No mutable view of them is exposed.
pub struct FrozenInvocation {
    bundle: ApprovedBundle,
    parameters: BoundParameters,
    targets: BTreeMap<String, TargetRef>,
    scope: Scope,
    binding: InvocationBinding,
}

impl FrozenInvocation {
    pub fn new(
        bundle: ApprovedBundle,
        parameters: Parameters,
        targets: impl IntoIterator<Item = (String, TargetRef)>,
        scope: Scope,
    ) -> Result<Self, Error> {
        bundle
            .validate_parameters(&parameters)
            .map_err(|_| Error::InvalidInput)?;
        let capabilities = &bundle.manifest().capabilities;
        // The broker requires this bit for root workflows too; project() still
        // refuses child invocations until their separate integration exists.
        if !scope.invoke_workflows
            || (!scope.discover.is_empty() && !capabilities.contains(&Capability::Observe))
            || (!scope.observe.is_empty()
                && ![
                    Capability::ReadScreen,
                    Capability::ReadScrollback,
                    Capability::GetSelection,
                    Capability::WaitForChange,
                ]
                .iter()
                .any(|capability| capabilities.contains(capability)))
            || (!scope.control.is_empty()
                && ![Capability::SendText, Capability::SendKeys]
                    .iter()
                    .any(|capability| capabilities.contains(capability)))
        {
            return Err(Error::Denied);
        }

        let scoped: BTreeSet<_> = scope
            .discover
            .union(&scope.observe)
            .copied()
            .chain(scope.control.iter().copied())
            .collect();
        if scoped.len() > MAX_SCOPE_TARGETS {
            return Err(Error::InvalidInput);
        }
        let mut mapped = BTreeMap::new();
        let mut references = BTreeSet::new();
        for (id, reference) in targets {
            if mapped.len() >= MAX_SCOPE_TARGETS {
                return Err(Error::InvalidInput);
            }
            let wire = Target {
                id: id.clone(),
                generation: reference.generation(),
            };
            if reference.generation() > MAX_SAFE_INTEGER
                || (HostRequest::ReadScreen {
                    target: wire,
                    max_bytes: 1,
                })
                .validate()
                .is_err()
                || mapped.insert(id, reference).is_some()
                || !references.insert(reference)
            {
                return Err(Error::InvalidInput);
            }
        }

        if references != scoped
            || references.iter().next().is_some_and(|first| {
                references.iter().any(|reference| {
                    reference.application_instance() != first.application_instance()
                })
            })
            || parameters.values().any(
                |value| matches!(value, ParameterValue::Target(id) if !mapped.contains_key(id)),
            )
        {
            return Err(Error::InvalidInput);
        }

        let encoded_bundle =
            serde_json::to_vec(bundle.snapshot()).map_err(|_| Error::InvalidInput)?;
        let bundle_hash = Sha256::digest(encoded_bundle);
        if format!("{bundle_hash:x}") != bundle.digest() {
            return Err(Error::InvalidInput);
        }
        let manifest = serde_json::to_vec(bundle.manifest()).map_err(|_| Error::InvalidInput)?;
        let encoded_parameters =
            serde_json::to_vec(&(&parameters, &mapped)).map_err(|_| Error::InvalidInput)?;
        let mut parameters_hash = Sha256::new();
        parameters_hash.update(PARAMETER_DOMAIN);
        parameters_hash.update(encoded_parameters);
        let binding = InvocationBinding {
            api_version: u32::from(bundle.manifest().api_version),
            invocation: Invocation::Workflow {
                bundle: Digest(bundle_hash.into()),
                manifest: Digest(Sha256::digest(manifest).into()),
            },
            parameters: Digest(parameters_hash.finalize().into()),
        };
        let target_generations = parameters
            .values()
            .filter_map(|value| match value {
                ParameterValue::Target(id) => Some(id),
                _ => None,
            })
            .map(|id| {
                mapped
                    .get(id)
                    .map(|reference| (id.clone(), reference.generation()))
                    .ok_or(Error::InvalidInput)
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let parameters = bundle
            .bind_parameters(parameters, target_generations)
            .map_err(|_| Error::InvalidInput)?;
        Ok(Self {
            bundle,
            parameters,
            targets: mapped,
            scope,
            binding,
        })
    }

    pub fn bundle(&self) -> &ApprovedBundle {
        &self.bundle
    }

    pub fn parameters(&self) -> &Parameters {
        self.parameters.values()
    }

    pub fn worker_parameters(&self) -> &BoundParameters {
        &self.parameters
    }

    pub fn targets(&self) -> &BTreeMap<String, TargetRef> {
        &self.targets
    }

    pub fn binding(&self) -> InvocationBinding {
        self.binding
    }

    /// Proposal for a native decision, never an approval itself.
    pub fn grant_request(&self) -> GrantRequest {
        GrantRequest {
            scope: self.scope.clone(),
            bindings: BTreeSet::from([self.binding]),
            direct_read_calls: BTreeSet::new(),
        }
    }

    pub fn run_spec(&self, budget: Budget, deadline: Instant) -> RunSpec {
        RunSpec {
            binding: self.binding,
            scope: self.scope.clone(),
            budget,
            deadline,
        }
    }

    /// Mandatory for every worker request: rejects calls outside the frozen
    /// manifest and exact target mapping. The projection is not authority and
    /// must still pass the live broker and controller.
    pub fn project(&self, request: &HostRequest) -> Result<ProjectedCall, Error> {
        request.validate().map_err(|_| Error::InvalidInput)?;
        let (capability, target, control) = match request {
            HostRequest::Observe {} => (Capability::Observe, None, false),
            HostRequest::ReadScreen { target, .. } => (Capability::ReadScreen, Some(target), false),
            HostRequest::ReadScrollback { target, .. } => {
                (Capability::ReadScrollback, Some(target), false)
            }
            HostRequest::GetSelection { target, .. } => {
                (Capability::GetSelection, Some(target), false)
            }
            HostRequest::WaitForChange { target, .. } => {
                (Capability::WaitForChange, Some(target), false)
            }
            HostRequest::SendText { target, .. } => (Capability::SendText, Some(target), true),
            HostRequest::SendKeys { target, .. } => (Capability::SendKeys, Some(target), true),
            HostRequest::Open { .. } => (Capability::Open, None, false),
            HostRequest::InvokeWorkflow { .. } => (Capability::InvokeWorkflow, None, false),
        };
        if !self.bundle.manifest().capabilities.contains(&capability) {
            return Err(Error::Denied);
        }
        if matches!(capability, Capability::Open | Capability::InvokeWorkflow) {
            return Err(Error::Unsupported);
        }
        let Some(target) = target else {
            return Ok(ProjectedCall::Discover);
        };
        let reference = self
            .targets
            .get(&target.id)
            .filter(|reference| reference.generation() == target.generation)
            .copied()
            .ok_or(Error::Denied)?;
        if control && self.scope.control.contains(&reference) {
            Ok(ProjectedCall::Control(reference))
        } else if !control && self.scope.observe.contains(&reference) {
            Ok(ProjectedCall::Observe(reference))
        } else {
            Err(Error::Denied)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_automation::{
        Broker, Completion, Error as BrokerError, NativeApproval, Operation, RunState,
    };
    use festerm_automation_js::snapshot::{
        Manifest, ParameterType, ParameterValue, SourceSnapshot,
    };
    use std::time::Duration;

    fn bundle(source: &str, capabilities: &[Capability]) -> ApprovedBundle {
        ApprovedBundle::new(SourceSnapshot {
            manifest: Manifest {
                version: 1,
                api_version: 1,
                name: "test".into(),
                entry: "main.js".into(),
                modules: vec!["main.js".into()],
                parameters: BTreeMap::from([("target".into(), ParameterType::Target {})]),
                capabilities: capabilities.iter().cloned().collect(),
            },
            modules: BTreeMap::from([("main.js".into(), source.into())]),
        })
        .unwrap()
    }

    fn parameters(target: &str) -> Parameters {
        BTreeMap::from([("target".into(), ParameterValue::Target(target.into()))])
    }

    fn wire(id: &str, generation: u64) -> Target {
        Target {
            id: id.into(),
            generation,
        }
    }

    #[test]
    fn frozen_bindings_include_code_parameters_and_exact_target_mapping() {
        let mut broker = Broker::new([1; 16]);
        let a = broker
            .attach_target(
                1,
                1,
                &festerm_automation::automation_admission_lock().unwrap(),
            )
            .unwrap();
        let b = broker
            .attach_target(
                2,
                2,
                &festerm_automation::automation_admission_lock().unwrap(),
            )
            .unwrap();
        let source = "export function main() { return 1; }";
        let approved = bundle(source, &[Capability::ReadScreen]);
        let scope = Scope {
            observe: BTreeSet::from([a, b]),
            invoke_workflows: true,
            return_results: true,
            ..Scope::default()
        };
        let make = |params, targets| {
            FrozenInvocation::new(approved.clone(), params, targets, scope.clone()).unwrap()
        };
        let original = make(parameters("a"), vec![("a".into(), a), ("b".into(), b)]);
        let reordered = make(parameters("a"), vec![("b".into(), b), ("a".into(), a)]);
        let swapped = make(parameters("a"), vec![("a".into(), b), ("b".into(), a)]);
        let changed_parameter = make(parameters("b"), vec![("a".into(), a), ("b".into(), b)]);
        assert_eq!(original.binding(), reordered.binding());
        assert_ne!(original.binding().parameters, swapped.binding().parameters);
        assert_ne!(
            original.binding().parameters,
            changed_parameter.binding().parameters
        );
        assert_eq!(original.targets()["a"], a);
        assert_eq!(original.worker_parameters().target("a"), Some(wire("a", 1)));
        assert_eq!(swapped.worker_parameters().target("a"), Some(wire("a", 2)));
        assert_eq!(original.worker_parameters().target("b"), None);

        let changed_source = FrozenInvocation::new(
            bundle(
                "export function main() { return 2; }",
                &[Capability::ReadScreen],
            ),
            parameters("a"),
            vec![("a".into(), a), ("b".into(), b)],
            scope,
        )
        .unwrap();
        assert_ne!(
            original.binding().invocation,
            changed_source.binding().invocation
        );
    }

    #[test]
    fn manifest_and_frozen_target_scopes_attenuate_host_calls() {
        let mut broker = Broker::new([2; 16]);
        let a = broker
            .attach_target(
                1,
                5,
                &festerm_automation::automation_admission_lock().unwrap(),
            )
            .unwrap();
        let b = broker
            .attach_target(
                2,
                6,
                &festerm_automation::automation_admission_lock().unwrap(),
            )
            .unwrap();
        let capabilities = [
            Capability::Observe,
            Capability::ReadScreen,
            Capability::SendText,
            Capability::Open,
            Capability::InvokeWorkflow,
        ];
        let scope = Scope {
            discover: BTreeSet::from([a]),
            observe: BTreeSet::from([a]),
            control: BTreeSet::from([b]),
            invoke_workflows: true,
            return_results: true,
        };
        let frozen = FrozenInvocation::new(
            bundle("export function main() {}", &capabilities),
            parameters("a"),
            vec![("a".into(), a), ("b".into(), b)],
            scope.clone(),
        )
        .unwrap();
        let read = |target| HostRequest::ReadScreen {
            target,
            max_bytes: 20,
        };
        assert_eq!(
            frozen.project(&HostRequest::Observe {}),
            Ok(ProjectedCall::Discover)
        );
        assert_eq!(
            frozen.project(&read(wire("a", 5))),
            Ok(ProjectedCall::Observe(a))
        );
        assert_eq!(frozen.project(&read(wire("a", 6))), Err(Error::Denied));
        assert_eq!(frozen.project(&read(wire("b", 6))), Err(Error::Denied));
        assert_eq!(
            frozen.project(&HostRequest::SendText {
                target: wire("b", 6),
                text: "hello".into(),
            }),
            Ok(ProjectedCall::Control(b))
        );
        assert_eq!(
            frozen.project(&HostRequest::SendKeys {
                target: wire("b", 6),
                keys: vec!["Enter".into()],
            }),
            Err(Error::Denied)
        );
        assert_eq!(
            frozen.project(&HostRequest::Open {
                profile_id: "profile".into(),
                revision: "1".into(),
            }),
            Err(Error::Unsupported)
        );
        assert_eq!(
            frozen.project(&HostRequest::InvokeWorkflow {
                digest: "a".repeat(64),
                parameters: Parameters::new(),
            }),
            Err(Error::Unsupported)
        );

        assert!(matches!(
            FrozenInvocation::new(
                bundle("export function main() {}", &capabilities),
                parameters("missing"),
                vec![("a".into(), a), ("b".into(), b)],
                scope.clone(),
            ),
            Err(Error::InvalidInput)
        ));
        assert!(matches!(
            FrozenInvocation::new(
                bundle("export function main() {}", &capabilities),
                parameters("a"),
                vec![("a".into(), a), ("a".into(), b)],
                scope.clone(),
            ),
            Err(Error::InvalidInput)
        ));
        assert!(matches!(
            FrozenInvocation::new(
                bundle("export function main() {}", &capabilities),
                parameters("a"),
                vec![("a".into(), a), ("b".into(), a)],
                scope.clone(),
            ),
            Err(Error::InvalidInput)
        ));
        assert!(matches!(
            FrozenInvocation::new(
                bundle("export function main() {}", &capabilities),
                parameters("a"),
                vec![("a".into(), a)],
                scope.clone(),
            ),
            Err(Error::InvalidInput)
        ));
        assert!(matches!(
            FrozenInvocation::new(
                bundle("export function main() {}", &[Capability::ReadScreen]),
                parameters("a"),
                vec![("a".into(), a), ("b".into(), b)],
                scope,
            ),
            Err(Error::Denied)
        ));
    }

    #[test]
    fn projection_does_not_replace_broker_approval_or_completion_cut() {
        let mut broker = Broker::new([3; 16]);
        let connection = broker.accept_connection().unwrap();
        let target = broker
            .attach_target(
                1,
                1,
                &festerm_automation::automation_admission_lock().unwrap(),
            )
            .unwrap();
        let scope = Scope {
            observe: BTreeSet::from([target]),
            invoke_workflows: true,
            return_results: true,
            ..Scope::default()
        };
        let make = |source| {
            FrozenInvocation::new(
                bundle(source, &[Capability::ReadScreen]),
                parameters("a"),
                vec![("a".into(), target)],
                scope.clone(),
            )
            .unwrap()
        };
        let frozen = make("export function main() { return 1; }");
        let changed = make("export function main() { return 2; }");
        let now = Instant::now();
        let spec = frozen.run_spec(Budget::default(), now + Duration::from_secs(30));
        assert_eq!(
            broker.admit_run(connection, None, spec.clone(), now),
            Err(BrokerError::Revoked)
        );
        let proposal = frozen.grant_request();
        let approval = broker
            .request_approval(connection, proposal.clone(), now)
            .unwrap();
        broker
            .approve(
                connection,
                approval,
                NativeApproval::new(proposal, Duration::from_secs(60)),
                now,
                &festerm_automation::automation_admission_lock().unwrap(),
            )
            .unwrap();
        assert_eq!(
            broker.admit_run(
                connection,
                None,
                changed.run_spec(Budget::default(), now + Duration::from_secs(30)),
                now,
            ),
            Err(BrokerError::Unauthorized)
        );
        let run = broker.admit_run(connection, None, spec, now).unwrap();
        let call = HostRequest::ReadScreen {
            target: wire("a", 1),
            max_bytes: 20,
        };
        assert_eq!(frozen.project(&call), Ok(ProjectedCall::Observe(target)));
        let request = broker
            .prepare_request(
                connection,
                run,
                Operation::Observe {
                    target,
                    max_bytes: 20,
                },
                now,
            )
            .unwrap();
        let status = broker
            .complete(connection, run, Completion::Returned, now)
            .unwrap();
        assert_eq!(status.state, RunState::Failed);
        assert_eq!(status.failure, Some(BrokerError::UnsettledHostCalls));
        assert_eq!(
            broker.commit_disclosure(connection, request, 10, now),
            Err(BrokerError::Closed)
        );
    }
}
