use super::*;
use festerm_config::{Configuration, DevTunnelAccount};
use festerm_secret_store::MemorySecretStore;

struct DeleteFailingStore(MemorySecretStore);

impl SecretStore for DeleteFailingStore {
    fn put(
        &self,
        secret: &SecretBytes,
    ) -> Result<SecretReference, festerm_secret_store::SecretStoreError> {
        self.0.put(secret)
    }

    fn get(
        &self,
        reference: &SecretReference,
    ) -> Result<SecretBytes, festerm_secret_store::SecretStoreError> {
        self.0.get(reference)
    }

    fn update(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), festerm_secret_store::SecretStoreError> {
        self.0.update(reference, secret)
    }

    fn delete(
        &self,
        _reference: &SecretReference,
    ) -> Result<bool, festerm_secret_store::SecretStoreError> {
        Err(festerm_secret_store::SecretStoreError::BackendFailure)
    }
}

#[test]
fn failed_tunnel_secret_cleanup_is_visible_on_terminal_and_automatic_reconnect() {
    for automatic in [false, true] {
        let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(DeleteFailingStore(MemorySecretStore::new()));
        let reference = store
            .put(&SecretBytes::copy_from_slice(b"fixture"))
            .unwrap();
        let account_reference = store
            .put(&SecretBytes::copy_from_slice(b"account"))
            .unwrap();
        let account = DevTunnelAccount::new(
            "account",
            AccountProvider::Github,
            42,
            "user",
            account_reference.duplicate_for_transport(),
        )
        .unwrap();
        let mut ready = prepared(
            &profile,
            Arc::clone(&store),
            reference.duplicate_for_transport(),
        );
        ready.stored.references.push(account_reference);
        ready.binding = Some(Binding {
            account: Some(account),
            connect: reference.duplicate_for_transport(),
            host: DevTunnelHost {
                tunnel_id: "owned".into(),
                cluster: "usw3".into(),
                host_id: "host".into(),
                public_key: "pin".into(),
                port: 47461,
            },
        });
        let (_sender, receiver) = mpsc::sync_channel(1);
        let mut pending = workflow(profile.clone(), receiver);
        pending.ready = Some(Box::new(ready));
        pending.automatic = automatic.then(Instant::now);
        let (mut app, _, _) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.state
            .replace_configuration(Configuration::new(vec![profile]).unwrap());
        app.native_tunnel = Some(pending);
        app.overlays.native_tunnel = true;
        let context = egui::Context::default();
        app.finish_native_tunnel(&context);
        assert!(!app.overlays.native_tunnel);
        assert!(store.get(&reference).is_ok());
        app.report_configuration_notice(crate::configuration_startup::BLOCKED_SAVE_MESSAGE);
        app.overlays.transient_notice = Some((
            "Unrelated terminal notice".into(),
            Instant::now() + Duration::from_secs(5),
        ));
        let text = &app.overlays.configuration_notice.as_ref().unwrap().0;
        assert!(text.contains("could not be removed"));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut output = egui::FullOutput::default();
        for _ in 0..3 {
            output = context.run_ui(input.clone(), |ui| app.show_transient_notice(ui.ctx()));
            output.textures_delta.clear();
        }
        assert!(output.shapes.iter().any(|shape| matches!(
            &shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("could not be removed")
        )));
    }
}

#[test]
fn tunnel_cancel_timeout_and_late_completion_report_cleanup_failure() {
    for mode in [
        "profile-change",
        "timeout",
        "cancel-queued",
        "cancel-late",
        "queue-full",
    ] {
        let context = egui::Context::default();
        let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
        let (mut app, _, _) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.state
            .replace_configuration(Configuration::new(vec![profile.clone()]).unwrap());
        let store: Arc<dyn SecretStore> = Arc::new(DeleteFailingStore(MemorySecretStore::new()));
        let reference = store
            .put(&SecretBytes::copy_from_slice(b"fixture"))
            .unwrap();
        let mut ready = prepared(
            &profile,
            Arc::clone(&store),
            reference.duplicate_for_transport(),
        );
        ready.stored.cleanup_reporter = CleanupReporter {
            failed: app.configuration_reloader.secret_cleanup_failure(),
            context: context.clone(),
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut pending = workflow(profile, receiver);
        if mode == "timeout" {
            pending.automatic = Some(Instant::now() - Duration::from_secs(1));
        }
        app.native_tunnel = Some(pending);
        if mode == "cancel-late" {
            app.cancel_native_tunnel();
            assert!(sender
                .try_send(Event::Complete(Ok(Box::new(ready))))
                .is_err());
        } else if mode == "queue-full" {
            assert!(sender
                .try_send(Event::Complete(Err(Error::Network)))
                .is_ok());
            assert!(sender
                .try_send(Event::Complete(Ok(Box::new(ready))))
                .is_err());
            app.cancel_native_tunnel();
        } else {
            assert!(sender
                .try_send(Event::Complete(Ok(Box::new(ready))))
                .is_ok());
            if mode == "profile-change" {
                app.state.replace_configuration(Configuration::empty());
            }
            if mode == "cancel-queued" {
                app.cancel_native_tunnel();
            } else {
                app.poll_native_tunnel(&context);
                assert!(app.native_tunnel.is_none(), "{mode}");
            }
        }
        assert!(store.get(&reference).is_ok(), "{mode}");
        app.frame_logic(&context);
        let notice = app.overlays.configuration_notice.as_ref().unwrap();
        assert!(notice.0.contains("could not be removed"), "{mode}");
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut output = egui::FullOutput::default();
        for _ in 0..3 {
            output = context.run_ui(input.clone(), |ui| app.show_transient_notice(ui.ctx()));
            output.textures_delta.clear();
        }
        assert!(output.shapes.iter().any(|shape| matches!(
            &shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("could not be removed")
        )), "{mode}");
    }
}

#[test]
fn tunnel_cleanup_after_secondary_close_wakes_primary() {
    let context = egui::Context::default();
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let mut primary =
        FesTermApp::for_test_with_configuration(Configuration::new(vec![profile.clone()]).unwrap());
    let (configuration, status, reloader, secret_store, documents) =
        primary.shared_application_services();
    let mut secondary = FesTermApp::secondary_window(
        &context,
        configuration,
        status,
        reloader,
        secret_store,
        documents,
    );
    let store: Arc<dyn SecretStore> = Arc::new(DeleteFailingStore(MemorySecretStore::new()));
    let reference = store
        .put(&SecretBytes::copy_from_slice(b"fixture"))
        .unwrap();
    let mut ready = prepared(
        &profile,
        Arc::clone(&store),
        reference.duplicate_for_transport(),
    );
    ready.stored.cleanup_reporter = CleanupReporter {
        failed: secondary.configuration_reloader.secret_cleanup_failure(),
        context: context.clone(),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    secondary.native_tunnel = Some(workflow(profile, receiver));
    drop(secondary);
    for _ in 0..4 {
        context
            .run_ui(egui::RawInput::default(), |_| {})
            .drop_without_applying_deltas();
    }
    assert!(!context.has_requested_repaint_for(&egui::ViewportId::ROOT));
    assert!(sender
        .try_send(Event::Complete(Ok(Box::new(ready))))
        .is_err());
    assert!(context.has_requested_repaint_for(&egui::ViewportId::ROOT));
    primary.frame_logic(&context);
    assert!(primary
        .overlays
        .configuration_notice
        .as_ref()
        .unwrap()
        .0
        .contains("could not be removed"));
    assert!(store.get(&reference).is_ok());
}

#[test]
fn tunnel_cleanup_in_closing_secondary_is_not_consumed_before_primary() {
    let context = egui::Context::default();
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let mut primary =
        FesTermApp::for_test_with_configuration(Configuration::new(vec![profile.clone()]).unwrap());
    let (configuration, status, reloader, secret_store, documents) =
        primary.shared_application_services();
    let mut secondary = FesTermApp::secondary_window(
        &context,
        configuration,
        status,
        reloader,
        secret_store,
        documents,
    );
    let store: Arc<dyn SecretStore> = Arc::new(DeleteFailingStore(MemorySecretStore::new()));
    let reference = store
        .put(&SecretBytes::copy_from_slice(b"fixture"))
        .unwrap();
    let mut ready = prepared(&profile, store, reference);
    ready.stored.cleanup_reporter = CleanupReporter {
        failed: secondary.configuration_reloader.secret_cleanup_failure(),
        context: context.clone(),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    secondary.native_tunnel = Some(workflow(profile, receiver));
    assert!(sender
        .try_send(Event::Complete(Ok(Box::new(ready))))
        .is_ok());
    secondary.cancel_native_tunnel();
    secondary.frame_logic(&context);
    drop(secondary);
    primary.frame_logic(&context);
    assert!(primary
        .overlays
        .configuration_notice
        .as_ref()
        .unwrap()
        .0
        .contains("could not be removed"));
}

#[test]
fn tunnel_cancel_cleans_queued_secret_without_failure_notice() {
    let context = egui::Context::default();
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let mut app =
        FesTermApp::for_test_with_configuration(Configuration::new(vec![profile.clone()]).unwrap());
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let reference = store
        .put(&SecretBytes::copy_from_slice(b"fixture"))
        .unwrap();
    let mut ready = prepared(
        &profile,
        Arc::clone(&store),
        reference.duplicate_for_transport(),
    );
    ready.stored.cleanup_reporter = CleanupReporter {
        failed: app.configuration_reloader.secret_cleanup_failure(),
        context: context.clone(),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    app.native_tunnel = Some(workflow(profile, receiver));
    assert!(sender
        .try_send(Event::Complete(Ok(Box::new(ready))))
        .is_ok());
    app.cancel_native_tunnel();
    app.frame_logic(&context);
    assert!(store.get(&reference).is_err());
    assert!(app.overlays.configuration_notice.is_none());
}

#[test]
fn tunnel_panic_after_install_retains_new_secrets_and_poison_blocks_other_saves() {
    use crate::configuration_startup::{
        ConfigurationLoadFailure, ConfigurationReloader, SessionNameConfigurationFixture,
    };
    let fixture = SessionNameConfigurationFixture::new();
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let configuration = Configuration::new(vec![profile.clone()]).unwrap();
    configuration.save_to_path(&fixture.path).unwrap();
    let reloader = ConfigurationReloader::from_path_for_test(fixture.path.clone());
    let mut app = FesTermApp::for_test_with_configuration(configuration.clone());
    app.configuration_reloader = reloader.clone().with_panic_after_installation_for_test();
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let connect = store
        .put(&SecretBytes::copy_from_slice(b"connect"))
        .unwrap();
    let identity = store
        .put(&SecretBytes::copy_from_slice(b"identity"))
        .unwrap();
    let account = DevTunnelAccount::new(
        "owner",
        AccountProvider::Github,
        42,
        "user",
        identity.duplicate_for_transport(),
    )
    .unwrap();
    let mut ready = prepared(
        &profile,
        Arc::clone(&store),
        connect.duplicate_for_transport(),
    );
    ready
        .stored
        .references
        .push(identity.duplicate_for_transport());
    ready.binding = Some(Binding {
        account: Some(account),
        connect: connect.duplicate_for_transport(),
        host: DevTunnelHost {
            tunnel_id: "owned".into(),
            cluster: "usw3".into(),
            host_id: "host".into(),
            public_key: "pin".into(),
            port: 47461,
        },
    });
    let (_sender, receiver) = mpsc::sync_channel(1);
    let mut pending = workflow(profile, receiver);
    pending.ready = Some(Box::new(ready));
    app.native_tunnel = Some(pending);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.finish_native_tunnel(&egui::Context::default());
    }));
    assert!(result.is_err());
    let installed = Configuration::load_from_path(&fixture.path).unwrap();
    assert!(installed.profile("owned").unwrap().credential_reference() == Some(&connect));
    assert!(installed.account("owner").unwrap().credential_reference() == &identity);
    assert!(store.get(&connect).is_ok() && store.get(&identity).is_ok());
    assert_eq!(
        reloader.save_configuration(&configuration),
        Err(ConfigurationLoadFailure::ReconciliationRequired)
    );
    assert_eq!(
        Configuration::load_from_path(&fixture.path).unwrap(),
        installed
    );
}

#[test]
fn tunnel_blocked_save_never_deletes_shared_or_fresh_candidate_references() {
    use crate::configuration_startup::{
        ConfigurationLoadFailure, ConfigurationReloader, SessionNameConfigurationFixture,
    };
    for busy in [false, true] {
        let fixture = SessionNameConfigurationFixture::new();
        let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let shared = store.put(&SecretBytes::copy_from_slice(b"shared")).unwrap();
        let fresh = store.put(&SecretBytes::copy_from_slice(b"fresh")).unwrap();
        let account = DevTunnelAccount::new(
            "owner",
            AccountProvider::Github,
            42,
            "user",
            shared.duplicate_for_transport(),
        )
        .unwrap();
        let configuration = Configuration::new(vec![profile.clone()])
            .unwrap()
            .with_account(account.clone())
            .unwrap()
            .0;
        configuration.save_to_path(&fixture.path).unwrap();
        let reloader = ConfigurationReloader::from_path_for_test(fixture.path.clone());
        let reloader = if busy {
            reloader.with_save_failure_for_test(ConfigurationLoadFailure::SaveBusy)
        } else {
            let first = reloader
                .clone()
                .with_save_failure_for_test(ConfigurationLoadFailure::OutcomeUnknown);
            assert_eq!(
                first.save_configuration(&configuration),
                Err(ConfigurationLoadFailure::OutcomeUnknown)
            );
            reloader
        };
        let context = egui::Context::default();
        let mut primary = FesTermApp::for_test_with_configuration(configuration.clone());
        primary.configuration_reloader = reloader;
        let (config, status, reloader, secret_store, documents) =
            primary.shared_application_services();
        let mut app = FesTermApp::secondary_window(
            &context,
            config,
            status,
            reloader,
            secret_store,
            documents,
        );
        let mut ready = prepared(
            &profile,
            Arc::clone(&store),
            fresh.duplicate_for_transport(),
        );
        ready
            .stored
            .references
            .push(shared.duplicate_for_transport());
        ready.binding = Some(Binding {
            account: None,
            connect: fresh.duplicate_for_transport(),
            host: DevTunnelHost {
                tunnel_id: "owned".into(),
                cluster: "usw3".into(),
                host_id: "host".into(),
                public_key: "pin".into(),
                port: 47461,
            },
        });
        let (_sender, receiver) = mpsc::sync_channel(1);
        let mut pending = workflow(profile, receiver);
        pending.account = Some(account);
        pending.ready = Some(Box::new(ready));
        app.native_tunnel = Some(pending);
        app.finish_native_tunnel(&context);
        assert!(store.get(&shared).is_ok() && store.get(&fresh).is_ok());
        assert_eq!(
            Configuration::load_from_path(&fixture.path).unwrap(),
            configuration
        );
        assert!(app
            .overlays
            .configuration_notice
            .as_ref()
            .unwrap()
            .0
            .contains("retained"));
        drop(app);
        primary.frame_logic(&context);
        assert!(primary
            .overlays
            .configuration_notice
            .as_ref()
            .unwrap()
            .0
            .contains("candidate Dev Tunnel workflow credentials"));
    }
}

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
                    assert!(text.galley.job.sections.iter().all(|section| {
                        section.format.font_id.family == egui::FontFamily::Monospace
                            && section.format.color == festerm_ui_egui::theme::TEXT_ON_ACCENT
                    }));
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
    _profile: &Profile,
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
        binding: None,
        launch: Launch::from_destination(
            destination,
            zeroize::Zeroizing::new("PRIVATE_CONNECT".into()),
        ),
        reclaim: false,
        stored: Stored {
            store,
            references: vec![reference],
            cleanup_reporter: CleanupReporter {
                failed: Arc::default(),
                context: egui::Context::default(),
            },
        },
    }
}
fn workflow(profile: Profile, receiver: mpsc::Receiver<Event>) -> Workflow {
    Workflow {
        expected: profile,
        account: None,
        cancel: CancellationToken::new(),
        receiver,
        stage: Stage::Working,
        connect: false,
        remember: true,
        ephemeral: false,
        takeover: true,
        focus_cancel: true,
        ready: None,
        reconnect: None,
        automatic: None,
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
        .with_tunnel_account(
            "github-owner",
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
    let (configuration, _) = Configuration::new(Vec::new())
        .unwrap()
        .with_account(
            DevTunnelAccount::new(
                "github-owner",
                festerm_config::AccountProvider::Github,
                42,
                "owner",
                identity.duplicate_for_transport(),
            )
            .unwrap(),
        )
        .unwrap();
    let mut app = FesTermApp::for_test_with_configuration(
        configuration.with_profile(profile.clone()).unwrap(),
    );
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
    assert!(store.get(&identity).is_ok());
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
        .with_tunnel_account(
            "github-owner",
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
    let account = DevTunnelAccount::new(
        "github-owner",
        festerm_config::AccountProvider::Github,
        42,
        "owner",
        reference.duplicate_for_transport(),
    )
    .unwrap();
    let configuration = Configuration::new(Vec::new())
        .unwrap()
        .with_account(account.clone())
        .unwrap()
        .0
        .with_profile(profile.clone())
        .unwrap()
        .with_profile(other.clone())
        .unwrap();
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
    let mut pending = workflow(profile.clone(), receiver);
    pending.account = Some(account);
    app.native_tunnel = Some(pending);
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

#[test]
fn remembered_sign_ins_for_one_github_user_share_a_single_account() {
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let host = || DevTunnelHost {
        tunnel_id: "owned".into(),
        cluster: "usw3".into(),
        host_id: "host".into(),
        public_key: "pin".into(),
        port: 47461,
    };
    let one = Profile::named_dev_tunnel("one", "one.usw3").unwrap();
    let two = Profile::named_dev_tunnel("two", "two.usw3").unwrap();
    let mut app = FesTermApp::for_test_with_configuration(
        Configuration::new(vec![one.clone(), two.clone()]).unwrap(),
    );
    app.secret_store = Ok(Arc::clone(&store));
    let directory = tempfile::Builder::new()
        .prefix("tunnel-account-tests-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    app.configuration_reloader =
        crate::configuration_startup::ConfigurationReloader::from_path_for_test(
            directory.path().join("config.toml"),
        );
    let sign_in = |bytes: &[u8]| {
        let reference = store.put(&SecretBytes::copy_from_slice(bytes)).unwrap();
        DevTunnelAccount::new(
            "github-owner",
            festerm_config::AccountProvider::Github,
            42,
            "owner",
            reference,
        )
        .unwrap()
    };
    let first = sign_in(b"FIRST");
    let first_reference = first.credential_reference().duplicate_for_transport();
    let binding = |account| Binding {
        account,
        connect: SecretReference::generate(),
        host: host(),
    };
    assert!(app.save_tunnel_binding(&one, None, binding(Some(first))));
    let account = app
        .state
        .configuration()
        .account("github-owner")
        .unwrap()
        .clone();
    // The second profile reuses the account: no new sign-in, no new account.
    assert!(app.save_tunnel_binding(&two, Some(&account), binding(None)));
    let configuration = app.state.configuration();
    assert_eq!(configuration.accounts().len(), 1);
    assert_eq!(configuration.account_profile_count("github-owner"), 2);
    // Signing in again replaces the credential and retires the old one.
    assert!(app.save_tunnel_binding(&one, None, binding(Some(sign_in(b"SECOND")))));
    let configuration = app.state.configuration();
    assert_eq!(configuration.accounts().len(), 1);
    assert_eq!(configuration.account_profile_count("github-owner"), 2);
    assert!(store.get(&first_reference).is_err());
}

#[test]
fn form_connect_without_save_as_is_ephemeral_and_keeps_the_choice_of_takeover() {
    let mut app = FesTermApp::for_test_with_configuration(Configuration::new(Vec::new()).unwrap());
    let context = egui::Context::default();
    app.connect_dev_tunnel_form("owned.usw3".into(), None, false, None, &context);
    let workflow = app.native_tunnel.as_ref().expect("a connection started");
    assert!(workflow.ephemeral && !workflow.takeover);
    assert!(
        app.state.configuration().profiles().is_empty(),
        "connecting once must not create a profile"
    );
}

#[test]
fn a_client_id_goes_only_to_hosts_that_advertise_reclaim() {
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let reference = store.put(&SecretBytes::copy_from_slice(b"x")).unwrap();
    let client = ClientId::new();
    let old_host = connection_launch(
        prepared(
            &profile,
            Arc::clone(&store),
            reference.duplicate_for_transport(),
        )
        .launch,
        true,
        false,
        &client,
    );
    assert!(old_host.takeover() && old_host.client().is_none());
    let reclaiming = connection_launch(
        prepared(&profile, store, reference).launch,
        false,
        true,
        &client,
    );
    assert!(!reclaiming.takeover());
    assert_eq!(reclaiming.client(), Some(&client));
}

#[test]
fn an_automatic_tunnel_reconnect_reclaims_only_and_gives_up_quietly() {
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let mut app = FesTermApp::for_test_with_configuration(Configuration::new(Vec::new()).unwrap());
    let context = egui::Context::default();
    let tab = app
        .state
        .place_tunnel_tab_for_test(TunnelOrigin::Ephemeral(Box::new(profile)));
    let client = app.state.tunnel_tab(tab).unwrap().client.clone();
    app.state.drop_tunnel_for_test(tab, "transport closed");
    app.drive_tunnel_reconnects(&context);
    let workflow = app
        .native_tunnel
        .as_mut()
        .expect("an automatic reconnect started");
    assert!(
        !workflow.takeover,
        "an automatic reconnect never takes over"
    );
    assert_eq!(workflow.reconnect, Some((tab, client)));
    assert!(workflow.automatic.is_some() && !app.overlays.native_tunnel);
    // Anything that would need the user ends the attempt without a word.
    workflow.stage = Stage::Error(Error::Network);
    app.poll_native_tunnel(&context);
    assert!(app.native_tunnel.is_none());
    assert!(app.secure_storage_feedback.is_none());
    // One attempt per activation: the still-lost tab is not retried.
    app.drive_tunnel_reconnects(&context);
    assert!(app.native_tunnel.is_none());
}

#[test]
fn a_saved_profile_connects_without_keeping_its_sign_in_when_secure_storage_is_unavailable() {
    let profile = Profile::named_dev_tunnel("owned", "owned.usw3").unwrap();
    let mut app =
        FesTermApp::for_test_with_configuration(Configuration::new(vec![profile]).unwrap());
    app.secret_store = Err(festerm_secret_store::SecretStoreError::LockedOrUnavailable);
    let context = egui::Context::default();
    app.begin_native_tunnel("owned".into(), false, true, true, &context);
    let workflow = app
        .native_tunnel
        .as_ref()
        .expect("the saved profile still connects");
    assert!(workflow.connect && !workflow.remember && !workflow.ephemeral);
    assert_eq!(
        app.secure_storage_feedback,
        Some("Secure storage unavailable. Connecting without keeping the sign-in.")
    );
}
