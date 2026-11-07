use std::{
    collections::HashSet,
    fmt,
    io::{self, Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use festerm_recording::{LocalStart, NameTemplates, RecordingId};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{fingerprint, CancelledReader, ExportError, StagedRecording, APPEARANCE_FILE_NAME};

/// A provider-issued identity and the actual name visible to the user.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    pub id: String,
    pub display_name: String,
}

/// Document-provider operations used for non-atomic publication.
///
/// Creation must return a newly created document, never an existing document.
/// A provider may adjust names; its returned names and identities are retained.
pub trait RecordingProvider {
    type Error: fmt::Display;
    type Writer: Write;
    type Reader: Read;

    fn create_directory(&mut self, name: &str) -> Result<ProviderEntry, Self::Error>;
    fn create_video(
        &mut self,
        directory: &ProviderEntry,
        name: &str,
    ) -> Result<ProviderEntry, Self::Error>;
    fn create_appearance(
        &mut self,
        directory: &ProviderEntry,
        name: &str,
    ) -> Result<ProviderEntry, Self::Error>;
    fn open_for_write(&mut self, video: &ProviderEntry) -> Result<Self::Writer, Self::Error>;
    fn open_for_read(&mut self, video: &ProviderEntry) -> Result<Self::Reader, Self::Error>;
}

/// A complete set verified through the provider.
#[derive(Debug)]
pub struct ProviderPublication {
    pub directory: ProviderEntry,
    pub videos: Vec<ProviderEntry>,
    pub appearance: ProviderEntry,
    pub(crate) stage_id: Uuid,
    source_deletion_attempted: bool,
}

/// A failed export may have left a visible, incomplete destination directory.
#[derive(Debug)]
pub struct ProviderPublicationError {
    message: String,
    pub partial_directory: Option<ProviderEntry>,
}

impl fmt::Display for ProviderPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)?;
        if let Some(directory) = &self.partial_directory {
            write!(
                formatter,
                "; the incomplete folder {:?} may still be visible",
                directory.display_name
            )?;
        } else {
            write!(
                formatter,
                "; check the destination for an incomplete folder"
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderPublicationError {}

/// Publishes private staged videos sequentially. The provider directory can
/// be visible before the set is complete; no provider content is deleted on
/// failure. `remember_partial` must durably record its identity before any
/// video is created so a later launch can report an interrupted export.
pub fn publish_to_provider<P: RecordingProvider>(
    staged: &StagedRecording,
    provider: &mut P,
    templates: &NameTemplates,
    id: RecordingId,
    start: LocalStart,
    cancelled: &AtomicBool,
    remember_partial: impl FnOnce(&ProviderEntry) -> io::Result<()>,
) -> Result<ProviderPublication, ProviderPublicationError> {
    let mut partial = None;
    let result = (|| {
        let staging_directory = staged
            .staging_directory()
            .map_err(|error| error.to_string())?;
        #[cfg(not(windows))]
        crate::source_unix::require_private(staging_directory, true, "video staging directory")
            .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        crate::source_windows::require_private(staging_directory, true, "video staging directory")
            .map_err(|error| error.to_string())?;
        let directory_name = templates
            .directory(id, start)
            .map_err(|error| error.to_string())?;
        let names = staged
            .videos
            .iter()
            .map(|video| {
                templates
                    .session(id, start, video.stream)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        ensure_not_cancelled(cancelled)?;
        let directory = provider
            .create_directory(&directory_name)
            .map_err(|error| format!("could not create a new export folder: {error}"))?;
        partial = Some(directory.clone());
        validate_entry(&directory)?;
        remember_partial(&directory)
            .map_err(|error| format!("could not remember the incomplete export: {error}"))?;
        if staged.recovered_prefix && !directory.display_name.contains("-recovered") {
            return Err(
                "the provider removed the recovered-prefix label from the folder".to_owned(),
            );
        }

        let mut videos = Vec::with_capacity(staged.videos.len());
        let mut ids = HashSet::new();
        let mut actual_names = HashSet::new();
        for (name, video) in names.into_iter().zip(&staged.videos) {
            ensure_not_cancelled(cancelled)?;
            let entry = provider
                .create_video(&directory, &name)
                .map_err(|error| format!("could not create a new destination MP4: {error}"))?;
            validate_entry(&entry)?;
            if entry.id == directory.id
                || !ids.insert(entry.id.clone())
                || !actual_names.insert(entry.display_name.clone())
            {
                return Err("the provider returned a duplicate destination".to_owned());
            }
            videos.push(entry);
            let entry = videos.last().expect("destination was added");
            let filename = video
                .path
                .file_name()
                .ok_or_else(|| "staged MP4 does not have a filename".to_owned())?;
            #[cfg(not(windows))]
            let source =
                festerm_unix_security::open_file_nofollow(staging_directory, Path::new(filename));
            #[cfg(windows)]
            let source = festerm_windows_security::open_file_no_reparse(
                staging_directory,
                Path::new(filename),
            );
            let mut source =
                source.map_err(|error| format!("could not open the staged MP4: {error}"))?;
            #[cfg(not(windows))]
            crate::source_unix::require_private(&source, false, "staged MP4")
                .map_err(|error| error.to_string())?;
            #[cfg(windows)]
            crate::source_windows::require_private(&source, false, "staged MP4")
                .map_err(|error| error.to_string())?;
            let mut output = provider
                .open_for_write(entry)
                .map_err(|error| format!("could not write destination MP4: {error}"))?;
            let mut hash = Sha256::new();
            let mut length = 0u64;
            let mut buffer = [0; 64 * 1024];
            loop {
                ensure_not_cancelled(cancelled)?;
                let count = source
                    .read(&mut buffer)
                    .map_err(|error| format!("could not read staged MP4: {error}"))?;
                if count == 0 {
                    break;
                }
                length = length
                    .checked_add(count as u64)
                    .filter(|length| *length <= video.length)
                    .ok_or_else(|| "a staged MP4 changed before publication".to_owned())?;
                hash.update(&buffer[..count]);
                output
                    .write_all(&buffer[..count])
                    .map_err(|error| format!("could not write destination MP4: {error}"))?;
            }
            output
                .flush()
                .map_err(|error| format!("could not finish destination MP4: {error}"))?;
            drop(output);
            if length != video.length || <[u8; 32]>::from(hash.finalize()) != video.fingerprint {
                return Err("a staged MP4 changed before publication".to_owned());
            }
            ensure_not_cancelled(cancelled)?;
            let input = provider
                .open_for_read(entry)
                .map_err(|error| format!("could not verify destination MP4: {error}"))?;
            let limit = video
                .length
                .checked_add(1)
                .ok_or_else(|| "staged MP4 size overflows".to_owned())?;
            let (actual, length) = fingerprint(&mut CancelledReader {
                inner: input.take(limit),
                cancelled,
            })
            .map_err(|error| format!("could not verify destination MP4: {error}"))?;
            if actual != video.fingerprint || length != video.length {
                return Err("a destination MP4 did not match the staged video".to_owned());
            }
        }
        ensure_not_cancelled(cancelled)?;
        let appearance = provider
            .create_appearance(&directory, APPEARANCE_FILE_NAME)
            .map_err(|error| format!("could not create recording appearance: {error}"))?;
        validate_entry(&appearance)?;
        if appearance.id == directory.id
            || !ids.insert(appearance.id.clone())
            || !actual_names.insert(appearance.display_name.clone())
            || appearance.display_name != APPEARANCE_FILE_NAME
        {
            return Err("the provider changed or reused the recording appearance name".to_owned());
        }
        let mut output = provider
            .open_for_write(&appearance)
            .map_err(|error| format!("could not write recording appearance: {error}"))?;
        output
            .write_all(&staged.appearance_metadata)
            .and_then(|()| output.flush())
            .map_err(|error| format!("could not write recording appearance: {error}"))?;
        drop(output);
        ensure_not_cancelled(cancelled)?;
        let input = provider
            .open_for_read(&appearance)
            .map_err(|error| format!("could not verify recording appearance: {error}"))?;
        let mut actual = Vec::new();
        CancelledReader {
            inner: input.take(staged.appearance_metadata.len() as u64 + 1),
            cancelled,
        }
        .read_to_end(&mut actual)
        .map_err(|error| format!("could not verify recording appearance: {error}"))?;
        if actual != staged.appearance_metadata {
            return Err("recording appearance did not match the export".to_owned());
        }
        ensure_not_cancelled(cancelled)?;
        Ok(ProviderPublication {
            directory,
            videos,
            appearance,
            stage_id: staged.stage_id,
            source_deletion_attempted: false,
        })
    })();
    result.map_err(|message| ProviderPublicationError {
        message,
        partial_directory: partial,
    })
}

/// Removes a private source only after its exact complete set was verified.
pub fn delete_source_after_provider_publication(
    staged: &StagedRecording,
    published: &mut ProviderPublication,
) -> Result<(), ExportError> {
    staged.require_complete_source()?;
    if staged.stage_id != published.stage_id {
        return Err(ExportError::new(
            "the published videos do not belong to this recording source",
        ));
    }
    if std::mem::replace(&mut published.source_deletion_attempted, true) {
        return Err(ExportError::new(
            "source deletion was already attempted for this publication",
        ));
    }
    staged.source.remove_verified(staged.source_fingerprint)
}

pub(crate) fn validate_entry(entry: &ProviderEntry) -> Result<(), String> {
    if entry.id.is_empty()
        || entry.id.len() > 8192
        || entry.id.chars().any(char::is_control)
        || entry.display_name.is_empty()
        || entry.display_name.len() > 1024
        || entry
            .display_name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err("the provider returned an invalid document identity or name".to_owned());
    }
    Ok(())
}

fn ensure_not_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("recording publication was cancelled".to_owned())
    } else {
        Ok(())
    }
}
