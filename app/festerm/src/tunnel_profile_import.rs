//! Public setup metadata only. Parsing/preview never performs authentication or I/O.
use std::{fmt, fs, io::Write, path::Path};

use festerm_config::{Configuration, Profile};
use festerm_tunnel_bridge::{
    files::PathStyle,
    protocol::Target,
    relay::{self, Destination},
};
use serde::{Deserialize, Serialize};

pub(crate) const MAX_IMPORT_BYTES: usize = 64 * 1024;
const INVALID: &str = "Invalid public setup JSON. Use version 1 public metadata only (at most 64 KiB); credentials and unknown fields are forbidden.";
const SAVE_FAILED: &str =
    "The imported profile could not be saved. No profile or credential was changed.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    festerm_profile_version: u16,
    name: String,
    destination: PublicDestination,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicDestination {
    #[serde(default)]
    file_read: bool,
    #[serde(default)]
    path_style: PathStyle,
    version: u16,
    tunnel_id: String,
    cluster: String,
    port: u16,
    target: Target,
    endpoint: PublicEndpoint,
}

// The SDK endpoint decoder ignores unknown fields and accepts command/URL
// templates. The paste boundary deliberately supports only relay binding data.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PublicEndpoint {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    connection_mode: String,
    host_id: String,
    host_public_keys: Vec<String>,
    client_relay_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_relay_uri: Option<String>,
}

#[derive(Clone)]
pub struct PublicProfileImport {
    name: String,
    destination: Destination,
    fingerprint: String,
}

impl fmt::Debug for PublicProfileImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicProfileImport(<validated public metadata>)")
    }
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn public_relay_uri(value: &str) -> bool {
    if value.len() > 4096
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
        || relay::validate_relay_url(value).is_err()
    {
        return false;
    }
    reqwest::Url::parse(value).is_ok_and(|url| url.query().is_none())
}

impl PublicProfileImport {
    pub(crate) fn parse(text: &str) -> Result<Self, &'static str> {
        if text.len() > MAX_IMPORT_BYTES {
            return Err(INVALID);
        }
        let value: Envelope = serde_json::from_str(text).map_err(|_| INVALID)?;
        let destination = &value.destination;
        let endpoint = &destination.endpoint;
        if value.festerm_profile_version != 1
            || destination.version != 1
            || Profile::dev_tunnel(&value.name, "managed-public-destination.json").is_err()
            || !identifier(&destination.tunnel_id, 128)
            || !identifier(&destination.cluster, 32)
            || destination.port == 0
            || !identifier(&destination.target.name, 128)
            || destination.target.pid == 0
            || destination.target.generation.is_empty()
            || destination.target.generation.len() > 39
            || !destination
                .target
                .generation
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            || !destination
                .target
                .generation
                .parse::<u128>()
                .is_ok_and(|generation| generation > 0)
            || endpoint.connection_mode != "TunnelRelay"
            || !identifier(&endpoint.host_id, 128)
            || endpoint
                .id
                .as_deref()
                .is_some_and(|id| !identifier(id, 200))
            || endpoint.host_public_keys.len() != 1
            || !public_relay_uri(&endpoint.client_relay_uri)
            || endpoint
                .host_relay_uri
                .as_deref()
                .is_some_and(|uri| !public_relay_uri(uri))
        {
            return Err(INVALID);
        }
        let fingerprint =
            relay::host_fingerprint(&endpoint.host_public_keys[0]).map_err(|_| INVALID)?;
        let destination =
            serde_json::from_value(serde_json::to_value(value.destination).map_err(|_| INVALID)?)
                .map_err(|_| INVALID)?;
        Ok(Self {
            name: value.name,
            destination,
            fingerprint,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn destination(&self) -> &Destination {
        &self.destination
    }
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Called only by the confirmed application command. Unique managed files
    /// avoid replacing any profile's immutable destination or live launch pin.
    pub(crate) fn save(
        &self,
        configuration: &Configuration,
        directory: &Path,
        persist: impl FnOnce(&Configuration) -> bool,
    ) -> Result<Configuration, &'static str> {
        if configuration.profile(&self.name).is_some() {
            return Err("A profile with this name already exists. Import never replaces a profile; change the public profile name and preview again.");
        }
        fs::create_dir_all(directory).map_err(|_| SAVE_FAILED)?;
        if fs::symlink_metadata(directory)
            .map_err(|_| SAVE_FAILED)?
            .file_type()
            .is_symlink()
        {
            return Err(SAVE_FAILED);
        }
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| SAVE_FAILED)?;
        let filename: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = directory.join(format!("{filename}.json"));
        let profile = Profile::dev_tunnel(&self.name, path.to_str().ok_or(SAVE_FAILED)?)
            .map_err(|_| SAVE_FAILED)?;
        let replacement = configuration
            .with_profile(profile)
            .map_err(|_| SAVE_FAILED)?;
        let bytes = serde_json::to_vec_pretty(&self.destination).map_err(|_| SAVE_FAILED)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| SAVE_FAILED)?;
        let saved = file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .is_ok()
            && persist(&replacement);
        if !saved {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(SAVE_FAILED);
        }
        Ok(replacement)
    }
}

#[cfg(test)]
pub(crate) fn fixture_json() -> serde_json::Value {
    serde_json::json!({
        "festerm_profile_version": 1, "name": "server-one",
        "destination": {
            "version": 1, "tunnel_id": "owned", "cluster": "usw3", "port": 47461,
            "target": {"name": "owned-shell", "pid": 77, "generation": "123"},
            "endpoint": {
                "connectionMode": "TunnelRelay", "hostId": "owned-host",
                "hostPublicKeys": ["AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea"],
                "clientRelayUri": "wss://usw3.rel.tunnels.api.visualstudio.com/owned"
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{configuration_startup::ConfigurationReloader, tunnel_activation::Launch};
    use festerm_secret_store::{MemorySecretStore, SecretStore};

    #[test]
    fn public_tunnel_import_parser_enforces_strict_schema_bounds_and_public_bindings() {
        let value = fixture_json();
        let text = value.to_string();
        let parsed = PublicProfileImport::parse(&text).unwrap();
        assert_eq!(parsed.name(), "server-one");
        assert!(parsed.fingerprint().starts_with("SHA256:"));
        let padded = format!("{text}{}", " ".repeat(MAX_IMPORT_BYTES - text.len()));
        assert!(PublicProfileImport::parse(&padded).is_ok());
        assert!(PublicProfileImport::parse(&(padded + " ")).is_err());
        assert!(PublicProfileImport::parse("").is_err());
        assert!(PublicProfileImport::parse(&text.replacen(
            "\"name\":",
            "\"name\":\"duplicate\",\"name\":",
            1
        ))
        .is_err());

        for (pointer, replacement) in [
            ("/festerm_profile_version", serde_json::json!(2)),
            ("/name", serde_json::json!(" leading")),
            ("/destination/version", serde_json::json!(2)),
            ("/destination/port", serde_json::json!(0)),
            ("/destination/tunnel_id", serde_json::json!("../../outside")),
            ("/destination/cluster", serde_json::json!("bad/cluster")),
            ("/destination/target/name", serde_json::json!("../session")),
            ("/destination/target/pid", serde_json::json!(0)),
            (
                "/destination/target/generation",
                serde_json::json!("not-a-generation"),
            ),
            (
                "/destination/target/generation",
                serde_json::json!("340282366920938463463374607431768211456"),
            ),
            (
                "/destination/endpoint/connectionMode",
                serde_json::json!("LocalNetwork"),
            ),
            (
                "/destination/endpoint/hostId",
                serde_json::json!("bad\nhost"),
            ),
            (
                "/destination/endpoint/hostPublicKeys",
                serde_json::json!([]),
            ),
            (
                "/destination/endpoint/hostPublicKeys",
                serde_json::json!(["bad-key"]),
            ),
            (
                "/destination/endpoint/clientRelayUri",
                serde_json::json!("ws://usw3.rel.tunnels.api.visualstudio.com/owned"),
            ),
            (
                "/destination/endpoint/clientRelayUri",
                serde_json::json!("wss://evil.example/owned"),
            ),
            (
                "/destination/endpoint/clientRelayUri",
                serde_json::json!("wss://usw3.rel.tunnels.api.visualstudio.com.evil.example/owned"),
            ),
            (
                "/destination/endpoint/clientRelayUri",
                serde_json::json!("wss://token@usw3.rel.tunnels.api.visualstudio.com/owned"),
            ),
            (
                "/destination/endpoint/clientRelayUri",
                serde_json::json!("wss://usw3.rel.tunnels.api.visualstudio.com/owned?token=secret"),
            ),
            (
                "/destination/endpoint/clientRelayUri",
                serde_json::json!("wss://usw3.rel.tunnels.api.visualstudio.com/owned#secret"),
            ),
        ] {
            let mut invalid = value.clone();
            *invalid.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                PublicProfileImport::parse(&invalid.to_string()).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn public_tunnel_import_refuses_secret_fields_at_every_level_without_echoing_input() {
        for pointer in [
            "",
            "/destination",
            "/destination/target",
            "/destination/endpoint",
        ] {
            for field in [
                "token",
                "connectToken",
                "hostToken",
                "access_token",
                "refresh_token",
                "password",
                "credential_reference",
                "destination_file",
                "unknown",
            ] {
                let mut value = fixture_json();
                value
                    .pointer_mut(pointer)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(field.into(), serde_json::json!("private-canary"));
                let error = PublicProfileImport::parse(&value.to_string()).unwrap_err();
                assert!(!error.contains("private-canary"));
            }
        }
        let mut value = fixture_json();
        value["destination"]["endpoint"]["hostPublicKeys"] = serde_json::json!([
            value["destination"]["endpoint"]["hostPublicKeys"][0],
            value["destination"]["endpoint"]["hostPublicKeys"][0]
        ]);
        assert!(PublicProfileImport::parse(&value.to_string()).is_err());
    }

    #[test]
    fn public_tunnel_import_command_persists_across_restart_without_auth_or_overwrite() {
        let root = tempfile::Builder::new()
            .prefix("tunnel-import-tests-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let config_path = root.path().join("config.toml");
        let reloader = ConfigurationReloader::from_path_for_test(config_path.clone());
        let import = PublicProfileImport::parse(&fixture_json().to_string()).unwrap();
        let other_reference = festerm_secret_store::SecretReference::generate();
        let other = Profile::named_dev_tunnel("native-other", "other.usw3")
            .unwrap()
            .with_tunnel_account(
                "github-owner",
                festerm_secret_store::SecretReference::generate(),
                festerm_config::DevTunnelHost {
                    tunnel_id: "other".into(),
                    cluster: "usw3".into(),
                    host_id: "other-host".into(),
                    public_key: "independent-native-pin".into(),
                    port: 47461,
                },
            )
            .unwrap();
        let legacy = Profile::dev_tunnel("legacy-other", "legacy-destination.json")
            .unwrap()
            .with_tunnel_connect_credential(other_reference.duplicate_for_transport())
            .unwrap();
        let configuration = Configuration::new(Vec::new())
            .unwrap()
            .with_account(
                festerm_config::DevTunnelAccount::new(
                    "github-owner",
                    festerm_config::AccountProvider::Github,
                    42,
                    "owner",
                    festerm_secret_store::SecretReference::generate(),
                )
                .unwrap(),
            )
            .unwrap()
            .0
            .with_profile(other.clone())
            .unwrap()
            .with_profile(legacy.clone())
            .unwrap();
        let saved = import
            .save(
                &configuration,
                &reloader.tunnel_import_directory().unwrap(),
                |replacement| reloader.save_configuration(replacement).is_ok(),
            )
            .unwrap();
        let restarted = Configuration::parse(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(saved, restarted);
        assert_eq!(restarted.profile("native-other"), Some(&other));
        assert_eq!(restarted.profile("legacy-other"), Some(&legacy));
        let profile = restarted.profile("server-one").unwrap();
        assert!(profile.credential_reference().is_none());
        assert!(profile.as_dev_tunnel().unwrap().account().is_none());
        let path = Path::new(profile.as_dev_tunnel().unwrap().destination_file());
        assert!(path.starts_with(root.path()));
        assert_eq!(
            festerm_tunnel_bridge::transport::read_destination(path)
                .unwrap()
                .target
                .generation,
            "123"
        );
        let store = MemorySecretStore::new();
        assert!(Launch::from_profile(profile, &store).is_err());
        let reference = store
            .put(
                &crate::tabs::TunnelConnectToken::new("separate-connect-canary".into())
                    .into_stored_secret(),
            )
            .unwrap();
        let authenticated = profile
            .clone()
            .with_tunnel_connect_credential(reference)
            .unwrap();
        assert!(Launch::from_profile(&authenticated, &store).is_ok());
        assert!(!fs::read_to_string(path)
            .unwrap()
            .contains("separate-connect-canary"));
        assert!(!fs::read_to_string(&config_path)
            .unwrap()
            .contains("separate-connect-canary"));
        let original = fs::read(&config_path).unwrap();
        assert!(import
            .save(&restarted, root.path(), |_| panic!(
                "duplicate must never save"
            ))
            .is_err());
        assert_eq!(fs::read(&config_path).unwrap(), original);
    }

    #[test]
    fn public_tunnel_import_rolls_back_managed_metadata_when_configuration_save_fails() {
        let root = tempfile::Builder::new()
            .prefix("tunnel-import-failure-tests-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let import = PublicProfileImport::parse(&fixture_json().to_string()).unwrap();
        assert!(import
            .save(&Configuration::empty(), root.path(), |_| false)
            .is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
