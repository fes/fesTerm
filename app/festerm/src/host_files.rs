//! Desktop Files authentication/tab composition; the protocol adapter is shared.
pub(crate) use festerm_tunnel_bridge::files_endpoint::{
    DeleteObservation, DeleteOutcome, DirectorySnapshotError, FilesChannel, FilesEndpoint,
    FirstFailure,
};
use festerm_tunnel_bridge::{
    file_client::NavigationPreference,
    host_client::ordinary::{ExpectedHost, Files},
};
use std::{future::Future, pin::Pin};
use tokio_util::sync::CancellationToken;

/// The stable identity of an authenticated Files opening, held for foreground
/// recovery only. Current account and host authority are checked on reopening.
#[derive(Clone, Debug)]
pub(crate) struct FilesRecoveryPin {
    pub(crate) account_id: u64,
    pub(crate) expected_host: ExpectedHost,
}

/// Worker-only ownership handoff. These streams never enter configuration or
/// the tab's presentation state, and share the retained account lifetime.
pub(crate) struct FilesConnection {
    pub browsing: Files,
    pub transfer: Files,
    pub account_id: u64,
    pub lifetime: CancellationToken,
    pub navigation: NavigationPreference,
}
pub(crate) type ConnectFiles = Pin<
    Box<dyn Future<Output = Result<FilesConnection, super::host_browser::RequestFailure>> + Send>,
>;

impl super::FesTermApp {
    pub(super) fn start_configured_files(
        &mut self,
        profile_id: &str,
        context: &eframe::egui::Context,
    ) {
        let Some(profile) = self
            .state
            .configuration()
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_dev_tunnel_files)
        else {
            return;
        };
        let target = crate::sftp_file_manager::HostFilesLaunchTarget {
            label: profile.identifier().into(),
            profile_id: Some(profile.identifier().into()),
            account: profile.account().map(str::to_owned),
            locator: profile.host().into(),
        };
        self.start_host_files(target, context);
    }

    pub(super) fn start_host_files(
        &mut self,
        target: crate::sftp_file_manager::HostFilesLaunchTarget,
        context: &eframe::egui::Context,
    ) {
        self.open_host_files(target, None, context);
    }

    pub(super) fn reopen_host_files(
        &mut self,
        target: crate::sftp_file_manager::HostFilesLaunchTarget,
        pin: FilesRecoveryPin,
        navigation: NavigationPreference,
        context: &eframe::egui::Context,
    ) {
        self.open_host_files(target, Some((pin, navigation)), context);
    }

    fn open_host_files(
        &mut self,
        target: crate::sftp_file_manager::HostFilesLaunchTarget,
        recovery: Option<(FilesRecoveryPin, NavigationPreference)>,
        context: &eframe::egui::Context,
    ) {
        use festerm_tunnel_bridge::host_client::{
            self,
            ordinary::{
                open_files, open_files_pinned, open_files_pinned_reviewed, open_files_reviewed,
                OrdinaryKeyIndex,
            },
        };
        use festerm_tunnel_client::{host_control::ConnectionPermit, service};
        if !context.input(|input| input.focused) {
            return;
        }
        if festerm_config::Profile::dev_tunnel_files(
            &target.label,
            target.account.clone(),
            &target.locator,
        )
        .is_err()
        {
            self.secure_storage_feedback = Some(
                "The Files destination metadata is invalid. Review its name, account and host.",
            );
            return;
        }
        if service::locator(&target.locator).is_err() {
            self.secure_storage_feedback =
                Some("The selected Files host is invalid. Choose it again.");
            return;
        }
        if let Some(profile_id) = &target.profile_id {
            let current = self
                .state
                .configuration()
                .profile(profile_id)
                .and_then(festerm_config::Profile::as_dev_tunnel_files)
                .is_some_and(|profile| {
                    profile.host() == target.locator
                        && profile.account() == target.account.as_deref()
                });
            if !current {
                self.secure_storage_feedback =
                    Some("The Files profile changed. Review it again before connecting.");
                return;
            }
        }
        let Some(account) = target
            .account
            .as_deref()
            .and_then(|key| self.state.configuration().account(key))
            .cloned()
        else {
            self.secure_storage_feedback =
                Some("Choose a signed-in account in the Files profile before connecting.");
            return;
        };
        if recovery
            .as_ref()
            .is_some_and(|(pin, _)| pin.account_id != account.account_id())
        {
            self.secure_storage_feedback =
                Some("The original Files account changed. Choose the account and host again.");
            return;
        }
        let store = match &self.secret_store {
            Ok(store) => store.clone(),
            Err(_) => {
                self.secure_storage_feedback =
                    Some("Native secure storage is unavailable. No Files fallback was attempted.");
                return;
            }
        };
        let account_cancel = match self.state.ordinary_account_token(&account) {
            Ok(token) => token,
            Err(message) => {
                self.secure_storage_feedback = Some(message);
                return;
            }
        };
        let permit = match ConnectionPermit::reserve(&self.state.host_client_budgets) {
            Ok(permit) => std::sync::Arc::new(permit),
            Err(_) => {
                self.secure_storage_feedback =
                    Some("The HOST connection budget is busy. Retry Files explicitly.");
                return;
            }
        };
        let lifetime = account_cancel.child_token();
        let request = lifetime.clone();
        let owner_cancel = lifetime.clone();
        let budgets = self.state.host_client_budgets.clone();
        let locator = target.locator.clone();
        let wake = context.clone();
        let connect: ConnectFiles = Box::pin(async move {
            if request.is_cancelled() {
                return Err("Files connection cancelled.".into());
            }
            let (identity, guard) = super::host_browser::prepare_ordinary_account(
                &account,
                store.as_ref(),
                account_cancel,
                permit,
                super::host_browser::PrivacyPreparationDiagnostic::new(1),
                request.clone(),
                wake,
                #[cfg(test)]
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )?;
            if request.is_cancelled() {
                return Err("Files connection cancelled.".into());
            }
            // Closing this tab fences only its two streams. The retained
            // parent guard still observes account/privacy retirement, while
            // closing one protocol client cannot cancel its sibling or account.
            let parent_guard = guard;
            let guard = host_client::ordinary::OrdinaryAccountGuard::new(
                request.clone(),
                std::sync::Arc::new(move || parent_guard.check()),
            );
            let index = OrdinaryKeyIndex::native()
                .map_err(|error| super::host_browser::RequestFailure::from(error.message()))?;
            let browsing = if let Some((pin, _)) = &recovery {
                open_files_pinned_reviewed(
                    &identity,
                    &locator,
                    index.clone(),
                    &budgets,
                    guard.clone(),
                    &request,
                    None,
                    &pin.expected_host,
                )
                .await
            } else {
                open_files_reviewed(
                    &identity,
                    &locator,
                    index.clone(),
                    &budgets,
                    guard.clone(),
                    &request,
                    None,
                )
                .await
            }
            .map_err(|error| super::host_browser::RequestFailure::from(error.to_string()))?
            .ok_or_else(|| {
                super::host_browser::RequestFailure::from(
                    "Files are disabled on the authenticated host.",
                )
            })?;
            let scope = browsing.scope().clone();
            let transfer = if let Some((pin, _)) = &recovery {
                open_files_pinned(
                    &identity,
                    &locator,
                    index,
                    &budgets,
                    guard.clone(),
                    &request,
                    Some(&scope),
                    &pin.expected_host,
                )
                .await
            } else {
                open_files(
                    &identity,
                    &locator,
                    index,
                    &budgets,
                    guard.clone(),
                    &request,
                    Some(&scope),
                )
                .await
            }
            .map_err(|error| super::host_browser::RequestFailure::from(error.to_string()))?
            .ok_or_else(|| {
                super::host_browser::RequestFailure::from(
                    "Files were disabled while opening the transfer stream.",
                )
            })?;
            guard
                .check()
                .map_err(|_| "OS privacy or account state changed.")?;
            if request.is_cancelled() {
                return Err("Files connection cancelled.".into());
            }
            Ok(FilesConnection {
                browsing,
                transfer,
                account_id: account.account_id(),
                lifetime: request,
                navigation: recovery.map_or(NavigationPreference::DefaultStart, |(_, mode)| mode),
            })
        });
        let settings = self.state.configuration().interface_settings();
        let local = settings
            .default_sftp_local_directory()
            .map(std::path::Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(crate::sftp_file_manager::local_home_directory);
        let tab = crate::sftp_file_manager::SftpFileManagerTab::new_files(
            target,
            connect,
            owner_cancel,
            local,
            settings.sftp_pane_order(),
            context,
        );
        self.state.place_sftp_file_manager(tab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::FesTermApp, sftp_file_manager::HostFilesLaunchTarget};
    use festerm_config::{AccountProvider, Configuration, DevTunnelAccount};
    use festerm_secret_store::{SecretReference, SecretStoreError};
    use festerm_tunnel_bridge::owner_auth::PinnedHost;

    #[test]
    fn desktop_files_recovery_refuses_remapped_provider_account_before_preparation() {
        let label = "owned-account";
        let pin = FilesRecoveryPin {
            account_id: 42,
            expected_host: ExpectedHost {
                host: PinnedHost {
                    host: Default::default(),
                    host_key_fingerprint: format!("SHA256:{}", "A".repeat(43)),
                },
                installation: Default::default(),
                principal: Default::default(),
            },
        };
        let target = HostFilesLaunchTarget {
            label: "Owned Files".into(),
            profile_id: None,
            account: Some(label.into()),
            locator: "owned.use".into(),
        };
        let context = eframe::egui::Context::default();
        for (account_id, feedback) in [
            (
                43,
                "The original Files account changed. Choose the account and host again.",
            ),
            (
                42,
                "Native secure storage is unavailable. No Files fallback was attempted.",
            ),
        ] {
            let account = DevTunnelAccount::new(
                label,
                AccountProvider::Github,
                account_id,
                "owned-login",
                SecretReference::generate(),
            )
            .unwrap();
            let (configuration, stored_label) =
                Configuration::empty().with_account(account).unwrap();
            assert_eq!(stored_label, label);
            let mut app = FesTermApp::for_test_with_configuration(configuration);
            // The unchanged account should reach this sentinel. A replacement
            // must refuse before this check or any account preparation.
            app.secret_store = Err(SecretStoreError::LockedOrUnavailable);
            let tab_count = app.state.tabs().len();
            for navigation in [
                NavigationPreference::ChooseLocation,
                NavigationPreference::DefaultStart,
            ] {
                app.secure_storage_feedback = None;
                let mut output = context.run_ui(
                    eframe::egui::RawInput {
                        focused: true,
                        ..Default::default()
                    },
                    |_| app.reopen_host_files(target.clone(), pin.clone(), navigation, &context),
                );
                output.textures_delta.clear();
                assert_eq!(app.secure_storage_feedback, Some(feedback));
                assert_eq!(app.state.tabs().len(), tab_count);
            }
        }
    }
}
