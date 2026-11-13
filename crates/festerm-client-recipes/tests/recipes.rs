use festerm_client_recipes::{
    AccountReference, BrowseRequest, BrowseScope, ConnectionBehavior, DuplicatePolicy,
    GenerationId, HostBrowser, HostHint, HostId, HostReference, Lifetime, MetadataCommand,
    MetadataOutcome, PublicRecipe, Recipe, RecipeBook, RecipeError, RecipeId, MAX_PORTABLE_BYTES,
};

fn host() -> HostReference {
    HostReference {
        id: HostId::new(),
        account: AccountReference::Github { account_id: 42 },
    }
}

fn recipe() -> PublicRecipe {
    PublicRecipe::persistent(
        "Project shell",
        host(),
        Recipe {
            shell: Some("/bin/zsh".into()),
            args: vec!["-l".into(), "argument with spaces".into(), "".into()],
            cwd: Some("/remote/project".into()),
        },
    )
    .unwrap()
}

fn saved(recipe: PublicRecipe) -> RecipeBook {
    RecipeBook::empty()
        .apply(MetadataCommand::Save(recipe))
        .unwrap()
        .0
}

#[test]
fn public_recipe_serialization_preserves_launch_and_four_behaviors() {
    let mut recipe = recipe();
    assert_eq!(recipe.behavior, ConnectionBehavior::ResumeOrCreate);
    for behavior in [
        ConnectionBehavior::ChooseSession,
        ConnectionBehavior::ResumeOrCreate,
        ConnectionBehavior::AlwaysCreate,
        ConnectionBehavior::AttachOnly {
            generation: GenerationId::new(123).unwrap(),
        },
    ] {
        recipe.behavior = behavior;
        let text = recipe.export().unwrap();
        assert_eq!(PublicRecipe::import(&text).unwrap(), recipe);
    }
    recipe.lifetime = Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    };
    assert_eq!(
        PublicRecipe::import(&recipe.export().unwrap()).unwrap(),
        recipe
    );
    let mut json = serde_json::to_value(&recipe).unwrap();
    json.as_object_mut().unwrap().remove("behavior");
    let book: RecipeBook = serde_json::from_value(serde_json::json!([json])).unwrap();
    assert_eq!(
        book.recipes()[0].behavior,
        ConnectionBehavior::ResumeOrCreate
    );
}

#[test]
fn resume_only_round_trips_without_changing_exact_generation_or_legacy_defaults() {
    let mut recipe = recipe();
    recipe.behavior = ConnectionBehavior::ResumeOnly {};
    let text = recipe.export().unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value["recipe"]["behavior"],
        serde_json::json!({"kind": "resume_only"})
    );
    assert_eq!(PublicRecipe::import(&text).unwrap(), recipe);

    let mut invalid = value.clone();
    invalid["recipe"]["behavior"]["generation"] = serde_json::json!("123");
    assert!(PublicRecipe::import(&invalid.to_string()).is_err());

    let mut exact = value.clone();
    exact["recipe"]["behavior"] = serde_json::json!({"kind": "attach_only", "generation": "123"});
    let exact = PublicRecipe::import(&exact.to_string()).unwrap();
    assert_eq!(exact.logical_session, recipe.logical_session);
    assert_eq!(
        exact.behavior,
        ConnectionBehavior::AttachOnly {
            generation: GenerationId::new(123).unwrap(),
        }
    );
    invalid = value.clone();
    invalid["recipe"]["behavior"] = serde_json::json!({"kind": "attach_only"});
    assert!(PublicRecipe::import(&invalid.to_string()).is_err());

    let mut legacy = value;
    legacy["recipe"].as_object_mut().unwrap().remove("behavior");
    assert_eq!(
        PublicRecipe::import(&legacy.to_string()).unwrap().behavior,
        ConnectionBehavior::ResumeOrCreate
    );
}

#[test]
fn remote_session_name_and_saved_host_hint_round_trip_with_legacy_fallback() {
    let mut recipe = recipe();
    let legacy = serde_json::to_value(&recipe).unwrap();
    assert!(legacy.get("session_name").is_none());
    assert!(legacy.get("host_hint").is_none());
    assert_eq!(
        serde_json::from_value::<PublicRecipe>(legacy).unwrap(),
        recipe
    );
    // Loading an old profile must not apply the new protocol byte limit.
    recipe.name = "界".repeat(200);
    let loaded = PublicRecipe::import(&recipe.export().unwrap()).unwrap();
    assert_eq!(loaded.session_name(), recipe.name);
    let mut edited = loaded.clone();
    edited.behavior = ConnectionBehavior::ResumeOnly {};
    let (book, _) = saved(loaded)
        .apply(MetadataCommand::Edit(edited.clone()))
        .unwrap();
    assert_eq!(book.get(&edited.id), Some(&edited));
    assert!(edited.session_name.is_none());
    assert_eq!(edited.session_name(), recipe.name);
    recipe.session_name = Some("界".repeat(42) + "ab");
    recipe.host_hint = Some(HostHint {
        locator: "project-host.usw3".into(),
        name: "Build workstation".into(),
    });
    let loaded = PublicRecipe::import(&recipe.export().unwrap()).unwrap();
    assert_eq!(loaded, recipe);
    assert_eq!(
        loaded.session_name(),
        recipe.session_name.as_deref().unwrap()
    );
    assert_ne!(loaded.session_name(), loaded.name);
}

#[test]
fn new_public_metadata_rejects_invalid_names_locators_and_protected_hint_fields() {
    let mut recipe = recipe();
    for name in [
        "".to_owned(),
        " ".into(),
        " project".into(),
        "project ".into(),
        "project\nshell".into(),
        "project\u{7f}".into(),
        "界".repeat(43),
        "x".repeat(129),
        "--password=not-a-secret".into(),
    ] {
        recipe.session_name = Some(name.clone());
        assert_eq!(recipe.validate(), Err(RecipeError::Invalid));
        let value = serde_json::to_value(&recipe).unwrap();
        assert!(serde_json::from_value::<PublicRecipe>(value).is_err());
        recipe.session_name = None;
        recipe.host_hint = Some(HostHint {
            locator: "project.usw3".into(),
            name,
        });
        assert_eq!(recipe.validate(), Err(RecipeError::Invalid));
        recipe.host_hint = None;
    }
    for locator in [
        "".to_owned(),
        " host".into(),
        "Host".into(),
        "-host".into(),
        "host-".into(),
        ".usw3".into(),
        "host.".into(),
        "host.usw3.other".into(),
        "host\n.usw3".into(),
        "https://host.usw3".into(),
        "host.usw3?token=private".into(),
        "x".repeat(101),
        format!("host.{}", "x".repeat(101)),
    ] {
        recipe.host_hint = Some(HostHint {
            locator,
            name: "Workstation".into(),
        });
        assert_eq!(recipe.validate(), Err(RecipeError::Invalid));
        assert!(
            serde_json::from_value::<PublicRecipe>(serde_json::to_value(&recipe).unwrap()).is_err()
        );
    }
    recipe.host_hint = Some(HostHint {
        locator: format!("{}.{}", "x".repeat(100), "y".repeat(100)),
        name: "x".repeat(128),
    });
    assert_eq!(
        PublicRecipe::import(&recipe.export().unwrap()).unwrap(),
        recipe
    );
    for field in [
        "credential_id",
        "token",
        "trusted_host",
        "online",
        "allow_remote_creation",
    ] {
        let mut value = serde_json::to_value(&recipe).unwrap();
        value["host_hint"][field] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<PublicRecipe>(value).is_err(),
            "{field}"
        );
    }
}

#[test]
fn invalid_recipe_ids_launch_and_protected_fields_are_refused() {
    let recipe = recipe();
    let base: serde_json::Value = serde_json::from_str(&recipe.export().unwrap()).unwrap();
    for (field, value) in [
        ("id", serde_json::json!("not-a-uuid")),
        (
            "logical_session",
            serde_json::json!("550E8400-E29B-41D4-A716-446655440000"),
        ),
        ("name", serde_json::json!(" bad ")),
        (
            "credential_id",
            serde_json::json!("550e8400-e29b-41d4-a716-446655440000"),
        ),
        ("principal", serde_json::json!("github:42")),
        ("allow_remote_creation", serde_json::json!(true)),
        ("grants", serde_json::json!([])),
        ("trusted_host", serde_json::json!(true)),
    ] {
        let mut bad = base.clone();
        bad["recipe"][field] = value;
        assert!(PublicRecipe::import(&bad.to_string()).is_err(), "{field}");
    }
    for (path, value) in [
        (vec!["host", "id"], serde_json::json!("host\n")),
        (vec!["host", "account", "account_id"], serde_json::json!(0)),
        (vec!["launch", "shell"], serde_json::json!("")),
        (vec!["launch", "cwd"], serde_json::json!("/work\nelsewhere")),
        (vec!["launch", "args"], serde_json::json!(vec!["x"; 33])),
        (
            vec!["launch", "args"],
            serde_json::json!(["--password=not-a-secret"]),
        ),
        (
            vec!["lifetime"],
            serde_json::json!({"kind": "temporary", "reconnect_grace_seconds": 0}),
        ),
        (
            vec!["behavior"],
            serde_json::json!({"kind": "attach_only", "generation": "0"}),
        ),
    ] {
        let mut bad = base.clone();
        let mut target = &mut bad["recipe"];
        for key in path {
            target = &mut target[key];
        }
        *target = value;
        assert!(PublicRecipe::import(&bad.to_string()).is_err(), "{bad}");
    }
    let mut bad = base.clone();
    bad["credential"] = serde_json::json!("private");
    assert!(PublicRecipe::import(&bad.to_string()).is_err());
    bad = base;
    bad["festerm_host_recipe_version"] = serde_json::json!(99);
    assert_eq!(
        PublicRecipe::import(&bad.to_string()),
        Err(RecipeError::UnsupportedVersion)
    );
    assert_eq!(
        PublicRecipe::import(&" ".repeat(MAX_PORTABLE_BYTES + 1)),
        Err(RecipeError::TooLarge)
    );
    assert!(RecipeId::parse("00000000-0000-0000-0000-000000000000").is_err());
    assert!(RecipeId::parse("550e8400-e29b-41d4-0716-446655440000").is_err());
    let paste = r#"{"credential":"DO_NOT_ECHO_PASTED_MATERIAL"}"#;
    let command = MetadataCommand::Import(paste.into());
    assert!(!format!("{command:?}").contains("DO_NOT_ECHO"));
    let error = RecipeBook::empty().apply(command).unwrap_err();
    assert!(!format!("{error} {error:?}").contains("DO_NOT_ECHO"));
    assert!(PublicRecipe::import(r#"{"festerm_profile_version":1}"#).is_err());
}

#[test]
fn metadata_commands_edit_future_recipe_without_mutating_previous_snapshot() {
    let original = recipe();
    let book = saved(original.clone());
    let mut edited = original.clone();
    edited.launch.cwd = Some("/remote/next-create".into());
    edited.session_name = Some("Remote project".into());
    edited.host_hint = Some(HostHint {
        locator: "project.usw3".into(),
        name: "Workstation".into(),
    });
    let (next, outcome) = book.apply(MetadataCommand::Edit(edited.clone())).unwrap();
    assert_eq!(outcome, MetadataOutcome::Saved(original.id.clone()));
    assert_eq!(book.get(&original.id), Some(&original));
    assert_eq!(next.get(&original.id), Some(&edited));
    let (unchanged, outcome) = next
        .apply(MetadataCommand::Export(original.id.clone()))
        .unwrap();
    assert_eq!(unchanged, next);
    let MetadataOutcome::Exported(text) = outcome else {
        panic!("expected export")
    };
    let (imported, _) = RecipeBook::empty()
        .apply(MetadataCommand::Import(text.clone()))
        .unwrap();
    assert_eq!(imported.get(&original.id), Some(&edited));
    assert_eq!(
        next.apply(MetadataCommand::Import(text)),
        Err(RecipeError::Duplicate)
    );
    let (deleted, outcome) = next
        .apply(MetadataCommand::Delete(original.id.clone()))
        .unwrap();
    assert!(deleted.is_empty());
    assert_eq!(outcome, MetadataOutcome::Deleted(original.id.clone()));
    assert_eq!(next.get(&original.id), Some(&edited));
    edited.launch.cwd = Some("\ninvalid".into());
    assert_eq!(
        next.apply(MetadataCommand::Edit(edited)),
        Err(RecipeError::Invalid)
    );
    assert!(next
        .apply(MetadataCommand::Delete(RecipeId::new()))
        .is_err());
}

#[test]
fn duplicate_allocates_logical_identity_unless_explicit_generation_shortcut() {
    let mut original = recipe();
    original.session_name = Some("Remote project".into());
    original.host_hint = Some(HostHint {
        locator: "project.usw3".into(),
        name: "Workstation".into(),
    });
    original.behavior = ConnectionBehavior::AttachOnly {
        generation: GenerationId::new(123).unwrap(),
    };
    let book = saved(original.clone());
    let (fresh, MetadataOutcome::Saved(fresh_id)) = book
        .apply(MetadataCommand::Duplicate {
            source: original.id.clone(),
            name: "Separate shell".into(),
            policy: DuplicatePolicy::FreshLogicalSession,
        })
        .unwrap()
    else {
        panic!("expected saved duplicate")
    };
    let duplicate = fresh.get(&fresh_id).unwrap();
    assert_ne!(duplicate.id, original.id);
    assert_ne!(duplicate.logical_session, original.logical_session);
    assert_eq!(duplicate.host, original.host);
    assert_eq!(duplicate.launch, original.launch);
    assert_eq!(duplicate.session_name, original.session_name);
    assert_eq!(duplicate.host_hint, original.host_hint);
    assert_eq!(duplicate.behavior, ConnectionBehavior::ResumeOrCreate);
    let generation = GenerationId::new(456).unwrap();
    let (shortcuts, MetadataOutcome::Saved(shortcut_id)) = book
        .apply(MetadataCommand::Duplicate {
            source: original.id.clone(),
            name: "Existing shell shortcut".into(),
            policy: DuplicatePolicy::ExistingSession { generation },
        })
        .unwrap()
    else {
        panic!("expected saved shortcut")
    };
    let shortcut = shortcuts.get(&shortcut_id).unwrap();
    assert_ne!(shortcut.id, original.id);
    assert_eq!(shortcut.logical_session, original.logical_session);
    assert_eq!(shortcut.session_name, original.session_name);
    assert_eq!(shortcut.host_hint, original.host_hint);
    assert_eq!(
        shortcut.behavior,
        ConnectionBehavior::AttachOnly { generation }
    );
}

#[test]
fn book_deserialization_rejects_duplicate_and_invalid_candidates() {
    let recipe = recipe();
    let duplicate = serde_json::json!([recipe, recipe]);
    assert!(serde_json::from_value::<RecipeBook>(duplicate).is_err());
    let mut invalid = serde_json::to_value(saved(recipe)).unwrap();
    invalid[0]["name"] = serde_json::json!("");
    assert!(serde_json::from_value::<RecipeBook>(invalid).is_err());
}

#[test]
fn local_browser_requires_explicit_scoped_requests_and_fences_stale_results() {
    let mut browser = HostBrowser::default();
    assert_eq!(browser.pending(), None);
    assert_eq!(browser.selected_host(), None);
    let selected = host();
    browser
        .select_account(Some(selected.account.clone()))
        .unwrap();
    // Account/card selection and repeated reads have no request or availability.
    for _ in 0..3 {
        assert_eq!(browser.pending(), None);
        assert_eq!(browser.selected_host(), None);
    }
    let BrowseRequest::FindHosts(first) = browser.find_hosts().unwrap() else {
        panic!("expected service inventory intent");
    };
    assert_eq!(
        first.scope(),
        &BrowseScope::Account(selected.account.clone())
    );
    let mut another_browser = HostBrowser::default();
    another_browser
        .select_account(Some(selected.account.clone()))
        .unwrap();
    another_browser.find_hosts().unwrap();
    assert!(!another_browser.complete(&first));
    let BrowseRequest::InspectHost(second) = browser.inspect_host(selected.clone()).unwrap() else {
        panic!("expected single-host inspection intent");
    };
    assert!(!browser.complete(&first));
    assert_eq!(second.scope(), &BrowseScope::Host(selected.clone()));
    browser.cancel();
    assert!(!browser.complete(&second));
    let BrowseRequest::InspectHost(third) = browser.refresh().unwrap() else {
        panic!("refresh must inspect only the selected host");
    };
    browser
        .select_account(Some(AccountReference::Github { account_id: 99 }))
        .unwrap();
    assert!(!browser.complete(&third));
    assert!(browser.inspect_host(selected).is_err());
    let BrowseRequest::FindHosts(current) = browser.find_hosts().unwrap() else {
        panic!("account switch clears host selection");
    };
    assert!(browser.complete(&current));
    assert!(!browser.complete(&current));
}
