//! Files profile metadata and explicit foreground opening; no shell actions.
use super::{files_shell::HostFilesTarget, *};
use festerm_host_protocol::{HostId, PrincipalId, Validate};
use festerm_mobile_profiles::DevTunnelFilesProfile;
use festerm_tunnel_bridge::{ordinary_auth::OrdinaryHostInfo, owner_auth::PinnedHost};

/// Stable identity only. Policy epochs and live authority are never cached here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FilesHostIdentity {
    pub(super) host: PinnedHost,
    pub(super) installation: HostId,
    pub(super) principal: PrincipalId,
}
impl From<&OrdinaryHostInfo> for FilesHostIdentity {
    fn from(info: &OrdinaryHostInfo) -> Self {
        Self {
            host: info.route.host.clone(),
            installation: info.installation.clone(),
            principal: info.principal.clone(),
        }
    }
}
impl FilesHostIdentity {
    pub(super) fn as_expected_host(
        &self,
    ) -> festerm_tunnel_bridge::host_client::ordinary::ExpectedHost {
        festerm_tunnel_bridge::host_client::ordinary::ExpectedHost {
            host: self.host.clone(),
            installation: self.installation.clone(),
            principal: self.principal.clone(),
        }
    }
    pub(super) fn matches(&self, current: &OrdinaryHostInfo) -> bool {
        self.host == current.route.host
            && self.installation == current.installation
            && self.principal == current.principal
    }
}
impl TryFrom<&DevTunnelFilesProfile> for FilesHostIdentity {
    type Error = String;
    fn try_from(value: &DevTunnelFilesProfile) -> Result<Self, String> {
        let invalid = || "The saved Files host is invalid. Choose the host again.".to_owned();
        let host = PinnedHost {
            host: value.host_id.parse().map_err(|_| invalid())?,
            host_key_fingerprint: value.host_key_fingerprint.clone(),
        };
        host.validate().map_err(|_| invalid())?;
        Ok(Self {
            host,
            installation: value.installation.parse().map_err(|_| invalid())?,
            principal: value.principal.parse().map_err(|_| invalid())?,
        })
    }
}

fn destination(target: &HostFilesTarget) -> DevTunnelFilesProfile {
    DevTunnelFilesProfile {
        account: target.account.id.clone(),
        locator: target.locator.clone(),
        host_id: target.expected.host.host.to_string(),
        host_key_fingerprint: target.expected.host.host_key_fingerprint.clone(),
        installation: target.expected.installation.to_string(),
        principal: target.expected.principal.to_string(),
    }
}

impl MobileApp {
    pub(super) fn show_host_files_profile_control(&mut self, ui: &mut egui::Ui) {
        let Some(target) = self.files_ctx.host.as_ref() else {
            return;
        };
        let existing = target
            .profile
            .as_ref()
            .and_then(|id| self.profiles.as_ref()?.host_files_spec(id))
            .filter(|spec| spec.destination == destination(target))
            .is_some();
        let ready = self.files.as_ref().is_some_and(|session| {
            session.is_ready()
                && session.deletion.is_none()
                && session.folder.is_none()
                && !session.transfers_sheet
                && session.conflict().is_none()
        });
        let save = ui.add_enabled(
            ready,
            egui::Button::new(if existing {
                "Edit Files profile"
            } else {
                "Save as profile"
            })
            .min_size(egui::vec2(0.0, 44.0)),
        );
        if save.clicked() {
            self.begin_host_files_profile();
            ui.close();
        }
    }

    fn begin_host_files_profile(&mut self) {
        let Some(target) = self.files_ctx.host.clone() else {
            return;
        };
        if !self
            .files
            .as_ref()
            .is_some_and(|session| session.is_ready())
        {
            return;
        }
        let saved = target
            .profile
            .as_ref()
            .and_then(|id| self.profiles.as_ref()?.host_files_spec(id))
            .filter(|spec| spec.destination == destination(&target));
        self.close_editor();
        if let Some(spec) = saved {
            self.editor.open_edit_host_files(&spec, self.caps);
        } else {
            self.editor.open_new();
            self.editor.name = target.profile_name_hint.clone().unwrap_or_else(|| {
                self.profiles
                    .as_ref()
                    .map(|p| p.suggest_name(&target.label))
                    .unwrap_or_else(|| target.label.clone())
            });
            self.editor.files_destination = Some(destination(&target));
            self.editor.files_account_label = format!("@{}", target.account.login);
            self.editor.kind_index = forms::EditorState::kinds(self.caps)
                .iter()
                .position(|kind| *kind == model::ProfileKind::DevTunnelFiles)
                .unwrap_or(0);
        }
        self.editor.files_from_session = true;
        self.editor.files_origin_profile = target.profile;
    }

    pub(super) fn bind_saved_host_files(&mut self, id: &str, name: &str) {
        let Some(spec) = self.profiles.as_ref().and_then(|p| p.host_files_spec(id)) else {
            return;
        };
        if let Some(target) = self.files_ctx.host.as_mut() {
            let explicitly_saved_as = self.editor.files_from_session
                && self.editor.editing.is_none()
                && target.profile == self.editor.files_origin_profile;
            if (explicitly_saved_as
                || target
                    .profile
                    .as_deref()
                    .is_none_or(|current| current == id))
                && destination(target) == spec.destination
            {
                target.profile = Some(id.to_owned());
                target.label = name.to_owned();
                if let Some(session) = self.files.as_mut() {
                    session.title = name.to_owned();
                }
            }
        }
    }

    fn saved_host_files_target(&self, id: &str) -> Result<HostFilesTarget, String> {
        let profiles = self
            .profiles
            .as_ref()
            .ok_or("Saved profiles are unavailable")?;
        let spec = profiles
            .host_files_spec(id)
            .ok_or("The Files profile is no longer available")?;
        let expected = FilesHostIdentity::try_from(&spec.destination)?;
        let store = profiles.store_handle();
        let account = store
            .lock()
            .map_err(|_| "Account storage is unavailable")?
            .accounts()
            .iter()
            .find(|account| account.id == spec.destination.account)
            .cloned()
            .ok_or("Sign in to this Files profile’s original account in Settings")?;
        Ok(HostFilesTarget {
            account,
            locator: spec.destination.locator,
            label: spec.name,
            expected,
            profile: Some(spec.profile.as_str().to_owned()),
            profile_name_hint: None,
        })
    }

    pub(super) fn check_saved_host_files_target(
        &self,
        target: &HostFilesTarget,
    ) -> Result<(), String> {
        if let Some(id) = &target.profile {
            let current = self
                .profiles
                .as_ref()
                .and_then(|p| p.host_files_spec(id))
                .ok_or(
                "The saved Files profile was removed. Close this view or save it as a new profile.",
            )?;
            if current.destination != destination(target) {
                return Err(
                    "The saved Files destination changed. Open its current profile separately."
                        .into(),
                );
            }
        }
        Ok(())
    }

    pub(super) fn open_saved_host_files(
        &mut self,
        ctx: &egui::Context,
        id: &str,
    ) -> Result<(), String> {
        let target = self.saved_host_files_target(id)?;
        // Always prepare fresh Files authority. No host-browser inventory,
        // cached approval, terminal attachment or transfer replay is used.
        self.open_host_files_target(target, ctx)?;
        if let Some(profiles) = self.profiles.as_mut() {
            profiles.mark_used(id);
        }
        self.notice = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_mobile_profiles::{ProfileKind as StoredKind, SshProfile};
    use std::{cell::Cell, rc::Rc};

    fn fixture() -> (MobileApp, String, DevTunnelFilesProfile) {
        let profiles = profiles::testing::profiles();
        let store = profiles.store_handle();
        let account = festerm_mobile_tunnels::testing::seed_account(&store, 7, "owner", false);
        let value = DevTunnelFilesProfile {
            account,
            locator: "controlled.fixture".into(),
            host_id: HostId::new().to_string(),
            host_key_fingerprint: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            installation: HostId::new().to_string(),
            principal: PrincipalId::new().to_string(),
        };
        let mut app = MobileApp::new(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })))
        .with_profiles(profiles);
        let id = app
            .profiles
            .as_mut()
            .unwrap()
            .save_host_files(None, "Remote files", value.clone())
            .unwrap();
        (app, id, value)
    }

    #[test]
    fn saved_files_profile_edit_is_offline_and_stale_kind_never_overwrites_another_profile() {
        let (mut app, id, _) = fixture();
        app.edit_profile(&id);
        assert_eq!(
            app.editor.kind(app.caps),
            model::ProfileKind::DevTunnelFiles
        );
        assert!(
            app.editor.auth.is_none()
                && app.editor.host.is_empty()
                && app.editor.session_name.is_empty()
        );
        assert_eq!(app.editor.files_account_label, "@owner");
        app.editor.name = "Renamed files".into();
        assert_eq!(app.save_editor_profile().as_deref(), Some(id.as_str()));
        assert!(app.files.is_none() && app.tabs.is_empty());
        let profiles = app.profiles.as_ref().unwrap();
        assert!(
            profiles.sftp_spec(&id).is_none()
                && profiles.ssh_profile(&id).is_none()
                && profiles.tunnel_spec(&id).is_none()
        );
        let store = profiles.store_handle();
        let parsed = festerm_mobile_profiles::ProfileId::parse(&id).unwrap();
        assert_eq!(
            store.lock().unwrap().get(&parsed).unwrap().last_used_at,
            None
        );
        let mut changed = app.editor.files_destination.clone().unwrap();
        changed.locator = "changed.fixture".into();
        store
            .lock()
            .unwrap()
            .update(
                &parsed,
                "Changed destination",
                StoredKind::DevTunnelFiles(changed.clone()),
            )
            .unwrap();
        assert!(app.save_editor_profile().is_none());
        assert_eq!(
            app.profiles
                .as_ref()
                .unwrap()
                .host_files_spec(&id)
                .unwrap()
                .destination,
            changed
        );
        store
            .lock()
            .unwrap()
            .update(
                &parsed,
                "Other SSH",
                StoredKind::Ssh(SshProfile::new("other.test", "user")),
            )
            .unwrap();
        app.editor.name = "Do not overwrite".into();
        assert!(app.save_editor_profile().is_none());
        assert_eq!(
            store.lock().unwrap().get(&parsed).unwrap().name,
            "Other SSH"
        );
        assert!(matches!(
            store.lock().unwrap().get(&parsed).unwrap().kind,
            StoredKind::Ssh(_)
        ));
    }

    #[test]
    fn saved_files_target_never_substitutes_another_signed_in_account() {
        let (mut app, id, value) = fixture();
        let target = app.saved_host_files_target(&id).unwrap();
        assert_eq!(target.account.id, value.account);
        assert_eq!(destination(&target), value);
        let store = app.profiles.as_ref().unwrap().store_handle();
        festerm_mobile_tunnels::testing::seed_account(&store, 8, "unrelated", false);
        store
            .lock()
            .unwrap()
            .remove_account(&value.account)
            .unwrap();
        assert!(app
            .saved_host_files_target(&id)
            .err()
            .unwrap()
            .contains("original account"));
        app.connect_profile(&egui::Context::default(), &id);
        assert!(app.notice.as_ref().unwrap().contains("original account"));
        assert!(app.files.is_none() && app.tabs.is_empty());
        assert_eq!(
            app.profiles
                .as_ref()
                .unwrap()
                .host_files_spec(&id)
                .unwrap()
                .destination,
            value
        );
    }

    #[test]
    fn saved_files_retry_keeps_destination_but_refuses_removed_or_changed_shortcut() {
        let (mut app, id, original) = fixture();
        let target = app.saved_host_files_target(&id).unwrap();
        app.profiles
            .as_mut()
            .unwrap()
            .save_host_files(Some(&id), "Renamed only", original.clone())
            .unwrap();
        app.check_saved_host_files_target(&target).unwrap();
        let store = app.profiles.as_ref().unwrap().store_handle();
        let parsed = festerm_mobile_profiles::ProfileId::parse(&id).unwrap();
        let mut changed = original.clone();
        changed.locator = "changed.fixture".into();
        store
            .lock()
            .unwrap()
            .update(&parsed, "Changed", StoredKind::DevTunnelFiles(changed))
            .unwrap();
        assert!(app.check_saved_host_files_target(&target).is_err());
        assert_eq!(destination(&target), original);
        store.lock().unwrap().delete(&parsed).unwrap();
        let ctx = egui::Context::default();
        let mut rendered = ctx.run_ui(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |_| {},
        );
        rendered.textures_delta.clear();
        app.files_ctx.host = Some(target);
        app.retry_files(&ctx);
        assert!(
            app.notice.as_ref().unwrap().contains("removed"),
            "must refuse before identity/vault preparation"
        );
        assert!(app.files.is_none() && app.tabs.is_empty());
        assert_eq!(
            app.files_ctx.host.as_ref().unwrap().profile.as_deref(),
            Some(id.as_str()),
            "never quietly downgrade a saved-origin retry"
        );
    }

    #[test]
    fn saving_a_new_shortcut_rebinds_only_after_explicit_save_and_preserves_typed_name() {
        for width in [360.0, 375.0] {
            use egui_kittest::{kittest::Queryable, Harness};
            use files_fixture::{fixture_entry, FixtureFs, FixtureTransfers};
            use std::sync::Arc;
            let (mut app, id, _) = fixture();
            let mut target = app.saved_host_files_target(&id).unwrap();
            target.profile_name_hint = Some("Typed name".into());
            let parsed = festerm_mobile_profiles::ProfileId::parse(&id).unwrap();
            app.profiles
                .as_ref()
                .unwrap()
                .store_handle()
                .lock()
                .unwrap()
                .delete(&parsed)
                .unwrap();
            let fs = |kind| {
                Arc::new(FixtureFs {
                    kind,
                    home: "/",
                    entries: (0..10)
                        .map(|index| {
                            fixture_entry(
                                &format!(
                                    "{}-{index}.txt",
                                    if kind == festerm_sftp::PaneKind::Remote {
                                        "remote"
                                    } else {
                                        "local"
                                    }
                                ),
                                false,
                                5,
                                1,
                            )
                        })
                        .collect(),
                })
            };
            app.files = Some(Box::new(files::FilesSession::ready(
                "Files".into(),
                "Host".into(),
                files::Ready {
                    remote_write: true,
                    remote: fs(festerm_sftp::PaneKind::Remote),
                    local: fs(festerm_sftp::PaneKind::Local),
                    local_root: "/nonexistent-festerm-test".into(),
                    transfers: Arc::new(FixtureTransfers::new(vec![])),
                    start_dir: None,
                },
                &egui::Context::default(),
            )));
            app.files_ctx.host = Some(target);
            app.files_ctx.only = true;
            app.surface = Surface::Terminal;
            app.workspace = workspaces::Workspace::Files;
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, 853.0))
                .build_ui_state(
                    |ui, app: &mut MobileApp| app.show_with_occlusion(ui, 0.0),
                    app,
                );
            harness.run_steps(5);
            let remote = harness.get_by_label("New remote folder").rect();
            let local = harness.get_by_label("New local folder").rect();
            for rect in [remote, local] {
                assert!(rect.width() >= 44.0 && rect.height() >= 44.0);
                assert!(rect.left() >= 16.0 && rect.right() <= width - 16.0);
            }
            let upload = harness.get_by_label("Upload").rect();
            let download = harness.get_by_label("Download").rect();
            assert_eq!(upload.top(), download.top(), "transfers share one row");
            assert!(
                harness.get_by_label("remote-2.txt").rect().bottom() <= upload.top(),
                "three complete remote rows at {width}"
            );
            assert!(
                harness.get_by_label("local-2.txt").rect().bottom()
                    <= harness.get_by_label("Transfers").rect().top(),
                "three complete local rows at {width}"
            );
            assert!(
                harness.query_by_label("Save as profile").is_none(),
                "Save must not consume list space"
            );
            harness.get_by_label("More").click();
            harness.run_steps(3);
            let save = harness.get_by_label("Save as profile");
            assert!(save.rect().height() >= 44.0);
            save.click();
            harness.run_steps(3);
            let app = harness.state_mut();
            assert_eq!(app.editor.name, "Typed name");
            assert!(app.editor.editing.is_none());
            app.close_editor();
            assert_eq!(
                app.files_ctx.host.as_ref().unwrap().profile.as_deref(),
                Some(id.as_str())
            );
            assert!(app
                .check_saved_host_files_target(app.files_ctx.host.as_ref().unwrap())
                .is_err());
            app.begin_host_files_profile();
            let new_id = app.save_editor_profile().unwrap();
            assert_ne!(new_id, id);
            let target = app.files_ctx.host.as_ref().unwrap();
            assert_eq!(target.profile.as_deref(), Some(new_id.as_str()));
            assert_eq!(target.label, "Typed name");
            app.check_saved_host_files_target(target).unwrap();
            assert!(app.tabs.is_empty());
        }
    }

    #[test]
    fn saved_files_identity_pins_key_host_installation_and_principal_but_not_epoch() {
        use festerm_tunnel_bridge::ordinary_auth::{OrdinaryRoute, ORDINARY_VERSION};
        let (_, _, value) = fixture();
        let expected = FilesHostIdentity::try_from(&value).unwrap();
        let info = OrdinaryHostInfo {
            version: ORDINARY_VERSION,
            route: OrdinaryRoute {
                tunnel_id: "controlled".into(),
                cluster: "fixture".into(),
                endpoint_id: format!("{}-relay", expected.host.host),
                port: 22,
                host: expected.host.clone(),
            },
            installation: expected.installation.clone(),
            principal: expected.principal.clone(),
            policy_epoch: festerm_host_protocol::Epoch::new(1).unwrap(),
        };
        assert!(expected.matches(&info));
        let mut changed = info.clone();
        changed.policy_epoch = festerm_host_protocol::Epoch::new(2).unwrap();
        assert!(
            expected.matches(&changed),
            "reopening requires a fresh epoch, never a persisted one"
        );
        changed.route.host.host_key_fingerprint =
            "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".into();
        assert!(!expected.matches(&changed));
        changed = info.clone();
        changed.route.host.host = HostId::new();
        assert!(!expected.matches(&changed));
        changed = info.clone();
        changed.installation = HostId::new();
        assert!(!expected.matches(&changed));
        changed = info;
        changed.principal = PrincipalId::new();
        assert!(!expected.matches(&changed));
    }

    #[test]
    fn files_session_profile_form_saves_without_reconnecting_or_ssh_fields() {
        use egui_kittest::{kittest::Queryable, Harness};
        let (mut app, id, _) = fixture();
        app.edit_profile(&id);
        app.editor.files_from_session = true;
        app.editor.name = "Saved from Files".into();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(375.0, 838.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show_editor(ui), app);
        harness.run();
        assert!(harness.query_by_label("Authentication").is_none());
        assert!(harness.query_by_label("Username").is_none());
        assert!(harness.query_by_label("Save & Open").is_none());
        harness.get_by_label("Save profile").click();
        harness.run();
        let app = harness.state();
        assert!(!app.editor.open);
        assert_eq!(
            app.profiles
                .as_ref()
                .unwrap()
                .host_files_spec(&id)
                .unwrap()
                .name,
            "Saved from Files"
        );
        assert!(app.files.is_none() && app.tabs.is_empty());
    }
}
