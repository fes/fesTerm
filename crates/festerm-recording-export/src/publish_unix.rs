use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use festerm_recording::{LocalStart, NameTemplates, RecordingId};
use festerm_unix_security::{
    create_private_directory, open_directory_read_nofollow, open_file_nofollow, open_private_file,
    rename_noreplace, secure_staging_parent, staging_parent_matches, StagingParentSecurity,
};
use nix::unistd::{unlinkat, UnlinkatFlags};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ExportError, StagedRecording};
use std::os::unix::fs::MetadataExt;

const MAX_NAME_BYTES: usize = 255;
const MAX_COLLISIONS: usize = 10_000;

/// A synchronized directory atomically published under the retained root.
#[derive(Debug)]
pub struct PublishedRecording {
    pub directory_name: String,
    pub video_names: Vec<String>,
    stage_id: Uuid,
    source_deletion_attempted: bool,
}

/// Copies a complete staged set into the authorized filesystem directory and
/// atomically publishes it without replacing an existing entry.
///
/// The caller retains the native directory authorization and the returned root
/// handle for the duration of this operation. This function never deletes the
/// source or the private staged files. Destination-side temporary files may be
/// observed or synchronized by the chosen filesystem provider.
pub fn publish_to_directory(
    staged: &StagedRecording,
    root: &File,
    templates: &NameTemplates,
    id: RecordingId,
    start: LocalStart,
    cancelled: &AtomicBool,
) -> Result<PublishedRecording, ExportError> {
    if !root
        .metadata()
        .map_err(|error| ExportError::new(format!("could not inspect export folder: {error}")))?
        .is_dir()
    {
        return Err(ExportError::new(
            "the selected export root is not a directory",
        ));
    }
    let parent_security = secure_staging_parent(root).map_err(|error| {
        ExportError::new(format!(
            "the export folder cannot safely stage a recording: {error}"
        ))
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
    let temp_path = Path::new(&temp_name);
    let directory = create_private_directory(root, temp_path).map_err(|error| {
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
            let name_path = Path::new(name);
            let mut destination =
                open_private_file(&directory, name_path, true).map_err(|error| {
                    ExportError::new(format!("could not create destination MP4: {error}"))
                })?;
            created_names.push(name.clone());
            let name = video
                .path
                .file_name()
                .ok_or_else(|| ExportError::new("staged MP4 does not have a valid filename"))?;
            let mut source = open_file_nofollow(&staged.directory, Path::new(name))
                .map_err(|error| ExportError::new(format!("could not open staged MP4: {error}")))?;
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
        }
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
            let name = collision_name(&base, collision);
            match rename_noreplace(root, temp_path, root, Path::new(&name)) {
                Ok(()) => {
                    published = Some(name);
                    let target = open_directory_read_nofollow(
                        root,
                        Path::new(published.as_deref().expect("directory was published")),
                    )
                    .and_then(|target| {
                        let source = directory.metadata()?;
                        let target = target.metadata()?;
                        if source.dev() == target.dev() && source.ino() == target.ino() {
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
        root.sync_all().map_err(|error| {
            ExportError::new(format!(
                "recording directory {name} was published but could not be synced; the source must be retained: {error}"
            ))
        })?;
        Ok(())
    })();

    if let Err(error) = result {
        if published.is_none() {
            let cleanup = cleanup(
                root,
                &directory,
                temp_path,
                &created_names,
                &parent_security,
            );
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

/// Removes the original private source only after this exact set was published.
///
/// The source directory must remain exclusively owned by the application.
/// Changed, replaced, linked or non-private sources are retained for review.
pub fn delete_source_after_publication(
    staged: &StagedRecording,
    published: &mut PublishedRecording,
) -> Result<(), ExportError> {
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

fn collision_name(base: &str, collision: usize) -> String {
    if collision == 0 {
        return base.to_owned();
    }
    let suffix = format!("-{}", collision + 1);
    let mut end = base.len().min(MAX_NAME_BYTES - suffix.len());
    while !base.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &base[..end], suffix)
}

fn cleanup(
    root: &File,
    directory: &File,
    temp_path: &Path,
    names: &[String],
    parent_security: &StagingParentSecurity,
) -> Vec<String> {
    let mut errors = Vec::new();
    for name in names {
        if let Err(error) = unlinkat(directory, Path::new(name), UnlinkatFlags::NoRemoveDir) {
            if error != nix::errno::Errno::ENOENT {
                errors.push(format!("{name}: {error}"));
            }
        }
    }
    match staging_parent_matches(root, parent_security) {
        Ok(true) => {
            if let Err(error) = unlinkat(root, temp_path, UnlinkatFlags::RemoveDir) {
                if error != nix::errno::Errno::ENOENT {
                    errors.push(format!("{}: {error}", temp_path.display()));
                }
            }
        }
        Ok(false) => errors.push("export folder security changed".to_owned()),
        Err(error) => errors.push(format!("could not recheck export folder security: {error}")),
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_suffix_respects_byte_and_unicode_boundaries() {
        let base = "é".repeat(127);
        let name = collision_name(&base, 1);
        assert!(name.len() <= MAX_NAME_BYTES);
        assert!(name.ends_with("-2"));
        assert!(name.is_char_boundary(name.len()));
    }
}
