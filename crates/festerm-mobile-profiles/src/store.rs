use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use festerm_secret_store::{SecretBytes, SecretStore, SecretStoreError};
use festerm_ssh::SshAuthentication;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::clock::{Clock, SystemClock};
use crate::document::{load_document, quarantine_copy, write_atomic, Loaded, Migration};
use crate::error::{ProfileError, StorageError, ValidationError};
use crate::keys::{store_private_key, ParsedPrivateKey};
use crate::model::{
    validate_name, AccountProvider, Profile, ProfileId, ProfileKind, SecretRef, SshAuth,
    TunnelAccount, TunnelSource,
};
use crate::tunnel_import::PublicProfileImport;

pub const PROFILES_FILE: &str = "profiles.json";
pub const CURRENT_VERSION: u32 = 1;
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_PROFILES: usize = 200;
pub const MAX_ACCOUNTS: usize = 8;
pub const MAX_PASSWORD_BYTES: usize = 1024;
const MIGRATIONS: &[Migration] = &[];

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct Document {
    version: u32,
    profiles: Vec<Profile>,
    accounts: Vec<TunnelAccount>,
    /// Secrets whose deletion failed (for example, a locked Keychain); retried
    /// until they are gone so a deleted profile never leaves credentials behind.
    pending_secret_deletions: Vec<SecretRef>,
}

impl Document {
    fn referenced_secrets(&self) -> HashSet<&SecretRef> {
        let mut secrets: HashSet<&SecretRef> = self
            .profiles
            .iter()
            .flat_map(|profile| profile.kind.secrets())
            .collect();
        secrets.extend(self.accounts.iter().map(|account| &account.credential));
        secrets
    }
}

/// What happened while opening the store. Anything non-default is worth a
/// one-time notice in the UI.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadReport {
    /// The document version migrated from, if it was upgraded.
    pub migrated_from: Option<u32>,
    /// The whole document was unreadable; it was moved aside and the list
    /// started empty.
    pub started_empty_after_corruption: bool,
    /// Individual entries that failed validation and were skipped.
    pub dropped_entries: usize,
    /// Where the unreadable data was preserved for support.
    pub quarantined_file: Option<PathBuf>,
}

/// Whether a profile's saved credential can currently be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialStatus {
    /// Nothing to read: the profile prompts for its password.
    NotRequired,
    Present,
    /// The Keychain item is gone (for example after a restore to a new
    /// device): ask the user to re-enter or re-import it.
    Missing,
    /// The Keychain is locked or unavailable; retry later.
    Unavailable,
}

/// The persistent list of profiles and Dev Tunnel accounts.
///
/// Every mutating method saves atomically before returning and leaves both
/// disk and memory unchanged on failure. It is not internally synchronized;
/// share it behind a `Mutex`.
///
/// Secrets handed to [`create`](Self::create), [`update`](Self::update) or
/// [`upsert_account`](Self::upsert_account) (through a [`SecretRef`]) become
/// owned by the store: if the call fails it deletes the ones it did not
/// already own, and when they are later dropped from a profile it deletes them.
pub struct ProfileStore {
    path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
    document: Document,
    report: LoadReport,
}

impl ProfileStore {
    /// Opens `profiles.json` in `directory` (the app sandbox, created by the
    /// host), starting empty when absent.
    pub fn open(directory: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self, ProfileError> {
        Self::open_with_clock(directory, secrets, Arc::new(SystemClock))
    }

    pub fn open_with_clock(
        directory: &Path,
        secrets: Arc<dyn SecretStore>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ProfileError> {
        let path = directory.join(PROFILES_FILE);
        let (document, report) = Self::load(&path)?;
        let mut store = Self {
            path,
            secrets,
            clock,
            document,
            report,
        };
        store.retry_pending_secret_deletions();
        Ok(store)
    }

    fn load(path: &Path) -> Result<(Document, LoadReport), StorageError> {
        let mut report = LoadReport::default();
        let mut document = Document {
            version: CURRENT_VERSION,
            ..Document::default()
        };
        match load_document(path, MAX_FILE_BYTES, CURRENT_VERSION, MIGRATIONS)? {
            Loaded::Missing => {}
            Loaded::Corrupt { quarantined } => {
                report.started_empty_after_corruption = true;
                report.quarantined_file = quarantined;
            }
            Loaded::Document {
                value,
                migrated_from,
            } => {
                report.migrated_from = migrated_from;
                Self::collect(&value, &mut document, &mut report);
                if report.dropped_entries > 0 {
                    report.quarantined_file = quarantine_copy(path);
                }
            }
        }
        Ok((document, report))
    }

    fn collect(value: &Value, document: &mut Document, report: &mut LoadReport) {
        let entries = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        for entry in entries("accounts") {
            match serde_json::from_value::<TunnelAccount>(entry) {
                Ok(account)
                    if account.validate().is_ok()
                        && document.accounts.len() < MAX_ACCOUNTS
                        && !document.accounts.iter().any(|other| other.id == account.id) =>
                {
                    document.accounts.push(account);
                }
                _ => report.dropped_entries += 1,
            }
        }
        for entry in entries("profiles") {
            let parsed = serde_json::from_value::<Profile>(entry)
                .ok()
                .and_then(|mut profile| {
                    validate_name(&mut profile.name).ok()?;
                    profile.kind.validate().ok()?;
                    Some(profile)
                });
            match parsed {
                Some(profile)
                    if document.profiles.len() < MAX_PROFILES
                        && !document.profiles.iter().any(|other| {
                            other.id == profile.id || same_name(&other.name, &profile.name)
                        }) =>
                {
                    document.profiles.push(profile);
                }
                _ => report.dropped_entries += 1,
            }
        }
        document.pending_secret_deletions = entries("pending_secret_deletions")
            .into_iter()
            .filter_map(|entry| serde_json::from_value(entry).ok())
            .collect();
    }

    #[must_use]
    pub fn load_report(&self) -> &LoadReport {
        &self.report
    }

    /// The underlying secret store, for callers that keep their own secrets
    /// (for example the Dev Tunnels refresh credential).
    #[must_use]
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    // ----- reading -----

    /// Profiles in the user's chosen order.
    #[must_use]
    pub fn profiles(&self) -> &[Profile] {
        &self.document.profiles
    }

    #[must_use]
    pub fn get(&self, id: &ProfileId) -> Option<&Profile> {
        self.document
            .profiles
            .iter()
            .find(|profile| profile.id == *id)
    }

    /// Most recently launched profiles first ("Continue working").
    #[must_use]
    pub fn recents(&self, limit: usize) -> Vec<&Profile> {
        let mut used: Vec<&Profile> = self
            .document
            .profiles
            .iter()
            .filter(|profile| profile.last_used_at.is_some())
            .collect();
        used.sort_by_key(|profile| std::cmp::Reverse(profile.last_used_at));
        used.truncate(limit);
        used
    }

    /// `base`, or `base 2`, `base 3`... when already taken.
    #[must_use]
    pub fn suggest_name(&self, base: &str) -> String {
        let base = base.trim();
        let taken = |name: &str| {
            self.document
                .profiles
                .iter()
                .any(|profile| same_name(&profile.name, name))
        };
        if !taken(base) {
            return base.to_owned();
        }
        (2..)
            .map(|number| format!("{base} {number}"))
            .find(|name| !taken(name))
            .expect("an unused suffix exists")
    }

    #[must_use]
    pub fn accounts(&self) -> &[TunnelAccount] {
        &self.document.accounts
    }

    #[must_use]
    pub fn account(&self, id: &str) -> Option<&TunnelAccount> {
        self.document
            .accounts
            .iter()
            .find(|account| account.id == id)
    }

    // ----- profile mutations -----

    /// Adds a profile at the end of the list.
    pub fn create(&mut self, name: &str, kind: ProfileKind) -> Result<ProfileId, ProfileError> {
        let owned: Vec<SecretRef> = kind.secrets().into_iter().cloned().collect();
        let result = self.try_create(name, kind);
        if result.is_err() {
            self.release_unreferenced(owned);
        }
        result
    }

    fn try_create(&mut self, name: &str, mut kind: ProfileKind) -> Result<ProfileId, ProfileError> {
        let mut name = name.to_owned();
        validate_name(&mut name)?;
        kind.validate()?;
        let mut next = self.document.clone();
        if next.profiles.len() >= MAX_PROFILES {
            return Err(ProfileError::LimitReached);
        }
        self.check_unique_and_account(&next, None, &name, &kind)?;
        let id = ProfileId::generate();
        next.profiles.push(Profile {
            id: id.clone(),
            name,
            created_at: self.clock.now_unix(),
            last_used_at: None,
            kind,
        });
        self.commit(next)?;
        Ok(id)
    }

    /// Replaces a profile's name and settings, keeping its id, position,
    /// creation time and last-used time.
    ///
    /// Secrets no longer referenced afterwards are deleted. Changing a Dev
    /// Tunnel profile's account drops its pinned host and connect credential,
    /// which belong to the previous account.
    pub fn update(
        &mut self,
        id: &ProfileId,
        name: &str,
        kind: ProfileKind,
    ) -> Result<(), ProfileError> {
        let owned: Vec<SecretRef> = kind.secrets().into_iter().cloned().collect();
        let result = self.try_update(id, name, kind);
        match result {
            Ok(removed) => {
                self.release_unreferenced(removed);
                Ok(())
            }
            Err(error) => {
                self.release_unreferenced(owned);
                Err(error)
            }
        }
    }

    fn try_update(
        &mut self,
        id: &ProfileId,
        name: &str,
        mut kind: ProfileKind,
    ) -> Result<Vec<SecretRef>, ProfileError> {
        let mut name = name.to_owned();
        validate_name(&mut name)?;
        let mut next = self.document.clone();
        let index = position(&next, id)?;
        if let (ProfileKind::DevTunnel(old), ProfileKind::DevTunnel(new)) =
            (&next.profiles[index].kind, &mut kind)
        {
            if old.account != new.account && old.source == new.source {
                if let TunnelSource::Named {
                    trusted_host,
                    connect_credential,
                    ..
                } = &mut new.source
                {
                    *trusted_host = None;
                    *connect_credential = None;
                }
            }
        }
        kind.validate()?;
        self.check_unique_and_account(&next, Some(id), &name, &kind)?;
        let previous: Vec<SecretRef> = next.profiles[index]
            .kind
            .secrets()
            .into_iter()
            .cloned()
            .collect();
        next.profiles[index].name = name;
        next.profiles[index].kind = kind;
        self.commit(next)?;
        Ok(previous)
    }

    /// Deletes a profile and the secrets only it used. Secrets that could not
    /// be deleted now (locked Keychain) are retried on the next open or
    /// [`retry_pending_secret_deletions`](Self::retry_pending_secret_deletions).
    pub fn delete(&mut self, id: &ProfileId) -> Result<(), ProfileError> {
        let mut next = self.document.clone();
        let index = position(&next, id)?;
        let removed = next.profiles.remove(index);
        self.commit(next)?;
        self.release_unreferenced(removed.kind.secrets().into_iter().cloned().collect());
        Ok(())
    }

    /// Sets the display order; `order` must list every profile exactly once.
    pub fn reorder(&mut self, order: &[ProfileId]) -> Result<(), ProfileError> {
        let mut next = self.document.clone();
        let mut sorted = Vec::with_capacity(order.len());
        for id in order {
            let index = next
                .profiles
                .iter()
                .position(|profile| profile.id == *id)
                .ok_or(ProfileError::InvalidOrder)?;
            sorted.push(next.profiles.swap_remove(index));
        }
        if !next.profiles.is_empty() {
            return Err(ProfileError::InvalidOrder);
        }
        next.profiles = sorted;
        self.commit(next)
    }

    /// Moves one profile to `index` (clamped to the end of the list).
    pub fn move_profile(&mut self, id: &ProfileId, index: usize) -> Result<(), ProfileError> {
        let mut next = self.document.clone();
        let from = position(&next, id)?;
        let profile = next.profiles.remove(from);
        next.profiles
            .insert(index.min(next.profiles.len()), profile);
        self.commit(next)
    }

    /// Records a launch so the profile appears in [`recents`](Self::recents).
    pub fn mark_used(&mut self, id: &ProfileId) -> Result<(), ProfileError> {
        let mut next = self.document.clone();
        let index = position(&next, id)?;
        next.profiles[index].last_used_at = Some(self.clock.now_unix());
        self.commit(next)
    }

    /// Saves a previewed `festerm-profile.json` as a new Dev Tunnel profile.
    /// An import never replaces a profile: a name clash is
    /// [`ProfileError::DuplicateName`]; use [`suggest_name`](Self::suggest_name)
    /// on `import.name()` and [`import_dev_tunnel_as`](Self::import_dev_tunnel_as).
    pub fn import_dev_tunnel(
        &mut self,
        import: PublicProfileImport,
        account: Option<String>,
    ) -> Result<ProfileId, ProfileError> {
        let name = import.name().to_owned();
        self.import_dev_tunnel_as(&name, import, account)
    }

    pub fn import_dev_tunnel_as(
        &mut self,
        name: &str,
        import: PublicProfileImport,
        account: Option<String>,
    ) -> Result<ProfileId, ProfileError> {
        self.create(
            name,
            ProfileKind::DevTunnel(crate::model::DevTunnelProfile {
                account,
                session: None,
                take_over_if_attached: true,
                source: TunnelSource::Imported {
                    destination: Box::new(import.into_destination()),
                },
            }),
        )
    }

    // ----- accounts -----

    /// Records a signed-in account, or re-signs-in an existing one (same
    /// provider and account id) with a new credential, replacing the old one.
    /// Returns the account's stable id.
    pub fn upsert_account(
        &mut self,
        provider: AccountProvider,
        account_id: u64,
        login: &str,
        credential: SecretRef,
    ) -> Result<String, ProfileError> {
        let owned = vec![credential.clone()];
        let result = self.try_upsert_account(provider, account_id, login, credential);
        match result {
            Ok((id, replaced)) => {
                self.release_unreferenced(replaced);
                Ok(id)
            }
            Err(error) => {
                self.release_unreferenced(owned);
                Err(error)
            }
        }
    }

    fn try_upsert_account(
        &mut self,
        provider: AccountProvider,
        account_id: u64,
        login: &str,
        credential: SecretRef,
    ) -> Result<(String, Vec<SecretRef>), ProfileError> {
        let account = TunnelAccount {
            id: format!("github-{account_id}"),
            provider,
            account_id,
            login: login.to_owned(),
            credential,
        };
        account.validate()?;
        let mut next = self.document.clone();
        let mut replaced = Vec::new();
        if let Some(existing) = next
            .accounts
            .iter_mut()
            .find(|other| other.id == account.id)
        {
            replaced.push(existing.credential.clone());
            *existing = account.clone();
        } else if next.accounts.len() >= MAX_ACCOUNTS {
            return Err(ProfileError::LimitReached);
        } else {
            next.accounts.push(account.clone());
        }
        self.commit(next)?;
        Ok((account.id, replaced))
    }

    /// Signs an account out: deletes its credential and, from every profile
    /// that used it, the selection, pinned host and connect credential.
    pub fn remove_account(&mut self, id: &str) -> Result<(), ProfileError> {
        let mut next = self.document.clone();
        let index = next
            .accounts
            .iter()
            .position(|account| account.id == id)
            .ok_or(ProfileError::UnknownAccount)?;
        let removed = next.accounts.remove(index);
        let mut released = vec![removed.credential];
        for profile in &mut next.profiles {
            let ProfileKind::DevTunnel(tunnel) = &mut profile.kind else {
                continue;
            };
            if tunnel.account.as_deref() != Some(id) {
                continue;
            }
            tunnel.account = None;
            if let TunnelSource::Named {
                trusted_host,
                connect_credential,
                ..
            } = &mut tunnel.source
            {
                *trusted_host = None;
                released.extend(connect_credential.take());
            }
        }
        self.commit(next)?;
        self.release_unreferenced(released);
        Ok(())
    }

    // ----- secrets -----

    /// Saves a password. Pass the result in `SshAuth::Password` to
    /// [`create`](Self::create) or [`update`](Self::update).
    pub fn store_password(&self, password: String) -> Result<SshAuth, ProfileError> {
        if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
            return Err(ValidationError::new("password", "is empty or too long").into());
        }
        let secret = SecretBytes::from_secret_string(password);
        let reference = self.secrets.put(&secret)?;
        Ok(SshAuth::Password {
            secret: SecretRef::from_reference(&reference),
        })
    }

    /// Saves an imported or generated key in the Keychain and returns the
    /// `SshAuth::Key` to put in a profile.
    pub fn store_key(&self, key: ParsedPrivateKey) -> Result<SshAuth, ProfileError> {
        let (secret, key) = store_private_key(self.secrets.as_ref(), key)?;
        Ok(SshAuth::Key { secret, key })
    }

    /// Saves arbitrary bounded secret bytes (for example the Dev Tunnels
    /// refresh or connect credential).
    pub fn store_secret(&self, secret: &SecretBytes) -> Result<SecretRef, ProfileError> {
        let reference = self.secrets.put(secret)?;
        Ok(SecretRef::from_reference(&reference))
    }

    /// Deletes a secret that was stored but never attached to a profile, for
    /// example when the user cancels after importing a key. It refuses to
    /// delete a secret that a profile or account still uses.
    pub fn discard_unused_secret(&self, secret: &SecretRef) -> Result<(), ProfileError> {
        if self.document.referenced_secrets().contains(secret) {
            return Ok(());
        }
        self.secrets.delete(&secret.to_reference())?;
        Ok(())
    }

    /// Builds the `festerm-ssh` authentication for a saved profile. Secrets
    /// are resolved by the SSH worker, never returned to the caller.
    #[must_use]
    pub fn ssh_authentication(&self, auth: &SshAuth) -> SshAuthentication {
        match auth {
            SshAuth::Ask => SshAuthentication::Interactive,
            SshAuth::Password { secret } => SshAuthentication::stored_password(
                Arc::clone(&self.secrets),
                &secret.to_reference(),
            ),
            SshAuth::Key { secret, .. } => SshAuthentication::stored_private_key(
                Arc::clone(&self.secrets),
                &secret.to_reference(),
            ),
        }
    }

    /// Checks that a saved credential is readable, for "re-enter password"
    /// affordances.
    #[must_use]
    pub fn credential_status(&self, auth: &SshAuth) -> CredentialStatus {
        let Some(secret) = auth.secret() else {
            return CredentialStatus::NotRequired;
        };
        match self.secrets.get(&secret.to_reference()) {
            Ok(_) => CredentialStatus::Present,
            Err(SecretStoreError::Missing) => CredentialStatus::Missing,
            Err(_) => CredentialStatus::Unavailable,
        }
    }

    /// Retries deleting secrets whose earlier deletion failed. Returns how
    /// many remain.
    pub fn retry_pending_secret_deletions(&mut self) -> usize {
        let pending = std::mem::take(&mut self.document.pending_secret_deletions);
        let before = pending.len();
        let mut remaining = Vec::new();
        for secret in pending {
            if self.document.referenced_secrets().contains(&secret) {
                continue;
            }
            if self.secrets.delete(&secret.to_reference()).is_err() {
                remaining.push(secret);
            }
        }
        let changed = remaining.len() != before;
        self.document.pending_secret_deletions = remaining;
        if changed {
            let _ = self.write(&self.document);
        }
        self.document.pending_secret_deletions.len()
    }

    // ----- internals -----

    fn check_unique_and_account(
        &self,
        document: &Document,
        ignoring: Option<&ProfileId>,
        name: &str,
        kind: &ProfileKind,
    ) -> Result<(), ProfileError> {
        if document
            .profiles
            .iter()
            .any(|other| Some(&other.id) != ignoring && same_name(&other.name, name))
        {
            return Err(ProfileError::DuplicateName);
        }
        if let ProfileKind::DevTunnel(tunnel) = kind {
            if let Some(account) = &tunnel.account {
                if !document.accounts.iter().any(|other| other.id == *account) {
                    return Err(ProfileError::UnknownAccount);
                }
            }
        }
        Ok(())
    }

    fn write(&self, document: &Document) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec_pretty(document).map_err(|_| StorageError::Io)?;
        write_atomic(&self.path, &bytes, MAX_FILE_BYTES)
    }

    fn commit(&mut self, mut next: Document) -> Result<(), ProfileError> {
        next.version = CURRENT_VERSION;
        self.write(&next)?;
        self.document = next;
        Ok(())
    }

    /// Deletes `candidates` that nothing references any more; failures are
    /// remembered for retry.
    fn release_unreferenced(&mut self, candidates: Vec<SecretRef>) {
        let mut failed = Vec::new();
        for secret in candidates {
            if self.document.referenced_secrets().contains(&secret)
                || self.document.pending_secret_deletions.contains(&secret)
                || failed.contains(&secret)
            {
                continue;
            }
            if self.secrets.delete(&secret.to_reference()).is_err() {
                failed.push(secret);
            }
        }
        if !failed.is_empty() {
            self.document.pending_secret_deletions.extend(failed);
            let _ = self.write(&self.document);
        }
    }
}

fn position(document: &Document, id: &ProfileId) -> Result<usize, ProfileError> {
    document
        .profiles
        .iter()
        .position(|profile| profile.id == *id)
        .ok_or(ProfileError::NotFound)
}

fn same_name(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}
