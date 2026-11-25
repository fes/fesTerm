//! Desktop Files authentication/tab composition; the protocol adapter is shared.
pub(crate) use festerm_tunnel_bridge::files_endpoint::{
    DeleteObservation, DeleteOutcome, FilesChannel, FilesEndpoint, FirstFailure,
};
use festerm_tunnel_bridge::host_client::ordinary::Files;
use std::{future::Future, pin::Pin};
use tokio_util::sync::CancellationToken;

/// Worker-only ownership handoff. These streams never enter configuration or
/// the tab's presentation state, and share the retained account lifetime.
pub(crate) struct FilesConnection {
    pub browsing: Files,
    pub transfer: Files,
    pub account_id: u64,
    pub lifetime: CancellationToken,
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
        use festerm_tunnel_bridge::host_client::{
            self,
            ordinary::{open_files, open_files_reviewed, OrdinaryKeyIndex},
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
            let browsing = open_files_reviewed(
                &identity,
                &locator,
                index.clone(),
                &budgets,
                guard.clone(),
                &request,
                None,
            )
            .await
            .map_err(|error| super::host_browser::RequestFailure::from(error.to_string()))?
            .ok_or_else(|| {
                super::host_browser::RequestFailure::from(
                    "Files are disabled on the authenticated host.",
                )
            })?;
            let scope = browsing.scope().clone();
            let transfer = open_files(
                &identity,
                &locator,
                index,
                &budgets,
                guard.clone(),
                &request,
                Some(&scope),
            )
            .await
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
