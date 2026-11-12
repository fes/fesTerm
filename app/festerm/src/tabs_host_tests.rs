use super::*;
use festerm_tunnel_bridge::{
    host_client::{ApprovedAttachment, ApprovedContext, Scope, Target},
    owner_auth::{OwnerBinding, PinnedHost},
};
use festerm_tunnel_client::host_control::{Binding, ConnectionPermit};

fn attachment() -> ApprovedAttachment {
    use russh::keys::{Algorithm, HashAlg, PrivateKey};
    let key = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    let public = key.public_key().to_openssh().unwrap();
    let approved = OwnerBinding {
        installation: uuid::Uuid::new_v4().to_string(),
        host: PinnedHost {
            host: uuid::Uuid::new_v4().to_string().try_into().unwrap(),
            host_key_fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string(),
        },
        account: "github:1234".into(),
        principal: uuid::Uuid::new_v4().to_string().try_into().unwrap(),
    };
    let binding = Binding::new(
        approved.host.host.clone(),
        approved.account.clone(),
        "owned-endpoint".into(),
    )
    .unwrap();
    let endpoint = serde_json::from_value(serde_json::json!({
        "connectionMode": "TunnelRelay", "hostId": "owned-host",
        "hostPublicKeys": [public.split_whitespace().nth(1).unwrap()],
        "clientRelayUri": "wss://example.invalid/client"
    }))
    .unwrap();
    let context = ApprovedContext::from_protected_approval(
        approved,
        festerm_secret_store::SecretReference::generate(),
        endpoint,
        22,
        zeroize::Zeroizing::new("fixture-token".into()),
        binding.clone(),
    )
    .unwrap();
    ApprovedAttachment::new(
        context,
        Scope {
            request: uuid::Uuid::new_v4().to_string().try_into().unwrap(),
            generation: 1,
            binding,
        },
        Target {
            logical: uuid::Uuid::new_v4().to_string().try_into().unwrap(),
            generation: "7".to_owned().try_into().unwrap(),
        },
    )
    .unwrap()
}

fn assert_no_host_work(state: &AppState) {
    assert!(state.host_account_bindings.is_empty());
    assert!(state
        .tabs
        .iter()
        .all(|tab| !matches!(tab.content, TabContent::Session(_))));
    let permits: Vec<_> = (0..4)
        .map(|_| ConnectionPermit::reserve(&state.host_client_budgets).unwrap())
        .collect();
    assert!(ConnectionPermit::reserve(&state.host_client_budgets).is_err());
    drop(permits);
}

#[test]
fn selected_host_desktop_origin_request_refuses_before_vault_network_or_new_tab() {
    for changed in 0..3 {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        state.open_profiles();
        let tab = state.active();
        let request = egui::Id::new("original-selected-host");
        crate::screens::install_host_recipe_request_for_test(&context, tab, request);
        let command = AppCommand::OpenApprovedHostFromProfiles {
            tab_id: if changed == 0 { TabId::next() } else { tab },
            request: if changed == 1 {
                egui::Id::new("stale")
            } else {
                request
            },
            attachment: Box::new(attachment()),
        };
        if changed == 2 {
            state.active_tab_mut().content = TabContent::Launcher;
        }
        state.dispatch(command, &context);
        assert_no_host_work(&state);
        assert!(crate::screens::host_recipe_request_is_current(
            &context, tab, request
        ));
    }
}

#[test]
fn selected_host_desktop_missing_account_is_not_an_empty_inventory_or_connection() {
    let context = egui::Context::default();
    let mut state = AppState::for_test();
    state.open_profiles();
    let tab = state.active();
    let request = egui::Id::new("original-selected-host");
    crate::screens::install_host_recipe_request_for_test(&context, tab, request);
    state.dispatch(
        AppCommand::OpenApprovedHostFromProfiles {
            tab_id: tab,
            request,
            attachment: Box::new(attachment()),
        },
        &context,
    );
    assert_no_host_work(&state);
    assert!(!crate::screens::host_recipe_request_is_current(
        &context, tab, request
    ));
}
