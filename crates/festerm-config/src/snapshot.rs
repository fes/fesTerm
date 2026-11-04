//! Host-side review data, not a launch authorization or an admission fence.

use std::{fmt, sync::Arc};

use crate::{Configuration, Profile, RemoteProfileKind, SshProfileConfiguration};

/// An in-process identity for one host-owned configuration generation.
///
/// The application must create a fresh value whenever it adopts or mutates
/// configuration, and must never restore an earlier value. This type does not
/// install that discipline or synchronize a configuration with its revision.
/// It is deliberately neither serializable nor deserializable.
#[derive(Clone)]
pub struct ConfigurationRevision(Arc<()>);

impl ConfigurationRevision {
    pub fn fresh() -> Self {
        Self(Arc::new(()))
    }

    fn same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for ConfigurationRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigurationRevision(<opaque>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileSnapshotError {
    MissingProfile,
    UnsupportedProfile,
    InvalidSshConnectionProfile,
}

/// Exact saved profile and separate effective SSH known-host record, looked up
/// using the same connection identity as the native saved-profile launch.
///
/// Capture never retrieves secret-store contents, but full profile data can
/// itself be sensitive, including freeform command arguments. An SSH profile
/// may also contain an opaque credential reference. Treat the snapshot and
/// its contents as trusted-only data: never blindly log them or disclose them
/// to a worker. Its `Debug` output hides *all* captured content; this does not
/// sanitize values returned by its accessors. It must not be supplied by a
/// caller or treated as consent.
///
/// ```compile_fail
/// let _: festerm_config::EffectiveProfileSnapshot = toml::from_str("").unwrap();
/// ```
pub struct EffectiveProfileSnapshot {
    revision: ConfigurationRevision,
    profile: Profile,
    known_host_fingerprint: Option<String>,
}

impl EffectiveProfileSnapshot {
    /// Captures only plain local, plain SSH shell and serial profiles. Durable
    /// sessions, SFTP and Dev Tunnels remain unavailable.
    pub fn capture(
        configuration: &Configuration,
        revision: &ConfigurationRevision,
        identifier: &str,
    ) -> Result<Self, ProfileSnapshotError> {
        let profile = configuration
            .profile(identifier)
            .ok_or(ProfileSnapshotError::MissingProfile)?;
        let fingerprint = match profile {
            Profile::Local(local) if local.persistence().is_none() => None,
            Profile::Serial(_) => None,
            Profile::Ssh(ssh)
                if ssh.persistence().is_none() && ssh.profile_kind() == RemoteProfileKind::Ssh =>
            {
                effective_known_host_fingerprint(configuration, ssh)?.map(str::to_owned)
            }
            _ => return Err(ProfileSnapshotError::UnsupportedProfile),
        };
        Ok(Self {
            revision: revision.clone(),
            profile: profile.clone(),
            known_host_fingerprint: fingerprint,
        })
    }

    /// Full captured profile, for trusted native review and future composition.
    /// It may contain sensitive freeform data or an opaque credential reference;
    /// do not blindly log it or disclose it to a worker.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The separately configured fingerprint, if present. A missing record
    /// means the normal first-seen host-key decision is still required; an
    /// existing record also permits a changed-key prompt in the current host.
    pub fn known_host_fingerprint(&self) -> Option<&str> {
        self.known_host_fingerprint.as_deref()
    }

    /// Exact value comparison and host-supplied generation identity check.
    ///
    /// This is not atomic with later use and is not sufficient for launch.
    /// Reusing a revision across configuration changes permits ABA; the owner
    /// must rotate it on every change and hold a future admission fence.
    pub fn matches_current(
        &self,
        configuration: &Configuration,
        revision: &ConfigurationRevision,
    ) -> bool {
        if !self.revision.same_generation(revision) {
            return false;
        }
        let Some(profile) = configuration.profile(self.profile.identifier()) else {
            return false;
        };
        if profile != &self.profile {
            return false;
        }
        let fingerprint = match profile {
            Profile::Ssh(ssh) => match effective_known_host_fingerprint(configuration, ssh) {
                Ok(fingerprint) => fingerprint,
                Err(_) => return false,
            },
            _ => None,
        };
        fingerprint == self.known_host_fingerprint.as_deref()
    }
}

fn effective_known_host_fingerprint<'a>(
    configuration: &'a Configuration,
    profile: &SshProfileConfiguration,
) -> Result<Option<&'a str>, ProfileSnapshotError> {
    let connection = profile
        .to_connection_profile()
        .map_err(|_| ProfileSnapshotError::InvalidSshConnectionProfile)?;
    let identity = connection.identity();
    Ok(configuration.known_host_fingerprint(identity.host(), identity.port()))
}

impl fmt::Debug for EffectiveProfileSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EffectiveProfileSnapshot(<private>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CredentialKind, PersistenceProviderKind, SshPortForwardConfiguration,
        SshPortForwardDirection,
    };
    use festerm_secret_store::SecretReference;

    const FP: &str = "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ";
    const ROTATED: &str = "SHA256:different0000000000000000000000000000000";
    const SECRET_REF: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn ssh(username: &str) -> Profile {
        Profile::ssh("saved", "ssh.example.test", 22, username, "xterm", 80, 24).unwrap()
    }

    #[test]
    fn snapshot_preserves_complete_profile_and_separate_trust() {
        let forward = SshPortForwardConfiguration::new(
            SshPortForwardDirection::Local,
            "127.0.0.1",
            8080,
            "db.example.test",
            5432,
        )
        .unwrap();
        let Profile::Ssh(profile) = ssh("deploy") else {
            unreachable!()
        };
        let profile = Profile::Ssh(profile.with_port_forwards(vec![forward]).unwrap())
            .with_credential_reference_kind(
                SecretReference::parse(SECRET_REF).unwrap(),
                CredentialKind::PrivateKey,
            )
            .unwrap();
        let config = Configuration::new(vec![profile.clone()])
            .unwrap()
            .with_known_host_trust("ssh.example.test", 22, FP)
            .unwrap();
        let revision = ConfigurationRevision::fresh();
        let snapshot = EffectiveProfileSnapshot::capture(&config, &revision, "saved").unwrap();
        assert_eq!(snapshot.profile(), &profile);
        assert_eq!(snapshot.known_host_fingerprint(), Some(FP));
        assert!(snapshot.matches_current(&config, &revision));
        let changed_forward = config
            .with_profile(
                ssh("deploy")
                    .with_credential_reference_kind(
                        SecretReference::parse(SECRET_REF).unwrap(),
                        CredentialKind::PrivateKey,
                    )
                    .unwrap(),
            )
            .unwrap();
        assert!(!snapshot.matches_current(&changed_forward, &revision));
        let debug = format!("{snapshot:?} {revision:?}");
        for sensitive in [SECRET_REF, "deploy", "ssh.example.test", FP] {
            assert!(!debug.contains(sensitive));
        }
    }

    #[test]
    fn local_sensitive_looking_argument_is_retained_but_not_in_snapshot_debug() {
        let argument = "example-private-value-12345";
        let profile = Profile::local("local", "/bin/echo", vec![argument.into()], None).unwrap();
        let config = Configuration::new(vec![profile]).unwrap();
        let revision = ConfigurationRevision::fresh();
        let snapshot = EffectiveProfileSnapshot::capture(&config, &revision, "local").unwrap();
        assert_eq!(
            snapshot.profile().as_local().unwrap().arguments(),
            &[argument.to_owned()]
        );
        assert!(!format!("{snapshot:?}").contains(argument));
    }

    #[test]
    fn revalidation_checks_profile_account_auth_and_trust_exactly() {
        let config = Configuration::new(vec![ssh("deploy")]).unwrap();
        let revision = ConfigurationRevision::fresh();
        let snapshot = EffectiveProfileSnapshot::capture(&config, &revision, "saved").unwrap();
        assert_eq!(snapshot.known_host_fingerprint(), None);
        let changed_account = config.with_profile(ssh("other")).unwrap();
        let changed_host = config
            .with_profile(
                Profile::ssh("saved", "other.example.test", 22, "deploy", "xterm", 80, 24).unwrap(),
            )
            .unwrap();
        let credential = SecretReference::parse(SECRET_REF).unwrap();
        let changed_auth = config
            .with_ssh_credential("saved", credential, CredentialKind::PrivateKey)
            .unwrap();
        let trusted = config
            .with_known_host_trust("ssh.example.test", 22, FP)
            .unwrap();
        for changed in [&changed_account, &changed_host, &changed_auth, &trusted] {
            assert!(!snapshot.matches_current(changed, &revision));
        }
        let auth_snapshot =
            EffectiveProfileSnapshot::capture(&changed_auth, &revision, "saved").unwrap();
        let changed_kind = config
            .with_ssh_credential(
                "saved",
                SecretReference::parse(SECRET_REF).unwrap(),
                CredentialKind::Password,
            )
            .unwrap();
        let changed_reference = config
            .with_ssh_credential(
                "saved",
                SecretReference::parse("c0f1f3a5-43cc-4bc7-8fd1-9cdb6b4c0001").unwrap(),
                CredentialKind::PrivateKey,
            )
            .unwrap();
        assert!(!auth_snapshot.matches_current(&changed_kind, &revision));
        assert!(!auth_snapshot.matches_current(&changed_reference, &revision));
        let trusted_snapshot =
            EffectiveProfileSnapshot::capture(&trusted, &revision, "saved").unwrap();
        let rotated = trusted
            .with_known_host_trust("ssh.example.test", 22, ROTATED)
            .unwrap();
        assert!(!trusted_snapshot.matches_current(&rotated, &revision));
        assert!(!trusted_snapshot.matches_current(&config, &revision));
    }

    #[test]
    fn ssh_trust_uses_launch_identity_not_raw_saved_host() {
        let raw_host = " SSH.Example.Test ";
        let profile = Profile::ssh("saved", raw_host, 22, "deploy", "xterm", 80, 24).unwrap();
        let config = Configuration::new(vec![profile])
            .unwrap()
            .with_known_host_trust(raw_host, 22, ROTATED)
            .unwrap()
            .with_known_host_trust("ssh.example.test", 22, FP)
            .unwrap();
        let revision = ConfigurationRevision::fresh();
        let snapshot = EffectiveProfileSnapshot::capture(&config, &revision, "saved").unwrap();
        assert_eq!(snapshot.known_host_fingerprint(), Some(FP));
        assert!(snapshot.matches_current(&config, &revision));

        let raw_rotated = config.with_known_host_trust(raw_host, 22, FP).unwrap();
        assert!(snapshot.matches_current(&raw_rotated, &revision));
        let effective_rotated = config
            .with_known_host_trust("ssh.example.test", 22, ROTATED)
            .unwrap();
        assert!(!snapshot.matches_current(&effective_rotated, &revision));
        let raw_only = config.without_known_host("ssh.example.test", 22);
        assert!(!snapshot.matches_current(&raw_only, &revision));
        let raw_only_snapshot =
            EffectiveProfileSnapshot::capture(&raw_only, &revision, "saved").unwrap();
        assert_eq!(raw_only_snapshot.known_host_fingerprint(), None);
        assert!(!raw_only_snapshot.matches_current(&config, &revision));
    }

    #[test]
    fn unrepresentable_ssh_identity_fails_closed() {
        // Simulate an unvalidated configuration at this boundary.
        let invalid: Profile =
            toml::from_str("kind = 'ssh'\nid = 'saved'\nhost = 'bad host'\nusername = 'deploy'\n")
                .unwrap();
        let revision = ConfigurationRevision::fresh();
        let mut config = Configuration::new(vec![ssh("deploy")]).unwrap();
        let mut snapshot = EffectiveProfileSnapshot::capture(&config, &revision, "saved").unwrap();
        config.profiles[0] = invalid.clone();
        assert!(matches!(
            EffectiveProfileSnapshot::capture(&config, &revision, "saved"),
            Err(ProfileSnapshotError::InvalidSshConnectionProfile)
        ));
        snapshot.profile = invalid;
        assert!(!snapshot.matches_current(&config, &revision));
    }

    #[test]
    fn removed_and_recreated_profiles_require_a_new_generation() {
        let config = Configuration::new(vec![ssh("deploy")]).unwrap();
        let old = ConfigurationRevision::fresh();
        let snapshot = EffectiveProfileSnapshot::capture(&config, &old, "saved").unwrap();
        let removed = config.without_profile("saved").unwrap();
        assert!(!snapshot.matches_current(&removed, &old));
        let recreated = removed.with_profile(ssh("deploy")).unwrap();
        // Content comparison alone cannot detect an identical remove/recreate.
        assert!(snapshot.matches_current(&recreated, &old));
        assert!(!snapshot.matches_current(&recreated, &ConfigurationRevision::fresh()));
    }

    #[test]
    fn unsupported_profiles_and_durable_sessions_are_rejected() {
        let local = Profile::local("local", "/bin/sh", vec![], None).unwrap();
        let serial = Profile::serial_with_defaults("serial", "/dev/tty.fixture").unwrap();
        let persistent_local = local
            .clone()
            .with_persistence(PersistenceProviderKind::FestermSessiond, "fixture")
            .unwrap();
        let persistent_ssh = ssh("deploy")
            .with_persistence(PersistenceProviderKind::Tmux, "fixture")
            .unwrap();
        let sftp = Profile::sftp("sftp", "ssh.example.test", 22, "deploy", false).unwrap();
        let tunnel = Profile::dev_tunnel("tunnel", "/fixture/destination").unwrap();
        let revision = ConfigurationRevision::fresh();
        let config = Configuration::new(vec![local, serial, persistent_ssh, sftp, tunnel]).unwrap();
        for id in ["local", "serial"] {
            assert!(EffectiveProfileSnapshot::capture(&config, &revision, id).is_ok());
        }
        let local_snapshot =
            EffectiveProfileSnapshot::capture(&config, &revision, "local").unwrap();
        let changed_local = config
            .with_profile(Profile::local("local", "/bin/sh", vec!["-l".into()], None).unwrap())
            .unwrap();
        assert!(!local_snapshot.matches_current(&changed_local, &revision));
        let serial_snapshot =
            EffectiveProfileSnapshot::capture(&config, &revision, "serial").unwrap();
        let changed_serial = config
            .with_profile(Profile::serial_with_defaults("serial", "/dev/tty.other").unwrap())
            .unwrap();
        assert!(!serial_snapshot.matches_current(&changed_serial, &revision));
        for id in ["saved", "sftp", "tunnel"] {
            assert!(matches!(
                EffectiveProfileSnapshot::capture(&config, &revision, id),
                Err(ProfileSnapshotError::UnsupportedProfile)
            ));
        }
        let config = config.with_profile(persistent_local).unwrap();
        assert!(matches!(
            EffectiveProfileSnapshot::capture(&config, &revision, "local"),
            Err(ProfileSnapshotError::UnsupportedProfile)
        ));
        assert!(matches!(
            EffectiveProfileSnapshot::capture(&config, &revision, "missing"),
            Err(ProfileSnapshotError::MissingProfile)
        ));
    }
}
