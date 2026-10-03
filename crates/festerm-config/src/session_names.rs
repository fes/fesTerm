use serde::{Deserialize, Serialize};

use crate::{contains_secret_bearing_value, ConfigError, ConfigErrorKind};

/// An explicit, local display override, never a profile or provider name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SessionAlias(String);

impl SessionAlias {
    pub const MAX_CHARACTERS: usize = 200;

    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > Self::MAX_CHARACTERS * 4
            || value.trim() != value
            || value.chars().count() > Self::MAX_CHARACTERS
            || value.chars().any(unsafe_display_character)
        {
            return Err(ConfigError::new(ConfigErrorKind::InvalidSessionAlias));
        }
        if contains_secret_bearing_value(&value) {
            return Err(ConfigError::new(ConfigErrorKind::ForbiddenSecretValue));
        }
        Ok(Self(value))
    }

    /// User edits are sanitized once, before both display and persistence.
    /// Empty edits cancel; they are not the reset-to-default operation.
    pub fn from_user_input(value: &str) -> Result<Option<Self>, ConfigError> {
        let sanitized: String = value
            .trim()
            .chars()
            .take(Self::MAX_CHARACTERS * 4)
            .filter(|character| !unsafe_display_character(*character))
            .take(Self::MAX_CHARACTERS)
            .collect();
        let sanitized = sanitized.trim();
        if sanitized.is_empty() {
            return Ok(None);
        }
        Self::new(sanitized).map(Some)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SessionAlias {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn unsafe_display_character(character: char) -> bool {
    character.is_control()
        || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{206f}')
}

/// Existing native daemon registry facts, not a runtime TabId or a reusable
/// attachment name. Endpoint includes the provider's local IPC namespace.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum DurableSessionIdentity {
    FestermSessiond {
        name: String,
        pid: u32,
        /// Decimal preserves the daemon's u128 generation without TOML's
        /// signed 64-bit integer limit.
        created_at_unix_ms: String,
        endpoint: String,
    },
}

impl DurableSessionIdentity {
    pub fn native(
        name: impl Into<String>,
        pid: u32,
        created_at_unix_ms: u128,
        endpoint: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let identity = Self::FestermSessiond {
            name: name.into(),
            pid,
            created_at_unix_ms: created_at_unix_ms.to_string(),
            endpoint: endpoint.into(),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        let Self::FestermSessiond {
            name,
            pid,
            created_at_unix_ms,
            endpoint,
        } = self;
        if created_at_unix_ms.is_empty() || created_at_unix_ms.len() > 39 {
            return Err(ConfigError::new(
                ConfigErrorKind::InvalidDurableSessionIdentity,
            ));
        }
        let generation = created_at_unix_ms.parse::<u128>().ok();
        let generation_endpoint = format!("{pid}-{created_at_unix_ms}");
        if festerm_ssh::PersistentSessionName::new(name).is_err()
            || *pid == 0
            || generation.is_none_or(|generation| {
                generation == 0 || generation.to_string() != *created_at_unix_ms
            })
            || endpoint.is_empty()
            || endpoint.len() > 4096
            || endpoint.chars().any(unsafe_display_character)
            || contains_secret_bearing_value(name)
            || contains_secret_bearing_value(endpoint)
            || !(endpoint.ends_with(&generation_endpoint)
                || endpoint.ends_with(&format!("{generation_endpoint}.sock")))
        {
            return Err(ConfigError::new(
                ConfigErrorKind::InvalidDurableSessionIdentity,
            ));
        }
        Ok(())
    }
}

/// A seed for newly opened native views, independent of workspace restoration.
/// Existing views retain their own explicit aliases.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableSessionAlias {
    pub(crate) identity: DurableSessionIdentity,
    pub(crate) alias: SessionAlias,
}

pub const MAX_DURABLE_SESSION_ALIASES: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Configuration, PersistenceProviderKind, Profile, WorkspaceConfiguration, WorkspaceTab,
    };

    fn identity(generation: u128) -> DurableSessionIdentity {
        DurableSessionIdentity::native(
            "same-name",
            42,
            generation,
            format!("owned-endpoint-42-{generation}"),
        )
        .unwrap()
    }

    #[test]
    fn session_alias_bounds_sanitize_user_edits_and_reject_invalid_metadata() {
        const REJECTED_ALIAS: &str = "-----BEGIN PRIVATE KEY-----";
        assert_eq!(
            SessionAlias::from_user_input(" \nBuild\u{202e}\t bench\r ")
                .unwrap()
                .unwrap()
                .as_str(),
            "Build bench"
        );
        for cancelled in ["", " \r\n ", "\u{202e}\u{2028}\0"] {
            assert!(SessionAlias::from_user_input(cancelled).unwrap().is_none());
        }
        assert_eq!(
            SessionAlias::from_user_input(REJECTED_ALIAS)
                .unwrap_err()
                .kind(),
            ConfigErrorKind::ForbiddenSecretValue
        );
        for invalid in [
            "",
            " leading",
            "trailing ",
            "two\nlines",
            "two\u{2028}lines",
            "two\u{2029}paragraphs",
            "bidi\u{2066}",
        ] {
            assert!(SessionAlias::new(invalid).is_err());
        }
        assert!(SessionAlias::new("é".repeat(201)).is_err());
        assert_eq!(
            SessionAlias::new("password=owned-fixture-value")
                .unwrap_err()
                .kind(),
            ConfigErrorKind::ForbiddenSecretValue
        );
        let bounded = SessionAlias::from_user_input(&"é".repeat(1000))
            .unwrap()
            .unwrap();
        assert_eq!(bounded.as_str().chars().count(), 200);
        assert!(Configuration::parse(
            "schema_version = 1\n[[durable_session_aliases]]\nalias = \"two\\nlines\""
        )
        .is_err());
    }

    #[test]
    fn durable_aliases_round_trip_without_workspace_and_refuse_recycled_generations() {
        let original = Configuration::empty();
        let alias = SessionAlias::new("Build bench").unwrap();
        let saved = original
            .with_durable_session_alias(identity(100), Some(alias.clone()))
            .unwrap();
        let restarted = Configuration::parse(&saved.to_toml().unwrap()).unwrap();
        assert_eq!(
            restarted.durable_session_alias(&identity(100)),
            Some(&alias)
        );
        assert!(restarted.durable_session_alias(&identity(101)).is_none());
        let other_namespace =
            DurableSessionIdentity::native("same-name", 42, 100, "other-context-42-100").unwrap();
        assert!(restarted.durable_session_alias(&other_namespace).is_none());
        assert!(restarted.workspace().is_none());
        assert!(original.durable_session_alias(&identity(100)).is_none());
        let reset = restarted
            .with_durable_session_alias(identity(100), None)
            .unwrap();
        assert!(!reset.to_toml().unwrap().contains("durable_session_aliases"));
        assert_eq!(reset, original);
    }

    #[test]
    fn workspace_replacements_preserve_durable_aliases_and_update_bookkeeping() {
        let identity = identity(100);
        let configuration = Configuration::empty()
            .with_durable_session_alias(identity.clone(), Some(SessionAlias::new("One").unwrap()))
            .unwrap()
            .with_update_check(100, None)
            .unwrap();
        let workspace =
            WorkspaceConfiguration::new(vec![WorkspaceTab::launcher("launcher").unwrap()], None)
                .unwrap();
        let replacement = configuration.with_workspace(workspace).unwrap();
        assert_eq!(
            replacement.durable_session_alias(&identity),
            configuration.durable_session_alias(&identity)
        );
        assert_eq!(replacement.update_check(), configuration.update_check());
    }

    #[test]
    fn durable_alias_registry_is_bounded_without_pruning_absent_sessions() {
        let mut configuration = Configuration::empty();
        for generation in 1..=MAX_DURABLE_SESSION_ALIASES {
            configuration = configuration
                .with_durable_session_alias(
                    identity(generation as u128),
                    Some(SessionAlias::new("Retained").unwrap()),
                )
                .unwrap();
        }
        assert_eq!(
            configuration
                .with_durable_session_alias(
                    identity(2000),
                    Some(SessionAlias::new("Extra").unwrap()),
                )
                .unwrap_err()
                .kind(),
            ConfigErrorKind::TooManyDurableSessionAliases
        );
        configuration = configuration
            .with_durable_session_alias(identity(1), None)
            .unwrap();
        assert!(configuration.durable_session_alias(&identity(2)).is_some());
        assert!(configuration
            .with_durable_session_alias(identity(2000), Some(SessionAlias::new("Extra").unwrap()),)
            .is_ok());
    }

    #[test]
    fn legacy_configuration_has_no_session_aliases_or_added_defaults() {
        let legacy = Configuration::parse("schema_version = 1\nprofiles = []").unwrap();
        assert_eq!(legacy, Configuration::empty());
        assert!(!legacy.to_toml().unwrap().contains("alias"));
    }

    #[test]
    fn workspace_aliases_round_trip_for_all_mux_kinds_without_provider_identity() {
        for provider in [
            PersistenceProviderKind::Tmux,
            PersistenceProviderKind::Screen,
        ] {
            let configuration = Configuration::new(vec![
                Profile::local("local", "owned-unused-child", Vec::new(), None)
                    .unwrap()
                    .with_persistence(provider, "build")
                    .unwrap(),
                Profile::ssh(
                    "remote",
                    "ssh.example.test",
                    22,
                    "deploy",
                    "xterm-256color",
                    80,
                    24,
                )
                .unwrap()
                .with_persistence(provider, "build")
                .unwrap(),
            ])
            .unwrap();
            let workspace = WorkspaceConfiguration::new(
                vec![
                    WorkspaceTab::local_session("local-one", "local")
                        .unwrap()
                        .with_session_alias(Some(SessionAlias::new("Local one").unwrap()))
                        .unwrap(),
                    WorkspaceTab::local_session("local-two", "local")
                        .unwrap()
                        .with_session_alias(Some(SessionAlias::new("Local two").unwrap()))
                        .unwrap(),
                    WorkspaceTab::ssh_session("ssh-one", "remote")
                        .unwrap()
                        .with_session_alias(Some(SessionAlias::new("Remote one").unwrap()))
                        .unwrap(),
                    WorkspaceTab::ssh_session("ssh-two", "remote").unwrap(),
                ],
                None,
            )
            .unwrap();
            let saved = configuration.with_workspace(workspace).unwrap();
            let text = saved.to_toml().unwrap();
            assert!(!text.contains("alias_identity"));
            assert!(!text.contains("durable_session_aliases"));
            let restarted = Configuration::parse(&text).unwrap();
            assert_eq!(restarted.profiles(), configuration.profiles());
            let tabs = restarted.workspace().unwrap().tabs();
            let WorkspaceTab::LocalSession(first) = &tabs[0] else {
                panic!("local");
            };
            let WorkspaceTab::LocalSession(second) = &tabs[1] else {
                panic!("local");
            };
            let WorkspaceTab::SshSession(remote) = &tabs[2] else {
                panic!("SSH");
            };
            let WorkspaceTab::SshSession(default) = &tabs[3] else {
                panic!("SSH");
            };
            assert_eq!(first.alias().unwrap().as_str(), "Local one");
            assert_eq!(second.alias().unwrap().as_str(), "Local two");
            assert_eq!(remote.alias().unwrap().as_str(), "Remote one");
            assert!(default.alias().is_none());
            let reset = tabs[0].clone().with_session_alias(None).unwrap();
            let workspace = WorkspaceConfiguration::new(
                vec![reset, tabs[1].clone(), tabs[2].clone(), tabs[3].clone()],
                None,
            )
            .unwrap();
            let reset = restarted.with_workspace(workspace).unwrap();
            let WorkspaceTab::LocalSession(first) = &reset.workspace().unwrap().tabs()[0] else {
                panic!("local");
            };
            assert!(first.alias().is_none());
            assert_eq!(reset.profiles(), configuration.profiles());
            assert!(reset.to_toml().unwrap().contains("Local two"));
        }
    }

    #[test]
    fn session_alias_metadata_rejects_legacy_endpoints_duplicates_and_unknown_fields() {
        assert!(DurableSessionIdentity::native("same-name", 42, 100, "reusable-name").is_err());
        assert!(DurableSessionIdentity::native("same-name", 0, 100, "owned-0-100").is_err());
        assert!(DurableSessionIdentity::native("same-name", 42, 0, "owned-42-0").is_err());
        let invalid_generation = DurableSessionIdentity::FestermSessiond {
            name: "same-name".into(),
            pid: 42,
            created_at_unix_ms: "1".repeat(40),
            endpoint: "owned-42-100".into(),
        };
        assert!(invalid_generation.validate().is_err());
        assert!(DurableSessionIdentity::native(
            "same-name",
            42,
            100,
            format!("{}42-100", "x".repeat(4096))
        )
        .is_err());
        let saved = Configuration::empty()
            .with_durable_session_alias(identity(100), Some(SessionAlias::new("Name").unwrap()))
            .unwrap();
        let text = saved.to_toml().unwrap();
        let entry = &text[text.find("[[durable_session_aliases]]").unwrap()..];
        assert_eq!(
            Configuration::parse(&format!("{text}\n{entry}"))
                .unwrap_err()
                .kind(),
            ConfigErrorKind::DuplicateDurableSessionAlias
        );
        assert!(Configuration::parse(
            &text.replace("alias = \"Name\"", "alias = \"Name\"\nunknown = true")
        )
        .is_err());
        assert!(
            Configuration::parse(&text.replace("alias = \"Name\"", "alias = \"two\\nlines\""))
                .is_err()
        );
        assert!(Configuration::parse(&text.replace(
            "alias = \"Name\"",
            &format!("alias = \"{}\"", "é".repeat(201))
        ))
        .is_err());
        assert!(WorkspaceTab::launcher("launcher")
            .unwrap()
            .with_session_alias(Some(SessionAlias::new("Wrong").unwrap()))
            .is_err());
    }
}
