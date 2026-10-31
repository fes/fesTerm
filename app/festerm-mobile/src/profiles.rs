//! Saved profiles, host-key trust and credentials for the shell.
//!
//! A thin adapter over `festerm-mobile-profiles`: documents hold no secrets;
//! passwords and private keys live in the injected secret store (the iOS
//! Keychain in production, an in-memory store in tests and the gallery).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use festerm_mobile_profiles::{
    keys, CredentialStatus, KeyImportError, KnownHosts, ProfileError, ProfileId,
    ProfileKind as StoredKind, ProfileStore, RemoteSession, SshAuth, SshProfile,
};
use festerm_secret_store::SecretStore;
use festerm_ssh::{HostIdentity, SessionStrategy, SshAuthentication};

use super::model::{self, Capabilities};

/// Everything needed to start (or restart) one SSH session. It is a snapshot,
/// so a session can reconnect even after its profile was edited or deleted.
#[derive(Clone, Debug)]
pub struct SessionSpec {
    pub profile: Option<ProfileId>,
    pub name: String,
    pub ssh: SshProfile,
}

impl SessionSpec {
    pub fn detail(&self) -> String {
        model::profile_detail(model::ProfileKind::Ssh, &self.ssh.host, &self.ssh.username)
    }

    /// `tmux · work` for a durable remote session; `None` for a plain shell.
    pub fn persistent_label(&self) -> Option<String> {
        match &self.ssh.remote_session {
            RemoteSession::Plain => None,
            RemoteSession::Tmux { name } => Some(format!("tmux · {name}")),
            RemoteSession::Screen { name } => Some(format!("Screen · {name}")),
        }
    }

    pub fn strategy(&self) -> Option<SessionStrategy> {
        self.ssh.remote_session.strategy().ok()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedHostRow {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
}

/// Outcome of a key import that the editor must react to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyImportFailure {
    PassphraseRequired,
    Message(String),
}

pub struct Profiles {
    store: ProfileStore,
    hosts: KnownHosts,
    durable_credentials: bool,
}

/// The Keychain can be present but unusable (for example a build signed
/// without a keychain access group), so a write is attempted up front rather
/// than failing on the first credential the person types.
fn probe_secret_store(
    store: &dyn festerm_secret_store::SecretStore,
) -> Result<(), festerm_secret_store::SecretStoreError> {
    let probe = festerm_secret_store::SecretBytes::copy_from_slice(b"probe");
    let reference = store.put(&probe)?;
    store.delete(&reference)?;
    Ok(())
}

impl Profiles {
    pub fn open(
        directory: &Path,
        secrets: Arc<dyn SecretStore>,
        durable_credentials: bool,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(directory)
            .map_err(|_| "Profile storage could not be created".to_owned())?;
        let store = ProfileStore::open(directory, secrets).map_err(|error| describe(&error))?;
        let hosts = KnownHosts::open(directory).map_err(|error| error.to_string())?;
        Ok(Self {
            store,
            hosts,
            durable_credentials,
        })
    }

    /// The sandbox directory profiles live in: `$HOME/Library/Application
    /// Support/festerm`, which is inside the app container on iOS.
    pub fn default_directory() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("festerm"),
        )
    }

    /// Opens the production store with the platform secret store (the iOS
    /// Keychain). When no secure store exists the app still works, but
    /// credentials only last until it closes, and the UI says so.
    pub fn open_default() -> Result<Self, String> {
        let directory = Self::default_directory()
            .ok_or_else(|| "No application storage directory is available".to_owned())?;
        match festerm_secret_store::native_store().and_then(|native| {
            probe_secret_store(native.as_ref())?;
            Ok(native)
        }) {
            Ok(native) => Self::open(&directory, Arc::from(native), true),
            Err(_) => Self::open(
                &directory,
                Arc::new(festerm_secret_store::MemorySecretStore::new()),
                false,
            ),
        }
    }

    /// Whether passwords and keys survive an app restart.
    pub fn durable_credentials(&self) -> bool {
        self.durable_credentials
    }

    fn row(profile: &festerm_mobile_profiles::Profile) -> Option<model::Profile> {
        let (kind, host, username) = match &profile.kind {
            StoredKind::Ssh(ssh) => (model::ProfileKind::Ssh, &ssh.host, &ssh.username),
            StoredKind::Sftp(sftp) => (model::ProfileKind::Sftp, &sftp.host, &sftp.username),
            StoredKind::DevTunnel(_) => return None,
        };
        Some(model::Profile {
            id: profile.id.as_str().to_owned(),
            name: profile.name.clone(),
            kind,
            detail: model::profile_detail(kind, host, username),
            last_used: profile.last_used_at,
        })
    }

    fn visible(kind: model::ProfileKind, caps: Capabilities) -> bool {
        match kind {
            model::ProfileKind::Ssh => true,
            model::ProfileKind::Sftp => caps.sftp_profiles,
            model::ProfileKind::DevTunnel => caps.dev_tunnel_profiles,
        }
    }

    /// Profiles in the user's order, limited to kinds the shell can open.
    pub fn rows(&self, caps: Capabilities) -> Vec<model::Profile> {
        self.store
            .profiles()
            .iter()
            .filter_map(Self::row)
            .filter(|row| Self::visible(row.kind, caps))
            .collect()
    }

    /// Most recently launched first, for "Continue working".
    pub fn recents(&self, caps: Capabilities, limit: usize) -> Vec<model::Profile> {
        self.store
            .recents(limit)
            .into_iter()
            .filter_map(Self::row)
            .filter(|row| Self::visible(row.kind, caps))
            .collect()
    }

    fn id(id: &str) -> Option<ProfileId> {
        ProfileId::parse(id).ok()
    }

    /// A launchable snapshot of a saved SSH profile.
    pub fn spec(&self, id: &str) -> Option<SessionSpec> {
        let id = Self::id(id)?;
        let profile = self.store.get(&id)?;
        let StoredKind::Ssh(ssh) = &profile.kind else {
            return None;
        };
        Some(SessionSpec {
            profile: Some(id),
            name: profile.name.clone(),
            ssh: ssh.clone(),
        })
    }

    pub fn suggest_name(&self, base: &str) -> String {
        self.store.suggest_name(base)
    }

    pub fn save_ssh(
        &mut self,
        id: Option<&str>,
        name: &str,
        ssh: SshProfile,
    ) -> Result<String, String> {
        match id.and_then(Self::id) {
            Some(id) => {
                self.store
                    .update(&id, name, StoredKind::Ssh(ssh))
                    .map_err(|error| describe(&error))?;
                Ok(id.as_str().to_owned())
            }
            None => self
                .store
                .create(name, StoredKind::Ssh(ssh))
                .map(|id| id.as_str().to_owned())
                .map_err(|error| describe(&error)),
        }
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let id = Self::id(id).ok_or_else(|| "Profile not found".to_owned())?;
        self.store.delete(&id).map_err(|error| describe(&error))
    }

    pub fn mark_used(&mut self, id: &str) {
        if let Some(id) = Self::id(id) {
            // Recency is a convenience; a failed write must not block connecting.
            let _ = self.store.mark_used(&id);
        }
    }

    /// The name and settings of a saved SSH profile, for the editor.
    pub fn ssh_profile(&self, id: &str) -> Option<(String, SshProfile)> {
        let spec = self.spec(id)?;
        Some((spec.name, spec.ssh))
    }

    pub fn authentication(&self, auth: &SshAuth) -> SshAuthentication {
        self.store.ssh_authentication(auth)
    }

    pub fn credential_status(&self, auth: &SshAuth) -> CredentialStatus {
        self.store.credential_status(auth)
    }

    pub fn store_password(&self, password: String) -> Result<SshAuth, String> {
        self.store
            .store_password(password)
            .map_err(|error| describe(&error))
    }

    pub fn import_key(
        &self,
        text: &str,
        passphrase: Option<&str>,
    ) -> Result<SshAuth, KeyImportFailure> {
        let parsed = keys::parse_private_key(text, passphrase).map_err(|error| match error {
            KeyImportError::PassphraseRequired => KeyImportFailure::PassphraseRequired,
            other => KeyImportFailure::Message(capitalized(&other.to_string())),
        })?;
        self.store
            .store_key(parsed)
            .map_err(|error| KeyImportFailure::Message(describe(&error)))
    }

    pub fn generate_key(&self, comment: &str) -> Result<SshAuth, String> {
        let parsed =
            keys::generate_ed25519(comment).map_err(|error| capitalized(&error.to_string()))?;
        self.store
            .store_key(parsed)
            .map_err(|error| describe(&error))
    }

    /// Deletes a credential that was stored but never attached to a profile.
    pub fn discard_credential(&self, auth: &SshAuth) {
        let secret = match auth {
            SshAuth::Ask => return,
            SshAuth::Password { secret } | SshAuth::Key { secret, .. } => secret,
        };
        let _ = self.store.discard_unused_secret(secret);
    }

    pub fn pinned_fingerprint(&self, identity: &HostIdentity) -> Option<String> {
        self.hosts.trusted_fingerprint(identity).map(str::to_owned)
    }

    pub fn trust_host(&mut self, identity: &HostIdentity, fingerprint: &str) -> Result<(), String> {
        self.hosts
            .trust(identity, fingerprint)
            .map_err(|error| error.to_string())
    }

    pub fn trusted_hosts(&self) -> Vec<TrustedHostRow> {
        self.hosts
            .hosts()
            .iter()
            .map(|known| TrustedHostRow {
                host: known.host.clone(),
                port: known.port,
                fingerprint: known.fingerprint.clone(),
            })
            .collect()
    }

    pub fn forget_host(&mut self, host: &str, port: u16) -> Result<(), String> {
        let identity =
            HostIdentity::new(host, port).map_err(|_| "Invalid trusted host".to_owned())?;
        self.hosts
            .forget(&identity)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
}

/// A short message for the editor; secret values are never part of it.
fn describe(error: &ProfileError) -> String {
    match error {
        ProfileError::Validation(validation) => match validation.field {
            "name" => "Enter a valid profile name".to_owned(),
            "host" => "Enter a valid host".to_owned(),
            "username" => "Enter a valid username".to_owned(),
            "port" => "Enter a valid port".to_owned(),
            "remote_session.name" => {
                "Enter a valid session name (letters, digits, - _ .)".to_owned()
            }
            _ => capitalized(&validation.to_string()),
        },
        other => capitalized(&other.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A store in a unique directory under the workspace `target` dir, with
    /// an in-memory secret store.
    pub fn profiles() -> Profiles {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/mobile-profile-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = std::fs::remove_dir_all(&directory);
        Profiles::open(
            &directory,
            Arc::new(festerm_secret_store::MemorySecretStore::new()),
            true,
        )
        .expect("test profile store")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(auth: SshAuth, remote_session: RemoteSession) -> SshProfile {
        SshProfile {
            host: "web.example.net".to_owned(),
            port: 22,
            username: "deploy".to_owned(),
            auth,
            remote_session,
            recovery: Default::default(),
        }
    }

    #[test]
    fn mobile_profiles_crud_recents_and_remote_session_strategy() {
        let mut profiles = testing::profiles();
        let id = profiles
            .save_ssh(
                None,
                "prod-web",
                ssh(
                    SshAuth::Ask,
                    RemoteSession::Tmux {
                        name: "work".to_owned(),
                    },
                ),
            )
            .unwrap();
        let spec = profiles.spec(&id).unwrap();
        assert_eq!(spec.persistent_label().as_deref(), Some("tmux · work"));
        assert!(matches!(
            spec.strategy(),
            Some(SessionStrategy::Persistent { .. })
        ));
        assert!(profiles.recents(Capabilities::production(), 2).is_empty());
        profiles.mark_used(&id);
        assert_eq!(profiles.recents(Capabilities::production(), 2).len(), 1);
        profiles
            .save_ssh(
                Some(&id),
                "prod-web",
                ssh(SshAuth::Ask, RemoteSession::Plain),
            )
            .unwrap();
        assert_eq!(profiles.spec(&id).unwrap().persistent_label(), None);
        assert!(profiles
            .save_ssh(
                None,
                "bad",
                ssh(
                    SshAuth::Ask,
                    RemoteSession::Tmux {
                        name: "a b; rm".to_owned()
                    }
                )
            )
            .is_err());
        profiles.delete(&id).unwrap();
        assert!(profiles.rows(Capabilities::production()).is_empty());
    }

    #[test]
    fn mobile_profiles_keys_are_generated_stored_and_rejected_when_invalid() {
        let profiles = testing::profiles();
        let generated = profiles.generate_key("fesTerm iPhone").unwrap();
        let SshAuth::Key { key, .. } = &generated else {
            panic!("generated credential is a key");
        };
        assert!(key.public_key.starts_with("ssh-ed25519 "));
        assert_eq!(
            profiles.credential_status(&generated),
            CredentialStatus::Present
        );
        profiles.discard_credential(&generated);
        assert_ne!(
            profiles.credential_status(&generated),
            CredentialStatus::Present
        );
        assert!(matches!(
            profiles.import_key("not a key", None),
            Err(KeyImportFailure::Message(_))
        ));
    }

    #[test]
    fn mobile_profiles_trusted_hosts_persist_and_can_be_forgotten() {
        let mut profiles = testing::profiles();
        let identity = HostIdentity::new("web.example.net", 22).unwrap();
        assert!(profiles.pinned_fingerprint(&identity).is_none());
        profiles
            .trust_host(
                &identity,
                "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            )
            .unwrap();
        assert!(profiles.pinned_fingerprint(&identity).is_some());
        assert_eq!(profiles.trusted_hosts().len(), 1);
        profiles.forget_host("web.example.net", 22).unwrap();
        assert!(profiles.pinned_fingerprint(&identity).is_none());
    }
}
