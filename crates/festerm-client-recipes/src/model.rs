use std::{collections::HashSet, fmt};

use festerm_host_protocol::{GenerationId, HostId, Lifetime, LogicalSessionId, Recipe};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_RECIPES: usize = 200;
pub const MAX_PORTABLE_BYTES: usize = 64 * 1024;
const PORTABLE_VERSION: u16 = 1;

/// Content-free errors; pasted documents and launch arguments are never echoed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecipeError {
    Invalid,
    UnsupportedVersion,
    TooLarge,
    Duplicate,
    NotFound,
    LimitReached,
}

impl RecipeError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Invalid => "invalid public host recipe metadata",
            Self::UnsupportedVersion => "unsupported public host recipe version",
            Self::TooLarge => "public host recipe exceeds its size limit",
            Self::Duplicate => "host recipe identifier already exists",
            Self::NotFound => "host recipe not found",
            Self::LimitReached => "host recipe limit reached",
        }
    }
}

impl fmt::Display for RecipeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for RecipeError {}

/// Client-local identity, separate from the remote logical-session identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RecipeId(String);

impl RecipeId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn parse(value: &str) -> Result<Self, RecipeError> {
        let id = Uuid::parse_str(value).map_err(|_| RecipeError::Invalid)?;
        if id.get_version_num() != 4
            || id.get_variant() != uuid::Variant::RFC4122
            || id.to_string() != value
        {
            return Err(RecipeError::Invalid);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RecipeId {
    fn default() -> Self {
        Self::new()
    }
}

impl<'de> Deserialize<'de> for RecipeId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A public service-account hint, never a principal, grant or sign-in proof.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountReference {
    Github { account_id: u64 },
}

impl AccountReference {
    pub fn validate(&self) -> Result<(), RecipeError> {
        match self {
            Self::Github { account_id: 0 } => Err(RecipeError::Invalid),
            Self::Github { .. } => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostReference {
    pub id: HostId,
    pub account: AccountReference,
}

impl HostReference {
    pub fn validate(&self) -> Result<(), RecipeError> {
        self.account.validate()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConnectionBehavior {
    ChooseSession,
    #[default]
    ResumeOrCreate,
    AlwaysCreate,
    AttachOnly {
        generation: GenerationId,
    },
}

/// Only public data may cross the portable boundary. Credentials, trust
/// decisions, host consent and grants have no fields here.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedRecipe")]
pub struct PublicRecipe {
    pub id: RecipeId,
    pub name: String,
    pub host: HostReference,
    pub logical_session: LogicalSessionId,
    pub launch: Recipe,
    pub lifetime: Lifetime,
    #[serde(default)]
    pub behavior: ConnectionBehavior,
}

impl fmt::Debug for PublicRecipe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicRecipe(<public metadata>)")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedRecipe {
    id: RecipeId,
    name: String,
    host: HostReference,
    logical_session: LogicalSessionId,
    launch: Recipe,
    lifetime: Lifetime,
    #[serde(default)]
    behavior: ConnectionBehavior,
}

impl TryFrom<UncheckedRecipe> for PublicRecipe {
    type Error = RecipeError;

    fn try_from(raw: UncheckedRecipe) -> Result<Self, Self::Error> {
        let recipe = Self {
            id: raw.id,
            name: raw.name,
            host: raw.host,
            logical_session: raw.logical_session,
            launch: raw.launch,
            lifetime: raw.lifetime,
            behavior: raw.behavior,
        };
        recipe.validate()?;
        Ok(recipe)
    }
}

impl PublicRecipe {
    /// A new persistent recipe. This saves intent; it does not reserve a
    /// remote identity or grant permission to create anything.
    pub fn persistent(
        name: impl Into<String>,
        host: HostReference,
        launch: Recipe,
    ) -> Result<Self, RecipeError> {
        let recipe = Self {
            id: RecipeId::new(),
            name: name.into(),
            host,
            logical_session: LogicalSessionId::new(),
            launch,
            lifetime: Lifetime::Persistent,
            behavior: ConnectionBehavior::ResumeOrCreate,
        };
        recipe.validate()?;
        Ok(recipe)
    }

    pub fn validate(&self) -> Result<(), RecipeError> {
        if self.name.is_empty()
            || self.name.chars().count() > 200
            || self.name.trim() != self.name
            || self.name.chars().any(char::is_control)
        {
            return Err(RecipeError::Invalid);
        }
        self.host.validate()?;
        self.launch.validate().map_err(|_| RecipeError::Invalid)?;
        self.lifetime.validate().map_err(|_| RecipeError::Invalid)?;
        // Arguments and cwd are public metadata, not a way to export secrets.
        // Match the existing configuration boundary's obvious-secret refusal.
        if self
            .launch
            .shell
            .iter()
            .chain(self.launch.cwd.iter())
            .chain(self.launch.args.iter())
            .any(|value| secret_bearing(value))
        {
            return Err(RecipeError::Invalid);
        }
        Ok(())
    }

    pub fn export(&self) -> Result<String, RecipeError> {
        self.validate()?;
        let text = serde_json::to_string_pretty(&PortableRecipe {
            festerm_host_recipe_version: PORTABLE_VERSION,
            recipe: self.clone(),
        })
        .map_err(|_| RecipeError::Invalid)?;
        if text.len() > MAX_PORTABLE_BYTES {
            return Err(RecipeError::TooLarge);
        }
        Ok(text)
    }

    pub fn import(text: &str) -> Result<Self, RecipeError> {
        if text.len() > MAX_PORTABLE_BYTES {
            return Err(RecipeError::TooLarge);
        }
        let document: PortableRecipe =
            serde_json::from_str(text).map_err(|_| RecipeError::Invalid)?;
        if document.festerm_host_recipe_version != PORTABLE_VERSION {
            return Err(RecipeError::UnsupportedVersion);
        }
        document.recipe.validate()?;
        Ok(document.recipe)
    }
}

fn secret_bearing(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    let option = value
        .split_once(['=', ':'])
        .map_or(value.as_str(), |(name, _)| name)
        .trim_start_matches('-');
    let normalized: String = option.chars().filter(char::is_ascii_alphanumeric).collect();
    value.contains("-----begin ")
        || [
            "password",
            "passphrase",
            "secret",
            "token",
            "privatekey",
            "credential",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
        || [
            "password=",
            "password:",
            "passwd=",
            "token=",
            "access_token",
            "refresh_token",
            "client_secret",
            "authorization:",
        ]
        .iter()
        .any(|marker| value.contains(marker))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableRecipe {
    festerm_host_recipe_version: u16,
    recipe: PublicRecipe,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DuplicatePolicy {
    FreshLogicalSession,
    /// Deliberate shortcut to one known generation, never a second creator.
    ExistingSession {
        generation: GenerationId,
    },
}

/// Product intent shared by desktop, iOS and Android. There is deliberately no
/// SaveAndConnect, sign-in, discovery, host-install or session-end variant.
#[derive(Clone, Eq, PartialEq)]
pub enum MetadataCommand {
    Save(PublicRecipe),
    Edit(PublicRecipe),
    Duplicate {
        source: RecipeId,
        name: String,
        policy: DuplicatePolicy,
    },
    Import(String),
    Export(RecipeId),
    Delete(RecipeId),
}

impl fmt::Debug for MetadataCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Save(_) => "MetadataCommand::Save(<public metadata>)",
            Self::Edit(_) => "MetadataCommand::Edit(<public metadata>)",
            Self::Duplicate { .. } => "MetadataCommand::Duplicate(<public metadata>)",
            Self::Import(_) => "MetadataCommand::Import(<untrusted document>)",
            Self::Export(_) => "MetadataCommand::Export(<recipe id>)",
            Self::Delete(_) => "MetadataCommand::Delete(<recipe id>)",
        })
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum MetadataOutcome {
    Saved(RecipeId),
    Deleted(RecipeId),
    Exported(String),
}

impl fmt::Debug for MetadataOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Saved(_) => "MetadataOutcome::Saved(<recipe id>)",
            Self::Deleted(_) => "MetadataOutcome::Deleted(<recipe id>)",
            Self::Exported(_) => "MetadataOutcome::Exported(<public document>)",
        })
    }
}

/// Independently persisted collection, not a reinterpretation of legacy
/// connection profiles. Deserialize validates the complete candidate.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RecipeBook(Vec<PublicRecipe>);

impl<'de> Deserialize<'de> for RecipeBook {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let book = Self(Vec::<PublicRecipe>::deserialize(deserializer)?);
        book.validate().map_err(serde::de::Error::custom)?;
        Ok(book)
    }
}

impl RecipeBook {
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn recipes(&self) -> &[PublicRecipe] {
        &self.0
    }

    pub fn get(&self, id: &RecipeId) -> Option<&PublicRecipe> {
        self.0.iter().find(|recipe| &recipe.id == id)
    }

    pub fn validate(&self) -> Result<(), RecipeError> {
        if self.0.len() > MAX_RECIPES {
            return Err(RecipeError::LimitReached);
        }
        let mut ids = HashSet::new();
        for recipe in &self.0 {
            recipe.validate()?;
            if !ids.insert(&recipe.id) {
                return Err(RecipeError::Duplicate);
            }
        }
        Ok(())
    }

    /// Returns a replacement for the composition root to persist. The
    /// original remains unchanged on validation or persistence failure.
    pub fn apply(&self, command: MetadataCommand) -> Result<(Self, MetadataOutcome), RecipeError> {
        let mut next = self.clone();
        let outcome = match command {
            MetadataCommand::Export(id) => {
                let recipe = self.get(&id).ok_or(RecipeError::NotFound)?;
                MetadataOutcome::Exported(recipe.export()?)
            }
            MetadataCommand::Delete(id) => {
                let index = self.index(&id)?;
                next.0.remove(index);
                MetadataOutcome::Deleted(id)
            }
            MetadataCommand::Edit(recipe) => {
                let index = self.index(&recipe.id)?;
                let id = recipe.id.clone();
                // Editing never retargets a logical session or host. Make a
                // new recipe instead; live sessions keep their own snapshot.
                if self.0[index].host != recipe.host
                    || self.0[index].logical_session != recipe.logical_session
                {
                    return Err(RecipeError::Invalid);
                }
                next.0[index] = recipe;
                MetadataOutcome::Saved(id)
            }
            MetadataCommand::Save(recipe) => next.insert(recipe)?,
            MetadataCommand::Import(text) => {
                // Preserve the advertised target. A collision is not an edit
                // and never silently allocates another remote shell.
                next.insert(PublicRecipe::import(&text)?)?
            }
            MetadataCommand::Duplicate {
                source,
                name,
                policy,
            } => {
                let mut recipe = self.get(&source).ok_or(RecipeError::NotFound)?.clone();
                recipe.id = RecipeId::new();
                recipe.name = name;
                match policy {
                    DuplicatePolicy::FreshLogicalSession => {
                        recipe.logical_session = LogicalSessionId::new();
                        // An old generation cannot be attached under a new ID.
                        if matches!(recipe.behavior, ConnectionBehavior::AttachOnly { .. }) {
                            recipe.behavior = ConnectionBehavior::ResumeOrCreate;
                        }
                    }
                    DuplicatePolicy::ExistingSession { generation } => {
                        recipe.behavior = ConnectionBehavior::AttachOnly { generation };
                    }
                }
                next.insert(recipe)?
            }
        };
        next.validate()?;
        Ok((next, outcome))
    }

    fn index(&self, id: &RecipeId) -> Result<usize, RecipeError> {
        self.0
            .iter()
            .position(|recipe| &recipe.id == id)
            .ok_or(RecipeError::NotFound)
    }

    fn insert(&mut self, recipe: PublicRecipe) -> Result<MetadataOutcome, RecipeError> {
        if self.get(&recipe.id).is_some() {
            return Err(RecipeError::Duplicate);
        }
        let id = recipe.id.clone();
        self.0.push(recipe);
        Ok(MetadataOutcome::Saved(id))
    }
}
