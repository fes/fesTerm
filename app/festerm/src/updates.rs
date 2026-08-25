use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use cargo_packager_updater::{semver::Version, url::Url, Config, Update};

const UPDATE_ENDPOINT: &str =
    "https://github.com/fes/fesTerm/releases/latest/download/festerm-update.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallationKind {
    Developer,
    PackageManaged,
    SelfUpdating,
}

impl InstallationKind {
    fn from_build_marker(marker: Option<&str>) -> Self {
        match marker {
            Some("managed") => Self::PackageManaged,
            Some("app" | "appimage" | "nsis") => Self::SelfUpdating,
            Some(_) | None => Self::Developer,
        }
    }

    pub(crate) const fn can_install(self) -> bool {
        matches!(self, Self::SelfUpdating)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UpdateSummary {
    pub(crate) version: String,
    pub(crate) notes: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UpdateStatus {
    Unavailable(&'static str),
    Idle,
    Checking,
    Current,
    Available(UpdateSummary),
    Downloading(UpdateSummary),
    ReadyToInstall(UpdateSummary),
    Installing(UpdateSummary),
    Installed(UpdateSummary),
    Failed {
        message: &'static str,
        retry_check: bool,
    },
}

impl UpdateStatus {
    pub(crate) const fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading(_) | Self::Installing(_)
        )
    }
}

enum WorkerResult {
    Checked(Result<Option<Update>, ()>),
    Downloaded(Result<(Update, Vec<u8>), ()>),
    Installed(Result<(), ()>),
}

pub(crate) struct UpdateController {
    status: UpdateStatus,
    installation_kind: InstallationKind,
    public_key: Option<String>,
    pending_update: Option<Update>,
    downloaded_update: Option<(Update, Vec<u8>)>,
    receiver: Option<Receiver<WorkerResult>>,
}

impl UpdateController {
    pub(crate) fn from_build() -> Self {
        Self::new(
            option_env!("FESTERM_INSTALLATION_KIND"),
            option_env!("FESTERM_UPDATE_PUBLIC_KEY"),
        )
    }

    fn new(installation_marker: Option<&str>, public_key: Option<&str>) -> Self {
        let installation_kind = InstallationKind::from_build_marker(installation_marker);
        let public_key = public_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_owned);
        let status = if installation_kind == InstallationKind::Developer {
            UpdateStatus::Unavailable("Update checks are available in packaged releases.")
        } else if public_key.is_none() {
            UpdateStatus::Unavailable("This build does not contain an update verification key.")
        } else {
            UpdateStatus::Idle
        };
        Self {
            status,
            installation_kind,
            public_key,
            pending_update: None,
            downloaded_update: None,
            receiver: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_test() -> Self {
        Self::new(None, None)
    }

    #[cfg(test)]
    pub(crate) fn configured_for_test() -> Self {
        Self::new(Some("app"), Some("test public key"))
    }

    pub(crate) const fn status(&self) -> &UpdateStatus {
        &self.status
    }

    pub(crate) const fn installation_kind(&self) -> InstallationKind {
        self.installation_kind
    }

    pub(crate) const fn endpoint() -> &'static str {
        UPDATE_ENDPOINT
    }

    pub(crate) fn begin_check(&mut self) {
        if self.status.is_busy() || matches!(self.status, UpdateStatus::Unavailable(_)) {
            return;
        }
        let Some(public_key) = self.public_key.clone() else {
            self.status = UpdateStatus::Unavailable(
                "This build does not contain an update verification key.",
            );
            return;
        };
        self.pending_update = None;
        self.downloaded_update = None;
        self.status = UpdateStatus::Checking;
        self.receiver = Some(spawn_worker(move || {
            let result = (|| {
                let endpoint = Url::parse(UPDATE_ENDPOINT).map_err(|error| {
                    tracing::error!(%error, "invalid compiled update endpoint");
                })?;
                let version = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
                    tracing::error!(%error, "invalid compiled application version");
                })?;
                cargo_packager_updater::check_update(
                    version,
                    Config {
                        endpoints: vec![endpoint],
                        pubkey: public_key,
                        windows: None,
                    },
                )
                .map_err(|error| {
                    tracing::warn!(%error, "update check failed");
                })
            })();
            WorkerResult::Checked(result)
        }));
    }

    pub(crate) fn begin_download(&mut self) {
        if !self.installation_kind.can_install() || self.status.is_busy() {
            return;
        }
        let Some(update) = self.pending_update.take() else {
            return;
        };
        let summary = summary(&update);
        self.status = UpdateStatus::Downloading(summary);
        self.receiver = Some(spawn_worker(move || {
            let result = update
                .download()
                .map(|bytes| (update, bytes))
                .map_err(|error| {
                    tracing::warn!(%error, "update download or verification failed");
                });
            WorkerResult::Downloaded(result)
        }));
    }

    pub(crate) fn begin_install(&mut self) {
        if !self.installation_kind.can_install() || self.status.is_busy() {
            return;
        }
        let Some((update, bytes)) = self.downloaded_update.take() else {
            return;
        };
        let summary = summary(&update);
        self.status = UpdateStatus::Installing(summary);
        self.receiver = Some(spawn_worker(move || {
            let result = update.install(bytes).map_err(|error| {
                tracing::error!(%error, "verified update installation failed");
            });
            WorkerResult::Installed(result)
        }));
    }

    pub(crate) fn poll(&mut self) {
        let Some(receiver) = self.receiver.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
                self.status = UpdateStatus::Failed {
                    message: "The update worker stopped unexpectedly.",
                    retry_check: true,
                };
                return;
            }
        };
        self.receiver = None;
        match result {
            WorkerResult::Checked(Ok(Some(update))) => {
                self.status = UpdateStatus::Available(summary(&update));
                self.pending_update = Some(update);
            }
            WorkerResult::Checked(Ok(None)) => self.status = UpdateStatus::Current,
            WorkerResult::Checked(Err(())) => {
                self.status = UpdateStatus::Failed {
                    message:
                        "Could not check for updates. Check your network connection and try again.",
                    retry_check: true,
                };
            }
            WorkerResult::Downloaded(Ok((update, bytes))) => {
                self.status = UpdateStatus::ReadyToInstall(summary(&update));
                self.downloaded_update = Some((update, bytes));
            }
            WorkerResult::Downloaded(Err(())) => {
                self.status = UpdateStatus::Failed {
                    message: "The update could not be downloaded or its signature was invalid.",
                    retry_check: true,
                };
            }
            WorkerResult::Installed(Ok(())) => {
                let summary = match &self.status {
                    UpdateStatus::Installing(summary) => summary.clone(),
                    _ => return,
                };
                self.status = UpdateStatus::Installed(summary);
            }
            WorkerResult::Installed(Err(())) => {
                self.status = UpdateStatus::Failed {
                    message: "The verified update could not be installed.",
                    retry_check: true,
                };
            }
        }
    }
}

fn spawn_worker(work: impl FnOnce() -> WorkerResult + Send + 'static) -> Receiver<WorkerResult> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
}

fn summary(update: &Update) -> UpdateSummary {
    UpdateSummary {
        version: update.version.clone(),
        notes: update.body.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn developer_builds_never_contact_the_update_endpoint() {
        let mut controller = UpdateController::new(None, Some("public key"));
        controller.begin_check();

        assert!(matches!(
            controller.status(),
            UpdateStatus::Unavailable("Update checks are available in packaged releases.")
        ));
        assert!(controller.receiver.is_none());
    }

    #[test]
    fn packaged_builds_fail_closed_without_a_public_key() {
        let controller = UpdateController::new(Some("app"), None);

        assert!(matches!(
            controller.status(),
            UpdateStatus::Unavailable("This build does not contain an update verification key.")
        ));
    }

    #[test]
    fn package_managers_can_check_but_cannot_install() {
        let controller = UpdateController::new(Some("managed"), Some("public key"));

        assert_eq!(
            controller.installation_kind(),
            InstallationKind::PackageManaged
        );
        assert!(!controller.installation_kind().can_install());
        assert_eq!(controller.status(), &UpdateStatus::Idle);
    }

    #[test]
    fn known_self_update_formats_are_installable() {
        for marker in ["app", "appimage", "nsis"] {
            assert!(InstallationKind::from_build_marker(Some(marker)).can_install());
        }
        assert_eq!(
            InstallationKind::from_build_marker(Some("wix")),
            InstallationKind::Developer
        );
    }
}
