use festerm_tunnel_bridge::{protocol::invalid, relay::Destination, transport};
use std::{fmt, io, path::Path};

pub struct Launch {
    pub destination: Destination,
    pub account: Option<festerm_tunnel_client::identity::Account>,
    token: zeroize::Zeroizing<String>,
}

impl fmt::Debug for Launch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExperimentalDevTunnel(<private credential>)")
    }
}

impl Launch {
    pub fn from_destination(destination: Destination, token: zeroize::Zeroizing<String>) -> Self {
        Self {
            destination,
            token,
            account: None,
        }
    }
    pub fn with_account(mut self, account: festerm_tunnel_client::identity::Account) -> Self {
        self.account = Some(account);
        self
    }
    pub fn from_secret(path: &Path, secret: festerm_secret_store::SecretBytes) -> io::Result<Self> {
        let token = secret.with_bytes(|bytes| {
            if bytes.is_empty()
                || bytes.len() > 16 * 1024
                || bytes.iter().any(|byte| !byte.is_ascii_graphic())
            {
                return Err(invalid());
            }
            Ok(zeroize::Zeroizing::new(
                std::str::from_utf8(bytes)
                    .map_err(|_| invalid())?
                    .to_owned(),
            ))
        })?;
        let destination = transport::read_destination(path)?;
        Ok(Self {
            destination,
            token,
            account: None,
        })
    }

    pub fn from_profile(
        profile: &festerm_config::Profile,
        store: &dyn festerm_secret_store::SecretStore,
    ) -> io::Result<Self> {
        let profile = profile.as_dev_tunnel().ok_or_else(invalid)?;
        let reference = profile.credential_reference().ok_or_else(|| io::Error::other("This Dev Tunnel profile has no stored connect token. Edit the profile to store one, or connect once."))?;
        let secret = store
            .get(reference)
            .map_err(|_| io::Error::other("Native connect credential unavailable."))?;
        let token = secret
            .with_bytes(|bytes| {
                bytes
                    .strip_prefix(crate::tabs::TUNNEL_TOKEN_STORE_PREFIX)
                    .map(festerm_secret_store::SecretBytes::copy_from_slice)
            })
            .ok_or_else(invalid)?;
        Self::from_secret(Path::new(profile.destination_file()), token)
    }

    pub fn start(
        self,
        notifier: std::sync::Arc<dyn festerm_session::SessionEventNotifier>,
    ) -> io::Result<transport::ClientSession> {
        transport::start_client(self.destination, self.token, notifier)
    }
}

pub fn parse_env() -> io::Result<Option<Launch>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args
        .first()
        .is_none_or(|arg| arg != "--experimental-dev-tunnel")
    {
        return Ok(None);
    }
    if args.len() < 2
        || args.len() > 3
        || args.get(2).is_some_and(|arg| arg != "--tunnel-token-stdin")
    {
        return Err(invalid());
    }
    let destination = transport::read_destination(Path::new(&args[1]))?;
    let token = transport::read_token(args.len() == 3)?;
    Ok(Some(Launch {
        destination,
        token,
        account: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_config::{Configuration, Profile};
    use festerm_secret_store::{MemorySecretStore, SecretBytes, SecretStore};
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self::for_account("owned")
        }
        fn for_account(tunnel: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::current_dir().unwrap().join(format!(
                "dev-tunnel-profile-{}-{nonce}.json",
                std::process::id()
            ));
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            serde_json::to_writer(file, &serde_json::json!({
                "version":1,"tunnel_id":tunnel,"cluster":"usw3","port":47461,
                "target":{"name":"owned-shell","pid":77,"generation":"123"},
                "endpoint":{"connectionMode":"TunnelRelay","hostId":"owned-host","hostPublicKeys":["owned-pin"],
                    "clientRelayUri":"wss://usw3.rel.tunnels.api.visualstudio.com/owned"}
            })).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn stored(store: &MemorySecretStore, value: &str) -> festerm_secret_store::SecretReference {
        store
            .put(&crate::tabs::TunnelConnectToken::new(value.into()).into_stored_secret())
            .unwrap()
    }

    #[test]
    fn dev_tunnel_profiles_isolate_accounts_and_pin_live_launches_across_rotation() {
        let fixture = Fixture::new();
        let second_fixture = Fixture::for_account("second-owned");
        let store = MemorySecretStore::new();
        let first = Profile::dev_tunnel("github-account-one", fixture.0.to_str().unwrap())
            .unwrap()
            .with_tunnel_connect_credential(stored(&store, "account-one-connect"))
            .unwrap();
        let second = Profile::dev_tunnel("github-account-two", second_fixture.0.to_str().unwrap())
            .unwrap()
            .with_tunnel_connect_credential(stored(&store, "account-two-connect"))
            .unwrap();
        let configuration = Configuration::new(vec![first.clone(), second.clone()]).unwrap();
        let (live_one, live_two) = std::thread::scope(|scope| {
            let one = scope.spawn(|| Launch::from_profile(&first, &store).unwrap());
            let two = scope.spawn(|| Launch::from_profile(&second, &store).unwrap());
            (one.join().unwrap(), two.join().unwrap())
        });
        let same_profile_tab = Launch::from_profile(&first, &store).unwrap();
        let rotated = configuration
            .with_tunnel_connect_credential(
                first.identifier(),
                stored(&store, "rotated-account-one"),
            )
            .unwrap();
        let next_one =
            Launch::from_profile(rotated.profile(first.identifier()).unwrap(), &store).unwrap();
        assert_eq!(&*live_one.token, "account-one-connect");
        assert_eq!(&*same_profile_tab.token, "account-one-connect");
        assert_eq!(&*live_two.token, "account-two-connect");
        assert_eq!(&*next_one.token, "rotated-account-one");
        assert_eq!(rotated.profile(second.identifier()), Some(&second));
        assert_eq!(live_one.destination.target.generation, "123");
        assert_eq!(live_one.destination.tunnel_id, "owned");
        assert_eq!(live_two.destination.tunnel_id, "second-owned");
        assert_eq!(live_two.destination.endpoint.host_id, "owned-host");
        let metadata = rotated.to_toml().unwrap();
        assert!(metadata.contains("kind = \"dev_tunnel\""));
        assert!(!metadata.contains("account-one-connect"));
        assert!(!format!("{live_one:?}").contains("account-one-connect"));
        assert!(Configuration::parse(&metadata).is_ok());
    }

    #[test]
    fn dev_tunnel_profile_refuses_other_secret_kinds_and_missing_credentials() {
        let fixture = Fixture::new();
        let store = MemorySecretStore::new();
        let profile = Profile::dev_tunnel("owned", fixture.0.to_str().unwrap()).unwrap();
        assert!(Launch::from_profile(&profile, &store).is_err());
        let wrong_kind = store
            .put(&SecretBytes::copy_from_slice(b"ordinary-ssh-password"))
            .unwrap();
        let profile = profile.with_tunnel_connect_credential(wrong_kind).unwrap();
        assert!(Launch::from_profile(&profile, &store).is_err());
        let transient = Launch::from_secret(
            &fixture.0,
            SecretBytes::copy_from_slice(b"session-only-connect"),
        )
        .unwrap();
        assert_eq!(&*transient.token, "session-only-connect");
        assert!(
            Launch::from_secret(&fixture.0, SecretBytes::copy_from_slice(b"bad\r\ntoken")).is_err()
        );
    }
}
