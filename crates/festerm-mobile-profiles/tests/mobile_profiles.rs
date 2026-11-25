//! Storage behaviour with a real file system (under the cargo target tmp dir)
//! and the in-memory secret store.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use festerm_mobile_profiles::keys::{generate_ed25519, parse_private_key};
use festerm_mobile_profiles::{
    CredentialStatus, HostKeyError, HostKeyStatus, KeyImportError, KnownHosts, ProfileError,
    ProfileKind, ProfileStore, PublicProfileImport, SshAuth, SshProfile, StorageError,
    PROFILES_FILE,
};
use festerm_secret_store::MemorySecretStore;
use festerm_ssh::HostIdentity;

const FP_A: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const FP_B: &str = "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn dir() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("profiles-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn memory() -> Arc<MemorySecretStore> {
    Arc::new(MemorySecretStore::new())
}

fn ssh(host: &str) -> ProfileKind {
    ProfileKind::Ssh(SshProfile::new(host, "me"))
}

fn files_destination(account: &str) -> festerm_mobile_profiles::DevTunnelFilesProfile {
    festerm_mobile_profiles::DevTunnelFilesProfile {
        account: account.to_owned(),
        locator: "controlled-host.usw2".into(),
        host_id: uuid::Uuid::new_v4().to_string(),
        host_key_fingerprint: FP_A.into(),
        installation: uuid::Uuid::new_v4().to_string(),
        principal: uuid::Uuid::new_v4().to_string(),
    }
}

#[test]
fn files_profiles_roundtrip_without_authority_and_stay_inert_after_account_removal() {
    use festerm_mobile_profiles::AccountProvider;
    use festerm_secret_store::{SecretBytes, SecretStore};
    let directory = dir();
    let secrets = memory();
    let mut store = ProfileStore::open(&directory, secrets.clone()).unwrap();
    let credential = store
        .store_secret(&SecretBytes::from_secret_string(
            "test-only-account-secret".into(),
        ))
        .unwrap();
    let account = store
        .upsert_account(AccountProvider::Github, 42, "owner", credential.clone())
        .unwrap();
    let destination = files_destination(&account);
    let kind = ProfileKind::DevTunnelFiles(destination.clone());
    let id = store.create("Remote files", kind.clone()).unwrap();
    let text = fs::read_to_string(directory.join(PROFILES_FILE)).unwrap();
    assert!(!text.contains("test-only-account-secret"));
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let fields = value["profiles"][0]["kind"].as_object().unwrap();
    assert_eq!(
        fields.len(),
        7,
        "type plus six public destination fields only"
    );
    for forbidden in [
        "credential",
        "policy_epoch",
        "scope",
        "approval",
        "session",
        "stream",
    ] {
        assert!(!fields.contains_key(forbidden));
    }
    let mut reopened = ProfileStore::open(&directory, secrets.clone()).unwrap();
    assert_eq!(reopened.profiles()[0].kind, kind);
    reopened.delete(&id).unwrap();
    assert!(
        secrets.get(&credential.to_reference()).is_ok(),
        "profile does not own the account credential"
    );
    reopened
        .create(
            "Remote files",
            ProfileKind::DevTunnelFiles(destination.clone()),
        )
        .unwrap();
    reopened.remove_account(&account).unwrap();
    let reopened = ProfileStore::open(&directory, secrets.clone()).unwrap();
    assert!(reopened.accounts().is_empty());
    assert_eq!(
        reopened.profiles()[0].kind,
        ProfileKind::DevTunnelFiles(destination)
    );
    assert!(secrets.get(&credential.to_reference()).is_err());
    let mut reopened = reopened;
    assert!(matches!(
        reopened.create(
            "Unbound files",
            ProfileKind::DevTunnelFiles(files_destination("github-999"))
        ),
        Err(ProfileError::UnknownAccount)
    ));
}

#[test]
fn files_profiles_reject_unbounded_unpinned_or_authority_shaped_metadata() {
    use festerm_mobile_profiles::{AccountProvider, SecretRef};
    let mut store = ProfileStore::open(&dir(), memory()).unwrap();
    let account = store
        .upsert_account(
            AccountProvider::Github,
            42,
            "owner",
            SecretRef::parse("550e8400-e29b-41d4-a716-446655440000").unwrap(),
        )
        .unwrap();
    let destination = files_destination(&account);
    let valid = serde_json::to_value(ProfileKind::DevTunnelFiles(destination)).unwrap();
    for (field, invalid) in [
        ("account", "Bearer private-value".to_owned()),
        ("locator", "https://untrusted.example/path".to_owned()),
        ("locator", "x".repeat(202)),
        ("host_id", "not-an-id".to_owned()),
        (
            "installation",
            "550e8400-e29b-11d4-a716-446655440000".to_owned(),
        ),
        (
            "principal",
            "550E8400-E29B-41D4-A716-446655440000".to_owned(),
        ),
        ("host_key_fingerprint", "".to_owned()),
        ("host_key_fingerprint", "SHA256:x".to_owned()),
        ("host_key_fingerprint", format!("SHA256:{}", "A".repeat(44))),
        ("host_key_fingerprint", format!("SHA256:{}", "_".repeat(43))),
        ("host_key_fingerprint", format!("SHA256:{}", "-".repeat(43))),
        (
            "host_key_fingerprint",
            format!("SHA256:{}=", "A".repeat(42)),
        ),
    ] {
        let mut candidate = valid.clone();
        candidate[field] = invalid.clone().into();
        let kind = serde_json::from_value(candidate).unwrap();
        let error = store.create("Rejected", kind).unwrap_err();
        assert!(matches!(error, ProfileError::Validation(_)), "{field}");
        assert!(!error.to_string().contains(&invalid) || invalid.is_empty());
    }
    for field in ["credential", "scope", "policy_epoch", "approval"] {
        let mut candidate = valid.clone();
        candidate[field] = "untrusted extra".into();
        assert!(serde_json::from_value::<ProfileKind>(candidate).is_err());
    }
    assert!(store.profiles().is_empty());
}

#[test]
fn v1_migration_preserves_existing_profile_account_and_recipe_metadata() {
    use festerm_mobile_profiles::host_recipes::{
        AccountReference, HostId, HostReference, MetadataCommand, PublicRecipe, Recipe,
    };
    use festerm_mobile_profiles::{AccountProvider, DevTunnelProfile, SftpProfile};
    use festerm_secret_store::{SecretBytes, SecretStore};
    let directory = dir();
    let secrets = memory();
    let mut store = ProfileStore::open(&directory, secrets.clone()).unwrap();
    let credential = store
        .store_secret(&SecretBytes::from_secret_string(
            "migration-only-secret".into(),
        ))
        .unwrap();
    let account = store
        .upsert_account(AccountProvider::Github, 42, "owner", credential.clone())
        .unwrap();
    store.create("Existing SSH", ssh("existing.test")).unwrap();
    store
        .create(
            "Existing tunnel",
            ProfileKind::DevTunnel(DevTunnelProfile::named("old-host.usw2", Some(account))),
        )
        .unwrap();
    store
        .create(
            "Existing SFTP",
            ProfileKind::Sftp(SftpProfile::new("files.test", "owner")),
        )
        .unwrap();
    let recipe = PublicRecipe::persistent(
        "Existing recipe",
        HostReference {
            id: HostId::new(),
            account: AccountReference::Github { account_id: 42 },
        },
        Recipe {
            shell: None,
            args: Vec::new(),
            cwd: None,
        },
    )
    .unwrap();
    store
        .apply_host_recipe_command(MetadataCommand::Save(recipe))
        .unwrap();
    let path = directory.join(PROFILES_FILE);
    let mut before: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    before["version"] = 1.into();
    let v1_bytes = serde_json::to_vec_pretty(&before).unwrap();
    fs::write(&path, &v1_bytes).unwrap();
    let mut reopened = ProfileStore::open(&directory, secrets.clone()).unwrap();
    assert_eq!(reopened.load_report().migrated_from, Some(1));
    assert_eq!(reopened.load_report().dropped_entries, 0);
    assert!(!reopened.load_report().started_empty_after_corruption);
    assert_eq!(
        fs::read(&path).unwrap(),
        v1_bytes,
        "opening does not rewrite the original"
    );
    let ids = reopened
        .profiles()
        .iter()
        .map(|p| p.id.clone())
        .collect::<Vec<_>>();
    reopened.reorder(&ids).unwrap();
    before["version"] = 2.into();
    let after: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(
        after, before,
        "only the schema version changes on next save"
    );
    assert!(secrets.get(&credential.to_reference()).is_ok());
}

#[test]
fn crud_order_recents_and_reopen() {
    let dir = dir();
    let mut store = ProfileStore::open(&dir, memory()).unwrap();
    let a = store.create("Alpha", ssh("a.example.com")).unwrap();
    let b = store.create("Beta", ssh("b.example.com")).unwrap();
    let c = store.create("Gamma", ssh("c.example.com")).unwrap();
    assert!(matches!(
        store.create("alpha", ssh("x.example.com")),
        Err(ProfileError::DuplicateName)
    ));
    assert_eq!(store.suggest_name("Alpha"), "Alpha 2");

    store.reorder(&[c.clone(), a.clone(), b.clone()]).unwrap();
    assert!(matches!(
        store.reorder(std::slice::from_ref(&a)),
        Err(ProfileError::InvalidOrder)
    ));
    store.move_profile(&c, 99).unwrap();
    store.mark_used(&b).unwrap();
    store.update(&a, "Alpha2", ssh("a2.example.com")).unwrap();

    let reopened = ProfileStore::open(&dir, memory()).unwrap();
    let names: Vec<_> = reopened
        .profiles()
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["Alpha2", "Beta", "Gamma"]);
    assert_eq!(reopened.recents(5).len(), 1);
    assert_eq!(reopened.recents(5)[0].id, b);

    let mut store = reopened;
    store.delete(&b).unwrap();
    assert!(matches!(store.delete(&b), Err(ProfileError::NotFound)));
    assert_eq!(store.profiles().len(), 2);
}

#[test]
fn secrets_are_never_written_and_deleted_with_profile() {
    let dir = dir();
    let secrets = memory();
    let mut store = ProfileStore::open(&dir, secrets.clone()).unwrap();
    let auth = store.store_password("hunter2-secret".to_owned()).unwrap();
    let mut profile = SshProfile::new("a.example.com", "me");
    profile.auth = auth.clone();
    let id = store.create("Box", ProfileKind::Ssh(profile)).unwrap();
    assert_eq!(store.credential_status(&auth), CredentialStatus::Present);
    assert!(!fs::read_to_string(dir.join(PROFILES_FILE))
        .unwrap()
        .contains("hunter2"));

    store.update(&id, "Box", ssh("a.example.com")).unwrap();
    assert_eq!(store.credential_status(&auth), CredentialStatus::Missing);

    let auth = store.store_password("second-secret".to_owned()).unwrap();
    let mut profile = SshProfile::new("a.example.com", "me");
    profile.auth = auth.clone();
    store.update(&id, "Box", ProfileKind::Ssh(profile)).unwrap();
    store.delete(&id).unwrap();
    assert_eq!(store.credential_status(&auth), CredentialStatus::Missing);
}

#[test]
fn failed_create_releases_the_secret() {
    let mut store = ProfileStore::open(&dir(), memory()).unwrap();
    let auth = store.store_password("pw".to_owned()).unwrap();
    let mut profile = SshProfile::new("bad host!", "me");
    profile.auth = auth.clone();
    assert!(store.create("Bad", ProfileKind::Ssh(profile)).is_err());
    assert_eq!(store.credential_status(&auth), CredentialStatus::Missing);
}

#[test]
fn corrupt_file_is_quarantined_and_newer_version_refused() {
    let dir = dir();
    fs::write(dir.join(PROFILES_FILE), b"{not json").unwrap();
    let store = ProfileStore::open(&dir, memory()).unwrap();
    assert!(store.load_report().started_empty_after_corruption);
    assert!(store.profiles().is_empty());

    let newer = dir.join("newer");
    fs::create_dir_all(&newer).unwrap();
    fs::write(
        newer.join(PROFILES_FILE),
        br#"{"version":999,"profiles":[],"accounts":[]}"#,
    )
    .unwrap();
    match ProfileStore::open(&newer, memory()) {
        Err(ProfileError::Storage(StorageError::UnsupportedVersion { found: 999 })) => {}
        other => panic!("unexpected {:?}", other.err()),
    }
    assert!(fs::read_to_string(newer.join(PROFILES_FILE))
        .unwrap()
        .contains("999"));
}

#[test]
fn one_bad_entry_does_not_lose_the_rest() {
    let dir = dir();
    let mut store = ProfileStore::open(&dir, memory()).unwrap();
    store.create("Good", ssh("a.example.com")).unwrap();
    let text = fs::read_to_string(dir.join(PROFILES_FILE)).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["profiles"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"id": "nope", "name": 5}));
    fs::write(dir.join(PROFILES_FILE), value.to_string()).unwrap();
    let store = ProfileStore::open(&dir, memory()).unwrap();
    assert_eq!(store.profiles().len(), 1);
    assert_eq!(store.load_report().dropped_entries, 1);
    assert!(store.load_report().quarantined_file.is_some());
}

#[test]
fn known_hosts_changed_key_is_a_hard_refusal() {
    let dir = dir();
    let identity = HostIdentity::new("Example.com", 22).unwrap();
    let mut hosts = KnownHosts::open(&dir).unwrap();
    assert_eq!(
        hosts.verify(&identity, FP_A).unwrap(),
        HostKeyStatus::Unknown
    );
    hosts.trust(&identity, FP_A).unwrap();

    let mut hosts = KnownHosts::open(&dir).unwrap();
    assert_eq!(
        hosts.verify(&identity, FP_A).unwrap(),
        HostKeyStatus::Trusted
    );
    assert!(matches!(
        hosts.verify(&identity, FP_B),
        Err(HostKeyError::Changed { .. })
    ));
    assert!(matches!(
        hosts.trust(&identity, FP_B),
        Err(HostKeyError::Changed { .. })
    ));
    assert_eq!(hosts.trusted_fingerprint(&identity), Some(FP_A));
    let other_port = HostIdentity::new("example.com", 2222).unwrap();
    assert_eq!(
        hosts.verify(&other_port, FP_B).unwrap(),
        HostKeyStatus::Unknown
    );

    assert!(hosts.forget(&identity).unwrap());
    hosts.trust(&identity, FP_B).unwrap();
    assert!(matches!(
        hosts.verify(&identity, "not-a-fingerprint"),
        Err(HostKeyError::InvalidFingerprint)
    ));
}

#[test]
fn key_import_generate_and_store() {
    let encrypted = include_str!("fixtures/enc_ed25519");
    assert!(matches!(
        parse_private_key(encrypted, None),
        Err(KeyImportError::PassphraseRequired)
    ));
    assert!(matches!(
        parse_private_key(encrypted, Some("wrong")),
        Err(KeyImportError::WrongPassphraseOrInvalid)
    ));
    let key = parse_private_key(encrypted, Some("correct horse")).unwrap();
    assert!(key.info().fingerprint.starts_with("SHA256:"));
    assert_eq!(key.info().comment, "test-only@fesTerm");
    assert!(key.info().public_key.starts_with("ssh-ed25519 "));

    let pkcs8 = parse_private_key(include_str!("fixtures/pkcs8_ed25519"), None).unwrap();
    assert!(pkcs8.info().public_key.starts_with("ssh-ed25519 "));
    assert!(matches!(
        parse_private_key(&key.info().public_key.clone(), None),
        Err(KeyImportError::LooksLikePublicKey)
    ));
    assert!(matches!(
        parse_private_key("  ", None),
        Err(KeyImportError::Empty)
    ));

    let secrets = memory();
    let store = ProfileStore::open(&dir(), secrets.clone()).unwrap();
    let generated = generate_ed25519("iPhone").unwrap();
    let auth = store.store_key(generated).unwrap();
    let SshAuth::Key { key, .. } = &auth else {
        panic!()
    };
    assert_eq!(key.comment, "iPhone");
    assert_eq!(store.credential_status(&auth), CredentialStatus::Present);
}

fn import_json() -> serde_json::Value {
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

#[test]
fn dev_tunnel_import_defaults_take_over_and_never_replaces() {
    let dir = dir();
    let mut store = ProfileStore::open(&dir, memory()).unwrap();
    let text = import_json().to_string();
    let import = PublicProfileImport::parse(&text).unwrap();
    assert!(import.host_fingerprint().starts_with("SHA256:"));
    let id = store.import_dev_tunnel(import, None).unwrap();
    let ProfileKind::DevTunnel(tunnel) = &store.get(&id).unwrap().kind else {
        panic!()
    };
    assert!(tunnel.take_over_if_attached);
    assert!(store.host_recipes().is_empty());
    let festerm_mobile_profiles::TunnelSource::Imported { destination } = &tunnel.source else {
        panic!("legacy import must keep its exact destination");
    };
    assert_eq!(destination.target.generation, "123");

    let again = PublicProfileImport::parse(&text).unwrap();
    assert!(matches!(
        store.import_dev_tunnel(again, None),
        Err(ProfileError::DuplicateName)
    ));
    let again = PublicProfileImport::parse(&text).unwrap();
    let name = store.suggest_name("server-one");
    store.import_dev_tunnel_as(&name, again, None).unwrap();
    assert_eq!(
        ProfileStore::open(&dir, memory()).unwrap().profiles().len(),
        2
    );

    let mut unknown = import_json();
    unknown["extra"] = serde_json::json!(1);
    assert!(PublicProfileImport::parse(&unknown.to_string()).is_err());
    let mut http = import_json();
    http["destination"]["endpoint"]["clientRelayUri"] = serde_json::json!("ws://evil.example/x");
    assert!(PublicProfileImport::parse(&http.to_string()).is_err());
    assert!(PublicProfileImport::parse("garbage").is_err());
}
