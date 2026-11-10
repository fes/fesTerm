use festerm_config::{
    host_recipes::{
        AccountReference, HostId, HostReference, MetadataCommand, MetadataOutcome, PublicRecipe,
        Recipe,
    },
    Configuration, Profile,
};

fn recipe() -> PublicRecipe {
    PublicRecipe::persistent(
        "Project",
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
fn host_recipe_toml_round_trip_keeps_legacy_profiles_separate() {
    let legacy = Configuration::parse(
        r#"schema_version = 1
[[profiles]]
kind = "dev_tunnel"
id = "existing-shell"
destination_file = "public-destination.json"
"#,
    )
    .unwrap();
    assert!(legacy.host_recipes().is_empty());
    let original = legacy.profile("existing-shell").unwrap().clone();
    assert!(original.as_dev_tunnel().unwrap().account().is_none());
    assert!(original.credential_reference().is_none());
    let recipe = recipe();
    let (candidate, _) = legacy
        .with_host_recipe_command(MetadataCommand::Save(recipe.clone()))
        .unwrap();
    let serialized = candidate.to_toml().unwrap();
    assert!(serialized.contains("[[host_recipes]]"));
    let loaded = Configuration::parse(&serialized).unwrap();
    assert_eq!(loaded, candidate);
    // Both serde paths validate and preserve the exact legacy metadata.
    assert_eq!(
        toml::from_str::<Configuration>(&serialized).unwrap(),
        loaded
    );
    assert_eq!(loaded.profile("existing-shell"), Some(&original));
    assert_eq!(loaded.host_recipes().get(&recipe.id), Some(&recipe));
    assert!(loaded.profiles().iter().all(|profile| profile
        .as_local()
        .and_then(|local| local.tunnel_hosting())
        .is_none()));
    assert!(!serialized.contains("allow_remote_creation"));
    assert!(!serialized.contains("principal"));
    assert!(legacy.host_recipes().is_empty());
}

#[test]
fn host_recipe_invalid_commands_and_documents_do_not_replace_configuration() {
    let local = Profile::local("local", "sh", vec![], None).unwrap();
    let original = Configuration::new(vec![local]).unwrap();
    let mut recipe = recipe();
    recipe.launch.args = vec!["--password".into(), "not-a-secret".into()];
    assert!(original
        .with_host_recipe_command(MetadataCommand::Save(recipe.clone()))
        .is_err());
    assert!(original.host_recipes().is_empty());
    recipe.launch.args.clear();
    let (saved, _) = original
        .with_host_recipe_command(MetadataCommand::Save(recipe.clone()))
        .unwrap();
    let serialized = saved.to_toml().unwrap();
    for (from, to) in [
        ("name = \"Project\"", "name = \"\""),
        ("provider = \"github\"", "provider = \"client_asserted\""),
        ("account_id = 42", "account_id = 0"),
        ("kind = \"resume_or_create\"", "kind = \"not_a_behavior\""),
    ] {
        assert!(serialized.contains(from), "{from}");
        let invalid = serialized.replace(from, to);
        assert!(Configuration::parse(&invalid).is_err());
        assert!(toml::from_str::<Configuration>(&invalid).is_err());
    }
    let (unchanged, MetadataOutcome::Exported(portable)) = saved
        .with_host_recipe_command(MetadataCommand::Export(recipe.id.clone()))
        .unwrap()
    else {
        panic!("expected metadata export")
    };
    assert_eq!(unchanged, saved);
    assert!(!portable.contains("credential"));
    let (deleted, _) = saved
        .with_host_recipe_command(MetadataCommand::Delete(recipe.id))
        .unwrap();
    assert_eq!(deleted.profiles(), original.profiles());
    assert_eq!(deleted.accounts(), original.accounts());
    assert!(deleted.host_recipes().is_empty());
}
