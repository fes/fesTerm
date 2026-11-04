//! Non-authorizing, observation-only workflow review inputs.
//!
//! The caller supplies the currently live application-owned references. A
//! successful review is only a frozen proposal; it neither grants access nor
//! substitutes for checking those references again at a later decision.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use festerm_automation::{Scope, TargetRef, MAX_SCOPE_TARGETS};
use festerm_automation_js::snapshot::{ApprovedBundle, Capability, ParameterValue, Parameters};

use crate::FrozenInvocation;

const MAX_PARAMETERS: usize = 64;
pub const MAX_TEXT_LITERAL_BYTES: usize = 6 * 16_384 + 2;

/// A selection-local revision. Exhaustion permanently invalidates the epoch.
#[derive(Clone, Default)]
pub struct ReviewEpoch(Arc<AtomicU64>);

#[derive(Clone)]
pub struct ReviewRevision {
    epoch: Arc<AtomicU64>,
    value: u64,
}

impl ReviewEpoch {
    pub fn capture(&self) -> Option<ReviewRevision> {
        let value = self.0.load(Ordering::Acquire);
        (value != u64::MAX).then(|| ReviewRevision {
            epoch: self.0.clone(),
            value,
        })
    }

    pub fn invalidate(&self) -> Option<ReviewRevision> {
        let previous = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .ok()?;
        let value = previous + 1;
        (value != u64::MAX).then(|| ReviewRevision {
            epoch: self.0.clone(),
            value,
        })
    }

    pub fn same_epoch(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl ReviewRevision {
    pub fn is_current(&self) -> bool {
        self.value != u64::MAX && self.epoch.load(Ordering::Acquire) == self.value
    }

    pub fn same_epoch(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.epoch, &other.epoch)
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.same_epoch(other) && self.value == other.value && self.is_current()
    }
}

/// Decode an unambiguous ASCII JSON literal for the native parameter editor.
pub fn decode_text_literal(value: &str) -> Result<String, ReviewError> {
    if value.len() > MAX_TEXT_LITERAL_BYTES
        || !value.bytes().all(|byte| (b' '..=b'~').contains(&byte))
    {
        return Err(ReviewError::InvalidParameters);
    }
    serde_json::from_str(value).map_err(|_| ReviewError::InvalidParameters)
}

pub fn supports_observation_review(bundle: &ApprovedBundle) -> bool {
    bundle.manifest().capabilities.iter().all(|capability| {
        matches!(
            capability,
            Capability::Observe
                | Capability::ReadScreen
                | Capability::ReadScrollback
                | Capability::GetSelection
                | Capability::WaitForChange
        )
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultRecipient {
    NativeUser,
}

#[derive(Clone, PartialEq, Eq)]
pub enum InputValue {
    Text(String),
    Integer(i64),
    Boolean(bool),
    Target(TargetRef),
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub parameters: BTreeMap<String, InputValue>,
    /// Each reference is explicitly selected by the native user.
    pub targets: BTreeSet<TargetRef>,
}

impl std::fmt::Debug for Draft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Draft(<private>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewError {
    UnsupportedCapability,
    TargetRequired,
    TargetNotSelected,
    TargetWithoutObservation,
    StaleTarget,
    TooManyTargets,
    InvalidParameters,
    InvalidInvocation,
}

impl std::fmt::Display for ReviewError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedCapability => {
                "This workflow requests an unsupported capability; no proposal was created."
            }
            Self::TargetRequired => "Explicitly select at least one live terminal attachment.",
            Self::TargetNotSelected => {
                "Explicitly choose a selected attachment for every target parameter."
            }
            Self::TargetWithoutObservation => {
                "Target parameters require a declared observation capability in this review."
            }
            Self::StaleTarget => "A selected attachment changed or is no longer live.",
            Self::TooManyTargets => "Select at most 32 terminal attachments.",
            Self::InvalidParameters => "Complete every parameter with a valid value.",
            Self::InvalidInvocation => "The exact workflow proposal could not be frozen.",
        })
    }
}

impl std::error::Error for ReviewError {}

/// One immutable native-user proposal. It contains no approval or grant.
pub struct Review {
    invocation: FrozenInvocation,
    draft: Draft,
    recipient: ResultRecipient,
}

impl Review {
    pub fn prepare(
        bundle: &ApprovedBundle,
        draft: Draft,
        live_targets: &BTreeSet<TargetRef>,
    ) -> Result<Self, ReviewError> {
        let capabilities = &bundle.manifest().capabilities;
        if !supports_observation_review(bundle) {
            return Err(ReviewError::UnsupportedCapability);
        }
        if draft.targets.len() > MAX_SCOPE_TARGETS {
            return Err(ReviewError::TooManyTargets);
        }
        if !draft.targets.is_subset(live_targets) {
            return Err(ReviewError::StaleTarget);
        }
        if draft.parameters.len() > MAX_PARAMETERS {
            return Err(ReviewError::InvalidParameters);
        }
        let discover = capabilities.contains(&Capability::Observe);
        let observe = [
            Capability::ReadScreen,
            Capability::ReadScrollback,
            Capability::GetSelection,
            Capability::WaitForChange,
        ]
        .iter()
        .any(|capability| capabilities.contains(capability));
        if (discover || observe) && draft.targets.is_empty() {
            return Err(ReviewError::TargetRequired);
        }
        if !draft.targets.is_empty() && !discover && !observe {
            return Err(ReviewError::TargetWithoutObservation);
        }

        let aliases: BTreeMap<_, _> = draft
            .targets
            .iter()
            .enumerate()
            .map(|(index, target)| (*target, format!("target-{}", index + 1)))
            .collect();
        let parameters = draft
            .parameters
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    InputValue::Text(value) => ParameterValue::Text(value.clone()),
                    InputValue::Integer(value) => ParameterValue::Integer(*value),
                    InputValue::Boolean(value) => ParameterValue::Boolean(*value),
                    InputValue::Target(target) => ParameterValue::Target(
                        aliases
                            .get(target)
                            .cloned()
                            .ok_or(ReviewError::TargetNotSelected)?,
                    ),
                };
                Ok((name.clone(), value))
            })
            .collect::<Result<Parameters, ReviewError>>()?;
        bundle
            .validate_parameters(&parameters)
            .map_err(|_| ReviewError::InvalidParameters)?;

        let scope = Scope {
            discover: if discover {
                draft.targets.clone()
            } else {
                BTreeSet::new()
            },
            observe: if observe {
                draft.targets.clone()
            } else {
                BTreeSet::new()
            },
            control: BTreeSet::new(),
            // The kernel uses this bit for the root workflow. Projection still
            // rejects child workflow calls, and this is not a grant.
            invoke_workflows: true,
            return_results: true,
        };
        let invocation = FrozenInvocation::new(
            bundle.clone(),
            parameters,
            aliases.into_iter().map(|(target, id)| (id, target)),
            scope,
        )
        .map_err(|_| ReviewError::InvalidInvocation)?;
        Ok(Self {
            invocation,
            draft,
            recipient: ResultRecipient::NativeUser,
        })
    }

    pub fn invocation(&self) -> &FrozenInvocation {
        &self.invocation
    }

    pub fn recipient(&self) -> ResultRecipient {
        self.recipient
    }

    /// A comparison only; native integration must also check its selection,
    /// settings and live-attachment fences before presenting or using a review.
    pub fn is_current(
        &self,
        bundle: &ApprovedBundle,
        draft: &Draft,
        live_targets: &BTreeSet<TargetRef>,
    ) -> bool {
        self.invocation.bundle().digest() == bundle.digest()
            && self.draft == *draft
            && self.draft.targets.is_subset(live_targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_automation::{Broker, Invocation};
    use festerm_automation_js::snapshot::{Manifest, ParameterType, SourceSnapshot};

    #[test]
    fn native_review_revision_is_single_selection_and_never_resurrects() {
        let epoch = ReviewEpoch::default();
        let initial = epoch.capture().unwrap();
        let same = epoch.capture().unwrap();
        assert!(initial.matches(&same));
        let changed = epoch.invalidate().unwrap();
        assert!(!initial.is_current());
        assert!(!initial.matches(&same));
        assert!(changed.is_current());
        assert!(!changed.matches(&ReviewEpoch::default().capture().unwrap()));
        epoch.0.store(u64::MAX - 1, Ordering::Release);
        let last = epoch.capture().unwrap();
        assert!(epoch.invalidate().is_none());
        assert!(!last.is_current());
        assert!(epoch.capture().is_none());
        assert!(epoch.invalidate().is_none());
    }

    #[test]
    fn native_review_text_literals_refuse_hidden_input_and_decode_escapes() {
        assert_eq!(
            decode_text_literal(r#""hello\u202e\n""#).unwrap(),
            "hello\u{202e}\n"
        );
        assert!(decode_text_literal("\"hello\u{202e}\"").is_err());
        assert!(decode_text_literal("\"line\nbreak\"").is_err());
        assert!(decode_text_literal("\"unfinished").is_err());
        assert!(decode_text_literal(&" ".repeat(MAX_TEXT_LITERAL_BYTES + 1)).is_err());
    }

    fn bundle(source: &str, capabilities: &[Capability]) -> ApprovedBundle {
        ApprovedBundle::new(SourceSnapshot {
            manifest: Manifest {
                version: 1,
                api_version: 1,
                name: "review".into(),
                entry: "main.js".into(),
                modules: vec!["main.js".into()],
                parameters: BTreeMap::from([
                    ("count".into(), ParameterType::Integer { min: 1, max: 3 }),
                    ("message".into(), ParameterType::Text { max_bytes: 32 }),
                    ("quiet".into(), ParameterType::Boolean {}),
                    ("target".into(), ParameterType::Target {}),
                ]),
                capabilities: capabilities.iter().cloned().collect(),
            },
            modules: BTreeMap::from([("main.js".into(), source.into())]),
        })
        .unwrap()
    }

    fn draft(target: TargetRef) -> Draft {
        Draft {
            parameters: BTreeMap::from([
                ("count".into(), InputValue::Integer(2)),
                ("message".into(), InputValue::Text("hello".into())),
                ("quiet".into(), InputValue::Boolean(false)),
                ("target".into(), InputValue::Target(target)),
            ]),
            targets: BTreeSet::from([target]),
        }
    }

    #[test]
    fn native_review_requires_explicit_target_and_freezes_observation_scope() {
        let mut registry = Broker::new([1; 16]);
        let a = registry.attach_target(1, 1).unwrap();
        let b = registry.attach_target(2, 2).unwrap();
        let live = BTreeSet::from([a, b]);
        let bundle = bundle(
            "export default 1;",
            &[Capability::Observe, Capability::ReadScreen],
        );
        let mut unselected = draft(a);
        unselected.targets.clear();
        assert!(matches!(
            Review::prepare(&bundle, unselected, &live),
            Err(ReviewError::TargetRequired)
        ));
        let mut wrong_parameter = draft(a);
        wrong_parameter
            .parameters
            .insert("target".into(), InputValue::Target(b));
        assert!(matches!(
            Review::prepare(&bundle, wrong_parameter, &live),
            Err(ReviewError::TargetNotSelected)
        ));
        let review = Review::prepare(&bundle, draft(a), &live).unwrap();
        let invocation = review.invocation();
        assert_eq!(review.recipient(), ResultRecipient::NativeUser);
        let scope = invocation.grant_request().scope;
        assert_eq!(scope.discover, BTreeSet::from([a]));
        assert_eq!(scope.observe, BTreeSet::from([a]));
        assert!(scope.control.is_empty());
        assert!(scope.return_results);
        assert_eq!(invocation.targets()["target-1"], a);
        assert_eq!(
            invocation.parameters()["target"],
            ParameterValue::Target("target-1".into())
        );
        assert!(review.is_current(&bundle, &draft(a), &live));
    }

    #[test]
    fn native_review_rejects_stale_and_replaced_attachment_generations() {
        let mut registry = Broker::new([2; 16]);
        let old = registry.attach_target(7, 1).unwrap();
        let bundle = bundle("export default 1;", &[Capability::ReadScreen]);
        let original = draft(old);
        let review = Review::prepare(&bundle, original.clone(), &BTreeSet::from([old])).unwrap();
        let replacement = registry.attach_target(7, 2).unwrap();
        let live = BTreeSet::from([replacement]);
        assert!(!review.is_current(&bundle, &original, &live));
        assert!(matches!(
            Review::prepare(&bundle, original, &live),
            Err(ReviewError::StaleTarget)
        ));
        let next = Review::prepare(&bundle, draft(replacement), &live).unwrap();
        assert_ne!(
            review.invocation().binding().parameters,
            next.invocation().binding().parameters
        );
    }

    #[test]
    fn native_review_invalidates_changed_bundle_and_typed_parameters() {
        let mut registry = Broker::new([3; 16]);
        let target = registry.attach_target(1, 1).unwrap();
        let live = BTreeSet::from([target]);
        let captured = bundle("export default 1;", &[Capability::ReadScreen]);
        let original = draft(target);
        let review = Review::prepare(&captured, original.clone(), &live).unwrap();
        let changed_bundle = bundle("export default 2;", &[Capability::ReadScreen]);
        assert!(!review.is_current(&changed_bundle, &original, &live));
        let mut changed_parameters = original.clone();
        changed_parameters
            .parameters
            .insert("message".into(), InputValue::Text("different".into()));
        assert!(!review.is_current(&captured, &changed_parameters, &live));
        let changed = Review::prepare(&captured, changed_parameters, &live).unwrap();
        assert_ne!(
            review.invocation().binding().parameters,
            changed.invocation().binding().parameters
        );
        let changed_source = Review::prepare(&changed_bundle, original, &live).unwrap();
        assert!(matches!(
            (review.invocation().binding().invocation, changed_source.invocation().binding().invocation),
            (Invocation::Workflow { bundle: left, .. }, Invocation::Workflow { bundle: right, .. }) if left != right
        ));
    }

    #[test]
    fn native_review_refuses_every_unsupported_manifest_capability() {
        let mut registry = Broker::new([4; 16]);
        let target = registry.attach_target(1, 1).unwrap();
        let live = BTreeSet::from([target]);
        for unsupported in [
            Capability::SendText,
            Capability::SendKeys,
            Capability::Open,
            Capability::InvokeWorkflow,
        ] {
            let bundle = bundle("export default 1;", &[Capability::ReadScreen, unsupported]);
            assert!(matches!(
                Review::prepare(&bundle, draft(target), &live),
                Err(ReviewError::UnsupportedCapability)
            ));
        }
    }

    #[test]
    fn native_review_validates_all_parameter_types_and_bounds() {
        let mut registry = Broker::new([5; 16]);
        let target = registry.attach_target(1, 1).unwrap();
        let live = BTreeSet::from([target]);
        let bundle = bundle("export default 1;", &[Capability::ReadScreen]);
        let mut invalid = draft(target);
        invalid
            .parameters
            .insert("count".into(), InputValue::Integer(4));
        assert!(matches!(
            Review::prepare(&bundle, invalid, &live),
            Err(ReviewError::InvalidParameters)
        ));
        let mut invalid = draft(target);
        invalid
            .parameters
            .insert("quiet".into(), InputValue::Text("false".into()));
        assert!(matches!(
            Review::prepare(&bundle, invalid, &live),
            Err(ReviewError::InvalidParameters)
        ));
        let mut too_long = draft(target);
        too_long
            .parameters
            .insert("message".into(), InputValue::Text("x".repeat(33)));
        assert!(matches!(
            Review::prepare(&bundle, too_long, &live),
            Err(ReviewError::InvalidParameters)
        ));
        let mut missing = draft(target);
        missing.parameters.remove("quiet");
        assert!(matches!(
            Review::prepare(&bundle, missing, &live),
            Err(ReviewError::InvalidParameters)
        ));
    }
}
