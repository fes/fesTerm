use std::{fs, path::PathBuf, sync::Arc};

use festerm_mobile_profiles::{
    host_recipes::{
        AccountReference, DuplicatePolicy, HostId, HostReference, MetadataCommand, MetadataOutcome,
        PublicRecipe, Recipe,
    },
    ProfileStore, PROFILES_FILE,
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
fn mobile_host_recipe_commands_persist_without_vault_or_legacy_mutation() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let original = recipe();
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
