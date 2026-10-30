use super::*;
use festerm_config::Configuration;
use festerm_secret_store::MemorySecretStore;

#[test]
fn native_auth_dialog_keeps_instructions_on_one_line_and_within_the_viewport() {
    for (width, error) in [
        (1000.0, false),
        (360.0, false),
        (1000.0, true),
        (360.0, true),
    ] {
        let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
        let mut app = FesTermApp::for_test_with_configuration(
            Configuration::new(vec![profile.clone()]).unwrap(),
        );
        let (_sender, receiver) = mpsc::sync_channel(2);
        let mut pending = workflow(profile, receiver);
        pending.stage = if error {
            Stage::Error(Error::Response)
        } else {
            Stage::Device(DevicePrompt {
                user_code: "ABCD-1234".into(),
            })
        };
        let label = if error {
            Error::Response.to_string()
        } else {
            "Enter this code on GitHub:".into()
        };
        app.native_tunnel = Some(pending);
        app.overlays.native_tunnel = true;
        let context = egui::Context::default();
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0));
        let input = egui::RawInput {
            screen_rect: Some(viewport),
            ..Default::default()
        };
        let mut output = egui::FullOutput::default();
        for _ in 0..3 {
            output = context.run_ui(input.clone(), |ui| {
                app.show_native_tunnel(ui.ctx(), false);
            });
            output.textures_delta.clear();
        }
        let dialog = context
            .memory(|memory| memory.area_rect(egui::Id::new("native_dev_tunnel")))
            .expect("the sign-in dialog is visible");
        assert!(viewport.contains_rect(dialog));
        if width >= 1000.0 {
            assert!(dialog.width() >= 480.0, "dialog is too narrow: {dialog:?}");
        }
        let instruction = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some((text, shape.clip_rect))
                }
                _ => None,
            })
            .expect("the sign-in instruction is visible");
        assert_eq!(instruction.0.galley.rows.len(), 1);
        let text_rect = instruction
            .0
            .galley
            .rect
            .translate(instruction.0.pos.to_vec2());
        assert!(viewport.contains_rect(text_rect));
        assert!(instruction.1.contains_rect(text_rect));
        if !error {
            let service = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text)
                        if text.galley.job.text == "Sign in to Microsoft Dev Tunnels" =>
                    {
                        Some(text)
                    }
                    _ => None,
                })
                .expect("the service application is identified without extra instructions");
            assert_eq!(service.galley.rows.len(), 1);
        }
    }
}

#[test]
fn native_auth_device_code_button_copies_and_opens_github_only_on_explicit_activation() {
    for width in [1000.0, 360.0] {
        let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
        let mut app = FesTermApp::for_test_with_configuration(
            Configuration::new(vec![profile.clone()]).unwrap(),
        );
        let (_sender, receiver) = mpsc::sync_channel(2);
        let mut pending = workflow(profile, receiver);
        pending.stage = Stage::Device(DevicePrompt {
            user_code: "ABCD-1234".into(),
        });
        app.native_tunnel = Some(pending);
        app.overlays.native_tunnel = true;
        let context = egui::Context::default();
        let mut render = |events| {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 700.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.show_native_tunnel(ui.ctx(), false),
            );
            output.textures_delta.clear();
            output
        };
        let mut output = egui::FullOutput::default();
        for _ in 0..3 {
            output = render(Vec::new());
            assert!(output.platform_output.commands.is_empty());
        }
        let code = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "ABCD-1234" => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
            .expect("the code button is visible");
        let button = |pressed| egui::Event::PointerButton {
            pos: code,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let pressed = render(vec![egui::Event::PointerMoved(code), button(true)]);
        assert!(pressed.platform_output.commands.is_empty());
        let output = render(vec![button(false)]);
        assert_eq!(output.platform_output.commands.len(), 2);
        assert!(
            matches!(&output.platform_output.commands[0], egui::OutputCommand::CopyText(text) if text == "ABCD-1234")
        );
        assert!(
            matches!(&output.platform_output.commands[1], egui::OutputCommand::OpenUrl(url)
                if url.url == identity::VERIFICATION_URI && url.new_tab)
        );
        assert!(render(Vec::new()).platform_output.commands.is_empty());
        assert!(matches!(
            app.native_tunnel.as_ref().unwrap().stage,
            Stage::Device(_)
        ));
    }
}

#[test]
fn native_auth_identity_lease_refuses_concurrent_credential_rotation_until_final_owner_drops() {
    let reference = SecretReference::generate();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("native-tunnel-lock-tests")
        .join(reference.to_persisted_string());
    let lease = Arc::new(acquire_identity_lease(&root, &reference).unwrap());
    let completing_ui = Arc::clone(&lease);
    assert!(matches!(
        acquire_identity_lease(&root, &reference),
        Err(Error::Busy)
    ));
    drop(lease);
    assert!(matches!(
        acquire_identity_lease(&root, &reference),
        Err(Error::Busy)
    ));
    drop(completing_ui);
    let next = acquire_identity_lease(&root, &reference).unwrap();
    drop(next);
    std::fs::remove_dir_all(root).unwrap();
}

fn prepared(
    profile: &Profile,
    store: Arc<dyn SecretStore>,
    reference: SecretReference,
) -> Prepared {
    let destination = serde_json::from_value(serde_json::json!({
        "version":1,"tunnel_id":"owned","cluster":"usw3","port":47461,
        "target":{"name":"owned-shell","pid":77,"generation":"123"},
        "endpoint":{"connectionMode":"TunnelRelay","hostId":"owned-host","hostPublicKeys":["owned-pin"],
            "clientRelayUri":"wss://usw3.rel.tunnels.api.visualstudio.com/owned"}
    })).unwrap();
    Prepared {
        profile: Some(profile.clone()),
        launch: Launch::from_destination(
            destination,
            zeroize::Zeroizing::new("PRIVATE_CONNECT".into()),
        ),
        stored: Stored {
            store,
            references: vec![reference],
        },
    }
}
fn workflow(profile: Profile, receiver: mpsc::Receiver<Event>) -> Workflow {
    Workflow {
        expected: profile,
        cancel: CancellationToken::new(),
        receiver,
        stage: Stage::Working,
        connect: false,
        remember: true,
        focus_cancel: true,
        ready: None,
        _slot: None,
    }
}
fn ready_event(
    profile: &Profile,
    store: Arc<dyn SecretStore>,
    reference: &SecretReference,
) -> Event {
    Event::Complete(Ok(Box::new(prepared(
        profile,
        store,
        reference.duplicate_for_transport(),
    ))))
}

#[test]
fn native_auth_profile_changes_retire_only_their_own_vault_entries() {
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let identity = store
        .put(&SecretBytes::copy_from_slice(b"OWNED_IDENTITY"))
        .unwrap();
    let connect = store
        .put(&SecretBytes::copy_from_slice(b"OWNED_CONNECT"))
        .unwrap();
    let independent = store
        .put(&SecretBytes::copy_from_slice(b"OTHER_PROFILE"))
        .unwrap();
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3")
        .unwrap()
        .with_tunnel_identity(
            42,
            "owner".into(),
            identity.duplicate_for_transport(),
            connect.duplicate_for_transport(),
            DevTunnelHost {
                tunnel_id: "owned".into(),
                cluster: "usw3".into(),
                host_id: "host".into(),
                public_key: "pin".into(),
                port: 47461,
            },
        )
        .unwrap();
    let mut app =
        FesTermApp::for_test_with_configuration(Configuration::new(vec![profile.clone()]).unwrap());
    app.secret_store = Ok(Arc::clone(&store));
    app.retire_native_tunnel_credentials(Some(&profile));
    assert!(store.get(&identity).is_ok());
    assert!(store.get(&connect).is_ok());
    app.state.replace_configuration(
        Configuration::new(vec![
            Profile::named_dev_tunnel("owned", "changed.usw3").unwrap()
        ])
        .unwrap(),
    );
    app.retire_native_tunnel_credentials(Some(&profile));
    assert!(store.get(&identity).is_err());
    assert!(store.get(&connect).is_err());
    assert!(store.get(&independent).is_ok());
}

#[test]
fn native_auth_completion_is_discarded_after_profile_edit_without_publishing_credentials() {
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let mut app =
        FesTermApp::for_test_with_configuration(Configuration::new(vec![profile.clone()]).unwrap());
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let reference = store
        .put(&SecretBytes::copy_from_slice(b"PRIVATE_IDENTITY"))
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel(2);
    assert!(sender
        .send(ready_event(&profile, Arc::clone(&store), &reference))
        .is_ok());
    let pending = workflow(profile, receiver);
    let cancelled = pending.cancel.clone();
    app.native_tunnel = Some(pending);
    app.overlays.native_tunnel = true;
    app.state.replace_configuration(
        Configuration::new(vec![
            Profile::named_dev_tunnel("owned", "changed.usw3").unwrap()
        ])
        .unwrap(),
    );
    app.poll_native_tunnel(&egui::Context::default());
    assert!(app.native_tunnel.is_none());
    assert!(cancelled.is_cancelled());
    assert!(store.get(&reference).is_err());
    assert!(!app.overlays.native_tunnel);
    assert_eq!(
        app.state
            .configuration()
            .profile("owned")
            .unwrap()
            .as_dev_tunnel()
            .unwrap()
            .tunnel_name(),
        Some("changed.usw3")
    );
}

#[test]
fn native_auth_cancel_wins_before_ready_credentials_are_published() {
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let configuration = Configuration::new(vec![profile.clone()]).unwrap();
    let mut app = FesTermApp::for_test_with_configuration(configuration.clone());
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let reference = store
        .put(&SecretBytes::copy_from_slice(b"PRIVATE_IDENTITY"))
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel(2);
    assert!(sender
        .send(ready_event(&profile, Arc::clone(&store), &reference))
        .is_ok());
    app.native_tunnel = Some(workflow(profile, receiver));
    app.overlays.native_tunnel = true;
    let context = egui::Context::default();
    app.poll_native_tunnel(&context);
    assert!(app.native_tunnel.as_ref().unwrap().ready.is_some());
    let mut output = context.run_ui(Default::default(), |ui| {
        app.show_native_tunnel(ui.ctx(), true)
    });
    output.textures_delta.clear();
    assert!(app.native_tunnel.is_none());
    assert!(store.get(&reference).is_err());
    assert_eq!(app.state.configuration(), &configuration);
    assert!(!app.overlays.blocks_terminal_input());
}

#[test]
fn native_auth_refresh_publishes_only_the_bound_profile_identity_and_rejects_stale_rotation() {
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let reference = store
        .put(&SecretBytes::copy_from_slice(b"OLD_PAIR"))
        .unwrap();
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3")
        .unwrap()
        .with_tunnel_identity(
            42,
            "owner".into(),
            reference.duplicate_for_transport(),
            SecretReference::generate(),
            DevTunnelHost {
                tunnel_id: "owned".into(),
                cluster: "usw3".into(),
                host_id: "host".into(),
                public_key: "pin".into(),
                port: 47461,
            },
        )
        .unwrap();
    let other = Profile::named_dev_tunnel("independent", "another.usw3").unwrap();
    let configuration = Configuration::new(vec![profile.clone(), other.clone()]).unwrap();
    let mut app = FesTermApp::for_test_with_configuration(configuration.clone());
    app.secret_store = Ok(Arc::clone(&store));
    let (sender, receiver) = mpsc::sync_channel(2);
    let (published, mut publication) = oneshot::channel();
    assert!(sender
        .send(Event::Refresh {
            bytes: zeroize::Zeroizing::new(b"NEW_PAIR".to_vec()),
            published,
        })
        .is_ok());
    app.native_tunnel = Some(workflow(profile.clone(), receiver));
    app.poll_native_tunnel(&egui::Context::default());
    assert_eq!(publication.try_recv().unwrap(), Ok(()));
    store
        .get(&reference)
        .unwrap()
        .with_bytes(|bytes| assert_eq!(bytes, b"NEW_PAIR"));
    assert_eq!(app.state.configuration(), &configuration);
    assert_eq!(
        app.state.configuration().profile("independent"),
        Some(&other)
    );

    let (published, mut publication) = oneshot::channel();
    assert!(sender
        .send(Event::Refresh {
            bytes: zeroize::Zeroizing::new(b"STALE_PAIR".to_vec()),
            published,
        })
        .is_ok());
    app.state.replace_configuration(
        Configuration::new(vec![
            Profile::named_dev_tunnel("owned", "changed.usw3").unwrap(),
            other,
        ])
        .unwrap(),
    );
    app.poll_native_tunnel(&egui::Context::default());
    assert!(publication.try_recv().is_err());
    store
        .get(&reference)
        .unwrap()
        .with_bytes(|bytes| assert_eq!(bytes, b"NEW_PAIR"));
    assert!(app.native_tunnel.is_none());
}
