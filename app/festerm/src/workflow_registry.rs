use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use festerm_automation_js::snapshot::{ApprovedBundle, SourceSnapshot, MAX_SNAPSHOT_BYTES};
use festerm_sessiond::private_storage;
use serde::{Deserialize, Serialize};

const INDEX_VERSION: u8 = 1;
const INDEX_NAME: &str = "index-v1.json";
const LOCK_NAME: &str = "registry.lock";
const MAX_INDEX_BYTES: usize = 512 * 1024;
const MAX_ENTRIES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) digest: String,
    pub(crate) name: String,
    pub(crate) imported_at: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct View {
    pub(crate) entries: Vec<Entry>,
    pub(crate) quarantined: usize,
    pub(crate) error: Option<String>,
    pub(crate) operation_error: Option<String>,
    pub(crate) busy: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Index {
    version: u8,
    entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIndex {
    version: u8,
    entries: Vec<EntryCandidate>,
}

#[derive(Debug)]
struct EntryCandidate(Option<Entry>);

impl<'de> Deserialize<'de> for EntryCandidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct EntryVisitor;

        impl<'de> serde::de::Visitor<'de> for EntryVisitor {
            type Value = EntryCandidate;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a workflow registry entry")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut digest = None;
                let mut name = None;
                let mut imported_at = None;
                let mut valid = true;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "digest" => {
                            let value: serde_json::Value = map.next_value()?;
                            valid &= digest.replace(value.as_str().map(str::to_owned)).is_none();
                        }
                        "name" => {
                            let value: serde_json::Value = map.next_value()?;
                            valid &= name.replace(value.as_str().map(str::to_owned)).is_none();
                        }
                        "importedAt" => {
                            let value: serde_json::Value = map.next_value()?;
                            valid &= imported_at.replace(value.as_u64()).is_none();
                        }
                        _ => {
                            let _: serde_json::Value = map.next_value()?;
                            valid = false;
                        }
                    }
                }
                let entry = if valid {
                    match (digest.flatten(), name.flatten(), imported_at.flatten()) {
                        (Some(digest), Some(name), Some(imported_at)) => {
                            let entry = Entry {
                                digest,
                                name,
                                imported_at,
                            };
                            validate_entry(&entry).ok().map(|_| entry)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                Ok(EntryCandidate(entry))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(EntryCandidate(None))
            }

            fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
                Ok(EntryCandidate(None))
            }

            fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
                Ok(EntryCandidate(None))
            }

            fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
                Ok(EntryCandidate(None))
            }

            fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
                Ok(EntryCandidate(None))
            }

            fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
                Ok(EntryCandidate(None))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(EntryCandidate(None))
            }
        }

        deserializer.deserialize_any(EntryVisitor)
    }
}

#[derive(Clone)]
pub(crate) struct WorkflowRegistry {
    directory: PathBuf,
}

#[derive(Debug)]
pub(crate) struct RegistryError(String);

impl std::fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RegistryError {}

fn error(message: &str) -> RegistryError {
    RegistryError(message.to_owned())
}

impl WorkflowRegistry {
    pub(crate) fn native() -> Option<Self> {
        directories::ProjectDirs::from("com", "fes", "fesTerm").map(|project| Self {
            directory: project.data_local_dir().join("workflows").join("v1"),
        })
    }

    #[cfg(test)]
    pub(crate) fn at(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub(crate) fn view(&self) -> View {
        match self.read_index() {
            Ok((entries, quarantined)) => View {
                entries,
                quarantined,
                error: None,
                busy: false,
                operation_error: None,
            },
            Err(error) => View {
                entries: Vec::new(),
                quarantined: 0,
                error: Some(error.to_string()),
                busy: false,
                operation_error: None,
            },
        }
    }

    pub(crate) fn import_bundle(&self, bundle: &ApprovedBundle) -> Result<Entry, RegistryError> {
        let canonical = serde_json::to_vec(bundle.snapshot())
            .map_err(|_| error("The workflow could not be serialized for the registry."))?;
        if canonical.len() > MAX_SNAPSHOT_BYTES {
            return Err(error("The workflow exceeds the registry artifact limit."));
        }
        let imported_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| error("The device clock could not produce a registry timestamp."))?
            .as_secs();
        let entry = Entry {
            digest: bundle.digest().to_owned(),
            name: bundle.manifest().name.clone(),
            imported_at,
        };
        self.with_lock(|directory| {
            let (mut entries, _) = read_index_file(directory)?;
            if entries.len() >= MAX_ENTRIES
                && entries
                    .iter()
                    .all(|existing| existing.digest != entry.digest)
            {
                return Err(error(
                    "The workflow registry is full; remove a workflow first.",
                ));
            }
            let artifact = artifact_name(&entry.digest)?;
            match read_artifact(directory, &entry.digest) {
                Ok(existing) => {
                    if existing.digest() != bundle.digest() {
                        return Err(error("A registry artifact with this digest is different."));
                    }
                }
                Err(RegistryArtifactError::Missing) => {
                    private_storage::replace_private(directory, &artifact, &canonical)
                        .map_err(|_| error("The workflow artifact could not be saved securely."))?;
                }
                Err(RegistryArtifactError::Invalid) => {
                    return Err(error(
                        "A corrupt registry artifact must be removed before reimport.",
                    ));
                }
            }
            entries.retain(|existing| existing.digest != entry.digest);
            entries.push(entry.clone());
            entries.sort_by(|left, right| left.digest.cmp(&right.digest));
            write_index(directory, &entries)?;
            Ok(entry)
        })
    }

    pub(crate) fn import_source(
        &self,
        source: &str,
    ) -> Result<(ApprovedBundle, Entry), RegistryError> {
        if source.len() > MAX_SNAPSHOT_BYTES {
            return Err(error(
                "The workflow source exceeds the registry snapshot limit.",
            ));
        }
        let snapshot: SourceSnapshot = serde_json::from_str(source)
            .map_err(|_| error("The workflow is not valid JSON or does not match its schema."))?;
        let bundle = ApprovedBundle::new(snapshot)
            .map_err(|_| error("The workflow manifest, modules or JavaScript are invalid."))?;
        let entry = self.import_bundle(&bundle)?;
        Ok((bundle, entry))
    }

    pub(crate) fn load(&self, digest: &str) -> Result<ApprovedBundle, RegistryError> {
        validate_digest(digest)?;
        self.with_lock(|directory| {
            let (entries, _) = read_index_file(directory)?;
            if !entries.iter().any(|entry| entry.digest == digest) {
                return Err(error("That workflow is no longer registered."));
            }
            read_artifact(directory, digest).map_err(|failure| match failure {
                RegistryArtifactError::Missing => {
                    error("The registered workflow artifact is missing.")
                }
                RegistryArtifactError::Invalid => error(
                    "The registered workflow artifact is corrupt or does not match its digest.",
                ),
            })
        })
    }

    pub(crate) fn remove(&self, digest: &str) -> Result<(), RegistryError> {
        validate_digest(digest)?;
        self.with_lock(|directory| {
            let (mut entries, _) = read_index_file(directory)?;
            let before = entries.len();
            entries.retain(|entry| entry.digest != digest);
            if entries.len() == before {
                return Err(error("That workflow is no longer registered."));
            }
            write_index(directory, &entries)?;
            private_storage::remove_private_file(directory, &artifact_name(digest)?).map_err(|_| {
                error(
                    "The workflow was unregistered, but its private artifact could not be removed.",
                )
            })
        })
    }

    fn read_index(&self) -> Result<(Vec<Entry>, usize), RegistryError> {
        self.with_lock(read_index_file)
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce(&File) -> Result<T, RegistryError>,
    ) -> Result<T, RegistryError> {
        let directory = ensure_private_directory(&self.directory)
            .map_err(|_| error("Private workflow registry storage is unavailable."))?;
        let lock = private_storage::open_trusted_lock(&directory, Path::new(LOCK_NAME))
            .map_err(|_| error("The workflow registry lock is unavailable."))?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|_| error("Another window is changing the workflow registry."))?;
        let _unlock = private_storage::UnlockOnDrop(lock);
        operation(&directory)
    }
}

enum RegistryArtifactError {
    Missing,
    Invalid,
}

fn read_index_file(directory: &File) -> Result<(Vec<Entry>, usize), RegistryError> {
    let bytes =
        match private_storage::read_private(directory, Path::new(INDEX_NAME), MAX_INDEX_BYTES) {
            Ok(bytes) => bytes,
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => {
                return Ok((Vec::new(), 0))
            }
            Err(_) => {
                return Err(error(
                    "The workflow registry index is unavailable or too large.",
                ))
            }
        };
    let raw: RawIndex = serde_json::from_slice(&bytes)
        .map_err(|_| error("The workflow registry index is corrupt."))?;
    if raw.version != INDEX_VERSION {
        return Err(error(
            "The workflow registry index uses an unsupported version.",
        ));
    }
    let mut entries = BTreeMap::new();
    let mut quarantined = 0;
    for raw in raw.entries {
        let Some(entry) = raw.0 else {
            quarantined += 1;
            continue;
        };
        match entries.entry(entry.digest.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(entry);
            }
            std::collections::btree_map::Entry::Occupied(_) => quarantined += 1,
        }
    }
    if entries.len() > MAX_ENTRIES {
        return Err(error("The workflow registry contains too many entries."));
    }
    Ok((entries.into_values().collect(), quarantined))
}

fn write_index(directory: &File, entries: &[Entry]) -> Result<(), RegistryError> {
    let index = Index {
        version: INDEX_VERSION,
        entries: entries.to_vec(),
    };
    let bytes = serde_json::to_vec(&index)
        .map_err(|_| error("The workflow registry index could not be serialized."))?;
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(error("The workflow registry index is too large."));
    }
    private_storage::replace_private(directory, Path::new(INDEX_NAME), &bytes)
        .map_err(|_| error("The workflow registry index could not be saved securely."))
}

fn read_artifact(directory: &File, digest: &str) -> Result<ApprovedBundle, RegistryArtifactError> {
    let bytes = private_storage::read_private(
        directory,
        &artifact_name(digest).map_err(|_| RegistryArtifactError::Invalid)?,
        MAX_SNAPSHOT_BYTES,
    )
    .map_err(|failure| {
        if failure.kind() == io::ErrorKind::NotFound {
            RegistryArtifactError::Missing
        } else {
            RegistryArtifactError::Invalid
        }
    })?;
    let snapshot: SourceSnapshot =
        serde_json::from_slice(&bytes).map_err(|_| RegistryArtifactError::Invalid)?;
    let bundle = ApprovedBundle::new(snapshot).map_err(|_| RegistryArtifactError::Invalid)?;
    if bundle.digest() != digest {
        return Err(RegistryArtifactError::Invalid);
    }
    Ok(bundle)
}

fn artifact_name(digest: &str) -> Result<PathBuf, RegistryError> {
    validate_digest(digest)?;
    Ok(PathBuf::from(format!("{digest}.workflow.json")))
}

fn validate_entry(entry: &Entry) -> Result<(), RegistryError> {
    validate_digest(&entry.digest)?;
    if entry.name.is_empty()
        || entry.name.len() > 256
        || entry.name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
        || entry.imported_at == 0
    {
        return Err(error("A workflow registry entry is invalid."));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<(), RegistryError> {
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(error("A workflow registry digest is invalid."))
    }
}

fn ensure_private_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "relative workflow registry",
        ));
    }
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(_) => break,
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_owned());
                current = current
                    .parent()
                    .ok_or_else(|| io::Error::other("registry root"))?;
            }
            Err(failure) => return Err(failure),
        }
    }
    for missing in missing.into_iter().rev() {
        drop(private_storage::private_directory(&missing, true)?);
        #[cfg(unix)]
        File::open(
            missing
                .parent()
                .ok_or_else(|| io::Error::other("registry parent"))?,
        )?
        .sync_all()?;
    }
    private_storage::private_directory(path, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn registry() -> (tempfile::TempDir, WorkflowRegistry) {
        let root = crate::recording_storage::test_private_parent();
        let path = crate::recording_storage::test_directory_path(root.path())
            .join("workflows")
            .join("v1");
        (root, WorkflowRegistry::at(path))
    }

    fn bundle(name: &str) -> ApprovedBundle {
        use festerm_automation_js::snapshot::{Capability, Manifest};
        ApprovedBundle::new(SourceSnapshot {
            manifest: Manifest {
                version: 1,
                api_version: 1,
                name: name.into(),
                entry: "main.js".into(),
                modules: vec!["main.js".into()],
                parameters: BTreeMap::new(),
                capabilities: BTreeSet::<Capability>::new(),
            },
            modules: BTreeMap::from([(
                "main.js".into(),
                "export async function main(){ return; }".into(),
            )]),
        })
        .unwrap()
    }

    #[test]
    fn import_is_explicit_deduplicated_and_loads_immutable_bytes() {
        let (_root, registry) = registry();
        let expected = bundle("one");
        let imported = registry.import_bundle(&expected).unwrap();
        assert_eq!(imported.digest, expected.digest());
        registry.import_bundle(&expected).unwrap();
        let view = registry.view();
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.quarantined, 0);
        assert_eq!(
            registry.load(expected.digest()).unwrap().digest(),
            expected.digest()
        );
    }

    #[test]
    fn removing_an_entry_never_followed_a_mutable_source_path() {
        let (_root, registry) = registry();
        let expected = bundle("remove");
        registry.import_bundle(&expected).unwrap();
        registry.remove(expected.digest()).unwrap();
        assert!(registry.view().entries.is_empty());
        assert!(registry.load(expected.digest()).is_err());
    }

    #[test]
    fn invalid_source_never_creates_a_registry_entry() {
        let (_root, registry) = registry();
        assert!(registry.import_source("{broken").is_err());
        assert!(registry.view().entries.is_empty());
    }

    #[test]
    fn corrupt_artifact_refuses_without_falling_back_to_a_source_path() {
        let (_root, registry) = registry();
        let expected = bundle("corrupt");
        registry.import_bundle(&expected).unwrap();
        let directory = ensure_private_directory(&registry.directory).unwrap();
        private_storage::replace_private(
            &directory,
            &artifact_name(expected.digest()).unwrap(),
            b"{broken",
        )
        .unwrap();

        assert!(registry.load(expected.digest()).is_err());
        let view = registry.view();
        assert_eq!(
            view.entries.len(),
            1,
            "metadata remains available for recovery"
        );
    }

    #[test]
    fn malformed_index_entries_are_quarantined_individually() {
        let (_root, registry) = registry();
        let valid = registry.import_bundle(&bundle("valid")).unwrap();
        let directory = ensure_private_directory(&registry.directory).unwrap();
        let index = serde_json::json!({
            "version": 1,
            "entries": [
                valid,
                {"digest": "not-a-digest", "name": "bad", "importedAt": 1}
            ]
        });
        private_storage::replace_private(
            &directory,
            Path::new(INDEX_NAME),
            &serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();

        let view = registry.view();
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.quarantined, 1);
    }

    #[test]
    fn duplicate_entry_fields_and_duplicate_digests_cannot_retarget_metadata() {
        let (_root, registry) = registry();
        let valid = bundle("first");
        registry.import_bundle(&valid).unwrap();
        let digest = valid.digest();
        let directory = ensure_private_directory(&registry.directory).unwrap();
        let index = format!(
            "{{\"version\":1,\"entries\":[{{\"digest\":\"{digest}\",\"name\":\"first\",\"importedAt\":1}},{{\"digest\":\"{digest}\",\"name\":\"second\",\"name\":\"poison\",\"importedAt\":2}},{{\"digest\":\"{digest}\",\"name\":\"third\",\"importedAt\":3}}]}}"
        );
        private_storage::replace_private(&directory, Path::new(INDEX_NAME), index.as_bytes())
            .unwrap();

        let view = registry.view();
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].name, "first");
        assert_eq!(view.quarantined, 2);
    }

    #[test]
    fn a_full_registry_refuses_before_writing_an_orphan_artifact() {
        let (_root, registry) = registry();
        let directory = ensure_private_directory(&registry.directory).unwrap();
        let entries = (0..MAX_ENTRIES)
            .map(|number| Entry {
                digest: format!("{number:064x}"),
                name: format!("workflow-{number}"),
                imported_at: 1,
            })
            .collect::<Vec<_>>();
        write_index(&directory, &entries).unwrap();
        let before = fs::read_dir(&registry.directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".workflow.json")
            })
            .count();

        assert!(registry.import_bundle(&bundle("overflow")).is_err());
        let after = fs::read_dir(registry.directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".workflow.json")
            })
            .count();
        assert_eq!(after, before);
    }
}
