use std::{fs, path::PathBuf, sync::Arc};

use festerm_mobile_profiles::{
    host_recipes::{
        AccountReference, ConnectionBehavior, DuplicatePolicy, HostHint, HostId, HostReference,
        MetadataCommand, MetadataOutcome, PublicRecipe, Recipe, RecipeError, RecipeId,
        MAX_PORTABLE_BYTES, MAX_RECIPES,
    },
    AccountProvider, DestinationEndpoint, DestinationTarget, DevTunnelProfile, PathStyle,
    ProfileError, ProfileKind, ProfileStore, PublicDestination, SecretRef, SshProfile,
    TunnelSource, PROFILES_FILE,
};
use festerm_secret_store::{SecretBytes, SecretReference, SecretStore, SecretStoreError};

struct NoVaultAccess;

impl SecretStore for NoVaultAccess {
    fn put(&self, _: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        panic!("recipe metadata must not store credentials")
    }
    fn get(&self, _: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        panic!("recipe metadata must not read credentials")
    }
    fn update(&self, _: &SecretReference, _: &SecretBytes) -> Result<(), SecretStoreError> {
        panic!("recipe metadata must not update credentials")
    }
    fn delete(&self, _: &SecretReference) -> Result<bool, SecretStoreError> {
        panic!("recipe metadata must not delete credentials")
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::current_dir()
            .unwrap()
            .join(format!(".host-recipes-fixture-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn open(&self) -> ProfileStore {
        ProfileStore::open(&self.0, Arc::new(NoVaultAccess)).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn recipe() -> PublicRecipe {
    PublicRecipe::persistent(
        "Shared project",
        HostReference {
            id: HostId::new(),
            account: AccountReference::Github { account_id: 42 },
        },
        Recipe {
            shell: Some("/bin/sh".into()),
            args: vec!["-l".into()],
            cwd: Some("/remote/project".into()),
        },
    )
    .unwrap()
}

#[test]
fn mobile_recipe_commands_preserve_legacy_generation_and_account_binding() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let credential = SecretRef::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let account = store
        .upsert_account(AccountProvider::Github, 42, "owner", credential)
        .unwrap();
    store
        .create(
            "Legacy shell",
            ProfileKind::DevTunnel(DevTunnelProfile {
                account: Some(account),
                session: None,
                take_over_if_attached: false,
                auto_reconnect: false,
                source: TunnelSource::Imported {
                    destination: Box::new(PublicDestination {
                        file_read: false,
                        path_style: PathStyle::Posix,
                        version: 1,
                        tunnel_id: "owned".into(),
                        cluster: "usw3".into(),
                        port: 47461,
                        target: DestinationTarget {
                            name: "legacy-shell".into(),
                            pid: 77,
                            generation: "123".into(),
                        },
                        endpoint: DestinationEndpoint {
                            id: None,
                            connection_mode: "TunnelRelay".into(),
                            host_id: "legacy-host".into(),
                            host_public_keys: vec![
                                "AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea".into()
                            ],
                            client_relay_uri:
                                "wss://usw3.rel.tunnels.api.visualstudio.com/owned".into(),
                            host_relay_uri: None,
                        },
                    }),
                },
            }),
        )
        .unwrap();
    let legacy_profiles = store.profiles().to_vec();
    let accounts = store.accounts().to_vec();
    let public = recipe();
    store
        .apply_host_recipe_command(MetadataCommand::Save(public.clone()))
        .unwrap();
    let mut edited = public.clone();
    edited.launch.cwd = Some("/remote/next-create".into());
    for command in [
        MetadataCommand::Edit(edited),
        MetadataCommand::Duplicate {
            source: public.id.clone(),
            name: "Separate recipe".into(),
            policy: DuplicatePolicy::FreshLogicalSession,
        },
        MetadataCommand::Export(public.id.clone()),
        MetadataCommand::Import(recipe().export().unwrap()),
        MetadataCommand::Delete(public.id.clone()),
    ] {
        store.apply_host_recipe_command(command).unwrap();
        let reopened = fixture.open();
        assert_eq!(reopened.profiles(), legacy_profiles);
        assert_eq!(reopened.accounts(), accounts);
    }
}

#[test]
fn mobile_host_recipe_commands_persist_without_vault_or_legacy_mutation() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let mut original = recipe();
    original.behavior = ConnectionBehavior::ResumeOnly {};
    original.session_name = Some("Remote project".into());
    original.host_hint = Some(HostHint {
        locator: "token-build.usw3".into(),
        name: "Build workstation".into(),
    });
    store
        .apply_host_recipe_command(MetadataCommand::Save(original.clone()))
        .unwrap();
    assert!(store.profiles().is_empty());
    assert!(store.accounts().is_empty());
    let reopened = fixture.open();
    assert_eq!(reopened.host_recipes().get(&original.id), Some(&original));
    let MetadataOutcome::Saved(duplicate_id) = store
        .apply_host_recipe_command(MetadataCommand::Duplicate {
            source: original.id.clone(),
            name: "Separate project".into(),
            policy: DuplicatePolicy::FreshLogicalSession,
        })
        .unwrap()
    else {
        panic!("expected duplicate")
    };
    assert_ne!(
        store
            .host_recipes()
            .get(&duplicate_id)
            .unwrap()
            .logical_session,
        original.logical_session
    );
    let duplicate = store.host_recipes().get(&duplicate_id).unwrap();
    assert_eq!(duplicate.session_name, original.session_name);
    assert_eq!(duplicate.host_hint, original.host_hint);
    let before_export = fs::read(fixture.0.join(PROFILES_FILE)).unwrap();
    let MetadataOutcome::Exported(text) = store
        .apply_host_recipe_command(MetadataCommand::Export(original.id.clone()))
        .unwrap()
    else {
        panic!("expected export")
    };
    assert_eq!(
        fs::read(fixture.0.join(PROFILES_FILE)).unwrap(),
        before_export
    );
    let imported_fixture = Fixture::new();
    let mut imported = imported_fixture.open();
    imported
        .apply_host_recipe_command(MetadataCommand::Import(text))
        .unwrap();
    assert_eq!(imported.host_recipes().get(&original.id), Some(&original));
    store
        .apply_host_recipe_command(MetadataCommand::Delete(original.id.clone()))
        .unwrap();
    assert!(fixture.open().host_recipes().get(&original.id).is_none());
    assert_eq!(reopened.host_recipes().get(&original.id), Some(&original));
}

#[test]
fn mobile_host_recipe_failed_write_keeps_previous_metadata_and_invalid_load_is_quarantined() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    fs::create_dir(fixture.0.join(PROFILES_FILE)).unwrap();
    assert!(store
        .apply_host_recipe_command(MetadataCommand::Save(recipe()))
        .is_err());
    assert!(store.host_recipes().is_empty());
    fs::remove_dir(fixture.0.join(PROFILES_FILE)).unwrap();
    let original = recipe();
    store
        .apply_host_recipe_command(MetadataCommand::Save(original))
        .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join(PROFILES_FILE)).unwrap()).unwrap();
    value["host_recipes"][0]["allow_remote_creation"] = serde_json::json!(true);
    fs::write(
        fixture.0.join(PROFILES_FILE),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let reopened = fixture.open();
    assert!(reopened.host_recipes().is_empty());
    assert_eq!(reopened.load_report().dropped_entries, 1);
    let quarantined = reopened.load_report().quarantined_file.as_ref().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(quarantined).unwrap()).unwrap(),
        value
    );
}

#[test]
fn mobile_host_recipe_recovery_retains_valid_entries_across_unrelated_save() {
    let fixture = Fixture::new();
    let candidates: Vec<_> = (0..=MAX_RECIPES).map(|_| recipe()).collect();
    let mut entries = vec![serde_json::to_value(&candidates[0]).unwrap()];
    let mut invalid = entries[0].clone();
    invalid["name"] = serde_json::json!("");
    entries.push(invalid);
    entries.push(entries[0].clone());
    entries.extend(
        candidates[1..]
            .iter()
            .map(|recipe| serde_json::to_value(recipe).unwrap()),
    );
    let document = serde_json::json!({
        "version": 1,
        "profiles": [],
        "accounts": [],
        "pending_secret_deletions": [],
        "host_recipes": entries,
    });
    fs::write(
        fixture.0.join(PROFILES_FILE),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    let mut recovered = fixture.open();
    assert_eq!(
        recovered.host_recipes().recipes(),
        &candidates[..MAX_RECIPES]
    );
    assert_eq!(recovered.load_report().dropped_entries, 3);
    assert!(!recovered.load_report().started_empty_after_corruption);
    let quarantine = recovered.load_report().quarantined_file.clone().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&quarantine).unwrap()).unwrap(),
        document
    );
    assert_eq!(
        recovered.apply_host_recipe_command(MetadataCommand::Save(recipe())),
        Err(ProfileError::HostRecipe(RecipeError::LimitReached))
    );
    recovered
        .create(
            "Unrelated SSH profile",
            ProfileKind::Ssh(SshProfile::new("host.example", "owner")),
        )
        .unwrap();
    let reopened = fixture.open();
    assert_eq!(
        reopened.host_recipes().recipes(),
        &candidates[..MAX_RECIPES]
    );
    assert_eq!(reopened.profiles().len(), 1);
    assert_eq!(reopened.load_report().dropped_entries, 0);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&quarantine).unwrap()).unwrap(),
        document
    );

    let malformed = Fixture::new();
    fs::write(
        malformed.0.join(PROFILES_FILE),
        r#"{"version":1,"host_recipes":{}}"#,
    )
    .unwrap();
    let rejected = malformed.open();
    assert!(rejected.host_recipes().is_empty());
    assert_eq!(rejected.load_report().dropped_entries, 1);
    assert!(rejected.load_report().quarantined_file.is_some());
}

#[test]
fn mobile_host_recipe_adapter_preserves_typed_command_failures() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let public = recipe();
    store
        .apply_host_recipe_command(MetadataCommand::Save(public.clone()))
        .unwrap();
    let portable = public.export().unwrap();
    let unsupported = portable.replace(
        "\"festerm_host_recipe_version\": 1",
        "\"festerm_host_recipe_version\": 99",
    );
    let before = fs::read(fixture.0.join(PROFILES_FILE)).unwrap();
    for (command, expected) in [
        (
            MetadataCommand::Save(public.clone()),
            RecipeError::Duplicate,
        ),
        (MetadataCommand::Import(portable), RecipeError::Duplicate),
        (
            MetadataCommand::Export(RecipeId::new()),
            RecipeError::NotFound,
        ),
        (
            MetadataCommand::Import(unsupported),
            RecipeError::UnsupportedVersion,
        ),
        (
            MetadataCommand::Import(" ".repeat(MAX_PORTABLE_BYTES + 1)),
            RecipeError::TooLarge,
        ),
    ] {
        let error = store.apply_host_recipe_command(command).unwrap_err();
        assert_eq!(error, ProfileError::HostRecipe(expected));
        assert_eq!(fs::read(fixture.0.join(PROFILES_FILE)).unwrap(), before);
        assert_eq!(store.host_recipes().get(&public.id), Some(&public));
    }
}
