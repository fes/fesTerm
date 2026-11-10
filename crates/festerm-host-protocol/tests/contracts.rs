use festerm_host_protocol::*;
use serde_json::{json, Value};
use std::str::FromStr;

fn id<T: FromStr>(n: u32) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("{n:08x}-0000-4000-8000-000000000001")
        .parse()
        .unwrap()
}

fn session(n: u32) -> SessionSummary {
    SessionSummary {
        target: Target {
            logical: id(n),
            generation: GenerationId::new(u128::from(n) + 1).unwrap(),
        },
        name: format!("Shell {n}"),
        lifetime: Lifetime::Persistent,
        attached: false,
    }
}

fn creation() -> Creation {
    Creation {
        logical: id(2),
        name: Some("Build".into()),
        recipe: Recipe {
            shell: Some("/bin/sh".into()),
            args: vec!["-l".into()],
            cwd: Some("/work".into()),
        },
        lifetime: Lifetime::Persistent,
    }
}

#[test]
fn host_wire_fixture_is_canonical_and_not_legacy_protocol() {
    let frame = Envelope::new(
        id(1),
        id(3),
        Request::Ensure {
            creation: creation(),
        },
    );
    let bytes = encode(&frame).unwrap();
    assert_eq!(
        bytes,
        include_str!("fixtures/ensure.json").trim_end().as_bytes()
    );
    assert_eq!(decode::<Envelope<Request>>(&bytes).unwrap(), frame);
    let mut legacy: Value = serde_json::from_slice(&bytes).unwrap();
    legacy["protocol"] = json!("festerm-bridge-v1");
    assert_eq!(
        decode::<Envelope<Request>>(&serde_json::to_vec(&legacy).unwrap()),
        Err(ContractError::UnsupportedVersion)
    );
}

#[test]
fn host_wire_rejects_malformed_oversized_and_version_mismatched_input() {
    let original = serde_json::to_value(Envelope::new(
        id(1),
        id(3),
        Request::Ensure {
            creation: creation(),
        },
    ))
    .unwrap();
    for (field, value) in [
        ("version", json!(2)),
        ("protocol", json!("festerm-host-v2")),
        ("host", json!("00000001-0000-4000-8000-00000000000A")),
        ("operation", json!("00000000-0000-0000-0000-000000000000")),
        ("unrecognized", json!(true)),
    ] {
        let mut malformed = original.clone();
        malformed[field] = value;
        assert!(decode::<Envelope<Request>>(&serde_json::to_vec(&malformed).unwrap()).is_err());
    }
    let mut recipe = original.clone();
    recipe["body"]["creation"]["recipe"]["args"] = json!(vec!["x"; 33]);
    assert!(decode::<Envelope<Request>>(&serde_json::to_vec(&recipe).unwrap()).is_err());
    recipe["body"]["creation"]["recipe"]["args"] = json!(["x\u{0}"]);
    assert!(decode::<Envelope<Request>>(&serde_json::to_vec(&recipe).unwrap()).is_err());
    assert!(decode::<Envelope<Request>>(br#"{"version":1,"version":1}"#).is_err());
    assert!(decode::<Envelope<Request>>(&vec![b' '; CONTROL_FRAME_BYTES + 1]).is_err());
    let mut trailing = encode(&Envelope::new(id(1), id(3), Request::Capabilities)).unwrap();
    trailing.extend_from_slice(b"{}");
    assert!(decode::<Envelope<Request>>(&trailing).is_err());
    for generation in [
        "0",
        "01",
        "+1",
        "-1",
        "340282366920938463463374607431768211456",
    ] {
        assert!(serde_json::from_value::<GenerationId>(json!(generation)).is_err());
    }
    assert!(serde_json::from_value::<Epoch>(json!(0)).is_err());
}

#[test]
fn host_operations_roundtrip_without_conflating_detach_end_or_absence() {
    let target = session(2).target;
    let requests = [
        Request::Capabilities,
        Request::List { cursor: None },
        Request::Lookup { logical: id(2) },
        Request::Ensure {
            creation: creation(),
        },
        Request::Create {
            creation: creation(),
        },
        Request::Attach {
            target: target.clone(),
            attachment: id(4),
            takeover: false,
        },
        Request::Detach {
            target: target.clone(),
            attachment: id(4),
            lease: Some(id(5)),
            lease_epoch: Some(Epoch::new(1).unwrap()),
            reason: DetachKind::TransportLost,
        },
        Request::ExplicitEnd { target },
    ];
    for request in requests {
        let frame = Envelope::new(id(1), id(3), request);
        assert_eq!(
            decode::<Envelope<Request>>(&encode(&frame).unwrap()).unwrap(),
            frame
        );
    }
    for result in [
        LookupResult::Running {
            session: session(2),
        },
        LookupResult::Absent {
            logical: id(2),
            authorization_epoch: Epoch::new(1).unwrap(),
        },
        LookupResult::Denied,
        LookupResult::Offline,
        LookupResult::Unknown,
        LookupResult::Busy,
        LookupResult::Incompatible,
    ] {
        let frame = Envelope::new(id(1), id(3), Response::Lookup { result });
        assert_eq!(
            decode::<Envelope<Response>>(&encode(&frame).unwrap()).unwrap(),
            frame
        );
    }
}

#[test]
fn host_recipe_and_temporary_lifetime_reject_unsafe_or_unqualified_values() {
    let mut recipe = Recipe::default();
    recipe.args.push("-c".into());
    assert!(recipe.validate().is_err());
    recipe.shell = Some("/bin/sh".into());
    assert!(recipe.validate().is_ok());
    recipe.cwd = Some("/work\nother".into());
    assert!(recipe.validate().is_err());
    recipe.cwd = None;
    recipe.args = vec!["x".repeat(1024); 17];
    assert_eq!(recipe.validate(), Err(ContractError::Limit));
    assert!(Lifetime::Temporary {
        reconnect_grace_seconds: 60
    }
    .validate()
    .is_ok());
    for reconnect_grace_seconds in [0, 1, 59, 61, u16::MAX] {
        assert!(Lifetime::Temporary {
            reconnect_grace_seconds
        }
        .validate()
        .is_err());
    }
}

#[test]
fn host_inventory_filters_before_pagination_and_fences_foreign_or_revoked_cursors() {
    let epoch = Epoch::new(2).unwrap();
    let entries = (1..=35).rev().map(|n| InventoryEntry {
        owner: if n == 35 { id(9) } else { id(7) },
        shared: n != 34,
        revoked: n == 33,
        session: session(n),
    });
    let inventory = AuthorizedInventory::capture(id(1), id(7), epoch, id(8), entries).unwrap();
    let first = inventory.page(&id(1), &id(7), epoch, None).unwrap();
    first.validate().unwrap();
    assert_eq!(first.sessions.len(), PAGE_SIZE);
    assert_eq!(first.sessions[0].target.logical, id(1));
    let second = inventory
        .page(&id(1), &id(7), epoch, first.next.as_ref())
        .unwrap();
    second.validate().unwrap();
    assert_eq!(second.sessions.len(), PAGE_SIZE);
    assert_eq!(second.sessions.last().unwrap().target.logical, id(32));
    assert!(second.next.is_none());
    assert_eq!(
        inventory.page(&id(1), &id(9), epoch, first.next.as_ref()),
        Err(ContractError::Stale)
    );
    assert_eq!(
        inventory.page(&id(2), &id(7), epoch, None),
        Err(ContractError::Stale)
    );
    assert_eq!(
        inventory.page(&id(1), &id(7), epoch.next().unwrap(), None),
        Err(ContractError::Stale)
    );
    let forged = Cursor {
        snapshot: id(9),
        offset: 16,
    };
    assert_eq!(
        inventory.page(&id(1), &id(7), epoch, Some(&forged)),
        Err(ContractError::Stale)
    );
}

#[test]
fn host_aggregate_budgets_refuse_saturation_and_release_exact_permits() {
    let budget = Budgets::default();
    let creates = (0..MAX_PENDING_CREATES)
        .map(|_| budget.reserve(Resource::PendingCreate, 1).unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(
        budget.reserve(Resource::PendingCreate, 1),
        Err(ContractError::Busy)
    ));
    let output = budget
        .reserve(Resource::TerminalBytes, MAX_QUEUED_BYTES)
        .unwrap();
    assert!(matches!(
        budget.reserve(Resource::TerminalBytes, 1),
        Err(ContractError::Busy)
    ));
    // Output cannot consume control/revocation capacity.
    let control = budget
        .reserve(Resource::ControlBytes, MAX_CONTROL_BYTES)
        .unwrap();
    assert!(budget.reserve(Resource::Control, 1).is_ok());
    drop((creates, output, control));
    assert!(budget.reserve(Resource::PendingCreate, 1).is_ok());
    assert!(budget.reserve(Resource::TerminalBytes, usize::MAX).is_err());
    let held = std::thread::scope(|scope| {
        let handles = (0..12)
            .map(|_| {
                let budget = budget.clone();
                scope.spawn(move || budget.reserve(Resource::Client, 1).ok())
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(held.len(), MAX_CLIENTS);
    drop(held);
    assert!(budget.reserve(Resource::Client, 1).is_ok());
}

#[test]
fn host_admission_fence_rejects_revocation_replacement_and_operation_change() {
    let fence = AdmissionFence {
        host: id(1),
        principal: id(7),
        connection_epoch: Epoch::new(1).unwrap(),
        host_epoch: Epoch::new(2).unwrap(),
        grant_epoch: Epoch::new(3).unwrap(),
        operation_epoch: Epoch::new(4).unwrap(),
        operation: id(8),
        logical: Some(id(2)),
        target: Some(session(2).target),
    };
    fence.revalidate(&fence).unwrap();
    for changed in 0..9 {
        let mut current = fence.clone();
        match changed {
            0 => current.host = id(9),
            1 => current.principal = id(9),
            2 => current.connection_epoch = current.connection_epoch.next().unwrap(),
            3 => current.host_epoch = current.host_epoch.next().unwrap(),
            4 => current.grant_epoch = current.grant_epoch.next().unwrap(),
            5 => current.operation_epoch = current.operation_epoch.next().unwrap(),
            6 => current.operation = id(9),
            7 => current.logical = Some(id(9)),
            _ => current.target.as_mut().unwrap().generation = GenerationId::new(99).unwrap(),
        }
        assert_eq!(fence.revalidate(&current), Err(ContractError::Stale));
    }
    assert_eq!(
        Epoch::new(u64::MAX).unwrap().next(),
        Err(ContractError::Exhausted)
    );
}
