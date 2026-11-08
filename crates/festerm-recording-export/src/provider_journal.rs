use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use festerm_recording::{LocalStart, NameTemplates, RecordingId, MAX_STREAMS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    publish_provider::validate_entry, source_unix::require_private, ProviderEntry,
    ProviderPublication, StagedRecording,
};

const VERSION: u8 = 1;
const MAX_JOURNAL_BYTES: u64 = 256 * 1024;
const MAX_JOURNALS: usize = 4096;

/// Durable information for reporting an interrupted or completed provider
/// export. A missing directory identity means creation may have happened but
/// the provider did not return its identity before interruption.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderJournalRecord {
    version: u8,
    recording_id: String,
    stage_id: String,
    destination_id: String,
    requested_directory: String,
    directory: Option<ProviderEntry>,
    videos: Option<Vec<ProviderEntry>>,
    #[serde(default)]
    appearance: Option<ProviderEntry>,
}

impl ProviderJournalRecord {
    pub fn recording_id(&self) -> RecordingId {
        RecordingId::parse(&self.recording_id).expect("validated journal recording ID")
    }

    pub fn destination_id(&self) -> &str {
        &self.destination_id
    }

    pub fn requested_directory(&self) -> &str {
        &self.requested_directory
    }

    pub fn directory(&self) -> Option<&ProviderEntry> {
        self.directory.as_ref()
    }

    /// `Some` means all videos were published and read back successfully.
    pub fn published_videos(&self) -> Option<&[ProviderEntry]> {
        self.videos.as_deref()
    }

    pub fn published_appearance(&self) -> Option<&ProviderEntry> {
        self.appearance.as_ref()
    }
}

/// One app-private journal. The caller must put its directory in storage
/// excluded from backup before calling `begin`.
pub struct ProviderJournal {
    directory_path: PathBuf,
    directory: File,
    name: String,
    device: u64,
    inode: u64,
    digest: [u8; 32],
    record: ProviderJournalRecord,
}

impl ProviderJournal {
    /// Durably records an export intent before asking a provider to create a
    /// directory. A crash before the provider returns a URI is therefore
    /// visible as an export whose destination should be checked.
    pub fn begin(
        directory_path: &Path,
        staged: &StagedRecording,
        destination_id: &str,
        templates: &NameTemplates,
        start: LocalStart,
    ) -> io::Result<Self> {
        let directory = open_directory(directory_path)?;
        let source_name = staged.source.name().to_str().ok_or_else(invalid_journal)?;
        let recording_id = source_name
            .strip_prefix("recording-")
            .and_then(|name| {
                name.strip_suffix(".fstrec")
                    .or_else(|| name.strip_suffix(".partial"))
            })
            .ok_or_else(invalid_journal)?;
        let recording_id = RecordingId::parse(recording_id).map_err(|_| invalid_journal())?;
        let requested_directory = templates
            .directory(recording_id, start)
            .map_err(io::Error::other)?;
        let record = ProviderJournalRecord {
            version: VERSION,
            recording_id: recording_id.to_string(),
            stage_id: staged.stage_id.to_string(),
            destination_id: destination_id.to_owned(),
            requested_directory,
            directory: None,
            videos: None,
            appearance: None,
        };
        validate_record(&record)?;
        let bytes = serialize(&record)?;
        let name = format!("provider-{}.json", Uuid::new_v4().simple());
        let mut file =
            festerm_unix_security::open_private_file(&directory, Path::new(&name), true)?;
        let initialized = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            directory.sync_all()?;
            check_directory(directory_path, &directory)?;
            Ok(())
        })();
        if let Err(error) = initialized {
            return Err(cleanup_error(&directory, &name, error));
        }
        let metadata = file.metadata()?;
        Ok(Self {
            directory_path: directory_path.to_owned(),
            directory,
            name,
            device: metadata.dev(),
            inode: metadata.ino(),
            digest: Sha256::digest(&bytes).into(),
            record,
        })
    }

    /// Loads every journal in the private directory. Malformed or unsafe
    /// entries are errors rather than silently hiding interrupted exports.
    pub fn recover(directory_path: &Path) -> io::Result<Vec<Self>> {
        let directory = open_directory(directory_path)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(directory_path)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with("provider-") || !name.ends_with(".json") {
                continue;
            }
            let opaque = &name["provider-".len()..name.len() - ".json".len()];
            if opaque.len() != 32 || !opaque.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid_journal());
            }
            names.push(name.to_owned());
            if names.len() > MAX_JOURNALS {
                return Err(io::Error::other("too many provider export journals"));
            }
        }
        check_directory(directory_path, &directory)?;
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let (metadata, bytes) = read_entry(&directory, &name)?;
                let record = serde_json::from_slice(&bytes).map_err(|_| invalid_journal())?;
                validate_record(&record)?;
                Ok(Self {
                    directory_path: directory_path.to_owned(),
                    directory: directory.try_clone()?,
                    name,
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    digest: Sha256::digest(&bytes).into(),
                    record,
                })
            })
            .collect()
    }

    pub fn record(&self) -> &ProviderJournalRecord {
        &self.record
    }

    /// Pass this to `publish_to_provider`'s `remember_partial` callback.
    /// No video may be created until this durable update succeeds.
    pub fn remember_directory(&mut self, entry: &ProviderEntry) -> io::Result<()> {
        if self.record.directory.is_some() || self.record.videos.is_some() {
            return Err(invalid_journal());
        }
        let mut next = self.record.clone();
        next.directory = Some(entry.clone());
        self.replace(next)
    }

    /// Durably distinguishes a verified complete set from a partial export.
    /// The caller must do this before applying source-retention policy.
    pub fn mark_published(&mut self, published: &ProviderPublication) -> io::Result<()> {
        if self.record.stage_id != published.stage_id.to_string()
            || self.record.directory.as_ref() != Some(&published.directory)
            || self.record.videos.is_some()
        {
            return Err(invalid_journal());
        }
        let mut next = self.record.clone();
        next.videos = Some(published.videos.clone());
        next.appearance = Some(published.appearance.clone());
        self.replace(next)
    }

    /// Removes only the unchanged journal bound to this handle. The host may
    /// retain partial records until the user explicitly acknowledges them.
    pub fn remove(self) -> io::Result<()> {
        self.verify_current()?;
        festerm_unix_security::remove_private_staging_file(&self.directory, Path::new(&self.name))?;
        self.directory.sync_all()
    }

    fn replace(&mut self, record: ProviderJournalRecord) -> io::Result<()> {
        validate_record(&record)?;
        let bytes = serialize(&record)?;
        check_directory(&self.directory_path, &self.directory)?;
        let staging = format!(".provider-{}.tmp", Uuid::new_v4().simple());
        let mut file =
            festerm_unix_security::open_private_file(&self.directory, Path::new(&staging), true)?;
        let prepared = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            self.verify_current()?;
            festerm_unix_security::replace_private_file(
                &self.directory,
                Path::new(&staging),
                Path::new(&self.name),
            )
        })();
        if let Err(error) = prepared {
            return Err(cleanup_error(&self.directory, &staging, error));
        }
        let metadata = file.metadata()?;
        self.device = metadata.dev();
        self.inode = metadata.ino();
        self.digest = Sha256::digest(&bytes).into();
        self.record = record;
        self.directory.sync_all()?;
        check_directory(&self.directory_path, &self.directory)
    }

    fn verify_current(&self) -> io::Result<()> {
        check_directory(&self.directory_path, &self.directory)?;
        let (metadata, bytes) = read_entry(&self.directory, &self.name)?;
        if metadata.dev() != self.device
            || metadata.ino() != self.inode
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != self.digest
        {
            return Err(io::Error::other("provider export journal changed"));
        }
        Ok(())
    }
}

fn open_directory(path: &Path) -> io::Result<File> {
    let directory = File::open(path)?;
    check_directory(path, &directory)?;
    Ok(directory)
}

fn check_directory(path: &Path, directory: &File) -> io::Result<()> {
    require_private(directory, true, "provider journal directory").map_err(io::Error::other)?;
    let pinned = directory.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
        return Err(io::Error::other("provider journal directory changed"));
    }
    Ok(())
}

fn read_entry(directory: &File, name: &str) -> io::Result<(fs::Metadata, Vec<u8>)> {
    let file = festerm_unix_security::open_private_file(directory, Path::new(name), false)?;
    require_private(&file, false, "provider journal").map_err(io::Error::other)?;
    let metadata = file.metadata()?;
    if metadata.nlink() != 1 || metadata.len() > MAX_JOURNAL_BYTES {
        return Err(invalid_journal());
    }
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(invalid_journal());
    }
    Ok((metadata, bytes))
}

fn validate_record(record: &ProviderJournalRecord) -> io::Result<()> {
    if record.version != VERSION
        || RecordingId::parse(&record.recording_id).is_err()
        || Uuid::parse_str(&record.stage_id).is_err()
        || record.destination_id.is_empty()
        || record.destination_id.len() > 8192
        || record.destination_id.chars().any(char::is_control)
        || record.requested_directory.is_empty()
        || record.requested_directory.len() > 1024
        || record
            .requested_directory
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(invalid_journal());
    }
    if let Some(directory) = &record.directory {
        validate_entry(directory).map_err(|_| invalid_journal())?;
    }
    if let Some(videos) = &record.videos {
        let directory = record.directory.as_ref().ok_or_else(invalid_journal)?;
        if videos.is_empty() || videos.len() > MAX_STREAMS {
            return Err(invalid_journal());
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for video in videos {
            validate_entry(video).map_err(|_| invalid_journal())?;
            if video.id == directory.id
                || !ids.insert(&video.id)
                || !names.insert(&video.display_name)
            {
                return Err(invalid_journal());
            }
        }
        if let Some(appearance) = &record.appearance {
            validate_entry(appearance).map_err(|_| invalid_journal())?;
            if appearance.id == directory.id
                || !ids.insert(&appearance.id)
                || !names.insert(&appearance.display_name)
                || appearance.display_name != crate::APPEARANCE_FILE_NAME
            {
                return Err(invalid_journal());
            }
        }
    } else if record.appearance.is_some() {
        return Err(invalid_journal());
    }
    Ok(())
}

fn serialize(record: &ProviderJournalRecord) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(invalid_journal());
    }
    Ok(bytes)
}

fn cleanup_error(directory: &File, name: &str, error: io::Error) -> io::Error {
    match festerm_unix_security::remove_private_staging_file(directory, Path::new(name)) {
        Ok(()) => error,
        Err(cleanup) => io::Error::new(
            error.kind(),
            format!("{error}; could not remove private journal staging: {cleanup}"),
        ),
    }
}

fn invalid_journal() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid provider export journal",
    )
}
