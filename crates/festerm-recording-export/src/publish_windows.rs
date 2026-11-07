use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use festerm_recording::{LocalStart, NameTemplates, RecordingId};
use festerm_windows_security::{
    create_current_user_only_directory, create_current_user_only_file, delete_directory_by_handle,
    lock_directory_for_sync, open_file_no_reparse, open_private_directory_for_move,
    rename_private_directory_noreplace, same_file_identity, secure_staging_parent,
    staging_parent_matches, StagingParentSecurity,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{collision_name, source_windows, ExportError, StagedRecording, APPEARANCE_FILE_NAME};

const MAX_COLLISIONS: usize = 10_000;

/// A synchronized directory atomically published under the retained root.
#[derive(Debug)]
pub struct PublishedRecording {
    pub directory_name: String,
    pub video_names: Vec<String>,
    stage_id: Uuid,
    source_deletion_attempted: bool,
}

/// Publishes a complete staged set without merging or replacing a directory.
pub fn publish_to_directory(
    staged: &StagedRecording,
    root: &File,
    templates: &NameTemplates,
    id: RecordingId,
    start: LocalStart,
    cancelled: &AtomicBool,
) -> Result<PublishedRecording, ExportError> {
    let staging_directory = staged.staging_directory()?;
    if !root
        .metadata()
        .map_err(|error| ExportError::new(format!("could not inspect export folder: {error}")))?
        .is_dir()
    {
        return Err(ExportError::new(
            "the selected export root is not a directory",
        ));
    }
    let parent_security = secure_staging_parent(root)
        .map_err(|error| {
            ExportError::new(format!(
                "the export folder cannot safely stage a recording: {error}"
            ))
        })?
        .ok_or_else(|| ExportError::new("the export folder permits staging-name substitution"))?;
    let sync_root = lock_directory_for_sync(root).map_err(|error| {
        ExportError::new(format!(
            "could not prepare export folder for syncing: {error}"
        ))
    })?;
    sync_root.sync_all().map_err(|error| {
        ExportError::new(format!("the export folder cannot be synced: {error}"))
    })?;
    let base = templates
        .directory(id, start)
        .map_err(|error| ExportError::new(error.to_string()))?;
    let names = staged
        .videos
        .iter()
        .map(|video| {
            templates
                .session(id, start, video.stream)
                .map_err(|error| ExportError::new(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if cancelled.load(Ordering::Acquire) {
        return Err(ExportError::new("recording publication was cancelled"));
    }
    let temp_name = format!(".festerm-export-{}.partial", Uuid::new_v4().simple());
    let directory =
        create_current_user_only_directory(root, Path::new(&temp_name)).map_err(|error| {
            ExportError::new(format!(
                "could not create private export staging {temp_name}: {error}"
            ))
        })?;
    let mut created_names = Vec::new();
    let mut published = None;
    let result = (|| {
        for (video, name) in staged.videos.iter().zip(&names) {
            if cancelled.load(Ordering::Acquire) {
                return Err(ExportError::new("recording publication was cancelled"));
            }
            let mut destination = create_current_user_only_file(&directory, Path::new(name))
                .map_err(|error| {
                    ExportError::new(format!("could not create destination MP4: {error}"))
                })?;
            created_names.push(name.clone());
            let staged_name = video
                .path
                .file_name()
                .ok_or_else(|| ExportError::new("staged MP4 does not have a valid filename"))?;
            let mut source = open_file_no_reparse(staging_directory, Path::new(staged_name))
                .map_err(|error| ExportError::new(format!("could not open staged MP4: {error}")))?;
            source_windows::require_private(&source, false, "staged MP4")?;
            let mut hash = Sha256::new();
            let mut bytes = 0u64;
            let mut buffer = [0; 64 * 1024];
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(ExportError::new("recording publication was cancelled"));
                }
                let count = source.read(&mut buffer).map_err(|error| {
                    ExportError::new(format!("could not read staged MP4: {error}"))
                })?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
                bytes = bytes
                    .checked_add(count as u64)
                    .ok_or_else(|| ExportError::new("staged video size overflows"))?;
                destination.write_all(&buffer[..count]).map_err(|error| {
                    ExportError::new(format!("could not publish staged MP4: {error}"))
                })?;
            }
            let fingerprint: [u8; 32] = hash.finalize().into();
            if fingerprint != video.fingerprint || bytes != video.length {
                return Err(ExportError::new("a staged MP4 changed before publication"));
            }
            destination.sync_all().map_err(|error| {
                ExportError::new(format!("could not sync destination MP4: {error}"))
            })?;
            crate::verify_published_file(&mut destination, video, cancelled)?;
        }
        let mut appearance =
            create_current_user_only_file(&directory, Path::new(APPEARANCE_FILE_NAME)).map_err(
                |error| ExportError::new(format!("could not create recording appearance: {error}")),
            )?;
        created_names.push(APPEARANCE_FILE_NAME.to_owned());
        crate::write_and_verify_appearance(&mut appearance, staged, cancelled)?;
        directory.sync_all().map_err(|error| {
            ExportError::new(format!("could not sync destination staging: {error}"))
        })?;
        if cancelled.load(Ordering::Acquire) {
            return Err(ExportError::new("recording publication was cancelled"));
        }
        if !staging_parent_matches(root, &parent_security).map_err(|error| {
            ExportError::new(format!("could not recheck export folder security: {error}"))
        })? {
            return Err(ExportError::new(
                "export folder security changed during publication",
            ));
        }
        for collision in 0..MAX_COLLISIONS {
            let name = collision_name(&base, collision, staged.recovered_prefix)?;
            match rename_private_directory_noreplace(&directory, root, Path::new(&name)) {
                Ok(()) => {
                    published = Some(name);
                    let target = open_private_directory_for_move(
                        root,
                        Path::new(published.as_deref().expect("directory was published")),
                    )
                    .and_then(|target| {
                        if same_file_identity(&directory, &target)? {
                            Ok(())
                        } else {
                            Err(io::Error::other("the published directory was replaced"))
                        }
                    });
                    if let Err(error) = target {
                        return Err(ExportError::new(format!(
                            "recording publication could not be verified; the source must be retained: {error}"
                        )));
                    }
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(ExportError::new(format!(
                        "could not publish recording directory without replacement: {error}"
                    )));
                }
            }
        }
        let Some(name) = published.as_ref() else {
            return Err(ExportError::new(
                "too many recording directories already use this name",
            ));
        };
        sync_root.sync_all().map_err(|error| {
            ExportError::new(format!(
                "recording directory {name} was published but could not be synced; the source must be retained: {error}"
            ))
        })?;
        Ok(())
    })();

    if let Err(error) = result {
        if published.is_none() {
            let cleanup = cleanup(root, &directory, &created_names, &parent_security);
            if !cleanup.is_empty() {
                return Err(ExportError::new(format!(
                    "{error}; destination staging {temp_name} could not be completely removed: {}",
                    cleanup.join("; ")
                )));
            }
        }
        return Err(error);
    }
    Ok(PublishedRecording {
        directory_name: published.expect("successful publication has a directory name"),
        video_names: names,
        stage_id: staged.stage_id,
        source_deletion_attempted: false,
    })
}

/// Deletes only the unchanged source belonging to this exact published set.
pub fn delete_source_after_publication(
    staged: &StagedRecording,
    published: &mut PublishedRecording,
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

fn cleanup(
    root: &File,
    directory: &File,
    names: &[String],
    parent_security: &StagingParentSecurity,
) -> Vec<String> {
    let mut errors = Vec::new();
    for name in names {
        let result = source_windows::remove_staged_file(directory, Path::new(name));
        if let Err(error) = result {
            if error.kind() != io::ErrorKind::NotFound {
                errors.push(format!("{name}: {error}"));
            }
        }
    }
    match staging_parent_matches(root, parent_security) {
        Ok(true) => {
            if let Err(error) = delete_directory_by_handle(directory) {
                if error.kind() != io::ErrorKind::NotFound {
                    errors.push(format!("staging directory: {error}"));
                }
            }
        }
        Ok(false) => errors.push("export folder security changed".to_owned()),
        Err(error) => errors.push(format!("could not recheck export folder security: {error}")),
    }
    errors
}
